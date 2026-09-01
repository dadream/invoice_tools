# Invoice Assistant

Windows 免安装、本地优先的发票收集、审核与报销整理工具。

> 当前状态：开发验证阶段，尚不是公开可发布版本。
> 唯一发布规格：`specs/mvp-release-baseline/` 1.0（已确认、实施中）。
> 历史文档不能作为当前完成度、安全或发布结论，见 `docs/HISTORICAL-DOCUMENTS.md`。

MVP 不调用外部大模型，不要求产品账号，不持久化邮箱授权码。用户数据默认保存在
`%LOCALAPPDATA%\InvoiceAssistant\Data`；跨电脑备份为用户主动操作的未加密包。

## 项目结构

```
.
├── crates/              # Rust 核心库
│   ├── invoice-parse    # 多格式发票解析
│   ├── invoice-collect  # IMAP 邮箱采集
│   ├── invoice-grouping # 行程归组引擎
│   └── invoice-store    # 本地台账与旧版凭据兼容
├── src-tauri/          # Tauri 后端
│   └── src/            # Rust 应用代码
└── ui/                 # Svelte 前端
    └── src/            # UI 代码
```

## 开发环境要求

- Windows 11 x64
- Rust 1.97.1（由 `rust-toolchain.toml` 精确固定）
- Node.js 24.14.0、npm 11.9.0
- WebView2 Evergreen Runtime 与 Windows C++ 构建工具

## 快速开始

### 安装依赖

```bash
# 使用锁文件安装前端依赖
cd ui
npm ci
cd ..
```

### 开发模式

```bash
cd ui
npm run tauri dev
```

这将启动：
- Vite 开发服务器（http://localhost:5173）
- Tauri 应用窗口（热重载）

### 构建发布版

```bash
cd ui
npm run tauri build -- --no-bundle
```

Windows 可执行文件位于根目录 `target/release/invoice-assistant.exe`。标准 portable ZIP
和签名流水线仍属于发布任务，不能直接分发裸 EXE。

## 测试

```bash
powershell -ExecutionPolicy Bypass -File .\scripts\verify-windows.ps1
```

## 日志

Windows 默认日志目录：

`%LOCALAPPDATA%\InvoiceAssistant\Data\logs`

日志仅允许记录版本、阶段、错误码、计数和脱敏路径，不记录授权码、邮件正文、
完整票号、税号或金额明细。开发模式下同时输出到终端。

## IPC 通道约定

前端通过 Tauri 的 `invoke` 调用后端命令：

```typescript
import { invoke } from '@tauri-apps/api/core'

// 成功调用
const result = await invoke('command_name', { arg1: value1 })

// 错误处理
try {
  const result = await invoke('command_name', { arg1: value1 })
} catch (error) {
  // error 格式: { type: 'ErrorType', message: 'error message' }
  console.error('Command failed:', error)
}
```

### 可用命令

#### 基础命令
- `greet(name: string) -> string` - 测试命令
- `get_version() -> VersionInfo` - 获取应用版本
- `health_check() -> HealthInfo` - 健康检查

#### 批次管理
- `list_batches() -> Vec<BatchDto>` - 列出所有批次
- `get_batch(id: i64) -> BatchDto` - 获取批次详情
- `create_batch(name: string, month: string) -> i64` - 创建新批次
- `transition_batch_status(id: i64, new_status: string) -> ()` - 转换批次状态
- `delete_batch(id: i64) -> ()` - 删除草稿批次

## 架构说明

### 错误处理

所有后端错误通过 `AppError` 枚举统一处理，自动序列化为 JSON 传递给前端：

```rust
pub enum AppError {
    Database(String),
    Parse(String),
    Network(String),
    Io(String),
    Validation(String),
    Internal(String),
}
```

前端接收到的错误格式：

```json
{
  "type": "Database",
  "message": "数据库错误: connection failed"
}
```

### 日志系统

使用 `tracing` 框架，支持：
- 多目标输出（文件 + 控制台）
- 环境变量配置日志级别（`RUST_LOG=debug`）
- 结构化日志

## 历史开发进度（已失效）

- [x] S0.1 技术验证（invoice-parse）
- [x] S0.2 Tauri 骨架
- [x] S0.3 核心数据模型
- [x] S0.4 加密存储（invoice-store）
- [x] S0.5 批次状态机
- [x] **S0.6 批次 CRUD UI** ← 当前
- [x] A 采集模块（invoice-collect）
- [x] B 解析模块（invoice-parse）
- [x] C 归组引擎（invoice-grouping）
- [ ] S0.7 发票添加流程
- [ ] G1 校验去重
- [ ] G2 审核界面
- [ ] H1 流水线集成

以上列表仅用于历史追溯。当前状态和阻断项见：

- `specs/mvp-release-baseline/tasks.md`
- `docs/release/mvp-gap-audit-2026-08-19.md`
- `docs/release/defect-and-release-gates.md`

## 许可证与发布

当前仓库未确认对外软件许可证；不得沿用旧文档中的 MIT 声明对外发布。
公开 Beta 和正式版还需要产品负责人提供签名主体、发布主体、版权与支持信息。
