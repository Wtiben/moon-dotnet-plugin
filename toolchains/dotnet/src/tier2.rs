use crate::config::DotnetToolchainConfig;
use crate::global_json::{SdkRequirement, parse_sdk_requirement, satisfies};
use crate::infer_tasks::{InferInputs, infer_tasks};
use crate::msbuild::{
    EvalEnv, common_source_prefix, evaluate_project, evaluate_projects_batch, normalize_path_key,
};
use crate::nuget_lock::parse_lock_file;
use extism_pdk::*;
use moon_config::{DependencyScope, UnresolvedVersionSpec, VersionSpec};
use moon_pdk::{
    HostLogInput, HostLogTarget, command_exists, get_host_env_var, get_host_environment, host_log,
    into_virtual_path, is_project_toolchain_enabled, parse_toolchain_config, plugin_err,
};
use moon_pdk_api::*;
use serde::{Deserialize, Serialize};
use starbase_utils::{fs, yaml};
use std::collections::{BTreeMap, BTreeSet};

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

/// NuGet lock file names: the default `packages.lock.json`, plus the
/// `packages.<project>.lock.json` convention used when `NuGetLockFilePath`
/// renames it (case-insensitive, NuGet accepts any casing).
fn is_lock_file_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();

    lower == "packages.lock.json"
        || (lower.starts_with("packages.") && lower.ends_with(".lock.json"))
}

/// List NuGet lock files directly inside a directory (non-recursive), sorted.
fn find_lock_files(dir: &VirtualPath) -> Vec<VirtualPath> {
    let mut names = vec![];

    if let Ok(entries) = std::fs::read_dir(dir.any_path()) {
        names = entries
            .flatten()
            .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_file()))
            .filter_map(|entry| entry.file_name().into_string().ok())
            .filter(|name| is_lock_file_name(name))
            .collect::<Vec<_>>();

        names.sort();
    }

    names.into_iter().map(|name| dir.join(name)).collect()
}

/// Workspace-level MSBuild/NuGet config files that can change evaluation,
/// restore, or build behavior from any level between a project dir and the
/// workspace root. Matched case-insensitively: NuGet itself accepts any
/// casing of `nuget.config`, and over-matching the others merely over-hashes
/// (a spurious cache invalidation, never a stale hit).
const CONFIG_FILE_NAMES: &[&str] = &[
    "directory.build.props",
    "directory.build.rsp",
    "directory.build.targets",
    "directory.packages.props",
    "global.json",
    "nuget.config",
];

/// List hash-relevant config files directly inside a directory
/// (non-recursive), sorted by actual file name.
fn find_config_files(dir: &VirtualPath) -> Vec<VirtualPath> {
    let mut names = vec![];

    if let Ok(entries) = std::fs::read_dir(dir.any_path()) {
        names = entries
            .flatten()
            .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_file()))
            .filter_map(|entry| entry.file_name().into_string().ok())
            .filter(|name| CONFIG_FILE_NAMES.contains(&name.to_ascii_lowercase().as_str()))
            .collect::<Vec<_>>();

        names.sort();
    }

    names.into_iter().map(|name| dir.join(name)).collect()
}

/// Depth-limited search for any NuGet lock file under a directory.
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
            if is_lock_file_name(&name) {
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

/// Partial shape of an inherited tasks file (`.moon/tasks.yml` or
/// `.moon/tasks/**/*.yml`) — just enough to know which task ids it defines
/// and whether it can apply to dotnet projects.
#[derive(Debug, Default, serde::Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct InheritedTasksFile {
    inherited_by: Option<InheritedByScope>,
    tasks: BTreeMap<String, serde::de::IgnoredAny>,
}

#[derive(Debug, Default, serde::Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct InheritedByScope {
    toolchains: Option<Vec<String>>,
    languages: Option<Vec<String>>,
}

/// Can an inherited tasks file apply to dotnet projects? Only an explicit
/// `inheritedBy` scope naming other toolchains/languages rules it out;
/// everything else (unscoped, tag/stack/layer-scoped) is conservatively
/// assumed to apply — suppressing an inferred task is recoverable, while
/// moon's args-append merge of an inferred task over an inherited one
/// produces garbage commands.
fn applies_to_dotnet(scope: Option<&InheritedByScope>) -> bool {
    let Some(scope) = scope else {
        return true;
    };

    let mut scoped = false;

    if let Some(toolchains) = &scope.toolchains {
        scoped = true;

        if toolchains.iter().any(|id| id.eq_ignore_ascii_case("dotnet")) {
            return true;
        }
    }

    if let Some(languages) = &scope.languages {
        scoped = true;

        if languages.iter().any(|lang| {
            matches!(
                lang.to_lowercase().as_str(),
                "csharp" | "c#" | "fsharp" | "f#" | "vb" | "visualbasic" | "dotnet"
            )
        }) {
            return true;
        }
    }

    !scoped
}

fn collect_yaml_files(dir: &VirtualPath, out: &mut Vec<VirtualPath>) {
    if let Ok(entries) = std::fs::read_dir(dir.any_path()) {
        for entry in entries.flatten() {
            let Ok(name) = entry.file_name().into_string() else {
                continue;
            };

            let path = dir.join(&name);

            if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                collect_yaml_files(&path, out);
            } else if name.ends_with(".yml") || name.ends_with(".yaml") {
                out.push(path);
            }
        }
    }
}

/// Task ids defined in inherited task files that can apply to dotnet
/// projects. Inference must never contribute one of these ids — see
/// `applies_to_dotnet` for why.
fn load_inherited_task_ids(workspace_root: &VirtualPath) -> BTreeSet<String> {
    let mut ids = BTreeSet::new();
    let mut files = vec![workspace_root.join(".moon").join("tasks.yml")];

    collect_yaml_files(&workspace_root.join(".moon").join("tasks"), &mut files);

    for file in files {
        if !file.exists() {
            continue;
        }

        // An unparseable file is moon's problem to report; there is nothing
        // for inference to yield to.
        if let Ok(parsed) = yaml::read_file::<InheritedTasksFile>(file.any_path()) {
            if applies_to_dotnet(parsed.inherited_by.as_ref()) {
                ids.extend(parsed.tasks.into_keys());
            }
        }
    }

    ids
}

/// FNV-1a digest, rendered hex. Used only to discriminate cache keys, never
/// for integrity — a plain content hash would mean pulling sha2 into the
/// wasm binary. Deterministic across Rust versions, unlike `DefaultHasher`.
fn content_digest(content: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;

    for byte in content.as_bytes() {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }

    format!("{hash:016x}")
}

/// Cached evaluated package set for one moon project, written by the batched
/// graph evaluation and read back by task hashing.
#[derive(Debug, Deserialize, Serialize)]
struct EvalCacheEntry {
    /// Digest of every file that can change the evaluated package set, so a
    /// stale entry is never used.
    digest: String,
    packages: BTreeMap<String, String>,
}

/// Where cached package sets live. Under `.moon/cache`, which moon already
/// treats as disposable.
fn eval_cache_file(workspace_root: &VirtualPath, project_id: &str) -> VirtualPath {
    let safe_id = project_id
        .chars()
        .map(|char| {
            if char.is_ascii_alphanumeric() || matches!(char, '-' | '_' | '.') {
                char
            } else {
                '_'
            }
        })
        .collect::<String>();

    workspace_root
        .join(".moon")
        .join("cache")
        .join("dotnet-toolchain")
        .join("eval")
        .join(format!("{safe_id}.json"))
}

/// Digest of everything that can change a project's evaluated package set:
/// its project files, plus every config file from the project directory up to
/// the workspace root. Effects of custom `<Import>`s outside the
/// `Directory.Build.*` conventions are not captured — the same caveat that
/// already applies to task hashing itself.
fn eval_cache_digest(project_root: &VirtualPath, workspace_root: &VirtualPath) -> String {
    let mut buffer = String::new();

    for file in find_project_files(project_root) {
        buffer.push_str(&fs::read_file(&file).unwrap_or_default());
    }

    let mut current = Some(project_root.to_owned());

    while let Some(dir) = current {
        for file in find_config_files(&dir) {
            buffer.push_str(&fs::read_file(&file).unwrap_or_default());
        }

        if dir.any_path() == workspace_root.any_path() {
            break;
        }

        current = dir.parent();
    }

    content_digest(&buffer)
}

/// Persist a project's evaluated package set for task hashing to reuse.
///
/// Task hashing needs the same data the project graph just evaluated, but
/// runs later (often in a separate process, against a cached project graph),
/// so it cannot rely on in-memory state. Without this, a workspace without
/// lock files pays one MSBuild evaluation *per project* during hashing —
/// which is what the batched graph evaluation exists to avoid.
fn write_eval_cache(
    workspace_root: &VirtualPath,
    project_id: &str,
    project_root: &VirtualPath,
    packages: BTreeMap<String, String>,
) {
    let file = eval_cache_file(workspace_root, project_id);

    let entry = EvalCacheEntry {
        digest: eval_cache_digest(project_root, workspace_root),
        packages,
    };

    // Best-effort: a failed write only costs a re-evaluation later. Two tasks
    // of the same project can race here, but they write identical content and
    // a torn read simply fails to parse (also a re-evaluation).
    if let Some(parent) = file.parent() {
        let _ = fs::create_dir_all(parent);
    }

    if let Ok(json) = serde_json::to_string(&entry) {
        let _ = fs::write_file(&file, json);
    }
}

/// Read a project's cached package set, if it is still current.
fn read_eval_cache(
    workspace_root: &VirtualPath,
    project_id: &str,
    project_root: &VirtualPath,
) -> Option<BTreeMap<String, String>> {
    let file = eval_cache_file(workspace_root, project_id);

    if !file.exists() {
        return None;
    }

    let entry: EvalCacheEntry = serde_json::from_str(&fs::read_file(&file).ok()?).ok()?;

    (entry.digest == eval_cache_digest(project_root, workspace_root)).then_some(entry.packages)
}

/// SDK versions laid out under a `DOTNET_ROOT` (`<root>/sdk/<version>`).
fn installed_sdk_versions(root: &VirtualPath) -> Vec<String> {
    let mut versions = vec![];

    if let Ok(entries) = std::fs::read_dir(root.join("sdk").any_path()) {
        for entry in entries.flatten() {
            if entry.file_type().is_ok_and(|kind| kind.is_dir())
                && let Ok(name) = entry.file_name().into_string()
            {
                versions.push(name);
            }
        }
    }

    versions
}

/// Nearest `global.json` SDK pin, searching from `start` up to (and
/// including) the workspace root — the same direction the dotnet host
/// searches from its working directory. Returns the file path (for messages)
/// and the parsed pin.
fn find_sdk_requirement(
    start: &VirtualPath,
    workspace_root: &VirtualPath,
) -> Option<(String, SdkRequirement)> {
    let mut current = Some(start.to_owned());

    while let Some(dir) = current {
        let file = dir.join("global.json");

        if file.exists()
            && let Ok(content) = fs::read_file(&file)
            && let Some(requirement) = parse_sdk_requirement(&content)
        {
            return Some((file.to_string(), requirement));
        }

        if dir.any_path() == workspace_root.any_path() {
            break;
        }

        current = dir.parent();
    }

    None
}

/// Where to look for a `global.json` SDK pin when validating the `~/.dotnet`
/// fallback: from `start` up to (and including) `workspace_root`.
struct SdkPinScope<'a> {
    start: &'a VirtualPath,
    workspace_root: &'a VirtualPath,
}

/// Resolve the DOTNET_ROOT for task environments *and* MSBuild evaluation —
/// both must agree, or the graph gets evaluated by one SDK while tasks run
/// under another.
///
/// Order: explicit config > existing host env var > `~/.dotnet` when it holds
/// a real SDK layout (where the proto dotnet plugin installs).
///
/// The `~/.dotnet` fallback is guarded: a leftover install there (a stale
/// proto experiment, say) would otherwise be injected over a perfectly good
/// system SDK, making every task fail against a `global.json` pin it cannot
/// satisfy. When a `dotnet` exists on PATH and the fallback cannot serve the
/// workspace's pin, the fallback is skipped so PATH wins. Explicit
/// configuration is never second-guessed.
fn resolve_dotnet_root(
    config: &DotnetToolchainConfig,
    scope: Option<SdkPinScope<'_>>,
) -> AnyResult<Option<String>> {
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
    // which a real SDK install provides.
    let exe = if env.os.is_windows() {
        "dotnet.exe"
    } else {
        "dotnet"
    };

    if !candidate.join(exe).exists() {
        return Ok(None);
    }

    if let Some(scope) = scope
        && command_exists(&env, "dotnet")
        && let Some((file, requirement)) =
            find_sdk_requirement(scope.start, scope.workspace_root)
    {
        let installed = installed_sdk_versions(&candidate);

        if !satisfies(&installed, &requirement) {
            host_log!(
                warn,
                "Ignoring the <path>~/.dotnet</path> fallback for DOTNET_ROOT: it has no SDK satisfying <symbol>{}</symbol> from <path>{}</path> (found: {}). Using the <symbol>dotnet</symbol> on PATH instead — set <property>dotnetRoot</property> to override.",
                requirement.version,
                file,
                if installed.is_empty() {
                    "none".to_owned()
                } else {
                    installed.join(", ")
                }
            );

            return Ok(None);
        }
    }

    if let Some(real) = candidate.real_path() {
        let root = real.to_string_lossy().to_string();

        host_log!(
            debug,
            "Using the <path>~/.dotnet</path> fallback as DOTNET_ROOT: <path>{}</path>",
            root
        );

        return Ok(Some(root));
    }

    Ok(None)
}

/// Build the MSBuild evaluation environment: the same DOTNET_ROOT tasks get,
/// plus an explicit working directory (`global.json` resolves from there).
fn build_eval_env(
    config: &DotnetToolchainConfig,
    cwd: VirtualPath,
    workspace_root: &VirtualPath,
) -> AnyResult<EvalEnv> {
    let dotnet_root = resolve_dotnet_root(
        config,
        Some(SdkPinScope {
            start: &cwd,
            workspace_root,
        }),
    )?;

    // Point at the muxer inside the root, but only when its existence can be
    // confirmed — a host path must be virtualized before wasm can stat it,
    // and roots outside the plugin's readable paths cannot be checked at all.
    // Guessing would turn a working evaluation into "command not found", so
    // unverifiable roots keep using the `dotnet` on PATH, as before.
    let dotnet_exe = dotnet_root.as_ref().and_then(|root| {
        let env = get_host_environment().ok()?;
        let exe = if env.os.is_windows() {
            "dotnet.exe"
        } else {
            "dotnet"
        };
        let real = std::path::PathBuf::from(root).join(exe);

        into_virtual_path(&real)
            .ok()?
            .exists()
            // The host converts a command containing a separator back from
            // its virtual form, so pass the real path.
            .then(|| real.to_string_lossy().to_string())
    });

    Ok(EvalEnv {
        dotnet_root,
        dotnet_exe,
        cwd: Some(cwd),
    })
}

#[plugin_fn]
pub fn extend_task_command(
    Json(input): Json<ExtendTaskCommandInput>,
) -> FnResult<Json<ExtendTaskCommandOutput>> {
    let config = parse_toolchain_config::<DotnetToolchainConfig>(input.toolchain_config)?;
    let mut output = ExtendTaskCommandOutput::default();

    // Tasks run in their project directory, so that is where the dotnet host
    // resolves `global.json` from — validate the fallback against that pin.
    let project_root = input.context.get_project_root(&input.project);
    let scope = SdkPinScope {
        start: &project_root,
        workspace_root: &input.context.workspace_root,
    };

    if let Some(root) = resolve_dotnet_root(&config, Some(scope))? {
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
    for probe in [find_lock_files, find_project_files] {
        if output.root.is_some() {
            break;
        }

        let mut current = Some(input.starting_dir.clone());

        while let Some(dir) = current {
            if !probe(&dir).is_empty() {
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
        load_inherited_task_ids(&input.context.workspace_root)
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
        // Package set collected for the task-hashing cache below.
        let mut packages: BTreeMap<String, String> = BTreeMap::new();

        for file in files {
            let Some(real_path) = file.real_path() else {
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
        let project_root = input
            .project_sources
            .get(id)
            .map(|source| input.context.workspace_root.join(source));

        if let Some(project_root) = project_root
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
pub fn setup_environment(
    Json(input): Json<SetupEnvironmentInput>,
) -> FnResult<Json<SetupEnvironmentOutput>> {
    let mut output = SetupEnvironmentOutput::default();

    // Restore local dotnet tools once per dependencies root when a tool
    // manifest exists. Local tools (.config/dotnet-tools.json) are distinct
    // from global tools, which remain out of scope.
    //
    // Search from the dependencies root up to the workspace root, the same
    // way the dotnet CLI resolves a tool manifest: it conventionally lives at
    // the repository root, which is not necessarily a dependencies root (any
    // project directory holding a lock file becomes one).
    let mut tool_manifest = None;
    let workspace_root = &input.context.workspace_root;
    let mut current = Some(input.root.clone());

    while let Some(dir) = current {
        let candidate = dir.join(".config").join("dotnet-tools.json");

        if candidate.exists() {
            tool_manifest = Some(candidate);
            break;
        }

        if dir.any_path() == workspace_root.any_path() {
            break;
        }

        current = dir.parent();
    }

    if let Some(tool_manifest) = tool_manifest {
        let mut command = ExecCommand::new(
            ExecCommandInput::new("dotnet", ["tool", "restore"]).cwd(input.root.clone()),
        );

        command.label = Some("dotnet tool restore".into());

        // The cache key carries a digest of the manifest content, because
        // moon fingerprints this action on the *declaration* we return here
        // and skips it wholesale when unchanged — a stable key would mean a
        // manifest edit never re-runs the restore. `inputs` then prevents
        // re-execution when the action runs again for unrelated reasons.
        command.cache = Some(format!(
            "dotnet-tool-restore-{}",
            content_digest(&fs::read_file(&tool_manifest)?)
        ));
        command.inputs.push(CacheInput::FileHash(tool_manifest));

        output.commands.push(command);
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
    let mut current = Some(project_root.clone());

    while let Some(dir) = current {
        for file in find_config_files(&dir) {
            let key = file
                .virtual_path()
                .map(|path| path.to_string_lossy().to_string())
                .unwrap_or_else(|| file.to_string());

            configs.insert(key, fs::read_file(&file)?);
        }

        if dir.any_path() == workspace_root.any_path() {
            break;
        }

        current = dir.parent();
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

    let packages: BTreeMap<String, String> = if let Some(cached) =
        var::get::<String>(&cache_key)?
    {
        serde_json::from_str(&cached)?
    } else if let Some(cached) =
        read_eval_cache(workspace_root, input.project.id.as_str(), &project_root)
    {
        var::set(&cache_key, serde_json::to_string(&cached)?)?;

        cached
    } else {
        let mut packages = BTreeMap::new();
        let env = get_host_environment()?;

        if command_exists(&env, "dotnet") {
            let config = parse_toolchain_config::<DotnetToolchainConfig>(input.toolchain_config)?;
            let eval_env = build_eval_env(&config, project_root.clone(), workspace_root)?;

            for file in find_project_files(&project_root) {
                let Some(real_path) = file.real_path() else {
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
                    }
                }
            }
        }

        var::set(&cache_key, serde_json::to_string(&packages)?)?;
        // Prime the on-disk cache too, so sibling tasks and later runs skip
        // the evaluation even when the project graph was already cached.
        write_eval_cache(
            workspace_root,
            input.project.id.as_str(),
            &project_root,
            packages.clone(),
        );

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
