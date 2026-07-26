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

Guarantees:

- A commit that fails tests cannot be released, even if tagged.
- A tag that doesn't match the crate version (or lacks a changelog entry) fails fast.
- Published `v*` tags cannot be deleted or moved (repository ruleset
  "protect-release-tags"); re-releasing a version is a hard error. The escape hatch is
  deliberately manual: temporarily disable the ruleset in repo settings.
- The verify path can be dry-run anytime via the workflow's "Run workflow" button
  (`workflow_dispatch`) — publishing steps only ever run on tag pushes.

## Upstreaming to moonrepo/plugins

The crate layout already matches `moonrepo/plugins` (`toolchains/<id>/` with
`Cargo.toml`, `CHANGELOG.md`, `src/tier{1,2,3}.rs`, `tests/`). These changes are
deliberately deferred to the commit that moves it there, because they are wrong
while the crate lives here:

- `toolchains/dotnet/Cargo.toml` — point `repository` at
  `https://github.com/moonrepo/plugins` and `documentation` at
  `.../tree/master/toolchains/dotnet`. Flipping either early mislabels the OCI
  artifacts this repo publishes to ghcr.io.
- `src/tier1.rs` — drop the `docs_url` pointing at this repo's README. No
  upstream toolchain points at a contributor's repo, and there is no
  `moonrepo.dev` .NET page to point at instead, so follow `toolchains/ruby` and
  return `InitializeToolchainOutput::default()`.
- `Cargo.toml` + `src/tier1.rs` — add
  `toolchain_common = { path = "../../crates/toolchain-common" }` as the first
  dependency, then `enable_tracing()` as the first statement of
  `register_toolchain`. All 14 upstream toolchains do this; it routes host-side
  crate logs into `moon --log debug` and self-disables under test. The path dep
  cannot exist in this repo, and vendoring it would pull in `proto_pdk` for
  nothing.
- `.moon/workspace.yml` (theirs) — register `dotnet-toolchain: toolchains/dotnet`.
- Their `.github/workflows/ci.yml` — add `actions/setup-dotnet` with
  `dotnet-version: '8.0.x'`. Their `.prototools` provisions only
  moon/bun/node/go/npm, and ~26 of our integration tests spawn a real `dotnet`.
  This is the one new CI requirement the PR introduces; call it out explicitly.
- Scope the PR to `toolchains/dotnet/**`. `README.md`, `CLAUDE.md`, `scripts/`,
  `.github/`, `.cargo/config.toml` and `rust-toolchain.toml` stay here — upstream
  supplies `moon ci`, `cargo release` and its own root config.

Note that there are **no** `toolchains/*/README.md` files upstream: a toolchain
directory is exactly `CHANGELOG.md` + `Cargo.toml` + `src/` + `tests/`, and the
user-facing documentation surface is doc comments on `config.rs` (which become
the JSON schema) plus `config_url`/`docs_url`.

One pre-submission check this repo cannot run: a default-features (host `cdylib`)
build on `x86_64-pc-windows-msvc`. Our CI only builds `--no-default-features` on
windows, and the `rustflags` workaround in `.cargo/config.toml` is
windows-**gnu**-specific — so the host `cdylib` path is unexercised on MSVC, which
is what upstream's windows runners use. Expected to pass (`toolchains/go` and
`toolchains/rust` ship the same `crate-type` and are built there), but worth
confirming on a machine with the MSVC toolchain before opening the PR.

Expect a maintainer to question a v1.0 from outside the project. The only
third-party precedent, `toolchains/ruby`, onboarded at `0.1.0` behind an
`unstable_ruby` config key.

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
