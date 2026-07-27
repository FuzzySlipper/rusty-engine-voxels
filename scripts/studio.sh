#!/usr/bin/env bash
set -euo pipefail

VOXEL_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
VOXEL_BIND_HOST="127.0.0.1"
VOXEL_BIND_PORT="4310"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --)
      shift
      ;;
    --host)
      VOXEL_BIND_HOST="${2:-}"
      shift 2
      ;;
    --port)
      VOXEL_BIND_PORT="${2:-}"
      shift 2
      ;;
    *)
      echo "unknown Studio argument: $1" >&2
      exit 2
      ;;
  esac
done

if [[ -z "$VOXEL_BIND_HOST" || "$VOXEL_BIND_HOST" =~ [[:space:]] ]]; then
  echo "--host must be a non-empty address" >&2
  exit 2
fi
if [[ ! "$VOXEL_BIND_PORT" =~ ^[0-9]+$ ]] \
  || (( VOXEL_BIND_PORT < 1 || VOXEL_BIND_PORT > 65535 )); then
  echo "--port must be an integer from 1 through 65535" >&2
  exit 2
fi

VOXEL_PROVIDER_CHECKOUT="$(bash "$VOXEL_ROOT/scripts/studio-provider.sh")"
cargo build --locked \
  --manifest-path "$VOXEL_ROOT/Cargo.toml" \
  --bin rusty-engine-voxels-studio-adapter

VOXEL_ADAPTER="$VOXEL_ROOT/target/debug/rusty-engine-voxels-studio-adapter"
VOXEL_ENCODED_ROOT="$(
  node -e 'console.log(encodeURIComponent(process.argv[1]))' "$VOXEL_ROOT"
)"
VOXEL_QUERY="root=$VOXEL_ENCODED_ROOT&project=content%2Fprojects%2Fvoxel-lab.project.json"
VOXEL_DISPLAY_HOST="$VOXEL_BIND_HOST"
if [[ "$VOXEL_DISPLAY_HOST" == "0.0.0.0" ]]; then
  VOXEL_DISPLAY_HOST="$(hostname -I 2>/dev/null | awk '{print $1}' || true)"
  VOXEL_DISPLAY_HOST="${VOXEL_DISPLAY_HOST:-127.0.0.1}"
fi

printf 'Voxel Lab Studio: http://%s:%s/?%s\n' \
  "$VOXEL_DISPLAY_HOST" "$VOXEL_BIND_PORT" "$VOXEL_QUERY"
exec pnpm --dir "$VOXEL_PROVIDER_CHECKOUT/studio" run serve:den -- \
  --adapter-binary "$VOXEL_ADAPTER" \
  --host "$VOXEL_BIND_HOST" \
  --port "$VOXEL_BIND_PORT"
