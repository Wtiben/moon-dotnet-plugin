# moon-dotnet-plugin

[![CI](https://github.com/Wtiben/moon-dotnet-plugin/actions/workflows/ci.yml/badge.svg)](https://github.com/Wtiben/moon-dotnet-plugin/actions/workflows/ci.yml)

A [moon](https://moonrepo.dev) 2.x toolchain WASM plugin for the .NET ecosystem —
SDK-style C#, F#, and VB projects.

It gives a moon workspace a real understanding of your .NET projects: the project
graph is derived from **actual MSBuild evaluation** rather than XML parsing, so
`Directory.Build.props` chains, Central Package Management, SDK defaults, and
`Condition`s all resolve exactly as they do in a normal build.

- **Project graph** — moon dependencies inferred from `ProjectReference` items,
  project aliases from `AssemblyName`.
- **Tasks** — `build`, `test`, `run`, and `publish` contributed automatically, so a
  zero-config workspace has working tasks (see [Task inference](#task-inference)).
- **Dependencies** — `dotnet restore` as moon's install-dependencies action, with
  automatic `--locked-mode` when a lock file is present, plus local tool restore.
- **Caching** — task hashing from lock files or the evaluated package set, together
  with every relevant `Directory.Build.*` / `Directory.Packages.props` /
  `nuget.config` / `global.json` above the project.
- **SDK installation** — optional; installs the .NET SDK from `version:` via the
  official `dotnet-install` scripts.
- **Docker** — restore-layer scaffold globs and `bin`/`obj` pruning.

All projects are evaluated in **one batched MSBuild invocation** — a generated
traversal project fans out to every project with parallel in-process worker nodes —
so MSBuild's startup cost is paid once per graph build instead of once per project
(~11s vs ~3min for a 60-project workspace in local measurements). Any project missing
from the batch output (a broken csproj, for example) falls back to individual
evaluation automatically.

## Requirements

- moon 2.0 or newer.
- .NET SDK 8 or newer — dependency inference relies on MSBuild 17.8+ `-getProperty` /
  `-getItem` JSON output. The plugin can install the SDK for you; see
  [SDK installation](#sdk-installation).

## Installation

Add the toolchain to `.moon/toolchains.yml`:

```yaml
dotnet:
  plugin: 'github://Wtiben/moon-dotnet-plugin@v0.2.0'
```

moon downloads the wasm from the GitHub release and caches it — there is nothing to
install locally.

Projects need no `moon.yml` **configuration** when task inference is enabled (the
default) — language, toolchain, tasks, and dependencies are all inferred. Add
configuration only to override or extend what the plugin contributes:

```yaml
language: 'csharp'   # moon rejects 'c#'

toolchains:
  default: 'dotnet'

tasks:
  build:              # a task with this id fully replaces the inferred one
    command: 'dotnet build --no-restore -c Release'
    inputs:
      - '**/*.cs'
      - '*.csproj'
```

> **Side note — discovery is still moon's job, and moon has no plugin hook for it.**
> moon only creates projects that `.moon/workspace.yml` declares; a toolchain plugin
> cannot contribute projects, and `projects.globs` only match directories or
> `moon.yml` files — a glob like `'src/**/*.csproj'` is rejected (verified through
> moon 2.4.5: *"Received a file path for a project root, must be a directory"*). So
> for a repo with many projects you still need one of:
>
> - **Explicit entries or directory globs** in `workspace.yml` covering every
>   project directory — then there are truly zero `moon.yml` files; or
> - **One empty `moon.yml` stub per project directory** plus a single glob like
>   `'src/**/moon.yml'` — the stub only marks the directory as a project, and every
>   piece of actual configuration is still inferred.
>
> Each moon project should be the directory that directly contains one `.csproj` —
> the plugin deliberately does not search subdirectories, so mapping a whole
> multi-project "service" folder as one moon project yields no inference.

Solution files are never parsed — `.sln`/`.slnx` only act as dependency-root
markers.

## Configuration

All settings live under `dotnet:` in `.moon/toolchains.yml`.

| Setting | Type | Default | Description |
|---|---|---|---|
| `version` | string | — | .NET SDK version/channel to install during toolchain setup. Omit to use an existing SDK. |
| `inferDependencies` | bool | `true` | Infer moon project dependencies from MSBuild `ProjectReference` items. |
| `inferTasks` | bool \| list | `true` | Infer `build`/`test`/`run`/`publish`. A list infers only the named tasks. |
| `restoreArgs` | list | `[]` | Extra arguments appended to `dotnet restore`. |
| `dotnetRoot` | string | — | Explicit `DOTNET_ROOT` for task environments. Falls back to an existing `DOTNET_ROOT`, then `~/.dotnet` when it holds a `dotnet` executable. |
| `inheritAliases` | bool | `true` | moon-level setting; set to `false` to stop `AssemblyName` aliases from being registered. |

```yaml
dotnet:
  plugin: 'github://Wtiben/moon-dotnet-plugin@v0.2.0'
  version: '8.0'
  inferTasks: ['build', 'test']
  restoreArgs: ['--no-cache']
```

## Task inference

**On by default.** Every dotnet project gets standard tasks derived from its real
MSBuild evaluation — no per-project configuration needed.

This is deliberately more proactive than moon's built-in toolchains (the JavaScript
toolchain only mirrors *user-declared* `package.json` scripts, and opt-in at that).
.NET has no equivalent script layer to mirror: without inference a zero-config dotnet
workspace has no tasks at all, and `build`/`test`/`run`/`publish` are the toolchain's
own universal verbs rather than per-repo conventions. It stays safe to leave on
because inference never overrides anything you wrote yourself.

`inferTasks` is a **workspace-level** setting, so one line controls the whole
workspace and turning inference off never requires per-project overrides:

```yaml
dotnet:
  inferTasks: true                # default: infer all four tasks
  # inferTasks: false             # infer nothing
  # inferTasks: ['build', 'test'] # infer only these
```

| Task | Inferred for | Command | Cached |
|---|---|---|---|
| `build` | every project | `dotnet build --no-restore --no-dependencies -c <cfg>` + `deps: ['^:build']` | ✅ outputs from evaluated `BaseOutputPath` |
| `test` | `IsTestProject=true` or a `Microsoft.NET.Test.Sdk` reference | `dotnet test --no-build --no-restore -c <cfg>` + `deps: ['~:build']` — VSTest and Microsoft.Testing.Platform both supported | ✅ (pass/fail state) |
| `run` | `Exe`/`WinExe`, non-test | `dotnet run` | never cached, excluded from CI |
| `publish` | `Exe`/`WinExe`, non-test, single-TFM | `dotnet publish --no-build --no-restore -c <cfg>` + `deps: ['~:build']` | ✅ outputs from evaluated `PublishDir` |

Design notes:

- **moon orchestrates the graph, not MSBuild.** `build` uses `--no-dependencies` and
  depends on `^:build`, so each project builds and caches independently. MSBuild
  resolves `ProjectReference`s from the upstream `bin` without rebuilding it, and moon
  re-runs downstream builds when an upstream project changes.
- **The configuration is pinned** (`-c`) to whatever the evaluation saw (Debug unless
  your props say otherwise). This is necessary because `dotnet publish` defaults to
  *Release* on .NET 8+ while `build` defaults to *Debug*, which would break
  `--no-build`. A repo that sets `Configuration` in `Directory.Build.props` gets that
  configuration everywhere. For a one-off Release publish, define your own task.
- **Outputs come from evaluated paths**, so redirected output locations (custom
  `BaseOutputPath`, .NET 8 `UseArtifactsOutput` under the workspace root) cache
  correctly. If an output path resolves outside the workspace, the task runs
  **uncached** rather than caching the wrong directory.
- **Inputs exclude the evaluated output/intermediate dirs** (`bin`/`obj` by default) —
  MSBuild mutates `obj` on every build, so including it would make hashes unstable.
- **`restore` is deliberately not a task**: moon models it as the install-dependencies
  action (with `--locked-mode`), which runs before tasks — hence `--no-restore`
  everywhere.
- **Your tasks always win.** A task with the same id in a project's `moon.yml` fully
  replaces the inferred one, and ids defined in inherited task files
  (`.moon/tasks.yml`, `.moon/tasks/**/*.yml`) that can apply to dotnet projects are
  never inferred at all — moon would otherwise merge the two into a broken command.
  Files explicitly scoped to other toolchains/languages via `inheritedBy` don't
  suppress anything. Whenever an inherited file does suppress a task, the plugin logs
  which id and which file, so missing tasks are never a mystery.
- Directories with several project files get the file passed explicitly
  (`dotnet build App.csproj ...`). For `test` the flavour follows the runner:
  Microsoft.Testing.Platform takes the project through `--project` and rejects a
  positional path, while classic VSTest mode rejects `--project`. MTP is detected from
  `{"test": {"runner": "Microsoft.Testing.Platform"}}` in the governing `global.json`
  or from a project's own `TestingPlatformDotnetTestSupport`.

Not inferred: `pack`, `watch`, and `clean`; and multi-TFM projects get no `publish`
task, since `dotnet publish` needs an explicit `-f` there.

## Multi-language and Central Package Management

Because MSBuild evaluation is language-agnostic, **`.fsproj` and `.vbproj` projects
are fully supported**, including cross-language `ProjectReference`s. **Central Package
Management** (`Directory.Packages.props` + versionless `PackageReference`) is
supported too: pinned versions reach the task hash through the
`Directory.Packages.props` content hash.

## Task hashing and lock files

Task hashes always include the contents of every `Directory.Build.props`,
`Directory.Build.targets`, `Directory.Build.rsp`, `Directory.Packages.props`,
`nuget.config` (any casing), and `global.json` from the project directory up to the
workspace root — changing any of them invalidates affected task caches even when the
package set is pinned by a lock file.

> [!IMPORTANT]
> **Without a lock file, hashing is approximate.** The package part of the hash is
> computed from the declared/evaluated `PackageReference` set, so floating versions
> (`1.*`) and unpinned transitive upgrades will **not** invalidate caches.

**Commit `packages.lock.json`** (generate it with `dotnet restore --use-lock-file`)
for exact hashing. The plugin then hashes the raw lock file content — which pins the
full resolved set including content hashes — and automatically passes `--locked-mode`
to `dotnet restore`, failing fast (NU1004) when the lock file drifts from the declared
dependencies. Renamed lock files following the `packages.<project>.lock.json`
convention (via `NuGetLockFilePath`) are recognized too.

Note that moon's install-dependencies action fingerprints the lock file plus the one
fixed-name .NET manifest, `Directory.Packages.props` (project files have variable
names, which moon's literal-name manifest matching cannot express). So a Central
Package Management version bump re-triggers installs, but editing a `.csproj` alone
does not until the lock file changes too — another reason to keep lock files committed
and current.

## SDK installation

Setting `version:` under `dotnet:` makes moon install the SDK during toolchain setup,
using the official
[dotnet-install scripts](https://learn.microsoft.com/en-us/dotnet/core/tools/dotnet-install-script):

```yaml
dotnet:
  plugin: 'github://Wtiben/moon-dotnet-plugin@v0.2.0'
  version: '8.0'   # channel; or '8.0.404' (exact), 'lts', 'sts', 'preview'
```

- Installs into **`~/.dotnet`** by default (SDK versions lay out side-by-side), or into
  `dotnetRoot` when configured — the same root the plugin injects as
  `DOTNET_ROOT`/`PATH`, so tasks find the installed SDK with no further wiring. Your
  shell profile is never touched (`--no-path`).
- Version semantics pass through to the script: `X.Y` installs the latest patch of that
  channel, fully-qualified versions install pinned (and short-circuit when that exact
  SDK is already present), `lts`/`sts`/`preview` map to the named channels.
- `global.json` stays a **runtime** concern: the dotnet host picks the matching SDK out
  of `DOTNET_ROOT` at execution time; setup does not parse it. If `global.json` demands
  a version that isn't installed, `dotnet` itself reports the error.

Without a `version:` setting, moon skips toolchain setup ("use globals") and the SDK is
expected from either an existing install in `~/.dotnet` or a system `dotnet` on `PATH`.
When no `DOTNET_ROOT` candidate exists, no environment injection happens. Because
`~/.dotnet` doubles as the dotnet CLI's user-level cache directory, it only counts as a
`DOTNET_ROOT` when the `dotnet` executable actually exists at its root.

## Project aliases

Each project gains its evaluated `AssemblyName` as a moon **alias**, so tasks and
commands can address it by its .NET name as well as its moon id — e.g.
`moon run MyCompany.App:build` for a project whose `moon.yml` id is `app`. Aliases also
drive `moon docker prune`'s per-toolchain package targeting. moon silently ignores an
alias that collides with another project's id or alias (and an alias equal to its own id
is a no-op), so mixed workspaces cannot break on collisions. Set `inheritAliases: false`
to opt out.

## Local dotnet tools

When a tool manifest (`.config/dotnet-tools.json`) is found, `dotnet tool restore` runs
during moon's setup-environment action, before dependency installs. The manifest is
searched for from the dependencies root **upward** to the workspace root, matching how
the dotnet CLI resolves it — tool manifests conventionally sit at the repository root,
which is not necessarily a dependencies root (any project directory holding a lock file
becomes one). The restore is keyed on the manifest's content, so editing it re-runs the
restore and repeat runs skip it. Global tools are out of scope.

## Docker

- `moon docker scaffold <project>`: the configs phase copies exactly the
  restore-relevant files (`*.{csproj,fsproj,vbproj}`/`*.{sln,slnx}`/`*.props`/
  `*.targets`/`Directory.Build.rsp`/`nuget.config`/lock files/`global.json`), with
  `bin`/`obj` explicitly excluded (generated `obj/*.nuget.g.props` and
  `obj/*.nuget.g.targets` would otherwise match). The sources phase copies full project
  sources by moon design.
- `moon docker prune` removes `bin`/`obj` directories in the dependencies root and each
  focused project. NuGet's user-level cache is deliberately not touched: moon runs a
  production install *after* pruning, so clearing it would force a full re-download. Use
  `dotnet nuget locals all --clear` in your Dockerfile if you want that.
- Add `.moon/cache` (and ideally `.moon/docker`) to your `.dockerignore`.

## Limitations

- **SDK-style projects only** — no legacy csproj. `dotnet` CLI only; no NuGet workloads
  and no global tools (local tool manifests *are* restored).
- **Multi-targeted projects** (`<TargetFrameworks>`) are evaluated as the **outer
  (cross-targeting) build**, where `$(TargetFramework)` is empty. References and
  packages gated on a specific TFM are therefore invisible to dependency inference and
  hashing; unconditional ones resolve normally.
- **Custom `<Import>`s** outside the `Directory.Build.*` conventions affect the
  *evaluated package set* (captured in hashes), but their file contents are not
  themselves hashed — build behavior changes in such files won't invalidate caches.
- **No `sync_project`** — the plugin never writes `<ProjectReference>` entries into
  project files from moon's graph. The project files are the source of truth here and
  inference flows one way, out of MSBuild.

## Contributing

Issues and pull requests are welcome. To build and test locally you need a Rust
toolchain with the `wasm32-wasip1` target and a .NET SDK 8+ on `PATH` (parts of the test
suite shell out to `dotnet`):

```bash
cargo build --target wasm32-wasip1          # build the wasm
cargo test --workspace --no-default-features # test (requires the wasm to be built first)

bash scripts/build-and-test.sh              # or both at once
```

To try a local build in a moon workspace, point the plugin locator at the built
artifact (relative to the `.moon` directory):

```yaml
dotnet:
  plugin: 'file://../../moon-dotnet-plugin/target/wasm32-wasip1/debug/dotnet_toolchain.wasm'
```

## License

[MIT](LICENSE)
