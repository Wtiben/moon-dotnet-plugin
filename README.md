# moon-dotnet-plugin

A [moon](https://moonrepo.dev) 2.x toolchain WASM plugin for the .NET ecosystem (SDK-style C# projects).

Provides:

- **Tier 1** — project usage detection (`*.csproj`/`*.sln`/`global.json`/props files),
  config schema, Docker metadata & scaffold globs.
- **Tier 2** — moon project-graph dependencies inferred from **real MSBuild evaluation**
  of `ProjectReference` items (`dotnet msbuild -getProperty/-getItem`, .NET SDK 8+),
  `dotnet restore` dependency installs (with automatic `--locked-mode`), task-content
  hashing from lock files or the evaluated package set, `packages.lock.json` parsing,
  Docker pruning of `bin`/`obj`, and `DOTNET_ROOT` injection into task environments.

Dependency extraction shells out to MSBuild instead of statically parsing XML, so
`Directory.Build.props` chains, Central Package Management, SDK defaults, and
`Condition`s all resolve correctly. This costs ~0.5s per project per graph build.

## Usage

`.moon/toolchains.yml`:

```yaml
dotnet:
  plugin: 'github://Wtiben/moon-dotnet-plugin@v0.1.0'
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
language: 'csharp'   # moon 2.3.3 rejects 'c#'

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

SDK-style projects only (no legacy csproj), `dotnet` CLI only, C# focus (`.fsproj`/
`.vbproj` may work incidentally, untested), no NuGet workloads, no global tools,
outer-build evaluation only for multi-targeted projects. See FOLLOWUPS.md.

## SDK installation (tier 3)

This plugin does **not** install the .NET SDK itself — it exports no `setup_toolchain`
or proto tool functions, so moon treats the toolchain as tier 1+2 only. A `version:`
field under `dotnet:` in `.moon/toolchains.yml` will NOT drive an SDK install. The SDK
is expected to come from either:

1. **proto** via a community dotnet plugin, installing into `~/.dotnet`. The plugin's
   `extend_task_command` injects `DOTNET_ROOT` + `PATH` into task environments when it
   finds a real SDK layout there (or an explicit `dotnetRoot` setting / existing
   `DOTNET_ROOT` env var).
2. **A system-installed dotnet** on `PATH` — the always-working fallback. When no
   DOTNET_ROOT candidate is found, `extend_task_command` is a no-op and tasks use
   whatever `dotnet` resolves on the system.

> **Caveat**: the archived community plugin `Phault/proto-dotnet-plugin` (v0.3.0) was
> tested on proto 0.58.2 on Windows and **fails during native install** with
> `%1 is not a valid Win32 application. (os error 193)` — it extracts `~/.dotnet/sdk/<ver>`
> but never places the `dotnet` host executable, leaving a broken root. See FOLLOWUPS.md
> for the tracked replacement options. Because `~/.dotnet` is also the dotnet CLI's
> user-level cache directory, the plugin only treats it as a DOTNET_ROOT when the
> `dotnet` executable actually exists at its root.

## ⚠️ Hashing without a lock file is approximate

Without `packages.lock.json`, moon's task hashes are computed from the
**declared/evaluated** `PackageReference` set (plus the contents of
`Directory.Build.props` / `Directory.Packages.props` up the tree) — floating
versions (`1.*`) and unpinned transitive upgrades will **NOT** invalidate caches.

**Commit `packages.lock.json`** (generate it with `dotnet restore --use-lock-file`)
for exact hashing: the plugin then hashes the raw lock file content (which pins the
full resolved set including content hashes) and automatically passes `--locked-mode`
to `dotnet restore` during dependency installation, failing fast (NU1004) when the
lock file drifts from the declared dependencies.

Note: moon's install-dependencies action fingerprints only the lock file (this
toolchain registers no manifest file names), so editing a `.csproj` alone does not
re-trigger the install action until the lock file changes too — another reason to
keep lock files committed and current.

## Docker

- `moon docker scaffold <project>`: the configs phase copies exactly the
  restore-relevant files (`*.csproj`/`*.sln`/`*.props`/`nuget.config`/
  `packages.lock.json`/`global.json`), with `bin`/`obj` explicitly excluded
  (generated `obj/*.nuget.g.props` would otherwise match). The sources phase
  copies full project sources by moon design.
- `prune_docker` removes `bin`/`obj` directories in the dependencies root and
  each focused project. NuGet's user-level cache is not touched in v1.
- Add `.moon/cache` (and ideally `.moon/docker`) to `.dockerignore`.

## Task inference (`inferTasks`, experimental)

When enabled, projects evaluating `IsTestProject=true` **or** referencing
`Microsoft.NET.Test.Sdk` get a `test` task (`dotnet test`), and `OutputType`
`Exe`/`WinExe` projects get a `run` task (`dotnet run`). Note `IsTestProject`
is only set by the test SDK's build props after a restore, hence the package
reference fallback. Off by default.

## Development notes

- Build: `cargo build --target wasm32-wasip1`
- Test: `cargo test --workspace --no-default-features` (requires the wasm to be built first)
- Or both: `bash scripts/build-and-test.sh`
- On this dev machine the host toolchain is `x86_64-pc-windows-gnu` (no MSVC C++ build
  tools installed; the GNU toolchain ships a self-contained linker).

### moon workspace facts (verified against moon 2.3.3)

- `moon toolchain info dotnet` requires the plugin locator as an explicit second
  argument (it does not read custom entries from `.moon/toolchains.yml`):
  `moon toolchain info dotnet "file://../moon-dotnet-plugin/target/wasm32-wasip1/debug/dotnet_toolchain.wasm"`.
  The locator is resolved relative to the current working directory.
- In `moon.yml`, `language: 'c#'` is rejected by moon 2.3.3 ("Invalid fallback
  variant"); use `language: 'csharp'`. The project-level toolchain key is
  `toolchains` (plural): `toolchains: { default: 'dotnet' }`.

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
