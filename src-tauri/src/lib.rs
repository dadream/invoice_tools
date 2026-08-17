// lib.rs - 公开测试需要的模块

pub mod commands;
pub mod logger;
pub mod error;

use std::path::PathBuf;
use error::{AppError, AppResult};
use invoice_store::{AccountsDb, LedgerDb};

/// 应用共享状态，持有数据库连接
pub struct AppState {
    ledger_db: Option<LedgerDb>,
    accounts_db: Option<AccountsDb>,
}

impl AppState {
    pub fn new() -> Self {
        Self {
            ledger_db: None,
            accounts_db: None,
        }
    }

    pub fn ledger_db(&self) -> AppResult<&LedgerDb> {
        self.ledger_db.as_ref().ok_or_else(|| AppError::internal("ledger.db 未初始化"))
    }

    pub fn accounts_db(&self) -> AppResult<&AccountsDb> {
        self.accounts_db.as_ref().ok_or_else(|| AppError::internal("accounts.db 未初始化"))
    }

    pub fn init_ledger_db(&mut self) -> AppResult<()> {
        let data_dir = std::env::var("INVOICE_ASSISTANT_HOME")
            .unwrap_or_else(|_| {
                let home = std::env::var("HOME")
                    .or_else(|_| std::env::var("USERPROFILE"))
                    .unwrap_or_else(|_| ".".to_string());
                format!("{}/.invoice-assistant", home)
            });

        let ledger_path = PathBuf::from(&data_dir).join("ledger.db");
        let accounts_path = PathBuf::from(&data_dir).join("accounts.db");
        std::fs::create_dir_all(&data_dir)?;

        self.ledger_db = Some(LedgerDb::new(&ledger_path)
            .map_err(|e| AppError::database(format!("初始化 ledger.db 失败: {}", e)))?);
        
        self.accounts_db = Some(AccountsDb::new(&accounts_path)
            .map_err(|e| AppError::database(format!("初始化 accounts.db 失败: {}", e)))?);

        Ok(())
    }
}
