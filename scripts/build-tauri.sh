#!/usr/bin/env bash
# 构建 Tauri 发布产物
set -euo pipefail
cd "$(dirname "$0")/.."

# shellcheck source=/dev/null
[ "$(uname)" = "Linux" ] && source scripts/tauri-env.sh
export PATH="$HOME/.cargo/bin:$PATH"

[ -d ui/node_modules ] || (cd ui && npm ci)

echo "==> 构建（前端由 tauri 的 beforeBuildCommand 触发）"
cd src-tauri && cargo tauri build "$@"

cd ..
echo "✅ 完成。产物："
find target/release -maxdepth 1 -name invoice-assistant -o -maxdepth 1 -name '*.AppImage' 2>/dev/null | head
ls target/release/bundle 2>/dev/null || true
