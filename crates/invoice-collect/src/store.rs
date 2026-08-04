use crate::classify::Classification;
use crate::dedupe::sha256_hex;
use crate::extract::RawAttachment;
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq)]
pub struct SavedSample {
    /// 相对 fixtures/ 的路径，直接写进 manifest.toml
    pub rel_path: String,
    pub sha8: String,
    pub byte_len: usize,
}

/// 落盘一份样本。
///
/// 命名格式 `{seq:02}-{platform}-{sha8}.{ext}`，例如 `01-12306-a3f9c1d2.pdf`。
/// 三个考量：序号让人工审阅有顺序；平台名让人一眼看出这是哪类票；
/// sha8 保证不撞名。原始中文文件名不进路径 —— 跨平台文件名兼容性问题太多。
pub fn save_sample(
    root: &Path,
    seq: usize,
    cls: &Classification,
    att: &RawAttachment,
) -> Result<SavedSample> {
    let samples_dir = root.join("samples");
    std::fs::create_dir_all(&samples_dir)
        .with_context(|| format!("创建目录 {} 失败", samples_dir.display()))?;

    let sha8 = sha256_hex(&att.data)[..8].to_string();
    let file_name = format!(
        "{seq:02}-{}-{}.{}",
        cls.platform,
        sha8,
        cls.format.extension()
    );
    let rel_path = format!("samples/{file_name}");
    let full_path: PathBuf = root.join(&rel_path);

    std::fs::write(&full_path, &att.data)
        .with_context(|| format!("写入 {} 失败", full_path.display()))?;

    Ok(SavedSample {
        rel_path,
        sha8,
        byte_len: att.data.len(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::classify::{MatchReason, SampleFormat};

    fn classification(format: SampleFormat, platform: &str) -> Classification {
        Classification {
            format,
            platform: platform.into(),
            reason: MatchReason::SenderWhitelist,
        }
    }

    fn attachment(data: &[u8]) -> RawAttachment {
        RawAttachment {
            filename: "电子发票.pdf".into(),
            content_type: "application/pdf".into(),
            data: data.to_vec(),
        }
    }

    #[test]
    fn writes_file_and_returns_relative_path() {
        let tmp = tempfile::tempdir().unwrap();
        let saved = save_sample(
            tmp.path(),
            1,
            &classification(SampleFormat::PdfRail, "12306"),
            &attachment(b"%PDF-1.4"),
        )
        .unwrap();

        assert!(saved.rel_path.starts_with("samples/01-12306-"));
        assert!(saved.rel_path.ends_with(".pdf"));
        assert_eq!(saved.byte_len, 8);

        let on_disk = tmp.path().join(&saved.rel_path);
        assert!(on_disk.exists(), "文件未落盘: {}", on_disk.display());
        assert_eq!(std::fs::read(&on_disk).unwrap(), b"%PDF-1.4");
    }

    #[test]
    fn sha8_is_first_8_chars_of_full_hash() {
        let tmp = tempfile::tempdir().unwrap();
        let data = b"%PDF-1.4";
        let saved = save_sample(
            tmp.path(),
            1,
            &classification(SampleFormat::PdfVat, "unknown"),
            &attachment(data),
        )
        .unwrap();
        assert_eq!(saved.sha8, sha256_hex(data)[..8]);
    }

    #[test]
    fn extension_follows_format_not_original_filename() {
        let tmp = tempfile::tempdir().unwrap();
        let mut att = attachment(b"<Invoice/>");
        att.filename = "发票.PDF".into(); // 原名误导
        let saved = save_sample(
            tmp.path(),
            3,
            &classification(SampleFormat::Xml, "tax"),
            &att,
        )
        .unwrap();
        assert!(saved.rel_path.ends_with(".xml"), "实际: {}", saved.rel_path);
    }

    #[test]
    fn sequence_number_is_zero_padded_to_two_digits() {
        let tmp = tempfile::tempdir().unwrap();
        let saved = save_sample(
            tmp.path(),
            7,
            &classification(SampleFormat::Ofd, "tax"),
            &attachment(b"PK\x03\x04"),
        )
        .unwrap();
        assert!(saved.rel_path.contains("/07-"), "实际: {}", saved.rel_path);
    }

    #[test]
    fn creates_samples_directory_if_absent() {
        let tmp = tempfile::tempdir().unwrap();
        let nested = tmp.path().join("does/not/exist");
        let saved = save_sample(
            &nested,
            1,
            &classification(SampleFormat::Image, "unknown"),
            &attachment(b"\xff\xd8\xff"),
        )
        .unwrap();
        assert!(nested.join(&saved.rel_path).exists());
    }
}
