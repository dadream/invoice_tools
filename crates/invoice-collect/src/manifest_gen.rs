use crate::store::SavedSample;

#[derive(Debug, Clone)]
pub struct ManifestEntry {
    pub saved: SavedSample,
    pub format: String,
    pub platform: String,
    pub original_filename: String,
    pub subject: String,
}

/// 生成 manifest.toml 内容。
///
/// 字段名与解析验证计划的 `manifest::Sample` 严格对应。
/// ticket_type 与各期望值留待人工填写。
pub fn render(entries: &[ManifestEntry]) -> String {
    use std::fmt::Write as _;

    let mut out = String::new();

    out.push_str("# 发票样本清单\n#\n");
    out.push_str("# 由 invoice-collect 自动生成。path 与 format 已填好，\n");
    out.push_str("# 其余字段需**人工填写** —— 打开每个样本文件，把实际值抄进来。\n");
    out.push_str("# 这些值是解析器的验收依据，填错会让验证结论失效。\n#\n");
    out.push_str("# ticket_type 取值: Rail | Flight | Hotel | CityTransport | Meal | Other\n");
    out.push_str("# 金额与税率用字符串，保留原始小数位，例如 \"553.00\" \"0.09\"\n");
    out.push_str("# 日期用 YYYY-MM-DD\n");
    out.push_str("# 发票上没有的可选字段，删掉该行即可\n\n");

    if entries.is_empty() {
        out.push_str("# 未采集到任何样本。请检查 invoice-collect probe 的输出，\n");
        out.push_str("# 确认目标日期范围内确实存在带发票附件的邮件。\n");
        return out;
    }

    for e in entries {
        let _ = writeln!(
            out,
            "# 原始文件名: {}",
            sanitize_comment(&e.original_filename)
        );
        let _ = writeln!(out, "# 邮件主题: {}", sanitize_comment(&e.subject));
        let _ = writeln!(
            out,
            "# 平台: {} · 大小: {} 字节",
            e.platform, e.saved.byte_len
        );
        out.push_str("[[sample]]\n");
        let _ = writeln!(out, "path = \"{}\"", e.saved.rel_path);
        let _ = writeln!(out, "format = \"{}\"", e.format);
        out.push_str("ticket_type = \"Other\"       # 待确认\n");
        // 必填字段：留空串，人工必须填
        out.push_str("invoice_number = \"\"\n");
        out.push_str("issue_date = \"\"\n");
        out.push_str("total_amount = \"\"\n");
        // 可选字段：以注释行输出。发票上有这一项就取消注释并填值；
        // 没有就保持注释。绝不能输出空串 —— 空串会被 serde 反序列化成
        // Some("")，比对时传给 Decimal::from_str 解析失败，
        // 每张样本的每个可选字段都会报假的不匹配。
        out.push_str("# tax_amount = \"\"\n");
        out.push_str("# tax_rate = \"\"\n");
        out.push_str("# buyer_name = \"\"\n");
        out.push_str("# seller_name = \"\"\n\n");
    }

    out
}

/// 注释里不能出现换行，否则会破坏 TOML 结构
fn sanitize_comment(s: &str) -> String {
    s.replace(['\n', '\r'], " ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(seq: &str, format: &str, platform: &str, subject: &str) -> ManifestEntry {
        ManifestEntry {
            saved: SavedSample {
                rel_path: format!("samples/{seq}-{platform}-abcd1234.pdf"),
                sha8: "abcd1234".into(),
                byte_len: 12345,
            },
            format: format.into(),
            platform: platform.into(),
            original_filename: "电子发票.pdf".into(),
            subject: subject.into(),
        }
    }

    #[test]
    fn renders_one_sample_block_per_entry() {
        let out = render(&[
            entry("01", "pdf-rail", "12306", "您的电子发票"),
            entry("02", "pdf-vat", "unknown", "住宿发票"),
        ]);
        assert_eq!(out.matches("[[sample]]").count(), 2);
    }

    #[test]
    fn includes_path_and_format_filled_in() {
        let out = render(&[entry("01", "pdf-rail", "12306", "x")]);
        assert!(
            out.contains(r#"path = "samples/01-12306-abcd1234.pdf""#),
            "实际:\n{out}"
        );
        assert!(out.contains(r#"format = "pdf-rail""#), "实际:\n{out}");
    }

    #[test]
    fn required_fields_are_empty_placeholders() {
        let out = render(&[entry("01", "pdf-rail", "12306", "x")]);
        // 必填字段留空串占位，人工填写前不应有假数据
        assert!(out.contains(r#"invoice_number = """#), "实际:\n{out}");
        assert!(out.contains(r#"total_amount = """#), "实际:\n{out}");
        assert!(out.contains(r#"issue_date = """#), "实际:\n{out}");
    }

    #[test]
    fn optional_fields_are_commented_out_not_empty_strings() {
        // 空串会反序列化成 Some("")，导致比对时把空串喂给 Decimal 解析器，
        // 每个可选字段都报假的不匹配。必须输出成注释行。
        let out = render(&[entry("01", "pdf-rail", "12306", "x")]);
        assert!(out.contains(r#"# tax_amount = """#), "实际:\n{out}");
        assert!(out.contains(r#"# tax_rate = """#), "实际:\n{out}");

        let parsed: toml::Value = toml::from_str(&out).unwrap();
        let sample = &parsed["sample"][0];
        assert!(
            sample.get("tax_amount").is_none(),
            "可选字段解析后应为缺失，实际存在: {sample:?}"
        );
    }

    #[test]
    fn annotates_original_filename_and_subject_as_comments() {
        let out = render(&[entry("01", "pdf-rail", "12306", "您的电子发票")]);
        assert!(out.contains("# 原始文件名: 电子发票.pdf"), "实际:\n{out}");
        assert!(out.contains("# 邮件主题: 您的电子发票"), "实际:\n{out}");
    }

    #[test]
    fn header_states_manual_fill_requirement() {
        let out = render(&[entry("01", "pdf-rail", "12306", "x")]);
        assert!(out.contains("人工填写"), "表头应说明需人工填写:\n{out}");
    }

    #[test]
    fn empty_input_still_produces_valid_header() {
        let out = render(&[]);
        assert!(out.contains("未采集到"), "实际:\n{out}");
        assert!(!out.contains("[[sample]]"));
    }

    #[test]
    fn output_parses_as_valid_toml() {
        let out = render(&[entry("01", "pdf-rail", "12306", "带\"引号\"的主题")]);
        let parsed: toml::Value = toml::from_str(&out)
            .unwrap_or_else(|e| panic!("生成的 TOML 无法解析: {e}\n---\n{out}"));
        assert!(parsed.get("sample").is_some());
    }
}
