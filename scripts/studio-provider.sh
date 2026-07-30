#!/usr/bin/env bash
set -euo pipefail

VOXEL_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
VOXEL_SOURCE_FILE="$VOXEL_ROOT/engine-source.json"
VOXEL_CACHE_ROOT="${RUSTY_ENGINE_VOXELS_STUDIO_CACHE_ROOT:-$VOXEL_ROOT/.studio-cache/provider}"

"$VOXEL_ROOT/scripts/engine-revision" check >/dev/null

mapfile -t VOXEL_SOURCE < <(
  node --input-type=module -e '
    import { readFileSync } from "node:fs";
    const source = JSON.parse(readFileSync(process.argv[1], "utf8"));
    console.log(source.repository);
    console.log(source.commit);
  ' "$VOXEL_SOURCE_FILE"
)

VOXEL_PROVIDER_REPOSITORY="${VOXEL_SOURCE[0]}"
VOXEL_PROVIDER_COMMIT="${VOXEL_SOURCE[1]}"
VOXEL_PROVIDER_CHECKOUT="$VOXEL_CACHE_ROOT/$VOXEL_PROVIDER_COMMIT"

mkdir -p "$VOXEL_CACHE_ROOT"
if [[ ! -d "$VOXEL_PROVIDER_CHECKOUT/.git" ]]; then
  git clone --filter=blob:none \
    "$VOXEL_PROVIDER_REPOSITORY" "$VOXEL_PROVIDER_CHECKOUT" >&2
fi

VOXEL_PROVIDER_ORIGIN="$(git -C "$VOXEL_PROVIDER_CHECKOUT" remote get-url origin)"
if [[ "$VOXEL_PROVIDER_ORIGIN" != "$VOXEL_PROVIDER_REPOSITORY" ]]; then
  echo "cached Studio provider has unexpected origin: $VOXEL_PROVIDER_ORIGIN" >&2
  exit 1
fi
if [[ -n "$(git -C "$VOXEL_PROVIDER_CHECKOUT" status --porcelain)" ]]; then
  echo "cached Studio provider is dirty: $VOXEL_PROVIDER_CHECKOUT" >&2
  exit 1
fi
if ! git -C "$VOXEL_PROVIDER_CHECKOUT" cat-file -e "$VOXEL_PROVIDER_COMMIT^{commit}" 2>/dev/null; then
  git -C "$VOXEL_PROVIDER_CHECKOUT" fetch --depth 1 origin "$VOXEL_PROVIDER_COMMIT" >&2
fi
git -C "$VOXEL_PROVIDER_CHECKOUT" checkout --quiet --detach "$VOXEL_PROVIDER_COMMIT"

VOXEL_RESOLVED_COMMIT="$(git -C "$VOXEL_PROVIDER_CHECKOUT" rev-parse HEAD)"
if [[ "$VOXEL_RESOLVED_COMMIT" != "$VOXEL_PROVIDER_COMMIT" ]]; then
  echo "Studio provider resolved $VOXEL_RESOLVED_COMMIT, expected $VOXEL_PROVIDER_COMMIT" >&2
  exit 1
fi

pnpm --dir "$VOXEL_PROVIDER_CHECKOUT/studio" install --frozen-lockfile >&2
printf '%s\n' "$VOXEL_PROVIDER_CHECKOUT"
