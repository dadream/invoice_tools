// lib.rs - 公开测试需要的模块

pub mod backup;
pub mod cleanup;
pub mod commands;
pub mod concur_smtp;
pub mod error;
pub mod local_source;
pub mod logger;
pub mod ocr_worker;
pub mod paths;
pub mod pipeline_checkpoint;
pub mod preflight;
pub mod windows_security;

use error::{AppError, AppResult};
use invoice_store::{AccountsDb, LedgerDb};
use zeroize::Zeroizing;

struct SessionCredential {
    email: String,
    password: Zeroizing<String>,
}

/// 应用共享状态。邮箱授权码只存在于 `session_credential`，不会写入数据库。
pub struct AppState {
    ledger_db: Option<LedgerDb>,
    accounts_db: Option<AccountsDb>,
    session_credential: Option<SessionCredential>,
}

impl Default for AppState {
    fn default() -> Self {
        Self::new()
    }
}

impl AppState {
    pub fn new() -> Self {
        Self {
            ledger_db: None,
            accounts_db: None,
            session_credential: None,
        }
    }

    pub fn ledger_db(&self) -> AppResult<&LedgerDb> {
        self.ledger_db
            .as_ref()
            .ok_or_else(|| AppError::internal("ledger.db 未初始化"))
    }

    pub fn accounts_db(&self) -> AppResult<&AccountsDb> {
        self.accounts_db
            .as_ref()
            .ok_or_else(|| AppError::internal("accounts.db 未初始化"))
    }

    pub fn init_ledger_db(&mut self) -> AppResult<()> {
        let data_dir = paths::data_root().map_err(AppError::from)?;
        let ledger_path = data_dir.join("ledger.db");
        let accounts_path = data_dir.join("accounts.db");
        std::fs::create_dir_all(&data_dir)?;

        let migration_snapshot_directory = data_dir.join("migration-backups");
        let (ledger_db, migration_snapshot) =
            LedgerDb::new_with_migration_snapshot(&ledger_path, &migration_snapshot_directory)
                .map_err(|e| AppError::database(format!("初始化 ledger.db 失败: {}", e)))?;
        if migration_snapshot.is_some() {
            tracing::info!("旧数据库迁移完成，迁移前快照已保留");
        }
        let interrupted = ledger_db
            .mark_running_pipeline_runs_interrupted()
            .map_err(|e| AppError::database(format!("标记中断流水线失败: {e}")))?;
        if interrupted > 0 {
            tracing::warn!(count = interrupted, "发现可从检查点恢复的中断流水线");
        }
        let interrupted_collections = ledger_db
            .mark_collecting_email_collection_tasks_interrupted()
            .map_err(|e| AppError::database(format!("标记中断邮件收集任务失败: {e}")))?;
        if interrupted_collections > 0 {
            tracing::warn!(count = interrupted_collections, "发现已中断的邮件收集任务");
        }
        self.ledger_db = Some(ledger_db);

        let accounts_db = AccountsDb::new(&accounts_path)
            .map_err(|e| AppError::database(format!("初始化 accounts.db 失败: {}", e)))?;
        let purged = accounts_db
            .purge_all_credentials()
            .map_err(|e| AppError::database(format!("清理旧版持久化授权码失败: {}", e)))?;
        if purged > 0 {
            tracing::warn!(count = purged, "已移除旧版持久化邮箱授权码");
        }
        self.accounts_db = Some(accounts_db);

        Ok(())
    }

    pub fn set_session_credential(&mut self, email: String, password: Zeroizing<String>) {
        self.session_credential = Some(SessionCredential {
            email: email.trim().to_ascii_lowercase(),
            password,
        });
    }

    pub fn session_credential_copy(&self) -> Option<(String, Zeroizing<String>)> {
        self.session_credential
            .as_ref()
            .map(|credential| (credential.email.clone(), credential.password.clone()))
    }

    pub fn session_email(&self) -> Option<&str> {
        self.session_credential
            .as_ref()
            .map(|credential| credential.email.as_str())
    }

    pub fn clear_session_credential(&mut self) {
        self.session_credential = None;
    }
}
