# moon-dotnet-plugin

A [moon](https://moonrepo.dev) toolchain WASM plugin for the .NET ecosystem (SDK-style C# projects).

Status: work in progress (Phase 0 scaffolding).

## Development notes

- Build: `cargo build --target wasm32-wasip1`
- Test: `cargo test --workspace --no-default-features` (requires the wasm to be built first)
- Or both: `bash scripts/build-and-test.sh`
- On this dev machine the host toolchain is `x86_64-pc-windows-gnu` (no MSVC C++ build
  tools installed; the GNU toolchain ships a self-contained linker).

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
