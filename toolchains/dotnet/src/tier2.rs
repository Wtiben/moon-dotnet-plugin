use crate::config::DotnetToolchainConfig;
use crate::msbuild::{evaluate_project, normalize_path_key};
use crate::nuget_lock::parse_lock_file;
use extism_pdk::*;
use moon_config::{
    DependencyScope, PartialTaskArgs, PartialTaskConfig, UnresolvedVersionSpec, VersionSpec,
};
use moon_pdk::{
    HostLogInput, HostLogTarget, command_exists, get_host_env_var, get_host_environment,
    host_log, is_project_toolchain_enabled, locate_root, parse_toolchain_config, plugin_err,
};
use moon_pdk_api::*;
use starbase_utils::fs;
use std::collections::BTreeMap;

#[host_fn]
extern "ExtismHost" {
    fn host_log(input: Json<HostLogInput>);
}

/// Project file extensions this toolchain understands.
const PROJECT_EXTENSIONS: &[&str] = &["csproj", "fsproj", "vbproj"];

/// List MSBuild project files (*.csproj etc.) directly inside a directory
/// (non-recursive).
fn find_project_files(dir: &VirtualPath) -> Vec<VirtualPath> {
    let mut found = vec![];

    if let Ok(entries) = std::fs::read_dir(dir.any_path()) {
        let mut names = entries
            .flatten()
            .filter_map(|entry| entry.file_name().into_string().ok())
            .filter(|name| {
                name.rsplit_once('.').is_some_and(|(_, ext)| {
                    PROJECT_EXTENSIONS
                        .iter()
                        .any(|known| known.eq_ignore_ascii_case(ext))
                })
            })
            .collect::<Vec<_>>();

        names.sort();

        for name in names {
            found.push(dir.join(name));
        }
    }

    found
}

/// Directories that never contain a `packages.lock.json` worth finding.
const SKIP_DIRS: &[&str] = &["bin", "obj", "node_modules", ".git", ".moon"];

/// Depth-limited search for any `packages.lock.json` under a directory.
/// Lock files live next to each project file, not at the dependencies root,
/// so a root-only check would miss them.
fn contains_lockfile(dir: &VirtualPath, depth: u8) -> bool {
    let Ok(entries) = std::fs::read_dir(dir.any_path()) else {
        return false;
    };

    let mut subdirs = vec![];

    for entry in entries.flatten() {
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };

        let is_dir = entry.file_type().is_ok_and(|kind| kind.is_dir());

        if !is_dir {
            if name == "packages.lock.json" {
                return true;
            }
        } else if depth > 0 && !SKIP_DIRS.iter().any(|skip| skip.eq_ignore_ascii_case(&name)) {
            subdirs.push(name);
        }
    }

    subdirs
        .into_iter()
        .any(|name| contains_lockfile(&dir.join(name), depth - 1))
}

/// Does a directory directly contain a solution file (*.sln / *.slnx)?
fn has_solution_file(dir: &VirtualPath) -> bool {
    std::fs::read_dir(dir.any_path()).is_ok_and(|entries| {
        entries
            .flatten()
            .filter_map(|entry| entry.file_name().into_string().ok())
            .any(|name| {
                name.rsplit_once('.').is_some_and(|(_, ext)| {
                    ext.eq_ignore_ascii_case("sln") || ext.eq_ignore_ascii_case("slnx")
                })
            })
    })
}

/// Resolve the DOTNET_ROOT to inject into task environments.
/// Order: explicit config > existing host env var > `~/.dotnet` when it
/// contains an actual SDK layout (the proto dotnet plugin installs there).
fn resolve_dotnet_root(config: &DotnetToolchainConfig) -> AnyResult<Option<String>> {
    if let Some(root) = &config.dotnet_root {
        return Ok(Some(root.clone()));
    }

    if let Some(existing) = get_host_env_var("DOTNET_ROOT")? {
        if !existing.is_empty() {
            return Ok(Some(existing));
        }
    }

    let env = get_host_environment()?;
    let candidate = env.home_dir.join(".dotnet");

    // `~/.dotnet` doubles as the dotnet CLI's user-level cache directory, so
    // mere existence is not enough — require the `dotnet` host executable,
    // which a real SDK install (e.g. via the proto dotnet plugin) provides.
    let exe = if env.os.is_windows() {
        "dotnet.exe"
    } else {
        "dotnet"
    };

    if candidate.join(exe).exists() {
        if let Some(real) = candidate.real_path() {
            return Ok(Some(real.to_string_lossy().to_string()));
        }
    }

    Ok(None)
}

#[plugin_fn]
pub fn extend_task_command(
    Json(input): Json<ExtendTaskCommandInput>,
) -> FnResult<Json<ExtendTaskCommandOutput>> {
    let config = parse_toolchain_config::<DotnetToolchainConfig>(input.toolchain_config)?;
    let mut output = ExtendTaskCommandOutput::default();

    if let Some(root) = resolve_dotnet_root(&config)? {
        output.env.insert("DOTNET_ROOT".into(), root.clone());
        output.paths.push(root.into());
        // Opt out of telemetry noise in CI task runs.
        output
            .env
            .insert("DOTNET_CLI_TELEMETRY_OPTOUT".into(), "1".into());
    }

    Ok(Json(output))
}

#[plugin_fn]
pub fn locate_dependencies_root(
    Json(input): Json<LocateDependenciesRootInput>,
) -> FnResult<Json<LocateDependenciesRootOutput>> {
    let mut output = LocateDependenciesRootOutput::default();
    let workspace_root = &input.context.workspace_root;

    // Walk upward from the starting dir to (and including) the workspace
    // root, never above it: parent() past the WASI preopen boundary is
    // undefined. Nearest solution file wins.
    let mut current = Some(input.starting_dir.clone());

    while let Some(dir) = current {
        if has_solution_file(&dir) {
            output.root = dir.virtual_path();
            break;
        }

        if dir.any_path() == workspace_root.any_path() {
            break;
        }

        current = dir.parent();
    }

    // Fall back to the nearest lockfile, then the nearest project file.
    if output.root.is_none() {
        if let Some(root) = locate_root(&input.starting_dir, "packages.lock.json") {
            output.root = root.virtual_path();
        }
    }

    if output.root.is_none() {
        let mut current = Some(input.starting_dir.clone());

        while let Some(dir) = current {
            if !find_project_files(&dir).is_empty() {
                output.root = dir.virtual_path();
                break;
            }

            if dir.any_path() == workspace_root.any_path() {
                break;
            }

            current = dir.parent();
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

    if !config.infer_dependencies && !config.infer_tasks {
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
                real_path_index.insert(
                    normalize_path_key(&real.to_string_lossy()),
                    id.to_owned(),
                );
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

    if !command_exists(&env, "dotnet") {
        return Err(plugin_err!(
            "dotnet executable not found — install a .NET 8+ SDK or configure proto to provide one."
        ));
    }

    // Pass 2: evaluate each project and map its ProjectReference items onto
    // moon project ids.
    for (id, files) in &project_files {
        let mut project_output = ExtendProjectOutput::default();
        let mut seen_deps: BTreeMap<Id, ()> = BTreeMap::new();

        for file in files {
            let Some(real_path) = file.real_path() else {
                continue;
            };

            let evaluation = match evaluate_project(&real_path) {
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
                    continue;
                }
            };

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

            if config.infer_tasks {
                // `IsTestProject` is set by Microsoft.NET.Test.Sdk's build
                // props, which are only imported after a restore. Fall back
                // to the package reference itself so unrestored projects are
                // detected too.
                let is_test_project = evaluation
                    .property("IsTestProject")
                    .eq_ignore_ascii_case("true")
                    || evaluation
                        .package_references()
                        .keys()
                        .any(|name| name.eq_ignore_ascii_case("Microsoft.NET.Test.Sdk"));

                if is_test_project {
                    project_output.tasks.entry(Id::raw("test")).or_insert_with(|| {
                        PartialTaskConfig {
                            command: Some(PartialTaskArgs::String("dotnet test".into())),
                            ..Default::default()
                        }
                    });
                }

                let output_type = evaluation.property("OutputType");

                if output_type.eq_ignore_ascii_case("Exe")
                    || output_type.eq_ignore_ascii_case("WinExe")
                {
                    project_output.tasks.entry(Id::raw("run")).or_insert_with(|| {
                        PartialTaskConfig {
                            command: Some(PartialTaskArgs::String("dotnet run".into())),
                            ..Default::default()
                        }
                    });
                }
            }

            if let Some(virtual_file) = file.virtual_path() {
                output.input_files.push(virtual_file);
            }
        }

        if !project_output.dependencies.is_empty() || !project_output.tasks.is_empty() {
            output.extended_projects.insert(id.to_owned(), project_output);
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
pub fn hash_task_contents(
    Json(input): Json<HashTaskContentsInput>,
) -> FnResult<Json<HashTaskContentsOutput>> {
    let mut output = HashTaskContentsOutput::default();

    if !is_project_toolchain_enabled(&input.project) {
        return Ok(Json(output));
    }

    let project_root = input.context.get_project_root(&input.project);
    let lockfile = project_root.join("packages.lock.json");

    // Lock file present: its content already pins the entire resolved
    // package set (incl. contentHashes) — include it raw.
    if lockfile.exists() {
        output.contents.push(json::json!({
            "lockfile": fs::read_file(&lockfile)?,
        }));

        return Ok(Json(output));
    }

    // No lock file: hash the *evaluated* PackageReference set plus the
    // contents of every Directory.Build.props / Directory.Packages.props
    // from the project dir up to the workspace root (conditions/imports can
    // make any of them affect the resolved set).
    //
    // Cache the evaluated package set per project within this plugin
    // instance — hash_task_contents runs once per task and MSBuild
    // evaluation costs ~0.5s.
    let cache_key = format!("eval-packages:{}", input.project.id);

    let packages: BTreeMap<String, String> = if let Some(cached) =
        var::get::<String>(&cache_key)?
    {
        serde_json::from_str(&cached)?
    } else {
        let mut packages = BTreeMap::new();
        let env = get_host_environment()?;

        if command_exists(&env, "dotnet") {
            for file in find_project_files(&project_root) {
                let Some(real_path) = file.real_path() else {
                    continue;
                };

                match evaluate_project(&real_path) {
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
                    }
                }
            }
        }

        var::set(&cache_key, serde_json::to_string(&packages)?)?;

        packages
    };

    let mut props: BTreeMap<String, String> = BTreeMap::new();
    let workspace_root = &input.context.workspace_root;
    let mut current = Some(project_root.clone());

    while let Some(dir) = current {
        for name in ["Directory.Build.props", "Directory.Packages.props"] {
            let file = dir.join(name);

            if file.exists() {
                let key = file
                    .virtual_path()
                    .map(|path| path.to_string_lossy().to_string())
                    .unwrap_or_else(|| name.to_string());

                props.insert(key, fs::read_file(&file)?);
            }
        }

        if dir.any_path() == workspace_root.any_path() {
            break;
        }

        current = dir.parent();
    }

    output.contents.push(json::json!({
        "packages": packages,
        "props": props,
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
