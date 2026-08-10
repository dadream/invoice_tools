# Tauri 构建环境

## Linux（Ubuntu 22.04 / WSL2）

有 root 权限：

```bash
sudo apt-get install -y libwebkit2gtk-4.1-dev libgtk-3-dev librsvg2-dev patchelf build-essential curl file
```

无 root 权限（本机情况）：

```bash
bash scripts/setup-linux-deps.sh   # 解包到 ~/.local/tauri-sysroot，约 292MB
source scripts/tauri-env.sh        # 每个新 shell 都要执行
```

`scripts/dev-tauri.sh` 与 `scripts/build-tauri.sh` 会自动 source 该脚本。

已知无害告警：`libpng16.so` 悬空（dev 符号链接指向的运行时包未随 apt 下载），不影响 webkit 链接。

## macOS

```bash
xcode-select --install
```

WebKit 由系统提供，无额外依赖。产出 `.app` / `.dmg`。

## Windows

- Visual Studio 2022 Build Tools（含 C++ 桌面开发）
- WebView2 Runtime（Win11 自带；Win10 需安装 Evergreen Bootstrapper）

产出 `.msi` / `.exe`（NSIS）。

## 交叉编译

Tauri 不支持跨平台交叉编译（各平台 webview 为系统组件）。三平台产物需在对应平台或 CI 上分别构建。
