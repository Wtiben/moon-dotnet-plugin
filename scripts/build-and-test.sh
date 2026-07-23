#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
cargo build --target wasm32-wasip1
cargo test --workspace --no-default-features "$@"
