# Changelog

## 0.3.0

#### 💥 Breaking

- **Requires moon 2.5 or newer.** moon 2.5 reworked the `VirtualPath` WASM API
  from an enum into a newtype over `PathBuf`, which changes how paths cross the
  host boundary. Stay on v0.2.0 for moon 2.0-2.4.

#### 🚀 Updates

- Project files can be globbed directly now that moon 2.5 allows a trailing file
  name in `projects.globs` — `'**/*.csproj'` replaces the generated
  `projects.sources` map, and a workspace needs no `moon.yml` at all. Note that
  moon derives each project id from its leaf directory name, so repositories with
  repeated directory names (sample and template trees, typically) need either a
  narrower glob or an `id:`-only `moon.yml` in the colliding directories.

#### 🐞 Fixes

- `dotnet restore` runs again for projects that sit below their dependencies root.
  `locate_dependencies_root` declared no member globs, which moon 2.5 reads as
  "the root is the only member" — so for the ordinary layout of one solution at
  the repository root and no per-project lock files, moon built no
  `install_dependencies` or `setup_environment` action for any project, and the
  inferred `build --no-restore` failed on a missing `project.assets.json`.
- `dotnet tool restore` moved to moon's `CacheStrategy::Hash`, which fingerprints
  the declared cache inputs. The manifest digest that used to be folded into the
  cache key by hand is now moon's job, and the key stays stable across manifest
  edits as it must.

## 0.2.0

#### 🚀 Updates

- New `msbuildProperties` setting: additional MSBuild properties applied to every
  evaluation behind dependency and task inference, as `-p:NAME=VALUE`. Use it to
  evaluate the graph the way the code actually deploys — e.g. conditional,
  codegen-only `ProjectReference`s (`ReferenceOutputAssembly=false`, gated on a
  property like `SkipApiClientGen`) no longer add build-ordering edges the
  deployed build never compiles. The properties are part of the eval-cache
  digest, and inferred task commands do not pass them (evaluation-time only).

#### 🐞 Fixes

- The `dotnet_toolchain.wasm.sha256` published with each release now matches the
  wasm it is published alongside. The wasm itself was never affected.

## 0.1.0

#### 🚀 Updates

- Initial release of the .NET toolchain plugin.
  - Tier 1: project and task detection (`*.csproj`/`*.fsproj`/`*.vbproj`, `*.sln`/`*.slnx`,
    `global.json`, `Directory.Build.*`, `Directory.Packages.props`, `nuget.config`),
    config schema, and Docker metadata with restore-layer scaffold globs.
  - Tier 2: dependency root location, `dotnet restore` installs with automatic
    `--locked-mode` when a lock file is present, local tool manifest restore
    (`.config/dotnet-tools.json`), `packages.lock.json` and `Directory.Packages.props`
    parsing, project-graph dependency inference from `ProjectReference`, task inference
    (`build`/`test`/`run`/`publish`), `AssemblyName` project aliases, task-content
    hashing, and `DOTNET_ROOT`/`PATH` injection into task environments.
  - Tier 3: .NET SDK installation via the official dotnet-install scripts when
    `version` is configured.
- Dependencies and tasks are inferred from a real MSBuild evaluation rather than by
  parsing project XML, so `Directory.Build.targets` imports, MSBuild properties such as
  `$(SolutionDir)`, conditional references and Central Package Management all resolve the
  way the SDK resolves them. Every project in the workspace is evaluated in a single
  batched invocation, and the results are cached on disk for task hashing to reuse.
