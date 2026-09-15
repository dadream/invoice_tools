use invoice_concur::{repository::Repository, *};
use invoice_delivery_contract::*;
use std::collections::BTreeMap;

fn form(sources: &[&str]) -> Form {
    Form {
        signature: "observed-form-v1".into(),
        fields: sources
            .iter()
            .map(|s| Field {
                label: s.to_string(),
                source: s.to_string(),
                required: true,
                control: "text".into(),
                ..Default::default()
            })
            .collect(),
    }
}
fn fixture() -> (Snapshot, Profile) {
    let source: Snapshot = serde_json::from_str(include_str!(
        "../../../sidecars/concur-browser/samples/sample.json"
    ))
    .unwrap();
    let mut profile = Profile {
        origin: "https://test.concursolutions.com".into(),
        account: "synthetic@example.invalid".into(),
        report: form(&["report_name", "report_date"]),
        ..Default::default()
    };
    for cat in ["rail", "hotel", "city_transport"] {
        profile
            .expense_types
            .insert(cat.into(), format!("测试-{cat}"));
        profile.expenses.insert(
            cat.into(),
            form(&["transaction_date", "gross_amount", "comment"]),
        );
    }
    (source, profile)
}
fn job(test: bool) -> Job {
    let (s, p) = fixture();
    plan("synthetic-job".into(), s, p, test, "2026-06-30").unwrap()
}

#[test]
fn produces_two_reports_three_expenses_six_individual_attachments() {
    let j = job(true);
    assert!(j.gaps.is_empty(), "{:?}", j.gaps);
    assert_eq!(j.steps.iter().filter(|s| s.kind == "report").count(), 2);
    assert_eq!(j.steps.iter().filter(|s| s.kind == "expense").count(), 3);
    assert_eq!(j.steps.iter().filter(|s| s.kind == "attachment").count(), 6);
    assert!(j
        .steps
        .iter()
        .filter(|s| s.kind == "report")
        .all(|s| s.label.contains("测试-请勿提交")));
}
#[test]
fn multiple_local_months_and_trips_still_only_two_reports() {
    let (mut s, p) = fixture();
    let mut e = s.expenses[0].clone();
    e.id = 4;
    e.group_id = 8;
    e.group_title = "五月市内费用".into();
    s.expenses.push(e);
    let mut e = s.expenses[1].clone();
    e.id = 5;
    e.group_id = 9;
    e.group_title = "另一次差旅".into();
    s.expenses.push(e);
    let j = plan("x".into(), s, p, false, "2026-06-30").unwrap();
    assert_eq!(j.steps.iter().filter(|s| s.kind == "report").count(), 2);
}
#[test]
fn local_only_is_one_report() {
    let (mut s, p) = fixture();
    s.expenses.retain(|e| e.group_kind == "local_month");
    assert_eq!(
        plan("x".into(), s, p, false, "2026-06-30")
            .unwrap()
            .steps
            .iter()
            .filter(|s| s.kind == "report")
            .count(),
        1
    );
}
#[test]
fn unknown_group_requires_delivery_choice_without_modifying_snapshot() {
    let (mut s, mut p) = fixture();
    s.expenses[0].group_kind = "logistics".into();
    let j = plan("x".into(), s.clone(), p.clone(), false, "2026-06-30").unwrap();
    assert!(j.gaps.iter().any(|s| s.contains("本地或差旅")));
    p.group_targets.insert("1".into(), "local".into());
    let j = plan("x".into(), s, p, false, "2026-06-30").unwrap();
    assert!(j.gaps.is_empty());
    assert_eq!(j.snapshot.expenses[0].group_kind, "logistics");
}
#[test]
fn actual_amount_not_capped_and_unknown_vat_not_zero() {
    let (mut s, mut p) = fixture();
    s.expenses[0]
        .fields
        .insert("gross_amount".into(), "126.00".into());
    p.expenses
        .get_mut("city_transport")
        .unwrap()
        .fields
        .push(Field {
            label: "VAT".into(),
            source: "tax_amount".into(),
            required: true,
            ..Default::default()
        });
    let j = plan("x".into(), s, p, false, "2026-06-30").unwrap();
    assert!(j.gaps.iter().any(|s| s.contains("VAT")));
    assert!(j.steps.iter().any(|s| s
        .values
        .iter()
        .any(|f| f.label == "gross_amount" && f.value == "126.00")));
}
#[test]
fn duplicate_local_expense_ids_are_rejected() {
    let (mut s, p) = fixture();
    s.expenses.push(s.expenses[0].clone());
    assert!(plan("x".into(), s, p, false, "2026-06-30").is_err());
}
#[test]
fn identical_files_and_duplicate_copy_are_not_uploaded_twice() {
    let (mut s, p) = fixture();
    let d = s.expenses[0].documents[0].clone();
    s.expenses[0].documents.push(d.clone());
    let mut other = d;
    other.sha256 = "different".into();
    other.role = "duplicate_copy".into();
    s.expenses[0].documents.push(other);
    let j = plan("x".into(), s, p, false, "2026-06-30").unwrap();
    assert_eq!(j.steps.iter().filter(|s| s.kind == "attachment").count(), 6);
}
#[test]
fn uncertain_writes_cannot_be_blindly_retried() {
    let mut j = job(true);
    j.begin(0).unwrap();
    assert!(j.begin(0).is_err());
    assert!(j.begin(1).is_err());
    assert!(!j.completed());
}
#[test]
fn repository_recovers_uncertain_and_rejects_stale_updates() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("test.db");
    let repo = Repository::open(&path).unwrap();
    let mut j = job(true);
    repo.create(&j).unwrap();
    j.begin(0).unwrap();
    let mut stale = j.clone();
    repo.update(&mut j).unwrap();
    assert!(repo.update(&mut stale).is_err());
    drop(repo);
    let repo = Repository::open(&path).unwrap();
    let restored = repo.get(&j.id).unwrap();
    assert_eq!(restored.steps[0].status, "writing");
}
#[test]
fn repeated_batch_and_reaudited_snapshot_are_not_recreated() {
    let dir = tempfile::tempdir().unwrap();
    let repo = Repository::open(&dir.path().join("test.db")).unwrap();
    let mut j = job(false);
    repo.create(&j).unwrap();
    j.id = "different-job".into();
    j.snapshot.fingerprint = "reaudited".into();
    assert!(repo.create(&j).is_err());
}
#[test]
fn profiles_versioned_without_core_schema_version_change() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("test.db");
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.pragma_update(None, "user_version", 44).unwrap();
    let repo = Repository::open(&path).unwrap();
    let (_, p) = fixture();
    let first = repo.save_profile(p.clone()).unwrap();
    let mut changed = p;
    changed
        .expense_types
        .insert("rail".into(), "新的铁路类型".into());
    let next = repo.save_profile(changed).unwrap();
    assert!(next.version > first.version);
    assert_eq!(
        conn.pragma_query_value(None, "user_version", |r| r.get::<_, i32>(0))
            .unwrap(),
        44
    );
}
#[test]
fn identical_profile_saves_reuse_version_even_after_restart() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("test.db");
    let (_, profile) = fixture();
    let repo = Repository::open(&path).unwrap();
    let saved = repo.save_profile(profile).unwrap();
    drop(repo);
    let repo = Repository::open(&path).unwrap();
    let mut legacy_duplicate = saved.clone();
    legacy_duplicate.version += 100;
    let reused = repo.save_profile(legacy_duplicate).unwrap();
    assert_eq!(reused.version, saved.version);
    let conn = rusqlite::Connection::open(path).unwrap();
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM concur_browser_profiles", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(count, 1);
}
#[test]
fn completed_test_remains_valid_after_repeated_or_legacy_profile_saves() {
    let dir = tempfile::tempdir().unwrap();
    let repo = Repository::open(&dir.path().join("test.db")).unwrap();
    let mut tested = job(true);
    tested.profile = repo.save_profile(tested.profile).unwrap();
    for step in &mut tested.steps {
        step.status = "verified".into();
    }
    repo.create(&tested).unwrap();
    let saved = repo.save_profile(tested.profile.clone()).unwrap();
    assert_eq!(saved.version, tested.profile.version);
    assert!(repo.has_completed_test_for(&saved).unwrap());
    let mut legacy = saved;
    legacy.version += 10;
    assert!(repo.has_completed_test_for(&legacy).unwrap());
}
#[test]
fn switching_accounts_keeps_the_last_confirmed_profile_active() {
    let dir = tempfile::tempdir().unwrap();
    let repo = Repository::open(&dir.path().join("test.db")).unwrap();
    let (_, profile) = fixture();
    let first = repo.save_profile(profile).unwrap();
    let mut other = first.clone();
    other.account = "other@example.invalid".into();
    let other = repo.save_profile(other).unwrap();
    let restored = repo.save_profile(first.clone()).unwrap();
    assert!(restored.version > other.version);
    assert_eq!(repo.profile().unwrap().unwrap().account, first.account);
    assert_eq!(
        repo.save_profile(restored.clone()).unwrap().version,
        restored.version
    );
    assert_eq!(restored.mapping_digest(), first.mapping_digest());
}
#[test]
fn mapping_changes_never_reuse_a_different_test() {
    let dir = tempfile::tempdir().unwrap();
    let repo = Repository::open(&dir.path().join("test.db")).unwrap();
    let mut tested = job(true);
    for step in &mut tested.steps {
        step.status = "verified".into();
    }
    repo.create(&tested).unwrap();
    let base = tested.profile;
    let mut changes = Vec::new();
    let mut p = base.clone();
    p.account = "other@example.invalid".into();
    changes.push(p);
    let mut p = base.clone();
    p.origin = "https://other.concursolutions.com".into();
    changes.push(p);
    let mut p = base.clone();
    p.report.signature = "changed-form".into();
    changes.push(p);
    let mut p = base.clone();
    p.report.fields[0].source = "constant".into();
    changes.push(p);
    let mut p = base.clone();
    p.report.fields[0].constant = "changed-value".into();
    changes.push(p);
    let mut p = base.clone();
    p.report.fields[0].required = false;
    changes.push(p);
    let mut p = base.clone();
    p.report.fields[0].control = "select".into();
    changes.push(p);
    let mut p = base.clone();
    p.report.fields[0].options.push("New option".into());
    changes.push(p);
    let mut p = base.clone();
    p.report.fields[0]
        .option_map
        .insert("local".into(), "本地".into());
    changes.push(p);
    let mut p = base.clone();
    p.report.fields[1].format = "MM/DD/YYYY".into();
    changes.push(p);
    let mut p = base.clone();
    p.expenses.get_mut("rail").unwrap().fields[0].label = "新日期标签".into();
    changes.push(p);
    let mut p = base.clone();
    p.expense_types.insert("rail".into(), "新类型".into());
    changes.push(p);
    let mut p = base.clone();
    p.group_targets.insert("1".into(), "trip".into());
    changes.push(p);
    for changed in changes {
        assert!(
            !repo.has_completed_test_for(&changed).unwrap(),
            "unexpected test reuse: {:?}",
            changed
        );
    }
}
#[test]
fn pending_uncertain_and_real_jobs_cannot_unlock_delivery() {
    for status in ["pending", "writing", "verified"] {
        let dir = tempfile::tempdir().unwrap();
        let repo = Repository::open(&dir.path().join("test.db")).unwrap();
        let mut j = job(status != "verified");
        for step in &mut j.steps {
            step.status = status.into();
        }
        repo.create(&j).unwrap();
        assert!(!repo.has_completed_test_for(&j.profile).unwrap());
    }
}
#[test]
fn sample_files_are_packaged_and_integrity_checked() {
    let mut j = job(true);
    for s in &mut j.steps {
        if let Some(d) = &mut s.document {
            d.path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../sidecars/concur-browser/samples")
                .join(&d.path)
                .to_string_lossy()
                .into();
            check_document(d).unwrap();
            d.sha256 = "changed".into();
            assert!(check_document(d).is_err());
        }
    }
}
#[test]
fn empty_job_cannot_claim_success() {
    let mut j = job(true);
    j.steps.clear();
    assert!(!j.completed());
}
#[test]
fn option_mapping_is_explicit() {
    let (mut s, mut p) = fixture();
    s.expenses[0]
        .fields
        .insert("payment_method".into(), "personal".into());
    p.expenses
        .get_mut("city_transport")
        .unwrap()
        .fields
        .push(Field {
            label: "Payment".into(),
            source: "payment_method".into(),
            required: true,
            control: "select".into(),
            options: vec!["Cash".into()],
            option_map: BTreeMap::from([("personal".into(), "Cash".into())]),
            constant: String::new(),
            format: String::new(),
        });
    let j = plan("x".into(), s, p, false, "2026-06-30").unwrap();
    assert!(j.gaps.is_empty());
    assert!(j.steps.iter().any(|s| s
        .values
        .iter()
        .any(|v| v.label == "Payment" && v.value == "Cash")));
}
