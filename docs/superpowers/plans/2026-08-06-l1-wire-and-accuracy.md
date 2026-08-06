# 计划：L1 接入评分 + 准确率提升

**日期：** 2026-08-06  
**基准提交：** e31a67f  
**当前通过率：** 15.6%（10/64）——数字低估真实能力，原因见下  
**目标通过率：** ≥40%（18 个已标注样本全部通过后的预估值）

---

## 背景与问题诊断

上一轮（parse-accuracy 计划，Tasks 1–8）完成了：

- L0/L1 坐标文本框提取（Tasks 4、5）
- 修复空期望值语义（Task 1）
- 18 个样本标注（Task 3）

**但 verify-all 分数未变（仍 15.6%）的原因不是"管道稳定"，而是 L1 代码根本没有接入评分路径：**

`main.rs:333` 的 `pdf-vat` 路由仍指向 `pdf::parse_vat_invoice_text`（flat text），  
`pdf_text::parse_vat_invoice_from_boxes` 仅在 `dump-pdf-boxes` 命令下可达。

此外，已发现一处字段定位缺陷：`locate_vat_fields` 要求标签与值在同一行基线，  
但部分电子发票将金额打印在标签 **下方一行**（`价税合计（大写）` / `（小写）¥15.00`），  
导致 `total_amount` 报"缺失"，实际上值就在文本框里。

---

## 全局约束

- 工作目录：`/home/holo/work-tools`
- cargo 路径：`$HOME/.cargo/bin/cargo`，若报 not found 先执行 `export PATH="$HOME/.cargo/bin:$PATH"`
- 测试命令：`cargo test -p invoice-parse`（当前 64 个测试全部通过，不允许退步）
- 每个 Task 完成后必须提交，并在报告中说明新的 verify-all 数字

---

## 文件结构

| 文件 | 职责 |
|---|---|
| `crates/invoice-parse/src/main.rs` | verify-all 路由、子命令入口 |
| `crates/invoice-parse/src/ocr.rs` | `locate_vat_fields`：标签→值的空间配对逻辑 |
| `crates/invoice-parse/src/pdf_text.rs` | PDF 坐标文本框提取与解析 |
| `fixtures/manifest.toml` | 样本注释与期望值 |
| `docs/parse-accuracy-final-report.md` | 准确率报告（当前数字有误，需修正） |

---

## Task 1：将 L1 坐标文本框路径接入 verify-all

**文件：**
- 修改：`crates/invoice-parse/src/main.rs`
- 修改：`crates/invoice-parse/src/pdf_text.rs`（若需要调整公开接口）

**关键认识：**  
verify-all 里 `pdf-vat` 走 flat-text 解析，Task 5 写的坐标路径从未被评分覆盖。  
只需在 `pdf-vat` 分支中先尝试坐标路径，失败则降级 flat-text，即可立即测量真实能力。

- [ ] **Step 1：写失败测试**

在 `pdf_text.rs` 的 `mod tests` 中追加一个测试，验证 `parse_vat_invoice_from_boxes`  
在给定的真实样本字节上能返回 `Ok(invoice)` 且发票号非空：

```rust
#[test]
fn real_sample_parses_invoice_number() {
    let bytes = std::fs::read("../../fixtures/samples/04-unknown-6554429d.pdf")
        .expect("样本文件不存在");
    let path = std::path::Path::new("fixtures/samples/04-unknown-6554429d.pdf");
    let invoice = parse_vat_invoice_from_boxes(&bytes, path).unwrap();
    assert!(!invoice.invoice_number.is_empty());
}
```

运行 `cargo test -p invoice-parse`，确认新测试失败（接口签名或调用路径不通）。

- [ ] **Step 2：确认 pdf_text 接口可被 main.rs 调用**

检查 `parse_vat_invoice_from_boxes` 的签名：

```rust
pub fn parse_vat_invoice_from_boxes(
    bytes: &[u8],
    path: &Path,
) -> Result<ParsedInvoice, ParseError>
```

若返回 `anyhow::Error` 或其他类型，统一改为 `Result<ParsedInvoice, ParseError>`，  
与 `parse_invoice_pdf` 保持一致，便于在 `catch_unwind` 闭包内统一处理。

- [ ] **Step 3：修改 verify-all 路由**

定位 `main.rs` 中 `verify-all` 的 `pdf-vat` 分支（约第 333 行），改为：

```rust
"pdf-vat" => {
    // 先尝试 L1 坐标文本框路径，失败降级 flat-text
    let bytes = std::fs::read(&full_path)
        .map_err(anyhow::Error::from)?;
    invoice_parse::pdf_text::parse_vat_invoice_from_boxes(&bytes, &full_path)
        .map_err(anyhow::Error::from)
        .or_else(|_| {
            invoice_parse::pdf::parse_vat_invoice_text(&bytes, &full_path)
                .map_err(anyhow::Error::from)
        })
},
```

注意：原 `parse_pdf_with` 辅助函数先读文件再调用解析器，坐标路径同样需要字节，  
可复用同一块 `std::fs::read`，不要读两次。

- [ ] **Step 4：运行测试**

```bash
cargo test -p invoice-parse
```

Step 1 的新测试应变为通过。其余 64 个测试不允许退步。

- [ ] **Step 5：运行 verify-all，记录新基线**

```bash
cargo run -p invoice-parse -- verify-all
```

记录新的格式通过率表格（pdf-vat 应有明显提升）。将结果写入本步骤末尾的注释。

- [ ] **Step 6：提交**

```
feat: 将 L1 坐标文本框路径接入 verify-all，建立真实基线
```

---

## Task 2：修复多行金额配对（大写行 + 小写行布局）

**文件：**
- 修改：`crates/invoice-parse/src/ocr.rs`

**关键认识：**  
部分电子发票布局：

```
价税合计（大写）    壹拾伍圆整 ¥15.00
（小写）
```

`locate_vat_fields` 在搜索 `total_amount` 时，找到 `价税合计` / `（小写）` 标签，  
但值（`¥15.00`）不在标签同行，而在标签右侧的同一框内（`壹拾伍圆整 ¥15.00`），  
或者确实在下方独立行。需要处理以下两种情形：

1. **同框混合**：`"壹拾伍圆整 ¥15.00"` → 从末尾提取阿拉伯数字部分
2. **跨行**：标签在 y=N，值在 y=N+Δ（Δ ≤ 20px），需向下搜索相邻行

- [ ] **Step 1：写失败测试**

在 `ocr.rs` 的 `mod tests` 中追加：

```rust
#[test]
fn extracts_amount_from_chinese_uppercase_mixed_box() {
    // "壹拾伍圆整 ¥15.00" 格式：中文大写 + 阿拉伯金额在同一文本框
    let boxes = vec![
        tb("价税合计（大写）", 50.0, 200.0, 1.0),
        tb("壹拾伍圆整 ¥15.00", 250.0, 200.0, 1.0),
        tb("发票号码：24312000000012345678", 400.0, 20.0, 1.0),
        tb("开票日期：2026-06-08", 400.0, 40.0, 1.0),
    ];
    let invoice = locate_vat_fields(&boxes, Path::new("test.pdf"), ParseLevel::L1).unwrap();
    assert_eq!(invoice.total_amount, Decimal::from_str("15.00").unwrap());
}

#[test]
fn extracts_amount_from_next_line_when_same_line_empty() {
    // 标签行无值，值在下方一行（y 差 ≤ 20px）
    let boxes = vec![
        tb("价税合计（大写）", 50.0, 200.0, 1.0),
        tb("（小写）", 50.0, 215.0, 1.0),
        tb("¥15.00", 120.0, 215.0, 1.0),
        tb("发票号码：24312000000012345678", 400.0, 20.0, 1.0),
        tb("开票日期：2026-06-08", 400.0, 40.0, 1.0),
    ];
    let invoice = locate_vat_fields(&boxes, Path::new("test.pdf"), ParseLevel::L1).unwrap();
    assert_eq!(invoice.total_amount, Decimal::from_str("15.00").unwrap());
}
```

运行测试，确认两个新测试失败。

- [ ] **Step 2：扩展 `find_value_for_label` 或 `locate_vat_fields` 内 amount 分支**

当 `total_amount` 候选标签的右侧同行未找到纯数字时，按此顺序降级：

1. 检查标签同框文本是否尾部含 `¥N.NN` 或 `￥N.NN` 格式，提取阿拉伯部分
2. 在 `boxes` 中搜索 y 坐标在标签 `[y, y+20]` 范围内的所有框，  
   找第一个文本匹配 `¥/￥ + 数字` 或纯 `数字.数字` 格式的框

修改范围：`locate_vat_fields` 内 `total_amount` 提取逻辑，不影响其他字段。

- [ ] **Step 3：运行测试**

```bash
cargo test -p invoice-parse
```

两个新测试应通过，其余测试不退步。

- [ ] **Step 4：验证三个样本**

```bash
cargo run -p invoice-parse -- dump-pdf-boxes fixtures/samples/27-unknown-08cfe721.pdf
cargo run -p invoice-parse -- dump-pdf-boxes fixtures/samples/31-unknown-4812770d.pdf
cargo run -p invoice-parse -- dump-pdf-boxes fixtures/samples/09-meituan-2f9595e6.pdf
```

三个样本的 `total_amount` 应从"缺失"变为正确值。

- [ ] **Step 5：运行 verify-all，记录提升**

```bash
cargo run -p invoice-parse -- verify-all
```

记录 pdf-vat 的新通过率，与 Task 1 基线对比。

- [ ] **Step 6：提交**

```
fix: 修复多行金额配对，支持大写行+小写行布局
```

---

## Task 3：修正最终报告数字

**文件：**
- 修改：`docs/parse-accuracy-final-report.md`

- [ ] **Step 1：更新报告**

用 Task 2 完成后的 verify-all 输出替换报告中的以下内容：

1. **执行摘要** 的总体通过率和格式分类表
2. **各格式详细结果** 中 PDF-VAT 和总计行
3. **与 Plan1 Task 7.5 的比较表**——改为与本次基线（Task 1 数字）的对比

在报告头部增加一段说明：

```markdown
> **修正说明（2026-08-06）**：原报告（基于 commit e31a67f）中 L1 坐标文本框路径
> 未接入 verify-all 评分，导致 pdf-vat 通过率低估。Tasks 1–2 完成接入后，
> 本报告数字已更新为真实基线。
```

- [ ] **Step 2：提交**

```
docs: 修正最终报告，L1 路径已接入评分，数字更新
```

---

## Task 4：扩大样本标注覆盖率

**文件：**
- 修改：`fixtures/manifest.toml`

**关键认识：**  
当前 18/64 标注（28%），剩余 46 个全为 Unverified。  
其中 24 个 DiDi 行程单——这是**范围决策**，不是技术问题：  
若纳入验收，需要专用解析器；若排除，应在 manifest 中标记 `scope = "excluded"`  
避免污染通过率分母。

- [ ] **Step 1：决策 DiDi 行程单范围**

逐一检查 `fixtures/samples/` 中带 `-didi-` 前缀的 PDF 样本（共 24 个）。  
确认它们均为行程报销单（非增值税发票），在 manifest 中为每条记录添加：

```toml
scope = "excluded"
exclude_reason = "DiDi行程报销单，非增值税发票格式，不在本期验收范围"
```

更新 verify-all 逻辑，跳过 `scope = "excluded"` 的样本（不计入分母）。

- [ ] **Step 2：标注 PDF-VAT 样本**

从 Task 2 验证通过的样本和已知能成功提取的样本入手，优先标注以下字段：
- `invoice_number`、`issue_date`、`total_amount`（核心三字段）
- `tax_amount`、`buyer_name`、`seller_name`（可选，有把握时才填）

目标：PDF-VAT 标注数从 11 提升到 22+。

使用 `cargo run -p invoice-parse -- dump-pdf-boxes <file>` 确认提取值后再填入 manifest。

- [ ] **Step 3：标注 OFD 样本**

运行 `cargo run -p invoice-parse -- dump-ofd <file>` 检查 6 个失败的 OFD 样本，  
确认是"无内嵌 XML"还是"坐标提取有值但字段定位失败"。  
对能提取出字段的样本，填入期望值；对纯布局文件，标记 `parse_note`。

目标：OFD 标注数从 0 提升到 4+。

- [ ] **Step 4：运行 verify-all，验证标注正确**

```bash
cargo run -p invoice-parse -- verify-all
```

新标注的样本不应出现意外的 Mismatch（若有，检查期望值是否填错）。

- [ ] **Step 5：提交**

```
feat: 扩大样本标注覆盖率，排除 DiDi 行程单，标注 22+ 个 PDF-VAT 和 4+ 个 OFD
```

---

## Task 5（待决策）：集成 Python sidecar OCR

**依赖：** Task 1、Task 2 完成后再评估

**背景：**
- 6 个图片样本（image 格式）当前 0/6，OCR 路径未接入
- `tools/ocr_sidecar.py` 已存在，`ocr::recognize_via_sidecar()` 接口已实现
- native Rust OCR（ort + PP-OCRv6）因 `ort = "2.0.0-rc.13"` API 不稳定暂阻塞

**前置条件（需用户确认）：**
1. 是否接受 ~50MB PaddleOCR 模型作为运行依赖？
2. Python 环境要求是否可接受（需要 paddleocr、paddlepaddle）？
3. 还是等 ort 2.0 stable 后做纯 Rust 路径？

**若决定集成：**

- [ ] **Step 1：验证 sidecar 环境**

```bash
pip install paddlepaddle paddleocr
python tools/ocr_sidecar.py fixtures/test-images/pdf-01.png
```

确认输出格式与 `recognize_via_sidecar` 期望的 JSON 格式一致。

- [ ] **Step 2：接入 verify-all**

在 `main.rs` verify-all 的 `image` 分支替换"未集成"错误：

```rust
"image" => {
    let boxes = invoice_parse::ocr::recognize_via_sidecar(&full_path)
        .map_err(anyhow::Error::from)?;
    invoice_parse::ocr::locate_vat_fields(
        &boxes,
        &full_path,
        invoice_parse::model::ParseLevel::L2,
    )
    .map_err(anyhow::Error::from)
},
```

- [ ] **Step 3：验证 6 个图片样本并标注**

运行后手工对照发票图片填入 manifest.toml 期望值，再次运行 verify-all。

- [ ] **Step 4：提交**

```
feat: 集成 Python sidecar OCR，image 格式接入 verify-all
```

---

## 预期结果

| 完成 Task | 预期 pdf-vat 通过率 | 预期总通过率 |
|---|---|---|
| Task 1（接入 L1 路径） | 4–11/41（10–27%）| 估计 12–20/64（19–31%）|
| Task 2（修复多行金额） | 11–18/41（27–44%）| 估计 18–28/64（28–44%）|
| Task 4（DiDi 排除 + 标注扩大） | 同上（分母缩小）| 估计 ≥40% |

（以上为估算，实际运行 verify-all 以准确数字为准。）
