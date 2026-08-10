# S0.2 Tauri 骨架实施计划

**目标**: 建立 Tauri v2 应用骨架和构建流水线

**当前状态**: 需要安装 Tauri CLI

---

## 前置准备

由于 Tauri CLI 安装需要较长时间，我将提供两种方案：

### 方案 A: 使用 Tauri CLI 初始化（推荐）

需要用户手动执行以下命令：

```bash
# 1. 安装 Tauri CLI（如果尚未安装）
cargo install tauri-cli --version "^2.0"

# 2. 创建新项目
cd /home/holo/work-tools
cargo tauri init

# 3. 配置选项：
# - App name: Invoice Assistant
# - Window title: 发票报销助手
# - Web assets: ../ui/dist
# - Dev server: http://localhost:5173
# - Frontend framework: Svelte (或 React)
```

### 方案 B: 手动创建项目结构（当前可行）

我可以立即创建完整的项目结构，包括：
1. Tauri 后端 crate (src-tauri/)
2. 前端项目 (ui/)
3. 配置文件
4. IPC 通道约定
5. 日志系统

---

## 建议行动

**推荐**: 方案 B - 手动创建结构

原因：
- ✅ 可以立即开始，不依赖 CLI 安装
- ✅ 完全可控的项目结构
- ✅ 符合现有 workspace 布局
- ✅ 可以直接集成现有 crates

---

## 下一步

等待用户确认：
1. 使用方案 A（需要手动运行 `cargo tauri init`）
2. 使用方案 B（我立即手动创建项目结构）
