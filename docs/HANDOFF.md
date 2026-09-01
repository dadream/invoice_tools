> **历史交接记录，禁止作为当前执行依据。** 当前任务与验收入口为 `specs/mvp-release-baseline/tasks.md`；当前候选事实见 `artifacts/final-internal-alpha-candidate.validation.json`，发布阻断见 `docs/release/open-defects.md`，Windows/QQ 证据见 `docs/release/windows-validation-2026-08-19.md`。下文“尚未写任何代码”等内容仅保留历史，不代表 2026-08-24 状态。

# 交接说明

上下文即将清空。本文件记录恢复工作所需的全部信息。

## 文件清单

| 文件 | 行数 | 内容 |
|---|---|---|
| `invoice-reimbursement-product-spec.md` | 1095 | 产品方案。10 轮讨论的结论，含论证 |
| `invoice-reimbursement-pricing.md` | 615 | 计费方案 |
| `invoice-reimbursement-dev-plan.md` | 533 | 开发计划总览。按依赖分五层，不含时间估算 |
| `docs/superpowers/plans/2026-08-03-invoice-email-collector.md` | ~2150 | **计划 0**：邮箱采集器（7 任务，TDD） |
| `docs/superpowers/plans/2026-08-03-invoice-parse-spike.md` | 3045 | **计划 1**：解析能力验证（9 任务，TDD） |

## 执行状态

**尚未写任何代码。** 用户已选定 subagent 驱动执行（每任务派新 subagent，任务间评审）。

**执行顺序**：计划 0 → 计划 1。计划 1 的 Task 4–9 阻塞在"没有真实发票样本"上，而计划 0 的 Task 7 正是产出样本的那一步。

计划 0 的 Task 1、3、4、5、6 不需要凭证也不需要联网，可立即开始。

## 两个待解决的阻塞项

### 1. QQ 邮箱授权码

用户提供了一个 QQ 邮箱账号和登录密码。**账号登录密码无法用于 IMAP** —— QQ 邮箱要求 16 位授权码（设置 → 账户 → POP3/IMAP/SMTP 服务）。

**那个登录密码曾在对话中出现过，需要轮换。** 具体值不记录在此。

计划只从环境变量 `INVOICE_IMAP_PASSWORD` 读取凭证，`ImapConfig` 手工实现 `Debug` 输出 `<redacted>`，并有测试锁住这一点。邮箱地址通过命令行参数传入，不硬编码。

拿到授权码后：

```bash
read -rs INVOICE_IMAP_PASSWORD && export INVOICE_IMAP_PASSWORD
cargo run -p invoice-collect -- probe <邮箱地址>
```

用 `read -rs` 而非 `export VAR='值'`，避免授权码进入 shell history。之后可执行计划 0 的 Task 2 和 7。

### 2. 图片样本采不到

`image` 格式（纸票扫描/拍照）不来自邮箱，采集器必然采不到。但它是 L2 OCR 的验证依据，而 L2 是计划 1 里技术风险最高的一环，需要 10 张（扫描件 + 手机照片各半）。

必须人工把纸质发票拍照/扫描放进 `fixtures/samples/`，并手工追加清单条目。计划 0 的 Task 7 Step 3 有四种缺口的对照表。

## 关键架构决定

**纯 Rust 优先，Python sidecar 兜底。** 产品方案原定的解析栈全是 Python（pdfplumber / PaddleOCR / lxml），但 Tauri 后端是 Rust。走 sidecar 会让包体到 300MB+，而选 Tauri 的理由就是 10MB vs Electron 的 120MB —— 那样当初该选 Electron。

已查证四项能力都有 Rust 方案：

| 能力 | 方案 |
|---|---|
| 本地 OCR | `paddle-ocr-rs` / `ocr-rs`（跑同一批 PaddleOCR ONNX 模型） |
| SM2/SM3 验签 | `smcrypto` / `gm-rs` |
| PDF 文本层 | `pdf-extract` / `pdfplumber-rs` |
| OFD 解析 | 自行实现（ZIP+XML，只需定位内嵌发票 XML，不渲染版式） |

**但这些 crate 大多很新，未经中国发票场景验证。** 计划 1 的 Task 1（S0.1）就是用真实样本验证，验收标准是"能处理样本"而非"能编译"。若 OCR 不达标，只让 OCR 走 sidecar。

## 两个计划的接缝

**Workspace 归属**：计划 0 创建 workspace root 和 `crates/invoice-collect`。计划 1 的 Task 1 只能**追加** `crates/invoice-parse` 到 `members`，不要重建 root。

**`format` 字段取值必须一致**，已核对两边完全匹配：

```
xml · ofd · pdf-rail · pdf-flight · pdf-vat · image
```

计划 0 的 `SampleFormat::as_manifest_str()` 产出这些值，计划 1 的 `verify-all` 按这些值分派解析器。

## 全局纪律（两个计划都适用）

- **金额一律 `rust_decimal::Decimal`，禁止 `f64`**。浮点在求和对账时产生分位误差，而金额对账是防"静默的错"的核心防线
- **核心数据模型不出现任何 Concur 概念**（无 `expense_type` / `report_header` / `itemization`）
- **文档、代码、注释、测试里不出现任何真实凭证或真实账号**。测试用明显是假的占位值（如 `test-user@qq.com`）；真实值只经环境变量和命令行参数传入
- 归组引擎与 UI 解耦，输入发票列表输出行程
- `fixtures/samples/` 必须 gitignore —— 个人财务数据

## 讨论中踩过的三个陷阱

这三处都曾算错并修正过，实现时容易重犯：

**1. 计费公式方向**

```
实收 = max(¥3, min(张数 × ¥0.20, 报销总额 × 0.002))
```

先 `min`（两种计费取对用户便宜的），再 `max`（¥3 保底）。曾错成外层 `max`，导致定价和订阅价格连带算错。把 `invoice-reimbursement-pricing.md` §7 速查表全部作为表驱动测试用例。典型场景：50 张 / ¥8,000 报销额 → 收 ¥10。

**2. Concur 附件名不含批次序号**

序号只在单批次内唯一，但 Available Receipts 是全局库，多次报销会出现重复的 `001_` 前缀。命名用 `{日期}_{类型}_{金额}_{发票号后6位}`。

**3. 空串 ≠ 缺失**

`Option<String>` 字段填空串会反序列化成 `Some("")`，比对时喂给 `Decimal::from_str` 解析失败，每个空字段报一条假的不匹配。清单里可选字段以**注释行**输出，有值才取消注释。计划 0 Task 6 有测试锁住这个行为。

## 计划里的一处已知空缺

计划 1 的 Task 7 Step 6，`OcrEngine::new` 和 `recognize` 是 `todo!()`。函数体取决于哪个 crate 能编译并跑通，无法预先写出。Step 6 给了三个候选 crate 的尝试顺序和 `TextBox` 映射契约，Step 7 的验收标准能查出填错。

Task 7 拆成两半是刻意的：Step 1–5 的字段定位是纯函数，用合成 `TextBox` 数据测，与引擎无关。即使引擎换成 sidecar，这段逻辑照样用。

## 一处刻意的任务顺序

计划 1 的 Task 3 必须在 Task 4 之前。Task 3 是个 `dump-tags` 探查工具，用来看真实数电票 XML 的实际元素名 —— 不同开票平台元素名不同且无公开 schema，凭猜测写标签名的解析器只会在碰巧测过的那个平台上通过。Task 4 的解析器从清单读候选标签名。

## 产品方案里两个未核实的数字

**TAM 待核实**：文档里"200 万高频差旅员工"是整体差旅人群，不是 Concur 中国用户数，两者差一到两个数量级。这个数直接影响收入模型可行性。

**竞品结论置信度低**：搜索没找到直接竞品，但微信小程序对外部搜索基本不可见。最强的验证信号是 β 招募时问一句"你现在用什么工具整理发票"。

## 恢复步骤

1. 读 `docs/superpowers/plans/2026-08-03-invoice-email-collector.md`
2. 用 superpowers:subagent-driven-development，派 subagent 执行 Task 1
3. 评审后继续 Task 3、4、5、6（都不需要凭证）
4. 拿到授权码后执行 Task 2（决策关口：确认 6 月有无发票邮件）和 Task 7
5. 样本齐备后转入计划 1
