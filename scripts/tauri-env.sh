#!/usr/bin/env bash
# 用法: source scripts/tauri-env.sh
# 让 pkg-config / 链接器 / 运行时都能找到用户级 sysroot 里的 webkit2gtk。
SYSROOT="${TAURI_SYSROOT:-$HOME/.local/tauri-sysroot}"
LIBDIR="$SYSROOT/usr/lib/x86_64-linux-gnu"

export PKG_CONFIG_SYSROOT_DIR="$SYSROOT"
export PKG_CONFIG_PATH="$LIBDIR/pkgconfig:$SYSROOT/usr/share/pkgconfig:$SYSROOT/usr/lib/pkgconfig"
export PKG_CONFIG_ALLOW_SYSTEM_LIBS=1
export PKG_CONFIG_ALLOW_SYSTEM_CFLAGS=1
# 运行时：sysroot 里的 .so.N 不在 ld.so 搜索路径中
export LD_LIBRARY_PATH="$LIBDIR${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
# 链接期 + 运行期（rpath 让产物脱离 LD_LIBRARY_PATH 也能跑）
export RUSTFLAGS="-L $LIBDIR -C link-arg=-Wl,-rpath,$LIBDIR${RUSTFLAGS:+ $RUSTFLAGS}"
export PATH="$SYSROOT/usr/bin:$HOME/.cargo/bin:$PATH"
