# Changelog

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
