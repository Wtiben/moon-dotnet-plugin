//! Locating the files this toolchain cares about, relative to a directory.
//!
//! All of these enumerate a single directory downwards. There is no PDK
//! equivalent: `warpgate_pdk` exposes no directory listing, and
//! `moon_pdk::locate_root*` only walks *up*.

use moon_pdk_api::VirtualPath;

/// Project file extensions this toolchain understands.
pub const PROJECT_EXTENSIONS: &[&str] = &["csproj", "fsproj", "vbproj"];

/// Directories never worth descending into: build output, or owned by another
/// tool. Shared with tier 3's `global.json` scan.
pub const SKIP_DIRS: &[&str] = &["bin", "obj", "node_modules", ".git", ".moon"];

/// Workspace-level MSBuild/NuGet config files that can change evaluation,
/// restore, or build behavior from any level between a project dir and the
/// workspace root. Matched case-insensitively: NuGet itself accepts any
/// casing of `nuget.config`, and over-matching the others merely over-hashes
/// (a spurious cache invalidation, never a stale hit).
pub const CONFIG_FILE_NAMES: &[&str] = &[
    "directory.build.props",
    "directory.build.rsp",
    "directory.build.targets",
    "directory.packages.props",
    "global.json",
    "nuget.config",
];

/// List MSBuild project files (*.csproj etc.) directly inside a directory
/// (non-recursive).
pub fn find_project_files(dir: &VirtualPath) -> Vec<VirtualPath> {
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

/// NuGet lock file names: the default `packages.lock.json`, plus the
/// `packages.<project>.lock.json` convention used when `NuGetLockFilePath`
/// renames it (case-insensitive, NuGet accepts any casing).
pub fn is_lock_file_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();

    lower == "packages.lock.json"
        || (lower.starts_with("packages.") && lower.ends_with(".lock.json"))
}

/// List NuGet lock files directly inside a directory (non-recursive), sorted.
pub fn find_lock_files(dir: &VirtualPath) -> Vec<VirtualPath> {
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

/// List hash-relevant config files directly inside a directory
/// (non-recursive), sorted by actual file name.
pub fn find_config_files(dir: &VirtualPath) -> Vec<VirtualPath> {
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
pub fn contains_lockfile(dir: &VirtualPath, depth: u8) -> bool {
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
        } else if depth > 0
            && !SKIP_DIRS
                .iter()
                .any(|skip| skip.eq_ignore_ascii_case(&name))
        {
            subdirs.push(name);
        }
    }

    subdirs
        .into_iter()
        .any(|name| contains_lockfile(&dir.join(name), depth - 1))
}

/// Does a directory directly contain a solution file (*.sln / *.slnx)?
pub fn has_solution_file(dir: &VirtualPath) -> bool {
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
