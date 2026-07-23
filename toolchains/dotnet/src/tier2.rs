use crate::config::DotnetToolchainConfig;
use crate::msbuild::{evaluate_project, normalize_path_key};
use extism_pdk::*;
use moon_config::{DependencyScope, PartialTaskArgs, PartialTaskConfig};
use moon_pdk::{
    HostLogInput, HostLogTarget, command_exists, get_host_env_var, get_host_environment,
    host_log, locate_root, parse_toolchain_config, plugin_err,
};
use moon_pdk_api::*;
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
    // resolved back to moon project ids.
    let mut project_files: BTreeMap<Id, Vec<VirtualPath>> = BTreeMap::new();
    let mut real_path_index: BTreeMap<String, Id> = BTreeMap::new();

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

                    let Some(dep_id) = real_path_index.get(&key) else {
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
