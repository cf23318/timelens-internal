#!/usr/bin/env bash
set -euo pipefail
cd "$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
[[ "$(uname -s)" == Darwin ]] || { echo '请在 macOS 构建'; exit 1; }
command -v cargo >/dev/null
command -v npm >/dev/null
node -e 'if (Number(process.versions.node.split(".")[0]) < 22) throw new Error("需要 Node.js 22 或更新版本")'
[[ -d node_modules ]] || { echo '请先使用代理按 package-lock.json 安装 npm 依赖：npm ci --ignore-scripts'; exit 1; }
# Only use already-installed dependencies by default. This script never fetches Git remotes.
export CARGO_NET_OFFLINE="${CARGO_NET_OFFLINE:-true}"
npm run tauri -- build --bundles app
echo '本地 .app 构建完成；分发前仍需使用组织证书签名、公证并完成实机验收。'
