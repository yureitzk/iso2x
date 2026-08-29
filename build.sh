#!/usr/bin/env bash
set -euo pipefail

DIR="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
cd "$DIR"

WBG_PINNED=$(cargo metadata --locked --format-version 1 \
    --manifest-path ./crate/Cargo.toml \
  | jq -r '[.packages[] | select(.name == "wasm-bindgen") | .version] | unique | .[]')

if [ -z "$WBG_PINNED" ]; then
  echo "Could not find wasm-bindgen in resolved dependency graph" >&2
  exit 1
fi

WBG_INSTALLED=$(wasm-bindgen -V | awk '{print $2}')

if [ "$WBG_PINNED" != "$WBG_INSTALLED" ]; then
  echo "wasm-bindgen CLI mismatch: crate resolves to $WBG_PINNED, installed CLI is $WBG_INSTALLED" >&2
  echo "Run: cargo install -f wasm-bindgen-cli --version $WBG_PINNED" >&2
  exit 1
fi

cargo test --locked --manifest-path ./crate/Cargo.toml

CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUNNER=wasm-bindgen-test-runner \
  cargo test --locked \
  --manifest-path ./crate/Cargo.toml \
  --target wasm32-unknown-unknown

cargo build --locked --release --target wasm32-unknown-unknown --manifest-path ./crate/Cargo.toml

rm -rf ./npm/src/wasm
mkdir -p ./npm/src/wasm

wasm-bindgen \
  --target web \
  --out-dir ./npm/src/wasm \
  ./target/wasm32-unknown-unknown/release/iso2x.wasm

wasm-opt -O3 \
  --enable-mutable-globals \
  --enable-sign-ext \
  --enable-bulk-memory-opt \
  --enable-nontrapping-float-to-int \
  --strip-debug \
  ./npm/src/wasm/iso2x_bg.wasm \
  -o ./npm/src/wasm/iso2x_bg.wasm.tmp

mv ./npm/src/wasm/iso2x_bg.wasm.tmp ./npm/src/wasm/iso2x_bg.wasm

cd npm
rm -rf dist
npm ci
npm run build
cp -r src/wasm dist/wasm
cd ..
