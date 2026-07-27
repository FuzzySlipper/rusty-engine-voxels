#!/usr/bin/env bash
set -euo pipefail

VOXEL_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

cargo fmt --manifest-path "$VOXEL_ROOT/Cargo.toml" -- --check
cargo clippy --locked --all-targets \
  --manifest-path "$VOXEL_ROOT/Cargo.toml" \
  -- -D warnings -A clippy::pedantic
cargo test --locked --all-targets --manifest-path "$VOXEL_ROOT/Cargo.toml"
cargo run --quiet --locked \
  --manifest-path "$VOXEL_ROOT/Cargo.toml" \
  --bin voxel-lab -- load --root "$VOXEL_ROOT" >/dev/null
