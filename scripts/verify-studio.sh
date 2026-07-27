#!/usr/bin/env bash
set -euo pipefail

VOXEL_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
VOXEL_PROVIDER_CHECKOUT="$(bash "$VOXEL_ROOT/scripts/studio-provider.sh")"

cargo build --locked \
  --manifest-path "$VOXEL_ROOT/Cargo.toml" \
  --bin rusty-engine-voxels-studio-adapter
pnpm --dir "$VOXEL_PROVIDER_CHECKOUT/studio" \
  --filter @rusty-engine/studio-adapter-client run build
node "$VOXEL_ROOT/scripts/protocol-smoke.mjs" \
  "$VOXEL_PROVIDER_CHECKOUT/studio/libs/adapter-client/dist/index.js" \
  "$VOXEL_ROOT/target/debug/rusty-engine-voxels-studio-adapter" \
  "$VOXEL_ROOT"
