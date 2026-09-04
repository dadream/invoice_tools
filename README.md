# Invoice Assistant

Invoice Assistant 是面向 Windows 的免安装、本地优先发票报销整理工具。它用于收集和整理发票材料、生成可审核费用、完成差旅归组，并在审核后导出 Excel 或打印 PDF；企业 Concur 权限满足要求时，还可创建未提交的报销单和费用草稿。

当前版本用于内部 Alpha 验证，不代表已满足公开发布条件。发布范围和验收标准以 [MVP 发布基线](specs/mvp-release-baseline/) 为准。

## 主要流程

1. 创建邮件收集任务，通过 IMAP 只读搜索邮件、分类并保存附件；或在报销批次中导入本地发票和配套材料。
2. 审核邮件来源，补充需要人工下载的文件，并排除无关邮件或无效附件。
3. 创建报销批次，全量导入已确认材料，执行解析、去重和费用生成。
4. 在费用清单中核对日期、费用类型、金额、交易方和材料。
5. 在归组视图中核对差旅行程、市内消费、快递物流及其关联材料。
6. 完成审核后导出 Excel、生成打印 PDF，或进入 Concur 草稿交付。

本地导入面向 PDF、OFD 和图片等发票或报销材料，不导入 `.eml` 邮件文件。Concur 功能依赖企业批准的 OAuth 应用、账号权限和字段映射；软件只创建未提交草稿，不自动提交报销单。

## 仓库结构

```text
.
├── crates/              # 采集、解析、归组和本地存储等 Rust 核心模块
├── src-tauri/           # Windows 桌面应用后端
├── ui/                  # Svelte 前端
├── scripts/             # Windows 验证、构建和发布脚本
├── specs/               # 当前产品规格与验收基线
└── docs/                # 产品、测试、安全和发布设计文档
```

## 开发环境

- Windows 11 x64
- Rust 1.97.1（由 `rust-toolchain.toml` 固定）
- Node.js 24.14.0（由 `.nvmrc` 固定）
- npm 11.9.0（本仓库前端唯一包管理器）
- Microsoft Edge WebView2 Runtime
- Microsoft C++ Build Tools

安装前端锁定依赖：

```powershell
npm --prefix ui ci
```

## 本地运行

```powershell
npm --prefix ui run tauri -- dev
```

开发模式支持前端热重载；Rust 后端发生变化时，Tauri 会重新编译并重启应用。

## 验证

运行 Windows 完整验证入口：

```powershell
powershell -ExecutionPolicy Bypass -File .\scripts\verify-windows.ps1
```

该脚本统一执行磁盘空间门禁、敏感信息扫描、依赖与版本检查、Rust 格式和静态检查、Rust/前端测试及前端构建。真实邮箱和 Concur 验证只在明确授权的受控环境中执行，不属于默认自动化测试。

## 构建免安装包

```powershell
powershell -ExecutionPolicy Bypass -File .\scripts\build-portable.ps1
```

脚本先执行完整验证，再构建 Windows x64 免安装包、校验文件和 SBOM，产物写入 `artifacts/`。同名产物已存在时脚本会停止，不会覆盖已有发布文件。

版本发布和 GitHub Actions 约定见 [分发包与 GitHub Release 策略](docs/release/distribution-package-and-github-release-strategy.md)。

## 数据与安全边界

- 用户数据默认保存在 `%LOCALAPPDATA%\InvoiceAssistant\Data`。
- 邮件收集只读访问邮箱，不应改变邮件 FLAGS。
- 仓库、日志、测试夹具和发布包不得包含真实邮箱授权码、OAuth 密钥、访问令牌或真实用户材料。
- Concur 测试凭据和令牌不写入数据库、备份或日志。
- 跨电脑迁移由用户主动导出和导入备份完成。

## 相关文档

- [产品设计一致性规范](docs/product-design-consistency-standards.md)
- [Concur 字段映射设计](docs/concur-field-mapping-design.md)
- [发布缺陷与门禁](docs/release/defect-and-release-gates.md)
- [历史文档使用说明](docs/HISTORICAL-DOCUMENTS.md)
- [仓库协作与构建规则](AGENTS.md)

## 许可证与发布状态

仓库尚未确认对外软件许可证。内部 Alpha 产物不得视为公开正式版，公开发布前必须完成签名主体、许可证、隐私条款和支持信息确认。
