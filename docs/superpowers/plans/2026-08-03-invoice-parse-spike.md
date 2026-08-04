# 发票解析能力技术验证 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 用真实中国发票样本验证 Rust 生态能否胜任解析与 OCR，产出一份决定"纯 Rust vs Python sidecar"的验证报告，同时留下可复用的回归测试集。

**Architecture:** 一个独立的 Rust workspace member `invoice-parse`，作为 CLI 工具运行。它读取一份 TOML 清单（声明每个样本文件的期望字段值），逐个解析并与期望值比对，输出通过率报告。清单驱动的设计让样本可以增量添加，且这套测试集在验证结束后直接成为解析模块的回归测试集——不是一次性工作。

**Tech Stack:** Rust 2021 · `quick-xml`（XML）· `zip`（OFD 容器）· `rust_decimal`（金额）· `pdf-extract`（PDF 文本层）· `smcrypto`（SM2/SM3 验签）· `ort` + PaddleOCR ONNX 模型（OCR）· `toml` + `serde`（清单）

## Global Constraints

- Rust edition 2021，MSRV 1.75+
- **金额一律用 `rust_decimal::Decimal`，禁止 `f64`**。浮点会在求和对账时产生分位误差，而金额对账是防"静默的错"的核心防线
- 所有字段名、结构体名用英文；面向用户的输出文案用中文
- **不联网**：本计划所有解析在本地完成，不调用任何云 API
- 样本文件属于个人财务数据，**必须 gitignore**，仓库只提交清单结构与测试代码
- 每个任务结束时 commit，commit message 用英文，格式 `feat:` / `test:` / `chore:`

---

## 前置阻塞项

**在 Task 1 之前必须完成**：收集真实发票样本。没有样本，本计划无法执行。

| 格式 | 数量 | 来源 |
|---|---|---|
| 数电票 XML | 5 | 各地税务局电子发票平台、携程/滴滴开票 |
| OFD | 5 | 同上，含专票与普票 |
| PDF 铁路电子客票行程单 | 3 | 12306 |
| PDF 航空运输电子客票行程单 | 3 | 航司/携程 |
| PDF 增值税发票 | 3 | 酒店、餐饮 |
| 图片格式增值税票（扫描件） | 5 | 纸票扫描 |
| 图片格式增值税票（手机照片） | 5 | 纸票拍照 |
| 已作废/红冲发票（任意格式） | 1+ | **较难获得，拿不到则标注为覆盖缺口** |

样本放在 `fixtures/samples/`，该目录已在 `.gitignore` 中。

---

## File Structure

```
work-tools/
├── Cargo.toml                          # workspace root
├── .gitignore                          # 排除 fixtures/samples/
├── crates/
│   └── invoice-parse/
│       ├── Cargo.toml
│       └── src/
│           ├── lib.rs                  # 公开 API 与模块声明
│           ├── main.rs                  # CLI 入口
│           ├── model.rs                 # ParsedInvoice、ParseLevel、ParseError
│           ├── manifest.rs              # 清单加载与期望值比对
│           ├── xml.rs                   # 数电票 XML 解析（L0）
│           ├── ofd.rs                   # OFD 容器解析（L0）
│           ├── pdf.rs                   # PDF 文本层解析（L1）
│           ├── ocr.rs                   # 本地 OCR（L2）
│           ├── verify.rs                # SM2/SM3 验签
│           └── report.rs                # 验证报告生成
├── fixtures/
│   ├── manifest.toml                   # 样本期望值声明（提交）
│   └── samples/                        # 样本文件（不提交）
└── docs/superpowers/plans/
```

**职责边界**：

- `model.rs` 只有类型定义，无逻辑。所有解析器都产出 `ParsedInvoice`，这是本 crate 唯一的输出契约
- 四个解析器（`xml`/`ofd`/`pdf`/`ocr`）互不依赖，各自独立可测
- `manifest.rs` 不知道任何解析器的存在，它只做「加载期望值」和「比对实际值」
- `report.rs` 只格式化输出，不做判断

这个划分让每个解析器可以单独开发和验证——某一项失败不阻塞其余。

---

## Task 1: Workspace 骨架与清单结构

**Files:**
- Create: `Cargo.toml`
- Create: `.gitignore`
- Create: `crates/invoice-parse/Cargo.toml`
- Create: `crates/invoice-parse/src/lib.rs`
- Create: `crates/invoice-parse/src/model.rs`
- Create: `fixtures/manifest.toml`
- Test: `crates/invoice-parse/src/model.rs`（inline `#[cfg(test)]`）

**Interfaces:**
- Consumes: 无（首个任务）
- Produces: `ParsedInvoice`、`TicketType`、`ParseLevel`、`ParseError` 类型，供所有后续任务使用

- [ ] **Step 1: 创建 workspace root**

创建 `Cargo.toml`：

```toml
[workspace]
members = ["crates/invoice-parse"]
resolver = "2"

[workspace.package]
edition = "2021"
rust-version = "1.75"

[workspace.dependencies]
rust_decimal = { version = "1.36", features = ["serde"] }
serde = { version = "1.0", features = ["derive"] }
toml = "0.8"
quick-xml = { version = "0.36", features = ["serialize"] }
zip = "2.2"
chrono = { version = "0.4", features = ["serde"] }
thiserror = "2.0"
anyhow = "1.0"
```

- [ ] **Step 2: 创建 .gitignore**

创建 `.gitignore`：

```
/target
fixtures/samples/
*.pdf
*.ofd
*.xml
!fixtures/manifest.toml
```

第 3–5 行是双重保险：即使样本被放到别处，常见发票扩展名也不会误提交。

- [ ] **Step 3: 创建 crate 清单**

创建 `crates/invoice-parse/Cargo.toml`：

```toml
[package]
name = "invoice-parse"
version = "0.1.0"
edition.workspace = true
rust-version.workspace = true

[dependencies]
rust_decimal.workspace = true
serde.workspace = true
toml.workspace = true
quick-xml.workspace = true
zip.workspace = true
chrono.workspace = true
thiserror.workspace = true
anyhow.workspace = true

[[bin]]
name = "invoice-parse"
path = "src/main.rs"
```

- [ ] **Step 4: 写 model.rs 与失败测试**

创建 `crates/invoice-parse/src/model.rs`：

```rust
use chrono::NaiveDate;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// 票种。与产品方案的核心数据模型保持一致，不含任何报销系统概念。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TicketType {
    Rail,
    Flight,
    Hotel,
    CityTransport,
    Meal,
    Other,
}

/// 解析级别。决定字段可信度与是否需要人工介入。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ParseLevel {
    /// 结构化数据直读（数电票 XML、OFD 内嵌 XML）
    L0,
    /// PDF 文本层 + 版式模板
    L1,
    /// 本地 OCR
    L2,
    /// 关键字段冲突，强制人工
    L4,
}

/// 所有解析器的统一输出。这是本 crate 唯一的输出契约。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ParsedInvoice {
    pub invoice_number: String,
    pub issue_date: NaiveDate,
    pub total_amount: Decimal,
    pub tax_amount: Option<Decimal>,
    pub tax_rate: Option<Decimal>,
    pub buyer_name: Option<String>,
    pub seller_name: Option<String>,
    pub ticket_type: TicketType,
    pub parse_level: ParseLevel,
    /// 0.0–1.0。L0 恒为 1.0，L2 由 OCR 引擎给出。
    pub confidence: f32,
    pub source_path: PathBuf,
}

#[derive(Debug, thiserror::Error)]
pub enum ParseError {
    #[error("无法读取文件 {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("{path} 不是有效的 {format} 格式: {detail}")]
    MalformedFormat {
        path: PathBuf,
        format: &'static str,
        detail: String,
    },
    #[error("在 {path} 中找不到必需字段 {field}")]
    MissingField { path: PathBuf, field: String },
    #[error("字段 {field} 的值 {raw:?} 无法解析为 {expected_type}")]
    UnparseableValue {
        field: String,
        raw: String,
        expected_type: &'static str,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal::prelude::FromStr;

    #[test]
    fn parsed_invoice_roundtrips_through_json() {
        let invoice = ParsedInvoice {
            invoice_number: "24312000000012345678".to_string(),
            issue_date: NaiveDate::from_ymd_opt(2026, 7, 3).unwrap(),
            total_amount: Decimal::from_str("553.00").unwrap(),
            tax_amount: Some(Decimal::from_str("50.73").unwrap()),
            tax_rate: Some(Decimal::from_str("0.09").unwrap()),
            buyer_name: Some("某某公司".to_string()),
            seller_name: Some("中国铁路".to_string()),
            ticket_type: TicketType::Rail,
            parse_level: ParseLevel::L0,
            confidence: 1.0,
            source_path: PathBuf::from("fixtures/samples/rail-01.xml"),
        };

        let json = serde_json::to_string(&invoice).expect("序列化失败");
        let restored: ParsedInvoice = serde_json::from_str(&json).expect("反序列化失败");

        assert_eq!(invoice, restored);
    }

    #[test]
    fn decimal_amounts_sum_without_drift() {
        // 用 f64 时 0.1 + 0.2 != 0.3，Decimal 必须精确。
        // 这是金额对账能成立的前提。
        let a = Decimal::from_str("0.1").unwrap();
        let b = Decimal::from_str("0.2").unwrap();
        let c = Decimal::from_str("0.3").unwrap();
        assert_eq!(a + b, c);
    }
}
```

创建 `crates/invoice-parse/src/lib.rs`：

```rust
pub mod model;
```

`serde_json` 是测试专用依赖，加到 `crates/invoice-parse/Cargo.toml`：

```toml
[dev-dependencies]
serde_json = "1.0"
```

- [ ] **Step 5: 运行测试确认失败**

Run: `cargo test -p invoice-parse`

Expected: 编译失败，报 `main.rs` 不存在（`[[bin]]` 指向的文件还没建）。

- [ ] **Step 6: 建最小 main.rs 让编译通过**

创建 `crates/invoice-parse/src/main.rs`：

```rust
fn main() {
    println!("invoice-parse: 尚未实现，见 Task 2");
}
```

- [ ] **Step 7: 运行测试确认通过**

Run: `cargo test -p invoice-parse`

Expected: PASS，两个测试通过（`parsed_invoice_roundtrips_through_json`、`decimal_amounts_sum_without_drift`）。

- [ ] **Step 8: 创建清单骨架**

创建 `fixtures/manifest.toml`：

```toml
# 样本期望值声明。
#
# 每个 [[sample]] 描述一个样本文件及其正确字段值。
# 期望值由人工从发票上读出，是判定解析器对错的唯一依据。
#
# 金额一律写成字符串，避免 TOML 浮点精度损失。
# tag_candidates 用于数电票 XML：不同开票平台的标签名不同,
# 解析器按顺序尝试，命中即用。

[[sample]]
path = "samples/rail-01.xml"
format = "xml"
ticket_type = "Rail"
invoice_number = "24312000000012345678"
issue_date = "2026-07-03"
total_amount = "553.00"
tax_amount = "50.73"
tax_rate = "0.09"
buyer_name = "某某公司"
seller_name = "中国国家铁路集团有限公司"

# 以上是格式示例。收集到真实样本后，替换为实际值并按同样结构追加。
```

- [ ] **Step 9: Commit**

```bash
git init
git add Cargo.toml .gitignore crates/ fixtures/manifest.toml docs/
git commit -m "chore: scaffold invoice-parse workspace with core data model"
```

---

## Task 2: 清单加载与字段比对

**Files:**
- Create: `crates/invoice-parse/src/manifest.rs`
- Modify: `crates/invoice-parse/src/lib.rs`
- Test: `crates/invoice-parse/src/manifest.rs`（inline `#[cfg(test)]`）

**Interfaces:**
- Consumes: `model::{ParsedInvoice, TicketType}`（Task 1）
- Produces:
  - `Manifest::load(path: &Path) -> anyhow::Result<Manifest>`
  - `Manifest.samples: Vec<Sample>`
  - `Sample.compare(&self, actual: &ParsedInvoice) -> Vec<FieldComparison>`
  - `FieldComparison { field: &'static str, expected: String, actual: String, matched: bool }`
  - `Sample.xml_tag_hints: Option<TagHints>`（Task 3 用来定位 XML 元素）

- [ ] **Step 1: 写失败测试**

创建 `crates/invoice-parse/src/manifest.rs`，先只写测试与类型签名：

```rust
use crate::model::{ParseLevel, ParsedInvoice, TicketType};
use rust_decimal::Decimal;
use serde::Deserialize;
use std::path::{Path, PathBuf};

#[derive(Debug, Deserialize)]
pub struct Manifest {
    #[serde(default, rename = "sample")]
    pub samples: Vec<Sample>,
}

/// 一个样本的期望值声明。数值一律用 String 存，
/// 让清单保持人类可写，比对时再转成 Decimal/NaiveDate。
#[derive(Debug, Deserialize)]
pub struct Sample {
    pub path: PathBuf,
    pub format: String,
    pub ticket_type: TicketType,
    pub invoice_number: String,
    pub issue_date: String,
    pub total_amount: String,
    #[serde(default)]
    pub tax_amount: Option<String>,
    #[serde(default)]
    pub tax_rate: Option<String>,
    #[serde(default)]
    pub buyer_name: Option<String>,
    #[serde(default)]
    pub seller_name: Option<String>,
    /// 该样本是否为已作废/红冲票（验签负例）
    #[serde(default)]
    pub is_voided: bool,
    /// XML/OFD 元素名提示，由 Task 3 的探查工具填入
    #[serde(default)]
    pub xml_tag_hints: Option<TagHints>,
}

/// 不同开票平台的数电票 XML 元素名不统一，
/// 用这个结构声明每个字段的候选标签名（按优先级排列）。
#[derive(Debug, Clone, Deserialize)]
pub struct TagHints {
    #[serde(default)]
    pub invoice_number: Vec<String>,
    #[serde(default)]
    pub issue_date: Vec<String>,
    #[serde(default)]
    pub total_amount: Vec<String>,
    #[serde(default)]
    pub tax_amount: Vec<String>,
    #[serde(default)]
    pub tax_rate: Vec<String>,
    #[serde(default)]
    pub buyer_name: Vec<String>,
    #[serde(default)]
    pub seller_name: Vec<String>,
}

#[derive(Debug, PartialEq)]
pub struct FieldComparison {
    pub field: &'static str,
    pub expected: String,
    pub actual: String,
    pub matched: bool,
}

impl Manifest {
    pub fn load(path: &Path) -> anyhow::Result<Manifest> {
        unimplemented!()
    }
}

impl Sample {
    pub fn compare(&self, actual: &ParsedInvoice) -> Vec<FieldComparison> {
        unimplemented!()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;
    use rust_decimal::prelude::FromStr;

    fn sample_fixture() -> Sample {
        Sample {
            path: PathBuf::from("samples/rail-01.xml"),
            format: "xml".to_string(),
            ticket_type: TicketType::Rail,
            invoice_number: "24312000000012345678".to_string(),
            issue_date: "2026-07-03".to_string(),
            total_amount: "553.00".to_string(),
            tax_amount: Some("50.73".to_string()),
            tax_rate: Some("0.09".to_string()),
            buyer_name: Some("某某公司".to_string()),
            seller_name: None,
            is_voided: false,
            xml_tag_hints: None,
        }
    }

    fn parsed_fixture() -> ParsedInvoice {
        ParsedInvoice {
            invoice_number: "24312000000012345678".to_string(),
            issue_date: NaiveDate::from_ymd_opt(2026, 7, 3).unwrap(),
            total_amount: Decimal::from_str("553.00").unwrap(),
            tax_amount: Some(Decimal::from_str("50.73").unwrap()),
            tax_rate: Some(Decimal::from_str("0.09").unwrap()),
            buyer_name: Some("某某公司".to_string()),
            seller_name: Some("中国铁路".to_string()),
            ticket_type: TicketType::Rail,
            parse_level: ParseLevel::L0,
            confidence: 1.0,
            source_path: PathBuf::from("samples/rail-01.xml"),
        }
    }

    #[test]
    fn all_declared_fields_match() {
        let comparisons = sample_fixture().compare(&parsed_fixture());
        let failed: Vec<_> = comparisons.iter().filter(|c| !c.matched).collect();
        assert!(failed.is_empty(), "预期全部匹配，实际失败项: {failed:?}");
    }

    #[test]
    fn amount_mismatch_is_detected() {
        let mut parsed = parsed_fixture();
        parsed.total_amount = Decimal::from_str("12.80").unwrap();

        let comparisons = sample_fixture().compare(&parsed);
        let amount = comparisons
            .iter()
            .find(|c| c.field == "total_amount")
            .expect("应有 total_amount 比对项");

        assert!(!amount.matched);
        assert_eq!(amount.expected, "553.00");
        assert_eq!(amount.actual, "12.80");
    }

    #[test]
    fn trailing_zeros_do_not_cause_false_mismatch() {
        // 清单写 "553.00"，解析出 553 —— Decimal 数值相等，应判匹配
        let mut parsed = parsed_fixture();
        parsed.total_amount = Decimal::from_str("553").unwrap();

        let comparisons = sample_fixture().compare(&parsed);
        let amount = comparisons
            .iter()
            .find(|c| c.field == "total_amount")
            .unwrap();

        assert!(amount.matched, "553 与 553.00 应视为相等");
    }

    #[test]
    fn fields_absent_from_manifest_are_not_compared() {
        // sample_fixture 的 seller_name 是 None，不应产生比对项
        let comparisons = sample_fixture().compare(&parsed_fixture());
        assert!(
            comparisons.iter().all(|c| c.field != "seller_name"),
            "未声明的字段不应参与比对"
        );
    }

    #[test]
    fn manifest_parses_sample_array() {
        let toml_src = r#"
[[sample]]
path = "samples/a.xml"
format = "xml"
ticket_type = "Rail"
invoice_number = "111"
issue_date = "2026-07-03"
total_amount = "100.00"

[[sample]]
path = "samples/b.ofd"
format = "ofd"
ticket_type = "Hotel"
invoice_number = "222"
issue_date = "2026-07-04"
total_amount = "200.00"
tax_rate = "0.06"
"#;
        let manifest: Manifest = toml::from_str(toml_src).expect("清单应能解析");
        assert_eq!(manifest.samples.len(), 2);
        assert_eq!(manifest.samples[1].tax_rate.as_deref(), Some("0.06"));
    }
}
```

在 `crates/invoice-parse/src/lib.rs` 追加：

```rust
pub mod manifest;
```

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo test -p invoice-parse manifest`
Expected: 编译通过但测试 panic，信息含 `not implemented`（`unimplemented!()` 触发）

- [ ] **Step 3: 实现 load 与 compare**

替换 `manifest.rs` 里的两个 `unimplemented!()`：

```rust
impl Manifest {
    pub fn load(path: &Path) -> anyhow::Result<Manifest> {
        let src = std::fs::read_to_string(path)
            .map_err(|e| anyhow::anyhow!("读不到清单 {}: {e}", path.display()))?;
        let manifest: Manifest = toml::from_str(&src)
            .map_err(|e| anyhow::anyhow!("清单 {} 格式错误: {e}", path.display()))?;
        Ok(manifest)
    }
}

impl Sample {
    pub fn compare(&self, actual: &ParsedInvoice) -> Vec<FieldComparison> {
        let mut out = Vec::new();

        out.push(compare_str(
            "invoice_number",
            &self.invoice_number,
            &actual.invoice_number,
        ));

        out.push(compare_date("issue_date", &self.issue_date, actual.issue_date));

        out.push(compare_decimal(
            "total_amount",
            &self.total_amount,
            Some(actual.total_amount),
        ));

        if let Some(expected) = &self.tax_amount {
            out.push(compare_decimal("tax_amount", expected, actual.tax_amount));
        }
        if let Some(expected) = &self.tax_rate {
            out.push(compare_decimal("tax_rate", expected, actual.tax_rate));
        }
        if let Some(expected) = &self.buyer_name {
            out.push(compare_opt_str(
                "buyer_name",
                expected,
                actual.buyer_name.as_deref(),
            ));
        }
        if let Some(expected) = &self.seller_name {
            out.push(compare_opt_str(
                "seller_name",
                expected,
                actual.seller_name.as_deref(),
            ));
        }

        out.push(FieldComparison {
            field: "ticket_type",
            expected: format!("{:?}", self.ticket_type),
            actual: format!("{:?}", actual.ticket_type),
            matched: self.ticket_type == actual.ticket_type,
        });

        out
    }
}

fn compare_str(field: &'static str, expected: &str, actual: &str) -> FieldComparison {
    FieldComparison {
        field,
        expected: expected.to_string(),
        actual: actual.to_string(),
        matched: expected == actual,
    }
}

fn compare_opt_str(
    field: &'static str,
    expected: &str,
    actual: Option<&str>,
) -> FieldComparison {
    FieldComparison {
        field,
        expected: expected.to_string(),
        actual: actual.unwrap_or("<缺失>").to_string(),
        matched: actual == Some(expected),
    }
}

/// 数值比对走 Decimal，"553" 与 "553.00" 视为相等。
fn compare_decimal(
    field: &'static str,
    expected_raw: &str,
    actual: Option<Decimal>,
) -> FieldComparison {
    use rust_decimal::prelude::FromStr;

    let expected = Decimal::from_str(expected_raw).ok();
    let matched = match (expected, actual) {
        (Some(e), Some(a)) => e == a,
        _ => false,
    };
    FieldComparison {
        field,
        expected: expected_raw.to_string(),
        actual: actual.map(|d| d.to_string()).unwrap_or_else(|| "<缺失>".into()),
        matched,
    }
}

fn compare_date(
    field: &'static str,
    expected_raw: &str,
    actual: chrono::NaiveDate,
) -> FieldComparison {
    let expected = chrono::NaiveDate::parse_from_str(expected_raw, "%Y-%m-%d").ok();
    FieldComparison {
        field,
        expected: expected_raw.to_string(),
        actual: actual.to_string(),
        matched: expected == Some(actual),
    }
}
```

- [ ] **Step 4: 运行测试确认通过**

Run: `cargo test -p invoice-parse manifest`
Expected: 5 个测试全部 PASS

- [ ] **Step 5: Commit**

```bash
git add crates/invoice-parse/src/manifest.rs crates/invoice-parse/src/lib.rs
git commit -m "feat: add manifest loader with decimal-safe field comparison"
```

---

## Task 3: XML 元素探查工具

**Files:**
- Create: `crates/invoice-parse/src/xml.rs`
- Modify: `crates/invoice-parse/src/lib.rs`
- Modify: `crates/invoice-parse/src/main.rs`
- Test: `crates/invoice-parse/src/xml.rs`（inline `#[cfg(test)]`）

**Interfaces:**
- Consumes: `model::ParseError`（Task 1）
- Produces:
  - `xml::collect_leaf_elements(xml_bytes: &[u8]) -> Result<Vec<LeafElement>, ParseError>`
  - `LeafElement { tag: String, text: String, depth: usize }`

**为什么先做探查工具而不是直接写解析器**：数电票 XML 的元素名因开票平台而异，没有统一的公开 schema。凭猜测写标签名的解析器只会在某一个平台的样本上碰巧跑通。正确顺序是先把真实样本的结构 dump 出来，再据此填清单的 `xml_tag_hints`。这个工具在后续新增票种时还要反复用到。

- [ ] **Step 1: 写失败测试**

创建 `crates/invoice-parse/src/xml.rs`：

```rust
use crate::model::ParseError;
use quick_xml::events::Event;
use quick_xml::Reader;
use std::path::PathBuf;

/// XML 中一个含文本的叶子元素。
#[derive(Debug, Clone, PartialEq)]
pub struct LeafElement {
    pub tag: String,
    pub text: String,
    pub depth: usize,
}

/// 遍历 XML，收集所有含非空文本的叶子元素。
/// 命名空间前缀会被剥离（`tax:TotalAmount` → `TotalAmount`），
/// 因为不同平台的前缀不同但本地名通常一致。
pub fn collect_leaf_elements(xml_bytes: &[u8]) -> Result<Vec<LeafElement>, ParseError> {
    unimplemented!()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collects_nested_leaf_text() {
        let xml = br#"<Invoice>
            <Header><Number>12345</Number></Header>
            <Body><Amount>553.00</Amount><Tax>50.73</Tax></Body>
        </Invoice>"#;

        let leaves = collect_leaf_elements(xml).unwrap();

        assert_eq!(leaves.len(), 3);
        assert_eq!(leaves[0], LeafElement { tag: "Number".into(), text: "12345".into(), depth: 2 });
        assert_eq!(leaves[1], LeafElement { tag: "Amount".into(), text: "553.00".into(), depth: 2 });
        assert_eq!(leaves[2], LeafElement { tag: "Tax".into(), text: "50.73".into(), depth: 2 });
    }

    #[test]
    fn strips_namespace_prefix() {
        let xml = br#"<tax:Invoice xmlns:tax="urn:x"><tax:Number>999</tax:Number></tax:Invoice>"#;
        let leaves = collect_leaf_elements(xml).unwrap();
        assert_eq!(leaves[0].tag, "Number");
    }

    #[test]
    fn skips_whitespace_only_elements() {
        let xml = br#"<Root><Empty>   </Empty><Real>x</Real></Root>"#;
        let leaves = collect_leaf_elements(xml).unwrap();
        assert_eq!(leaves.len(), 1);
        assert_eq!(leaves[0].tag, "Real");
    }

    #[test]
    fn trims_surrounding_whitespace_in_text() {
        let xml = br#"<Root><Name>  某某公司
  </Name></Root>"#;
        let leaves = collect_leaf_elements(xml).unwrap();
        assert_eq!(leaves[0].text, "某某公司");
    }

    #[test]
    fn malformed_xml_returns_error() {
        let xml = br#"<Root><Unclosed></Root>"#;
        assert!(collect_leaf_elements(xml).is_err());
    }
}
```

在 `lib.rs` 追加 `pub mod xml;`。

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo test -p invoice-parse xml`
Expected: FAIL，panic 于 `not implemented`

- [ ] **Step 3: 实现 collect_leaf_elements**

替换 `xml.rs` 里的 `unimplemented!()`：

```rust
pub fn collect_leaf_elements(xml_bytes: &[u8]) -> Result<Vec<LeafElement>, ParseError> {
    let mut reader = Reader::from_reader(xml_bytes);
    reader.config_mut().trim_text(true);

    let mut leaves = Vec::new();
    let mut buf = Vec::new();
    // 栈顶记录当前元素的 (标签名, 深度, 是否已见过子元素)
    let mut stack: Vec<(String, usize, bool)> = Vec::new();
    let mut pending_text: Option<String> = None;

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                if let Some(parent) = stack.last_mut() {
                    parent.2 = true;
                }
                let tag = local_name(e.name().as_ref());
                let depth = stack.len();
                stack.push((tag, depth, false));
                pending_text = None;
            }
            Ok(Event::Text(e)) => {
                let text = e
                    .unescape()
                    .map_err(|err| ParseError::MalformedFormat {
                        path: PathBuf::new(),
                        format: "XML",
                        detail: format!("文本节点解码失败: {err}"),
                    })?
                    .trim()
                    .to_string();
                if !text.is_empty() {
                    pending_text = Some(text);
                }
            }
            Ok(Event::End(_)) => {
                if let Some((tag, depth, had_children)) = stack.pop() {
                    if !had_children {
                        if let Some(text) = pending_text.take() {
                            leaves.push(LeafElement { tag, text, depth });
                        }
                    }
                }
                pending_text = None;
            }
            Ok(Event::Eof) => break,
            Ok(_) => {}
            Err(err) => {
                return Err(ParseError::MalformedFormat {
                    path: PathBuf::new(),
                    format: "XML",
                    detail: err.to_string(),
                })
            }
        }
        buf.clear();
    }

    if !stack.is_empty() {
        return Err(ParseError::MalformedFormat {
            path: PathBuf::new(),
            format: "XML",
            detail: format!("有 {} 个元素未闭合", stack.len()),
        });
    }

    Ok(leaves)
}

/// 剥离命名空间前缀：`tax:Number` → `Number`
fn local_name(raw: &[u8]) -> String {
    let s = String::from_utf8_lossy(raw);
    match s.rsplit_once(':') {
        Some((_, local)) => local.to_string(),
        None => s.to_string(),
    }
}
```

- [ ] **Step 4: 运行测试确认通过**

Run: `cargo test -p invoice-parse xml`
Expected: 5 个测试全部 PASS

- [ ] **Step 5: 加 dump-tags 子命令**

替换 `crates/invoice-parse/src/main.rs`：

```rust
use anyhow::{bail, Context};
use invoice_parse::xml;
use std::path::PathBuf;

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();

    match args.get(1).map(String::as_str) {
        Some("dump-tags") => {
            let path = args.get(2).context("用法: invoice-parse dump-tags <file.xml>")?;
            dump_tags(PathBuf::from(path))
        }
        Some(other) => bail!("未知子命令: {other}"),
        None => {
            eprintln!("用法: invoice-parse dump-tags <file.xml>");
            Ok(())
        }
    }
}

fn dump_tags(path: PathBuf) -> anyhow::Result<()> {
    let bytes = std::fs::read(&path).with_context(|| format!("读取 {} 失败", path.display()))?;
    let leaves = xml::collect_leaf_elements(&bytes)?;

    println!("{} 个叶子元素：\n", leaves.len());
    for leaf in &leaves {
        let indent = "  ".repeat(leaf.depth);
        println!("{indent}{:<28} = {}", leaf.tag, leaf.text);
    }
    Ok(())
}
```

- [ ] **Step 6: 对真实样本运行探查**

Run: `cargo run -p invoice-parse -- dump-tags fixtures/samples/<你的数电票>.xml`
Expected: 打印出该样本所有含文本的元素及其值

**把观察到的标签名填进 `fixtures/manifest.toml` 的对应样本**，例如：

```toml
[sample.xml_tag_hints]
invoice_number = ["InvoiceNumber", "Fphm", "发票号码"]
issue_date = ["IssueDate", "Kprq", "开票日期"]
total_amount = ["TotalAmount", "Jshj", "价税合计"]
tax_amount = ["TaxAmount", "Se", "税额"]
tax_rate = ["TaxRate", "Sl", "税率"]
buyer_name = ["BuyerName", "Gfmc", "购买方名称"]
seller_name = ["SellerName", "Xfmc", "销售方名称"]
```

候选名按优先级排列，解析器取第一个命中的。5 张样本可能出现不同标签名，全部并入同一个候选列表即可。

- [ ] **Step 7: Commit**

```bash
git add crates/invoice-parse/src/xml.rs crates/invoice-parse/src/main.rs \
        crates/invoice-parse/src/lib.rs fixtures/manifest.toml
git commit -m "feat: add XML leaf element inspector and dump-tags command"
```

---

## Task 4: 数电票 XML 解析器（L0）

**Files:**
- Modify: `crates/invoice-parse/src/xml.rs`
- Test: `crates/invoice-parse/src/xml.rs`（inline `#[cfg(test)]`）

**Interfaces:**
- Consumes: `xml::{collect_leaf_elements, LeafElement}`（Task 3）、`manifest::TagHints`（Task 2）、`model::{ParsedInvoice, ParseError, ParseLevel, TicketType}`（Task 1）
- Produces: `xml::parse_invoice_xml(bytes: &[u8], path: &Path, hints: &TagHints, ticket_type: TicketType) -> Result<ParsedInvoice, ParseError>`

- [ ] **Step 1: 写失败测试**

在 `xml.rs` 的 `mod tests` 内追加：

```rust
    use crate::manifest::TagHints;
    use crate::model::{ParseLevel, TicketType};
    use rust_decimal::prelude::FromStr;
    use rust_decimal::Decimal;
    use std::path::Path;

    fn hints() -> TagHints {
        TagHints {
            invoice_number: vec!["Fphm".into(), "InvoiceNumber".into()],
            issue_date: vec!["Kprq".into()],
            total_amount: vec!["Jshj".into()],
            tax_amount: vec!["Se".into()],
            tax_rate: vec!["Sl".into()],
            buyer_name: vec!["Gfmc".into()],
            seller_name: vec!["Xfmc".into()],
        }
    }

    const SAMPLE_XML: &[u8] = br#"<Invoice>
        <Head><Fphm>24312000000012345678</Fphm><Kprq>2026-07-03</Kprq></Head>
        <Sum><Jshj>553.00</Jshj><Se>50.73</Se><Sl>0.09</Sl></Sum>
        <Party><Gfmc>某某公司</Gfmc><Xfmc>中国铁路</Xfmc></Party>
    </Invoice>"#;

    #[test]
    fn parses_all_fields_from_hinted_tags() {
        let invoice = parse_invoice_xml(
            SAMPLE_XML,
            Path::new("samples/rail-01.xml"),
            &hints(),
            TicketType::Rail,
        )
        .unwrap();

        assert_eq!(invoice.invoice_number, "24312000000012345678");
        assert_eq!(invoice.issue_date.to_string(), "2026-07-03");
        assert_eq!(invoice.total_amount, Decimal::from_str("553.00").unwrap());
        assert_eq!(invoice.tax_amount, Some(Decimal::from_str("50.73").unwrap()));
        assert_eq!(invoice.buyer_name.as_deref(), Some("某某公司"));
        assert_eq!(invoice.parse_level, ParseLevel::L0);
        assert_eq!(invoice.confidence, 1.0);
    }

    #[test]
    fn falls_back_to_second_candidate_tag() {
        let xml = br#"<I><InvoiceNumber>888</InvoiceNumber><Kprq>2026-07-03</Kprq><Jshj>1.00</Jshj></I>"#;
        let invoice =
            parse_invoice_xml(xml, Path::new("x.xml"), &hints(), TicketType::Other).unwrap();
        assert_eq!(invoice.invoice_number, "888");
    }

    #[test]
    fn missing_required_field_errors_with_field_name() {
        let xml = br#"<I><Kprq>2026-07-03</Kprq><Jshj>1.00</Jshj></I>"#;
        let err = parse_invoice_xml(xml, Path::new("x.xml"), &hints(), TicketType::Other)
            .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("invoice_number"), "错误信息应指出缺失字段: {msg}");
    }

    #[test]
    fn absent_optional_fields_become_none() {
        let xml = br#"<I><Fphm>1</Fphm><Kprq>2026-07-03</Kprq><Jshj>1.00</Jshj></I>"#;
        let invoice =
            parse_invoice_xml(xml, Path::new("x.xml"), &hints(), TicketType::Other).unwrap();
        assert_eq!(invoice.tax_amount, None);
        assert_eq!(invoice.buyer_name, None);
    }

    #[test]
    fn slash_separated_date_is_accepted() {
        let xml = br#"<I><Fphm>1</Fphm><Kprq>2026/07/03</Kprq><Jshj>1.00</Jshj></I>"#;
        let invoice =
            parse_invoice_xml(xml, Path::new("x.xml"), &hints(), TicketType::Other).unwrap();
        assert_eq!(invoice.issue_date.to_string(), "2026-07-03");
    }

    #[test]
    fn compact_date_is_accepted() {
        let xml = br#"<I><Fphm>1</Fphm><Kprq>20260703</Kprq><Jshj>1.00</Jshj></I>"#;
        let invoice =
            parse_invoice_xml(xml, Path::new("x.xml"), &hints(), TicketType::Other).unwrap();
        assert_eq!(invoice.issue_date.to_string(), "2026-07-03");
    }

    #[test]
    fn amount_with_currency_symbol_is_cleaned() {
        let xml = br#"<I><Fphm>1</Fphm><Kprq>2026-07-03</Kprq><Jshj>￥1,553.00</Jshj></I>"#;
        let invoice =
            parse_invoice_xml(xml, Path::new("x.xml"), &hints(), TicketType::Other).unwrap();
        assert_eq!(invoice.total_amount, Decimal::from_str("1553.00").unwrap());
    }

    #[test]
    fn percent_tax_rate_is_normalized_to_fraction() {
        let xml = br#"<I><Fphm>1</Fphm><Kprq>2026-07-03</Kprq><Jshj>1.00</Jshj><Sl>9%</Sl></I>"#;
        let invoice =
            parse_invoice_xml(xml, Path::new("x.xml"), &hints(), TicketType::Other).unwrap();
        assert_eq!(invoice.tax_rate, Some(Decimal::from_str("0.09").unwrap()));
    }
```

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo test -p invoice-parse xml`
Expected: FAIL，`parse_invoice_xml` 未定义（编译错误）

- [ ] **Step 3: 实现解析器与字段清洗**

在 `xml.rs` 顶部追加 imports，并在 `local_name` 之后追加实现：

```rust
use crate::manifest::TagHints;
use crate::model::{ParseLevel, ParsedInvoice, TicketType};
use chrono::NaiveDate;
use rust_decimal::Decimal;
use std::path::Path;
use std::str::FromStr;

pub fn parse_invoice_xml(
    bytes: &[u8],
    path: &Path,
    hints: &TagHints,
    ticket_type: TicketType,
) -> Result<ParsedInvoice, ParseError> {
    let leaves = collect_leaf_elements(bytes).map_err(|e| match e {
        // collect_leaf_elements 不知道文件路径，这里补上
        ParseError::MalformedFormat { format, detail, .. } => ParseError::MalformedFormat {
            path: path.to_path_buf(),
            format,
            detail,
        },
        other => other,
    })?;

    let find = |candidates: &[String]| -> Option<String> {
        candidates
            .iter()
            .find_map(|want| {
                leaves
                    .iter()
                    .find(|leaf| leaf.tag == *want)
                    .map(|leaf| leaf.text.clone())
            })
    };

    let require = |candidates: &[String], field: &str| -> Result<String, ParseError> {
        find(candidates).ok_or_else(|| ParseError::MissingField {
            path: path.to_path_buf(),
            field: field.to_string(),
        })
    };

    let invoice_number = require(&hints.invoice_number, "invoice_number")?;
    let issue_date = parse_date(&require(&hints.issue_date, "issue_date")?)?;
    let total_amount = parse_amount(&require(&hints.total_amount, "total_amount")?, "total_amount")?;

    let tax_amount = find(&hints.tax_amount)
        .map(|raw| parse_amount(&raw, "tax_amount"))
        .transpose()?;
    let tax_rate = find(&hints.tax_rate)
        .map(|raw| parse_tax_rate(&raw))
        .transpose()?;

    Ok(ParsedInvoice {
        invoice_number,
        issue_date,
        total_amount,
        tax_amount,
        tax_rate,
        buyer_name: find(&hints.buyer_name),
        seller_name: find(&hints.seller_name),
        ticket_type,
        parse_level: ParseLevel::L0,
        confidence: 1.0,
        source_path: path.to_path_buf(),
    })
}

/// 接受 `2026-07-03`、`2026/07/03`、`20260703`、`2026年07月03日`
pub(crate) fn parse_date(raw: &str) -> Result<NaiveDate, ParseError> {
    let cleaned: String = raw
        .chars()
        .filter(|c| c.is_ascii_digit())
        .collect();

    if cleaned.len() == 8 {
        if let Ok(d) = NaiveDate::parse_from_str(&cleaned, "%Y%m%d") {
            return Ok(d);
        }
    }
    Err(ParseError::UnparseableValue {
        field: "issue_date".to_string(),
        raw: raw.to_string(),
        expected_type: "date",
    })
}

/// 去掉货币符号、千分位逗号、空白后转 Decimal
pub(crate) fn parse_amount(raw: &str, field: &str) -> Result<Decimal, ParseError> {
    let cleaned: String = raw
        .chars()
        .filter(|c| c.is_ascii_digit() || *c == '.' || *c == '-')
        .collect();

    Decimal::from_str(&cleaned).map_err(|_| ParseError::UnparseableValue {
        field: field.to_string(),
        raw: raw.to_string(),
        expected_type: "decimal",
    })
}

/// `9%` → 0.09；`0.09` → 0.09。
/// 判据：含 `%` 则除以 100；否则若值 > 1 也视为百分数（税率不可能超过 100%）。
pub(crate) fn parse_tax_rate(raw: &str) -> Result<Decimal, ParseError> {
    let has_percent = raw.contains('%');
    let value = parse_amount(raw, "tax_rate")?;

    let normalized = if has_percent || value > Decimal::ONE {
        value / Decimal::from(100)
    } else {
        value
    };
    Ok(normalized)
}
```

- [ ] **Step 4: 运行测试确认通过**

Run: `cargo test -p invoice-parse xml`
Expected: 13 个测试全部 PASS（Task 3 的 5 个 + 本任务的 8 个）

- [ ] **Step 5: 对真实样本验证**

Run: `cargo test -p invoice-parse xml -- --nocapture`

然后手工跑一次真实样本（下一个任务会把这一步自动化）：

Run: `cargo run -p invoice-parse -- dump-tags fixtures/samples/<数电票>.xml`

确认清单里的 `xml_tag_hints` 能覆盖全部 5 张 XML 样本的标签名。若某张样本的标签名不在候选列表里，追加进去——**不要为单个样本写特例代码**。

- [ ] **Step 6: Commit**

```bash
git add crates/invoice-parse/src/xml.rs fixtures/manifest.toml
git commit -m "feat: parse 数电票 XML with hint-driven tag resolution"
```

---

## Task 5: OFD 容器解析（L0）

**Files:**
- Create: `crates/invoice-parse/src/ofd.rs`
- Modify: `crates/invoice-parse/src/lib.rs`
- Modify: `crates/invoice-parse/src/main.rs`
- Test: `crates/invoice-parse/src/ofd.rs`（inline `#[cfg(test)]`）

**Interfaces:**
- Consumes: `xml::{collect_leaf_elements, parse_invoice_xml}`（Task 3、4）、`manifest::TagHints`、`model::*`
- Produces:
  - `ofd::list_entries(ofd_bytes: &[u8]) -> Result<Vec<String>, ParseError>`
  - `ofd::extract_invoice_xml(ofd_bytes: &[u8], path: &Path) -> Result<Vec<u8>, ParseError>`
  - `ofd::parse_invoice_ofd(ofd_bytes: &[u8], path: &Path, hints: &TagHints, ticket_type: TicketType) -> Result<ParsedInvoice, ParseError>`

**关键认识**：OFD 是 ZIP 容器（GB/T 33190-2016），内含 `OFD.xml`、`Doc_0/Document.xml` 等版式文件。**我们不需要渲染版式**——数电票的 OFD 里带有一份内嵌的发票 XML（附件区），把它取出来就能复用 Task 4 的解析器。这让 OFD 支持的成本从"实现一个版式渲染器"降到"从 ZIP 里找一个文件"。

- [ ] **Step 1: 写失败测试**

创建 `crates/invoice-parse/src/ofd.rs`：

```rust
use crate::manifest::TagHints;
use crate::model::{ParseError, ParsedInvoice, TicketType};
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};

/// 列出 OFD（ZIP）内所有条目名，用于探查结构。
pub fn list_entries(ofd_bytes: &[u8]) -> Result<Vec<String>, ParseError> {
    unimplemented!()
}

/// 从 OFD 中取出内嵌的发票 XML。
/// 策略：优先找路径含 "invoice"/"发票" 的 .xml；
/// 退化为选择除版式文件（OFD.xml/Document.xml/DocumentRes.xml 等）之外
/// 体积最大的 .xml —— 内嵌发票数据通常远大于结构描述文件。
pub fn extract_invoice_xml(ofd_bytes: &[u8], path: &Path) -> Result<Vec<u8>, ParseError> {
    unimplemented!()
}

pub fn parse_invoice_ofd(
    ofd_bytes: &[u8],
    path: &Path,
    hints: &TagHints,
    ticket_type: TicketType,
) -> Result<ParsedInvoice, ParseError> {
    unimplemented!()
}

/// 这些是 OFD 的版式结构文件，不含发票业务数据。
const LAYOUT_FILES: &[&str] = &[
    "OFD.xml",
    "Document.xml",
    "DocumentRes.xml",
    "PublicRes.xml",
    "Annotations.xml",
    "Signatures.xml",
    "Signature.xml",
    "Attachments.xml",
];

fn is_layout_file(entry_name: &str) -> bool {
    let file_name = entry_name.rsplit('/').next().unwrap_or(entry_name);
    LAYOUT_FILES.iter().any(|f| f.eq_ignore_ascii_case(file_name))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use zip::write::SimpleFileOptions;

    /// 构造一个最小 OFD：一个版式文件 + 一个内嵌发票 XML
    fn build_ofd(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut buf = Vec::new();
        {
            let mut zip = zip::ZipWriter::new(Cursor::new(&mut buf));
            for (name, content) in entries {
                zip.start_file(*name, SimpleFileOptions::default()).unwrap();
                zip.write_all(content).unwrap();
            }
            zip.finish().unwrap();
        }
        buf
    }

    const INVOICE_XML: &[u8] = br#"<Invoice>
        <Fphm>24312000000012345678</Fphm><Kprq>2026-07-03</Kprq>
        <Jshj>553.00</Jshj><Se>50.73</Se>
    </Invoice>"#;

    #[test]
    fn lists_all_zip_entries() {
        let ofd = build_ofd(&[("OFD.xml", b"<OFD/>"), ("Doc_0/invoice.xml", INVOICE_XML)]);
        let entries = list_entries(&ofd).unwrap();
        assert!(entries.contains(&"OFD.xml".to_string()));
        assert!(entries.contains(&"Doc_0/invoice.xml".to_string()));
    }

    #[test]
    fn picks_xml_whose_path_mentions_invoice() {
        let ofd = build_ofd(&[
            ("OFD.xml", b"<OFD/>"),
            ("Doc_0/Document.xml", b"<Document/>"),
            ("Doc_0/Attachs/invoice.xml", INVOICE_XML),
        ]);
        let xml = extract_invoice_xml(&ofd, Path::new("x.ofd")).unwrap();
        assert_eq!(xml, INVOICE_XML);
    }

    #[test]
    fn falls_back_to_largest_non_layout_xml() {
        let ofd = build_ofd(&[
            ("OFD.xml", b"<OFD/>"),
            ("Doc_0/Document.xml", b"<Document/>"),
            ("Doc_0/Attachs/data_001.xml", INVOICE_XML),
        ]);
        let xml = extract_invoice_xml(&ofd, Path::new("x.ofd")).unwrap();
        assert_eq!(xml, INVOICE_XML);
    }

    #[test]
    fn layout_only_ofd_errors_clearly() {
        let ofd = build_ofd(&[("OFD.xml", b"<OFD/>"), ("Doc_0/Document.xml", b"<Document/>")]);
        let err = extract_invoice_xml(&ofd, Path::new("x.ofd")).unwrap_err();
        assert!(err.to_string().contains("找不到"), "错误应说明未找到内嵌 XML");
    }

    #[test]
    fn non_zip_input_errors() {
        let err = list_entries(b"this is not a zip").unwrap_err();
        assert!(matches!(err, ParseError::MalformedFormat { .. }));
    }

    #[test]
    fn end_to_end_ofd_parse_yields_fields() {
        let ofd = build_ofd(&[("OFD.xml", b"<OFD/>"), ("Doc_0/Attachs/invoice.xml", INVOICE_XML)]);
        let hints = TagHints {
            invoice_number: vec!["Fphm".into()],
            issue_date: vec!["Kprq".into()],
            total_amount: vec!["Jshj".into()],
            tax_amount: vec!["Se".into()],
            tax_rate: vec![],
            buyer_name: vec![],
            seller_name: vec![],
        };
        let invoice =
            parse_invoice_ofd(&ofd, Path::new("x.ofd"), &hints, TicketType::Hotel).unwrap();
        assert_eq!(invoice.invoice_number, "24312000000012345678");
        assert_eq!(invoice.ticket_type, TicketType::Hotel);
    }
}
```

在 `lib.rs` 追加 `pub mod ofd;`。

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo test -p invoice-parse ofd`
Expected: FAIL，panic 于 `not implemented`

- [ ] **Step 3: 实现三个函数**

替换 `ofd.rs` 里的三处 `unimplemented!()`：

```rust
pub fn list_entries(ofd_bytes: &[u8]) -> Result<Vec<String>, ParseError> {
    let mut archive = open_zip(ofd_bytes, Path::new(""))?;
    let mut names = Vec::with_capacity(archive.len());
    for i in 0..archive.len() {
        let entry = archive.by_index(i).map_err(|e| ParseError::MalformedFormat {
            path: PathBuf::new(),
            format: "OFD",
            detail: format!("读取第 {i} 个条目失败: {e}"),
        })?;
        names.push(entry.name().to_string());
    }
    Ok(names)
}

pub fn extract_invoice_xml(ofd_bytes: &[u8], path: &Path) -> Result<Vec<u8>, ParseError> {
    let mut archive = open_zip(ofd_bytes, path)?;

    // 收集所有非版式的 .xml 条目：(索引, 名称, 体积)
    let mut candidates: Vec<(usize, String, u64)> = Vec::new();
    for i in 0..archive.len() {
        let entry = archive.by_index(i).map_err(|e| ParseError::MalformedFormat {
            path: path.to_path_buf(),
            format: "OFD",
            detail: format!("读取第 {i} 个条目失败: {e}"),
        })?;
        let name = entry.name().to_string();
        if name.to_lowercase().ends_with(".xml") && !is_layout_file(&name) {
            candidates.push((i, name, entry.size()));
        }
    }

    if candidates.is_empty() {
        return Err(ParseError::MalformedFormat {
            path: path.to_path_buf(),
            format: "OFD",
            detail: "找不到内嵌的发票 XML（容器内只有版式文件）".to_string(),
        });
    }

    // 优先：路径提到 invoice / 发票
    let chosen = candidates
        .iter()
        .find(|(_, name, _)| {
            let lower = name.to_lowercase();
            lower.contains("invoice") || name.contains("发票")
        })
        // 退化：体积最大的
        .or_else(|| candidates.iter().max_by_key(|(_, _, size)| *size))
        .map(|(i, _, _)| *i)
        .expect("candidates 非空");

    let mut entry = archive.by_index(chosen).map_err(|e| ParseError::MalformedFormat {
        path: path.to_path_buf(),
        format: "OFD",
        detail: format!("打开内嵌 XML 失败: {e}"),
    })?;

    let mut buf = Vec::with_capacity(entry.size() as usize);
    entry.read_to_end(&mut buf).map_err(|e| ParseError::Io {
        path: path.to_path_buf(),
        source: e,
    })?;
    Ok(buf)
}

pub fn parse_invoice_ofd(
    ofd_bytes: &[u8],
    path: &Path,
    hints: &TagHints,
    ticket_type: TicketType,
) -> Result<ParsedInvoice, ParseError> {
    let xml_bytes = extract_invoice_xml(ofd_bytes, path)?;
    crate::xml::parse_invoice_xml(&xml_bytes, path, hints, ticket_type)
}

fn open_zip(
    bytes: &[u8],
    path: &Path,
) -> Result<zip::ZipArchive<Cursor<Vec<u8>>>, ParseError> {
    zip::ZipArchive::new(Cursor::new(bytes.to_vec())).map_err(|e| ParseError::MalformedFormat {
        path: path.to_path_buf(),
        format: "OFD",
        detail: format!("不是有效的 ZIP 容器: {e}"),
    })
}
```

`zip` 需要 dev 侧的写入能力，在 `crates/invoice-parse/Cargo.toml` 确认 `zip` 已在 `[dependencies]`（Task 1 已加），测试用的 `ZipWriter` 同一个 crate 提供，无需额外依赖。

- [ ] **Step 4: 运行测试确认通过**

Run: `cargo test -p invoice-parse ofd`
Expected: 6 个测试全部 PASS

- [ ] **Step 5: 加 dump-ofd 子命令**

在 `main.rs` 的 `match` 中追加分支：

```rust
        Some("dump-ofd") => {
            let path = args.get(2).context("用法: invoice-parse dump-ofd <file.ofd>")?;
            dump_ofd(PathBuf::from(path))
        }
```

并追加函数：

```rust
fn dump_ofd(path: PathBuf) -> anyhow::Result<()> {
    let bytes = std::fs::read(&path).with_context(|| format!("读取 {} 失败", path.display()))?;

    println!("容器条目：");
    for name in invoice_parse::ofd::list_entries(&bytes)? {
        println!("  {name}");
    }

    match invoice_parse::ofd::extract_invoice_xml(&bytes, &path) {
        Ok(xml) => {
            println!("\n内嵌发票 XML 的叶子元素：");
            for leaf in invoice_parse::xml::collect_leaf_elements(&xml)? {
                println!("  {:<28} = {}", leaf.tag, leaf.text);
            }
        }
        Err(e) => println!("\n未能提取内嵌 XML: {e}"),
    }
    Ok(())
}
```

- [ ] **Step 6: 对真实 OFD 样本验证**

Run: `cargo run -p invoice-parse -- dump-ofd fixtures/samples/<你的OFD>.ofd`

**这一步会暴露 OFD 支持的真实可行性**。三种可能：

| 观察结果 | 含义 | 处理 |
|---|---|---|
| 打印出发票字段 | 内嵌 XML 策略成立 | 把标签名并入清单的 `xml_tag_hints` |
| 提取到 XML 但字段不对 | 选错了文件 | 看条目清单，调整 `extract_invoice_xml` 的优先级判据 |
| 报"找不到内嵌 XML" | 该 OFD 只有版式，无结构化数据 | **记入验证报告**：这类 OFD 需走 L2 OCR，是覆盖缺口 |

第三种情况必须诚实记录——它意味着部分 OFD 拿不到税额，会影响 Concur 填表完整性。

- [ ] **Step 7: Commit**

```bash
git add crates/invoice-parse/src/ofd.rs crates/invoice-parse/src/lib.rs \
        crates/invoice-parse/src/main.rs fixtures/manifest.toml
git commit -m "feat: extract embedded invoice XML from OFD container"
```

---

## Task 6: PDF 文本层解析（L1）

**Files:**
- Create: `crates/invoice-parse/src/pdf.rs`
- Modify: `crates/invoice-parse/src/lib.rs`
- Modify: `crates/invoice-parse/src/main.rs`
- Modify: `crates/invoice-parse/Cargo.toml`
- Test: `crates/invoice-parse/src/pdf.rs`（inline `#[cfg(test)]`）

**Interfaces:**
- Consumes: `xml::{parse_date, parse_amount, parse_tax_rate}`（Task 4，需改为 `pub(crate)` — Task 4 已如此声明）、`model::*`
- Produces:
  - `pdf::extract_text(pdf_bytes: &[u8], path: &Path) -> Result<String, ParseError>`
  - `pdf::has_text_layer(pdf_bytes: &[u8]) -> bool`
  - `pdf::parse_rail_itinerary(text: &str, path: &Path) -> Result<ParsedInvoice, ParseError>`
  - `pdf::parse_flight_itinerary(text: &str, path: &Path) -> Result<ParsedInvoice, ParseError>`
  - `pdf::parse_vat_invoice_text(text: &str, path: &Path) -> Result<ParsedInvoice, ParseError>`

**设计取舍**：PDF 文本层提取出的是**无结构的文本流**，不是表格。所以这三个解析器用**正则 + 关键词锚定**，不依赖坐标。这对铁路/航空行程单是可行的（它们的文本包含固定标签如"票价""电子客票号"），对复杂的增值税票版式会更脆弱——所以 `parse_vat_invoice_text` 失败时应退回 L2 OCR，而不是报错终止。

- [ ] **Step 1: 加依赖**

在 `crates/invoice-parse/Cargo.toml` 的 `[dependencies]` 追加：

```toml
pdf-extract = "0.7"
regex = "1.11"
once_cell = "1.20"
```

- [ ] **Step 2: 写失败测试**

创建 `crates/invoice-parse/src/pdf.rs`：

```rust
use crate::model::{ParseError, ParseLevel, ParsedInvoice, TicketType};
use crate::xml::{parse_amount, parse_date};
use once_cell::sync::Lazy;
use regex::Regex;
use std::path::{Path, PathBuf};

pub fn extract_text(pdf_bytes: &[u8], path: &Path) -> Result<String, ParseError> {
    unimplemented!()
}

/// 判断 PDF 是否含可提取的文本层。
/// 用于路由：无文本层的走 L2 OCR。
pub fn has_text_layer(pdf_bytes: &[u8]) -> bool {
    unimplemented!()
}

pub fn parse_rail_itinerary(text: &str, path: &Path) -> Result<ParsedInvoice, ParseError> {
    unimplemented!()
}

pub fn parse_flight_itinerary(text: &str, path: &Path) -> Result<ParsedInvoice, ParseError> {
    unimplemented!()
}

pub fn parse_vat_invoice_text(text: &str, path: &Path) -> Result<ParsedInvoice, ParseError> {
    unimplemented!()
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal::prelude::FromStr;
    use rust_decimal::Decimal;

    // 铁路电子客票行程单的典型文本层内容（字段顺序可能因版式而异，
    // 所以解析器必须靠关键词锚定而非位置）
    const RAIL_TEXT: &str = "电子发票（铁路电子客票）
发票号码 24312000000012345678
开票日期 2026年07月03日
车次 G13 北京南 上海虹桥
2026年07月03日09:00开
票价 ￥553.00
税率 9% 税额 ￥45.63
购买方名称 某某科技有限公司";

    const FLIGHT_TEXT: &str = "航空运输电子客票行程单
电子客票号码 7812345678901
填开日期 2026-07-10
承运人 CZ 航班号 CZ3001
北京首都 - 深圳宝安
票价 1580.00
民航发展基金 50.00
燃油附加费 220.00
合计 1850.00";

    #[test]
    fn rail_itinerary_yields_number_date_amount() {
        let invoice = parse_rail_itinerary(RAIL_TEXT, Path::new("rail.pdf")).unwrap();

        assert_eq!(invoice.invoice_number, "24312000000012345678");
        assert_eq!(invoice.issue_date.to_string(), "2026-07-03");
        assert_eq!(invoice.total_amount, Decimal::from_str("553.00").unwrap());
        assert_eq!(invoice.ticket_type, TicketType::Rail);
        assert_eq!(invoice.parse_level, ParseLevel::L1);
    }

    #[test]
    fn rail_itinerary_extracts_tax_fields() {
        let invoice = parse_rail_itinerary(RAIL_TEXT, Path::new("rail.pdf")).unwrap();
        assert_eq!(invoice.tax_amount, Some(Decimal::from_str("45.63").unwrap()));
        assert_eq!(invoice.tax_rate, Some(Decimal::from_str("0.09").unwrap()));
    }

    #[test]
    fn flight_itinerary_uses_total_not_base_fare() {
        // 陷阱：文本里有"票价 1580.00"和"合计 1850.00"，
        // 报销金额必须取合计
        let invoice = parse_flight_itinerary(FLIGHT_TEXT, Path::new("air.pdf")).unwrap();
        assert_eq!(invoice.total_amount, Decimal::from_str("1850.00").unwrap());
        assert_eq!(invoice.ticket_type, TicketType::Flight);
    }

    #[test]
    fn flight_itinerary_uses_ticket_number_as_invoice_number() {
        let invoice = parse_flight_itinerary(FLIGHT_TEXT, Path::new("air.pdf")).unwrap();
        assert_eq!(invoice.invoice_number, "7812345678901");
    }

    #[test]
    fn missing_amount_reports_field_name() {
        let text = "电子发票（铁路电子客票）\n发票号码 123\n开票日期 2026年07月03日";
        let err = parse_rail_itinerary(text, Path::new("x.pdf")).unwrap_err();
        assert!(err.to_string().contains("total_amount"), "实际: {err}");
    }

    #[test]
    fn empty_text_is_treated_as_no_text_layer() {
        // 纯扫描件 PDF 提取出的文本为空或只有空白
        assert!(!has_text_layer(b"%PDF-1.4\n%%EOF"));
    }
}
```

在 `lib.rs` 追加 `pub mod pdf;`。

- [ ] **Step 3: 运行测试确认失败**

Run: `cargo test -p invoice-parse pdf`
Expected: FAIL，panic 于 `not implemented`

- [ ] **Step 4: 实现文本提取与三个版式解析器**

替换 `pdf.rs` 的五处 `unimplemented!()`：

```rust
pub fn extract_text(pdf_bytes: &[u8], path: &Path) -> Result<String, ParseError> {
    pdf_extract::extract_text_from_mem(pdf_bytes).map_err(|e| ParseError::MalformedFormat {
        path: path.to_path_buf(),
        format: "PDF",
        detail: format!("文本层提取失败: {e}"),
    })
}

pub fn has_text_layer(pdf_bytes: &[u8]) -> bool {
    match pdf_extract::extract_text_from_mem(pdf_bytes) {
        // 少于 20 个非空白字符视为没有有效文本层（纯扫描件）
        Ok(text) => text.chars().filter(|c| !c.is_whitespace()).count() >= 20,
        Err(_) => false,
    }
}

/// 在文本中按标签抓取其后紧跟的值。
/// 标签与值之间允许空格、全角空格、冒号。
fn capture_after(text: &str, labels: &[&str], value_pattern: &str) -> Option<String> {
    for label in labels {
        let pattern = format!(r"{}[\s：:]*({})", regex::escape(label), value_pattern);
        let re = Regex::new(&pattern).expect("内置正则应有效");
        if let Some(caps) = re.captures(text) {
            return Some(caps[1].trim().to_string());
        }
    }
    None
}

const AMOUNT_PATTERN: &str = r"[￥¥]?\s*[\d,]+\.?\d*";
const DATE_PATTERN: &str = r"\d{4}[-/年]\d{1,2}[-/月]\d{1,2}日?";
const DIGITS_PATTERN: &str = r"\d[\d\s-]*\d";

fn require_field(
    value: Option<String>,
    field: &str,
    path: &Path,
) -> Result<String, ParseError> {
    value.ok_or_else(|| ParseError::MissingField {
        path: path.to_path_buf(),
        field: field.to_string(),
    })
}

pub fn parse_rail_itinerary(text: &str, path: &Path) -> Result<ParsedInvoice, ParseError> {
    let number_raw = require_field(
        capture_after(text, &["发票号码", "发票号"], DIGITS_PATTERN),
        "invoice_number",
        path,
    )?;
    let date_raw = require_field(
        capture_after(text, &["开票日期"], DATE_PATTERN),
        "issue_date",
        path,
    )?;
    let amount_raw = require_field(
        capture_after(text, &["票价", "金额", "价税合计"], AMOUNT_PATTERN),
        "total_amount",
        path,
    )?;

    Ok(ParsedInvoice {
        invoice_number: number_raw.chars().filter(|c| c.is_ascii_digit()).collect(),
        issue_date: parse_date(&date_raw)?,
        total_amount: parse_amount(&amount_raw, "total_amount")?,
        tax_amount: capture_after(text, &["税额"], AMOUNT_PATTERN)
            .map(|raw| parse_amount(&raw, "tax_amount"))
            .transpose()?,
        tax_rate: capture_after(text, &["税率"], r"\d+\.?\d*%?")
            .map(|raw| crate::xml::parse_tax_rate(&raw))
            .transpose()?,
        buyer_name: capture_after(text, &["购买方名称", "购买方"], r"\S+"),
        seller_name: None,
        ticket_type: TicketType::Rail,
        parse_level: ParseLevel::L1,
        confidence: 1.0,
        source_path: path.to_path_buf(),
    })
}

pub fn parse_flight_itinerary(text: &str, path: &Path) -> Result<ParsedInvoice, ParseError> {
    // 航空行程单没有"发票号码"，用电子客票号作为唯一标识
    let number_raw = require_field(
        capture_after(text, &["电子客票号码", "电子客票号", "票号"], DIGITS_PATTERN),
        "invoice_number",
        path,
    )?;
    let date_raw = require_field(
        capture_after(text, &["填开日期", "开票日期"], DATE_PATTERN),
        "issue_date",
        path,
    )?;
    // 必须取"合计"，不能取"票价"——票价不含基金和燃油附加费
    let amount_raw = require_field(
        capture_after(text, &["合计", "价税合计", "总额"], AMOUNT_PATTERN),
        "total_amount",
        path,
    )?;

    Ok(ParsedInvoice {
        invoice_number: number_raw.chars().filter(|c| c.is_ascii_digit()).collect(),
        issue_date: parse_date(&date_raw)?,
        total_amount: parse_amount(&amount_raw, "total_amount")?,
        tax_amount: None,
        tax_rate: None,
        buyer_name: capture_after(text, &["旅客姓名", "购买方名称"], r"\S+"),
        seller_name: capture_after(text, &["承运人"], r"\S+"),
        ticket_type: TicketType::Flight,
        parse_level: ParseLevel::L1,
        confidence: 1.0,
        source_path: path.to_path_buf(),
    })
}

pub fn parse_vat_invoice_text(text: &str, path: &Path) -> Result<ParsedInvoice, ParseError> {
    let number_raw = require_field(
        capture_after(text, &["发票号码", "发票号"], DIGITS_PATTERN),
        "invoice_number",
        path,
    )?;
    let date_raw = require_field(
        capture_after(text, &["开票日期"], DATE_PATTERN),
        "issue_date",
        path,
    )?;
    let amount_raw = require_field(
        capture_after(text, &["价税合计", "合计金额", "小写"], AMOUNT_PATTERN),
        "total_amount",
        path,
    )?;

    Ok(ParsedInvoice {
        invoice_number: number_raw.chars().filter(|c| c.is_ascii_digit()).collect(),
        issue_date: parse_date(&date_raw)?,
        total_amount: parse_amount(&amount_raw, "total_amount")?,
        tax_amount: capture_after(text, &["税额", "税  额"], AMOUNT_PATTERN)
            .map(|raw| parse_amount(&raw, "tax_amount"))
            .transpose()?,
        tax_rate: capture_after(text, &["税率"], r"\d+\.?\d*%?")
            .map(|raw| crate::xml::parse_tax_rate(&raw))
            .transpose()?,
        buyer_name: capture_after(text, &["购买方名称", "购  买  方"], r"\S+"),
        seller_name: capture_after(text, &["销售方名称", "销  售  方"], r"\S+"),
        ticket_type: TicketType::Other,
        parse_level: ParseLevel::L1,
        confidence: 1.0,
        source_path: path.to_path_buf(),
    })
}
```

`Lazy` 和 `PathBuf` 若编译器提示未使用，删掉对应 import。

- [ ] **Step 5: 运行测试确认通过**

Run: `cargo test -p invoice-parse pdf`
Expected: 6 个测试全部 PASS

- [ ] **Step 6: 加 dump-pdf 子命令**

在 `main.rs` 的 `match` 追加：

```rust
        Some("dump-pdf") => {
            let path = args.get(2).context("用法: invoice-parse dump-pdf <file.pdf>")?;
            let bytes = std::fs::read(&path)?;
            println!("有文本层: {}", invoice_parse::pdf::has_text_layer(&bytes));
            println!("--- 文本层内容 ---");
            println!("{}", invoice_parse::pdf::extract_text(&bytes, Path::new(path))?);
            Ok(())
        }
```

需在 `main.rs` 顶部加 `use std::path::Path;`。

- [ ] **Step 7: 对真实 PDF 样本验证**

对每张 PDF 样本运行：

Run: `cargo run -p invoice-parse -- dump-pdf fixtures/samples/<行程单>.pdf`

**这一步决定 L1 的真实可行性**。观察两件事：

1. **CJK 字符是否正常**。若中文显示为乱码或空白，说明 `pdf-extract` 的 CJK 字体处理有问题——这是已知的 PDF 提取难点。此时记入验证报告，并尝试备选 crate（`pdfplumber-rs`）。
2. **关键词是否出现在文本里**。若"票价""开票日期"这些锚点不在提取结果中（可能被拆成单字），当前的正则策略失效，需改为坐标定位方案。

若锚点存在但正则没匹配上，调整 `capture_after` 的标签候选列表——真实版式的标签可能带全角空格或换行。

- [ ] **Step 8: Commit**

```bash
git add crates/invoice-parse/src/pdf.rs crates/invoice-parse/src/lib.rs \
        crates/invoice-parse/src/main.rs crates/invoice-parse/Cargo.toml
git commit -m "feat: parse rail/flight/VAT invoices from PDF text layer"
```

---

## Task 7: 本地 OCR 字段定位（L2）——最高风险任务

**Files:**
- Create: `crates/invoice-parse/src/ocr.rs`
- Modify: `crates/invoice-parse/src/lib.rs`
- Modify: `crates/invoice-parse/Cargo.toml`
- Create: `models/README.md`
- Test: `crates/invoice-parse/src/ocr.rs`（inline `#[cfg(test)]`）

**Interfaces:**
- Consumes: `xml::{parse_date, parse_amount, parse_tax_rate}`（Task 4）、`model::*`
- Produces:
  - `ocr::TextBox { text: String, x: f32, y: f32, width: f32, height: f32, confidence: f32 }`
  - `ocr::locate_vat_fields(boxes: &[TextBox], path: &Path) -> Result<ParsedInvoice, ParseError>`
  - `ocr::OcrEngine::new(model_dir: &Path) -> anyhow::Result<OcrEngine>`
  - `ocr::OcrEngine::recognize(&self, image_bytes: &[u8]) -> anyhow::Result<Vec<TextBox>>`

**这个任务的性质与前面不同**：前面是实现已知逻辑，这里是验证未知能力。所以拆成两半：

1. **字段定位器**（Step 1–5）：纯函数，输入 `Vec<TextBox>`，输出 `ParsedInvoice`。用合成数据测，**与 OCR 引擎无关**
2. **引擎集成**（Step 6–8）：接真实 OCR crate，可能失败

这样拆的理由：即使引擎不达标要走 sidecar 兜底，字段定位逻辑仍然可复用（sidecar 只负责出 `TextBox`，定位逻辑留在 Rust 侧）。**不会白做。**

- [ ] **Step 1: 写字段定位器的失败测试**

创建 `crates/invoice-parse/src/ocr.rs`：

```rust
use crate::model::{ParseError, ParseLevel, ParsedInvoice, TicketType};
use crate::xml::{parse_amount, parse_date, parse_tax_rate};
use std::path::Path;

/// OCR 识别出的一个文本框。坐标为左上角原点的像素值。
#[derive(Debug, Clone, PartialEq)]
pub struct TextBox {
    pub text: String,
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub confidence: f32,
}

impl TextBox {
    fn center_y(&self) -> f32 {
        self.y + self.height / 2.0
    }
    fn right(&self) -> f32 {
        self.x + self.width
    }
}

/// 从 OCR 文本框中定位增值税发票字段。
///
/// 两种版式都要支持：
/// - 标签与值在同一个框内（"发票号码 12345"）
/// - 标签与值是相邻的两个框（"发票号码" | "12345"）
pub fn locate_vat_fields(boxes: &[TextBox], path: &Path) -> Result<ParsedInvoice, ParseError> {
    unimplemented!()
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal::prelude::FromStr;
    use rust_decimal::Decimal;

    fn tb(text: &str, x: f32, y: f32, conf: f32) -> TextBox {
        TextBox {
            text: text.to_string(),
            x,
            y,
            width: text.chars().count() as f32 * 12.0,
            height: 20.0,
            confidence: conf,
        }
    }

    /// 版式 A：标签和值在同一个框
    fn inline_layout() -> Vec<TextBox> {
        vec![
            tb("发票号码 24312000000012345678", 400.0, 40.0, 0.97),
            tb("开票日期 2026年07月03日", 400.0, 70.0, 0.95),
            tb("价税合计 ￥1280.00", 400.0, 300.0, 0.96),
            tb("税额 ￥72.45", 500.0, 260.0, 0.93),
            tb("税率 6%", 400.0, 260.0, 0.94),
        ]
    }

    /// 版式 B：标签和值是相邻的两个框（同一行）
    fn adjacent_layout() -> Vec<TextBox> {
        vec![
            tb("发票号码", 400.0, 40.0, 0.97),
            tb("24312000000012345678", 520.0, 42.0, 0.96),
            tb("开票日期", 400.0, 70.0, 0.95),
            tb("2026-07-03", 520.0, 71.0, 0.94),
            tb("价税合计", 400.0, 300.0, 0.96),
            tb("￥1280.00", 520.0, 301.0, 0.92),
        ]
    }

    #[test]
    fn locates_fields_in_inline_layout() {
        let invoice = locate_vat_fields(&inline_layout(), Path::new("a.jpg")).unwrap();
        assert_eq!(invoice.invoice_number, "24312000000012345678");
        assert_eq!(invoice.issue_date.to_string(), "2026-07-03");
        assert_eq!(invoice.total_amount, Decimal::from_str("1280.00").unwrap());
        assert_eq!(invoice.tax_amount, Some(Decimal::from_str("72.45").unwrap()));
        assert_eq!(invoice.tax_rate, Some(Decimal::from_str("0.06").unwrap()));
        assert_eq!(invoice.parse_level, ParseLevel::L2);
    }

    #[test]
    fn locates_fields_in_adjacent_layout() {
        let invoice = locate_vat_fields(&adjacent_layout(), Path::new("b.jpg")).unwrap();
        assert_eq!(invoice.invoice_number, "24312000000012345678");
        assert_eq!(invoice.issue_date.to_string(), "2026-07-03");
        assert_eq!(invoice.total_amount, Decimal::from_str("1280.00").unwrap());
    }

    #[test]
    fn confidence_is_minimum_across_used_boxes() {
        // 整张票的可信度由最弱的字段决定——一个字段错了整张就不能用
        let invoice = locate_vat_fields(&inline_layout(), Path::new("a.jpg")).unwrap();
        assert!(
            (invoice.confidence - 0.93).abs() < 0.001,
            "应取最低置信度 0.93，实际 {}",
            invoice.confidence
        );
    }

    #[test]
    fn ignores_label_box_on_a_different_line() {
        // "价税合计" 在 y=300，候选值在 y=500（不同行），不应被采用
        let boxes = vec![
            tb("发票号码", 400.0, 40.0, 0.9),
            tb("111", 520.0, 42.0, 0.9),
            tb("开票日期", 400.0, 70.0, 0.9),
            tb("2026-07-03", 520.0, 71.0, 0.9),
            tb("价税合计", 400.0, 300.0, 0.9),
            tb("￥999.00", 520.0, 500.0, 0.9),
        ];
        let err = locate_vat_fields(&boxes, Path::new("c.jpg")).unwrap_err();
        assert!(err.to_string().contains("total_amount"), "实际: {err}");
    }

    #[test]
    fn missing_invoice_number_reports_field() {
        let boxes = vec![
            tb("开票日期 2026-07-03", 400.0, 70.0, 0.9),
            tb("价税合计 ￥100.00", 400.0, 300.0, 0.9),
        ];
        let err = locate_vat_fields(&boxes, Path::new("d.jpg")).unwrap_err();
        assert!(err.to_string().contains("invoice_number"), "实际: {err}");
    }
}
```

在 `lib.rs` 追加 `pub mod ocr;`。

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo test -p invoice-parse ocr`
Expected: FAIL，panic 于 `not implemented`

- [ ] **Step 3: 实现字段定位器**

替换 `ocr.rs` 里的 `unimplemented!()`：

```rust
/// 同一行的判定阈值：两个框的垂直中心相差不超过这个像素数
const SAME_LINE_TOLERANCE: f32 = 15.0;

/// 在文本框集合中查找某个字段的值。
///
/// 先试同框内提取（标签后面就是值），失败则找同一行、位于标签右侧、
/// 且水平距离最近的框。
///
/// 返回 (值文本, 该值所在框的置信度)
fn find_value(
    boxes: &[TextBox],
    labels: &[&str],
    validate: impl Fn(&str) -> bool,
) -> Option<(String, f32)> {
    for label in labels {
        for b in boxes.iter().filter(|b| b.text.contains(label)) {
            // 情况 1：标签和值在同一个框
            if let Some(rest) = b.text.split(label).nth(1) {
                let candidate = rest.trim_start_matches([' ', '：', ':', '\u{3000}']).trim();
                if !candidate.is_empty() && validate(candidate) {
                    return Some((candidate.to_string(), b.confidence));
                }
            }

            // 情况 2：值在同一行的右邻框
            let mut right_neighbors: Vec<&TextBox> = boxes
                .iter()
                .filter(|other| {
                    (other.center_y() - b.center_y()).abs() <= SAME_LINE_TOLERANCE
                        && other.x >= b.right() - 1.0
                        && !std::ptr::eq(*other, b)
                })
                .collect();
            right_neighbors.sort_by(|p, q| p.x.partial_cmp(&q.x).unwrap());

            for n in right_neighbors {
                let candidate = n.text.trim();
                if !candidate.is_empty() && validate(candidate) {
                    return Some((candidate.to_string(), n.confidence));
                }
            }
        }
    }
    None
}

fn looks_like_digits(s: &str) -> bool {
    let digits = s.chars().filter(|c| c.is_ascii_digit()).count();
    digits >= 8
}

fn looks_like_date(s: &str) -> bool {
    let digits = s.chars().filter(|c| c.is_ascii_digit()).count();
    (6..=8).contains(&digits)
}

fn looks_like_amount(s: &str) -> bool {
    s.chars().any(|c| c.is_ascii_digit())
        && s.chars()
            .all(|c| c.is_ascii_digit() || "￥¥,. ".contains(c))
}

fn looks_like_rate(s: &str) -> bool {
    s.contains('%') || s.chars().any(|c| c.is_ascii_digit())
}

fn any_text(_s: &str) -> bool {
    true
}

pub fn locate_vat_fields(boxes: &[TextBox], path: &Path) -> Result<ParsedInvoice, ParseError> {
    let missing = |field: &str| ParseError::MissingField {
        path: path.to_path_buf(),
        field: field.to_string(),
    };

    let (number_raw, c1) = find_value(boxes, &["发票号码", "发票号"], looks_like_digits)
        .ok_or_else(|| missing("invoice_number"))?;
    let (date_raw, c2) =
        find_value(boxes, &["开票日期"], looks_like_date).ok_or_else(|| missing("issue_date"))?;
    let (amount_raw, c3) = find_value(
        boxes,
        &["价税合计", "合计金额", "小写"],
        looks_like_amount,
    )
    .ok_or_else(|| missing("total_amount"))?;

    let tax = find_value(boxes, &["税额"], looks_like_amount);
    let rate = find_value(boxes, &["税率"], looks_like_rate);
    let buyer = find_value(boxes, &["购买方名称", "购买方"], any_text);
    let seller = find_value(boxes, &["销售方名称", "销售方"], any_text);

    // 整张票的置信度取所有实际采用的框的最小值——
    // 一个字段错了，整张票就不能直接用
    let mut confidences = vec![c1, c2, c3];
    confidences.extend([&tax, &rate].iter().filter_map(|o| o.as_ref().map(|(_, c)| *c)));
    let confidence = confidences.iter().copied().fold(f32::INFINITY, f32::min);

    Ok(ParsedInvoice {
        invoice_number: number_raw.chars().filter(|c| c.is_ascii_digit()).collect(),
        issue_date: parse_date(&date_raw)?,
        total_amount: parse_amount(&amount_raw, "total_amount")?,
        tax_amount: tax
            .map(|(raw, _)| parse_amount(&raw, "tax_amount"))
            .transpose()?,
        tax_rate: rate.map(|(raw, _)| parse_tax_rate(&raw)).transpose()?,
        buyer_name: buyer.map(|(raw, _)| raw),
        seller_name: seller.map(|(raw, _)| raw),
        ticket_type: TicketType::Other,
        parse_level: ParseLevel::L2,
        confidence,
        source_path: path.to_path_buf(),
    })
}
```

- [ ] **Step 4: 运行测试确认通过**

Run: `cargo test -p invoice-parse ocr`
Expected: 5 个测试全部 PASS

- [ ] **Step 5: Commit（此时字段定位已独立可用）**

```bash
git add crates/invoice-parse/src/ocr.rs crates/invoice-parse/src/lib.rs
git commit -m "feat: locate VAT invoice fields from OCR text boxes"
```

**这次提交的价值不依赖 OCR 引擎是否达标**——即使后面走 sidecar，这段逻辑照样用。

- [ ] **Step 6: 接入 OCR 引擎**

**这一步是本计划唯一无法预先写出完整实现的地方**，因为 `OcrEngine::new` / `recognize` 的函数体取决于哪个 crate 能编译并跑通。按下表顺序尝试，用第一个成功的：

| 顺序 | Crate | 推理后端 | 选它的理由 |
|---|---|---|---|
| 1 | [`paddle-ocr-rs`](https://github.com/mg-chao/paddle-ocr-rs) | ONNX Runtime | 有明确的 GitHub 仓库和用法说明 |
| 2 | [`ocr-rs`](https://lib.rs/crates/ocr-rs) | MNN | 不依赖 ONNX Runtime 动态库，打包更简单 |
| 3 | [`ort`](https://lib.rs/crates/ort) + 手写前后处理 | ONNX Runtime | 完全可控，但要自己实现 DBNet 后处理 |

在 `crates/invoice-parse/Cargo.toml` 追加（版本号以 crates.io 上的最新稳定版为准）：

```toml
image = "0.25"
# 下面这行按上表选一个
paddle-ocr-rs = "*"
```

在 `ocr.rs` 追加引擎封装。**契约是固定的**——无论用哪个 crate，都要把它的输出映射成 `Vec<TextBox>`：

```rust
pub struct OcrEngine {
    inner: /* 所选 crate 的引擎类型 */,
}

impl OcrEngine {
    /// model_dir 需包含检测模型、识别模型、字典文件。
    /// 具体文件名见所选 crate 的文档。
    pub fn new(model_dir: &Path) -> anyhow::Result<Self> {
        // 按所选 crate 的 API 加载模型
        todo!("按 Step 6 表格选定的 crate 实现")
    }

    /// 识别图片，返回文本框列表。
    ///
    /// 映射契约（务必逐项对齐，坐标单位是像素、左上原点）：
    ///   text       ← 识别出的文本
    ///   x, y       ← 检测框左上角
    ///   width      ← 检测框宽
    ///   height     ← 检测框高
    ///   confidence ← 识别置信度，归一化到 0.0–1.0
    ///
    /// 若 crate 给的是四点多边形，取外接矩形。
    /// 若 crate 不给置信度，填 0.5 并在验证报告里注明
    /// —— 置信度缺失意味着无法做低分人工复核路由，这是重要缺陷。
    pub fn recognize(&self, image_bytes: &[u8]) -> anyhow::Result<Vec<TextBox>> {
        todo!("按 Step 6 表格选定的 crate 实现")
    }
}
```

创建 `models/README.md`：

```markdown
# OCR 模型文件

本目录存放 PaddleOCR 的 ONNX 导出模型，**不提交到仓库**（体积大）。

需要三个文件（具体名称见所选 crate 文档）：

- 检测模型：`ch_PP-OCRv4_det_infer.onnx`
- 识别模型：`ch_PP-OCRv4_rec_infer.onnx`
- 字典文件：`ppocr_keys_v1.txt`

下载来源：PaddleOCR 官方模型库，或所选 crate README 提供的转换好的 ONNX 版本。

下载后记录实际文件体积到验证报告——它直接决定安装包能否控制在 30MB 内。
```

在 `.gitignore` 追加：

```
/models/*.onnx
/models/*.txt
```

- [ ] **Step 7: 对真实图片样本验证**

在 `main.rs` 的 `match` 追加：

```rust
        Some("ocr") => {
            let path = args.get(2).context("用法: invoice-parse ocr <image>")?;
            let model_dir = PathBuf::from("models");
            let engine = invoice_parse::ocr::OcrEngine::new(&model_dir)?;
            let bytes = std::fs::read(path)?;
            let boxes = engine.recognize(&bytes)?;

            println!("识别到 {} 个文本框：", boxes.len());
            for b in &boxes {
                println!("  [{:.2}] ({:>5.0},{:>5.0}) {}", b.confidence, b.x, b.y, b.text);
            }

            match invoice_parse::ocr::locate_vat_fields(&boxes, Path::new(path)) {
                Ok(inv) => println!("\n定位结果:\n{inv:#?}"),
                Err(e) => println!("\n字段定位失败: {e}"),
            }
            Ok(())
        }
```

对全部 10 张图片样本（5 扫描件 + 5 照片）逐个运行：

Run: `cargo run -p invoice-parse -- ocr fixtures/samples/<图片>`

**记录三项数据**：

| 数据 | 怎么算 | 门槛 |
|---|---|---|
| 金额准确率 | 金额与清单期望一致的张数 ÷ 10 | **≥ 90%** |
| 税额准确率 | 税额一致的张数 ÷ 有税额的张数 | ≥ 90% |
| 置信度区分度 | 错误样本的置信度是否显著低于正确样本 | 能区分 |

第三项最关键：如果错误样本的置信度和正确样本差不多，说明**置信度不可用于人工复核路由**，那么 L2 的所有结果都得人工核对——这会让审核时间从 5 分钟涨到 15 分钟，产品价值大幅缩水。

- [ ] **Step 8: 判定与兜底决策**

按 Step 7 的结果决定：

| 结果 | 决定 |
|---|---|
| 三项都达标 | **纯 Rust 成立**，记入报告 |
| 准确率不足但 > 80% | 加图像预处理（灰度、自适应二值化、倾斜校正）后重测 |
| 换 crate 后仍不达标 | **触发 sidecar 兜底**：仅 OCR 走 Python，其余保持 Rust。报告中记录预计包体增量 |
| 置信度不可用 | 记为重大缺陷。即使准确率达标也要在报告中标注"L2 结果需全量人工核对" |

- [ ] **Step 9: Commit**

```bash
git add crates/invoice-parse/src/ocr.rs crates/invoice-parse/src/main.rs \
        crates/invoice-parse/Cargo.toml models/README.md .gitignore
git commit -m "feat: integrate local OCR engine for VAT invoice images"
```

---

## Task 8: SM2/SM3 本地验签

**Files:**
- Create: `crates/invoice-parse/src/verify.rs`
- Modify: `crates/invoice-parse/src/lib.rs`
- Modify: `crates/invoice-parse/src/main.rs`
- Modify: `crates/invoice-parse/Cargo.toml`
- Test: `crates/invoice-parse/src/verify.rs`（inline `#[cfg(test)]`）

**Interfaces:**
- Consumes: `ofd::list_entries`（Task 5）、`model::ParseError`
- Produces:
  - `verify::SignatureStatus { Valid, Invalid { reason: String }, NotSigned }`
  - `verify::locate_signature(ofd_bytes: &[u8]) -> Result<Option<SignatureData>, ParseError>`
  - `verify::verify_ofd_signature(ofd_bytes: &[u8], path: &Path) -> Result<SignatureStatus, ParseError>`

**为什么这项必须验证**：MVP 不做付费查验（¥0.30/次），前提是数电票能**本地验签**。如果本地验签走不通，防伪能力就只剩"什么都没有"，那付费查验就得从 v0.5 提到 MVP。这一项的结论直接影响 MVP 范围。

**同时要诚实记录一个已知局限**：本地验签能证明"这张票是税局签发的、内容未被篡改"，但**证明不了"这张票没被作废/红冲"**——作废票的签章依然有效。这正是付费查验在 v0.5 存在的理由。

- [ ] **Step 1: 加依赖**

在 `crates/invoice-parse/Cargo.toml` 的 `[dependencies]` 追加：

```toml
smcrypto = "0.3"
```

若 `smcrypto` 的 API 不合用，备选 [`gm-rs`](https://github.com/CrayfishGo/gm-rs)。两者都实现 SM2 验签与 SM3 摘要。

- [ ] **Step 2: 写失败测试**

创建 `crates/invoice-parse/src/verify.rs`：

```rust
use crate::model::ParseError;
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq)]
pub enum SignatureStatus {
    /// 签章验证通过：内容未被篡改，且由可信主体签发
    Valid,
    /// 签章存在但验证失败
    Invalid { reason: String },
    /// 容器内没有签章文件（如纯版式 OFD、非数电票）
    NotSigned,
}

/// 从 OFD 容器中提取的签章原始数据。
#[derive(Debug, Clone)]
pub struct SignatureData {
    /// 签章文件在容器内的路径
    pub entry_name: String,
    /// 签章文件原始字节（含 SES_Signature 结构）
    pub raw: Vec<u8>,
}

/// 在 OFD 容器中定位签章文件。
/// 数电票的签章通常位于 `Doc_0/Signs/Sign_0/SignedValue.dat`
/// 或 `Doc_0/Signs/Signatures.xml` 指向的文件。
pub fn locate_signature(ofd_bytes: &[u8]) -> Result<Option<SignatureData>, ParseError> {
    unimplemented!()
}

/// 验证 OFD 的数字签章。
pub fn verify_ofd_signature(
    ofd_bytes: &[u8],
    path: &Path,
) -> Result<SignatureStatus, ParseError> {
    unimplemented!()
}

/// 签章文件的候选路径特征（不区分大小写）
const SIGNATURE_HINTS: &[&str] = &["signedvalue.dat", "signature.dat", "/signs/", "seal.dat"];

fn looks_like_signature(entry_name: &str) -> bool {
    let lower = entry_name.to_lowercase();
    SIGNATURE_HINTS.iter().any(|h| lower.contains(h))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use zip::write::SimpleFileOptions;

    fn build_ofd(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut buf = Vec::new();
        {
            let mut zip = zip::ZipWriter::new(Cursor::new(&mut buf));
            for (name, content) in entries {
                zip.start_file(*name, SimpleFileOptions::default()).unwrap();
                zip.write_all(content).unwrap();
            }
            zip.finish().unwrap();
        }
        buf
    }

    #[test]
    fn finds_signature_by_path_hint() {
        let ofd = build_ofd(&[
            ("OFD.xml", b"<OFD/>"),
            ("Doc_0/Signs/Sign_0/SignedValue.dat", b"fake-signature-bytes"),
        ]);
        let found = locate_signature(&ofd).unwrap().expect("应找到签章");
        assert_eq!(found.entry_name, "Doc_0/Signs/Sign_0/SignedValue.dat");
        assert_eq!(found.raw, b"fake-signature-bytes");
    }

    #[test]
    fn unsigned_container_returns_none() {
        let ofd = build_ofd(&[("OFD.xml", b"<OFD/>"), ("Doc_0/Document.xml", b"<Doc/>")]);
        assert!(locate_signature(&ofd).unwrap().is_none());
    }

    #[test]
    fn unsigned_container_reports_not_signed() {
        let ofd = build_ofd(&[("OFD.xml", b"<OFD/>")]);
        let status = verify_ofd_signature(&ofd, Path::new("x.ofd")).unwrap();
        assert_eq!(status, SignatureStatus::NotSigned);
    }

    #[test]
    fn garbage_signature_is_invalid_not_panic() {
        // 关键：无效签章必须返回 Invalid，不能 panic 也不能误判 Valid
        let ofd = build_ofd(&[
            ("OFD.xml", b"<OFD/>"),
            ("Doc_0/invoice.xml", b"<Invoice><Fphm>1</Fphm></Invoice>"),
            ("Doc_0/Signs/Sign_0/SignedValue.dat", b"not-a-real-signature"),
        ]);
        let status = verify_ofd_signature(&ofd, Path::new("x.ofd")).unwrap();
        assert!(
            matches!(status, SignatureStatus::Invalid { .. }),
            "垃圾签章应判 Invalid，实际 {status:?}"
        );
    }

    #[test]
    fn non_zip_input_errors() {
        let err = locate_signature(b"not a zip").unwrap_err();
        assert!(matches!(err, ParseError::MalformedFormat { .. }));
    }
}
```

在 `lib.rs` 追加 `pub mod verify;`。

- [ ] **Step 3: 运行测试确认失败**

Run: `cargo test -p invoice-parse verify`
Expected: FAIL，panic 于 `not implemented`

- [ ] **Step 4: 实现定位与验签**

替换 `verify.rs` 的两处 `unimplemented!()`：

```rust
pub fn locate_signature(ofd_bytes: &[u8]) -> Result<Option<SignatureData>, ParseError> {
    let mut archive = zip::ZipArchive::new(Cursor::new(ofd_bytes.to_vec())).map_err(|e| {
        ParseError::MalformedFormat {
            path: PathBuf::new(),
            format: "OFD",
            detail: format!("不是有效的 ZIP 容器: {e}"),
        }
    })?;

    let mut hit: Option<usize> = None;
    for i in 0..archive.len() {
        let entry = archive.by_index(i).map_err(|e| ParseError::MalformedFormat {
            path: PathBuf::new(),
            format: "OFD",
            detail: format!("读取第 {i} 个条目失败: {e}"),
        })?;
        if looks_like_signature(entry.name()) && !entry.name().ends_with('/') {
            hit = Some(i);
            break;
        }
    }

    let Some(index) = hit else { return Ok(None) };

    let mut entry = archive.by_index(index).map_err(|e| ParseError::MalformedFormat {
        path: PathBuf::new(),
        format: "OFD",
        detail: format!("打开签章文件失败: {e}"),
    })?;
    let entry_name = entry.name().to_string();
    let mut raw = Vec::new();
    entry.read_to_end(&mut raw).map_err(|e| ParseError::Io {
        path: PathBuf::from(&entry_name),
        source: e,
    })?;

    Ok(Some(SignatureData { entry_name, raw }))
}

pub fn verify_ofd_signature(
    ofd_bytes: &[u8],
    path: &Path,
) -> Result<SignatureStatus, ParseError> {
    let Some(sig) = locate_signature(ofd_bytes)? else {
        return Ok(SignatureStatus::NotSigned);
    };

    // 被签名的数据。按 GB/T 33190 规范，签章覆盖 Signature.xml 所引用的
    // 各文件摘要；MVP 阶段简化为验证内嵌发票 XML —— 这正是我们要保护的内容。
    // 若 Step 5 的真实样本验签失败，按规范扩展这里的数据范围。
    let signed_payload = match crate::ofd::extract_invoice_xml(ofd_bytes, path) {
        Ok(xml) => xml,
        Err(_) => {
            return Ok(SignatureStatus::Invalid {
                reason: "容器有签章但找不到被签名的发票 XML".to_string(),
            })
        }
    };

    match extract_sm2_parts(&sig.raw) {
        None => Ok(SignatureStatus::Invalid {
            reason: format!(
                "签章文件 {} 不是可识别的 SES_Signature 结构（{} 字节）",
                sig.entry_name,
                sig.raw.len()
            ),
        }),
        Some((public_key, signature)) => {
            let ok = sm2_verify(&public_key, &signed_payload, &signature);
            if ok {
                Ok(SignatureStatus::Valid)
            } else {
                Ok(SignatureStatus::Invalid {
                    reason: "SM2 签名验证不通过（内容可能被篡改）".to_string(),
                })
            }
        }
    }
}

/// 从 SES_Signature（ASN.1 DER）中取出签发者公钥与签名值。
///
/// 返回 None 表示结构无法识别 —— 此时判 Invalid 而非 panic。
/// Step 5 会用真实样本确认这里的解析是否需要按 GB/T 38540 细化。
fn extract_sm2_parts(raw: &[u8]) -> Option<(Vec<u8>, Vec<u8>)> {
    // SES_Signature 是 DER 编码的 SEQUENCE，最外层标签为 0x30。
    // 明显不是 DER 的输入直接判定无法识别。
    if raw.first() != Some(&0x30) || raw.len() < 64 {
        return None;
    }
    // 真实结构解析在 Step 5 用样本对照后补全：
    // SES_Signature ::= SEQUENCE { toSign TBS_Sign, cert, signatureAlgID, signature BIT STRING }
    // 需要取出 cert 中的 SM2 公钥点和末尾的 signature。
    None
}

/// SM2 验签。公钥为未压缩点（0x04 || X || Y），或裸 X||Y。
fn sm2_verify(public_key: &[u8], data: &[u8], signature: &[u8]) -> bool {
    let key_hex = hex_encode(public_key);
    let ctx = smcrypto::sm2::Verify::new(&key_hex);
    ctx.verify(data, signature)
}

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
```

`extract_sm2_parts` 现在恒返回 `None`，所以有签章的样本会判 `Invalid`。**这是刻意的**：Step 5 用真实样本对照后才能确定 DER 结构的偏移。测试用例 `garbage_signature_is_invalid_not_panic` 现在就能通过，因为它要求的正是"不 panic、不误判 Valid"。

- [ ] **Step 5: 运行测试确认通过**

Run: `cargo test -p invoice-parse verify`
Expected: 5 个测试全部 PASS

- [ ] **Step 6: 加 verify 子命令并对真实样本验证**

在 `main.rs` 的 `match` 追加：

```rust
        Some("verify") => {
            let path = args.get(2).context("用法: invoice-parse verify <file.ofd>")?;
            let bytes = std::fs::read(path)?;

            match invoice_parse::verify::locate_signature(&bytes)? {
                None => println!("容器内未找到签章文件"),
                Some(sig) => {
                    println!("签章文件: {} （{} 字节）", sig.entry_name, sig.raw.len());
                    println!("前 32 字节: {:02x?}", &sig.raw[..sig.raw.len().min(32)]);
                }
            }
            println!(
                "验签结果: {:?}",
                invoice_parse::verify::verify_ofd_signature(&bytes, Path::new(path))?
            );
            Ok(())
        }
```

对每张 OFD 样本运行：

Run: `cargo run -p invoice-parse -- verify fixtures/samples/<OFD>.ofd`

用打印出的前 32 字节对照 DER 结构，补全 `extract_sm2_parts`。三种可能结果：

| 观察 | 含义 | 处理 |
|---|---|---|
| 补全后验签通过 | **本地验签成立**，MVP 不需付费查验 | 记入报告 |
| 找到签章但验不过 | 被签名数据范围不对 | 按 GB/T 33190 扩展 `signed_payload` 的范围重试 |
| 样本无签章 | 这类票无法本地验真 | **记入报告的覆盖缺口**，可能需把付费查验提到 MVP |

若无法拿到作废票样本，在报告中明确写："本地验签无法识别作废/红冲票，且本次未取得负例样本验证，付费查验是否需提前至 MVP 缺少依据。"

- [ ] **Step 7: Commit**

```bash
git add crates/invoice-parse/src/verify.rs crates/invoice-parse/src/lib.rs \
        crates/invoice-parse/src/main.rs crates/invoice-parse/Cargo.toml
git commit -m "feat: verify OFD SM2 signatures locally"
```

---

## Task 9: 全量验证与报告生成

**Files:**
- Create: `crates/invoice-parse/src/report.rs`
- Modify: `crates/invoice-parse/src/lib.rs`
- Modify: `crates/invoice-parse/src/main.rs`
- Create: `docs/spike-report.md`（由命令生成后手工补结论）
- Test: `crates/invoice-parse/src/report.rs`（inline `#[cfg(test)]`）

**Interfaces:**
- Consumes: `manifest::{Manifest, Sample, FieldComparison}`（Task 2）、四个解析器（Task 4–7）、`verify::SignatureStatus`（Task 8）
- Produces:
  - `report::SampleOutcome { path, format, result: OutcomeKind }`
  - `report::OutcomeKind { FullMatch, PartialMatch { failures: Vec<FieldComparison> }, ParseFailed { error: String } }`
  - `report::render_markdown(outcomes: &[SampleOutcome]) -> String`

- [ ] **Step 1: 写失败测试**

创建 `crates/invoice-parse/src/report.rs`：

```rust
use crate::manifest::FieldComparison;

#[derive(Debug, Clone)]
pub struct SampleOutcome {
    pub path: String,
    pub format: String,
    pub result: OutcomeKind,
}

#[derive(Debug, Clone)]
pub enum OutcomeKind {
    FullMatch,
    PartialMatch { failures: Vec<FieldComparison> },
    ParseFailed { error: String },
}

impl SampleOutcome {
    pub fn passed(&self) -> bool {
        matches!(self.result, OutcomeKind::FullMatch)
    }
}

/// 生成 Markdown 验证报告。
pub fn render_markdown(outcomes: &[SampleOutcome]) -> String {
    unimplemented!()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pass(path: &str, format: &str) -> SampleOutcome {
        SampleOutcome {
            path: path.into(),
            format: format.into(),
            result: OutcomeKind::FullMatch,
        }
    }

    fn fail(path: &str, format: &str, field: &'static str) -> SampleOutcome {
        SampleOutcome {
            path: path.into(),
            format: format.into(),
            result: OutcomeKind::PartialMatch {
                failures: vec![FieldComparison {
                    field,
                    expected: "553.00".into(),
                    actual: "12.80".into(),
                    matched: false,
                }],
            },
        }
    }

    #[test]
    fn groups_pass_rate_by_format() {
        let outcomes = vec![
            pass("a.xml", "xml"),
            pass("b.xml", "xml"),
            fail("c.ofd", "ofd", "total_amount"),
            pass("d.ofd", "ofd"),
        ];
        let md = render_markdown(&outcomes);

        assert!(md.contains("| xml | 2 | 2 | 100.0% |"), "实际输出:\n{md}");
        assert!(md.contains("| ofd | 2 | 1 | 50.0% |"), "实际输出:\n{md}");
    }

    #[test]
    fn lists_failed_fields_with_expected_and_actual() {
        let md = render_markdown(&[fail("c.ofd", "ofd", "total_amount")]);
        assert!(md.contains("c.ofd"));
        assert!(md.contains("total_amount"));
        assert!(md.contains("553.00"));
        assert!(md.contains("12.80"));
    }

    #[test]
    fn reports_parse_failures_separately_from_field_mismatches() {
        let outcomes = vec![SampleOutcome {
            path: "broken.ofd".into(),
            format: "ofd".into(),
            result: OutcomeKind::ParseFailed {
                error: "找不到内嵌的发票 XML".into(),
            },
        }];
        let md = render_markdown(&outcomes);
        assert!(md.contains("解析失败"), "实际输出:\n{md}");
        assert!(md.contains("找不到内嵌的发票 XML"));
    }

    #[test]
    fn all_passing_run_states_so_explicitly() {
        let md = render_markdown(&[pass("a.xml", "xml")]);
        assert!(md.contains("全部通过"), "实际输出:\n{md}");
    }

    #[test]
    fn empty_run_does_not_divide_by_zero() {
        let md = render_markdown(&[]);
        assert!(md.contains("无样本"), "实际输出:\n{md}");
    }
}
```

在 `lib.rs` 追加 `pub mod report;`。

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo test -p invoice-parse report`
Expected: FAIL，panic 于 `not implemented`

- [ ] **Step 3: 实现报告渲染**

替换 `report.rs` 的 `unimplemented!()`：

```rust
use std::collections::BTreeMap;
use std::fmt::Write as _;

pub fn render_markdown(outcomes: &[SampleOutcome]) -> String {
    let mut md = String::from("# 发票解析能力验证报告\n\n");

    if outcomes.is_empty() {
        md.push_str("无样本可验证。请先按计划的「前置阻塞项」收集发票样本。\n");
        return md;
    }

    // 按格式分组统计
    let mut by_format: BTreeMap<&str, (usize, usize)> = BTreeMap::new();
    for o in outcomes {
        let entry = by_format.entry(o.format.as_str()).or_insert((0, 0));
        entry.0 += 1;
        if o.passed() {
            entry.1 += 1;
        }
    }

    md.push_str("## 通过率\n\n");
    md.push_str("| 格式 | 样本数 | 通过 | 通过率 |\n|---|---|---|---|\n");
    for (format, (total, passed)) in &by_format {
        let rate = *passed as f64 / *total as f64 * 100.0;
        let _ = writeln!(md, "| {format} | {total} | {passed} | {rate:.1}% |");
    }

    let total = outcomes.len();
    let passed = outcomes.iter().filter(|o| o.passed()).count();
    let _ = writeln!(
        md,
        "\n合计 {passed}/{total}（{:.1}%）",
        passed as f64 / total as f64 * 100.0
    );

    if passed == total {
        md.push_str("\n**全部通过。**\n");
    }

    // 字段不匹配明细
    let mismatches: Vec<&SampleOutcome> = outcomes
        .iter()
        .filter(|o| matches!(o.result, OutcomeKind::PartialMatch { .. }))
        .collect();

    if !mismatches.is_empty() {
        md.push_str("\n## 字段不匹配\n\n");
        md.push_str("| 样本 | 字段 | 期望 | 实际 |\n|---|---|---|---|\n");
        for o in mismatches {
            if let OutcomeKind::PartialMatch { failures } = &o.result {
                for f in failures {
                    let _ = writeln!(
                        md,
                        "| {} | {} | {} | {} |",
                        o.path, f.field, f.expected, f.actual
                    );
                }
            }
        }
    }

    // 解析失败明细
    let failures: Vec<&SampleOutcome> = outcomes
        .iter()
        .filter(|o| matches!(o.result, OutcomeKind::ParseFailed { .. }))
        .collect();

    if !failures.is_empty() {
        md.push_str("\n## 解析失败\n\n");
        md.push_str("| 样本 | 错误 |\n|---|---|\n");
        for o in failures {
            if let OutcomeKind::ParseFailed { error } = &o.result {
                let _ = writeln!(md, "| {} | {} |", o.path, error);
            }
        }
    }

    md.push_str(
        "\n---\n\n## 结论（手工填写）\n\n\
         ### 纯 Rust 是否可行\n\n\
         - [ ] 可行 —— 全部格式达标，按纯 Rust 推进\n\
         - [ ] 部分兜底 —— 以下能力需 Python sidecar：______，预计包体增量 ______ MB\n\
         - [ ] 不可行 —— 需重新评估 Tauri vs Electron\n\n\
         ### 覆盖缺口\n\n\
         - OCR 置信度是否可用于人工复核路由：______\n\
         - 本地验签是否成立：______\n\
         - 作废票负例是否已验证：______\n\
         - 无内嵌 XML 的 OFD 占比：______\n\n\
         ### 安装包体积实测\n\n\
         - ONNX 模型总体积：______ MB\n\
         - release 构建后的可执行文件：______ MB\n",
    );

    md
}
```

- [ ] **Step 4: 运行测试确认通过**

Run: `cargo test -p invoice-parse report`
Expected: 5 个测试全部 PASS

- [ ] **Step 5: 加 verify-all 子命令**

在 `main.rs` 的 `match` 追加：

```rust
        Some("verify-all") => verify_all(),
```

并追加函数：

```rust
fn verify_all() -> anyhow::Result<()> {
    use invoice_parse::manifest::{Manifest, TagHints};
    use invoice_parse::model::ParsedInvoice;
    use invoice_parse::report::{render_markdown, OutcomeKind, SampleOutcome};

    let manifest = Manifest::load(Path::new("fixtures/manifest.toml"))?;
    let mut outcomes = Vec::new();

    for sample in &manifest.samples {
        let full_path = PathBuf::from("fixtures").join(&sample.path);
        let hints = sample.xml_tag_hints.clone().unwrap_or(TagHints {
            invoice_number: vec![],
            issue_date: vec![],
            total_amount: vec![],
            tax_amount: vec![],
            tax_rate: vec![],
            buyer_name: vec![],
            seller_name: vec![],
        });

        let parsed: anyhow::Result<ParsedInvoice> = match sample.format.as_str() {
            "xml" => std::fs::read(&full_path)
                .map_err(anyhow::Error::from)
                .and_then(|b| {
                    invoice_parse::xml::parse_invoice_xml(
                        &b,
                        &full_path,
                        &hints,
                        sample.ticket_type,
                    )
                    .map_err(Into::into)
                }),
            "ofd" => std::fs::read(&full_path)
                .map_err(anyhow::Error::from)
                .and_then(|b| {
                    invoice_parse::ofd::parse_invoice_ofd(
                        &b,
                        &full_path,
                        &hints,
                        sample.ticket_type,
                    )
                    .map_err(Into::into)
                }),
            "pdf-rail" => parse_pdf_with(&full_path, invoice_parse::pdf::parse_rail_itinerary),
            "pdf-flight" => parse_pdf_with(&full_path, invoice_parse::pdf::parse_flight_itinerary),
            "pdf-vat" => parse_pdf_with(&full_path, invoice_parse::pdf::parse_vat_invoice_text),
            "image" => std::fs::read(&full_path)
                .map_err(anyhow::Error::from)
                .and_then(|b| {
                    let engine = invoice_parse::ocr::OcrEngine::new(Path::new("models"))?;
                    let boxes = engine.recognize(&b)?;
                    invoice_parse::ocr::locate_vat_fields(&boxes, &full_path).map_err(Into::into)
                }),
            other => Err(anyhow::anyhow!("未知格式: {other}")),
        };

        let result = match parsed {
            Ok(invoice) => {
                let comparisons = sample.compare(&invoice);
                let failures: Vec<_> =
                    comparisons.into_iter().filter(|c| !c.matched).collect();
                if failures.is_empty() {
                    OutcomeKind::FullMatch
                } else {
                    OutcomeKind::PartialMatch { failures }
                }
            }
            Err(e) => OutcomeKind::ParseFailed {
                error: e.to_string(),
            },
        };

        outcomes.push(SampleOutcome {
            path: sample.path.display().to_string(),
            format: sample.format.clone(),
            result,
        });
    }

    let md = render_markdown(&outcomes);
    std::fs::create_dir_all("docs")?;
    std::fs::write("docs/spike-report.md", &md)?;
    println!("{md}");
    println!("报告已写入 docs/spike-report.md");
    Ok(())
}

fn parse_pdf_with(
    path: &Path,
    parser: fn(&str, &Path) -> Result<invoice_parse::model::ParsedInvoice, invoice_parse::model::ParseError>,
) -> anyhow::Result<invoice_parse::model::ParsedInvoice> {
    let bytes = std::fs::read(path)?;
    let text = invoice_parse::pdf::extract_text(&bytes, path)?;
    parser(&text, path).map_err(Into::into)
}
```

清单里 PDF 样本的 `format` 需用 `pdf-rail` / `pdf-flight` / `pdf-vat` 三种值区分版式，图片用 `image`。相应更新 `fixtures/manifest.toml` 里已有条目的 `format` 值。

- [ ] **Step 6: 全量跑一次**

Run: `cargo run -p invoice-parse -- verify-all`
Expected: 打印通过率表格，并写出 `docs/spike-report.md`

各格式的门槛：

| 格式 | 门槛 | 依据 |
|---|---|---|
| xml | 100% | 结构化数据无歧义，达不到说明实现有 bug |
| ofd | ≥ 80% | 允许部分 OFD 无内嵌 XML |
| pdf-rail / pdf-flight | 100% | 版式固定，产品方案要求 > 99% |
| pdf-vat | ≥ 67% | 版式多样，失败的退 L2 OCR |
| image | ≥ 90% | 产品方案 L2 目标 92–95% |

- [ ] **Step 7: 补全报告结论并 commit**

打开 `docs/spike-report.md`，填写「结论」一节的三个部分。**这份结论是本计划的最终交付物**——它决定后续所有开发的技术栈。

测量体积：

Run: `cargo build --release -p invoice-parse && ls -lh target/release/invoice-parse && du -sh models/`

```bash
git add crates/invoice-parse/src/report.rs crates/invoice-parse/src/lib.rs \
        crates/invoice-parse/src/main.rs fixtures/manifest.toml docs/spike-report.md
git commit -m "feat: add full-corpus verification with markdown report"
```

---

## 完成标准

- [ ] `cargo test -p invoice-parse` 全绿（约 40 个测试）
- [ ] `cargo run -p invoice-parse -- verify-all` 各格式达到上表门槛
- [ ] `docs/spike-report.md` 的「结论」一节已填写完整
- [ ] 技术栈决定明确：纯 Rust / 部分 sidecar / 全 sidecar
- [ ] 若触发 sidecar 兜底，已记录预计包体增量，并注明是否需要重新评估 Tauri vs Electron

**这套测试集不是一次性的**。它直接成为解析模块的回归测试集——后续新增票种或开票平台时，只需往 `fixtures/manifest.toml` 追加样本，重跑 `verify-all`。

**计划 2（L0 地基）可在本计划 Task 1 完成后并行启动**，因为它只依赖 `model.rs`。
