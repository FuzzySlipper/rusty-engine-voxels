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

RELATIVE_PATHS=(
  "content/sources/kenney-retro-character/run-multiview.nut"
  "evidence/video-motion/landmarks.json"
  "evidence/video-motion/fitted-motion.json"
  "evidence/video-motion/proxy-motion.json"
  "content/sources/video-fitted-rifleman/motion.glb"
)
STAGED_NAMES=(
  "run-multiview.nut"
  "landmarks.json"
  "fitted-motion.json"
  "proxy-motion.json"
  "motion.glb"
)

for fail_after in 1 2 3 4 5; do
  fixture="$CHECK_ROOT/publication-$fail_after"
  staging="$fixture/staging"
  mkdir -p "$staging"
  for ((index = 0; index < ${#RELATIVE_PATHS[@]}; index++)); do
    destination="$fixture/${RELATIVE_PATHS[$index]}"
    mkdir -p "$(dirname "$destination")"
    cp "$VOXEL_ROOT/${RELATIVE_PATHS[$index]}" "$destination"
    cp "$destination" "$staging/${STAGED_NAMES[$index]}"
    printf '\0changed' >> "$staging/${STAGED_NAMES[$index]}"
  done
  if VIDEO_MOTION_FAIL_AFTER_INSTALL="$fail_after" \
    "$VOXEL_ROOT/scripts/publish-video-motion-evidence.sh" "$fixture" "$staging"
  then
    echo "publication failure injection $fail_after unexpectedly succeeded" >&2
    exit 1
  fi
  for relative_path in "${RELATIVE_PATHS[@]}"; do
    cmp "$fixture/$relative_path" "$VOXEL_ROOT/$relative_path"
  done
done
