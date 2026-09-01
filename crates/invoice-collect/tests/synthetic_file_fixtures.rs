use invoice_collect::dedupe::Deduper;
use invoice_collect::extract::{extract_zip_if_needed, RawAttachment};
use std::path::{Path, PathBuf};

fn fixture_path(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("fixtures")
        .join("synthetic")
        .join(name)
}

fn read_fixture(name: &str) -> Vec<u8> {
    let path = fixture_path(name);
    std::fs::read(&path).unwrap_or_else(|error| {
        panic!(
            "failed to read synthetic fixture {}: {error}",
            path.display()
        )
    })
}

#[test]
fn duplicate_fixture_content_is_detected_across_message_ids() {
    let first = read_fixture("duplicate-a.xml");
    let second = read_fixture("duplicate-b.xml");
    assert_eq!(
        first, second,
        "duplicate fixture pair must remain byte-identical"
    );

    let mut deduper = Deduper::new();
    assert!(deduper.is_new(Some("synthetic-a@example.invalid"), &first));
    assert!(!deduper.is_new(Some("synthetic-b@example.invalid"), &second));
}

#[test]
fn expanded_over_limit_zip_is_rejected_without_allocating_the_entry() {
    let attachment = RawAttachment {
        filename: "expanded-over-limit.zip".into(),
        content_type: "application/zip".into(),
        data: read_fixture("expanded-over-limit.zip"),
    };

    assert!(extract_zip_if_needed(&attachment).is_empty());
}

#[test]
fn malformed_zip_bytes_are_rejected() {
    let attachment = RawAttachment {
        filename: "malformed.zip".into(),
        content_type: "application/zip".into(),
        data: read_fixture("malformed.ofd"),
    };

    assert!(extract_zip_if_needed(&attachment).is_empty());
}
