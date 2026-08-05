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
    use std::collections::BTreeMap;
    use std::fmt::Write as _;

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
                    status: crate::manifest::FieldStatus::Mismatch,
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
