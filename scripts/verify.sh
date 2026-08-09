#!/usr/bin/env bash
set -euo pipefail

VOXEL_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

node "$VOXEL_ROOT/scripts/generate-textured-voxel-fixture.mjs" --check
"$VOXEL_ROOT/scripts/engine-revision" check
"$VOXEL_ROOT/scripts/engine-revision" dev sync --report-only >/dev/null
"$VOXEL_ROOT/scripts/engine-revision" dev check
CARGO_TARGET_DIR="$VOXEL_ROOT/target/engine-revision" \
  cargo fmt --manifest-path "$VOXEL_ROOT/tools/engine-revision/Cargo.toml" -- --check
CARGO_TARGET_DIR="$VOXEL_ROOT/target/engine-revision" \
  cargo clippy --locked --all-targets \
    --manifest-path "$VOXEL_ROOT/tools/engine-revision/Cargo.toml" -- -D warnings
CARGO_TARGET_DIR="$VOXEL_ROOT/target/engine-revision" \
  cargo test --locked --all-targets \
    --manifest-path "$VOXEL_ROOT/tools/engine-revision/Cargo.toml"
cargo fmt --manifest-path "$VOXEL_ROOT/Cargo.toml" -- --check
cargo clippy --locked --all-targets \
  --manifest-path "$VOXEL_ROOT/Cargo.toml" \
  -- -D warnings -A clippy::pedantic
cargo test --locked --all-targets --manifest-path "$VOXEL_ROOT/Cargo.toml"
cargo run --quiet --locked \
  --manifest-path "$VOXEL_ROOT/Cargo.toml" \
  --bin textured-voxel-evidence -- --check
"$VOXEL_ROOT/scripts/check-video-motion-evidence.sh"
cargo run --quiet --locked \
  --manifest-path "$VOXEL_ROOT/Cargo.toml" \
  --bin voxel-lab -- load --root "$VOXEL_ROOT" >/dev/null
