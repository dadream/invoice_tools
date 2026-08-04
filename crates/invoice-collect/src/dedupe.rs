use sha2::{Digest, Sha256};
use std::collections::HashSet;

pub fn sha256_hex(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    hasher.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// 双键去重器：Message-ID 去重整封邮件，文件 SHA256 去重同一份附件。
#[derive(Default)]
pub struct Deduper {
    seen_message_ids: HashSet<String>,
    seen_file_hashes: HashSet<String>,
}

impl Deduper {
    pub fn new() -> Self {
        Self::default()
    }

    /// 判断这份附件是否为新内容。有副作用：会记录已见过的键。
    ///
    /// Message-ID 相同**且**文件内容相同才算重复。
    /// 只有 Message-ID 相同不算 —— 一封邮件可以带多张不同的发票。
    pub fn is_new(&mut self, message_id: Option<&str>, data: &[u8]) -> bool {
        let file_hash = sha256_hex(data);

        // 文件内容重复 = 同一份附件，无论来自哪封邮件
        if self.seen_file_hashes.contains(&file_hash) {
            return false;
        }

        self.seen_file_hashes.insert(file_hash);
        if let Some(id) = message_id {
            self.seen_message_ids.insert(id.to_string());
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_is_stable_and_64_hex_chars() {
        let h = sha256_hex(b"hello");
        assert_eq!(h.len(), 64);
        assert_eq!(h, sha256_hex(b"hello"));
        assert_ne!(h, sha256_hex(b"world"));
    }

    #[test]
    fn first_occurrence_is_new() {
        let mut d = Deduper::new();
        assert!(d.is_new(Some("id1"), b"content"));
    }

    #[test]
    fn identical_content_from_same_email_is_duplicate() {
        let mut d = Deduper::new();
        assert!(d.is_new(Some("id1"), b"content"));
        assert!(!d.is_new(Some("id1"), b"content"));
    }

    #[test]
    fn same_content_resent_under_new_message_id_is_duplicate() {
        // 平台重发场景：新邮件，同一份 PDF
        let mut d = Deduper::new();
        assert!(d.is_new(Some("id1"), b"same-pdf"));
        assert!(!d.is_new(Some("id2"), b"same-pdf"));
    }

    #[test]
    fn different_attachments_in_one_email_are_both_new() {
        // 一封邮件带两张不同的发票
        let mut d = Deduper::new();
        assert!(d.is_new(Some("id1"), b"invoice-a"));
        assert!(d.is_new(Some("id1"), b"invoice-b"));
    }

    #[test]
    fn missing_message_id_still_dedupes_by_content() {
        let mut d = Deduper::new();
        assert!(d.is_new(None, b"x"));
        assert!(!d.is_new(None, b"x"));
    }
}
