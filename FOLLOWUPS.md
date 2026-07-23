# Follow-ups (deferred from v1)

1. **Full test matrix (props chains, multi-TFM, .slnx, Conditions, CPM, alternate lock names)** —
   fixtures for `Directory.Build.props` inheritance chains (nested props, `GetPathOfFileAbove`
   chaining), multi-targeting TFMs (per-TFM re-evaluation with `-p:TargetFramework=`), `.slnx`
   solution enumeration, MSBuild `Condition`s, Central Package Management
   (`Directory.Packages.props` + versionless PackageReference), and `packages.<project>.lock.json`
   alternate lock file names.
2. **Static XML fast-path for ProjectReference parsing** — parse csproj XML directly when no
   props/conditions/CPM are detected, falling back to full MSBuild evaluation. Perf optimization
   only; correctness stays with evaluation.
3. **CI: build/test + release workflows** — GitHub Actions with `moonrepo/setup-rust@v1`
   (`targets: wasm32-wasip1`, `bins: cargo-nextest`) for tests, and
   `moonrepo/build-wasm-plugin@v0` + `ncipollo/release-action@v1` for releases on tags like
   `dotnet_toolchain-v0.1.0` (mirror `moonrepo/plugins` `.github/workflows/release.yml`);
   consume via `github://<owner>/moon-dotnet-plugin@vX.Y.Z` locators.
4. **Registry submission: moonrepo third-party plugin registry** — submit an entry once released.
5. **Tier 3 proper: replace Phault delegation** — the archived `Phault/proto-dotnet-plugin`
   (v0.3.0) fails on proto 0.58.2/Windows with `%1 is not a valid Win32 application. (os error
   193)` during native install (extracts `~/.dotnet/sdk/<ver>` but never places the `dotnet` host
   executable). Evaluate the `RemiKalbe/proto-dotnet-plugin` TOML plugin (registry default for
   `dotnet`; installs version-per-directory with shims but does NOT handle the co-located
   DOTNET_ROOT SDK layout) or implement our own `setup_toolchain` installing into a shared
   `DOTNET_ROOT` (which alone flips `supports_tier_3` to true without proto tool functions).
6. **Soak testing on a real multi-solution repo (N=50+ projects)** — measure
   `extend_project_graph` wall time at scale (~0.5s MSBuild eval per project ⇒ ~25s cold at
   N=50); explore batching/parallel evaluation strategies.
7. **parse_manifest, alias support, setup_environment, sync_project, .targets globs, NuGet cache
   pruning** — `parse_manifest` for csproj files, project `alias` from `AssemblyName`,
   `setup_environment`, `sync_project`, `*.targets` scaffold globs (can affect restore in rare
   cases), and NuGet user-cache handling in `prune_docker`.
