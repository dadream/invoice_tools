# Parse Accuracy & Measurable Ground Truth Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make invoice parse accuracy measurable against hand-verified ground truth, then raise the verified pass rate on real invoice samples above 90%.

**Architecture:** The existing `verify-all` harness reports 0/64 because every `fixtures/manifest.toml` entry has blank expected values that `Sample::compare` scores as mismatches. Task 1 separates "parser produced a struct" from "output matched hand-verified truth" and makes blank fields mean *unverified* rather than *failed*. Tasks 2–3 populate ground truth. Tasks 4–7 fix the three real parser gaps found by inspection: OFD layout XML is never read as text+coordinates, `pdf-extract` panics on non-Identity-H CMaps, and non-invoice documents are parsed as if they were invoices. Task 8 measures the result.

**Tech Stack:** Rust 2021 (`quick-xml` 0.36, `regex` 1, `rust_decimal` 1, `chrono` 0.4, `zip` 0.6, `pdf-extract` 0.7, `serde`/`serde_json` 1), Python 3.10 sidecar (`paddleocr`, `pypdfium2`), `cargo test` / `cargo run -p invoice-parse`.

## Global Constraints

- Working directory is `/home/holo/work-tools`. `cargo` is at `$HOME/.cargo/bin/cargo`; if `cargo: command not found`, run `export PATH="$HOME/.cargo/bin:$PATH"` first.
- **No new system-level dependencies.** `java` and `pdftoppm`/`poppler` are NOT installed and MUST NOT be introduced.
- **OCR is native Rust via `ort`** (ONNX Runtime bindings) — no Python sidecar at runtime. Python is used only as an offline, one-time model-conversion tool; the shipped binary must not call it.
- **New Rust crates are limited to this list** (already fetched into the cargo registry cache, so `--offline` builds work):
  - `ort = { version = "=2.0.0-rc.13", default-features = false, features = ["std", "ndarray", "load-dynamic"] }`
  - `ndarray = "0.17"` — `ort` requires 0.17, NOT 0.16. With `default-features = false` you MUST include the `std` feature or `ndarray::NdFloat` is gated out and `ort` fails to compile.
  - `lopdf = "0.34"` — needed to build a `Document` for `pdf_extract::output_doc`.
  Any crate beyond these needs a note in the report explaining why.
- **ONNX Runtime is a vendored dynamic library, loaded at runtime.** `vendor/onnxruntime/lib/libonnxruntime.so` (16MB) is already in place, extracted from the PyPI manylinux wheel. Set `ORT_DYLIB_PATH` to its absolute path when running anything that touches OCR. Do NOT use the `download-binaries` feature: its build script fails to compile against the vendored `ureq`, and the 101MB static `libonnxruntime.a` in the cargo cache does not link on this machine — it needs glibc 2.38 (`__isoc23_strtol`) and GCC 13 (`_M_replace_cold`) while this box has glibc 2.35 / GCC 11. Add `vendor/` and `models/*.onnx` to `.gitignore`; record acquisition steps in `models/README.md`.
- Money is `rust_decimal::Decimal`, never `f32`/`f64`. Dates are `chrono::NaiveDate`.
- All comments, log output, and report text in Simplified Chinese, matching existing files.
- Never edit files under `fixtures/samples/` — they are immutable evidence.
- Ground truth in `fixtures/manifest.toml` is filled ONLY by reading the rendered document. Never paste parser output into a manifest expected-value field: that makes the test assert the bug.
- Commit after each task with Conventional Commits (`feat:`, `fix:`, `test:`, `refactor:`, `docs:`).
- `ParseLevel` semantics: `L0` structured tags, `L1` deterministic text+coordinates from the file itself, `L2` OCR pixels. OFD text extraction is **L1**, not L2. PDF positioned-text extraction is also **L1**.
- **OCR model is PP-OCRv6_small**, already downloaded, converted, and verified present:
  - `models/PP-OCRv6_small_det.onnx` (9.5 MiB), `models/PP-OCRv6_small_rec.onnx` (20.2 MiB), `models/ppocrv6_keys.txt` (18708 chars, one per line).
  - Source: `https://paddle-model-ecology.bj.bcebos.com/paddlex/official_inference_model/paddle3.0.0/PP-OCRv6_small_{det,rec}_infer.tar`, converted with `paddle2onnx --opset_version 14`. In v6 the small tier replaces v5's "mobile" naming; there is no `PP-OCRv6_mobile_*`.
  - Combined ONNX is 30 MiB, which **exceeds** the 30MB installer target in `models/README.md` on its own. Do not silently ignore this — record the real numbers in the final report and let the packaging decision (quantisation, or download-on-first-run) be made separately with data.
- **Verified pre/post-processing parameters** (read from each model's `inference.yml`; do not re-derive by guessing):
  - det input `x` is `[N,3,H,W]`, H/W dynamic and must be multiples of 32; longest side capped at 960. Normalise with mean `[0.485,0.456,0.406]`, std `[0.229,0.224,0.225]` after scaling to `1/255`, channel order RGB→CHW. Output is a single-channel probability map.
  - det post-process is DBNet: `thresh=0.2`, `box_thresh=0.45`, `unclip_ratio=1.4`, `max_candidates=3000`. The unclip step is required — without it boxes are too tight and recognition degrades badly.
  - rec input `x` is `[N,3,48,W]`, W dynamic. Resize crop to height 48 preserving aspect, normalise `(x/255 - 0.5) / 0.5`.
  - rec post-process is `CTCLabelDecode`: argmax per timestep, drop blank index 0, collapse consecutive duplicates, and map index `k` to `keys[k-1]`. Output last dim is 18710 = 18708 chars + blank + 1.
  - A working Python reference implementation of this exact pipeline is at `/tmp/ocr_ref.py`. It correctly reads `发票号码:26154000000018314746` and `价税合计(元):¥24.60` from `fixtures/samples/35-meituan-21a391a6.jpg`. Use it to cross-check the Rust port's intermediate tensors when output disagrees.

---

## File Structure

| File | Responsibility |
|---|---|
| `crates/invoice-parse/src/manifest.rs` (modify) | `Sample` expected fields become `Option<String>`; `compare` emits `Verified`/`Unverified` per field |
| `crates/invoice-parse/src/report.rs` (modify) | Report distinguishes parse-success from verified-pass; adds unverified column |
| `crates/invoice-parse/src/main.rs` (modify) | `verify-all` uses new outcome types; adds `suggest-truth` and `classify-all` subcommands |
| `crates/invoice-parse/src/ofd_text.rs` (create) | OFD `Content.xml` → `Vec<TextBox>`; mm→px scaling; fragment merging |
| `crates/invoice-parse/src/ofd.rs` (modify) | Falls back to `ofd_text` when no structured XML is found |
| `crates/invoice-parse/src/pdf.rs` (modify) | Contains the `pdf-extract` panic locally; returns a typed error |
| `crates/invoice-parse/src/classifier.rs` (create) | Decides invoice vs non-invoice before parsing |
| `crates/invoice-parse/src/ocr.rs` (modify) | `locate_vat_fields` takes a `ParseLevel`; adds `merge_line_fragments` |
| `tools/ocr_sidecar.py` (modify) | Accepts PDF input, rasterises page 1 via `pypdfium2` |
| `fixtures/manifest.toml` (modify) | Hand-verified ground truth values |

---

## Task 1: Make blank ground truth mean "unverified", not "failed"

**Files:**
- Modify: `crates/invoice-parse/src/manifest.rs:14-40` (`Sample` struct), `:76-122` (`compare`), `:126-180` (comparator helpers)
- Test: `crates/invoice-parse/src/manifest.rs` (inline `#[cfg(test)]`)

**Interfaces:**
- Consumes: `model::{ParsedInvoice, TicketType}`
- Produces:
  - `manifest::FieldComparison { field: &'static str, expected: String, actual: String, status: FieldStatus }`
  - `manifest::FieldStatus { Match, Mismatch, Unverified }`
  - `manifest::Sample` with `invoice_number/issue_date/total_amount: Option<String>` and `ticket_type: Option<TicketType>`
  - `manifest::Sample::compare(&self, actual: &ParsedInvoice) -> Vec<FieldComparison>` (unchanged signature)
  - `manifest::Sample::has_ground_truth(&self) -> bool`

**Why this task is first:** `verify-all` currently reports 0/64. Every sample has `invoice_number = ""` and `compare_str` scores `"" == "26112..."` as a mismatch, so a correct parse is indistinguishable from a wrong one. No accuracy target is measurable until this is fixed.

- [ ] **Step 1: Write the failing tests**

Replace the whole `#[cfg(test)] mod tests` block at the end of `crates/invoice-parse/src/manifest.rs` with:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;
    use rust_decimal::prelude::FromStr;

    fn parsed() -> ParsedInvoice {
        ParsedInvoice {
            invoice_number: "24312000000012345678".to_string(),
            issue_date: NaiveDate::from_ymd_opt(2026, 7, 3).unwrap(),
            total_amount: Decimal::from_str("553.00").unwrap(),
            tax_amount: Some(Decimal::from_str("50.73").unwrap()),
            tax_rate: None,
            buyer_name: Some("某某公司".to_string()),
            seller_name: None,
            ticket_type: TicketType::Rail,
            parse_level: crate::model::ParseLevel::L0,
            confidence: 1.0,
            source_path: PathBuf::from("samples/rail-01.xml"),
        }
    }

    /// 完全没填期望值的样本：每个字段都是 Unverified，一个 Mismatch 都不能有。
    /// 这是 Task 1 的核心——空清单不等于解析错误。
    fn blank_sample() -> Sample {
        Sample {
            path: PathBuf::from("samples/rail-01.xml"),
            format: "xml".to_string(),
            ticket_type: None,
            invoice_number: None,
            issue_date: None,
            total_amount: None,
            tax_amount: None,
            tax_rate: None,
            buyer_name: None,
            seller_name: None,
            is_voided: false,
            is_invoice: None,
            xml_tag_hints: None,
        }
    }

    #[test]
    fn blank_expectations_are_unverified_not_mismatched() {
        let cs = blank_sample().compare(&parsed());
        assert!(
            cs.iter().all(|c| c.status == FieldStatus::Unverified),
            "空期望值必须全部是 Unverified，实际: {cs:?}"
        );
        assert!(!cs.is_empty(), "仍应为每个字段产出一条记录");
    }

    #[test]
    fn blank_sample_reports_no_ground_truth() {
        assert!(!blank_sample().has_ground_truth());
    }

    #[test]
    fn filled_sample_reports_ground_truth() {
        let mut s = blank_sample();
        s.invoice_number = Some("24312000000012345678".to_string());
        s.issue_date = Some("2026-07-03".to_string());
        s.total_amount = Some("553.00".to_string());
        assert!(s.has_ground_truth());
    }

    #[test]
    fn matching_values_are_match() {
        let mut s = blank_sample();
        s.invoice_number = Some("24312000000012345678".to_string());
        s.issue_date = Some("2026-07-03".to_string());
        s.total_amount = Some("553.00".to_string());
        s.ticket_type = Some(TicketType::Rail);
        let cs = s.compare(&parsed());
        for f in ["invoice_number", "issue_date", "total_amount", "ticket_type"] {
            let c = cs.iter().find(|c| c.field == f).expect(f);
            assert_eq!(c.status, FieldStatus::Match, "{f} 应匹配: {c:?}");
        }
    }

    #[test]
    fn wrong_value_is_mismatch() {
        let mut s = blank_sample();
        s.invoice_number = Some("99999999999999999999".to_string());
        let c = s
            .compare(&parsed())
            .into_iter()
            .find(|c| c.field == "invoice_number")
            .unwrap();
        assert_eq!(c.status, FieldStatus::Mismatch);
    }

    /// "553" 与 "553.00" 必须相等——金额比对走 Decimal，不是字符串。
    #[test]
    fn decimal_compare_ignores_trailing_zeros() {
        let mut s = blank_sample();
        s.total_amount = Some("553".to_string());
        let c = s
            .compare(&parsed())
            .into_iter()
            .find(|c| c.field == "total_amount")
            .unwrap();
        assert_eq!(c.status, FieldStatus::Match);
    }

    /// 期望值填了但解析结果缺失该字段 —— 必须是 Mismatch，不能悄悄算过。
    #[test]
    fn expected_but_missing_is_mismatch() {
        let mut s = blank_sample();
        s.tax_rate = Some("0.09".to_string());
        let c = s
            .compare(&parsed())
            .into_iter()
            .find(|c| c.field == "tax_rate")
            .unwrap();
        assert_eq!(c.status, FieldStatus::Mismatch);
        assert_eq!(c.actual, "<缺失>");
    }

    /// 期望值本身写坏了（不是合法 Decimal）要报 Mismatch，而不是 panic。
    #[test]
    fn unparseable_expectation_is_mismatch_not_panic() {
        let mut s = blank_sample();
        s.total_amount = Some("五百五十三".to_string());
        let c = s
            .compare(&parsed())
            .into_iter()
            .find(|c| c.field == "total_amount")
            .unwrap();
        assert_eq!(c.status, FieldStatus::Mismatch);
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

```bash
export PATH="$HOME/.cargo/bin:$PATH"
cd /home/holo/work-tools
cargo test -p invoice-parse --lib manifest 2>&1 | tail -25
```

Expected: compile errors — `FieldStatus` not found, `Sample` has no field `is_invoice`, `expected struct String, found Option<String>`.

- [ ] **Step 3: Change the `Sample` struct and `FieldComparison`**

In `crates/invoice-parse/src/manifest.rs`, replace the `Sample` struct (lines 14–40) and the `FieldComparison` struct (around line 58) with:

```rust
/// 一个样本的期望值声明。数值一律用 String 存，
/// 让清单保持人类可写，比对时再转成 Decimal/NaiveDate。
///
/// 所有期望字段都是 Option：`None` 表示"人工尚未核对"，
/// 不代表解析失败。这是通过率能被正确统计的前提。
#[derive(Debug, Deserialize)]
pub struct Sample {
    pub path: PathBuf,
    pub format: String,
    #[serde(default)]
    pub ticket_type: Option<TicketType>,
    #[serde(default)]
    pub invoice_number: Option<String>,
    #[serde(default)]
    pub issue_date: Option<String>,
    #[serde(default)]
    pub total_amount: Option<String>,
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
    /// 人工判定该文件是否为发票。None 表示未判定。
    /// 由 Task 6 的分类器对照，非发票不计入解析通过率分母。
    #[serde(default)]
    pub is_invoice: Option<bool>,
    /// XML/OFD 元素名提示，由 explore-xml 工具填入
    #[serde(default)]
    pub xml_tag_hints: Option<TagHints>,
}

/// 单个字段的比对结果。
#[derive(Debug, Clone, PartialEq)]
pub struct FieldComparison {
    pub field: &'static str,
    pub expected: String,
    pub actual: String,
    pub status: FieldStatus,
}

/// 比对状态。`Unverified` 表示清单里没填期望值——
/// 既不算通过也不算失败，只是还没人核对过。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldStatus {
    Match,
    Mismatch,
    Unverified,
}
```

Note the `matched: bool` field is gone. Any code reading `.matched` must switch to `.status`; Task 1 Step 5 and Task 2 handle the call sites.

- [ ] **Step 4: Rewrite `compare` and the comparator helpers**

Replace `impl Sample { pub fn compare(...) }` (lines 76–122) and the three helpers `compare_str`/`compare_opt_str`/`compare_decimal`/`compare_date` (lines 126–180) with:

```rust
impl Sample {
    /// 清单里是否已有可比对的期望值（三个必填字段任一已填）。
    pub fn has_ground_truth(&self) -> bool {
        self.invoice_number.is_some()
            || self.issue_date.is_some()
            || self.total_amount.is_some()
    }

    pub fn compare(&self, actual: &ParsedInvoice) -> Vec<FieldComparison> {
        vec![
            cmp_str("invoice_number", self.invoice_number.as_deref(), Some(actual.invoice_number.as_str())),
            cmp_date("issue_date", self.issue_date.as_deref(), actual.issue_date),
            cmp_decimal("total_amount", self.total_amount.as_deref(), Some(actual.total_amount)),
            cmp_decimal("tax_amount", self.tax_amount.as_deref(), actual.tax_amount),
            cmp_decimal("tax_rate", self.tax_rate.as_deref(), actual.tax_rate),
            cmp_str("buyer_name", self.buyer_name.as_deref(), actual.buyer_name.as_deref()),
            cmp_str("seller_name", self.seller_name.as_deref(), actual.seller_name.as_deref()),
            cmp_ticket_type("ticket_type", self.ticket_type, actual.ticket_type),
        ]
    }
}

const MISSING: &str = "<缺失>";

/// 没填期望值 → Unverified。填了但实际缺失 → Mismatch。
fn cmp_str(
    field: &'static str,
    expected: Option<&str>,
    actual: Option<&str>,
) -> FieldComparison {
    let actual_s = actual.unwrap_or(MISSING).to_string();
    match expected {
        None => FieldComparison {
            field,
            expected: String::new(),
            actual: actual_s,
            status: FieldStatus::Unverified,
        },
        Some(e) => FieldComparison {
            field,
            expected: e.to_string(),
            actual: actual_s,
            status: if actual == Some(e) {
                FieldStatus::Match
            } else {
                FieldStatus::Mismatch
            },
        },
    }
}

/// 数值比对走 Decimal，"553" 与 "553.00" 视为相等。
/// 期望值本身解析不出来时算 Mismatch，不 panic。
fn cmp_decimal(
    field: &'static str,
    expected: Option<&str>,
    actual: Option<Decimal>,
) -> FieldComparison {
    use rust_decimal::prelude::FromStr;
    let actual_s = actual.map(|d| d.to_string()).unwrap_or_else(|| MISSING.into());
    match expected {
        None => FieldComparison {
            field,
            expected: String::new(),
            actual: actual_s,
            status: FieldStatus::Unverified,
        },
        Some(raw) => {
            let matched = match (Decimal::from_str(raw).ok(), actual) {
                (Some(e), Some(a)) => e == a,
                _ => false,
            };
            FieldComparison {
                field,
                expected: raw.to_string(),
                actual: actual_s,
                status: if matched { FieldStatus::Match } else { FieldStatus::Mismatch },
            }
        }
    }
}

fn cmp_date(
    field: &'static str,
    expected: Option<&str>,
    actual: chrono::NaiveDate,
) -> FieldComparison {
    let actual_s = actual.to_string();
    match expected {
        None => FieldComparison {
            field,
            expected: String::new(),
            actual: actual_s,
            status: FieldStatus::Unverified,
        },
        Some(raw) => {
            let matched = chrono::NaiveDate::parse_from_str(raw, "%Y-%m-%d").ok() == Some(actual);
            FieldComparison {
                field,
                expected: raw.to_string(),
                actual: actual_s,
                status: if matched { FieldStatus::Match } else { FieldStatus::Mismatch },
            }
        }
    }
}

fn cmp_ticket_type(
    field: &'static str,
    expected: Option<TicketType>,
    actual: TicketType,
) -> FieldComparison {
    let actual_s = format!("{actual:?}");
    match expected {
        None => FieldComparison {
            field,
            expected: String::new(),
            actual: actual_s,
            status: FieldStatus::Unverified,
        },
        Some(e) => FieldComparison {
            field,
            expected: format!("{e:?}"),
            actual: actual_s,
            status: if e == actual { FieldStatus::Match } else { FieldStatus::Mismatch },
        },
    }
}
```

Also remove the now-unused `ParseLevel` import flagged by the compiler: change line 1 to

```rust
use crate::model::{ParsedInvoice, TicketType};
```

- [ ] **Step 5: Update the two call sites that read `.matched`**

`crates/invoice-parse/src/main.rs` around line 288 currently filters on `!c.matched`. Change that expression to:

```rust
                let failures: Vec<_> = comparisons
                    .into_iter()
                    .filter(|c| c.status == invoice_parse::manifest::FieldStatus::Mismatch)
                    .collect();
```

`crates/invoice-parse/src/report.rs` — if it reads `.matched` anywhere, switch to `c.status == FieldStatus::Mismatch`. Verify with:

```bash
grep -rn "\.matched" crates/invoice-parse/src/
```

Expected after the edits: no output.

- [ ] **Step 6: Make the manifest parse under the new schema**

The current `fixtures/manifest.toml` has `invoice_number = ""` on 64 entries. Empty strings would now deserialize as `Some("")`, which is a Mismatch, not Unverified. Delete those blank lines:

```bash
cd /home/holo/work-tools
cp fixtures/manifest.toml fixtures/manifest.toml.bak
sed -i '/^invoice_number = ""$/d; /^issue_date = ""$/d; /^total_amount = ""$/d' fixtures/manifest.toml
sed -i 's/^ticket_type = "Other"       # 待确认$/# ticket_type 待人工核对/' fixtures/manifest.toml
grep -c '^\[\[sample\]\]' fixtures/manifest.toml
```

Expected: `64`. Then confirm no blank expectations survive:

```bash
grep -n '= ""' fixtures/manifest.toml | grep -v '^\s*#'
```

Expected: no output.

- [ ] **Step 7: Run the tests to verify they pass**

```bash
cargo test -p invoice-parse --lib manifest 2>&1 | tail -15
```

Expected: `test result: ok. 8 passed; 0 failed`.

- [ ] **Step 8: Confirm the whole crate still builds and all tests pass**

```bash
cargo test -p invoice-parse 2>&1 | tail -20
```

Expected: all pre-existing tests still pass (xml, ocr, ofd, pdf, report, verify modules), zero failures.

- [ ] **Step 9: Commit**

```bash
git add crates/invoice-parse/src/manifest.rs crates/invoice-parse/src/main.rs crates/invoice-parse/src/report.rs fixtures/manifest.toml
git rm --cached fixtures/manifest.toml.bak 2>/dev/null || true
rm -f fixtures/manifest.toml.bak
git commit -m "fix: 空期望值记为未核对而非不匹配，使通过率可度量"
```

---

## Task 2: Report parse-success and verified-pass as separate rates

**Files:**
- Modify: `crates/invoice-parse/src/report.rs:3-24` (types and `render_markdown`)
- Modify: `crates/invoice-parse/src/main.rs:215-306` (`verify_all`)
- Test: `crates/invoice-parse/src/report.rs` (inline `#[cfg(test)]`)

**Interfaces:**
- Consumes: `manifest::{FieldComparison, FieldStatus}` (Task 1)
- Produces:
  - `report::OutcomeKind { Verified, Mismatched { failures: Vec<FieldComparison> }, ParsedUnverified, ParseFailed { error: String }, Skipped { reason: String } }`
  - `report::SampleOutcome { path: String, format: String, result: OutcomeKind }`
  - `report::SampleOutcome::parsed(&self) -> bool`
  - `report::SampleOutcome::verified(&self) -> bool`
  - `report::render_markdown(outcomes: &[SampleOutcome]) -> String`

**Why:** A sample that parses correctly but has no hand-verified expectation is neither a pass nor a failure. Collapsing those into one number is what produced the bogus 29.7% and 57.6% figures. The report needs both columns so the two can never be confused again.

- [ ] **Step 1: Write the failing tests**

Replace the `#[cfg(test)]` block in `crates/invoice-parse/src/report.rs` with:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::{FieldComparison, FieldStatus};

    fn o(path: &str, format: &str, result: OutcomeKind) -> SampleOutcome {
        SampleOutcome { path: path.into(), format: format.into(), result }
    }

    fn mismatch() -> FieldComparison {
        FieldComparison {
            field: "total_amount",
            expected: "100.00".into(),
            actual: "1.00".into(),
            status: FieldStatus::Mismatch,
        }
    }

    fn fixture() -> Vec<SampleOutcome> {
        vec![
            o("a.xml", "xml", OutcomeKind::Verified),
            o("b.xml", "xml", OutcomeKind::ParsedUnverified),
            o("c.pdf", "pdf-vat", OutcomeKind::Mismatched { failures: vec![mismatch()] }),
            o("d.pdf", "pdf-vat", OutcomeKind::ParseFailed { error: "无文本层".into() }),
            o("e.jpg", "image", OutcomeKind::Skipped { reason: "非发票".into() }),
        ]
    }

    #[test]
    fn parsed_counts_verified_mismatched_and_unverified() {
        let os = fixture();
        assert!(os[0].parsed(), "Verified 算解析成功");
        assert!(os[1].parsed(), "ParsedUnverified 算解析成功");
        assert!(os[2].parsed(), "Mismatched 算解析成功——它产出了结构体");
        assert!(!os[3].parsed(), "ParseFailed 不算");
        assert!(!os[4].parsed(), "Skipped 不算");
    }

    #[test]
    fn verified_only_counts_verified() {
        let os = fixture();
        assert!(os[0].verified());
        for i in 1..5 {
            assert!(!os[i].verified(), "索引 {i} 不应算已核对通过");
        }
    }

    /// 报告必须把两个率分开印，且 Skipped 不进分母。
    #[test]
    fn markdown_reports_both_rates_and_excludes_skipped() {
        let md = render_markdown(&fixture());
        assert!(md.contains("解析成功率"), "缺少解析成功率: {md}");
        assert!(md.contains("已核对通过率"), "缺少已核对通过率: {md}");
        // 5 个样本里 1 个 Skipped，分母是 4；解析成功 3 个
        assert!(md.contains("3/4"), "解析成功率应为 3/4: {md}");
        // 已核对的只有 2 个（Verified + Mismatched），其中通过 1 个
        assert!(md.contains("1/2"), "已核对通过率应为 1/2: {md}");
    }

    #[test]
    fn markdown_lists_mismatch_field_detail() {
        let md = render_markdown(&fixture());
        assert!(md.contains("total_amount"), "应列出不匹配字段名");
        assert!(md.contains("100.00"), "应列出期望值");
        assert!(md.contains("1.00"), "应列出实际值");
    }

    #[test]
    fn markdown_lists_parse_failures_with_reason() {
        let md = render_markdown(&fixture());
        assert!(md.contains("无文本层"), "应列出解析失败原因");
    }

    /// 全部未核对时不能显示 NaN 或 0%，要明确说没有可核对样本。
    #[test]
    fn all_unverified_does_not_report_zero_percent() {
        let md = render_markdown(&[o("a.xml", "xml", OutcomeKind::ParsedUnverified)]);
        assert!(!md.contains("0.0%"), "不应把未核对显示成 0%: {md}");
        assert!(md.contains("尚无") || md.contains("0/0"), "应明确标注无可核对样本: {md}");
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

```bash
export PATH="$HOME/.cargo/bin:$PATH"
cd /home/holo/work-tools
cargo test -p invoice-parse --lib report 2>&1 | tail -20
```

Expected: compile errors — no variant `Verified`, no variant `ParsedUnverified`, no method `parsed`.

- [ ] **Step 3: Replace the types in `report.rs`**

Replace lines 3–22 of `crates/invoice-parse/src/report.rs` (the `SampleOutcome` struct, `OutcomeKind` enum, and `impl SampleOutcome`) with:

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
    /// 解析成功且与人工核对的期望值全部一致
    Verified,
    /// 解析成功但至少一个已核对字段不一致
    Mismatched { failures: Vec<FieldComparison> },
    /// 解析成功，但清单里还没有人工核对的期望值
    ParsedUnverified,
    /// 解析器没能产出 ParsedInvoice
    ParseFailed { error: String },
    /// 判定为非发票或无需解析，不计入任何分母
    Skipped { reason: String },
}

impl SampleOutcome {
    /// 解析器是否产出了 ParsedInvoice（不论对错）
    pub fn parsed(&self) -> bool {
        matches!(
            self.result,
            OutcomeKind::Verified | OutcomeKind::Mismatched { .. } | OutcomeKind::ParsedUnverified
        )
    }

    /// 是否已人工核对且全部字段一致
    pub fn verified(&self) -> bool {
        matches!(self.result, OutcomeKind::Verified)
    }

    /// 是否已有人工核对的期望值（Verified 或 Mismatched）
    pub fn has_truth(&self) -> bool {
        matches!(self.result, OutcomeKind::Verified | OutcomeKind::Mismatched { .. })
    }

    fn skipped(&self) -> bool {
        matches!(self.result, OutcomeKind::Skipped { .. })
    }
}
```

- [ ] **Step 4: Rewrite `render_markdown`**

Replace the entire `pub fn render_markdown` body with:

```rust
pub fn render_markdown(outcomes: &[SampleOutcome]) -> String {
    use std::collections::BTreeMap;
    use std::fmt::Write;

    let mut s = String::from("# 发票解析能力验证报告\n\n");

    let scored: Vec<&SampleOutcome> = outcomes.iter().filter(|o| !o.skipped()).collect();
    let parsed = scored.iter().filter(|o| o.parsed()).count();
    let with_truth: Vec<&&SampleOutcome> = scored.iter().filter(|o| o.has_truth()).collect();
    let verified = with_truth.iter().filter(|o| o.verified()).count();

    let pct = |n: usize, d: usize| {
        if d == 0 { "—".to_string() } else { format!("{:.1}%", n as f64 / d as f64 * 100.0) }
    };

    writeln!(s, "## 总览\n").unwrap();
    writeln!(
        s,
        "- **解析成功率**：{parsed}/{} （{}）— 解析器产出了 ParsedInvoice",
        scored.len(),
        pct(parsed, scored.len())
    )
    .unwrap();
    if with_truth.is_empty() {
        writeln!(s, "- **已核对通过率**：0/0 —— 尚无人工核对的期望值，准确率暂不可度量").unwrap();
    } else {
        writeln!(
            s,
            "- **已核对通过率**：{verified}/{} （{}）— 与人工核对值完全一致",
            with_truth.len(),
            pct(verified, with_truth.len())
        )
        .unwrap();
    }
    let unverified = parsed - with_truth.iter().filter(|o| o.parsed()).count();
    writeln!(s, "- 解析成功但未核对：{unverified}").unwrap();
    writeln!(s, "- 跳过（非发票）：{}\n", outcomes.len() - scored.len()).unwrap();

    // 按格式分组
    let mut by_format: BTreeMap<&str, (usize, usize, usize, usize)> = BTreeMap::new();
    for o in &scored {
        let e = by_format.entry(o.format.as_str()).or_insert((0, 0, 0, 0));
        e.0 += 1;
        if o.parsed() { e.1 += 1; }
        if o.has_truth() { e.2 += 1; }
        if o.verified() { e.3 += 1; }
    }
    writeln!(s, "## 按格式\n").unwrap();
    writeln!(s, "| 格式 | 样本 | 解析成功 | 已核对 | 核对通过 |").unwrap();
    writeln!(s, "|---|---|---|---|---|").unwrap();
    for (fmt, (total, ok, truth, ver)) in &by_format {
        writeln!(s, "| {fmt} | {total} | {ok} ({}) | {truth} | {ver} |", pct(*ok, *total)).unwrap();
    }
    s.push('\n');

    let mismatches: Vec<_> = outcomes
        .iter()
        .filter_map(|o| match &o.result {
            OutcomeKind::Mismatched { failures } => Some((o, failures)),
            _ => None,
        })
        .collect();
    if !mismatches.is_empty() {
        writeln!(s, "## 字段不匹配\n").unwrap();
        writeln!(s, "| 样本 | 字段 | 期望 | 实际 |").unwrap();
        writeln!(s, "|---|---|---|---|").unwrap();
        for (o, failures) in mismatches {
            for f in failures {
                writeln!(s, "| {} | {} | {} | {} |", o.path, f.field, f.expected, f.actual).unwrap();
            }
        }
        s.push('\n');
    }

    let failed: Vec<_> = outcomes
        .iter()
        .filter_map(|o| match &o.result {
            OutcomeKind::ParseFailed { error } => Some((o, error)),
            _ => None,
        })
        .collect();
    if !failed.is_empty() {
        writeln!(s, "## 解析失败\n").unwrap();
        writeln!(s, "| 样本 | 格式 | 原因 |").unwrap();
        writeln!(s, "|---|---|---|").unwrap();
        for (o, err) in failed {
            writeln!(s, "| {} | {} | {} |", o.path, o.format, err).unwrap();
        }
        s.push('\n');
    }

    s
}
```

- [ ] **Step 5: Run the tests to verify they pass**

```bash
cargo test -p invoice-parse --lib report 2>&1 | tail -12
```

Expected: `test result: ok. 6 passed; 0 failed`.

- [ ] **Step 6: Wire `verify_all` to the new outcome types**

In `crates/invoice-parse/src/main.rs`, replace the `let result = match parsed { ... }` block (around lines 278–300) with:

```rust
        let result = match parsed {
            Ok(invoice) => {
                if !sample.has_ground_truth() {
                    OutcomeKind::ParsedUnverified
                } else {
                    let failures: Vec<_> = sample
                        .compare(&invoice)
                        .into_iter()
                        .filter(|c| c.status == invoice_parse::manifest::FieldStatus::Mismatch)
                        .collect();
                    if failures.is_empty() {
                        OutcomeKind::Verified
                    } else {
                        OutcomeKind::Mismatched { failures }
                    }
                }
            }
            Err(e) => OutcomeKind::ParseFailed { error: e.to_string() },
        };
```

- [ ] **Step 7: Run `verify-all` and confirm the headline is now honest**

```bash
cargo run -q -p invoice-parse -- verify-all 2>/dev/null | head -14
```

Expected: `解析成功率：19/64 （29.7%）` and `已核对通过率：0/0 —— 尚无人工核对的期望值，准确率暂不可度量`. The 19 is real; the 0/0 correctly states that accuracy is not yet measurable. This is the honest baseline Task 3 starts from.

- [ ] **Step 8: Commit**

```bash
git add crates/invoice-parse/src/report.rs crates/invoice-parse/src/main.rs
git commit -m "feat: 报告区分解析成功率与已核对通过率"
```

---

## Task 3: Ground-truth entry tool, then hand-verify the 19 parsing samples

**Files:**
- Modify: `crates/invoice-parse/src/main.rs` (add `dump-text` subcommand + `Commands` enum arm)
- Modify: `fixtures/manifest.toml` (hand-entered ground truth)
- Create: `docs/ground-truth-log.md`

**Interfaces:**
- Consumes: `pdf::extract_text`, `xml::collect_leaf_elements`, `ofd::list_entries`
- Produces: `dump-text <path>` CLI subcommand printing human-readable document text for manual reading

**Critical rule:** `dump-text` prints the *document's own text* so a human can read the values off it. It does NOT print `ParsedInvoice` output. Copying parser output into the manifest would make every test assert current behaviour, including its bugs — the manifest would then be incapable of detecting a regression. Read the values, type them in.

- [ ] **Step 1: Add the `dump-text` subcommand**

In `crates/invoice-parse/src/main.rs`, add to the `Commands` enum:

```rust
    /// 打印文件自身的文本内容，供人工核对期望值
    DumpText {
        path: PathBuf,
    },
```

Add the dispatch arm in `main()`:

```rust
        Commands::DumpText { path } => dump_text(path),
```

Add the function:

```rust
/// 打印文档自身的文本，供人工读出期望值填进 manifest。
///
/// 注意：这里刻意不调用任何 parse_invoice_* 函数。
/// 期望值必须来自文档本身，不能来自解析器输出——
/// 否则测试就变成"断言当前行为"，永远发现不了错。
fn dump_text(path: PathBuf) -> anyhow::Result<()> {
    let bytes = std::fs::read(&path)?;
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();

    println!("=== {} ({} 字节) ===\n", path.display(), bytes.len());

    match ext.as_str() {
        "xml" => {
            for leaf in invoice_parse::xml::collect_leaf_elements(&bytes)? {
                println!("{:<40} = {}", leaf.name, leaf.text);
            }
        }
        "pdf" => match invoice_parse::pdf::extract_text(&bytes, &path) {
            Ok(text) => println!("{text}"),
            Err(e) => println!("[文本层提取失败: {e}]\n请用 PDF 阅读器打开该文件人工读取。"),
        },
        "ofd" => {
            println!("--- ZIP 条目 ---");
            for entry in invoice_parse::ofd::list_entries(&bytes)? {
                println!("  {entry}");
            }
            println!("\n--- 版式文本（含坐标，单位毫米）---");
            match invoice_parse::ofd_text::extract_text_boxes(&bytes, &path) {
                Ok(boxes) => {
                    for b in boxes {
                        println!("({:7.2},{:6.2}) {:?}", b.x, b.y, b.text);
                    }
                }
                Err(e) => println!("[版式文本提取失败: {e}]"),
            }
        }
        "jpg" | "jpeg" | "png" => {
            println!("图片样本：请直接用图片查看器打开人工读取。");
        }
        other => println!("未知扩展名 {other}，请人工打开。"),
    }
    Ok(())
}
```

Note this references `invoice_parse::ofd_text`, created in Task 4. Until then, comment out the `"ofd"` arm's text-boxes block or implement Task 4 first — the two tasks may be reordered. If running Task 3 first, replace that block with `println!("(OFD 版式文本提取见 Task 4)");`.

- [ ] **Step 2: Verify `dump-text` works on one XML and one PDF**

```bash
export PATH="$HOME/.cargo/bin:$PATH"
cd /home/holo/work-tools
cargo run -q -p invoice-parse -- dump-text fixtures/samples/03-unknown-6201d368.xml 2>/dev/null | head -20
cargo run -q -p invoice-parse -- dump-text fixtures/samples/04-unknown-6554429d.pdf 2>/dev/null | head -30
```

Expected: XML shows element-name/value pairs; PDF shows the invoice's text content including a 20-digit number, a date, and an amount.

- [ ] **Step 3: List the samples that currently parse**

These are the ones worth hand-verifying first — a ground-truth value on a sample that cannot parse yields a `ParseFailed`, which teaches nothing until the parser is fixed.

```bash
cargo run -q -p invoice-parse -- verify-all 2>/dev/null \
  | awk '/^## 待人工核对/,/^$/' | grep '^- ' | sed 's/^- //' > /tmp/to-verify.txt
wc -l /tmp/to-verify.txt; cat /tmp/to-verify.txt
```

Expected: ~19 paths.

- [ ] **Step 4: Hand-verify each sample and fill the manifest**

For each path in `/tmp/to-verify.txt`:

```bash
cargo run -q -p invoice-parse -- dump-text fixtures/<path> 2>/dev/null | less
```

Read these values off the document and add them to that sample's `[[sample]]` block in `fixtures/manifest.toml`:

```toml
invoice_number = "26112000002208097411"   # 发票号码，纯数字
issue_date = "2026-06-01"                 # 开票日期，一律 YYYY-MM-DD
total_amount = "47.40"                    # 价税合计（小写），不带 ¥
ticket_type = "Vat"                       # Vat / Rail / Flight / Other
```

Rules:
- `issue_date` is always `YYYY-MM-DD`, regardless of how the document writes it (`2026年06月01日` → `2026-06-01`).
- `total_amount` is the tax-inclusive total (价税合计 / 小写), never the pre-tax subtotal (金额) — mixing these up is the single most likely ground-truth error.
- Fill `tax_amount`/`tax_rate`/`buyer_name`/`seller_name` only when the document clearly shows them. Leave the line out entirely if unsure — omitted means unverified, which is honest.
- `tax_rate` is a fraction: 6% → `"0.06"`.
- If the document is not an invoice (travel report, order detail, itinerary summary), set `is_invoice = false` and add no expected values.
- If a value is genuinely unreadable, add `# 无法读取: <reason>` and leave the field out.

- [ ] **Step 5: Log the verification**

Create `docs/ground-truth-log.md`:

```markdown
# 人工核对记录

**核对人：** <你的名字>
**日期：** 2026-08-05
**方法：** `cargo run -p invoice-parse -- dump-text <文件>`，从文档自身文本读取，未参考解析器输出。

## 已核对样本

| 样本 | 是否发票 | 发票号码 | 开票日期 | 价税合计 | 备注 |
|---|---|---|---|---|---|
| samples/03-unknown-6201d368.xml | 是 | 26112000002208097411 | 2026-06-01 | 47.40 | |

## 判定为非发票

| 样本 | 理由 |
|---|---|
| samples/49-didi-2745a005.pdf | 滴滴行程单，无发票号码/价税合计 |

## 无法核对

| 样本 | 原因 |
|---|---|
| samples/02-unknown-f6f7c6b1.ofd | ZIP 中央目录损坏，无法打开 |
```

Fill in every row you actually verified.

- [ ] **Step 6: Re-run `verify-all` — now the pass rate means something**

```bash
cargo run -q -p invoice-parse -- verify-all 2>/dev/null | head -25
```

Expected: 已核对通过率 is no longer `0/0`. Any `Mismatched` rows are now genuine parser bugs with a concrete expected-vs-actual pair. Record the numbers.

- [ ] **Step 7: Commit**

```bash
git add crates/invoice-parse/src/main.rs fixtures/manifest.toml docs/ground-truth-log.md docs/spike-report.md
git commit -m "feat: 新增 dump-text 工具并人工核对首批样本期望值"
```

---

## Task 4: Extract OFD text + coordinates directly from layout XML

**Files:**
- Create: `crates/invoice-parse/src/ofd_text.rs`
- Modify: `crates/invoice-parse/src/lib.rs` (add `pub mod ofd_text;`)
- Modify: `crates/invoice-parse/src/ocr.rs` (`locate_vat_fields` gains a `level` parameter; add `merge_line_fragments`)
- Modify: `crates/invoice-parse/src/ofd.rs` (fall back to `ofd_text` when no structured XML)
- Test: inline `#[cfg(test)]` in `ofd_text.rs` and `ocr.rs`

**Interfaces:**
- Consumes: `ocr::TextBox`, `ocr::locate_vat_fields`, `model::{ParseError, ParseLevel, ParsedInvoice}`
- Produces:
  - `ofd_text::extract_text_boxes(bytes: &[u8], path: &Path) -> Result<Vec<TextBox>, ParseError>`
  - `ofd_text::MM_TO_PX: f32` (= 3.7795)
  - `ocr::merge_line_fragments(boxes: Vec<TextBox>, max_gap: f32) -> Vec<TextBox>`
  - `ocr::locate_vat_fields(boxes: &[TextBox], path: &Path, level: ParseLevel) -> Result<ParsedInvoice, ParseError>`

**Why no Java:** `Doc_0/Pages/Page_0/Content.xml` inside each OFD holds `<ofd:TextObject Boundary="x y w h">` elements wrapping `<ofd:TextCode>` with the real text. Verified by inspection: sample `48-unknown-cb25d50d.ofd` contains `26132000001954318426`, `2026年06月22日`, and the seller name as literal text. This is deterministic text with exact coordinates, so it is **ParseLevel::L1** — no rendering, no OCR, no `java`, no new crates.

**Two traps this task must handle:**

1. **Units.** `Boundary` is millimetres; page pitch between lines is 3–6mm. `ocr::SAME_LINE_TOLERANCE` is `15.0` assuming pixels. Feeding raw mm collapses every box onto one line. Scale by `MM_TO_PX = 3.7795` (96 DPI) so the existing tolerance stays valid.
2. **Fragmentation.** Sample `11-meituan-34ee412d.ofd` splits `电子发票（普通发票）` across four `TextObject`s (`电子` / `发票（` / `普通发` / `票）`), each with its own `Boundary`. A regex over `<TextObject ...><TextCode>` also fails there because `CGTransform`/`FillColor` children sit between the two tags — a real XML parser is required, and same-line neighbours must be merged before field location.

- [ ] **Step 1: Write the failing tests for `ofd_text`**

Create `crates/invoice-parse/src/ofd_text.rs`:

```rust
//! 从 OFD 版式 XML 中直接提取文本与坐标。
//!
//! OFD 是 ZIP 容器，`Doc_0/Pages/Page_*/Content.xml` 里的
//! `<ofd:TextObject Boundary="x y w h">` 包着 `<ofd:TextCode>` 文本。
//! 这是文件自带的确定性文本 + 精确坐标，因此定级 L1 而不是 L2：
//! 不需要渲染、不需要 OCR、不需要 java。

use crate::model::{ParseError, ParseLevel, ParsedInvoice};
use crate::ocr::{locate_vat_fields, merge_line_fragments, TextBox};
use quick_xml::events::Event;
use quick_xml::Reader;
use std::io::{Cursor, Read};
use std::path::Path;

/// 毫米转像素（96 DPI）。
///
/// OFD Boundary 单位是毫米，行间距只有 3–6mm，
/// 而 ocr::SAME_LINE_TOLERANCE 是按像素定的 15.0。
/// 不换算的话所有框会被判成同一行。
pub const MM_TO_PX: f32 = 3.7795;

/// 同一行内两个碎片框之间允许的最大水平间隙（像素）。
/// 美团票把一个词拆成 4 个 TextObject，间隙接近 0；
/// 不同字段之间的间隙远大于此值。
const FRAGMENT_MAX_GAP: f32 = 6.0;

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::path::PathBuf;

    /// 构造一个最小 OFD：ZIP 里只放一个 Content.xml。
    fn make_ofd(content_xml: &str) -> Vec<u8> {
        let mut buf = Vec::new();
        {
            let mut zip = zip::ZipWriter::new(Cursor::new(&mut buf));
            let opts: zip::write::FileOptions<'_, ()> = zip::write::FileOptions::default();
            zip.start_file("Doc_0/Pages/Page_0/Content.xml", opts).unwrap();
            zip.write_all(content_xml.as_bytes()).unwrap();
            zip.finish().unwrap();
        }
        buf
    }

    /// 标签与值各自一个 TextObject，同一行，值在右侧。
    const CLEAN: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<ofd:Page xmlns:ofd="http://www.ofdspec.org/2016"><ofd:Content><ofd:Layer>
<ofd:TextObject ID="14" Boundary="154.5 10.9 20 4" Size="3.175"><ofd:TextCode X="0" Y="3.143">发票号码：</ofd:TextCode></ofd:TextObject>
<ofd:TextObject ID="15" Boundary="176.0 10.9 30 4" Size="3.175"><ofd:TextCode X="0" Y="3.143">26132000001954318426</ofd:TextCode></ofd:TextObject>
<ofd:TextObject ID="16" Boundary="154.5 16.9 20 4" Size="3.175"><ofd:TextCode X="0" Y="3.143">开票日期：</ofd:TextCode></ofd:TextObject>
<ofd:TextObject ID="17" Boundary="176.0 16.9 30 4" Size="3.175"><ofd:TextCode X="0" Y="3.143">2026年06月22日</ofd:TextCode></ofd:TextObject>
<ofd:TextObject ID="18" Boundary="20.0 80.0 24 4" Size="3.175"><ofd:TextCode X="0" Y="3.143">价税合计</ofd:TextCode></ofd:TextObject>
<ofd:TextObject ID="19" Boundary="50.0 80.0 20 4" Size="3.175"><ofd:TextCode X="0" Y="3.143">￥47.40</ofd:TextCode></ofd:TextObject>
</ofd:Layer></ofd:Content></ofd:Page>"#;

    /// 美团版式：TextCode 前夹着 CGTransform / FillColor 子元素，
    /// 且一个词被拆成多个 TextObject。正则做法在这里必然失败。
    const FRAGMENTED: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<ofd:Page xmlns:ofd="http://www.ofdspec.org/2016"><ofd:Content><ofd:Layer>
<ofd:TextObject ID="116" Boundary="154.530 10.701 3.300 4.874" Size="3.300" CTM="1.0 0.0 0.0 1.0 0.0 0.0"><ofd:CGTransform CodePosition="0" GlyphCount="1" CodeCount="1"><ofd:Glyphs>1</ofd:Glyphs></ofd:CGTransform><ofd:FillColor Value="0 0 0"/><ofd:TextCode X="0" Y="3.3">发</ofd:TextCode></ofd:TextObject>
<ofd:TextObject ID="117" Boundary="157.830 10.701 3.300 4.874" Size="3.300"><ofd:FillColor Value="0 0 0"/><ofd:TextCode X="0" Y="3.3">票号</ofd:TextCode></ofd:TextObject>
<ofd:TextObject ID="118" Boundary="161.130 10.701 3.300 4.874" Size="3.300"><ofd:TextCode X="0" Y="3.3">码：</ofd:TextCode></ofd:TextObject>
<ofd:TextObject ID="119" Boundary="165.000 10.701 30.000 4.874" Size="3.300"><ofd:TextCode X="0" Y="3.3">26112000002208097411</ofd:TextCode></ofd:TextObject>
</ofd:Layer></ofd:Content></ofd:Page>"#;

    #[test]
    fn extracts_text_with_coordinates() {
        let boxes = extract_text_boxes(&make_ofd(CLEAN), Path::new("t.ofd")).unwrap();
        assert!(boxes.len() >= 6, "应提取到至少 6 个文本框，实际 {}", boxes.len());
        assert!(
            boxes.iter().any(|b| b.text.contains("26132000001954318426")),
            "应含发票号码，实际: {:?}",
            boxes.iter().map(|b| &b.text).collect::<Vec<_>>()
        );
    }

    /// 坐标必须换算成像素，否则同行判定失效。
    #[test]
    fn coordinates_are_scaled_to_pixels() {
        let boxes = extract_text_boxes(&make_ofd(CLEAN), Path::new("t.ofd")).unwrap();
        let num = boxes.iter().find(|b| b.text.contains("2613")).unwrap();
        // Boundary x=176.0 mm → 176.0 * 3.7795 ≈ 665
        assert!(
            (num.x - 176.0 * MM_TO_PX).abs() < 1.0,
            "x 应换算为像素 ≈{:.1}，实际 {:.1}",
            176.0 * MM_TO_PX,
            num.x
        );
    }

    /// 结构化置信度恒为 1.0——这是文件自带文本，不是识别猜测。
    #[test]
    fn confidence_is_one_for_structured_text() {
        let boxes = extract_text_boxes(&make_ofd(CLEAN), Path::new("t.ofd")).unwrap();
        assert!(boxes.iter().all(|b| b.confidence == 1.0));
    }

    /// 标签与值在同一行、值在右侧 —— 字段定位器应该能配上。
    #[test]
    fn clean_layout_locates_all_three_required_fields() {
        let inv = parse_invoice_ofd_text(&make_ofd(CLEAN), Path::new("t.ofd")).unwrap();
        assert_eq!(inv.invoice_number, "26132000001954318426");
        assert_eq!(inv.issue_date.to_string(), "2026-06-22");
        assert_eq!(
            inv.total_amount,
            rust_decimal::Decimal::from_str_exact("47.40").unwrap()
        );
    }

    /// OFD 版式文本是文件自带的确定性文本，定级 L1，不是 L2。
    #[test]
    fn parse_level_is_l1_not_l2() {
        let inv = parse_invoice_ofd_text(&make_ofd(CLEAN), Path::new("t.ofd")).unwrap();
        assert_eq!(inv.parse_level, ParseLevel::L1);
        assert_eq!(inv.confidence, 1.0);
    }

    /// 被拆成 发/票号/码： 的碎片必须先合并，否则找不到 "发票号码" 标签。
    #[test]
    fn fragmented_layout_merges_and_locates() {
        let inv = parse_invoice_ofd_text(&make_ofd(FRAGMENTED), Path::new("t.ofd"));
        match inv {
            Ok(i) => assert_eq!(i.invoice_number, "26112000002208097411"),
            Err(e) => {
                // 该测试样本只有发票号码，缺日期和金额，
                // 因此允许因缺字段失败，但绝不能是找不到 invoice_number
                let msg = e.to_string();
                assert!(
                    !msg.contains("invoice_number"),
                    "碎片合并失败，没能识别出发票号码标签: {msg}"
                );
            }
        }
    }

    #[test]
    fn merged_text_reassembles_label() {
        let boxes = extract_text_boxes(&make_ofd(FRAGMENTED), Path::new("t.ofd")).unwrap();
        let merged = merge_line_fragments(boxes, FRAGMENT_MAX_GAP);
        assert!(
            merged.iter().any(|b| b.text.contains("发票号码")),
            "合并后应出现完整标签 发票号码，实际: {:?}",
            merged.iter().map(|b| &b.text).collect::<Vec<_>>()
        );
    }

    #[test]
    fn corrupt_zip_returns_malformed_error() {
        let err = extract_text_boxes(b"not a zip at all", Path::new("bad.ofd")).unwrap_err();
        assert!(
            matches!(err, ParseError::MalformedFormat { .. }),
            "应返回 MalformedFormat，实际 {err:?}"
        );
    }

    #[test]
    fn zip_without_content_xml_reports_missing_field() {
        let empty = make_ofd_named("Doc_0/Document.xml", "<a/>");
        let err = extract_text_boxes(&empty, Path::new("e.ofd")).unwrap_err();
        assert!(matches!(err, ParseError::MissingField { .. }), "实际 {err:?}");
    }

    fn make_ofd_named(name: &str, body: &str) -> Vec<u8> {
        let mut buf = Vec::new();
        {
            let mut zip = zip::ZipWriter::new(Cursor::new(&mut buf));
            let opts: zip::write::FileOptions<'_, ()> = zip::write::FileOptions::default();
            zip.start_file(name, opts).unwrap();
            zip.write_all(body.as_bytes()).unwrap();
            zip.finish().unwrap();
        }
        buf
    }

    /// 真实样本回归：这 6 个 OFD 必须能提取到文本框。
    /// 02-unknown-f6f7c6b1.ofd 的 ZIP 中央目录损坏，不在其列。
    #[test]
    fn real_samples_yield_text_boxes() {
        let names = [
            "11-meituan-34ee412d.ofd",
            "28-unknown-36c9093e.ofd",
            "33-unknown-1f1e61a4.ofd",
            "40-meituan-12f8065e.ofd",
            "45-unknown-3ed9ed77.ofd",
            "48-unknown-cb25d50d.ofd",
            "63-unknown-19d988e1.ofd",
        ];
        for n in names {
            let p = PathBuf::from("../../fixtures/samples").join(n);
            let Ok(bytes) = std::fs::read(&p) else { continue };
            let boxes = extract_text_boxes(&bytes, &p)
                .unwrap_or_else(|e| panic!("{n} 提取失败: {e}"));
            assert!(!boxes.is_empty(), "{n} 应提取到文本框");
        }
    }
}
```

- [ ] **Step 2: Add the module and run the tests to verify they fail**

Add to `crates/invoice-parse/src/lib.rs`, keeping alphabetical order:

```rust
pub mod ofd_text;
```

```bash
export PATH="$HOME/.cargo/bin:$PATH"
cd /home/holo/work-tools
cargo test -p invoice-parse --lib ofd_text 2>&1 | tail -20
```

Expected: compile errors — `extract_text_boxes` not found, `parse_invoice_ofd_text` not found, `merge_line_fragments` not found in `ocr`.

- [ ] **Step 3: Add `merge_line_fragments` to `ocr.rs`**

Append to `crates/invoice-parse/src/ocr.rs`, before the `#[cfg(test)]` block:

```rust
/// 把同一行内水平相邻的碎片框合并成一个。
///
/// 有些 OFD 把一个词拆成多个 TextObject（"发"/"票号"/"码："），
/// 不合并就找不到 "发票号码" 这个标签。
/// `max_gap` 是允许的最大水平间隙（像素）：小于它就认为属于同一串文本。
pub fn merge_line_fragments(mut boxes: Vec<TextBox>, max_gap: f32) -> Vec<TextBox> {
    if boxes.is_empty() {
        return boxes;
    }
    // 先按行（y 中心）再按 x 排序
    boxes.sort_by(|a, b| {
        a.center_y()
            .partial_cmp(&b.center_y())
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.x.partial_cmp(&b.x).unwrap_or(std::cmp::Ordering::Equal))
    });

    let mut out: Vec<TextBox> = Vec::with_capacity(boxes.len());
    for b in boxes {
        match out.last_mut() {
            Some(prev)
                if (prev.center_y() - b.center_y()).abs() <= SAME_LINE_TOLERANCE
                    && b.x - prev.right() <= max_gap
                    && b.x >= prev.x =>
            {
                // 同一行且紧邻：拼接文本，扩展边框
                prev.text.push_str(&b.text);
                let right = prev.right().max(b.right());
                prev.width = right - prev.x;
                prev.height = prev.height.max(b.height);
                prev.confidence = prev.confidence.min(b.confidence);
            }
            _ => out.push(b),
        }
    }
    out
}
```

`SAME_LINE_TOLERANCE` is already declared in this file, and `center_y`/`right` already exist on `TextBox` — but they are private (`fn`, not `pub fn`). Since `merge_line_fragments` lives in the same module, no visibility change is needed.

- [ ] **Step 4: Give `locate_vat_fields` a `level` parameter**

OFD structured text is L1, OCR pixels are L2, so the level can no longer be hardcoded. In `crates/invoice-parse/src/ocr.rs`, change the signature and the two affected lines:

```rust
pub fn locate_vat_fields(
    boxes: &[TextBox],
    path: &Path,
    level: ParseLevel,
) -> Result<ParsedInvoice, ParseError> {
```

and in the returned struct literal replace `parse_level: ParseLevel::L2,` with:

```rust
        parse_level: level,
```

Update the existing `ocr.rs` tests: every `locate_vat_fields(&boxes, Path::new("a.jpg"))` call becomes `locate_vat_fields(&boxes, Path::new("a.jpg"), ParseLevel::L2)`. Find them with:

```bash
grep -n "locate_vat_fields(" crates/invoice-parse/src/*.rs
```

- [ ] **Step 5: Implement `extract_text_boxes` and `parse_invoice_ofd_text`**

Append to `crates/invoice-parse/src/ofd_text.rs`, before the `#[cfg(test)]` block:

```rust
/// 从 OFD 中提取所有页面的文本框，坐标已换算为像素。
pub fn extract_text_boxes(bytes: &[u8], path: &Path) -> Result<Vec<TextBox>, ParseError> {
    let mut zip = zip::ZipArchive::new(Cursor::new(bytes.to_vec())).map_err(|e| {
        ParseError::MalformedFormat {
            path: path.to_path_buf(),
            format: "OFD",
            detail: format!("不是有效的 ZIP 容器: {e}"),
        }
    })?;

    // 收集所有 Content.xml（可能多页）
    let content_names: Vec<String> = (0..zip.len())
        .filter_map(|i| zip.by_index(i).ok().map(|f| f.name().to_string()))
        .filter(|n| {
            let l = n.to_lowercase();
            l.ends_with("content.xml") && l.contains("/pages/")
        })
        .collect();

    if content_names.is_empty() {
        return Err(ParseError::MissingField {
            path: path.to_path_buf(),
            field: "Content.xml".to_string(),
        });
    }

    let mut boxes = Vec::new();
    for name in content_names {
        let mut raw = Vec::new();
        zip.by_name(&name)
            .map_err(|e| ParseError::MalformedFormat {
                path: path.to_path_buf(),
                format: "OFD",
                detail: format!("读取 {name} 失败: {e}"),
            })?
            .read_to_end(&mut raw)
            .map_err(|e| ParseError::Io {
                path: path.to_path_buf(),
                source: e,
            })?;
        boxes.extend(parse_content_xml(&raw, path)?);
    }
    Ok(boxes)
}

/// 解析单个 Content.xml。
///
/// 必须用真正的 XML 解析器：某些开票平台在 TextObject 与 TextCode 之间
/// 夹了 CGTransform / FillColor 等子元素，正则匹配不到。
fn parse_content_xml(xml: &[u8], path: &Path) -> Result<Vec<TextBox>, ParseError> {
    let mut reader = Reader::from_reader(xml);
    reader.config_mut().trim_text(true);

    let mut out = Vec::new();
    let mut buf = Vec::new();
    // 当前 TextObject 的 Boundary（毫米）
    let mut pending: Option<(f32, f32, f32, f32)> = None;
    let mut in_text_code = false;
    let mut text = String::new();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                let name = local_tag(e.name().as_ref());
                if name == "TextObject" {
                    pending = e
                        .attributes()
                        .flatten()
                        .find(|a| local_tag(a.key.as_ref()) == "Boundary")
                        .and_then(|a| parse_boundary(&a.value));
                    text.clear();
                } else if name == "TextCode" {
                    in_text_code = true;
                }
            }
            Ok(Event::Text(e)) if in_text_code => {
                if let Ok(s) = e.unescape() {
                    text.push_str(&s);
                }
            }
            Ok(Event::End(e)) => {
                let name = local_tag(e.name().as_ref());
                if name == "TextCode" {
                    in_text_code = false;
                } else if name == "TextObject" {
                    if let Some((x, y, w, h)) = pending.take() {
                        let t = text.trim();
                        if !t.is_empty() {
                            out.push(TextBox {
                                text: t.to_string(),
                                x: x * MM_TO_PX,
                                y: y * MM_TO_PX,
                                width: w * MM_TO_PX,
                                height: h * MM_TO_PX,
                                // 文件自带文本，不是识别结果
                                confidence: 1.0,
                            });
                        }
                    }
                    text.clear();
                }
            }
            Ok(Event::Eof) => break,
            Err(e) => {
                return Err(ParseError::MalformedFormat {
                    path: path.to_path_buf(),
                    format: "OFD",
                    detail: format!("版式 XML 解析失败: {e}"),
                })
            }
            _ => {}
        }
        buf.clear();
    }
    Ok(out)
}

/// 去掉命名空间前缀：`ofd:TextObject` → `TextObject`
fn local_tag(raw: &[u8]) -> String {
    let s = String::from_utf8_lossy(raw);
    s.rsplit(':').next().unwrap_or(&s).to_string()
}

/// `Boundary="x y w h"`，单位毫米。
fn parse_boundary(raw: &[u8]) -> Option<(f32, f32, f32, f32)> {
    let s = String::from_utf8_lossy(raw);
    let v: Vec<f32> = s.split_whitespace().filter_map(|p| p.parse().ok()).collect();
    if v.len() == 4 {
        Some((v[0], v[1], v[2], v[3]))
    } else {
        None
    }
}

/// OFD 版式文本 → ParsedInvoice（L1）。
pub fn parse_invoice_ofd_text(bytes: &[u8], path: &Path) -> Result<ParsedInvoice, ParseError> {
    let boxes = extract_text_boxes(bytes, path)?;
    let merged = merge_line_fragments(boxes, FRAGMENT_MAX_GAP);
    locate_vat_fields(&merged, path, ParseLevel::L1)
}
```

- [ ] **Step 6: Run the tests to verify they pass**

```bash
cargo test -p invoice-parse --lib ofd_text 2>&1 | tail -25
```

Expected: all `ofd_text` tests pass. If `fragmented_layout_merges_and_locates` or `merged_text_reassembles_label` fails, tune `FRAGMENT_MAX_GAP` — print the gaps first:

```bash
cargo run -q -p invoice-parse -- dump-text fixtures/samples/11-meituan-34ee412d.ofd 2>/dev/null | head -30
```

Adjust the constant so intra-label gaps merge and inter-field gaps do not. Do not raise it past the narrowest label→value gap, or labels will swallow their own values.

- [ ] **Step 7: Wire the fallback into `ofd.rs`**

In `crates/invoice-parse/src/ofd.rs`, replace the body of `parse_invoice_ofd`:

```rust
pub fn parse_invoice_ofd(
    ofd_bytes: &[u8],
    path: &Path,
    hints: &TagHints,
    ticket_type: TicketType,
) -> Result<ParsedInvoice, ParseError> {
    // 优先走结构化 XML（L0）——有语义标签，最可靠
    if let Ok(xml_bytes) = extract_invoice_xml(ofd_bytes, path) {
        if let Ok(invoice) =
            crate::xml::parse_invoice_xml(&xml_bytes, path, hints, ticket_type)
        {
            return Ok(invoice);
        }
    }
    // 退回版式文本（L1）：实测 23 个样本全是版式格式，没有结构化标签
    crate::ofd_text::parse_invoice_ofd_text(ofd_bytes, path)
}
```

- [ ] **Step 8: Measure against real OFD samples**

```bash
cargo test -p invoice-parse 2>&1 | tail -12
cargo run -q -p invoice-parse -- verify-all 2>/dev/null | grep -A4 "^| 格式"
```

Expected: the `ofd` row's parse-success count rises from 0 toward 7/8. `02-unknown-f6f7c6b1.ofd` stays failed — its ZIP central directory is corrupt, which is bad data, not a code defect. Record the actual count.

- [ ] **Step 9: Hand-verify the newly parsing OFD samples**

Repeat Task 3 Steps 4–5 for each OFD that now parses. `dump-text` on an `.ofd` now prints text with coordinates, so values can be read directly.

- [ ] **Step 10: Commit**

```bash
git add crates/invoice-parse/src/ofd_text.rs crates/invoice-parse/src/lib.rs \
        crates/invoice-parse/src/ocr.rs crates/invoice-parse/src/ofd.rs \
        fixtures/manifest.toml docs/ground-truth-log.md
git commit -m "feat: 从 OFD 版式 XML 直接提取文本与坐标（L1，无需 java/OCR）"
```

---

## Task 5: Extract PDF text with coordinates, and contain the `pdf-extract` panic

**Files:**
- Create: `crates/invoice-parse/src/pdf_boxes.rs`
- Modify: `crates/invoice-parse/src/pdf.rs:24-42` (`extract_text`, `has_text_layer`)
- Modify: `crates/invoice-parse/src/lib.rs` (add `pub mod pdf_boxes;`)
- Modify: `crates/invoice-parse/Cargo.toml` (add `lopdf = "0.34"`)
- Modify: `crates/invoice-parse/src/main.rs:215-306` (remove the outer `catch_unwind`)
- Test: inline `#[cfg(test)]` in `pdf_boxes.rs` and `pdf.rs`

**Interfaces:**
- Consumes: `model::{ParseError, ParseLevel, ParsedInvoice}`, `ocr::{TextBox, locate_vat_fields, merge_line_fragments}`
- Produces:
  - `pdf_boxes::extract_text_boxes(pdf_bytes: &[u8], path: &Path) -> Result<Vec<TextBox>, ParseError>`
  - `pdf_boxes::parse_invoice_pdf_boxes(pdf_bytes: &[u8], path: &Path) -> Result<ParsedInvoice, ParseError>`
  - `pdf::TextLayer { Present(String), Absent, Unsupported { detail: String } }`
  - `pdf::probe_text_layer(pdf_bytes: &[u8], path: &Path) -> TextLayer`
  - `pdf::extract_text(...) -> Result<String, ParseError>` — same signature, now panic-free

**Why this task changed shape.** The original plan assumed most PDFs lacked a text layer and needed OCR. Measurement says otherwise. Of 43 PDF samples, `pdf-extract` returns text for 41, errors on 1 (`01-unknown-1cb9ce98.pdf`, invalid trailer), panics on 1 (`06-unknown-fbf5dc58.pdf`, the `Identity-H` assert).

The real defect: `pdf-extract`'s flat text **drops field values and keeps only labels**. On `04-unknown-6554429d.pdf` it emits `发票号码：` followed by nothing, then `开票日期：` followed by nothing — the regexes find labels and no values. The values are in the file; they are separate text runs positioned to the right of each label, and flattening to a string in document order separates them.

Verified fix: `pdf_extract::OutputDev` is a public trait whose `output_character` receives a per-glyph `Transform`, and `pdf_extract::output_doc(&Document, &mut dyn OutputDev)` is public. Collecting glyphs into positioned boxes and pairing label→value geometrically recovers every field. Prototyped, all three correct:

| sample | 发票号码 | 开票日期 |
|---|---|---|
| `04-unknown-6554429d.pdf` | `26312000003445962271` | `2026年06月03日` |
| `05-unknown-b4511bc3.pdf` | `26112000002267104336` | `2026年06月04日` |
| `08-meituan-42f0da2f.pdf` | `26117000000812661045` | `2026年06月08日` |

This makes the PDF path structurally identical to Task 4's OFD path: produce `Vec<TextBox>`, merge fragments, hand to the same `locate_vat_fields`. One field-location implementation serves OFD, PDF, and OCR.

**Two samples are unrecoverable without a rasteriser, and that is acceptable.** Cross-checked with `lopdf` directly: `06` fails with `ToUnicode CMap error: Could not parse ToUnicodeCMap`, `01` fails with `Invalid file trailer`. Neither is a code defect. Record both as bad data with the specific cause; do not add a rasteriser to chase them.

**Coordinate-system warning.** PDF user space has y increasing **upward** from the bottom-left; `ocr::TextBox` uses screen coordinates with y increasing **downward**. Convert with `y_screen = (media_box_height - y_pdf) * PT_TO_PX`. If you skip this, `merge_line_fragments` still works (it compares absolute differences) but reading order inverts and any future top-down heuristic silently breaks.

- [ ] **Step 1: Add `lopdf`, then write the failing tests**

Add to `crates/invoice-parse/Cargo.toml` under `[dependencies]`:

```toml
lopdf = "0.34"
```

Create `crates/invoice-parse/src/pdf_boxes.rs` with the module doc, imports, constants, and tests — no implementation yet. The module doc must state why flat text is insufficient, so the next reader does not "simplify" this back into `extract_text_from_mem`.

Constants to define: `PT_TO_PX: f32 = 96.0 / 72.0`, `SAME_BASELINE: f64 = 1.0` (same-baseline threshold in user space), `GLYPH_GAP_RATIO: f64 = 0.9` (max gap between glyphs in one box, as a multiple of font size), `FRAGMENT_MAX_GAP: f32 = 8.0` (max gap when merging same-line fragments, in pixels).

Write these five tests:

1. `recovers_values_that_flat_text_drops` — assert `pdf_extract::extract_text_from_mem` does **not** contain `26312000003445962271`, then assert `extract_text_boxes` does. This test locks in the module's reason for existing: if `pdf-extract` is ever fixed upstream, it fails and tells you to re-evaluate.
2. `label_and_value_land_on_the_same_line` — find the box containing `发票号码`, take the nearest box to its right within 5px of the same `center_y`, assert its text is `26312000003445962271`.
3. `coordinates_are_pixels_not_points` — the invoice-number box's `x` must be near `438.0 * PT_TO_PX ≈ 584`, not `438`.
4. `parse_invoice_pdf_boxes_yields_l1` — `parse_level == ParseLevel::L1`, and `invoice_number` / `issue_date` / `total_amount` match `26312000003445962271` / `2026-06-03` / `1500.00`.
5. `cmap_panic_is_contained_not_propagated` — `extract_text_boxes` on `06-unknown-fbf5dc58.pdf` returns `Err`, never unwinds. Guard every file read with `let Ok(bytes) = std::fs::read(p) else { return }` so the suite still runs if fixtures are absent.

- [ ] **Step 2: Add the module and run the tests to verify they fail**

Add `pub mod pdf_boxes;` to `crates/invoice-parse/src/lib.rs`, keeping alphabetical order.

```bash
export PATH="$HOME/.cargo/bin:$PATH"
cd /home/holo/work-tools
cargo test -p invoice-parse --lib pdf_boxes 2>&1 | tail -20
```

Expected: `cannot find function extract_text_boxes`. If it compiles, the tests are not testing anything yet — fix that before continuing.

- [ ] **Step 3: Implement the `OutputDev` collector**

Define a private `struct BoxCollector { boxes: Vec<TextBox>, cur: Option<Partial>, page_height: f64 }` where `Partial` holds the in-progress text plus its user-space `x`, `y`, `width`, `font_size`.

Implement `OutputDev` for it. The two methods that matter:

- `begin_page` — record `media_box.height()` (or `top - bottom`) into `page_height`; this is what makes the y-flip possible. Flush any in-progress box first.
- `output_character(trm, width, _spacing, font_size, c)` — the glyph's user-space origin is `(trm.m31, trm.m32)` and its advance is `width * font_size * trm.m11`. Append to `cur` when the baseline matches within `SAME_BASELINE` **and** the horizontal gap from the current box's right edge is under `font_size * GLYPH_GAP_RATIO`; otherwise flush and start a new box.

`end_page` and `end_word` must flush. `begin_word`, `end_line`, `stroke`, and `fill` are no-ops returning `Ok(())` — `stroke`/`fill` already have default bodies, so overriding them is optional.

On flush, drop boxes whose text is entirely whitespace, convert to `TextBox` with `x = x_pt * PT_TO_PX`, `y = (page_height - y_pt) * PT_TO_PX`, `width`/`height` likewise scaled, and `confidence = 1.0` — this is deterministic file text, not a guess.

- [ ] **Step 4: Implement `extract_text_boxes` and `parse_invoice_pdf_boxes`**

`extract_text_boxes` must wrap the whole `lopdf::Document::load_mem` + `pdf_extract::output_doc` sequence in `std::panic::catch_unwind`, because `pdf-extract` asserts on unsupported CMaps rather than returning an error. Bytes are `&[u8]`; `catch_unwind` requires `UnwindSafe`, so pass an owned `Vec<u8>` into the closure or wrap the borrow in `std::panic::AssertUnwindSafe` — prefer `AssertUnwindSafe` to avoid a copy of every PDF.

Silence the default panic printer around the call so `verify-all` output stays readable:

```rust
let prev = std::panic::take_hook();
std::panic::set_hook(Box::new(|_| {}));
let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| { /* ... */ }));
std::panic::set_hook(prev);
```

Map all three failure modes to `ParseError::MalformedFormat { path, format: "PDF", detail }` with distinct details: load failure carries the `lopdf` message, panic carries `字体 CMap 不受支持（pdf-extract 内部断言失败）`, and an empty box list carries `未提取到任何文本框（疑似扫描件）`.

`parse_invoice_pdf_boxes` is then three lines: extract, `merge_line_fragments(boxes, FRAGMENT_MAX_GAP)`, `locate_vat_fields(&merged, path, ParseLevel::L1)`.

- [ ] **Step 5: Run the tests to verify they pass**

```bash
cargo test -p invoice-parse --lib pdf_boxes 2>&1 | tail -20
```

If `label_and_value_land_on_the_same_line` fails, print the actual boxes before touching any constant:

```bash
cargo run -q -p invoice-parse -- dump-text fixtures/samples/04-unknown-6554429d.pdf 2>/dev/null | head -40
```

Tune `GLYPH_GAP_RATIO` only if glyphs of one value are splitting apart or a label is swallowing its value. Do not raise `FRAGMENT_MAX_GAP` past the narrowest label→value gap.

- [ ] **Step 6: Add `TextLayer` and panic containment to `pdf.rs`**

Add to `crates/invoice-parse/src/pdf.rs`:

```rust
/// PDF 文本层的探测结果。
///
/// 把"没有文本层"和"文本层取不出来"分开：
/// 前者是扫描件，该走 OCR；后者是字体编码不受支持，也走 OCR，
/// 但报告里要能区分，否则没法判断是数据问题还是代码问题。
pub enum TextLayer {
    Present(String),
    Absent,
    Unsupported { detail: String },
}
```

Add `probe_text_layer(pdf_bytes: &[u8], path: &Path) -> TextLayer` using the same hook-suppression + `catch_unwind` pattern as Step 4. Treat fewer than `MIN_TEXT_CHARS` non-whitespace characters as `Absent` — define that constant rather than inlining a magic number, and note in a comment that it exists to catch PDFs whose only text is a watermark.

Rewrite `extract_text` as a thin match over `probe_text_layer`, mapping `Absent` and `Unsupported` to `ParseError::MalformedFormat` with distinct details. Rewrite `has_text_layer` as `matches!(probe_text_layer(...), TextLayer::Present(_))`.

- [ ] **Step 7: Remove the now-redundant outer `catch_unwind` in `verify_all`**

In `crates/invoice-parse/src/main.rs`, the `let parsed: anyhow::Result<ParsedInvoice> = ...` block wraps parsing in `catch_unwind` and flattens every crash to `解析器崩溃`. That was a blunt guard against this exact panic. Now that `pdf.rs` and `pdf_boxes.rs` contain it at the source, remove the wrapper so real panics surface as bugs instead of being silently absorbed. Keep the call site's error handling — only the `catch_unwind` layer goes.

- [ ] **Step 8: Wire the PDF path to use positioned boxes**

Find where `verify_all` (and any other caller) parses PDFs. Route through `pdf_boxes::parse_invoice_pdf_boxes` first; fall back to the existing flat-text parser only if the box path returns `Err`. Order matters: boxes are strictly more informative, and the flat path is what loses values.

- [ ] **Step 9: Run the tests and measure**

```bash
cargo test -p invoice-parse 2>&1 | tail -12
cargo run -q -p invoice-parse -- verify-all 2>/dev/null | grep -A6 "^| 格式"
```

Expected: no `panicked` line on stderr, and the `pdf` row's parse-success count rises substantially — the prototype recovered all three required fields on every non-didi sample it was tried against. Record the actual count; do not assume a number.

- [ ] **Step 10: Commit**

```bash
git add crates/invoice-parse/src/pdf_boxes.rs crates/invoice-parse/src/pdf.rs \
        crates/invoice-parse/src/lib.rs crates/invoice-parse/src/main.rs \
        crates/invoice-parse/Cargo.toml Cargo.lock
git commit -m "feat: 从 PDF 提取带坐标文本框（L1），就地捕获 pdf-extract 的 CMap 断言"
```

## Task 6: Classify invoice vs non-invoice before parsing

**Files:**
- Create: `crates/invoice-parse/src/classifier.rs`
- Modify: `crates/invoice-parse/src/lib.rs` (add `pub mod classifier;`)
- Modify: `crates/invoice-parse/src/main.rs` (add `classify-all` subcommand; `verify_all` emits `Skipped`)
- Test: inline `#[cfg(test)]` in `classifier.rs`

**Interfaces:**
- Consumes: `pdf::{probe_text_layer, TextLayer}`, `ofd_text::extract_text_boxes` (Task 4), `xml::collect_leaf_elements`
- Produces:
  - `classifier::DocKind { Invoice, NonInvoice { reason: String }, Undetermined }`
  - `classifier::classify_text(text: &str) -> DocKind`
  - `classifier::classify_file(path: &Path) -> DocKind`

**Why:** The 12 `didi-*.pdf` samples fail with `找不到必需字段 total_amount` because they are ride itineraries (行程单), not invoices. Counting those as parser failures understates real accuracy and, worse, invites "fixes" that would make the parser hallucinate an amount out of a document that has none. Classification must precede parsing.

**Ground truth for this task comes from Task 3's `is_invoice` field.** The classifier is scored against human judgement, not the other way round.

- [ ] **Step 1: Write the failing tests**

Create `crates/invoice-parse/src/classifier.rs`:

```rust
//! 判定一个文件是不是发票。
//!
//! 样本里混着行程单、订单详情、差旅报告——它们没有发票号码和价税合计。
//! 把它们算进解析失败会低估真实准确率，更糟的是会诱导"修复"成
//! 从没有金额的文档里编出一个金额来。所以分类必须在解析之前。

use std::path::Path;

#[derive(Debug, Clone, PartialEq)]
pub enum DocKind {
    Invoice,
    NonInvoice { reason: String },
    /// 文本不足以判定（例如纯扫描件还没 OCR）
    Undetermined,
}

/// 发票必备的强特征：出现其一即高度可能是发票
const INVOICE_STRONG: &[&str] = &[
    "发票号码", "价税合计", "增值税专用发票", "增值税普通发票", "电子发票",
];

/// 发票常见弱特征：需要凑够 MIN_WEAK 个
const INVOICE_WEAK: &[&str] = &[
    "开票日期", "销售方", "购买方", "纳税人识别号", "税率", "税额", "开票人",
];

const MIN_WEAK: usize = 2;

/// 明确的非发票标志：出现即判非发票，除非同时有强特征
const NON_INVOICE: &[&str] = &[
    "行程单", "订单详情", "交易记录", "账单明细", "差旅报告",
    "消费明细", "支付凭证", "行程报告",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strong_marker_alone_is_invoice() {
        assert_eq!(classify_text("发票号码：26112000002208097411"), DocKind::Invoice);
        assert_eq!(classify_text("价税合计（大写）肆拾柒圆肆角"), DocKind::Invoice);
    }

    #[test]
    fn two_weak_markers_are_invoice() {
        let t = "开票日期 2026-06-01\n销售方 某某公司";
        assert_eq!(classify_text(t), DocKind::Invoice);
    }

    #[test]
    fn one_weak_marker_alone_is_undetermined() {
        assert_eq!(classify_text("开票日期 2026-06-01"), DocKind::Undetermined);
    }

    #[test]
    fn itinerary_is_non_invoice() {
        match classify_text("滴滴出行 行程单\n共 3 单，合计 88.00 元") {
            DocKind::NonInvoice { reason } => assert!(reason.contains("行程单"), "reason={reason}"),
            other => panic!("应判非发票，实际 {other:?}"),
        }
    }

    #[test]
    fn order_detail_is_non_invoice() {
        assert!(matches!(
            classify_text("订单详情\n下单时间 2026-06-01"),
            DocKind::NonInvoice { .. }
        ));
    }

    /// 关键歧义：真发票的附页可能写着"行程单"字样，
    /// 但只要同时出现强特征（发票号码），就必须判为发票。
    #[test]
    fn strong_marker_overrides_non_invoice_marker() {
        let t = "电子发票（普通发票）\n发票号码：26112000002208097411\n附：行程单";
        assert_eq!(classify_text(t), DocKind::Invoice);
    }

    #[test]
    fn empty_text_is_undetermined() {
        assert_eq!(classify_text(""), DocKind::Undetermined);
        assert_eq!(classify_text("   \n  \t "), DocKind::Undetermined);
    }

    /// 分类器必须与 Task 3 的人工判定一致。
    /// 这是对照人工标注跑的回归，不是对照解析器输出。
    #[test]
    fn agrees_with_human_labels_on_real_samples() {
        use crate::manifest::Manifest;
        let Ok(m) = Manifest::load(Path::new("../../fixtures/manifest.toml")) else {
            eprintln!("清单缺失，跳过");
            return;
        };
        let mut checked = 0;
        let mut wrong = Vec::new();
        for s in &m.samples {
            let Some(human) = s.is_invoice else { continue };
            let p = Path::new("../../fixtures").join(&s.path);
            let got = classify_file(&p);
            let agrees = match (&got, human) {
                (DocKind::Invoice, true) => true,
                (DocKind::NonInvoice { .. }, false) => true,
                // Undetermined 不算错——它诚实地表示"看不出来"
                (DocKind::Undetermined, _) => true,
                _ => false,
            };
            checked += 1;
            if !agrees {
                wrong.push(format!("{}: 人工={human} 分类器={got:?}", s.path.display()));
            }
        }
        if checked == 0 {
            eprintln!("尚无人工 is_invoice 标注，跳过");
            return;
        }
        assert!(wrong.is_empty(), "与人工判定不一致 {}/{checked}:\n{}", wrong.len(), wrong.join("\n"));
    }
}
```

- [ ] **Step 2: Add the module and run the tests to verify they fail**

Add `pub mod classifier;` to `crates/invoice-parse/src/lib.rs`, then:

```bash
export PATH="$HOME/.cargo/bin:$PATH"
cd /home/holo/work-tools
cargo test -p invoice-parse --lib classifier 2>&1 | tail -20
```

Expected: compile errors — `classify_text` and `classify_file` not found.

- [ ] **Step 3: Implement the classifier**

Append to `crates/invoice-parse/src/classifier.rs`, before the `#[cfg(test)]` block:

```rust
/// 按文本内容判定。强特征优先于非发票特征：
/// 真发票的附页也可能写着"行程单"。
pub fn classify_text(text: &str) -> DocKind {
    if text.trim().is_empty() {
        return DocKind::Undetermined;
    }

    if INVOICE_STRONG.iter().any(|m| text.contains(m)) {
        return DocKind::Invoice;
    }

    if let Some(marker) = NON_INVOICE.iter().find(|m| text.contains(**m)) {
        return DocKind::NonInvoice {
            reason: format!("含非发票标志「{marker}」且无发票强特征"),
        };
    }

    let weak = INVOICE_WEAK.iter().filter(|m| text.contains(**m)).count();
    if weak >= MIN_WEAK {
        DocKind::Invoice
    } else {
        DocKind::Undetermined
    }
}


/// 读取文件、取出可判定的文本，再分类。
/// 拿不到文本时返回 Undetermined，绝不猜。
pub fn classify_file(path: &Path) -> DocKind {
    let Ok(bytes) = std::fs::read(path) else {
        return DocKind::Undetermined;
    };
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();

    let text = match ext.as_str() {
        "xml" => crate::xml::collect_leaf_elements(&bytes)
            .map(|ls| {
                ls.iter()
                    .map(|l| format!("{} {}", l.name, l.text))
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_default(),
        "ofd" => crate::ofd_text::extract_text_boxes(&bytes, path)
            .map(|bs| bs.iter().map(|b| b.text.clone()).collect::<Vec<_>>().join("\n"))
            .unwrap_or_default(),
        "pdf" => match crate::pdf::probe_text_layer(&bytes, path) {
            crate::pdf::TextLayer::Present(t) => t,
            // 无文本层/编码不支持：此刻无法判定，等 OCR 出文本再说
            _ => String::new(),
        },
        // 图片需要先 OCR，这里不猜
        _ => String::new(),
    };

    classify_text(&text)
}
```

- [ ] **Step 4: Run the tests to verify they pass**

```bash
cargo test -p invoice-parse --lib classifier 2>&1 | tail -20
```

Expected: all 8 tests pass. `agrees_with_human_labels_on_real_samples` passes trivially if Task 3 has not yet filled `is_invoice`; it becomes a real check once labels exist.

- [ ] **Step 5: Add the `classify-all` subcommand**

Add to the `Commands` enum in `crates/invoice-parse/src/main.rs`:

```rust
    /// 对全部样本跑分类器，打印发票/非发票判定
    ClassifyAll,
```

Dispatch arm:

```rust
        Commands::ClassifyAll => classify_all(),
```

Function:

```rust
fn classify_all() -> anyhow::Result<()> {
    use invoice_parse::classifier::{classify_file, DocKind};
    use invoice_parse::manifest::Manifest;

    let manifest = Manifest::load(Path::new("fixtures/manifest.toml"))?;
    let (mut inv, mut non, mut und) = (0, 0, 0);

    for sample in &manifest.samples {
        let full = PathBuf::from("fixtures").join(&sample.path);
        let kind = classify_file(&full);
        let label = match &kind {
            DocKind::Invoice => { inv += 1; "发票".to_string() }
            DocKind::NonInvoice { reason } => { non += 1; format!("非发票（{reason}）") }
            DocKind::Undetermined => { und += 1; "待定".to_string() }
        };
        let agree = match (&kind, sample.is_invoice) {
            (_, None) => "",
            (DocKind::Invoice, Some(true)) | (DocKind::NonInvoice { .. }, Some(false)) => " ✓",
            (DocKind::Undetermined, _) => " ?",
            _ => " ✗与人工不一致",
        };
        println!("{:<44} {}{}", sample.path.display(), label, agree);
    }

    println!("\n发票 {inv} / 非发票 {non} / 待定 {und}");
    Ok(())
}
```

- [ ] **Step 6: Run it and cross-check against human labels**

```bash
cargo run -q -p invoice-parse -- classify-all 2>/dev/null | tail -25
```

Expected: the 12 `didi-*.pdf` samples show 非发票; XML and OFD invoices show 发票; image samples show 待定. Any `✗与人工不一致` line means the keyword lists need adjusting — adjust `INVOICE_STRONG`/`NON_INVOICE`, never the human label.

- [ ] **Step 7: Make `verify_all` skip non-invoices**

In `verify_all`, insert immediately after `let full_path = ...`:

```rust
        // 非发票不进任何分母：把行程单算作解析失败会低估真实准确率
        let kind = invoice_parse::classifier::classify_file(&full_path);
        if let invoice_parse::classifier::DocKind::NonInvoice { reason } = kind {
            outcomes.push(SampleOutcome {
                path: sample.path.display().to_string(),
                format: sample.format.clone(),
                result: OutcomeKind::Skipped { reason },
            });
            continue;
        }
```

- [ ] **Step 8: Re-run `verify-all`**

```bash
cargo run -q -p invoice-parse -- verify-all 2>/dev/null | head -22
```

Expected: 已跳过 count around 12, and the parse-success denominator drops accordingly.

- [ ] **Step 9: Run the full suite and commit**

```bash
cargo test -p invoice-parse 2>&1 | tail -15
git add crates/invoice-parse/src/classifier.rs crates/invoice-parse/src/lib.rs crates/invoice-parse/src/main.rs
git commit -m "feat: 解析前判定发票/非发票，非发票不计入通过率分母"
```

---

## Task 7: Native Rust OCR via `ort` + PP-OCRv6_small

**Files:**
- Create: `crates/invoice-parse/src/ocr_onnx.rs` (session management, det + rec inference)
- Create: `crates/invoice-parse/src/ocr_db.rs` (DBNet post-processing: threshold, contours, unclip)
- Modify: `crates/invoice-parse/src/lib.rs` (add both modules)
- Modify: `crates/invoice-parse/Cargo.toml` (add `ort`, `ndarray`)
- Modify: `crates/invoice-parse/src/main.rs` (route images and text-less PDFs to OCR)
- Create: `.gitignore` entries for `vendor/` and `models/*.onnx`
- Modify: `models/README.md` (record real sizes and acquisition commands)
- Test: inline `#[cfg(test)]` in both new modules

**Interfaces:**
- Consumes: `model::{ParseError, ParseLevel, ParsedInvoice}`, `ocr::{TextBox, locate_vat_fields, merge_line_fragments}`, `image` (already a dependency)
- Produces:
  - `ocr_onnx::OcrEngine::new(model_dir: &Path) -> Result<OcrEngine, ParseError>`
  - `OcrEngine::recognize(&self, img: &image::DynamicImage) -> Result<Vec<TextBox>, ParseError>`
  - `ocr_onnx::parse_invoice_ocr(bytes: &[u8], path: &Path) -> Result<ParsedInvoice, ParseError>`
  - `ocr_db::boxes_from_prob_map(prob: &ndarray::ArrayView2<f32>, ...) -> Vec<Quad>`

**Scope is small — check this before writing code.** After Tasks 4–6, OCR is needed for **6 images only**: `26-unknown-d3006c0b.jpg`, `35-meituan-21a391a6.jpg`, `42-meituan-b6c3341f.jpg`, `60-unknown-ccff78f5.jpg`, `61-unknown-c27071a2.jpg`, `62-unknown-70a24c65.jpg`. The two broken PDFs (`01`, `06`) cannot be rasterised in pure Rust and are recorded as bad data in Task 5 — **do not** add a rasteriser, and do not add `pdfium`. If your OCR path compiles but you find yourself needing to render a PDF page, stop and re-read Task 5.

**Everything below is measured, not guessed.** Models, runtime library, and every pre/post-processing parameter are already verified present and correct — see Global Constraints for paths, sizes, and the exact values. A working Python reference of this pipeline is at `/tmp/ocr_ref.py`; it reads `发票号码:26154000000018314746` and `价税合计(元):¥24.60` from `35-meituan-21a391a6.jpg`. When your Rust output disagrees, diff intermediate tensors against it rather than tweaking thresholds blind.

**Build and run requirements.** `ort` is configured with `load-dynamic`, so the library is resolved at runtime from `ORT_DYLIB_PATH`:

```bash
export PATH="$HOME/.cargo/bin:$PATH"
export ORT_DYLIB_PATH=/home/holo/work-tools/vendor/onnxruntime/lib/libonnxruntime.so
```

Every `cargo test` and `cargo run` in this task needs that variable. Set it once per shell. A missing or wrong path surfaces as a runtime dylib-load error, not a compile error.

- [ ] **Step 1: Add dependencies and confirm `ort` links before writing any logic**

Add to `crates/invoice-parse/Cargo.toml`:

```toml
ort = { version = "=2.0.0-rc.13", default-features = false, features = ["std", "ndarray", "load-dynamic"] }
ndarray = "0.17"
```

`ndarray` must be 0.17 and the `std` feature must be on — with `default-features = false` and no `std`, `ndarray::NdFloat` is gated out and `ort` will not compile. This exact combination is verified working.

Write a throwaway test that loads the det model and prints its input shape, then run it. Confirm the dylib resolves and the session opens **before** writing preprocessing — otherwise you will be debugging two things at once.

```bash
cargo test -p invoice-parse --lib ocr_onnx -- --nocapture 2>&1 | tail -20
```

- [ ] **Step 2: Write the failing tests**

In `ocr_db.rs`, test post-processing on synthetic probability maps — no model needed, so these are fast and deterministic:

1. `single_blob_becomes_one_quad` — a 40×20 rectangle of `0.9` inside a zero map yields exactly one quad covering roughly that area.
2. `unclip_expands_the_box` — the returned quad is strictly larger than the raw contour, since `unclip_ratio = 1.4`. Assert the area ratio is between 1.1 and 2.5; without unclip this test fails, which is the point.
3. `low_score_blob_is_dropped` — a blob of `0.3` is rejected by `box_thresh = 0.45`.
4. `blob_below_min_size_is_dropped` — a 1×1 blob yields no quad.

In `ocr_onnx.rs`, test against the real sample, guarded with `let Ok(..) = .. else { return }` so absent models skip rather than fail:

5. `recognizes_invoice_number_from_meituan_jpg` — running the engine on `35-meituan-21a391a6.jpg` produces a box whose text contains `26154000000018314746`.
6. `recognizes_total_amount` — some box contains `24.60`.
7. `parse_yields_l2` — `parse_invoice_ocr` returns `parse_level == ParseLevel::L2` (OCR is pixels, unlike Tasks 4 and 5).
8. `confidence_is_below_one` — OCR confidence must be `< 1.0`, distinguishing it from the deterministic L1 paths that hard-code `1.0`.

- [ ] **Step 3: Implement DBNet post-processing in `ocr_db.rs`**

This is the part with no crate to lean on, so it carries the most risk. Steps, in order:

1. Threshold the probability map at `0.2` into a binary mask.
2. Find connected components. `image` has no contour finder; implement flood-fill labelling (4-connectivity) over the mask — straightforward and easier to verify than a Suzuki-style tracer.
3. For each component, compute its mean probability over the component's pixels and drop it if below `box_thresh = 0.45`. Note: score over the **component's own pixels**, not its bounding rect — using the rect inflates scores for diagonal text.
4. Compute the minimum-area rectangle. A rotating-calipers implementation over the component's convex hull is correct; an axis-aligned bounding rect is an acceptable first cut for these samples since invoice text is not rotated. If you take the axis-aligned shortcut, say so in a comment and in the report — do not leave it silently implied.
5. Apply the unclip expansion by `unclip_ratio = 1.4`: offset the polygon outward by `distance = area * ratio / perimeter`. For an axis-aligned rect this reduces to inflating each side by that distance, which is a few lines. Do not skip this step — without it boxes clip the glyph edges and recognition accuracy drops sharply.
6. Scale quad coordinates from the resized model input back to original image pixels by `x * orig_w / resized_w`, same for y.

- [ ] **Step 4: Implement detection preprocessing and inference in `ocr_onnx.rs`**

Resize so the longest side is at most 960 and both dimensions are multiples of 32 (the det model downsamples by 32; a non-multiple silently distorts the output map). Keep the scale factors — you need them in Step 3.6.

Normalise as `(pixel/255 - mean) / std` with the mean/std in Global Constraints, then transpose to `[1, 3, H, W]`. Note the channel order: `image` gives RGB, and the recorded mean/std are in RGB order, so no BGR swap is needed despite `inference.yml` saying `img_mode: BGR` — that field describes Paddle's own loader, not this pipeline. Getting this backwards degrades accuracy without erroring, so assert it once by checking a known sample's output rather than trusting either reading.

Build the session once and reuse it. Constructing an `ort::session::Session` per image dominates runtime and will make the sweep appear pathologically slow.

- [ ] **Step 5: Implement recognition and CTC decoding**

For each detected quad: crop the axis-aligned bounding rect from the **original** image, resize to height 48 preserving aspect ratio, clamp width to `[16, 1600]`, normalise as `(pixel/255 - 0.5) / 0.5`, and run the rec session.

Decode the `[1, T, 18710]` output as CTC: per timestep take the argmax, skip index 0 (blank), skip a token equal to the previous timestep's token, and map index `k` to `keys[k - 1]`. The off-by-one is load-bearing — index 0 is blank, so the dictionary is offset by one. Average the max-probabilities of the emitted timesteps for `confidence`; if no timestep is emitted, drop the box rather than pushing an empty string.

Load `models/ppocrv6_keys.txt` once, one character per line, trailing newline dropped. Assert on load that the count is 18708 and fail loudly with the actual number otherwise — a truncated dictionary produces plausible-looking but wrong Chinese, which is far worse than a clean failure.

- [ ] **Step 6: Wire the OCR path into `parse_invoice_ocr` and `main.rs`**

`parse_invoice_ocr` decodes bytes with `image::load_from_memory`, runs the engine, then `merge_line_fragments` → `locate_vat_fields(&merged, path, ParseLevel::L2)`. Same tail as Tasks 4 and 5; only the source of the boxes differs.

Resolve the model directory from an env var (`INVOICE_OCR_MODELS`) falling back to `./models`, and return a clear `ParseError` naming the missing file when a model is absent. Route images through this path in `verify_all`; PDFs reach it only when `pdf::probe_text_layer` returns `Absent` — and per the scope note, that case has no rasteriser, so it should record a clear "需要栅格化，暂不支持" error rather than silently returning an empty result.

- [ ] **Step 7: Run the tests**

```bash
export ORT_DYLIB_PATH=/home/holo/work-tools/vendor/onnxruntime/lib/libonnxruntime.so
cargo test -p invoice-parse --lib ocr 2>&1 | tail -20
```

If recognition returns garbled Chinese, the dictionary offset or the rec normalisation is wrong — check those two before touching detection. If boxes are missing entirely, dump the probability map's max value; a max near 0 means preprocessing is wrong, while a healthy max with no boxes means the post-processing thresholds are.

- [ ] **Step 8: Measure against the 6 image samples**

```bash
cargo run -q -p invoice-parse -- verify-all 2>/dev/null | grep -A8 "^| 格式"
```

Record how many of the 6 images now yield all three required fields. `60-unknown-ccff78f5.jpg` is a 240×1200 banner containing only `数字化税票服务平台` — it is not an invoice and Task 6's classifier should exclude it. If it still appears in the denominator, that is a Task 6 gap worth noting, not an OCR failure.

- [ ] **Step 9: Hand-verify OCR results before trusting them**

For each image that now parses, open it and confirm the three fields by eye, then fill `fixtures/manifest.toml`. OCR output is a guess with a confidence score; pasting it into the manifest would make the test assert whatever the model happened to say. This is the one step in the task that cannot be automated.

- [ ] **Step 10: Record real numbers in `models/README.md` and commit**

Replace the speculative PP-OCRv4 text in `models/README.md` with what is actually in use: PP-OCRv6_small, the two ONNX files and their real sizes (9.5 MiB + 20.2 MiB = 30 MiB), the dictionary, the `paddle2onnx --opset_version 14` conversion command, the bcebos URLs, and the vendored `libonnxruntime.so` with its provenance and glibc requirement. State plainly that the model total already meets the 30MB installer budget on its own, so packaging needs a separate decision.

Add to `.gitignore`:

```
vendor/
models/*.onnx
```

```bash
git add crates/invoice-parse/src/ocr_onnx.rs crates/invoice-parse/src/ocr_db.rs \
        crates/invoice-parse/src/lib.rs crates/invoice-parse/src/main.rs \
        crates/invoice-parse/Cargo.toml Cargo.lock models/README.md .gitignore \
        fixtures/manifest.toml
git commit -m "feat: 用 ort + PP-OCRv6_small 做原生 Rust OCR（L2），不再依赖 Python"
```

---

## Task 8: Verify remaining samples and publish the measured rate

**Files:**
- Modify: `fixtures/manifest.toml` (ground truth for newly-parsing samples)
- Modify: `docs/ground-truth-log.md`
- Create: `docs/parse-accuracy-report.md`

**Interfaces:**
- Consumes: everything above; `verify-all` output
- Produces: `docs/parse-accuracy-report.md` — the first accuracy figure in this project backed by hand-verified ground truth

**Why:** Tasks 4–7 added parsing capability; only ground truth converts that into a defensible accuracy number. Tasks 1–3 established the mechanism on ~19 samples. This task extends it to the OFD and OCR samples now parsing for the first time.

- [ ] **Step 1: List newly-parsing samples that still lack ground truth**

```bash
export PATH="$HOME/.cargo/bin:$PATH"
cd /home/holo/work-tools
cargo run -q -p invoice-parse -- verify-all 2>/dev/null \
  | awk '/^## 待人工核对/,0' | grep '^- ' | sed 's/^- //' > /tmp/todo.txt
wc -l /tmp/todo.txt; cat /tmp/todo.txt
```

- [ ] **Step 2: Hand-verify each, following Task 3's rules**

```bash
while read -r p; do
  echo "=============== $p"
  cargo run -q -p invoice-parse -- dump-text "fixtures/$p" 2>/dev/null | head -40
done < /tmp/todo.txt
```

For each, read the values off the document text and add them to `fixtures/manifest.toml`. Same rules as Task 3 Step 4: `issue_date` as `YYYY-MM-DD`, `total_amount` is 价税合计 not 金额, omit anything unreadable, never paste parser output.

For OCR-derived samples specifically: if the OCR text is too garbled to read a value confidently, do **not** guess. Omit the field and note it in the log. A guessed expectation is worse than no expectation — it silently locks in a wrong answer.

- [ ] **Step 3: Run the final measurement**

```bash
cargo run -q -p invoice-parse -- verify-all 2>/dev/null | tee /tmp/final.txt | head -30
```

Record verbatim: 解析成功率, 已核对通过率, per-format table, 已跳过 count.

- [ ] **Step 4: Write the accuracy report**

Create `docs/parse-accuracy-report.md`:

```markdown
# 解析准确率报告

**日期：** 2026-08-05
**方法：** `cargo run -p invoice-parse -- verify-all`，期望值由人工阅读文档填入（见 docs/ground-truth-log.md）

## 度量口径

- **解析成功率** = 产出 ParsedInvoice 的样本 / 非跳过样本。只说明解析器没报错，不说明值对。
- **已核对通过率** = 全部字段与人工核对值一致的样本 / 已填期望值的样本。**这是准确率。**
- 判定为非发票的样本不进任何分母。

## 结果

| 指标 | 数值 |
|---|---|
| 样本总数 | |
| 判为非发票（已跳过） | |
| 解析成功率 | |
| 已核对通过率 | |

### 按格式

| 格式 | 样本 | 解析成功 | 已核对 | 核对通过 |
|---|---|---|---|---|

## 与 spike 报告的差异

Plan 1 的 spike 报告曾给出 29.7% 与 57.6% 两个数字，两者都不成立：
当时清单里 64 条样本的期望值全为空字符串，`Sample::compare` 把空期望
判成不匹配，`verify-all` 的真实输出是 **0/64**。29.7% 实际是"解析器产出了
结构体"的比例，57.6% 则是子代理对"看起来像发票"的样本的主观计数，
没有任何人工核对数据支撑。本报告的数字是首个有人工核对依据的口径。

## 已知未解决

| 问题 | 样本 | 说明 |
|---|---|---|
| ZIP 中央目录损坏 | samples/02-unknown-f6f7c6b1.ofd | 数据缺陷，非代码问题 |

## 尚未核对

列出仍处于"解析成功但无期望值"状态的样本及原因。
```

Fill every cell from `/tmp/final.txt`. Leave no placeholder.

- [ ] **Step 5: Correct the stale figures in the spike report**

Append to `docs/spike-report.md`:

```markdown
---

## 更正（2026-08-05）

本报告先前引用的 29.7% 与 57.6% 自动化率均不成立。当时 `fixtures/manifest.toml`
的 64 条样本期望值全为空字符串，比对逻辑将空期望记为不匹配，`verify-all`
的实际输出是 0/64。准确率在当时不可度量。

以人工核对为基准的口径见 `docs/parse-accuracy-report.md`。

另更正两项技术结论：
- OFD **不需要** java 渲染器或 OCR。版式 XML（`Doc_0/Pages/Page_*/Content.xml`）
  的 `<ofd:TextObject Boundary>` + `<ofd:TextCode>` 直接带文本与精确坐标，
  定级 L1。本机也没有 java。
- PDF 栅格化用 Python 侧的 `pypdfium2`，不用 `pdftoppm`——本机没装 poppler。
```

- [ ] **Step 6: Full suite green, then commit**

```bash
cargo test -p invoice-parse 2>&1 | tail -15
git add fixtures/manifest.toml docs/ground-truth-log.md \
        docs/parse-accuracy-report.md docs/spike-report.md
git commit -m "docs: 发布基于人工核对的解析准确率，更正 spike 报告失效数字"
```

---

## Done When

- `cargo test -p invoice-parse` passes with zero failures.
- `cargo run -p invoice-parse -- verify-all` prints both rates, and 已核对通过率 has a non-zero denominator.
- `docs/parse-accuracy-report.md` states a hand-verified accuracy figure with no placeholder cells.
- `docs/ground-truth-log.md` names who verified what, and lists every unverifiable sample with a reason.
- No `java`, `poppler`, or new Rust crate was introduced.
- 7 of 8 OFD samples parse (the 8th has a corrupt ZIP).
- `verify-all` emits no `panicked` line on stderr.

## Deliberately Out of Scope

- **Raising accuracy to a specific target.** The target is set *after* Task 8 produces the first real measurement. Committing to 90% before knowing the baseline is how the 57.6% figure came about.
- **`is_voided` / signature verification.** Task 8 of the spike established local SM2 verification is not MVP-viable (3/15 samples carry signatures, 0 verify). Unchanged here.
- **Desktop app, SQLite, Concur, packaging.** Separate subsystems, separate plans.
