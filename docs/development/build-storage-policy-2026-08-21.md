# Windows 构建磁盘占用评审与治理方案

日期：2026-08-21
范围：开发/测试工作区生成物，不涉及产品 `DataRoot`、用户原件、备份和输出证据。

## 1. 评审结论

原 `AGENTS.md` 将 `target/debug > 20 GiB` 作为清理条件，并要求使用
`cargo clean --profile dev` 保留 release 产物，方向合理；但它只是人工提示，且根
`Cargo.toml` 实际没有对应 profile 配置，Windows 构建脚本也没有容量门禁，因而不能
阻止重复编译持续占满磁盘。

2026-08-21 清理前实测：

| 目录/文件 | 大小 | 结论 |
| --- | ---: | --- |
| `target` | 69.23 GiB | 不合理，必须治理 |
| `target/debug` | 62.29 GiB | 超过 20 GiB 条件 3.1 倍 |
| `target/debug/incremental` | 28.37 GiB | 可完全重建，是主要增长源之一 |
| `target/debug/deps` | 30.14 GiB | 多轮全工作区/不同编译配置残留 |
| `target/release` | 6.94 GiB | 对当前 Tauri/Rust 工作区可接受，但需 12 GiB 上限 |
| `artifacts` | 0.94 GiB | 当前可接受；6 份历史候选需要上限治理 |
| `ui/node_modules` | 91.43 MiB | 合理 |
| `.tmp` + `tmp` | 小于 1 MiB | 合理 |
| 最终 portable ZIP | 51.63 MiB | 产品交付大小合理 |
| `invoice-assistant.exe` | 45.80 MiB | 合理 |
| `invoice-ocr-worker.exe` | 5.69 MiB | 合理 |

当时 C 盘总容量约 293 GiB，仅剩约 22.15 GiB；继续全量构建存在耗尽磁盘风险。
按 `AGENTS.md` 先预览再执行 `cargo clean --profile dev` 后，`target/debug` 降至
1.41 GiB，`target` 降至 7.87 GiB，C 盘可用空间约 74.97 GiB；release EXE、worker、
portable ZIP 及其 SHA-256 均保持不变。

## 2. 容量预算

| 项目 | 上限/保留线 | 处理方式 |
| --- | ---: | --- |
| `target/debug` | 20 GiB | 构建前后触发 Cargo dev profile 清理 |
| `target/release` | 12 GiB | 硬阻断；release 只在明确重建时清理 |
| `target` 总计 | 32 GiB | 硬阻断 |
| `artifacts` | 2 GiB | 硬阻断；显式预览后修剪历史候选 |
| `ui/node_modules` | 2 GiB | 硬阻断，排查异常依赖树 |
| `.tmp` + `tmp` | 2 GiB | 硬阻断，按任务生命周期清理 |
| 构建前磁盘余量 | 20 GiB | 不足则不开始全量验证/发布构建 |
| 构建后磁盘余量 | 8 GiB | 不足则发布门禁失败 |

20 GiB debug 预算允许一次完整 workspace 的 check/clippy/test 和 OCR 进程测试；32 GiB
总预算同时容纳约 12 GiB release 与 20 GiB debug。构建前 20 GiB、构建后 8 GiB
保留线避免编译器、链接器或打包压缩在峰值阶段耗尽系统盘。

## 3. 已实施措施

1. `Cargo.toml` 的 dev/test profile 使用 `debug = "line-tables-only"`，保留回溯文件名和
   行号；关闭 incremental，避免长期缓存增长。
2. `scripts/check-build-storage.ps1` 只遍历项目内已知生成目录，不跟随重解析点；支持
   Audit、Preflight、Postflight 和 JSON 证据。
3. 超过 debug 上限时，仅在未运行 Cargo、rustc、Rust Analyzer、应用或 OCR worker
   的情况下执行官方 `cargo clean --profile dev`；不会清理 release。
4. `verify-windows.ps1` 和 `build-portable.ps1` 均在开始和结束时执行门禁。
5. `scripts/prune-build-archives.ps1` 默认只预览，`-Apply` 才删除
   `artifacts/archive` 中除最新三份以外的历史候选；不会触碰当前候选、证据、`output`
   或用户数据。
6. `output` 和产品 `DataRoot` 明确排除在自动构建清理之外。

## 4. 操作命令

```powershell
# 只读审计
.\scripts\check-build-storage.ps1 -Mode Audit

# 构建前检查；debug 超限时仅清理 dev profile
.\scripts\check-build-storage.ps1 -Mode Preflight -AutoCleanDev

# 预览历史候选修剪
.\scripts\prune-build-archives.ps1

# 人工核对路径后执行
.\scripts\prune-build-archives.ps1 -Apply
```

禁止用通配符或手工选择性删除 `target/debug/deps`、`build`、`incremental` 内文件。
release 超限时先保留当前 portable ZIP、哈希和验证证据，再明确安排完整 release 重建。
