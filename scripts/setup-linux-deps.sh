#!/usr/bin/env bash
# 无 sudo 安装 Tauri 的 Linux 构建依赖到用户级 sysroot。
# 背景：本机无 root 权限，且配置的 apt 镜像对 .deb 返回 403，因此改写为 archive.ubuntu.com。
set -uo pipefail

SYSROOT="${TAURI_SYSROOT:-$HOME/.local/tauri-sysroot}"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

PKGS=(libwebkit2gtk-4.1-dev libgtk-3-dev librsvg2-dev patchelf)

echo "==> 解析依赖闭环"
apt-get install --print-uris -y --no-install-recommends "${PKGS[@]}" 2>/dev/null \
  | grep "^'" | sed "s/^'//; s/'.*//" \
  | sed -E 's#https?://[^/]+/ubuntu#https://archive.ubuntu.com/ubuntu#' \
  > "$WORK/uris.txt"
echo "    共 $(wc -l < "$WORK/uris.txt") 个包"

echo "==> 下载"
mkdir -p "$WORK/debs" "$SYSROOT"
( cd "$WORK/debs" && xargs -P 8 -n 1 curl -sSfL -O --retry 3 < "$WORK/uris.txt" )

echo "==> 解包到 $SYSROOT"
for d in "$WORK"/debs/*.deb; do
  dpkg-deb -x "$d" "$SYSROOT" || { echo "解包失败: $d" >&2; exit 1; }
done

echo "==> 修复悬空符号链接"
# dev 包提供 libfoo.so -> libfoo.so.N，但 apt 认为运行时包已装在系统里而未下载，
# 导致 sysroot 内的符号链接悬空。把它们指向系统目录下的真实文件。
LIBDIR="$SYSROOT/usr/lib/x86_64-linux-gnu"
SYS=/usr/lib/x86_64-linux-gnu
fixed=0; broken=0
for l in "$LIBDIR"/*.so; do
  [ -L "$l" ] && [ ! -e "$l" ] || continue
  tgt="$(readlink "$l")"
  if [ -e "$SYS/$tgt" ]; then ln -sf "$SYS/$tgt" "$LIBDIR/$tgt"; fixed=$((fixed+1))
  else echo "    仍缺失: $tgt" >&2; broken=$((broken+1)); fi
done
echo "    已修复 $fixed，仍缺失 $broken（libpng16.so 缺失是已知且无害的）"

echo "==> 校验"
# shellcheck source=/dev/null
source "$(dirname "$0")/tauri-env.sh"
rc=0
for p in webkit2gtk-4.1 javascriptcoregtk-4.1 libsoup-3.0 gtk+-3.0; do
  if pkg-config --exists "$p"; then echo "    OK   $p $(pkg-config --modversion "$p")"
  else echo "    MISS $p" >&2; rc=1; fi
done
[ $rc -eq 0 ] && echo "✅ sysroot 就绪：$SYSROOT" || echo "❌ sysroot 不完整" >&2
exit $rc
