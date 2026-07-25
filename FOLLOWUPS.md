# Follow-ups (deferred from v1)

1. **Remaining test matrix (props chains, multi-TFM, Conditions, .slnx enumeration)** —
   fixtures for `Directory.Build.props` inheritance chains (nested props, `GetPathOfFileAbove`
   chaining), multi-targeting TFMs (per-TFM re-evaluation with `-p:TargetFramework=`), and
   MSBuild `Condition`s. `.slnx`/`.sln` member *enumeration* (feeding `output.members` from
   parsed solution files) remains deliberately unimplemented — moon's `workspace.yml` owns
   project discovery; revisit only if moon grows a use for members. (CPM, alternate lock
   names, F#/VB, and `.slnx` marker behavior gained fixtures/tests in v0.2.)
2. **Registry submission: moonrepo third-party plugin registry** — submit an entry once released.
3. **Soak testing on a real multi-solution repo (N=50+ projects)** — measure the batched
   `extend_project_graph` wall time at scale (a single traversal invocation with parallel
   in-process evaluation; ~1s startup + well under 200ms/project marginal on a 60-project
   synthetic workspace). If cold graph builds still hurt on very large repos, add per-project
   eval-result caching keyed on hashes of the restore-relevant files.
4. **More inferred tasks: `pack`, `watch`, `clean`** — `pack` needs an intent signal
   (`IsPackable` defaults to true for every classlib, so "pack if packable" would spam pack
   tasks); `watch` (`dotnet watch`, persistent + local) and `clean` are trivial but low-value.
   Also consider a config option to override the pinned build configuration (e.g.
   `taskConfiguration: 'Release'`), and a real-moon e2e harness in CI (cache hit/miss/cascade
   round-trip — currently verified manually and by the sandbox tests).
5. **parse_manifest, alias support, setup_environment, sync_project, NuGet cache pruning** —
   `parse_manifest` for csproj files, project `alias` from `AssemblyName`,
   `setup_environment`, `sync_project`, and NuGet user-cache handling in `prune_docker`.
