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

Known defect in the published assets: the `dotnet_toolchain.wasm.sha256` uploaded
alongside the wasm does **not** match the wasm. Verified on v0.1.0, v0.2.0 and
v0.3.0/v0.3.1, so it predates any of our workflow changes — `build-wasm-plugin`
evidently checksums a different artifact than the one it leaves in `builds/`.
moon does not verify it (the cached plugin matches the wasm bytes exactly), so
nothing is broken today, but anyone verifying a download by hand will fail. Fix
by computing the checksum ourselves in `release.yml` before the upload step.

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

## moon facts (verified against moon 2.3.3 and 2.4.5)

- `moon toolchain info dotnet` requires the plugin locator as an explicit second
  argument — it does not read custom entries from `.moon/toolchains.yml`:
  `moon toolchain info dotnet "file://../moon-dotnet-plugin/target/wasm32-wasip1/debug/dotnet_toolchain.wasm"`.
  The locator resolves relative to the current working directory.
- In `moon.yml`, `language: 'c#'` is rejected ("Invalid fallback variant"); use
  `language: 'csharp'`. The project-level key is `toolchains` (plural):
  `toolchains: { default: 'dotnet' }`.
- moon 2.4.x introduced no toolchain WASM API changes (2.4.0 added built-in
  Poetry/Ruby toolchains only); the plugin runs unmodified on 2.0–2.4.
- `inheritAliases` is a moon-level per-toolchain setting, not one of ours.
- A `file://` plugin locator in `.moon/toolchains.yml` resolves relative to the
  `.moon` directory, **not** the workspace root — `file://../../x.wasm` from a repo
  root, not `file://../x.wasm`. A wrong path fails loudly
  (`plugin::loader::file::missing`), but the error prints the joined path, which
  reads oddly (`<workspace>/.moon/../x.wasm`).

## Test harness facts (verified against vendored sources)

- **`exec_command` in the test sandbox is REAL** — `warpgate-0.30.5/src/host.rs`
  (`fn exec_command`) spawns an actual `std::process::Command`, resolving the executable
  from the host `PATH`. moon's `crates/pdk-test-utils` sandbox registers these warpgate
  host functions unmocked (only moon's `load_*` data functions are mocked). Sandbox
  tests that shell out to `dotnet` therefore require a .NET SDK on the test machine.
- **`find_wasm_file` prefers `release` over `debug`**
  (`warpgate-0.30.5/src/test_utils.rs`, `profiles = ["release", "debug"]`). Never leave a
  stale `target/wasm32-wasip1/release/dotnet_toolchain.wasm` around while running tests
  against a freshly built debug wasm — delete the release artifact first.
