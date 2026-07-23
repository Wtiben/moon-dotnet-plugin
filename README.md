# moon-dotnet-plugin

A [moon](https://moonrepo.dev) toolchain WASM plugin for the .NET ecosystem (SDK-style C# projects).

Status: work in progress.

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
