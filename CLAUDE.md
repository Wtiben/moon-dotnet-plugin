# Maintainer notes

Internal notes for working on this repository. Everything user-facing belongs in
[README.md](README.md); this file is for the release process and the non-obvious
facts about moon and the test harness that are easy to rediscover the hard way.

## Layout

- `toolchains/dotnet/` — the plugin crate (`dotnet_toolchain.wasm`).
  - `src/tier1.rs` / `tier2.rs` / `tier3.rs` — the moon toolchain API surface per tier.
  - `src/msbuild.rs` — batched + per-project MSBuild evaluation.
  - `src/infer_tasks.rs`, `src/nuget_lock.rs`, `src/dotnet_install.rs`, `src/config.rs`.
  - `CHANGELOG.md` — release notes; the release workflow requires an entry per version.
- `scripts/build-and-test.sh` — build the wasm, then run the test suite.
- `.github/workflows/ci.yml` — test matrix (ubuntu + windows); `release.yml` — release.

## Build and test

```bash
cargo build --target wasm32-wasip1
cargo test --workspace --no-default-features   # needs the wasm built first
bash scripts/build-and-test.sh                 # both
cargo test -- --ignored soak                   # 60-project generated workspace (~4s)
```

The `rust-toolchain.toml` pin exists because `moonrepo/build-wasm-plugin` reads it to
select `wasm32-wasip1` — without it the action tries the removed `wasm32-wasi` target.

## Releasing

Releases are tag-driven and gated. To publish version `X.Y.Z`:

1. Bump `version` in `toolchains/dotnet/Cargo.toml`.
2. Add a `## X.Y.Z` entry to `toolchains/dotnet/CHANGELOG.md`.
3. Commit, then: `git tag vX.Y.Z && git push origin main vX.Y.Z`
4. Bump the `plugin:` version in the README's examples for the new release.

The Release workflow then enforces, in order: the full CI test matrix (ubuntu +
windows), tag == crate version, changelog entry exists, and a smoke test that loads the
exact built wasm with a pinned moon binary (`MOON_SMOKE_VERSION` in `release.yml` — bump
it deliberately; it is the compatibility contract the release is verified against). Only
after all gates pass does it publish the ghcr.io OCI artifact and create the GitHub
release (with `immutableCreate`, so assets are locked after creation).

The "Recompute wasm checksums" step in `release.yml` is load-bearing, not
belt-and-braces: `build-wasm-plugin` emits a `dotnet_toolchain.wasm.sha256` that
does not match the wasm sitting next to it in `builds/`, so without that step
every release ships a checksum that fails verification (#2). Don't drop the step
on the assumption the action got fixed — verify against a published asset first.

The wasm itself has always been fine. It matches its ghcr OCI layer byte for
byte, and moon never reads the `.sha256`, which is why this went unnoticed until
someone verified a download by hand.

Guarantees:

- A commit that fails tests cannot be released, even if tagged.
- A tag that doesn't match the crate version (or lacks a changelog entry) fails fast.
- Published `v*` tags cannot be deleted or moved (repository ruleset
  "protect-release-tags"); re-releasing a version is a hard error. The escape hatch is
  deliberately manual: temporarily disable the ruleset in repo settings.
- The verify path can be dry-run anytime via the workflow's "Run workflow" button
  (`workflow_dispatch`) — publishing steps only ever run on tag pushes.

## Known coverage gap

The host `cdylib` is never built on `x86_64-pc-windows-msvc`. CI builds
`--no-default-features` on windows (which cfg-gates the whole plugin surface out),
and the `rustflags` workaround in `.cargo/config.toml` is windows-**gnu**-specific.
So a default-features build on MSVC is unexercised. Nothing consumes the host
DLL's exports, so this is a build-time risk only — but if you have the MSVC
toolchain, `cargo build` on it is worth running before a release.

## moon facts (verified against moon 2.5.2)

- `moon toolchain info dotnet` requires the plugin locator as an explicit second
  argument — it does not read custom entries from `.moon/toolchains.yml`:
  `moon toolchain info dotnet "file://../moon-dotnet-plugin/target/wasm32-wasip1/debug/dotnet_toolchain.wasm"`.
  The locator resolves relative to the current working directory.
- In `moon.yml`, `language: 'c#'` is rejected ("Invalid fallback variant"); use
  `language: 'csharp'`. The project-level key is `toolchains` (plural):
  `toolchains: { default: 'dotnet' }`.
- **`LocateDependenciesRootOutput::members` is load-bearing since 2.5.** moon's
  `in_dependencies_workspace` (`crates/toolchain-plugin/src/toolchain_plugin.rs`)
  treats `None` as "the root path is the only member", so a project below the root
  is neither root nor member and moon builds *no* `install_dependencies` or
  `setup_environment` action for it — silently. We return `["**"]`. Symptom if it
  regresses: `moon run x:build` runs the task and nothing else, then MSBuild fails
  with NETSDK1004 (missing `project.assets.json`). A workspace with per-project
  lock files hides it, because each project is then its own root.
- `projects.globs` accepts a trailing file name since 2.5 (`'**/*.csproj'`), and
  the project id comes from the **leaf directory name** — so repeated directory
  names are a hard `project_graph::duplicate_id`. abp's `templates/` has 39 such
  collisions over 80 projects, Ocelot's `samples/` has 2 over 8.
- 2.4.x introduced no toolchain WASM API changes; 2.5.0 broke `VirtualPath`, which
  is where the 2.5 floor comes from. `ExecCommand::cache` also changed shape there
  (key string -> `CacheStrategy`, key now derived from `label`).
- `moon action-graph <target>` opens a browser visualiser and blocks — do not run
  it unattended.
- `inheritAliases` is a moon-level per-toolchain setting, not one of ours.
- A `file://` plugin locator in `.moon/toolchains.yml` resolves relative to the
  `.moon` directory, **not** the workspace root — `file://../../x.wasm` from a repo
  root, not `file://../x.wasm`. A wrong path fails loudly
  (`plugin::loader::file::missing`), but the error prints the joined path, which
  reads oddly (`<workspace>/.moon/../x.wasm`).

## Dependency pins

`moon_pdk_test_utils` requires an exact `proto_core`, and `proto_pdk_api` 0.33.2
adds a `ChecksumAlgorithm` variant that `proto_core` 0.60.4 does not match on — a
`cargo update` that takes 0.33.2 fails to compile the dev-dependency tree. The
lockfile pins `proto_pdk_api` to 0.33.0, matching upstream `moonrepo/plugins`.

## Test harness facts (verified against vendored sources)

- **`exec_command` in the test sandbox is REAL** — `warpgate-0.35.x/src/host.rs`
  (`fn exec_command`) spawns an actual `std::process::Command`, resolving the executable
  from the host `PATH`. moon's `crates/pdk-test-utils` sandbox registers these warpgate
  host functions unmocked (only moon's `load_*` data functions are mocked). Sandbox
  tests that shell out to `dotnet` therefore require a .NET SDK on the test machine.
- **`find_wasm_file` prefers `release` over `debug`**
  (`warpgate-0.35.x/src/test_utils.rs`, `profiles = ["release", "debug"]`). Never leave a
  stale `target/wasm32-wasip1/release/dotnet_toolchain.wasm` around while running tests
  against a freshly built debug wasm — delete the release artifact first.
