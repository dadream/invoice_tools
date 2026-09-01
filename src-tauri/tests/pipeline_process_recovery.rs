use std::fs;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use invoice_store::LedgerDb;

const CHILD_MODE: &str = "INVOICE_ASSISTANT_RECOVERY_CHILD";
const CHILD_DB: &str = "INVOICE_ASSISTANT_RECOVERY_DB";
const CHILD_READY: &str = "INVOICE_ASSISTANT_RECOVERY_READY";

/// Child fixture for the parent process-kill test. A normal workspace run sees
/// no child marker and returns immediately.
#[test]
fn pipeline_child_holds_running_state() {
    if std::env::var(CHILD_MODE).as_deref() != Ok("1") {
        return;
    }
    let db_path = std::env::var_os(CHILD_DB).expect("child database path");
    let ready_path = std::env::var_os(CHILD_READY).expect("child ready marker");
    let db = LedgerDb::new(db_path).expect("open child ledger");
    db.create_pipeline_run(
        "process-kill-pipeline",
        r#"{"sourceKind":"local","batchName":"强杀恢复验证"}"#,
        "local",
        "C:\\synthetic-task",
    )
    .expect("persist running pipeline");
    fs::write(ready_path, b"ready").expect("write child ready marker");

    loop {
        thread::sleep(Duration::from_secs(60));
    }
}

#[test]
fn running_pipeline_is_recoverable_after_abrupt_process_kill() {
    let root = tempfile::tempdir().expect("temporary recovery root");
    let db_path = root.path().join("ledger.db");
    let ready_path = root.path().join("child-ready");
    let checkpoint = root.path().join("collected-checkpoint.json");
    fs::write(&checkpoint, br#"{"synthetic":true}"#).expect("write durable checkpoint");

    let mut child = Command::new(std::env::current_exe().expect("current test executable"))
        .args([
            "--exact",
            "pipeline_child_holds_running_state",
            "--nocapture",
        ])
        .env(CHILD_MODE, "1")
        .env(CHILD_DB, &db_path)
        .env(CHILD_READY, &ready_path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn recovery child");

    let deadline = Instant::now() + Duration::from_secs(15);
    while !ready_path.exists() && Instant::now() < deadline {
        if child.try_wait().expect("poll recovery child").is_some() {
            panic!("recovery child exited before persisting running state");
        }
        thread::sleep(Duration::from_millis(50));
    }
    if !ready_path.exists() {
        let _ = child.kill();
        let _ = child.wait();
        panic!("recovery child did not become ready");
    }

    child.kill().expect("abruptly terminate recovery child");
    child.wait().expect("reap recovery child");

    let reopened = LedgerDb::new(&db_path).expect("reopen ledger after process kill");
    assert_eq!(
        reopened
            .mark_running_pipeline_runs_interrupted()
            .expect("mark interrupted"),
        1
    );
    let recoverable = reopened
        .list_recoverable_pipeline_runs()
        .expect("list recoverable pipelines");
    assert_eq!(recoverable.len(), 1);
    assert_eq!(recoverable[0].pipeline_id, "process-kill-pipeline");
    assert_eq!(recoverable[0].status, "interrupted");
    assert_eq!(recoverable[0].stage, "created");
    assert_eq!(
        fs::read(&checkpoint).expect("checkpoint survives kill"),
        br#"{"synthetic":true}"#
    );
}
