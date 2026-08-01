#!/usr/bin/env bash
set -euo pipefail

VOXEL_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ENGINE_COMMIT="${1:-}"
if [[ ! "$ENGINE_COMMIT" =~ ^[0-9a-f]{40}$ ]]; then
  echo "usage: $0 <40-character-reverse-provider-commit>" >&2
  exit 2
fi

CACHE_ROOT="${RUSTY_ENGINE_VOXELS_REVERSE_CACHE_ROOT:-$VOXEL_ROOT/.studio-cache/reverse-provider}"
CHECKOUT="$CACHE_ROOT/$ENGINE_COMMIT"
mkdir -p "$CACHE_ROOT"
if [[ ! -d "$CHECKOUT/.git" ]]; then
  git clone --filter=blob:none --no-checkout \
    https://github.com/FuzzySlipper/rusty-engine.git "$CHECKOUT"
fi
git -C "$CHECKOUT" fetch --depth=1 origin "$ENGINE_COMMIT"
git -C "$CHECKOUT" checkout --detach --force FETCH_HEAD
if [[ "$(git -C "$CHECKOUT" rev-parse HEAD)" != "$ENGINE_COMMIT" ]]; then
  echo "reverse provider checkout did not resolve exact commit $ENGINE_COMMIT" >&2
  exit 1
fi
if [[ -n "$(git -C "$CHECKOUT" status --porcelain=v1 --untracked-files=all)" ]]; then
  echo "reverse provider checkout must be clean before certification" >&2
  exit 1
fi

"$CHECKOUT/scripts/verify-studio-voxel-integration.sh" "$VOXEL_ROOT"

if [[ -n "$(git -C "$CHECKOUT" status --porcelain=v1 --untracked-files=all)" ]]; then
  echo "reverse provider integration mutated its exact checkout" >&2
  exit 1
fi
