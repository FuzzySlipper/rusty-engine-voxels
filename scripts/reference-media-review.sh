#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
MANIFEST="${1:-content/reviews/directional-sentinel-reference-review.json}"
CAPTURE_ROOT="${RUSTY_STUDIO_CAPTURE_ROOT:-$ROOT/local/reference-media-review}"
PROJECT_FILE="${RUSTY_STUDIO_PROJECT_FILE:-content/projects/directional-sprite-experiment.project.json}"
PROVIDER_ROOT="${RUSTY_STUDIO_PROVIDER_ROOT:-$($ROOT/scripts/studio-provider.sh)}"
PROVIDER_COMMIT="$(git -C "$PROVIDER_ROOT" rev-parse HEAD)"
ADAPTER_BINARY="${RUSTY_STUDIO_ADAPTER_BINARY:-$ROOT/target/debug/rusty-engine-voxels-studio-adapter}"
SETTINGS_ROOT="${RUSTY_STUDIO_SETTINGS_ROOT:-$(mktemp -d /tmp/rusty-studio-reference-review.XXXXXX)}"

if [[ ! -x "$ADAPTER_BINARY" ]]; then
  cargo build --quiet --locked --manifest-path "$ROOT/Cargo.toml" --bin rusty-engine-voxels-studio-adapter
fi

export RUSTY_STUDIO_PROVIDER_ROOT="$PROVIDER_ROOT"
export RUSTY_STUDIO_REFERENCE_MANIFEST="$MANIFEST"
export RUSTY_STUDIO_PROJECT_ROOT="$ROOT"
export RUSTY_STUDIO_PROJECT_FILE="$PROJECT_FILE"
export RUSTY_STUDIO_CAPTURE_ROOT="$CAPTURE_ROOT"
export RUSTY_STUDIO_PROVIDER_COMMIT="$PROVIDER_COMMIT"
export RUSTY_STUDIO_ENGINE_COMMIT="$PROVIDER_COMMIT"
export RUSTY_STUDIO_ADAPTER_BINARY="$ADAPTER_BINARY"
export RUSTY_STUDIO_SETTINGS_ROOT="$SETTINGS_ROOT"

pnpm --dir "$PROVIDER_ROOT/studio" exec playwright test \
  --config "$ROOT/scripts/reference-media-review.playwright.config.mjs"
