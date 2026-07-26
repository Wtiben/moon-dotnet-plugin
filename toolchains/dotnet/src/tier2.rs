use crate::config::DotnetToolchainConfig;
use crate::discovery::{
    contains_lockfile, find_config_files, find_lock_files, find_project_files, has_solution_file,
    walk_up,
};
use crate::eval_cache::{read_eval_cache, write_eval_cache};
use crate::infer_tasks::{InferInputs, infer_tasks, reportable_conflicts};
use crate::inherited_tasks::load_inherited_task_ids;
use crate::msbuild::{
    EvalEnv, common_source_prefix, evaluate_project, evaluate_projects_batch,
    is_sdk_resolution_failure, normalize_path_key,
};
use crate::nuget_lock::parse_lock_file;
use crate::tier2_env::{build_eval_env, find_sdk_requirement, uses_test_platform_runner};
use extism_pdk::*;
use moon_config::{DependencyScope, UnresolvedVersionSpec, VersionSpec};
use moon_pdk::{
    HostLogInput, HostLogTarget, command_exists, get_host_environment, host_log,
    is_project_toolchain_enabled, parse_toolchain_config, plugin_err,
};
use moon_pdk_api::*;
use starbase_utils::fs;
use std::collections::{BTreeMap, BTreeSet};

#[host_fn]
extern "ExtismHost" {
    fn host_log(input: Json<HostLogInput>);
}

#[plugin_fn]
pub fn locate_dependencies_root(
    Json(input): Json<LocateDependenciesRootInput>,
) -> FnResult<Json<LocateDependenciesRootOutput>> {
    let mut output = LocateDependenciesRootOutput::default();
    let workspace_root = &input.context.workspace_root;

    // Nearest solution file wins.
    for dir in walk_up(&input.starting_dir, workspace_root) {
        if has_solution_file(&dir) {
            output.root = dir.virtual_path();
            break;
        }
    }

    // Fall back to the nearest lockfile, then the nearest project file.
    for probe in [find_lock_files, find_project_files] {
        if output.root.is_some() {
            break;
        }

        for dir in walk_up(&input.starting_dir, workspace_root) {
            if !probe(&dir).is_empty() {
                output.root = dir.virtual_path();
                break;
            }
        }
    }

    // Single dependencies root for v1; no member globs.
    output.members = None;

    Ok(Json(output))
}

#[plugin_fn]
pub fn extend_project_graph(
    Json(input): Json<ExtendProjectGraphInput>,
) -> FnResult<Json<ExtendProjectGraphOutput>> {
    let config = parse_toolchain_config::<DotnetToolchainConfig>(input.toolchain_config)?;
    let mut output = ExtendProjectGraphOutput::default();

    let infer_tasks_enabled = config.infer_tasks.any_enabled();

    if !config.infer_dependencies && !infer_tasks_enabled {
        return Ok(Json(output));
    }

    // Pass 1: locate every project's MSBuild project files, and index their
    // host-real paths (normalized) so ProjectReference targets can be
    // resolved back to moon project ids. A secondary index on the
    // workspace-relative suffix ("<source>/<file>") covers cases where the
    // exact real paths differ lexically — e.g. Windows 8.3 short names in
    // the workspace prefix (MSBuild prints expanded long paths).
    let mut project_files: BTreeMap<Id, Vec<VirtualPath>> = BTreeMap::new();
    let mut real_path_index: BTreeMap<String, Id> = BTreeMap::new();
    // suffix -> Some(id), or None when two projects share a suffix (ambiguous).
    let mut suffix_index: BTreeMap<String, Option<Id>> = BTreeMap::new();

    for (id, source) in &input.project_sources {
        let project_root = input.context.workspace_root.join(source);
        let files = find_project_files(&project_root);

        if files.is_empty() {
            // Not a .NET project; none of our business.
            continue;
        }

        for file in &files {
            if let Some(real) = file.real_path() {
                real_path_index.insert(normalize_path_key(&real.to_string_lossy()), id.to_owned());
            }

            if let Some(name) = file.file_name().and_then(|name| name.to_str()) {
                let source = source.trim_matches('/');
                let suffix = if source.is_empty() || source == "." {
                    normalize_path_key(&format!("/{name}"))
                } else {
                    normalize_path_key(&format!("/{source}/{name}"))
                };

                suffix_index
                    .entry(suffix)
                    .and_modify(|existing| *existing = None)
                    .or_insert_with(|| Some(id.to_owned()));
            }
        }

        project_files.insert(id.to_owned(), files);
    }

    if project_files.is_empty() {
        return Ok(Json(output));
    }

    let env = get_host_environment()?;

    // Degrade rather than fail, like `parse_manifest` and `hash_task_contents`
    // below. The graph is built before the action pipeline runs, so a `version:`
    // configured for tier 3 to install has not been installed yet on a fresh
    // machine — erroring here would fail the whole-workspace graph, for every
    // toolchain, before moon ever gets to install the SDK it was told to
    // install.
    if !command_exists(&env, "dotnet") {
        host_log!(
            warn,
            "No <symbol>dotnet</symbol> executable found on PATH, skipping .NET project graph evaluation — no dependency edges or inferred tasks will be contributed. Install a .NET 8+ SDK, or set <property>version</property> in <file>.moon/toolchains.yml</file> to have moon install one."
        );

        return Ok(Json(output));
    }

    // Evaluate from the deepest directory containing every .NET project, so
    // a `global.json` in that subtree governs evaluation exactly as it
    // governs the tasks that run inside it. Without an explicit working
    // directory the dotnet host would resolve `global.json` from wherever
    // moon happened to be invoked, so the same workspace could evaluate
    // under different SDKs run to run.
    let sources = input
        .project_sources
        .iter()
        .filter(|(id, _)| project_files.contains_key(*id))
        .map(|(_, source)| source.as_str())
        .collect::<Vec<_>>();

    let eval_prefix = common_source_prefix(&sources);
    let eval_dir = if eval_prefix.is_empty() {
        input.context.workspace_root.clone()
    } else {
        input.context.workspace_root.join(&eval_prefix)
    };

    let eval_env = build_eval_env(&config, eval_dir, &input.context.workspace_root)?;

    // Pass 2: evaluate every project with a single batched MSBuild
    // invocation (one process, parallel in-process evaluation) — the
    // dotnet/MSBuild startup cost dominates per-project evaluation, so this
    // is the difference between minutes and seconds on large workspaces.
    // Anything missing from the batch (broken project, batch-level failure)
    // falls back to per-project evaluation below, keeping the batch purely
    // an optimization.
    let all_project_paths = project_files
        .values()
        .flatten()
        .filter_map(|file| file.real_path())
        .collect::<Vec<_>>();

    let mut batch = match evaluate_projects_batch(
        &input.context.workspace_root,
        &all_project_paths,
        &eval_env,
    ) {
        Ok(results) => results,
        Err(error) => {
            let message = error.to_string();

            // A missing SDK dooms every project, so retrying each one only
            // repeats the host's cryptic output N times and leaves the graph
            // silently empty. Fail once, with the pin that cannot be served
            // and the ways to fix it.
            if is_sdk_resolution_failure(&message) {
                let pin = find_sdk_requirement(
                    eval_env
                        .cwd
                        .as_ref()
                        .unwrap_or(&input.context.workspace_root),
                    &input.context.workspace_root,
                );

                let requirement = match &pin {
                    Some((file, requirement)) => format!(
                        "The .NET SDK pinned by <path>{}</path> (<symbol>{}</symbol>) is not available",
                        file, requirement.version
                    ),
                    None => "No usable .NET SDK was found".to_owned(),
                };

                return Err(plugin_err!(
                    "{requirement}, so MSBuild evaluation cannot run.\n\nInstall that SDK, set <property>version</property> under <property>dotnet</property> in <file>.moon/toolchains.yml</file> to have moon install it, or point <property>dotnetRoot</property> at an SDK that satisfies the pin.\n\n{message}"
                ));
            }

            host_log!(
                warn,
                "Batched MSBuild evaluation failed; falling back to per-project evaluation: {}",
                error
            );
            BTreeMap::new()
        }
    };

    // Inference must yield to task ids already defined in inherited task
    // files (moon merges plugin tasks over inherited ones with args-append
    // semantics — a garbage command). Project-level moon.yml needs no such
    // handling: moon guarantees local tasks win over plugin tasks.
    let reserved_task_ids = if infer_tasks_enabled {
        let reserved = load_inherited_task_ids(&input.context.workspace_root);

        // Report once per workspace, not once per project: without this,
        // "no project has a build task" has no visible cause.
        for (task_id, file) in reportable_conflicts(&reserved, &config.infer_tasks) {
            host_log!(
                warn,
                "Not inferring the <id>{}</id> task: <path>{}</path> already defines it, and moon merges inherited and plugin tasks by appending args — which would produce a broken command. Rename or remove that task to let inference contribute, or list only the tasks you want in <property>inferTasks</property>.",
                task_id,
                file
            );
        }

        reserved.into_keys().collect()
    } else {
        BTreeSet::new()
    };

    let workspace_dir = input
        .context
        .workspace_root
        .real_path()
        .map(|path| path.to_string_lossy().to_string())
        .unwrap_or_default();

    // Pass 3: map each project's ProjectReference items onto moon project ids.
    for (id, files) in &project_files {
        let mut project_output = ExtendProjectOutput::default();
        let mut seen_deps: BTreeMap<Id, ()> = BTreeMap::new();
        // Package set collected for the task-hashing cache below. Only cached
        // when every project file was evaluated — a partial set is
        // indistinguishable from a complete one once written, and it would
        // then be served under a digest that stays valid.
        let mut packages: BTreeMap<String, String> = BTreeMap::new();
        let mut evaluated_all = true;

        let project_root = input
            .project_sources
            .get(id)
            .map(|source| input.context.workspace_root.join(source));

        // Which `dotnet test` flavour this project's tasks will run under.
        let test_platform_runner = infer_tasks_enabled
            && project_root
                .as_ref()
                .is_some_and(|root| uses_test_platform_runner(root, &input.context.workspace_root));

        for file in files {
            let Some(real_path) = file.real_path() else {
                evaluated_all = false;
                continue;
            };

            let batch_key = normalize_path_key(&real_path.to_string_lossy());

            let evaluation = if let Some(evaluation) = batch.remove(&batch_key) {
                evaluation
            } else {
                // Fall back with the project's own directory as the working
                // directory — the same `global.json` its tasks will resolve.
                let single_env = EvalEnv {
                    cwd: file.parent().or_else(|| eval_env.cwd.clone()),
                    ..eval_env.clone()
                };

                match evaluate_project(&real_path, &single_env) {
                    Ok(evaluation) => evaluation,
                    Err(error) => {
                        // One broken project must not take down graph
                        // construction for the whole workspace.
                        host_log!(
                            warn,
                            "MSBuild evaluation failed for project <id>{}</id> ({}): {}",
                            id,
                            real_path.display(),
                            error
                        );

                        evaluated_all = false;
                        continue;
                    }
                }
            };

            packages.extend(evaluation.package_references());

            // Project alias from the evaluated AssemblyName, so tasks can
            // reference the project by its .NET name (e.g.
            // `moon run MyCompany.App:build`). moon silently skips aliases
            // that collide with project ids or already-claimed aliases, and
            // an alias equal to its own id is a no-op — no need to filter
            // beyond emptiness here.
            if project_output.alias.is_none() {
                let assembly_name = evaluation.property("AssemblyName");

                if !assembly_name.is_empty() {
                    project_output.alias = Some(assembly_name.to_owned());
                }
            }

            if config.infer_dependencies {
                for reference in evaluation.project_reference_paths() {
                    let key = normalize_path_key(&reference);

                    // Exact real-path match first; fall back to the unique
                    // workspace-relative suffix.
                    let matched = real_path_index.get(&key).or_else(|| {
                        suffix_index
                            .iter()
                            .find(|(suffix, id)| id.is_some() && key.ends_with(suffix.as_str()))
                            .and_then(|(_, id)| id.as_ref())
                    });

                    let Some(dep_id) = matched else {
                        host_log!(
                            debug,
                            "Project <id>{}</id> references {} which is outside the moon workspace; skipping",
                            id,
                            reference
                        );
                        continue;
                    };

                    if dep_id != id && !seen_deps.contains_key(dep_id) {
                        seen_deps.insert(dep_id.to_owned(), ());

                        let file_name = std::path::Path::new(&reference)
                            .file_name()
                            .map(|name| name.to_string_lossy().to_string())
                            .unwrap_or(reference.clone());

                        project_output.dependencies.push(ProjectDependency {
                            id: dep_id.to_owned(),
                            scope: DependencyScope::Production,
                            via: Some(format!("project-reference {file_name}")),
                        });
                    }
                }
            }

            if infer_tasks_enabled {
                let project_dir = real_path
                    .parent()
                    .map(|dir| dir.to_string_lossy().to_string())
                    .unwrap_or_default();

                // Bare `dotnet build` errors on ambiguity when the directory
                // holds several project files — pass the file explicitly.
                let explicit_project_file = if files.len() > 1 {
                    file.file_name().and_then(|name| name.to_str())
                } else {
                    None
                };

                let inferred = infer_tasks(
                    &config.infer_tasks,
                    &reserved_task_ids,
                    &InferInputs {
                        evaluation: &evaluation,
                        explicit_project_file,
                        project_dir: &project_dir,
                        workspace_dir: &workspace_dir,
                        test_platform_runner,
                    },
                );

                match inferred {
                    Ok(tasks) => {
                        for (task_id, task) in tasks {
                            project_output.tasks.entry(task_id).or_insert(task);
                        }
                    }
                    Err(error) => {
                        host_log!(
                            warn,
                            "Task inference failed for project <id>{}</id>: {}",
                            id,
                            error
                        );
                    }
                }
            }

            if let Some(virtual_file) = file.virtual_path() {
                output.input_files.push(virtual_file);
            }
        }

        // Hand the evaluated package set to task hashing. Projects with a
        // lock file take the lock-file branch there and never need it.
        if evaluated_all
            && let Some(project_root) = project_root
            && find_lock_files(&project_root).is_empty()
        {
            write_eval_cache(
                &input.context.workspace_root,
                id.as_str(),
                &project_root,
                packages,
            );
        }

        if !project_output.dependencies.is_empty()
            || !project_output.tasks.is_empty()
            || project_output.alias.is_some()
        {
            output
                .extended_projects
                .insert(id.to_owned(), project_output);
        }
    }

    Ok(Json(output))
}

#[plugin_fn]
pub fn install_dependencies(
    Json(input): Json<InstallDependenciesInput>,
) -> FnResult<Json<InstallDependenciesOutput>> {
    let config = parse_toolchain_config::<DotnetToolchainConfig>(input.toolchain_config)?;
    let mut output = InstallDependenciesOutput::default();

    let mut args: Vec<String> = vec!["restore".into()];

    // The mere presence of a lock file opts a project into lock-file restore;
    // --locked-mode additionally fails restore (NU1004) when declared
    // dependencies drifted from the lock file.
    if contains_lockfile(&input.root, 5) {
        args.push("--locked-mode".into());
    }

    args.extend(config.restore_args.iter().cloned());

    output.install_command = Some(
        ExecCommandInput::new("dotnet", args)
            .cwd(input.root.clone())
            .into(),
    );
    // NuGet has no dedupe concept.
    output.dedupe_command = None;

    Ok(Json(output))
}

#[plugin_fn]
pub fn parse_lock(Json(input): Json<ParseLockInput>) -> FnResult<Json<ParseLockOutput>> {
    let mut output = ParseLockOutput::default();
    let lock = parse_lock_file(&fs::read_file(&input.path)?)?;

    // Dedupe identical entries across target frameworks.
    for entries in lock.dependencies.into_values() {
        for (name, entry) in entries {
            // Project-type entries are in-repo ProjectReferences, not packages.
            if entry.dep_type.eq_ignore_ascii_case("Project") {
                continue;
            }

            let versions = output.dependencies.entry(name).or_default();

            let version = entry
                .resolved
                .as_deref()
                .and_then(|value| VersionSpec::parse(value).ok());

            let already_present = versions.iter().any(|existing: &LockDependency| {
                existing.version == version && existing.hash == entry.content_hash
            });

            if !already_present {
                versions.push(LockDependency {
                    hash: entry.content_hash,
                    meta: None,
                    // NuGet ranges like "[13.0.3, )" may not parse; omit then.
                    req: entry
                        .requested
                        .as_deref()
                        .and_then(|value| UnresolvedVersionSpec::parse(value).ok()),
                    version,
                });
            }
        }
    }

    Ok(Json(output))
}

#[plugin_fn]
pub fn parse_manifest(
    Json(input): Json<ParseManifestInput>,
) -> FnResult<Json<ParseManifestOutput>> {
    let mut output = ParseManifestOutput::default();

    let Some(real_path) = input.path.real_path() else {
        return Ok(Json(output));
    };

    let env = get_host_environment()?;

    // Degrade silently like hash_task_contents: a missing dotnet must not
    // fail moon's install fingerprinting.
    if !command_exists(&env, "dotnet") {
        return Ok(Json(output));
    }

    let manifest_dir = input
        .path
        .parent()
        .unwrap_or_else(|| input.context.workspace_root.clone());

    // `parse_manifest` carries no toolchain config, so an explicit
    // `dotnetRoot` cannot be honored here; the env var and the guarded
    // `~/.dotnet` fallback still apply.
    let eval_env = build_eval_env(
        &DotnetToolchainConfig::default(),
        manifest_dir,
        &input.context.workspace_root,
    )?;

    let evaluation = match evaluate_project(&real_path, &eval_env) {
        Ok(evaluation) => evaluation,
        Err(error) => {
            host_log!(
                warn,
                "MSBuild evaluation failed while parsing manifest {}: {}",
                real_path.display(),
                error
            );

            return Ok(Json(output));
        }
    };

    // NuGet range syntax ("[13.0.3]", "(1.0,2.0)") is not a moon version
    // spec; keep the raw string as a reference so the dependency is still
    // listed (it just won't contribute a version to fingerprints).
    let to_dependency = |version: String| match UnresolvedVersionSpec::parse(&version) {
        Ok(spec) => ManifestDependency::new(spec),
        Err(_) => ManifestDependency::Config(ManifestDependencyConfig {
            reference: Some(version),
            ..Default::default()
        }),
    };

    let is_packages_props = input
        .path
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.eq_ignore_ascii_case("Directory.Packages.props"));

    if is_packages_props {
        // Central Package Management: PackageVersion items declare the
        // workspace-level versions that versionless PackageReferences
        // inherit. This is the only manifest name moon can actually track
        // for .NET — project files have variable names, which moon's
        // literal-name manifest matching cannot express.
        for (name, version) in evaluation.package_versions() {
            output.dependencies.insert(name, to_dependency(version));
        }
    } else {
        for (name, version) in evaluation.package_references() {
            let dep = if version == "*" {
                // Versionless under CPM: inherited from the workspace
                // manifest (Directory.Packages.props).
                ManifestDependency::inherited()
            } else {
                to_dependency(version)
            };

            output.dependencies.insert(name, dep);
        }

        output.publishable = evaluation
            .property("IsPackable")
            .eq_ignore_ascii_case("true");
    }

    Ok(Json(output))
}

#[plugin_fn]
pub fn hash_task_contents(
    Json(input): Json<HashTaskContentsInput>,
) -> FnResult<Json<HashTaskContentsOutput>> {
    let mut output = HashTaskContentsOutput::default();

    if !is_project_toolchain_enabled(&input.project) {
        return Ok(Json(output));
    }

    let project_root = input.context.get_project_root(&input.project);

    // Config files (Directory.Build.props/targets/rsp, Directory.Packages.props,
    // nuget.config, global.json) from the project dir up to the workspace root
    // are always hashed: conditions/imports can make any of them affect the
    // resolved package set, and props/targets/rsp change build behavior even
    // when the package set is fully pinned by a lock file. Effects of custom
    // `<Import>`s outside these conventions are only captured via the
    // evaluated package set below, not content-hashed.
    let mut configs: BTreeMap<String, String> = BTreeMap::new();
    let workspace_root = &input.context.workspace_root;

    for dir in walk_up(&project_root, workspace_root) {
        for file in find_config_files(&dir) {
            let key = file
                .virtual_path()
                .map(|path| path.to_string_lossy().to_string())
                .unwrap_or_else(|| file.to_string());

            configs.insert(key, fs::read_file(&file)?);
        }
    }

    // Lock file(s) present: their content already pins the entire resolved
    // package set (incl. contentHashes) — include them raw and skip the
    // costly MSBuild evaluation.
    let lock_files = find_lock_files(&project_root);

    if !lock_files.is_empty() {
        let mut lockfiles: BTreeMap<String, String> = BTreeMap::new();

        for file in &lock_files {
            let key = file
                .virtual_path()
                .map(|path| path.to_string_lossy().to_string())
                .unwrap_or_else(|| file.to_string());

            lockfiles.insert(key, fs::read_file(file)?);
        }

        output.contents.push(json::json!({
            "configs": configs,
            "lockfiles": lockfiles,
        }));

        return Ok(Json(output));
    }

    // No lock file: hash the *evaluated* PackageReference set instead.
    //
    // Three levels of reuse, because this function runs once per task and a
    // cold MSBuild evaluation costs ~0.5s per project:
    //   1. a plugin-instance var, for repeated tasks of the same project;
    //   2. the on-disk cache the batched graph evaluation primed, which is
    //      what keeps a lock-file-less workspace from paying one evaluation
    //      per project here (the batch already evaluated them all at once);
    //   3. evaluating this project alone.
    let cache_key = format!("eval-packages:{}", input.project.id);

    let packages: BTreeMap<String, String> = if let Some(cached) = var::get::<String>(&cache_key)? {
        serde_json::from_str(&cached)?
    } else if let Some(cached) =
        read_eval_cache(workspace_root, input.project.id.as_str(), &project_root)
    {
        var::set(&cache_key, serde_json::to_string(&cached)?)?;

        cached
    } else {
        let mut packages = BTreeMap::new();
        let mut evaluated_all = false;
        let env = get_host_environment()?;

        if command_exists(&env, "dotnet") {
            let config = parse_toolchain_config::<DotnetToolchainConfig>(input.toolchain_config)?;
            let eval_env = build_eval_env(&config, project_root.clone(), workspace_root)?;

            evaluated_all = true;

            for file in find_project_files(&project_root) {
                let Some(real_path) = file.real_path() else {
                    evaluated_all = false;
                    continue;
                };

                match evaluate_project(&real_path, &eval_env) {
                    Ok(evaluation) => {
                        packages.extend(evaluation.package_references());
                    }
                    Err(error) => {
                        host_log!(
                            warn,
                            "MSBuild evaluation failed while hashing <id>{}</id>: {}",
                            input.project.id,
                            error
                        );

                        evaluated_all = false;
                    }
                }
            }
        }

        // Kept regardless: the var is scoped to this plugin instance, so it
        // stops us re-evaluating once per task while an SDK is genuinely
        // missing, and it disappears with the process.
        var::set(&cache_key, serde_json::to_string(&packages)?)?;

        // The on-disk cache only ever holds a complete set. Writing a partial
        // one would persist it under a digest that keeps validating, and since
        // this set is the only hash signal for a workspace without lock files,
        // package changes would stop invalidating task hashes — moon would
        // serve stale builds, and installing the missing SDK later would not
        // recover it.
        if evaluated_all {
            write_eval_cache(
                workspace_root,
                input.project.id.as_str(),
                &project_root,
                packages.clone(),
            );
        }

        packages
    };

    output.contents.push(json::json!({
        "configs": configs,
        "packages": packages,
    }));

    Ok(Json(output))
}

#[plugin_fn]
pub fn prune_docker(Json(input): Json<PruneDockerInput>) -> FnResult<Json<PruneDockerOutput>> {
    let mut output = PruneDockerOutput::default();

    let mut roots = vec![input.root.clone()];

    for project in &input.projects {
        roots.push(input.context.get_project_root(project));
    }

    for root in roots {
        for dir_name in ["bin", "obj"] {
            let dir = root.join(dir_name);

            if dir.exists() {
                fs::remove_dir_all(&dir)?;

                if let Some(file) = dir.virtual_path() {
                    output.changed_files.push(file);
                }
            }
        }
    }

    Ok(Json(output))
}
