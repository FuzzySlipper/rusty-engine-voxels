#!/usr/bin/env bash
set -euo pipefail

VOXEL_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CHECK_ROOT="$(mktemp -d)"
trap 'rm -rf -- "$CHECK_ROOT"' EXIT

cargo run --quiet --manifest-path "$VOXEL_ROOT/Cargo.toml" \
  --bin video-motion-fit --locked -- \
  "$VOXEL_ROOT/evidence/video-motion/landmarks.json" \
  "$CHECK_ROOT/fitted-motion.json"
cargo run --quiet --manifest-path "$VOXEL_ROOT/Cargo.toml" \
  --bin video-motion-calibrate --locked -- \
  "$VOXEL_ROOT" \
  "$CHECK_ROOT/fitted-motion.json" \
  "$CHECK_ROOT/proxy-motion.json"
python3 "$VOXEL_ROOT/tools/video-motion/export_motion_glb.py" \
  --motion "$CHECK_ROOT/proxy-motion.json" \
  --output "$CHECK_ROOT/motion.glb"

cmp "$CHECK_ROOT/fitted-motion.json" \
  "$VOXEL_ROOT/evidence/video-motion/fitted-motion.json"
cmp "$CHECK_ROOT/proxy-motion.json" \
  "$VOXEL_ROOT/evidence/video-motion/proxy-motion.json"
cmp "$CHECK_ROOT/motion.glb" \
  "$VOXEL_ROOT/content/sources/video-fitted-rifleman/motion.glb"
