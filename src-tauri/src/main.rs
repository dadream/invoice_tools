// Prevents additional console window on Windows in release
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::sync::Mutex;

mod backup;
mod cleanup;
mod commands;
mod concur_smtp;
mod error;
mod local_source;
mod logger;
mod ocr_worker;
mod paths;
mod pipeline_checkpoint;
mod preflight;
mod windows_security;

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

        tracing::info!("数据库已初始化");
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

fn main() {
    if let Err(error) = windows_security::harden_dll_search() {
        preflight::show_fatal_error(
            "发票报销助手无法启动",
            &format!("Windows DLL 安全初始化失败。请重新下载完整程序包。\n\n{error}"),
        );
        return;
    }
    if cleanup::run_helper_if_requested() {
        return;
    }

    let preflight_report = match preflight::run() {
        Ok(report) => report,
        Err(error) => {
            preflight::show_fatal_error("发票报销助手无法启动", &error.to_string());
            return;
        }
    };

    // guard 必须绑定到具名变量：绑到 `_` 会立即 drop，退出时丢日志。
    let _log_guard = match logger::init() {
        Ok(guard) => guard,
        Err(error) => {
            preflight::show_fatal_error(
                "发票报销助手无法启动",
                &format!("日志目录初始化失败；请检查数据目录权限。\n\n{error}"),
            );
            return;
        }
    };
    tracing::info!(
        webview2_version = %preflight_report.webview2_version,
        free_mib = preflight_report.free_bytes / 1024 / 1024,
        "Windows 启动前检查通过"
    );

    let mut app_state = AppState::new();
    if let Err(e) = app_state.init_ledger_db() {
        tracing::error!(error_kind = ?e.kind(), "数据库初始化失败");
        preflight::show_fatal_error(
            "发票报销助手无法启动",
            &format!(
                "数据库无法打开或版本不兼容。请勿删除原数据；可先备份数据目录并联系支持。\n\n{e}"
            ),
        );
        return;
    }

    let result = tauri::Builder::default()
        .manage(Mutex::new(app_state))
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![
            commands::base::greet,
            commands::base::get_version,
            commands::base::health_check,
            commands::base::trigger_error,
            commands::update::check_for_updates,
            commands::batch::list_batches,
            commands::batch::get_batch,
            commands::batch::get_batch_grouping,
            commands::batch::create_batch,
            commands::batch::transition_batch_status,
            commands::batch::delete_batch,
            commands::email_collection::create_email_collection_task,
            commands::email_collection::list_email_collection_tasks,
            commands::email_collection::delete_email_collection_task,
            commands::email_collection::get_email_collection_task,
            commands::email_collection::start_email_collection_task,
            commands::email_collection::list_collected_email_messages,
            commands::email_collection::get_collected_email_review_detail,
            commands::email_collection::reanalyze_collected_email_message,
            commands::email_collection::open_collected_email_link,
            commands::email_collection::extract_collected_attachment_qr_links,
            commands::email_collection::get_collected_attachment_preview_metadata,
            commands::email_collection::read_collected_attachment_preview,
            commands::email_collection::render_collected_pdf_preview_page,
            commands::email_collection::render_collected_pdf_text_preview_page,
            commands::email_collection::render_collected_ofd_preview_page,
            commands::email_collection::open_collected_attachment,
            commands::email_collection::resolve_collected_email_message,
            commands::email_collection::set_collected_email_attachment_excluded,
            commands::email_collection::supplement_collected_email_message,
            commands::email_collection::complete_email_collection_review,
            commands::email_collection::create_batch_collection_import,
            commands::email_collection::list_batch_collection_sources,
            commands::email_ledger::list_email_import_ledger,
            commands::email_ledger::resolve_email_import_message,
            commands::concur::get_concur_capability,
            commands::concur::get_concur_send_status,
            commands::concur::prepare_concur_send,
            commands::concur::send_concur_trial,
            commands::concur::confirm_concur_trial,
            commands::concur::send_concur_remaining,
            commands::concur::resolve_concur_unknown,
            commands::invoice::parse_invoice,
            commands::invoice::check_duplicate,
            commands::invoice::add_invoice_to_batch,
            commands::invoice::list_batch_invoices,
            commands::invoice::delete_invoice,
            commands::invoice::clear_duplicate_flag,
            commands::invoice::confirm_duplicate_flag,
            commands::review::update_invoice_review,
            commands::review::list_expense_items,
            commands::review::update_expense_item,
            commands::review::attach_expense_document,
            commands::review::remove_expense_document,
            commands::review::link_duplicate_invoice_to_expense,
            commands::review::list_pending_invoice_documents,
            commands::review::assign_pending_invoice_document,
            commands::review::convert_didi_itinerary_to_expense,
            commands::review::ignore_pending_invoice_document,
            commands::review::get_invoice_preview_metadata,
            commands::review::read_invoice_preview,
            commands::review::get_expense_document_preview_metadata,
            commands::review::read_expense_document_preview,
            commands::review::get_pending_document_preview_metadata,
            commands::review::read_pending_document_preview,
            commands::review::render_pdf_preview_page,
            commands::review::render_pdf_text_preview_page,
            commands::review::render_ofd_preview_page,
            commands::review::open_preview_path,
            commands::review::repair_missing_preview_file,
            commands::review::set_invoice_excluded,
            commands::review::create_manual_group,
            commands::review::move_invoice_group,
            commands::review::merge_groups,
            commands::review::reanalyze_expense_categories,
            commands::review::recompute_batch_grouping,
            commands::review::set_group_transport_evidence,
            commands::review::confirm_invoice_group,
            commands::review::confirm_grouping,
            commands::review::list_review_actions,
            commands::review::undo_last_review_action,
            commands::review::complete_batch_review,
            commands::review::get_active_review_snapshot,
            commands::review::reopen_batch_review,
            commands::review::list_delivery_tasks,
            commands::review::list_concur_mapping_profiles,
            commands::review::save_concur_mapping_profile,
            commands::review::prepare_concur_upload,
            commands::review::list_concur_upload_sessions,
            commands::review::get_concur_upload_status,
            commands::review::get_concur_draft_capability,
            commands::review::resolve_concur_upload_verification,
            commands::review::start_concur_delivery,
            commands::export::export_batch_excel,
            commands::export::export_batch_excel_to_path,
            commands::export::export_batch_csv,
            commands::export_package::export_batch_package,
            commands::print_export::export_batch_print_pdf_to_path,
            commands::print_export::open_delivery_pdf,
            commands::pipeline::start_pipeline,
            commands::pipeline::preview_local_import,
            commands::pipeline::cancel_pipeline,
            commands::pipeline::list_recoverable_pipelines,
            commands::pipeline::resume_pipeline,
            commands::settings::list_accounts,
            commands::settings::add_account,
            commands::settings::delete_account,
            commands::settings::test_account_connection,
            commands::settings::get_session_credential_status,
            commands::settings::clear_session_credential,
            commands::settings::get_setting,
            commands::settings::set_setting,
            commands::settings::get_all_settings,
            commands::settings::get_home_station_library,
            commands::settings::save_home_station_library,
            commands::settings::get_grouping_rules,
            commands::settings::save_grouping_rules,
            commands::onboarding::is_first_run,
            backup::export_backup,
            backup::preview_backup_import,
            backup::stage_backup_import,
            cleanup::preview_cleanup,
            cleanup::start_cleanup,
        ])
        .run(tauri::generate_context!());

    if result.is_err() {
        tracing::error!("Tauri 应用运行失败");
        preflight::show_fatal_error(
            "发票报销助手无法启动",
            "应用窗口无法创建；请确认 WebView2 未被企业安全策略阻止，并联系 IT。",
        )
    }
}
