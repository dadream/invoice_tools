# Invoice Assistant

发票报销 AI 助手 - 把散落在邮箱里的发票，变成按行程归好、查过重、排好版的批次。

## 项目结构

```
.
├── crates/              # Rust 核心库
│   ├── invoice-parse    # 多格式发票解析
│   ├── invoice-collect  # IMAP 邮箱采集
│   ├── invoice-grouping # 行程归组引擎
│   └── invoice-store    # 加密存储系统
├── src-tauri/          # Tauri 后端
│   └── src/            # Rust 应用代码
└── ui/                 # Svelte 前端
    └── src/            # UI 代码
```

## 开发环境要求

- Rust 1.97+
- Node.js 18+
- Tauri CLI 2.0+

## 快速开始

### 安装依赖

```bash
# 安装 Tauri CLI
cargo install tauri-cli --version "^2.0"

# 安装前端依赖
cd ui
npm install
cd ..
```

### 开发模式

```bash
# 方式 1: 使用脚本
./scripts/dev-tauri.sh

# 方式 2: 手动启动
cd src-tauri
cargo tauri dev
```

这将启动：
- Vite 开发服务器（http://localhost:5173）
- Tauri 应用窗口（热重载）

### 构建发布版

```bash
# 方式 1: 使用脚本
./scripts/build-tauri.sh

# 方式 2: 手动构建
cd src-tauri
cargo tauri build
```

构建产物：
- **Windows**: `src-tauri/target/release/invoice-assistant.exe`
- **macOS**: `src-tauri/target/release/bundle/macos/Invoice Assistant.app`
- **Linux**: `src-tauri/target/release/invoice-assistant`

## 测试

```bash
# 测试所有 Rust crates
cargo test --workspace

# 测试特定模块
cargo test -p invoice-parse
cargo test -p invoice-store

# 前端测试（待添加）
cd ui
npm run test
```

## 日志

应用日志存储在：
- **Linux/macOS**: `~/.invoice-assistant/logs/app.log`
- **Windows**: `%USERPROFILE%\.invoice-assistant\logs\app.log`

开发模式下同时输出到终端。

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

## 开发进度

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

详见：`docs/next-steps-roadmap.md`

## 许可证

MIT License
