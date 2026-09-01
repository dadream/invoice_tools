# Windows 开发与验证工具链

> 基线日期：2026-08-19

## 固定版本

| 组件 | 版本/约束 |
|---|---|
| Windows | Windows 11 x64，标准用户 |
| Rust | 1.97.1 精确固定；workspace 最低版本 1.75 |
| Cargo | 本机验证 1.97.1 |
| Node.js | 24.14.0 |
| npm | 11.9.0 |
| Windows SDK | 10.0.26100.0 |
| Tauri CLI | 2.11.4（`ui/package-lock.json`） |
| Tauri Rust | 2.11.5（`Cargo.lock`） |
| TypeScript | 5.9.3 |
| WebView2 | 系统 Evergreen Runtime；发布包不静默安装 |

依赖安装必须使用锁文件：前端使用 `npm ci`，Rust 使用 `Cargo.lock` 和 `--locked`。

## 干净环境复现

```powershell
git clone <repository>
cd invoice_tools
rustup toolchain install stable --component rustfmt --component clippy
cd ui
npm ci
cd ..
.\scripts\verify-windows.ps1
```

GUI 构建还需要 Visual Studio Build Tools 的 Desktop development with C++、Windows 10/11 SDK 和 WebView2 Runtime。应用以标准用户运行，不需要管理员权限。

## WebView2 记录

系统验证需记录 Runtime 是否存在和版本，但不得自动安装。微软当前定义的 64 位 Windows 检测位置为：

- `HKLM\SOFTWARE\WOW6432Node\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}`
- `HKCU\Software\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}`

至少一个位置的 `pv` 必须存在且大于 `0.0.0.0`。本机验证版本为 `151.0.4129.93`；产品不锁死该版本，使用系统维护的 Evergreen Runtime。

缺失时只提供微软官方下载页和企业 IT 处理说明，不静默下载、不安装、不申请管理员权限：

- 微软分发和检测说明：<https://learn.microsoft.com/microsoft-edge/webview2/concepts/distribution>
- Tauri Windows WebView2 说明：<https://v2.tauri.app/distribute/windows-installer/>

## 统一质量门禁

`.\scripts\verify-windows.ps1` 依次执行秘密扫描、许可证扫描、Rust 格式/Clippy/检查/测试，以及前端检查/测试/生产构建。秘密扫描只输出命中文件名，不输出匹配内容；许可证扫描只读取锁文件和本地 Cargo 元数据，不联网。

## 已知真实验证边界

- 真实 QQ 测试只在秘密扫描、IMAP 只读命令防线和前后指纹功能通过后运行。
- 真实样本测试必须显式使用 `--ignored`；日常回归不会访问真实数据。
- Concur 外发每次都需要产品负责人单独批准。
