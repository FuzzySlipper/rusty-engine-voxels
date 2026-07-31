#!/usr/bin/env bash
set -Eeuo pipefail

if [[ $# -ne 2 ]]; then
  echo "usage: $0 <repository-root> <staging-directory>" >&2
  exit 2
fi

VOXEL_ROOT="$(realpath "$1")"
STAGING="$(realpath "$2")"
CACHE_ROOT="$VOXEL_ROOT/.video-motion-cache"
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

mkdir -p "$CACHE_ROOT"
ROLLBACK="$(mktemp -d "$CACHE_ROOT/video-motion-rollback.XXXXXX")"
BACKED_UP=0

cleanup() {
  rm -rf -- "$ROLLBACK"
}

rollback() {
  local status=$?
  trap - ERR INT TERM
  set +e
  for ((index = 0; index < BACKED_UP; index++)); do
    destination="$VOXEL_ROOT/${RELATIVE_PATHS[$index]}"
    if [[ -f "$ROLLBACK/$index.original" ]]; then
      install -m 0644 "$ROLLBACK/$index.original" "$destination"
    elif [[ -e "$destination" ]]; then
      unlink "$destination"
    fi
  done
  cleanup
  exit "$status"
}

trap rollback ERR INT TERM
trap cleanup EXIT

for ((index = 0; index < ${#RELATIVE_PATHS[@]}; index++)); do
  source_file="$STAGING/${STAGED_NAMES[$index]}"
  destination="$VOXEL_ROOT/${RELATIVE_PATHS[$index]}"
  if [[ ! -f "$source_file" ]]; then
    echo "missing staged video-motion artifact: $source_file" >&2
    false
  fi
  mkdir -p "$(dirname "$destination")"
  if [[ -f "$destination" ]]; then
    cp --preserve=mode -- "$destination" "$ROLLBACK/$index.original"
  fi
  BACKED_UP=$((index + 1))
done

for ((index = 0; index < ${#RELATIVE_PATHS[@]}; index++)); do
  install -m 0644 \
    "$STAGING/${STAGED_NAMES[$index]}" \
    "$VOXEL_ROOT/${RELATIVE_PATHS[$index]}"
  published=$((index + 1))
  if [[ "${VIDEO_MOTION_FAIL_AFTER_INSTALL:-0}" == "$published" ]]; then
    echo "injected video-motion publication failure after artifact $published" >&2
    false
  fi
done

trap - ERR INT TERM
