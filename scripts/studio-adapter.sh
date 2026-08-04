#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cargo build --quiet --locked \
  --manifest-path "$ROOT/Cargo.toml" \
  --bin rusty-engine-voxels-studio-adapter
exec "$ROOT/target/debug/rusty-engine-voxels-studio-adapter"
