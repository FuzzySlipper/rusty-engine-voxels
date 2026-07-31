#!/usr/bin/env bash
set -euo pipefail

VOXEL_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CACHE_ROOT="$VOXEL_ROOT/.video-motion-cache"
PYTHON_ENV="$CACHE_ROOT/python"
BLENDER_ENV="$CACHE_ROOT/blender-python"
MODEL="$CACHE_ROOT/pose_landmarker_full.task"
SOURCE="$VOXEL_ROOT/content/sources/kenney-retro-character/character-medium.glb"
VIDEO_PATH="content/sources/kenney-retro-character/run-multiview.nut"
LANDMARKS_PATH="evidence/video-motion/landmarks.json"
FITTED_PATH="evidence/video-motion/fitted-motion.json"
PROXY_MOTION_PATH="evidence/video-motion/proxy-motion.json"
MOTION_PATH="content/sources/video-fitted-rifleman/motion.glb"
MODEL_URL="https://storage.googleapis.com/mediapipe-models/pose_landmarker/pose_landmarker_full/float16/1/pose_landmarker_full.task"
MODEL_SHA256="5134a3aad27a58b93da0088d431f366da362b44e3ccfbe3462b3827a839011b1"

mkdir -p "$CACHE_ROOT"
STAGING="$(mktemp -d "$CACHE_ROOT/staging.XXXXXX")"
trap 'rm -rf "$STAGING"' EXIT
VIDEO="$STAGING/run-multiview.nut"
LANDMARKS="$STAGING/landmarks.json"
FITTED="$STAGING/fitted-motion.json"
PROXY_MOTION="$STAGING/proxy-motion.json"
MOTION="$STAGING/motion.glb"

UV_PROJECT_ENVIRONMENT="$PYTHON_ENV" \
  uv sync --project "$VOXEL_ROOT/tools/video-motion" --locked --python 3.12
if [[ ! -x "$BLENDER_ENV/bin/python" ]]; then
  uv venv --python /usr/bin/python3.14 "$BLENDER_ENV"
fi
uv pip install --python "$BLENDER_ENV/bin/python" "numpy==2.5.1"

if [[ ! -f "$MODEL" ]]; then
  curl -fL --retry 2 -o "$MODEL" "$MODEL_URL"
fi
printf '%s  %s\n' "$MODEL_SHA256" "$MODEL" | sha256sum --check -

PYTHONPATH="$BLENDER_ENV/lib/python3.14/site-packages" \
  blender --python-use-system-env -b \
    --python "$VOXEL_ROOT/tools/video-motion/render_multiview.py" -- \
    --source "$SOURCE" \
    --output "$CACHE_ROOT/rendered" \
    --clip run \
    --frames 16

"$PYTHON_ENV/bin/python" "$VOXEL_ROOT/tools/video-motion/build_evidence.py" \
  --rendered "$CACHE_ROOT/rendered" \
  --model "$MODEL" \
  --source "$SOURCE" \
  --video "$VIDEO" \
  --video-label "$VIDEO_PATH" \
  --landmarks "$LANDMARKS" \
  --root "$VOXEL_ROOT"

cargo run --quiet --manifest-path "$VOXEL_ROOT/Cargo.toml" \
  --bin video-motion-fit --locked -- "$LANDMARKS" "$FITTED"
cargo run --quiet --manifest-path "$VOXEL_ROOT/Cargo.toml" \
  --bin video-motion-calibrate --locked -- "$VOXEL_ROOT" "$FITTED" "$PROXY_MOTION"
"$PYTHON_ENV/bin/python" "$VOXEL_ROOT/tools/video-motion/export_motion_glb.py" \
  --motion "$PROXY_MOTION" \
  --output "$MOTION"

install -m 0644 "$VIDEO" "$VOXEL_ROOT/$VIDEO_PATH"
install -m 0644 "$LANDMARKS" "$VOXEL_ROOT/$LANDMARKS_PATH"
install -m 0644 "$FITTED" "$VOXEL_ROOT/$FITTED_PATH"
install -m 0644 "$PROXY_MOTION" "$VOXEL_ROOT/$PROXY_MOTION_PATH"
install -m 0644 "$MOTION" "$VOXEL_ROOT/$MOTION_PATH"
