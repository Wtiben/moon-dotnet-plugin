# Changelog

## 0.3.1

#### 🐞 Fixes

- An unresolvable SDK pin with `version:` configured now skips .NET graph evaluation entirely
  instead of letting every project fall back to its own MSBuild invocation. 0.3.0 returned an empty
  batch there, which sent each project through the per-project fallback and reproduced the dotnet
  host's output once per project — the exact noise the single report exists to replace. Observed on a
  232-project workspace: one warning instead of one per project.

## 0.3.0

#### 🚀 Updates

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
- The `dotnet` toolchain settings now carry full documentation, which is what moon renders into the
  JSON schema — so an editor with `$schema` on `.moon/toolchains.yml` shows the actual inference
  rules, the resolution order behind `dotnetRoot`, and that `--locked-mode` is added to
  `restoreArgs` automatically, instead of a one-line summary.
- Task inference now reports the task ids it yielded to an inherited task file, naming the file
  and how to change it. Yielding was silent, so a workspace whose `.moon/tasks/*.yml` defines
  `build` simply had no inferred build tasks and no visible reason why.

#### 💥 Breaking

- `extend_task_command` no longer injects `DOTNET_CLI_TELEMETRY_OPTOUT`. It was only set when a
  `DOTNET_ROOT` was resolved, so it never applied to the common case of a system SDK on `PATH`; no
  other moon toolchain injects vendor environment variables; and it does not suppress the "Welcome
  to .NET" first-run banner that its comment claimed to silence (`DOTNET_NOLOGO` does). Set either
  in a task's `env` if you want them. `DOTNET_ROOT` and `PATH` are still injected.

#### 🐞 Fixes

- An unresolvable `global.json` SDK pin no longer fails the project graph when `version:` is
  configured for moon to install an SDK. The graph is built before the action pipeline runs, so
  failing there deadlocked the very bootstrap that setting exists for — a system SDK 8 plus a
  subtree pinning 10.x plus `version: '10.0'` could never get past the first command. It now warns
  and contributes nothing for that run. Without `version:` configured nothing will install the
  missing SDK, so that case still fails with the same guidance as before.
- A `ProjectReference` that has to be resolved through the workspace-relative suffix index now takes
  the **longest** matching suffix instead of the first in sort order. With projects at both `lib` and
  `src/lib` holding an `App.csproj`, a reference to `src/lib/App.csproj` matched both and the
  shorter `/lib/...` won — a dependency edge pointing at the wrong project. Only reachable when the
  exact real-path lookup misses, i.e. Windows 8.3 short names.
- The evaluated-package-set cache digest now frames each file by name and byte length instead of
  concatenating contents. Without framing, the same bytes distributed differently across two files
  produced an identical digest — moving a `<PackageVersion>` declaration from the end of
  `Directory.Build.props` to the start of `Directory.Packages.props`, a routine Central Package
  Management migration, left every task hash unchanged. Cached entries invalidate once on upgrade.
- `global.json` SDK-pin discovery now stops at the nearest file, matching the dotnet host, which
  resolves exactly one `global.json` and neither merges them nor keeps searching. A nearer file
  declaring no `sdk.version` previously let an ancestor's pin apply, so the `~/.dotnet` fallback
  could be rejected against a pin that does not govern that directory — and the warning named the
  wrong file. The sibling test-runner lookup already implemented this rule; the two now agree.
- Batch evaluation now identifies failing projects from MSBuild's positionless
  `<path> : error ...` diagnostics, not just the `<path>(line,col): error CODE:` form. An
  unresolvable SDK reference emits the former, so no offender was detected, the retry-without-it
  never fired, and the entire batch was discarded in favour of per-project evaluation — correct, but
  it silently gave up the batching speedup on exactly the workspaces that need it.
- `setup_toolchain` now fetches the `dotnet-install` script once instead of on every moon
  invocation. A code comment claimed moon fingerprint-caches this action; it does not —
  `create_hash_and_return_lock` has no "manifest exists, skip" short-circuit — so every single
  command re-downloaded the script over HTTPS, which broke offline and air-gapped workspaces and
  cost everyone else seconds per command. A fully-qualified `version:` already skipped the network
  entirely when that SDK was present; channels and aliases (`'8.0'`, `'lts'`) still run the script
  each time, since only the server can resolve which patch a channel points at, and that is now
  documented rather than implied.
- The evaluated package-set cache is no longer written when evaluation was incomplete. A missing
  `dotnet`, an unloadable project, or a project file without a host path previously persisted a
  **partial or empty** set under a digest that then validated forever. Since that set is the only
  hash signal for a workspace without `packages.lock.json`, a package bump stopped invalidating task
  hashes and moon served stale builds — and installing the SDK afterwards did not recover it. The
  in-process memo is still written either way, so a genuinely absent SDK is not re-probed once per
  task.
- The inferred `test` and `publish` tasks now mark their `~:build` dependency **optional**. moon
  defaults `~:` deps to mandatory, so `inferTasks: ['test']` (or `['publish']`) produced a task
  depending on a `build` that was never inferred, and project-graph construction failed outright
  with `Invalid dependency ~:build ... target does not exist` instead of simply losing the ordering
  edge.
- A missing `dotnet` executable no longer fails the project graph. `extend_project_graph` now warns
  and contributes nothing, matching `parse_manifest` and `hash_task_contents`. The graph is built
  before the action pipeline runs, so a `version:` configured for the toolchain to install is not
  installed yet on a fresh machine — erroring meant the first `moon` command failed the
  whole-workspace graph, for every toolchain, before moon could install the SDK it was told to
  install. (A `dotnet` that exists but has no SDK satisfying a `global.json` pin still fails once
  with guidance, as before.)
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

#### 💥 Breaking

- `inferTasks` now defaults to **enabled** (previously off and experimental). Projects gain
  `build`/`test`/`run`/`publish` tasks automatically; opt out with `inferTasks: false` or
  select granularly with a list. If an inherited task file defines the same task ids, those
  ids are skipped automatically.
- The `hash_task_contents` payload shape changed (`lockfile`/`props` → `lockfiles`/`configs`
  maps), so all task hashes invalidate once on upgrade.

#### 🚀 Updates

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

#### 🐞 Fixes

- `initialize_toolchain` now points at the real repository docs URL.

## 0.1.0

#### 🚀 Updates

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
