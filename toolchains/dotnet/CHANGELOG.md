# Changelog

## 0.2.0

#### 🚀 Features

- **Tier 3**: `setup_toolchain` installs the .NET SDK when `version:` is configured in
  `.moon/toolchains.yml`, via the official dotnet-install scripts into `~/.dotnet` (or
  `dotnetRoot`). `X.Y` installs a channel, exact versions install pinned (and skip when
  already present), `lts`/`sts`/`preview` map to named channels.
- Task inference is no longer experimental and is **enabled by default**: every project gets a
  cached `build` task (`--no-dependencies` + `deps: ^:build` so moon orchestrates and caches the
  graph), test projects get `test` (`--no-build` on top of the build dep), and `Exe`/`WinExe`
  projects get `run` (never cached, excluded from CI) and `publish` (cached, single-TFM only).
  The setting is now granular: `inferTasks: true | false | ['build', 'test', 'run', 'publish']`.
  Commands pin the evaluated `Configuration` (dotnet `publish` defaults to Release on .NET 8+
  while `build` defaults to Debug — they must agree for `--no-build`); outputs come from the
  evaluated `BaseOutputPath`/`PublishDir` (tasks with output paths outside the workspace run
  uncached instead of caching the wrong directory); inputs exclude the evaluated
  output/intermediate dirs so task hashes are stable. Inference never overrides your own tasks:
  project `moon.yml` tasks replace inferred ones wholesale, and task ids defined in applicable
  inherited task files (`.moon/tasks*`) are not inferred at all.
- Project-graph MSBuild evaluation is now batched: a single traversal invocation evaluates
  every project in parallel (in-process worker nodes, target injected via
  `CustomAfterMicrosoftCommon(CrossTargeting)Targets`) instead of spawning one `dotnet msbuild`
  process per project — a 60-project workspace went from ~3 minutes to ~11 seconds in local
  measurements. Per-project evaluation remains as an automatic fallback for anything missing
  from the batch output.
- F# (`.fsproj`) and VB (`.vbproj`) projects are now supported and test-covered, including
  cross-language `ProjectReference` inference (C# → F# → VB fixture). MSBuild evaluation
  was always language-agnostic; the scope-cut caveat is gone.
- Central Package Management (`Directory.Packages.props` + versionless `PackageReference`)
  is verified and test-covered: versionless references hash as `*`, with the pinned versions
  contributing through the `Directory.Packages.props` content hash.
- `packages.<project>.lock.json` alternate lock file names (via `NuGetLockFilePath`) are now
  recognized for `--locked-mode` restores, lock-file hashing, and dependencies-root location.
- Task hashing now also includes `Directory.Build.targets`, `Directory.Build.rsp`,
  `nuget.config` (any casing), and `global.json` from the project directory up to the
  workspace root — and these config files are hashed even when a lock file is present
  (previously a lock file skipped props hashing entirely).
- Docker scaffold globs now include `**/*.targets`, `Directory.Build.rsp`, cased
  `NuGet.Config` variants, and `packages.*.lock.json`.
- `.slnx` dependency-root marker behavior is now test-covered.

#### 🐛 Fixes

- `initialize_toolchain` now points at the real repository docs URL.

#### ⚠️ Breaking

- `inferTasks` now defaults to **enabled** (previously off and experimental). Projects gain
  `build`/`test`/`run`/`publish` tasks automatically; opt out with `inferTasks: false` or
  select granularly with a list. If an inherited task file defines the same task ids, those
  ids are skipped automatically.
- The `hash_task_contents` payload shape changed (`lockfile`/`props` → `lockfiles`/`configs`
  maps), so all task hashes invalidate once on upgrade.

## 0.1.0

#### 🚀 Features

- Initial release.
- Tier 1: project usage detection (`*.csproj`/`*.sln`/`global.json`/props files), config schema,
  Docker metadata with restore-layer scaffold globs (`bin`/`obj` excluded).
- Tier 2: moon project-graph dependencies inferred from real MSBuild evaluation of
  `ProjectReference` items (requires .NET SDK 8+).
- Tier 2: `dotnet restore` dependency installs with automatic `--locked-mode` when a
  `packages.lock.json` is present.
- Tier 2: task-content hashing from the raw lock file, or the evaluated `PackageReference` set
  plus `Directory.Build.props`/`Directory.Packages.props` contents.
- Tier 2: `packages.lock.json` parsing, Docker pruning of `bin`/`obj`, and
  `DOTNET_ROOT`/`PATH` injection into task environments.
- Experimental `inferTasks` setting: contributes `test` (test projects) and `run`
  (Exe/WinExe projects) tasks.
