//! On-disk cache of evaluated NuGet package sets, keyed per moon project.
//!
//! Task hashing needs the same data the project graph just evaluated, but runs
//! later — often in a separate process, against an already-cached project graph
//! — so it cannot rely on in-memory state. Without this, a workspace with no
//! lock files pays one MSBuild evaluation *per project* while hashing, which is
//! exactly what batching the graph evaluation exists to avoid.

use crate::discovery::{find_config_files, find_project_files};
use moon_pdk_api::VirtualPath;
use serde::{Deserialize, Serialize};
use starbase_utils::fs;
use std::collections::BTreeMap;

/// FNV-1a digest, rendered hex. Used only to discriminate cache keys, never
/// for integrity — a plain content hash would mean pulling sha2 into the
/// wasm binary. Deterministic across Rust versions, unlike `DefaultHasher`.
pub fn content_digest(content: &str) -> String {
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
/// Callers must only pass a set they evaluated *completely*: a partial set is
/// indistinguishable from a complete one once written, and it would be served
/// under a digest that keeps validating.
pub fn write_eval_cache(
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
pub fn read_eval_cache(
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
