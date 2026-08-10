#!/usr/bin/env bash
# S0.2 验收：逐条检查交付物
set -uo pipefail
cd "$(dirname "$0")/.."
export PATH="$HOME/.cargo/bin:$PATH"

fails=0
check() {
  if eval "$2" >/dev/null 2>&1; then
    echo "  ✅ $1"
  else
    echo "  ❌ $1"
    fails=$((fails+1))
  fi
}

echo "== 1. 构建依赖 =="
if [ "$(uname)" = "Linux" ]; then
  # shellcheck source=/dev/null
  source scripts/tauri-env.sh
  check "webkit2gtk-4.1 可用" "pkg-config --exists webkit2gtk-4.1"
  check "gtk+-3.0 可用" "pkg-config --exists gtk+-3.0"
fi

echo "== 2. workspace 接入 =="
check "src-tauri 在 workspace 内" "cargo metadata --no-deps --format-version 1 | grep -q invoice-assistant"
check "build.rs 位置正确" "test -f src-tauri/build.rs"
check "无残留 src-tauri/src-tauri" "! test -d src-tauri/src-tauri"

echo "== 3. 编译与测试 =="
# invoice-assistant needs tauri-env, so source it first
# shellcheck source=/dev/null
[ "$(uname)" = "Linux" ] && source scripts/tauri-env.sh
check "invoice-assistant 编译通过" "cargo build -p invoice-assistant"
# workspace tests (excluding invoice-assistant) conflict with tauri-env, run in clean environment
WORKDIR="$(pwd)"
if (unset PKG_CONFIG_SYSROOT_DIR PKG_CONFIG_PATH PKG_CONFIG_ALLOW_SYSTEM_LIBS PKG_CONFIG_ALLOW_SYSTEM_CFLAGS LD_LIBRARY_PATH RUSTFLAGS; export PATH="$HOME/.cargo/bin:$PATH"; cd "$WORKDIR" && cargo test --workspace --exclude invoice-assistant) >/dev/null 2>&1; then
  echo "  ✅ workspace 测试通过（非 Tauri）"
else
  echo "  ❌ workspace 测试通过（非 Tauri）"
  fails=$((fails+1))
fi
check "invoice-assistant 测试通过" "cargo test -p invoice-assistant"
echo -n "  warning 数: "; cargo build -p invoice-assistant 2>&1 | grep -c '^warning' || true

echo "== 4. 前端 =="
check "node_modules 已装" "test -d ui/node_modules"
if (cd ui && npx svelte-check --tsconfig ./tsconfig.json --threshold error) >/dev/null 2>&1; then
  echo "  ✅ 前端类型检查"
else
  echo "  ❌ 前端类型检查"
  fails=$((fails+1))
fi
if (cd ui && npx vitest run) >/dev/null 2>&1; then
  echo "  ✅ 前端单测通过"
else
  echo "  ❌ 前端单测通过"
  fails=$((fails+1))
fi
if (cd ui && npm run build && test -f dist/index.html) >/dev/null 2>&1; then
  echo "  ✅ 前端可构建"
else
  echo "  ❌ 前端可构建"
  fails=$((fails+1))
fi

echo "== 5. 日志 =="
check "日志目录可创建" "cargo test -p invoice-assistant logger"

echo "== 6. 打包资源 =="
if test -f src-tauri/icons/icon.png && test -f src-tauri/icons/32x32.png; then
  echo "  ✅ 图标齐备"
else
  echo "  ❌ 图标齐备"
  fails=$((fails+1))
fi
check "capabilities 存在" "test -f src-tauri/capabilities/default.json"

echo
[ $fails -eq 0 ] && echo "✅ S0.2 验收通过" || echo "❌ $fails 项未通过"
exit $fails
