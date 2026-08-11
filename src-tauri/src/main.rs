// Prevents additional console window on Windows in release
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::sync::Mutex;
use std::path::PathBuf;

mod commands;
mod error;
mod logger;

use error::{AppError, AppResult};
use invoice_store::LedgerDb;

/// 应用共享状态，持有 ledger.db 数据库连接
pub struct AppState {
    ledger_db: Option<LedgerDb>,
}

impl AppState {
    pub fn new() -> Self {
        Self { ledger_db: None }
    }

    pub fn ledger_db(&self) -> AppResult<&LedgerDb> {
        self.ledger_db.as_ref().ok_or_else(|| AppError::internal("数据库未初始化"))
    }

    pub fn init_ledger_db(&mut self) -> AppResult<()> {
        let data_dir = std::env::var("INVOICE_ASSISTANT_HOME")
            .unwrap_or_else(|_| {
                let home = std::env::var("HOME")
                    .or_else(|_| std::env::var("USERPROFILE"))
                    .unwrap_or_else(|_| ".".to_string());
                format!("{}/.invoice-assistant", home)
            });

        let db_path = PathBuf::from(data_dir).join("ledger.db");
        std::fs::create_dir_all(db_path.parent().unwrap())?;

        self.ledger_db = Some(LedgerDb::new(&db_path)
            .map_err(|e| AppError::database(format!("初始化数据库失败: {}", e)))?);

        tracing::info!(path = %db_path.display(), "ledger.db 已初始化");
        Ok(())
    }
}

fn main() {
    // guard 必须绑定到具名变量：绑到 `_` 会立即 drop，退出时丢日志。
    let _log_guard = logger::init().expect("日志系统初始化失败");

    let mut app_state = AppState::new();
    if let Err(e) = app_state.init_ledger_db() {
        tracing::error!("数据库初始化失败: {}", e);
        std::process::exit(1);
    }

    tauri::Builder::default()
        .manage(Mutex::new(app_state))
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![
            commands::base::greet,
            commands::base::get_version,
            commands::base::health_check,
            commands::base::trigger_error,
            commands::batch::list_batches,
            commands::batch::get_batch,
            commands::batch::create_batch,
            commands::batch::transition_batch_status,
            commands::batch::delete_batch,
            commands::invoice::parse_invoice,
            commands::invoice::check_duplicate,
            commands::invoice::add_invoice_to_batch,
            commands::invoice::list_batch_invoices,
            commands::invoice::delete_invoice,
            commands::invoice::clear_duplicate_flag,
            commands::export::export_batch_excel,
            commands::export::export_batch_pdf,
        ])
        .run(tauri::generate_context!())
        .expect("Tauri 应用运行失败");
}
