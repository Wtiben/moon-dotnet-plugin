# Follow-ups (deferred from v1)

1. **Full test matrix (props chains, multi-TFM, .slnx, Conditions, CPM, alternate lock names)** —
   fixtures for `Directory.Build.props` inheritance chains (nested props, `GetPathOfFileAbove`
   chaining), multi-targeting TFMs (per-TFM re-evaluation with `-p:TargetFramework=`), `.slnx`
   solution enumeration, MSBuild `Condition`s, Central Package Management
   (`Directory.Packages.props` + versionless PackageReference), and `packages.<project>.lock.json`
   alternate lock file names.
2. **Registry submission: moonrepo third-party plugin registry** — submit an entry once released.
3. **Tier 3 proper: replace Phault delegation** — the archived `Phault/proto-dotnet-plugin`
   (v0.3.0) fails on proto 0.58.2/Windows with `%1 is not a valid Win32 application. (os error
   193)` during native install (extracts `~/.dotnet/sdk/<ver>` but never places the `dotnet` host
   executable). Evaluate the `RemiKalbe/proto-dotnet-plugin` TOML plugin (registry default for
   `dotnet`; installs version-per-directory with shims but does NOT handle the co-located
   DOTNET_ROOT SDK layout) or implement our own `setup_toolchain` installing into a shared
   `DOTNET_ROOT` (which alone flips `supports_tier_3` to true without proto tool functions).
4. **Soak testing on a real multi-solution repo (N=50+ projects)** — measure the batched
   `extend_project_graph` wall time at scale (a single traversal invocation with parallel
   in-process evaluation; ~1s startup + well under 200ms/project marginal on a 60-project
   synthetic workspace). If cold graph builds still hurt on very large repos, add per-project
   eval-result caching keyed on hashes of the restore-relevant files.
5. **parse_manifest, alias support, setup_environment, sync_project, .targets globs, NuGet cache
   pruning** — `parse_manifest` for csproj files, project `alias` from `AssemblyName`,
   `setup_environment`, `sync_project`, `*.targets` scaffold globs (can affect restore in rare
   cases), and NuGet user-cache handling in `prune_docker`.
