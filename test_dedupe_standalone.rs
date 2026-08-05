// Standalone test to verify dedupe logic without cargo
use std::collections::HashSet;

fn sha256_hex(data: &[u8]) -> String {
    // Simplified for testing - in real code uses sha2 crate
    format!("{:x}", data.iter().fold(0u64, |acc, &b| acc.wrapping_mul(31).wrapping_add(b as u64)))
}

#[derive(Default)]
struct Deduper {
    seen_message_ids: HashSet<String>,
    seen_file_hashes: HashSet<String>,
}

impl Deduper {
    fn new() -> Self {
        Self::default()
    }

    fn is_new(&mut self, message_id: Option<&str>, data: &[u8]) -> bool {
        let file_hash = sha256_hex(data);

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

fn main() {
    let mut d = Deduper::new();

    // Test 1: first occurrence is new
    assert!(d.is_new(Some("id1"), b"content"));
    println!("✓ Test 1: first occurrence is new");

    // Test 2: identical content from same email is duplicate
    assert!(!d.is_new(Some("id1"), b"content"));
    println!("✓ Test 2: identical content from same email is duplicate");

    // Test 3: same content under new message ID is duplicate
    let mut d2 = Deduper::new();
    assert!(d2.is_new(Some("id1"), b"same-pdf"));
    assert!(!d2.is_new(Some("id2"), b"same-pdf"));
    println!("✓ Test 3: same content resent under new message ID is duplicate");

    // Test 4: different attachments in one email are both new
    let mut d3 = Deduper::new();
    assert!(d3.is_new(Some("id1"), b"invoice-a"));
    assert!(d3.is_new(Some("id1"), b"invoice-b"));
    println!("✓ Test 4: different attachments in one email are both new");

    // Test 5: missing message ID still dedupes by content
    let mut d4 = Deduper::new();
    assert!(d4.is_new(None, b"x"));
    assert!(!d4.is_new(None, b"x"));
    println!("✓ Test 5: missing message ID still dedupes by content");

    println!("\nAll dedupe logic tests passed!");
}
