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

Releases are gated, and can be started either by pushing `vX.Y.Z` or by dispatching the
workflow with a version. The dispatch route is documented here because it needs no
hand-created tag. To publish version `X.Y.Z`:

1. Bump `version` in `toolchains/dotnet/Cargo.toml`.
2. Add a `## X.Y.Z` entry to `toolchains/dotnet/CHANGELOG.md`.
3. Bump the `plugin:` version in the README's examples for the new release.
4. Commit and push to `main`.
5. Run the Release workflow ("Run workflow") with **version** = `X.Y.Z`. Leave the
   input empty to dry-run the verify path without publishing anything.

Never reuse a version number whose release was deleted; it is gone for good. See
"A released version number can never be reused" below.

The Release workflow then enforces, in order: the full CI test matrix (ubuntu, macOS +
windows), version == crate version, changelog entry exists, the released commit is an
ancestor of `main`, and a smoke test that loads the exact built wasm with a pinned moon
binary (`MOON_SMOKE_VERSION` in `release.yml` — bump it deliberately; it is the
compatibility contract the release is verified against). Only after all gates pass does
it publish the ghcr.io OCI artifact and create the GitHub release (with
`immutableCreate`, so assets are locked after creation).

### A released version number can never be reused

Deleting a release does **not** free its tag name. GitHub keeps a reservation, and any
later attempt to create that tag fails — from `git push`, from `gh release create`, and
from `ncipollo/release-action` alike:

```
- Cannot create ref due to creations being restricted.
```

The message names the wrong culprit. It is not the `protect-release-tags` ruleset, which
holds only `deletion`, `non_fast_forward` and `update` (confirmed via REST, GraphQL and
the UI) and refuses the push identically while disabled. The real source appears only in
the rule-suites API:

```bash
gh api "repos/Wtiben/moon-dotnet-plugin/rulesets/rule-suites?ref=refs/tags/vX.Y.Z"
gh api "repos/Wtiben/moon-dotnet-plugin/rulesets/rule-suites/<id>" --jq '.rule_evaluations'
# => { "rule_source": { "type": "immutable_release_tag" }, "rule_type": "creation", ... }
```

`immutable_release_tag` is GitHub-managed: attached to no ruleset, listed nowhere, and
not bypassable by a repository admin. The accompanying error `tag_name was used by an
immutable release` is **literally true** — believe it, even when no such release is
present any more.

**v0.3.0, v0.3.1 and v0.3.2 are burned on this repository.** They were published as
immutable releases on 2026-07-26, under the version series that predates the
renumbering to 0.1.0 (see the `chore: release 0.3.0` / `0.3.3` commits), and deleting
those releases did not release the names. Which is why 0.3.0 was skipped and this went
straight from 0.2.0 to 0.4.0. Verified free by probe: `v0.3.3`, `v0.4.0`, `v9.9.9`.

Consequences worth knowing:

- It is per-name, not a pattern. A fresh version number tags and releases normally.
- Never reuse a version number after deleting its release. Move forward instead.
- `gh release create` cannot substitute for a tag push here; it fails the same way.
- When a publish fails at the release step, a **draft** release is left behind holding
  the uploaded assets (drafts need no tag). Delete it before retrying, or
  `skipIfReleaseExists: true` makes the next run skip asset upload entirely.
- Do not probe with a `v`-prefixed name. `protect-release-tags` restricts deletions over
  `refs/tags/v*` with no bypass actor, so a throwaway `vfoo` cannot be removed again
  without temporarily disabling the ruleset. Probe with a non-`v` name, or disable the
  ruleset first.

The workflow accepts either trigger: pushing `vX.Y.Z`, or a `workflow_dispatch` with the
**version** input. The dispatch path exists because it needs no hand-created tag —
`ncipollo/release-action` creates the tag alongside the release — which keeps the process
working even when tagging by hand is awkward.

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
