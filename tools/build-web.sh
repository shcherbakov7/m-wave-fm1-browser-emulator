#!/usr/bin/env bash
# Build the WebAssembly core and place it next to the static web app.
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
cargo build --manifest-path "$root/emulator/Cargo.toml" --release -p fm1-wasm --target wasm32-unknown-unknown
cp "$root/emulator/target/wasm32-unknown-unknown/release/fm1_wasm.wasm" "$root/web/fm1.wasm"
echo "web/fm1.wasm: $(wc -c < "$root/web/fm1.wasm") bytes"
