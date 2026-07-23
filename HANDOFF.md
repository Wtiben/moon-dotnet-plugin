# Handoff: moon-dotnet-plugin — state of work & path to publishing

> **Audience**: an AI coding agent (or human) with NO access to the conversation that
> produced this repo. Everything needed to pick up the work is in this document plus
> the repo itself. The **remaining goal** is: publish this plugin on GitHub with CI so
> users can consume it with a one-line `github://` locator.

---

## 1. What this is

A [moon](https://moonrepo.dev) 2.x **toolchain WASM plugin for .NET** (SDK-style C#
projects), written in Rust, compiled to `wasm32-wasip1`, loaded by moon via Extism.
It was built by following `moon-dotnet-toolchain-plugin-plan.md` (a 5-phase plan; all
phases are complete). It is modeled on the official Go plugin in
[moonrepo/plugins](https://github.com/moonrepo/plugins) (`toolchains/go`).

- **Remote**: https://github.com/w-tiben_innobv/moon-dotnet-plugin (branch `main`, up to date)
- **Crate**: `toolchains/dotnet` → package `dotnet_toolchain` → artifact `dotnet_toolchain.wasm`
- **Toolchain id**: `dotnet`
- **Version**: 0.1.0 (in `toolchains/dotnet/Cargo.toml`)

### Exported plugin functions (all implemented and tested)

| Tier | Functions |
|---|---|
| 1 | `register_toolchain`, `define_toolchain_config`, `initialize_toolchain`, `define_docker_metadata` |
| 2 | `locate_dependencies_root`, `extend_project_graph`, `extend_task_command`, `install_dependencies`, `parse_lock`, `hash_task_contents`, `prune_docker` |

Tier 3 (SDK install) is deliberately **not** implemented — the SDK comes from proto or
the system PATH; `extend_task_command` injects `DOTNET_ROOT`/`PATH` when a real SDK
layout exists at `~/.dotnet` (or via the `dotnetRoot` setting). See README "SDK
installation" and FOLLOWUPS.md item 5.

### Design cornerstones (do not regress)

- **Project-graph extraction uses REAL MSBuild evaluation** (`dotnet msbuild
  <proj> -nologo -getProperty:... -getItem:ProjectReference,PackageReference`, JSON
  output, needs .NET SDK 8+) — NOT static XML parsing. `src/msbuild.rs` holds the
  parser (pure, natively tested) and the exec wrapper (wasm-only).
- **Path domain discipline**: real host paths go into exec args and are compared
  against MSBuild output after lexical normalization (`normalize_path_key`: forward
  slashes + lowercase); virtual/workspace paths are returned to moon.
- **Hashing policy**: raw `packages.lock.json` content when present (and
  `--locked-mode` is auto-added to `dotnet restore`); otherwise the evaluated
  `PackageReference` set + contents of `Directory.Build.props`/`Directory.Packages.props`
  up the tree. Loudly documented in README.
- **Scaffold globs exclude `**/bin/**` and `**/obj/**`** (negated globs) — generated
  `obj/*.nuget.g.props` would otherwise leak into the Docker restore layer.

## 2. Current verified state

- `cargo build --target wasm32-wasip1` → `target/wasm32-wasip1/debug/dotnet_toolchain.wasm`
- `cargo test --workspace --no-default-features` → **30 tests, all passing**
  (9 native parser/config, 2 tier1 sandbox, 19 tier2 sandbox). Sandbox tests spawn a
  REAL `dotnet` (the harness's `exec_command` host fn is unmocked — verified in
  `warpgate-0.30.5/src/host.rs:134`), so **a .NET SDK 8+ must be installed** wherever
  tests run.
- E2E verified against moon 2.3.3 in a sibling scratch workspace (`../dotnet-moon-e2e`,
  local git repo, not on GitHub): dependency edges app→lib→core and app-tests→app
  inferred; `inferDependencies: false` disables inference; cache hit/miss/revert
  cycles; `Directory.Build.props` content changes bust caches via `hash_task_contents`;
  lockfile content changes bust caches; moon's install action runs
  `dotnet restore --locked-mode` and drift fails with NU1004; docker scaffold configs
  phase contains exactly the restore-relevant files.
- **Zero-`moon.yml` mode verified**: projects need no per-project config. Workspace
  globs + `.moon/tasks/dotnet.yml` with `inheritedBy: { toolchains: ['dotnet'] }`
  suffice (NOTE: `inheritedBy: languages: ['csharp']` did NOT match on moon 2.3.3 —
  scope by toolchain, not language).

### Known caveats already documented in README

- moon 2.3.3 rejects `language: 'c#'` in moon.yml — use `'csharp'`.
- `moon toolchain info dotnet` needs the locator as an explicit 2nd CLI argument.
- moon's install-dependencies action fingerprints only the lock file (we register no
  manifest file names) — csproj edits alone don't re-trigger installs.
- `IsTestProject` is empty before a restore; task inference falls back to detecting a
  `Microsoft.NET.Test.Sdk` PackageReference.
- The archived `Phault/proto-dotnet-plugin` v0.3.0 is broken on proto 0.58.2/Windows
  (os error 193); system dotnet is the working fallback.

### Dev-machine quirks (only relevant on the original Windows machine)

- No MSVC C++ build tools → Rust host toolchain is `stable-x86_64-pc-windows-gnu`.
  `.cargo/config.toml` carries `-Wl,--exclude-all-symbols` for that target only
  (host cdylib otherwise exceeds the 65k DLL export limit). Harmless elsewhere.
- The test harness picks `release` wasm over `debug` if both exist — delete
  `target/wasm32-wasip1/release/dotnet_toolchain.wasm` before unit-test cycles, or
  never build release locally.
- Reference clones live as siblings: `../moonrepo-plugins-reference`,
  `../moonrepo-moon-reference`. Use them (or docs.rs for the pinned crate versions:
  moon_pdk/moon_pdk_api/moon_pdk_test_utils 2.0.4, moon_config 2.1.0) as ground truth
  for any API question.

---

## 3. THE REMAINING TASK: publish with CI so users can install easily

The distribution mechanism is built into moon (verified in the vendored
`warpgate-0.30.5` loader): a `github://owner/repo[@tag]` plugin locator downloads the
**first `.wasm` asset of the repo's GitHub release** and caches it. End-user
experience after publishing:

```yaml
# .moon/toolchains.yml
dotnet:
  plugin: 'github://w-tiben_innobv/moon-dotnet-plugin@v0.1.0'
  inferDependencies: true
```

### Step 1 — Licensing & metadata (prerequisite for public consumption)

- [ ] Add a `LICENSE` file (MIT recommended — matches moonrepo/plugins).
- [ ] Extend `toolchains/dotnet/Cargo.toml` `[package]` with `description`,
      `license = "MIT"`, `repository`. Keep `publish = false` (we don't ship to
      crates.io; the wasm goes to GitHub Releases).
- [ ] Add a `CHANGELOG.md` at the crate root (`toolchains/dotnet/CHANGELOG.md`) with
      an `## Unreleased` section — `moonrepo/build-wasm-plugin` extracts the release
      body from it (see upstream `[package.metadata.release]` pre-release-replacements
      in `moonrepo-plugins-reference/toolchains/go/Cargo.toml` for the pattern).

### Step 2 — CI workflow (`.github/workflows/ci.yml`)

Upstream's ci.yml is moon-specific (uses `moon ci`); do NOT copy it. A plain
cargo-based workflow is right for this repo:

```yaml
name: CI
on:
  push:
    branches: [main]
  pull_request:
jobs:
  test:
    runs-on: ${{ matrix.os }}
    strategy:
      matrix:
        os: [ubuntu-latest, windows-latest]
      fail-fast: false
    steps:
      - uses: actions/checkout@v4
      - uses: actions/setup-dotnet@v4        # sandbox tests spawn real `dotnet`
        with:
          dotnet-version: '8.0.x'
      - uses: moonrepo/setup-rust@v1
        with:
          bins: cargo-nextest
          targets: wasm32-wasip1
        env:
          GITHUB_TOKEN: ${{ secrets.GITHUB_TOKEN }}
      - run: cargo build --target wasm32-wasip1
      - run: cargo nextest run --workspace --no-default-features
```

Notes:
- `actions/setup-dotnet` is REQUIRED — 19 of the 30 tests execute `dotnet msbuild`
  through the sandbox. Runners have SDKs preinstalled but pin 8.x explicitly
  (`-getProperty` JSON needs SDK 8+; fixtures target net8.0).
- The first tier2 test run on a cold NuGet cache is slow (~2 min locally); set a
  generous job timeout. `NEXTEST_RETRIES: 2` (as upstream uses) is a reasonable guard.
- The windows-gnu rustflags in `.cargo/config.toml` are target-scoped and inert on
  GitHub's MSVC-based Windows runners.
- VERIFY at implementation time: action major versions current (`actions/checkout@v4`
  vs `@v6` upstream, etc.).

### Step 3 — Release workflow (`.github/workflows/release.yml`)

Mirror upstream `moonrepo-plugins-reference/.github/workflows/release.yml` (quoted
here verbatim as of 2026-07-23) and adapt:

```yaml
name: Release
permissions:
  contents: write
  packages: write
  attestations: write
  id-token: write
on:
  push:
    tags:
      - "**[0-9]+.[0-9]+.[0-9]+*"
  pull_request:
jobs:
  build:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v6
      - uses: moonrepo/setup-rust@v1
        with:
          cache: false
      - id: build
        uses: moonrepo/build-wasm-plugin@v0
        with:
          publish: ${{ github.event_name == 'push' && github.ref_type == 'tag' }}
        env:
          GITHUB_TOKEN: ${{ secrets.GITHUB_TOKEN }}
      - if: ${{ github.event_name == 'push' && github.ref_type == 'tag' }}
        uses: ncipollo/release-action@v1
        with:
          artifacts: builds/*
          artifactErrorsFailBuild: true
          body: ${{ steps.build.outputs.changelog-entry }}
          immutableCreate: true
          makeLatest: false
          prerelease: ${{ contains(github.ref_name, '-alpha') || contains(github.ref_name, '-beta') || contains(github.ref_name, '-rc') }}
          skipIfReleaseExists: true
```

Key facts / VERIFY markers:

- **Tag format**: upstream (a multi-plugin monorepo) tags `dotnet_toolchain-v0.1.0`
  style (`<crate>-v<semver>`); `build-wasm-plugin` uses the tag prefix to select the
  crate. This repo is also workspace-shaped (`toolchains/dotnet`), so the
  `dotnet_toolchain-v0.1.0` convention is the safe choice. VERIFY against the action's
  README (https://github.com/moonrepo/build-wasm-plugin) whether a bare `v0.1.0` also
  works for a single-crate workspace — if it does, prefer it (nicer locator pins).
- The action builds `--release` for `wasm32-wasip1`, optimizes with `wasm-opt`, and
  drops artifacts in `builds/*` (`dotnet_toolchain.wasm` + sha256 checksum).
- The `github://` loader picks the first asset with `.wasm` extension or
  `application/wasm` content type — one wasm per release keeps this unambiguous.
- `makeLatest: false` is an upstream monorepo artifact; for this single-plugin repo
  consider `makeLatest: true` so `github://...` without `@tag` resolves sensibly
  (VERIFY: the warpgate GitHub loader looks up "a release with assets", latest first).

### Step 4 — Release procedure (repeat per version)

1. Bump `version` in `toolchains/dotnet/Cargo.toml`; move CHANGELOG "Unreleased" notes
   under the new version heading.
2. Commit, then tag and push: `git tag dotnet_toolchain-v0.1.0 && git push origin main --tags`.
3. Workflow publishes the GitHub release with the optimized wasm attached.
4. **Smoke test** (mandatory): in a scratch moon workspace change the locator to
   `github://w-tiben_innobv/moon-dotnet-plugin@dotnet_toolchain-v0.1.0`, run
   `moon clean && moon project <x>` + `moon run <x>:build`, confirm graph edges and
   build work. The sibling `../dotnet-moon-e2e` workspace on the original machine is
   ready-made for this (swap `plugin:` in `.moon/toolchains.yml`).
5. Update README usage snippet to the `github://` locator once the first release exists.

### Step 5 — Optional follow-through

- Submit to the moonrepo third-party plugins listing (docs PR) for discoverability —
  FOLLOWUPS.md item 4.
- The rest of FOLLOWUPS.md (7 items) is deferred scope, not release blockers.

### Acceptance criteria for "published"

- [ ] CI green on `main` (both OSes) from a clean clone.
- [ ] A GitHub release exists whose assets include `dotnet_toolchain.wasm`.
- [ ] A fresh moon 2.x workspace with ONLY the `github://` locator (no local wasm)
      builds a C# project and shows inferred ProjectReference dependencies.
- [ ] README shows the `github://` locator as the primary install method.

---

## 4. Local dev quick reference

```bash
cargo build --target wasm32-wasip1                       # build debug wasm
cargo test --workspace --no-default-features             # run all 30 tests (needs dotnet 8+)
bash scripts/build-and-test.sh                           # both
```

Repo map:

```
toolchains/dotnet/
├── Cargo.toml            # crate dotnet_toolchain, cdylib+lib, wasm feature gate
├── src/
│   ├── lib.rs            # module wiring; tier1/tier2 behind #[cfg(feature = "wasm")]
│   ├── config.rs         # DotnetToolchainConfig (+ native schema/default tests)
│   ├── msbuild.rs        # MSBuild -get* eval: parser (native-tested) + exec wrapper
│   ├── nuget_lock.rs     # packages.lock.json parser (native-tested)
│   ├── tier1.rs          # metadata, config schema, docker metadata
│   └── tier2.rs          # graph, deps root, task command, install, lock, hash, prune
└── tests/
    ├── tier1_test.rs
    ├── tier2_test.rs
    └── __fixtures__/     # projects (4-project graph), locate, locate-no-sln, locked
```
