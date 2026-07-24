# moon-dotnet-plugin

A [moon](https://moonrepo.dev) 2.x toolchain WASM plugin for the .NET ecosystem
(SDK-style C#, F#, and VB projects).

Provides:

- **Tier 1** — project usage detection (`*.{csproj,fsproj,vbproj}`/`*.{sln,slnx}`/
  `global.json`/`Directory.Build.*`/`Directory.Packages.props`/`nuget.config`),
  config schema, Docker metadata & scaffold globs.
- **Tier 2** — moon project-graph dependencies inferred from **real MSBuild evaluation**
  of `ProjectReference` items (`dotnet msbuild -getProperty/-getItem`, .NET SDK 8+),
  `dotnet restore` dependency installs (with automatic `--locked-mode`), task-content
  hashing from lock files or the evaluated package set, `packages.lock.json` parsing,
  Docker pruning of `bin`/`obj`, and `DOTNET_ROOT` injection into task environments.
- **Tier 3** — .NET SDK installation driven by `version:` in `.moon/toolchains.yml`,
  via the official `dotnet-install` scripts into a shared `DOTNET_ROOT`.

Dependency extraction shells out to MSBuild instead of statically parsing XML, so
`Directory.Build.props` chains, Central Package Management, SDK defaults, and
`Condition`s all resolve correctly — these need no special handling in the plugin
because the real evaluation engine resolves them. All projects are evaluated in
**one batched MSBuild invocation** — a generated traversal project (under
`.moon/cache/dotnet-toolchain/`) fans out to every project with parallel in-process
worker nodes — so the dotnet/MSBuild startup cost is paid once per graph build
instead of once per project (~11s vs ~3min for a 60-project workspace in local
measurements). Any project missing from the batch output (e.g. a broken csproj)
automatically falls back to individual evaluation.

Because MSBuild evaluation is language-agnostic, **`.fsproj` and `.vbproj` projects
are fully supported**, including cross-language `ProjectReference`s (covered by the
`mixed-lang` test fixture: C# → F# → VB). **Central Package Management**
(`Directory.Packages.props` + versionless `PackageReference`) is likewise supported
and test-covered: pinned versions reach the task hash through the
`Directory.Packages.props` content hash. `.slnx` solution files act as dependency-root
markers exactly like `.sln` (solution files are never parsed — moon's `workspace.yml`
is the source of project discovery).

## Usage

`.moon/toolchains.yml`:

```yaml
dotnet:
  plugin: 'github://Wtiben/moon-dotnet-plugin@v0.2.0'
  inferDependencies: true   # default
  inferTasks: false         # default; experimental
  restoreArgs: []           # extra args for `dotnet restore`
  # dotnetRoot: 'C:/Users/me/.dotnet'
```

moon downloads the wasm from the GitHub release and caches it — nothing to install.
For local development a `file://` locator also works:
`plugin: 'file://../path/to/dotnet_toolchain.wasm'` (relative from the `.moon` dir).

`moon.yml` (per project):

```yaml
language: 'csharp'   # moon rejects 'c#' (verified through 2.4.5)

toolchains:
  default: 'dotnet'

tasks:
  build:
    command: 'dotnet build --no-restore'
    inputs:
      - '**/*.cs'
      - '*.csproj'
```

Requires a .NET SDK 8+ (`-getProperty`/`-getItem` JSON output needs MSBuild 17.8+).

## Scope cuts (v1)

SDK-style projects only (no legacy csproj), `dotnet` CLI only, no NuGet workloads,
no global tools, outer-build evaluation only for multi-targeted projects. Custom
`<Import>`s outside the `Directory.Build.*` conventions affect the *evaluated package
set* (captured in hashes) but their file contents are not themselves hashed — build
behavior changes in such files won't invalidate caches. See FOLLOWUPS.md.

## SDK installation (tier 3)

Setting `version:` under `dotnet:` in `.moon/toolchains.yml` makes moon install the
SDK during toolchain setup, using the official
[dotnet-install scripts](https://learn.microsoft.com/en-us/dotnet/core/tools/dotnet-install-script):

```yaml
dotnet:
  plugin: 'github://Wtiben/moon-dotnet-plugin@v0.2.0'
  version: '8.0'   # channel; or '8.0.404' (exact), 'lts', 'sts', 'preview'
```

- Installs into **`~/.dotnet`** by default (SDK versions lay out side-by-side), or
  into `dotnetRoot` when configured — the same root `extend_task_command` injects as
  `DOTNET_ROOT`/`PATH`, so tasks find the installed SDK with no further wiring. The
  user's shell profile is never touched (`--no-path`).
- Version semantics pass through to the script: `X.Y` installs the latest patch of
  that channel, fully-qualified versions install pinned (and short-circuit when that
  exact SDK is already present), `lts`/`sts`/`preview` map to the named channels.
- `global.json` stays a **runtime** concern: the dotnet host picks the matching SDK
  out of `DOTNET_ROOT` at execution time; setup does not parse it. If `global.json`
  demands a version that isn't installed, `dotnet` itself reports the error.

Without a `version:` setting, moon skips toolchain setup ("use globals") and the SDK
is expected from either a proto-managed install in `~/.dotnet` or a system `dotnet`
on `PATH` — when no `DOTNET_ROOT` candidate exists, `extend_task_command` is a no-op.
Because `~/.dotnet` doubles as the dotnet CLI's user-level cache directory, it only
counts as a `DOTNET_ROOT` when the `dotnet` executable actually exists at its root.

## ⚠️ Hashing without a lock file is approximate

Task hashes always include the contents of every `Directory.Build.props`,
`Directory.Build.targets`, `Directory.Build.rsp`, `Directory.Packages.props`,
`nuget.config` (any casing), and `global.json` from the project directory up to
the workspace root — changing any of them invalidates affected task caches even
when the package set is pinned by a lock file.

Without a lock file, the package part of the hash is computed from the
**declared/evaluated** `PackageReference` set — floating versions (`1.*`) and
unpinned transitive upgrades will **NOT** invalidate caches.

**Commit `packages.lock.json`** (generate it with `dotnet restore --use-lock-file`)
for exact hashing: the plugin then hashes the raw lock file content (which pins the
full resolved set including content hashes) and automatically passes `--locked-mode`
to `dotnet restore` during dependency installation, failing fast (NU1004) when the
lock file drifts from the declared dependencies. Renamed lock files following the
`packages.<project>.lock.json` convention (via `NuGetLockFilePath`) are recognized
too.

Note: moon's install-dependencies action fingerprints only the lock file (this
toolchain registers no manifest file names), so editing a `.csproj` alone does not
re-trigger the install action until the lock file changes too — another reason to
keep lock files committed and current.

## Docker

- `moon docker scaffold <project>`: the configs phase copies exactly the
  restore-relevant files (`*.{csproj,fsproj,vbproj}`/`*.{sln,slnx}`/`*.props`/
  `*.targets`/`Directory.Build.rsp`/`nuget.config`/lock files/`global.json`),
  with `bin`/`obj` explicitly excluded (generated `obj/*.nuget.g.props` and
  `obj/*.nuget.g.targets` would otherwise match). The sources phase copies full
  project sources by moon design.
- `prune_docker` removes `bin`/`obj` directories in the dependencies root and
  each focused project. NuGet's user-level cache is not touched in v1.
- Add `.moon/cache` (and ideally `.moon/docker`) to `.dockerignore`.

## Task inference (`inferTasks`, experimental)

When enabled, projects evaluating `IsTestProject=true` **or** referencing
`Microsoft.NET.Test.Sdk` get a `test` task (`dotnet test`), and `OutputType`
`Exe`/`WinExe` projects get a `run` task (`dotnet run`). Note `IsTestProject`
is only set by the test SDK's build props after a restore, hence the package
reference fallback. Off by default.

## Releasing

Releases are tag-driven and gated. To publish version `X.Y.Z`:

1. Bump `version` in `toolchains/dotnet/Cargo.toml`.
2. Add a `## X.Y.Z` entry to `toolchains/dotnet/CHANGELOG.md`.
3. Commit, then: `git tag vX.Y.Z && git push origin main vX.Y.Z`

The Release workflow then enforces, in order: the full CI test matrix (ubuntu +
windows), tag == crate version, changelog entry exists, and a smoke test that loads
the exact built wasm with a pinned moon binary (`MOON_SMOKE_VERSION` in
`release.yml` — bump it deliberately). Only after all gates pass does it publish the
ghcr.io OCI artifact and create the GitHub release (with `immutableCreate`, so assets
are locked after creation).

Guarantees:

- A commit that fails tests cannot be released, even if tagged.
- A tag that doesn't match the crate version (or lacks a changelog entry) fails fast.
- Published `v*` tags cannot be deleted or moved (repository ruleset
  "protect-release-tags"); re-releasing a version is a hard error. The escape hatch
  is deliberately manual: temporarily disable the ruleset in repo settings.
- The verify path can be dry-run anytime via the workflow's "Run workflow" button
  (`workflow_dispatch`) — publishing steps only ever run on tag pushes.

## Development notes

- Build: `cargo build --target wasm32-wasip1`
- Test: `cargo test --workspace --no-default-features` (requires the wasm to be built first)
- Or both: `bash scripts/build-and-test.sh`
- On this dev machine the host toolchain is `x86_64-pc-windows-gnu` (no MSVC C++ build
  tools installed; the GNU toolchain ships a self-contained linker). The repo's
  `rust-toolchain.toml` (required by `moonrepo/build-wasm-plugin` to select
  `wasm32-wasip1` — without it the action tries the removed `wasm32-wasi` target)
  would resolve to the MSVC host toolchain, so a rustup directory override keeps
  local builds on GNU: `rustup override set stable-x86_64-pc-windows-gnu --path .`

### moon workspace facts (verified against moon 2.3.3 and 2.4.5)

- `moon toolchain info dotnet` requires the plugin locator as an explicit second
  argument (it does not read custom entries from `.moon/toolchains.yml`):
  `moon toolchain info dotnet "file://../moon-dotnet-plugin/target/wasm32-wasip1/debug/dotnet_toolchain.wasm"`.
  The locator is resolved relative to the current working directory.
- In `moon.yml`, `language: 'c#'` is rejected ("Invalid fallback variant");
  use `language: 'csharp'`. The project-level toolchain key is
  `toolchains` (plural): `toolchains: { default: 'dotnet' }`.
- moon 2.4.x introduced no toolchain WASM API changes (2.4.0 added built-in
  Poetry/Ruby toolchains only); the plugin runs unmodified on 2.0–2.4.

### Test harness facts (verified against vendored sources)

- **`exec_command` in the test sandbox is REAL** — `warpgate-0.30.5/src/host.rs:134`
  (`fn exec_command`) spawns an actual `std::process::Command`, resolving the executable
  from the host `PATH` via `find_command_on_path`. moon's `crates/pdk-test-utils`
  sandbox registers these warpgate host functions unmocked (only moon's `load_*`
  data functions are mocked). Sandbox tests that shell out to `dotnet` therefore
  require a .NET SDK on the test machine.
- **`find_wasm_file` prefers `release` over `debug`** (`warpgate-0.30.5/src/test_utils.rs`,
  `profiles = ["release", "debug"]`). Never leave a stale
  `target/wasm32-wasip1/release/dotnet_toolchain.wasm` lying around while running unit
  tests against a freshly built debug wasm — delete the release artifact first.
