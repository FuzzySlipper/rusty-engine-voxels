#!/usr/bin/env bash
set -euo pipefail

VOXEL_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
VOXEL_BIND_HOST="127.0.0.1"
VOXEL_BIND_PORT="4310"
VOXEL_PROJECT_FILE="content/projects/voxel-lab.project.json"

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
    --project)
      VOXEL_PROJECT_FILE="${2:-}"
      shift 2
      ;;
    *)
      echo "unknown Studio argument: $1" >&2
      exit 2
      ;;
  esac
done

if [[ -z "$VOXEL_PROJECT_FILE" || "$VOXEL_PROJECT_FILE" == /* \
  || "$VOXEL_PROJECT_FILE" == ".." || "$VOXEL_PROJECT_FILE" == ../* \
  || "$VOXEL_PROJECT_FILE" == */../* || "$VOXEL_PROJECT_FILE" == */.. ]]; then
  echo "--project must be a non-empty project-relative path" >&2
  exit 2
fi
if [[ ! -f "$VOXEL_ROOT/$VOXEL_PROJECT_FILE" ]]; then
  echo "--project does not name a project file below the repository: $VOXEL_PROJECT_FILE" >&2
  exit 2
fi

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
pnpm --dir "$VOXEL_PROVIDER_CHECKOUT/studio" run build

VOXEL_ADAPTER="$VOXEL_ROOT/target/debug/rusty-engine-voxels-studio-adapter"
VOXEL_ENCODED_ROOT="$(
  node -e 'console.log(encodeURIComponent(process.argv[1]))' "$VOXEL_ROOT"
)"
VOXEL_ENCODED_PROJECT="$(
  node -e 'console.log(encodeURIComponent(process.argv[1]))' "$VOXEL_PROJECT_FILE"
)"
VOXEL_QUERY="root=$VOXEL_ENCODED_ROOT&project=$VOXEL_ENCODED_PROJECT"
VOXEL_DISPLAY_HOST="$VOXEL_BIND_HOST"
if [[ "$VOXEL_DISPLAY_HOST" == "0.0.0.0" ]]; then
  VOXEL_DISPLAY_HOST="$(hostname -I 2>/dev/null | awk '{print $1}' || true)"
  VOXEL_DISPLAY_HOST="${VOXEL_DISPLAY_HOST:-127.0.0.1}"
fi

printf 'Voxel Lab Studio: http://%s:%s/?%s\n' \
  "$VOXEL_DISPLAY_HOST" "$VOXEL_BIND_PORT" "$VOXEL_QUERY"
exec pnpm --dir "$VOXEL_PROVIDER_CHECKOUT/studio" run host -- \
  --adapter-binary "$VOXEL_ADAPTER" \
  --host "$VOXEL_BIND_HOST" \
  --port "$VOXEL_BIND_PORT"
