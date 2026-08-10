#!/usr/bin/env bash
# 启动 Tauri 开发环境（前端 dev server + Rust 热重载）
set -euo pipefail
cd "$(dirname "$0")/.."

# shellcheck source=/dev/null
[ "$(uname)" = "Linux" ] && source scripts/tauri-env.sh
export PATH="$HOME/.cargo/bin:$PATH"

[ -d ui/node_modules ] || (cd ui && npm ci)

# 用项目内固定版本的 CLI，而非全局 cargo-tauri
cd ui && npx tauri dev
