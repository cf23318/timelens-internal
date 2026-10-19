#!/usr/bin/env bash
# Build only: never installs/starts the app, grants permissions, or uploads data.
set -euo pipefail
[[ "$(uname -s)" == "Darwin" ]] || { echo "This build requires macOS." >&2; exit 1; }
TIMELENS_BUILD_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
[[ -d "$TIMELENS_BUILD_ROOT/project/node_modules" ]] || { echo "Install the reviewed project dependencies first." >&2; exit 1; }
node -e 'if (Number(process.versions.node.split(".")[0]) < 18) { console.error("Use Node.js 18+ (20+ recommended) on PATH before building."); process.exit(1); }'
command -v cargo >/dev/null || { echo "Put the reviewed Rust toolchain on PATH first." >&2; exit 1; }
cd "$TIMELENS_BUILD_ROOT/project"
npm run tauri -- build --bundles app --no-sign
python3 "$TIMELENS_BUILD_ROOT/scripts/source-fingerprint.py" "$TIMELENS_BUILD_ROOT"
