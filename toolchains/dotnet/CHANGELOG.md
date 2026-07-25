# Changelog

## Unreleased

#### 🚀 Features

- Task hashing now reuses the package sets from the batched graph evaluation instead of spawning
  one `dotnet msbuild` per project. Workspaces without `packages.lock.json` were paying a full
  evaluation per project on every command that hashes tasks — measured on 40 projects with all
  builds already cached: **5s with the shared results vs 115s without**, scaling linearly with
  project count. The cache lives in `.moon/cache/dotnet-toolchain/eval/` and is keyed on a digest
  of each project's own files plus every config file up to the workspace root, so an edit to any
  of them re-evaluates rather than serving a stale set.

- A missing SDK now fails the graph build once, with guidance: which `global.json` pins which
  version, and the three ways out (install it, set `version:`, or point `dotnetRoot` at a
  satisfying SDK). Previously the batch failure fell back to per-project evaluation, so a
  232-project workspace produced 233 copies of the dotnet host's output and then a silently
  empty graph. Detection keys on the host's (non-localized) help URL.
- `setup_toolchain` warns when the SDKs it installed cannot satisfy a `global.json` pin found in
  the workspace — e.g. `version: '8.0'` configured while a subtree pins 10.x, which otherwise
  fails much later, once tasks run.
- Task inference now reports the task ids it yielded to an inherited task file, naming the file
  and how to change it. Yielding was silent, so a workspace whose `.moon/tasks/*.yml` defines
  `build` simply had no inferred build tasks and no visible reason why.

#### 🐛 Fixes

- The inferred `test` task now works under **Microsoft.Testing.Platform**, not just VSTest. When a
  project directory holds several project files, the task has to name one — and MTP's `dotnet test`
  takes it through `--project` and rejects a positional path, while classic VSTest mode rejects
  `--project` (both verified against SDK 10.0.201). The flavour is picked from the governing
  `global.json` (`{"test": {"runner": "Microsoft.Testing.Platform"}}`) or a project's own
  `TestingPlatformDotnetTestSupport`. Detection of test projects was already correct: MTP projects
  still evaluate `IsTestProject=true`.
- MSBuild evaluation and tasks now resolve the **same** SDK. Evaluation runs under the same
  `DOTNET_ROOT` that `extend_task_command` injects (by invoking that root's `dotnet` muxer —
  `DOTNET_ROOT` alone does not redirect SDK resolution), and from an explicit working directory:
  the deepest directory containing every .NET project, so a `global.json` in that subtree governs
  evaluation exactly as it governs the tasks that run there. Previously evaluation used whichever
  `dotnet` was on `PATH` and resolved `global.json` from wherever moon happened to be invoked.
- The `~/.dotnet` fallback for `DOTNET_ROOT` is now validated against the workspace's `global.json`
  SDK pin (`version` + `rollForward`, including `latest*`/`disable` and prerelease rules). A
  leftover install there — a stale proto experiment, say — is no longer injected over a working
  system SDK, which previously made every task fail with the dotnet host's "compatible SDK was not
  found" while graph evaluation still succeeded. Skips and uses of the fallback are both logged.

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
- Projects now gain their evaluated `AssemblyName` as a moon **alias**, so they can be
  addressed by .NET name (`moon run MyCompany.App:build`) and `moon docker prune` can target
  packages per toolchain.
- `parse_manifest` (MSBuild-based, no XML parsing): `PackageReference` items become moon
  manifest dependencies — versionless ones as workspace-`inherited` (Central Package
  Management) — and `Directory.Packages.props` `PackageVersion` items resolve what they inherit.
  `Directory.Packages.props` is registered as the toolchain's manifest file, so CPM version
  bumps now re-trigger dependency installs.
- `setup_environment` runs `dotnet tool restore` when a local tool manifest
  (`.config/dotnet-tools.json`) is found, searching from the dependencies root upward to the
  workspace root the way the dotnet CLI does. Keyed on the manifest's content, so edits re-run
  the restore and repeat runs skip it.
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
- Test coverage for the remaining edge cases: `Directory.Build.props` inheritance chains
  (`GetPathOfFileAbove`), MSBuild `Condition`s on references and packages, multi-targeted
  (`TargetFrameworks`) projects pinning the documented outer-build behavior, and an opt-in
  soak test that generates a 60-project workspace and verifies every inferred edge
  (`cargo test -- --ignored soak`; 3.6s locally).

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
