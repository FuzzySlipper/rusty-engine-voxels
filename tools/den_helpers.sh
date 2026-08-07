#!/usr/bin/env bash
# Convenience wrappers around tools/den-mcp-wrapper.py for Muse sessions where native MCP tools are not auto-populated.
# Usage: ./tools/den_helpers.sh get_task 6629
#        ./tools/den_helpers.sh get_task_context 6629  # alias for get_task
#        ./tools/den_helpers.sh query_librarian "voxel conversion"
set -euo pipefail
TOOL="$1"; shift || true
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WRAPPER="$ROOT/tools/den-mcp-wrapper.py"
if [[ "$TOOL" == "get_task_context" ]]; then TOOL="get_task"; fi
# parse remaining args as --arg k=v
ARGS=()
for a in "$@"; do
  ARGS+=(--arg "$a")
done
# special handling for query_librarian etc where query may contain spaces: pass as --args JSON instead
if [[ $# -eq 1 && "$1" == *" "* ]]; then
  python3 "$WRAPPER" "$TOOL" --args "{\"query\":\"$1\"}"
else
  python3 "$WRAPPER" "$TOOL" "${ARGS[@]}"
fi
