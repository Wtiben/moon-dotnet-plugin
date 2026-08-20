# Maintainer notes

Non-obvious facts about this repository, moon, and the test harness. Everything
user-facing belongs in [README.md](README.md).

## Build and test

```bash
cargo build --target wasm32-wasip1
cargo test --workspace --no-default-features   # needs the wasm built first
bash scripts/build-and-test.sh                 # both
cargo test -- --ignored soak                   # 60-project generated workspace
```

- `rust-toolchain.toml` exists so `moonrepo/build-wasm-plugin` selects `wasm32-wasip1`;
  without it the action tries the removed `wasm32-wasi` target.
- The host `cdylib` is never built on `x86_64-pc-windows-msvc`: CI builds
  `--no-default-features` on windows, and the `rustflags` workaround in
  `.cargo/config.toml` is windows-**gnu**-specific. Nothing consumes the DLL's exports,
  so this is a build-time risk only.
- `proto_pdk_api` is pinned to 0.33.0 in the lockfile. 0.33.2 adds a `ChecksumAlgorithm`
  variant that `proto_core` 0.60.4 — required exactly by `moon_pdk_test_utils` — does
  not match on, so a `cargo update` taking it fails to compile the dev-dependency tree.

## Releasing

1. Bump `version` in `toolchains/dotnet/Cargo.toml`.
2. Add a `## X.Y.Z` entry to `toolchains/dotnet/CHANGELOG.md`; the workflow requires one.
3. Bump the `plugin:` version in the README examples.
4. Commit and push to `main`.
5. Run the Release workflow with **version** = `X.Y.Z`. An empty input dry-runs the
   verify path without publishing. Pushing a `vX.Y.Z` tag also works.

Gates, in order: CI matrix, version == crate version, changelog entry, the commit is an
ancestor of `main`, and a smoke test that loads the built wasm with moon
`MOON_SMOKE_VERSION` — that pin is the compatibility contract the release is verified
against, so bump it deliberately. Only then does it push the ghcr.io OCI artifact and
create the release.

The "Recompute wasm checksums" step is load-bearing, not belt-and-braces:
`build-wasm-plugin` emits a `.sha256` that does not match the wasm sitting beside it, so
without that step every release ships a checksum that fails verification.

**A version number is single-use.** Deleting a release does not free its tag name, and
every route to recreate it is then refused — `git push`, `gh release create` and
`ncipollo/release-action` alike — with `Cannot create ref due to creations being
restricted`. `v0.3.0` through `v0.3.2` are spent this way. The error blames the wrong
thing: `protect-release-tags` carries no creation rule, and the push fails with that
ruleset disabled. Identify the real source with:

```bash
gh api "repos/Wtiben/moon-dotnet-plugin/rulesets/rule-suites?ref=refs/tags/vX.Y.Z"
gh api "repos/Wtiben/moon-dotnet-plugin/rulesets/rule-suites/<id>" --jq '.rule_evaluations'
# rule_source.type == "immutable_release_tag" — GitHub-managed, in no ruleset,
# not bypassable by an admin, and unaffected by the repo immutability setting
```

- A failed publish leaves a **draft** release holding the uploaded assets. Delete it
  before retrying, or `skipIfReleaseExists: true` makes the next run skip asset upload.
- `protect-release-tags` blocks deletion of `refs/tags/v*` with no bypass actor, so never
  create a throwaway `v…` tag — it cannot be removed again without disabling the ruleset.

## moon

- **`LocateDependenciesRootOutput::members` is load-bearing.** `None` means "the root
  path is the only member", so a project *below* the root silently gets no
  `install_dependencies` and no `setup_environment` action. We return `["**"]`. Symptom
  if it regresses: `moon run x:build` runs the task and nothing else, then MSBuild fails
  with NETSDK1004. A workspace with per-project lock files hides it, because each
  project is then its own root.
- `projects.globs` accepts a trailing file name (`'**/*.csproj'`), and the project id
  comes from the **leaf directory name** — so repeated directory names are a hard
  `project_graph::duplicate_id`.
- In `moon.yml`, `language: 'c#'` is rejected; use `'csharp'`. The project-level key is
  `toolchains` (plural).
- `inheritAliases` is a moon-level per-toolchain setting, not one of ours.
- `moon toolchain info dotnet <locator>` needs the locator as an explicit second
  argument — it ignores custom entries in `.moon/toolchains.yml` — and resolves it
  relative to the current working directory.
- A `file://` locator *inside* `.moon/toolchains.yml` resolves relative to the `.moon`
  directory, not the workspace root. A wrong path fails as
  `plugin::loader::file::missing`, printing a joined path that reads oddly.
- `moon action-graph <target>` opens a browser visualiser and blocks; never run it
  unattended.

## Test harness

- **`exec_command` is real.** Warpgate spawns an actual process, resolved from the host
  `PATH`; moon's sandbox mocks only its own `load_*` data functions. Tests that shell out
  to `dotnet` therefore need a .NET SDK on the machine.
- **`find_wasm_file` prefers `release` over `debug`.** Never leave a stale
  `target/wasm32-wasip1/release/dotnet_toolchain.wasm` around while testing a freshly
  built debug wasm.
