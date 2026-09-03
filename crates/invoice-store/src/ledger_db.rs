//! ledger.db 管理模块
//!
//! 管理报销批次和已报销发票记录

use chrono::{NaiveDate, NaiveDateTime, Utc};
use rusqlite::{params, Connection, OpenFlags, OptionalExtension, Row, Transaction};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use crate::{
    models::{
        Batch, BatchCollectionImport, BatchGrouping, BatchReviewSnapshot, BatchStatus,
        CollectedEmailAttachment, CollectedEmailLink, CollectedEmailMessage,
        CollectedEmailReviewSnapshot, ConcurMappingGap, ConcurMappingProfile,
        ConcurMappingProfileInput, ConcurUploadAttachmentState, ConcurUploadItemState,
        ConcurUploadPreflight, ConcurUploadSession, ConcurUploadStatus, DeliveryTask,
        EmailCollectionTask, EmailImportAttachment, EmailImportMessage, ExpenseCategoryDetection,
        ExpenseItem, ExpenseItemUpdate, ExpenseLocation, ExpenseTaxDetail, IndexedBatchGrouping,
        InvoiceDocument, InvoiceGroup, InvoiceGroupMember, InvoiceReviewUpdate,
        MappedExpensePayload, NewBatchGrouping, NewCollectedEmailAttachment, NewCollectedEmailLink,
        NewCollectedEmailMessage, NewCollectedEmailReviewSnapshot, NewEmailImportMessage,
        NewPendingInvoiceDocument, PendingInvoiceDocument, PipelineRun, ReportedInvoice,
        ReviewAction, TicketType,
    },
    StoreError, StoreResult,
};

/// Current on-disk ledger schema. Backup/import metadata must use this same value.
pub const LEDGER_SCHEMA_VERSION: i32 = 19;

/// ledger.db 管理器
pub struct LedgerDb {
    pub(crate) conn: Connection,
}

impl LedgerDb {
    /// 打开或创建 ledger.db 数据库
    pub fn new<P: AsRef<Path>>(db_path: P) -> StoreResult<Self> {
        let conn = Connection::open(db_path)?;
        conn.execute("PRAGMA foreign_keys = ON", [])?;

        let db = Self { conn };
        db.init_schema()?;

        Ok(db)
    }

    /// 打开旧 schema 前先创建一致性快照；成功迁移后仍保留快照供人工回退。
    pub fn new_with_migration_snapshot<P, Q>(
        db_path: P,
        snapshot_directory: Q,
    ) -> StoreResult<(Self, Option<PathBuf>)>
    where
        P: AsRef<Path>,
        Q: AsRef<Path>,
    {
        let db_path = db_path.as_ref();
        let snapshot =
            Self::create_migration_snapshot_if_needed(db_path, snapshot_directory.as_ref())?;
        match Self::new(db_path) {
            Ok(db) => Ok((db, snapshot)),
            Err(error) => {
                if let Some(snapshot) = snapshot {
                    Err(StoreError::Internal(format!(
                        "数据库迁移失败；原数据库未提交修改，迁移前快照保留在 {}。原因：{error}",
                        snapshot.display()
                    )))
                } else {
                    Err(error)
                }
            }
        }
    }

    fn create_migration_snapshot_if_needed(
        db_path: &Path,
        snapshot_directory: &Path,
    ) -> StoreResult<Option<PathBuf>> {
        if !db_path.exists() {
            return Ok(None);
        }
        let database_metadata = std::fs::symlink_metadata(db_path)?;
        if !database_metadata.is_file() || database_metadata.file_type().is_symlink() {
            return Err(StoreError::Validation(
                "ledger.db 必须是本机普通文件，不能是目录、符号链接或联接点".to_string(),
            ));
        }

        let connection = Connection::open(db_path)?;
        let source_version: i32 =
            connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        if source_version >= LEDGER_SCHEMA_VERSION {
            return Ok(None);
        }

        std::fs::create_dir_all(snapshot_directory)?;
        let snapshot_directory_metadata = std::fs::symlink_metadata(snapshot_directory)?;
        if !snapshot_directory_metadata.is_dir()
            || snapshot_directory_metadata.file_type().is_symlink()
        {
            return Err(StoreError::Validation(
                "迁移快照目录必须是本机普通目录，不能是符号链接或联接点".to_string(),
            ));
        }

        let timestamp = Utc::now().format("%Y%m%dT%H%M%S%.3fZ");
        let mut selected = None;
        for sequence in 0..1000u16 {
            let name = format!(
                "ledger-v{source_version}-before-v{LEDGER_SCHEMA_VERSION}-{timestamp}-{}-{sequence}",
                std::process::id()
            );
            let final_path = snapshot_directory.join(format!("{name}.db"));
            let temporary_path = snapshot_directory.join(format!(".{name}.tmp"));
            if !final_path.exists() && !temporary_path.exists() {
                selected = Some((temporary_path, final_path));
                break;
            }
        }
        let (temporary_path, final_path) = selected
            .ok_or_else(|| StoreError::Internal("无法为迁移快照生成唯一文件名".to_string()))?;
        let temporary_text = temporary_path.to_string_lossy().to_string();

        let snapshot_result = (|| -> StoreResult<()> {
            connection.execute("VACUUM INTO ?1", params![temporary_text])?;
            drop(connection);

            std::fs::OpenOptions::new()
                .write(true)
                .open(&temporary_path)?
                .sync_all()?;
            let snapshot_connection = Connection::open(&temporary_path)?;
            let snapshot_version: i32 =
                snapshot_connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
            if snapshot_version != source_version {
                return Err(StoreError::Internal(format!(
                    "迁移快照版本校验失败：预期 {source_version}，实际 {snapshot_version}"
                )));
            }
            let integrity: String =
                snapshot_connection.query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
            if !integrity.eq_ignore_ascii_case("ok") {
                return Err(StoreError::Internal(format!(
                    "迁移快照完整性检查失败: {integrity}"
                )));
            }
            drop(snapshot_connection);
            std::fs::rename(&temporary_path, &final_path)?;
            Ok(())
        })();

        if let Err(error) = snapshot_result {
            let _ = std::fs::remove_file(&temporary_path);
            return Err(error);
        }
        Ok(Some(final_path))
    }

    /// 使用 SQLite `VACUUM INTO` 创建事务一致的、独立数据库快照。
    pub fn backup_to<P: AsRef<Path>>(&self, destination: P) -> StoreResult<()> {
        let destination = destination.as_ref();
        if destination.exists() {
            return Err(StoreError::Internal("数据库快照目标已存在".to_string()));
        }
        let destination = destination.to_string_lossy().to_string();
        self.conn.execute("VACUUM INTO ?1", params![destination])?;
        Ok(())
    }

    /// 导入和发布验证使用的 SQLite 完整性检查。
    pub fn integrity_check(&self) -> StoreResult<()> {
        let result: String = self
            .conn
            .query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
        if result.eq_ignore_ascii_case("ok") {
            Ok(())
        } else {
            Err(StoreError::Internal(format!(
                "SQLite integrity_check 失败: {result}"
            )))
        }
    }

    /// 只读检查尚未导入的数据库，不执行迁移、不改写文件。
    /// 返回其 `user_version`，供备份导入在切换数据前核对清单。
    pub fn inspect_existing_database<P: AsRef<Path>>(db_path: P) -> StoreResult<i32> {
        let connection = Connection::open_with_flags(db_path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        let version: i32 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        let integrity: String =
            connection.query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
        if !integrity.eq_ignore_ascii_case("ok") {
            return Err(StoreError::Internal(format!(
                "SQLite integrity_check 失败: {integrity}"
            )));
        }
        Ok(version)
    }

    /// 初始化数据库表结构
    fn init_schema(&self) -> StoreResult<()> {
        self.init_schema_with_migration_hook(|_| Ok(()))
    }

    /// Run all initialization and migrations in one transaction. Tests may inject a
    /// failure at a named migration point without relying on environment state.
    fn init_schema_with_migration_hook<F>(&self, mut migration_hook: F) -> StoreResult<()>
    where
        F: FnMut(&'static str) -> StoreResult<()>,
    {
        let version: i32 = self
            .conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))?;
        if version > LEDGER_SCHEMA_VERSION {
            return Err(StoreError::Internal(format!(
                "Unknown database version: {version}"
            )));
        }

        // Transaction drop rolls back on every early return. This keeps tables, indexes,
        // columns, user_version and recovery updates atomic across all schema versions.
        let transaction = self.conn.unchecked_transaction()?;

        // 创建 batches 表
        self.conn.execute(
            "CREATE TABLE IF NOT EXISTS batches (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                name TEXT NOT NULL,
                month TEXT NOT NULL,
                status INTEGER NOT NULL DEFAULT 0,
                total_amount TEXT NOT NULL,
                invoice_count INTEGER NOT NULL DEFAULT 0,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                submitted_at TEXT,
                approved_at TEXT,
                completed_at TEXT,
                rejected_at TEXT
            )",
            [],
        )?;

        // 创建 reported_invoices 表
        self.conn.execute(
            "CREATE TABLE IF NOT EXISTS reported_invoices (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                batch_id INTEGER NOT NULL,
                invoice_number TEXT NOT NULL,
                issue_date TEXT NOT NULL,
                amount TEXT NOT NULL,
                tax_amount TEXT,
                buyer_name TEXT,
                seller_name TEXT,
                ticket_type TEXT NOT NULL,
                city TEXT,
                departure_time TEXT,
                checkin_date TEXT,
                file_path TEXT NOT NULL,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                verification_result TEXT,
                is_duplicate INTEGER DEFAULT 0,
                duplicate_reason TEXT,
                FOREIGN KEY (batch_id) REFERENCES batches(id) ON DELETE CASCADE
            )",
            [],
        )?;

        // 创建索引
        self.conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_batches_month ON batches(month)",
            [],
        )?;
        self.conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_invoices_batch_id ON reported_invoices(batch_id)",
            [],
        )?;
        self.conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_invoices_number ON reported_invoices(invoice_number)",
            [],
        )?;

        // 创建 settings 表（应用配置）
        self.conn.execute(
            "CREATE TABLE IF NOT EXISTS settings (
                key TEXT PRIMARY KEY NOT NULL,
                value TEXT NOT NULL,
                updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
            )",
            [],
        )?;

        // 数据库迁移：v1 验签/去重，v2 归组，v3 流水线，v4 审核，v5 排除，
        // v6 Concur 发送状态，v7 同批次重复发票号补标，v8 审核快照与交付任务，
        // v9 允许同一批次由多次来源任务增量导入；v10 增加稳定费用项、
        // 材料聚合与版本化 Concur 映射/预检会话；v11 增加待挂载材料队列；
        // v12 增加批次邮件与逻辑附件处理台账；v13 独立邮件收集工作台；
        // v14 持久化收集阶段生成的安全正文与下载链接；v15 增加来源附件人工排除；
        // v16 强制一项费用只属于一个归组；v17 增加独立费用分类来源和确认状态。
        self.migrate_schema(&mut migration_hook)?;
        self.recover_interrupted_concur_sends()?;
        self.recover_interrupted_concur_uploads()?;

        transaction.commit()?;

        Ok(())
    }

    /// 执行数据库迁移
    fn migrate_schema<F>(&self, migration_hook: &mut F) -> StoreResult<()>
    where
        F: FnMut(&'static str) -> StoreResult<()>,
    {
        let version: i32 = self
            .conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))?;

        let mut version = match version {
            0 => {
                // 检查字段是否已存在（避免重复迁移）
                let column_exists: Result<String, _> = self.conn.query_row(
                    "SELECT sql FROM sqlite_master WHERE type='table' AND name='reported_invoices'",
                    [],
                    |row| row.get(0),
                );

                if let Ok(schema) = column_exists {
                    // 只在字段不存在时才执行 ALTER TABLE
                    if !schema.contains("verification_result") {
                        self.conn.execute(
                            "ALTER TABLE reported_invoices ADD COLUMN verification_result TEXT",
                            [],
                        )?;
                        self.conn.execute(
                            "ALTER TABLE reported_invoices ADD COLUMN is_duplicate INTEGER DEFAULT 0",
                            [],
                        )?;
                        self.conn.execute(
                            "ALTER TABLE reported_invoices ADD COLUMN duplicate_reason TEXT",
                            [],
                        )?;
                    }
                }

                // 更新版本号
                self.conn.execute("PRAGMA user_version = 1", [])?;
                1
            }
            1..=LEDGER_SCHEMA_VERSION => version,
            _ => {
                return Err(StoreError::Internal(format!(
                    "Unknown database version: {}",
                    version
                )));
            }
        };

        if version == 1 {
            self.conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS batch_grouping (
                    batch_id INTEGER PRIMARY KEY,
                    rule_version TEXT NOT NULL,
                    home_cities_json TEXT NOT NULL,
                    overall_confidence REAL NOT NULL,
                    ambiguities_json TEXT NOT NULL,
                    created_at TEXT NOT NULL,
                    FOREIGN KEY (batch_id) REFERENCES batches(id) ON DELETE CASCADE
                );
                CREATE TABLE IF NOT EXISTS invoice_groups (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    batch_id INTEGER NOT NULL,
                    group_index INTEGER NOT NULL,
                    kind TEXT NOT NULL,
                    title TEXT NOT NULL,
                    start_date TEXT NOT NULL,
                    end_date TEXT NOT NULL,
                    confidence REAL NOT NULL,
                    requires_review INTEGER NOT NULL DEFAULT 0,
                    evidence_json TEXT NOT NULL,
                    UNIQUE(batch_id, group_index),
                    FOREIGN KEY (batch_id) REFERENCES batch_grouping(batch_id) ON DELETE CASCADE
                );
                CREATE TABLE IF NOT EXISTS invoice_group_members (
                    group_id INTEGER NOT NULL,
                    invoice_id INTEGER NOT NULL,
                    input_index INTEGER NOT NULL,
                    match_reason TEXT NOT NULL,
                    PRIMARY KEY (group_id, invoice_id),
                    FOREIGN KEY (group_id) REFERENCES invoice_groups(id) ON DELETE CASCADE,
                    FOREIGN KEY (invoice_id) REFERENCES reported_invoices(id) ON DELETE CASCADE
                );
                CREATE INDEX IF NOT EXISTS idx_invoice_groups_batch
                    ON invoice_groups(batch_id);
                CREATE INDEX IF NOT EXISTS idx_group_members_invoice
                    ON invoice_group_members(invoice_id);
                PRAGMA user_version = 2;",
            )?;
            version = 2;
        }

        if version == 2 {
            self.conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS pipeline_runs (
                    pipeline_id TEXT PRIMARY KEY,
                    config_json TEXT NOT NULL,
                    source_kind TEXT NOT NULL,
                    stage TEXT NOT NULL,
                    status TEXT NOT NULL,
                    task_dir TEXT NOT NULL,
                    batch_id INTEGER UNIQUE,
                    last_error TEXT,
                    created_at TEXT NOT NULL,
                    updated_at TEXT NOT NULL,
                    FOREIGN KEY (batch_id) REFERENCES batches(id) ON DELETE SET NULL
                );
                CREATE INDEX IF NOT EXISTS idx_pipeline_runs_status
                    ON pipeline_runs(status, updated_at);
                PRAGMA user_version = 3;",
            )?;
            version = 3;
        }

        if version == 3 {
            self.conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS review_actions (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    batch_id INTEGER NOT NULL,
                    action_type TEXT NOT NULL,
                    summary TEXT NOT NULL,
                    before_json TEXT NOT NULL,
                    after_json TEXT NOT NULL,
                    created_at TEXT NOT NULL,
                    undone_at TEXT,
                    FOREIGN KEY (batch_id) REFERENCES batches(id) ON DELETE CASCADE
                );
                CREATE INDEX IF NOT EXISTS idx_review_actions_batch
                    ON review_actions(batch_id, id DESC);
                PRAGMA user_version = 4;",
            )?;
            version = 4;
        }

        if version == 4 {
            self.conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS excluded_invoices (
                    invoice_id INTEGER PRIMARY KEY,
                    reason TEXT NOT NULL,
                    excluded_at TEXT NOT NULL,
                    FOREIGN KEY (invoice_id) REFERENCES reported_invoices(id) ON DELETE CASCADE
                );
                PRAGMA user_version = 5;",
            )?;
            version = 5;
        }

        if version == 5 {
            self.conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS concur_send_sessions (
                    batch_id INTEGER PRIMARY KEY,
                    sender_email TEXT NOT NULL,
                    recipient_email TEXT NOT NULL,
                    trial_invoice_id INTEGER NOT NULL,
                    trial_status TEXT NOT NULL DEFAULT 'not_started'
                        CHECK(trial_status IN ('not_started', 'sending', 'sent', 'confirmed', 'failed', 'unknown')),
                    confirmed_behavior TEXT
                        CHECK(confirmed_behavior IS NULL OR confirmed_behavior IN ('receipt_library', 'expenseit')),
                    confirmed_at TEXT,
                    created_at TEXT NOT NULL,
                    updated_at TEXT NOT NULL,
                    FOREIGN KEY (batch_id) REFERENCES batches(id) ON DELETE CASCADE,
                    FOREIGN KEY (trial_invoice_id) REFERENCES reported_invoices(id) ON DELETE RESTRICT
                );
                CREATE TABLE IF NOT EXISTS concur_send_items (
                    batch_id INTEGER NOT NULL,
                    invoice_id INTEGER NOT NULL,
                    idempotency_key TEXT NOT NULL UNIQUE,
                    attachment_name TEXT NOT NULL,
                    attachment_sha256 TEXT NOT NULL,
                    status TEXT NOT NULL DEFAULT 'pending'
                        CHECK(status IN ('pending', 'sending', 'sent', 'failed', 'unknown')),
                    attempt_count INTEGER NOT NULL DEFAULT 0,
                    last_error TEXT,
                    message_id TEXT,
                    sent_at TEXT,
                    updated_at TEXT NOT NULL,
                    PRIMARY KEY (batch_id, invoice_id),
                    FOREIGN KEY (batch_id) REFERENCES batches(id) ON DELETE CASCADE,
                    FOREIGN KEY (invoice_id) REFERENCES reported_invoices(id) ON DELETE RESTRICT
                );
                CREATE INDEX IF NOT EXISTS idx_concur_send_items_status
                    ON concur_send_items(batch_id, status, invoice_id);",
            )?;
            migration_hook("after_v6_ddl_before_version")?;
            self.conn.execute("PRAGMA user_version = 6", [])?;
            version = 6;
        }

        if version == 6 {
            // 旧流水线只查询已经入库的历史记录，无法发现同一次流水线中的重复
            // 发票号。v7 只补标同批次中较晚的记录，不删除任何原件；若该批次
            // 已有人工作出“非重复”决定，则整批跳过，避免覆盖用户审核结论。
            migration_hook("before_v7_in_batch_duplicate_backfill")?;
            let review_actions_exist = self.conn.query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM sqlite_master
                    WHERE type = 'table' AND name = 'review_actions'
                 )",
                [],
                |row| row.get::<_, i64>(0),
            )? != 0;
            let update_without_review_guard = "UPDATE reported_invoices AS current
                 SET is_duplicate = 1,
                     duplicate_reason = '同一批次内发票号一致；已保留该号码的首条记录，需人工确认',
                     updated_at = datetime('now')
                 WHERE current.is_duplicate = 0
                   AND trim(current.invoice_number) <> ''
                   AND EXISTS (
                       SELECT 1
                       FROM reported_invoices AS earlier
                       WHERE earlier.batch_id = current.batch_id
                         AND earlier.id < current.id
                         AND trim(earlier.invoice_number) = trim(current.invoice_number)
                   )";
            if review_actions_exist {
                self.conn.execute(
                    "UPDATE reported_invoices AS current
                     SET is_duplicate = 1,
                         duplicate_reason = '同一批次内发票号一致；已保留该号码的首条记录，需人工确认',
                         updated_at = datetime('now')
                     WHERE current.is_duplicate = 0
                       AND trim(current.invoice_number) <> ''
                       AND EXISTS (
                           SELECT 1
                           FROM reported_invoices AS earlier
                           WHERE earlier.batch_id = current.batch_id
                             AND earlier.id < current.id
                             AND trim(earlier.invoice_number) = trim(current.invoice_number)
                       )
                       AND NOT EXISTS (
                           SELECT 1
                           FROM review_actions AS action
                           WHERE action.batch_id = current.batch_id
                             AND action.action_type = 'duplicate_resolved'
                             AND action.undone_at IS NULL
                       )",
                    [],
                )?;
            } else {
                self.conn.execute(update_without_review_guard, [])?;
            }
            migration_hook("after_v7_in_batch_duplicate_backfill_before_version")?;
            self.conn.execute("PRAGMA user_version = 7", [])?;
            version = 7;
        }

        if version == 7 {
            self.conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS batch_review_snapshots (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    batch_id INTEGER NOT NULL,
                    version INTEGER NOT NULL,
                    content_json TEXT NOT NULL,
                    content_sha256 TEXT NOT NULL,
                    invoice_count INTEGER NOT NULL,
                    total_amount TEXT NOT NULL,
                    created_at TEXT NOT NULL,
                    invalidated_at TEXT,
                    UNIQUE(batch_id, version),
                    FOREIGN KEY (batch_id) REFERENCES batches(id) ON DELETE CASCADE
                );
                CREATE UNIQUE INDEX IF NOT EXISTS idx_review_snapshot_active
                    ON batch_review_snapshots(batch_id)
                    WHERE invalidated_at IS NULL;
                CREATE TABLE IF NOT EXISTS delivery_tasks (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    batch_id INTEGER NOT NULL,
                    review_snapshot_id INTEGER NOT NULL,
                    kind TEXT NOT NULL CHECK(kind IN ('excel', 'concur')),
                    status TEXT NOT NULL DEFAULT 'pending'
                        CHECK(status IN ('pending', 'running', 'succeeded', 'failed')),
                    output_path TEXT,
                    last_error TEXT,
                    created_at TEXT NOT NULL,
                    updated_at TEXT NOT NULL,
                    completed_at TEXT,
                    UNIQUE(review_snapshot_id, kind),
                    FOREIGN KEY (batch_id) REFERENCES batches(id) ON DELETE CASCADE,
                    FOREIGN KEY (review_snapshot_id) REFERENCES batch_review_snapshots(id) ON DELETE RESTRICT
                );
                CREATE INDEX IF NOT EXISTS idx_delivery_tasks_batch
                    ON delivery_tasks(batch_id, id DESC);
                PRAGMA user_version = 8;",
            )?;
            version = 8;
        }

        if version == 8 {
            self.conn.execute_batch(
                "ALTER TABLE pipeline_runs RENAME TO pipeline_runs_v8;
                 CREATE TABLE pipeline_runs (
                    pipeline_id TEXT PRIMARY KEY,
                    config_json TEXT NOT NULL,
                    source_kind TEXT NOT NULL,
                    stage TEXT NOT NULL,
                    status TEXT NOT NULL,
                    task_dir TEXT NOT NULL,
                    batch_id INTEGER,
                    last_error TEXT,
                    created_at TEXT NOT NULL,
                    updated_at TEXT NOT NULL,
                    FOREIGN KEY (batch_id) REFERENCES batches(id) ON DELETE SET NULL
                 );
                 INSERT INTO pipeline_runs (
                    pipeline_id, config_json, source_kind, stage, status, task_dir,
                    batch_id, last_error, created_at, updated_at
                 ) SELECT pipeline_id, config_json, source_kind, stage, status, task_dir,
                          batch_id, last_error, created_at, updated_at
                   FROM pipeline_runs_v8;
                 DROP TABLE pipeline_runs_v8;
                 CREATE INDEX IF NOT EXISTS idx_pipeline_runs_status
                    ON pipeline_runs(status, updated_at);
                 CREATE INDEX IF NOT EXISTS idx_pipeline_runs_batch
                    ON pipeline_runs(batch_id, updated_at);
                 PRAGMA user_version = 9;",
            )?;
            let batch_ids = {
                let mut statement = self.conn.prepare("SELECT id FROM batches ORDER BY id")?;
                let values = statement
                    .query_map([], |row| row.get::<_, i64>(0))?
                    .collect::<Result<Vec<_>, _>>()?;
                values
            };
            for batch_id in batch_ids {
                // v7 只补了重复标记；从 v9 起，疑似重复票默认不进入金额与张数。
                Self::update_batch_stats_for_connection(&self.conn, batch_id)?;
            }
            version = 9;
        }

        if version == 9 {
            self.conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS expense_items (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    batch_id INTEGER NOT NULL,
                    primary_invoice_id INTEGER NOT NULL UNIQUE,
                    model_version INTEGER NOT NULL DEFAULT 1,
                    category_code TEXT NOT NULL,
                    category_source TEXT NOT NULL DEFAULT 'parser.classification',
                    category_confirmed INTEGER NOT NULL DEFAULT 0,
                    transaction_date TEXT NOT NULL,
                    transaction_date_source TEXT NOT NULL,
                    transaction_date_confirmed INTEGER NOT NULL DEFAULT 0,
                    description TEXT NOT NULL DEFAULT '',
                    counterparty_name TEXT NOT NULL DEFAULT '',
                    location_json TEXT NOT NULL DEFAULT '{}',
                    payment_method TEXT NOT NULL DEFAULT 'unknown',
                    gross_amount TEXT NOT NULL,
                    currency_code TEXT NOT NULL DEFAULT 'CNY',
                    tax_details_json TEXT NOT NULL DEFAULT '[]',
                    trip_group_id INTEGER,
                    inclusion_status TEXT NOT NULL DEFAULT 'included'
                        CHECK(inclusion_status IN ('included', 'duplicate_suspect', 'excluded')),
                    provenance_json TEXT NOT NULL DEFAULT '{}',
                    created_at TEXT NOT NULL,
                    updated_at TEXT NOT NULL,
                    FOREIGN KEY (batch_id) REFERENCES batches(id) ON DELETE CASCADE,
                    FOREIGN KEY (primary_invoice_id) REFERENCES reported_invoices(id) ON DELETE CASCADE
                );
                CREATE INDEX IF NOT EXISTS idx_expense_items_batch
                    ON expense_items(batch_id, transaction_date, id);
                CREATE INDEX IF NOT EXISTS idx_expense_items_trip_group
                    ON expense_items(batch_id, trip_group_id);

                CREATE TABLE IF NOT EXISTS invoice_documents (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    batch_id INTEGER NOT NULL,
                    expense_item_id INTEGER NOT NULL,
                    source_invoice_id INTEGER,
                    role TEXT NOT NULL
                        CHECK(role IN ('main_invoice', 'itinerary', 'detail', 'supporting', 'duplicate_copy')),
                    file_path TEXT NOT NULL,
                    original_name TEXT NOT NULL,
                    mime_type TEXT,
                    sha256 TEXT,
                    created_at TEXT NOT NULL,
                    UNIQUE(expense_item_id, file_path),
                    FOREIGN KEY (batch_id) REFERENCES batches(id) ON DELETE CASCADE,
                    FOREIGN KEY (expense_item_id) REFERENCES expense_items(id) ON DELETE CASCADE,
                    FOREIGN KEY (source_invoice_id) REFERENCES reported_invoices(id) ON DELETE SET NULL
                );
                CREATE INDEX IF NOT EXISTS idx_invoice_documents_expense
                    ON invoice_documents(expense_item_id, role, id);

                CREATE TABLE IF NOT EXISTS concur_mapping_profiles (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    name TEXT NOT NULL,
                    company_label TEXT NOT NULL,
                    version INTEGER NOT NULL,
                    status TEXT NOT NULL DEFAULT 'active'
                        CHECK(status IN ('active', 'archived')),
                    adapter_kind TEXT NOT NULL DEFAULT 'ui_assisted'
                        CHECK(adapter_kind IN ('ui_assisted', 'api')),
                    field_rules_json TEXT NOT NULL DEFAULT '{}',
                    expense_type_map_json TEXT NOT NULL DEFAULT '{}',
                    location_map_json TEXT NOT NULL DEFAULT '{}',
                    payment_type_map_json TEXT NOT NULL DEFAULT '{}',
                    vat_rate_map_json TEXT NOT NULL DEFAULT '{}',
                    required_fields_json TEXT NOT NULL DEFAULT '[]',
                    custom_fields_json TEXT NOT NULL DEFAULT '{}',
                    created_at TEXT NOT NULL,
                    updated_at TEXT NOT NULL,
                    UNIQUE(name, version)
                );
                CREATE UNIQUE INDEX IF NOT EXISTS idx_concur_mapping_profile_active_name
                    ON concur_mapping_profiles(name) WHERE status = 'active';

                CREATE TABLE IF NOT EXISTS concur_upload_sessions (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    batch_id INTEGER NOT NULL,
                    review_snapshot_id INTEGER NOT NULL,
                    mapping_profile_id INTEGER NOT NULL,
                    mapping_profile_version INTEGER NOT NULL,
                    report_name TEXT NOT NULL,
                    report_date TEXT NOT NULL,
                    comment TEXT NOT NULL DEFAULT '',
                    status TEXT NOT NULL DEFAULT 'preflight'
                        CHECK(status IN ('preflight', 'ready', 'running', 'partial', 'draft_created', 'needs_verification', 'failed')),
                    idempotency_key TEXT NOT NULL UNIQUE,
                    external_report_id TEXT,
                    upload_overrides_json TEXT NOT NULL DEFAULT '{}',
                    mapped_payload_json TEXT NOT NULL,
                    gaps_json TEXT NOT NULL DEFAULT '[]',
                    last_error TEXT,
                    created_at TEXT NOT NULL,
                    updated_at TEXT NOT NULL,
                    FOREIGN KEY (batch_id) REFERENCES batches(id) ON DELETE CASCADE,
                    FOREIGN KEY (review_snapshot_id) REFERENCES batch_review_snapshots(id) ON DELETE RESTRICT,
                    FOREIGN KEY (mapping_profile_id) REFERENCES concur_mapping_profiles(id) ON DELETE RESTRICT
                );
                CREATE INDEX IF NOT EXISTS idx_concur_upload_sessions_batch
                    ON concur_upload_sessions(batch_id, id DESC);

                CREATE TABLE IF NOT EXISTS concur_upload_items (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    session_id INTEGER NOT NULL,
                    expense_item_id INTEGER NOT NULL,
                    status TEXT NOT NULL DEFAULT 'pending'
                        CHECK(status IN ('pending', 'running', 'created', 'needs_verification', 'failed')),
                    idempotency_key TEXT NOT NULL UNIQUE,
                    mapped_payload_json TEXT NOT NULL,
                    external_expense_id TEXT,
                    attempt_count INTEGER NOT NULL DEFAULT 0,
                    last_error TEXT,
                    last_verified_at TEXT,
                    updated_at TEXT NOT NULL,
                    UNIQUE(session_id, expense_item_id),
                    FOREIGN KEY (session_id) REFERENCES concur_upload_sessions(id) ON DELETE CASCADE,
                    FOREIGN KEY (expense_item_id) REFERENCES expense_items(id) ON DELETE RESTRICT
                );

                CREATE TABLE IF NOT EXISTS concur_upload_attachments (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    upload_item_id INTEGER NOT NULL,
                    document_id INTEGER NOT NULL,
                    status TEXT NOT NULL DEFAULT 'pending'
                        CHECK(status IN ('pending', 'running', 'uploaded', 'needs_verification', 'failed')),
                    idempotency_key TEXT NOT NULL UNIQUE,
                    external_attachment_id TEXT,
                    attempt_count INTEGER NOT NULL DEFAULT 0,
                    last_error TEXT,
                    last_verified_at TEXT,
                    updated_at TEXT NOT NULL,
                    UNIQUE(upload_item_id, document_id),
                    FOREIGN KEY (upload_item_id) REFERENCES concur_upload_items(id) ON DELETE CASCADE,
                    FOREIGN KEY (document_id) REFERENCES invoice_documents(id) ON DELETE RESTRICT
                );",
            )?;
            let invoice_ids = {
                let mut statement = self
                    .conn
                    .prepare("SELECT id FROM reported_invoices ORDER BY id")?;
                let ids = statement
                    .query_map([], |row| row.get::<_, i64>(0))?
                    .collect::<Result<Vec<_>, _>>()?;
                ids
            };
            for invoice_id in invoice_ids {
                Self::ensure_expense_item_for_invoice(&self.conn, invoice_id)?;
            }
            migration_hook("after_v10_expense_backfill_before_version")?;
            self.conn.execute("PRAGMA user_version = 10", [])?;
            version = 10;
        }

        if version == 10 {
            self.conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS pending_invoice_documents (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    batch_id INTEGER NOT NULL,
                    proposed_role TEXT NOT NULL
                        CHECK(proposed_role IN ('itinerary', 'detail', 'supporting')),
                    file_path TEXT NOT NULL,
                    original_name TEXT NOT NULL,
                    mime_type TEXT,
                    sha256 TEXT,
                    detection_reason TEXT NOT NULL,
                    status TEXT NOT NULL DEFAULT 'pending'
                        CHECK(status IN ('pending', 'attached', 'ignored')),
                    assigned_expense_item_id INTEGER,
                    created_at TEXT NOT NULL,
                    updated_at TEXT NOT NULL,
                    UNIQUE(batch_id, file_path),
                    FOREIGN KEY (batch_id) REFERENCES batches(id) ON DELETE CASCADE,
                    FOREIGN KEY (assigned_expense_item_id) REFERENCES expense_items(id) ON DELETE SET NULL
                );
                CREATE INDEX IF NOT EXISTS idx_pending_invoice_documents_batch
                    ON pending_invoice_documents(batch_id, status, id);",
            )?;
            let pending_source_column_exists = self.conn.query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM pragma_table_info('invoice_documents')
                    WHERE name = 'source_pending_document_id'
                 )",
                [],
                |row| row.get::<_, i64>(0),
            )? != 0;
            if !pending_source_column_exists {
                self.conn.execute_batch(
                    "ALTER TABLE invoice_documents
                        ADD COLUMN source_pending_document_id INTEGER
                        REFERENCES pending_invoice_documents(id) ON DELETE SET NULL;",
                )?;
            }
            self.conn.execute_batch(
                "CREATE UNIQUE INDEX IF NOT EXISTS idx_invoice_documents_pending_source
                    ON invoice_documents(source_pending_document_id)
                    WHERE source_pending_document_id IS NOT NULL;",
            )?;
            migration_hook("after_v11_pending_documents_before_version")?;
            self.conn.execute("PRAGMA user_version = 11", [])?;
            version = 11;
        }

        if version == 11 {
            self.conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS email_import_messages (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    batch_id INTEGER NOT NULL,
                    pipeline_id TEXT NOT NULL,
                    mailbox_folder TEXT NOT NULL DEFAULT 'INBOX',
                    uid INTEGER NOT NULL,
                    message_id_sha256 TEXT,
                    sender TEXT NOT NULL DEFAULT '',
                    subject TEXT NOT NULL DEFAULT '',
                    received_at TEXT,
                    status TEXT NOT NULL
                        CHECK(status IN ('imported', 'needs_attachment_review', 'manual_download',
                                         'needs_confirmation', 'not_invoice', 'failed')),
                    resolution_status TEXT NOT NULL DEFAULT 'open'
                        CHECK(resolution_status IN ('open', 'resolved', 'ignored')),
                    error_category TEXT,
                    created_at TEXT NOT NULL,
                    updated_at TEXT NOT NULL,
                    resolved_at TEXT,
                    UNIQUE(pipeline_id, mailbox_folder, uid),
                    FOREIGN KEY (batch_id) REFERENCES batches(id) ON DELETE CASCADE,
                    FOREIGN KEY (pipeline_id) REFERENCES pipeline_runs(pipeline_id) ON DELETE CASCADE
                );
                CREATE INDEX IF NOT EXISTS idx_email_import_messages_batch
                    ON email_import_messages(batch_id, resolution_status, status, id);

                CREATE TABLE IF NOT EXISTS email_import_attachments (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    message_id INTEGER NOT NULL,
                    content_sha256 TEXT,
                    original_name TEXT NOT NULL,
                    container_name TEXT,
                    mime_type TEXT,
                    byte_len INTEGER NOT NULL DEFAULT 0,
                    status TEXT NOT NULL
                        CHECK(status IN ('invoice', 'supporting', 'duplicate', 'not_invoice',
                                         'unsupported', 'failed')),
                    role_hint TEXT NOT NULL DEFAULT 'unknown'
                        CHECK(role_hint IN ('invoice', 'itinerary', 'detail', 'supporting', 'unknown')),
                    reason TEXT NOT NULL,
                    reported_invoice_id INTEGER,
                    pending_document_id INTEGER,
                    manual_import INTEGER NOT NULL DEFAULT 0,
                    created_at TEXT NOT NULL,
                    updated_at TEXT NOT NULL,
                    FOREIGN KEY (message_id) REFERENCES email_import_messages(id) ON DELETE CASCADE,
                    FOREIGN KEY (reported_invoice_id) REFERENCES reported_invoices(id) ON DELETE SET NULL,
                    FOREIGN KEY (pending_document_id) REFERENCES pending_invoice_documents(id) ON DELETE SET NULL
                );
                CREATE INDEX IF NOT EXISTS idx_email_import_attachments_message
                    ON email_import_attachments(message_id, id);
                CREATE INDEX IF NOT EXISTS idx_email_import_attachments_hash
                    ON email_import_attachments(content_sha256);",
            )?;
            migration_hook("after_v12_email_ledger_before_version")?;
            self.conn.execute("PRAGMA user_version = 12", [])?;
            version = 12;
        }

        if version == 12 {
            self.conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS email_collection_tasks (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    name TEXT NOT NULL,
                    account_email TEXT NOT NULL,
                    mailbox_folder TEXT NOT NULL DEFAULT 'INBOX',
                    date_start TEXT NOT NULL,
                    date_end TEXT NOT NULL,
                    status TEXT NOT NULL DEFAULT 'created'
                        CHECK(status IN ('created', 'collecting', 'review', 'completed',
                                         'failed', 'interrupted')),
                    review_status TEXT NOT NULL DEFAULT 'open'
                        CHECK(review_status IN ('open', 'completed')),
                    pipeline_id TEXT UNIQUE,
                    last_error_category TEXT,
                    scanned_message_count INTEGER NOT NULL DEFAULT 0,
                    candidate_file_count INTEGER NOT NULL DEFAULT 0,
                    actionable_message_count INTEGER NOT NULL DEFAULT 0,
                    created_at TEXT NOT NULL,
                    updated_at TEXT NOT NULL,
                    completed_at TEXT
                );
                CREATE INDEX IF NOT EXISTS idx_email_collection_tasks_status
                    ON email_collection_tasks(status, updated_at DESC, id DESC);

                CREATE TABLE IF NOT EXISTS collected_email_messages (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    task_id INTEGER NOT NULL,
                    legacy_source_message_id INTEGER UNIQUE,
                    mailbox_folder TEXT NOT NULL DEFAULT 'INBOX',
                    uid INTEGER NOT NULL,
                    message_id_sha256 TEXT,
                    sender TEXT NOT NULL DEFAULT '',
                    subject TEXT NOT NULL DEFAULT '',
                    received_at TEXT,
                    status TEXT NOT NULL
                        CHECK(status IN ('has_candidates', 'materials_only', 'manual_download',
                                         'needs_confirmation', 'not_relevant', 'failed')),
                    resolution_status TEXT NOT NULL DEFAULT 'open'
                        CHECK(resolution_status IN ('open', 'resolved', 'ignored')),
                    error_category TEXT,
                    created_at TEXT NOT NULL,
                    updated_at TEXT NOT NULL,
                    resolved_at TEXT,
                    UNIQUE(task_id, mailbox_folder, uid),
                    FOREIGN KEY (task_id) REFERENCES email_collection_tasks(id) ON DELETE CASCADE
                );
                CREATE INDEX IF NOT EXISTS idx_collected_email_messages_task
                    ON collected_email_messages(task_id, resolution_status, status, id);

                CREATE TABLE IF NOT EXISTS collected_email_attachments (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    message_id INTEGER NOT NULL,
                    legacy_source_attachment_id INTEGER UNIQUE,
                    content_sha256 TEXT,
                    original_name TEXT NOT NULL,
                    container_name TEXT,
                    mime_type TEXT,
                    byte_len INTEGER NOT NULL DEFAULT 0,
                    status TEXT NOT NULL
                        CHECK(status IN ('candidate', 'supporting_candidate', 'filtered',
                                         'unsupported', 'failed')),
                    role_hint TEXT NOT NULL DEFAULT 'unknown'
                        CHECK(role_hint IN ('invoice', 'itinerary', 'detail', 'supporting', 'unknown')),
                    reason TEXT NOT NULL,
                    stored_path TEXT,
                    manual_import INTEGER NOT NULL DEFAULT 0,
                    created_at TEXT NOT NULL,
                    updated_at TEXT NOT NULL,
                    FOREIGN KEY (message_id) REFERENCES collected_email_messages(id) ON DELETE CASCADE
                );
                CREATE INDEX IF NOT EXISTS idx_collected_email_attachments_message
                    ON collected_email_attachments(message_id, id);
                CREATE INDEX IF NOT EXISTS idx_collected_email_attachments_hash
                    ON collected_email_attachments(content_sha256);

                CREATE TABLE IF NOT EXISTS batch_collection_imports (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    batch_id INTEGER NOT NULL,
                    task_id INTEGER NOT NULL,
                    status TEXT NOT NULL DEFAULT 'pending'
                        CHECK(status IN ('pending', 'processing', 'completed', 'failed', 'legacy')),
                    pipeline_id TEXT UNIQUE,
                    created_at TEXT NOT NULL,
                    updated_at TEXT NOT NULL,
                    FOREIGN KEY (batch_id) REFERENCES batches(id) ON DELETE CASCADE,
                    FOREIGN KEY (task_id) REFERENCES email_collection_tasks(id) ON DELETE RESTRICT
                );
                CREATE INDEX IF NOT EXISTS idx_batch_collection_imports_batch
                    ON batch_collection_imports(batch_id, id);

                CREATE TABLE IF NOT EXISTS batch_collection_import_items (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    import_id INTEGER NOT NULL,
                    attachment_id INTEGER NOT NULL,
                    source_sha256 TEXT,
                    original_name TEXT NOT NULL,
                    created_at TEXT NOT NULL,
                    UNIQUE(import_id, attachment_id),
                    FOREIGN KEY (import_id) REFERENCES batch_collection_imports(id) ON DELETE CASCADE,
                    FOREIGN KEY (attachment_id) REFERENCES collected_email_attachments(id) ON DELETE RESTRICT
                );
                CREATE INDEX IF NOT EXISTS idx_batch_collection_import_items_attachment
                    ON batch_collection_import_items(attachment_id, import_id);",
            )?;

            // 将 v12 已存在的批次内邮件台账复制为只读历史收集任务；旧表保留，
            // 便于回滚且避免对已审核批次产生任何重写。
            self.conn.execute_batch(
                "INSERT OR IGNORE INTO email_collection_tasks (
                    name, account_email, mailbox_folder, date_start, date_end, status,
                    review_status, pipeline_id, last_error_category,
                    scanned_message_count, candidate_file_count, actionable_message_count,
                    created_at, updated_at, completed_at
                 )
                 SELECT '历史邮件收集 · ' || b.name, '', 'INBOX',
                        COALESCE(substr(MIN(m.received_at), 1, 10), substr(MIN(m.created_at), 1, 10)),
                        COALESCE(substr(MAX(m.received_at), 1, 10), substr(MAX(m.created_at), 1, 10)),
                        'completed', 'completed', 'legacy:' || m.pipeline_id, NULL,
                        COUNT(DISTINCT m.id),
                        COALESCE(SUM(CASE WHEN a.status IN ('invoice', 'supporting', 'duplicate') THEN 1 ELSE 0 END), 0),
                        0, MIN(m.created_at), MAX(m.updated_at), MAX(m.updated_at)
                 FROM email_import_messages m
                 JOIN batches b ON b.id = m.batch_id
                 LEFT JOIN email_import_attachments a ON a.message_id = m.id
                 GROUP BY m.pipeline_id, m.batch_id, b.name;

                 INSERT OR IGNORE INTO collected_email_messages (
                    task_id, legacy_source_message_id, mailbox_folder, uid, message_id_sha256,
                    sender, subject, received_at, status, resolution_status, error_category,
                    created_at, updated_at, resolved_at
                 )
                 SELECT t.id, m.id, m.mailbox_folder, m.uid, m.message_id_sha256,
                        m.sender, m.subject, m.received_at,
                        CASE m.status
                            WHEN 'imported' THEN 'has_candidates'
                            WHEN 'needs_attachment_review' THEN 'materials_only'
                            WHEN 'manual_download' THEN 'manual_download'
                            WHEN 'needs_confirmation' THEN 'needs_confirmation'
                            WHEN 'failed' THEN 'failed'
                            ELSE 'not_relevant' END,
                        m.resolution_status, m.error_category, m.created_at, m.updated_at, m.resolved_at
                 FROM email_import_messages m
                 JOIN email_collection_tasks t ON t.pipeline_id = 'legacy:' || m.pipeline_id;

                 INSERT OR IGNORE INTO collected_email_attachments (
                    message_id, legacy_source_attachment_id, content_sha256, original_name,
                    container_name, mime_type, byte_len, status, role_hint, reason,
                    stored_path, manual_import, created_at, updated_at
                 )
                 SELECT cm.id, a.id, a.content_sha256, a.original_name, a.container_name,
                        a.mime_type, a.byte_len,
                        CASE
                            WHEN a.status IN ('invoice', 'duplicate') THEN 'candidate'
                            WHEN a.status = 'supporting' THEN 'supporting_candidate'
                            WHEN a.status = 'not_invoice' THEN 'filtered'
                            WHEN a.status = 'unsupported' THEN 'unsupported'
                            ELSE 'failed' END,
                        a.role_hint, a.reason,
                        COALESCE(ri.file_path, pd.file_path), a.manual_import,
                        a.created_at, a.updated_at
                 FROM email_import_attachments a
                 JOIN collected_email_messages cm ON cm.legacy_source_message_id = a.message_id
                 LEFT JOIN reported_invoices ri ON ri.id = a.reported_invoice_id
                 LEFT JOIN pending_invoice_documents pd ON pd.id = a.pending_document_id;

                 INSERT OR IGNORE INTO batch_collection_imports (
                    batch_id, task_id, status, pipeline_id, created_at, updated_at
                 )
                 SELECT DISTINCT m.batch_id, t.id, 'legacy', 'legacy-import:' || m.pipeline_id,
                        MIN(m.created_at), MAX(m.updated_at)
                 FROM email_import_messages m
                 JOIN email_collection_tasks t ON t.pipeline_id = 'legacy:' || m.pipeline_id
                 GROUP BY m.batch_id, t.id, m.pipeline_id;

                 INSERT OR IGNORE INTO batch_collection_import_items (
                    import_id, attachment_id, source_sha256, original_name, created_at
                 )
                 SELECT bi.id, ca.id, ca.content_sha256, ca.original_name, ca.created_at
                 FROM batch_collection_imports bi
                 JOIN email_collection_tasks t ON t.id = bi.task_id
                 JOIN collected_email_messages cm ON cm.task_id = t.id
                 JOIN collected_email_attachments ca ON ca.message_id = cm.id
                 WHERE bi.status = 'legacy' AND ca.stored_path IS NOT NULL;",
            )?;
            migration_hook("after_v13_email_collection_workbench_before_version")?;
            self.conn.execute("PRAGMA user_version = 13", [])?;
            version = 13;
        }

        if version == 13 {
            for (column, definition) in [
                ("review_sender_name", "review_sender_name TEXT"),
                ("review_sender_address", "review_sender_address TEXT"),
                ("review_body_text", "review_body_text TEXT"),
                (
                    "review_body_truncated",
                    "review_body_truncated INTEGER NOT NULL DEFAULT 0",
                ),
                ("review_analyzed_at", "review_analyzed_at TEXT"),
            ] {
                let exists: bool = self.conn.query_row(
                    "SELECT EXISTS(
                        SELECT 1 FROM pragma_table_info('collected_email_messages') WHERE name = ?1
                     )",
                    params![column],
                    |row| row.get(0),
                )?;
                if !exists {
                    self.conn.execute(
                        &format!("ALTER TABLE collected_email_messages ADD COLUMN {definition}"),
                        [],
                    )?;
                }
            }
            self.conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS collected_email_links (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    message_id INTEGER NOT NULL,
                    position INTEGER NOT NULL,
                    label TEXT NOT NULL,
                    host TEXT NOT NULL,
                    url TEXT NOT NULL,
                    scheme TEXT NOT NULL CHECK(scheme IN ('http', 'https')),
                    created_at TEXT NOT NULL,
                    updated_at TEXT NOT NULL,
                    UNIQUE(message_id, position),
                    FOREIGN KEY (message_id) REFERENCES collected_email_messages(id) ON DELETE CASCADE
                 );
                 CREATE INDEX IF NOT EXISTS idx_collected_email_links_message
                    ON collected_email_links(message_id, position);",
            )?;
            migration_hook("after_v14_collected_email_review_cache_before_version")?;
            self.conn.execute("PRAGMA user_version = 14", [])?;
            version = 14;
        }

        if version == 14 {
            for (column, definition) in [
                ("user_excluded", "user_excluded INTEGER NOT NULL DEFAULT 0"),
                ("user_excluded_at", "user_excluded_at TEXT"),
            ] {
                let exists: bool = self.conn.query_row(
                    "SELECT EXISTS(
                        SELECT 1 FROM pragma_table_info('collected_email_attachments') WHERE name = ?1
                     )",
                    params![column],
                    |row| row.get(0),
                )?;
                if !exists {
                    self.conn.execute(
                        &format!("ALTER TABLE collected_email_attachments ADD COLUMN {definition}"),
                        [],
                    )?;
                }
            }
            migration_hook("after_v15_collected_attachment_user_exclusion_before_version")?;
            self.conn.execute("PRAGMA user_version = 15", [])?;
            version = 15;
        }

        if version == 15 {
            // 一项稳定费用只能属于一个有效归组。旧版本可能因同票多格式输入，
            // 同时留下差旅组和市内组成员关系；优先保留费用事实已经选择的组，
            // 其余历史冲突保留在 grouping ambiguities/review 日志中供追溯。
            self.conn.execute_batch(
                "DELETE FROM invoice_group_members AS member
                 WHERE EXISTS (
                     SELECT 1 FROM expense_items expense
                     WHERE expense.primary_invoice_id = member.invoice_id
                       AND expense.trip_group_id IS NOT NULL
                       AND expense.trip_group_id <> member.group_id
                 );
                 DELETE FROM invoice_group_members AS member
                 WHERE EXISTS (
                     SELECT 1 FROM invoice_group_members earlier
                     WHERE earlier.invoice_id = member.invoice_id
                       AND earlier.group_id < member.group_id
                 );
                 CREATE UNIQUE INDEX IF NOT EXISTS idx_group_members_unique_invoice
                     ON invoice_group_members(invoice_id);
                 PRAGMA user_version = 16;",
            )?;
            version = 16;
        }

        if version == 16 {
            for (column, definition) in [
                (
                    "category_source",
                    "category_source TEXT NOT NULL DEFAULT 'parser.classification'",
                ),
                (
                    "category_confirmed",
                    "category_confirmed INTEGER NOT NULL DEFAULT 0",
                ),
            ] {
                let exists: bool = self.conn.query_row(
                    "SELECT EXISTS(
                        SELECT 1 FROM pragma_table_info('expense_items') WHERE name = ?1
                     )",
                    params![column],
                    |row| row.get(0),
                )?;
                if !exists {
                    self.conn.execute(
                        &format!("ALTER TABLE expense_items ADD COLUMN {definition}"),
                        [],
                    )?;
                }
            }
            self.conn.execute_batch(
                "UPDATE expense_items
                 SET category_source = CASE
                         WHEN category_code = 'other' THEN 'unclassified'
                         ELSE 'parser.classification'
                     END,
                     category_confirmed = CASE
                         WHEN category_code = 'other' THEN 0
                         ELSE 1
                     END;
                 PRAGMA user_version = 17;",
            )?;
            version = 17;
        }

        if version == 17 {
            // 旧版 XML/OFD 验签使用了不符合规范的简化算法：XML 直接验整个文档，
            // OFD 可能把 Signatures.xml 当作 SignedValue.dat，并把结果传播给同票 PDF。
            // 因此历史 invalid 不能作为“密码学已证明无效”的证据，统一降级为非阻断的
            // unsupported，等待完整 XMLDSig / OFD SES 验签器重新分析。
            self.conn.execute_batch(
                "UPDATE reported_invoices
                 SET verification_result = 'unsupported',
                     updated_at = CURRENT_TIMESTAMP
                 WHERE verification_result = 'invalid';
                 PRAGMA user_version = 18;",
            )?;
            version = 18;
        }

        if version == 18 {
            // PDF 打印合订本与 Excel、Concur 一样绑定冻结审核版本，并在同一
            // 交付历史中记录。SQLite 不能直接修改 CHECK 约束，因此保留原记录
            // 重建表；没有其他表引用 delivery_tasks，可安全进行本次迁移。
            self.conn.execute_batch(
                "ALTER TABLE delivery_tasks RENAME TO delivery_tasks_v18;
                 CREATE TABLE delivery_tasks (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    batch_id INTEGER NOT NULL,
                    review_snapshot_id INTEGER NOT NULL,
                    kind TEXT NOT NULL CHECK(kind IN ('excel', 'pdf', 'concur')),
                    status TEXT NOT NULL DEFAULT 'pending'
                        CHECK(status IN ('pending', 'running', 'succeeded', 'failed')),
                    output_path TEXT,
                    last_error TEXT,
                    created_at TEXT NOT NULL,
                    updated_at TEXT NOT NULL,
                    completed_at TEXT,
                    UNIQUE(review_snapshot_id, kind),
                    FOREIGN KEY (batch_id) REFERENCES batches(id) ON DELETE CASCADE,
                    FOREIGN KEY (review_snapshot_id) REFERENCES batch_review_snapshots(id) ON DELETE RESTRICT
                 );
                 INSERT INTO delivery_tasks (
                    id, batch_id, review_snapshot_id, kind, status, output_path,
                    last_error, created_at, updated_at, completed_at
                 ) SELECT id, batch_id, review_snapshot_id, kind, status, output_path,
                          last_error, created_at, updated_at, completed_at
                   FROM delivery_tasks_v18;
                 DROP TABLE delivery_tasks_v18;
                 CREATE INDEX idx_delivery_tasks_batch
                    ON delivery_tasks(batch_id, id DESC);
                 PRAGMA user_version = 19;",
            )?;
            version = 19;
        }

        debug_assert_eq!(version, LEDGER_SCHEMA_VERSION);

        Ok(())
    }

    /// 创建一条尚未接触来源的流水线。授权码绝不进入 config_json。
    pub fn create_pipeline_run(
        &self,
        pipeline_id: &str,
        config_json: &str,
        source_kind: &str,
        task_dir: &str,
    ) -> StoreResult<()> {
        Self::validate_pipeline_labels("created", "running")?;
        if !matches!(source_kind, "local" | "email" | "collection_import") {
            return Err(StoreError::Validation(
                "invalid pipeline source kind".to_string(),
            ));
        }
        let now = Self::now_text();
        self.conn.execute(
            "INSERT INTO pipeline_runs (
                pipeline_id, config_json, source_kind, stage, status, task_dir,
                batch_id, last_error, created_at, updated_at
             ) VALUES (?1, ?2, ?3, 'created', 'running', ?4, NULL, NULL, ?5, ?5)",
            params![pipeline_id, config_json, source_kind, task_dir, now],
        )?;
        Ok(())
    }

    pub fn get_pipeline_run(&self, pipeline_id: &str) -> StoreResult<PipelineRun> {
        self.conn
            .query_row(
                "SELECT pipeline_id, config_json, source_kind, stage, status, task_dir,
                        batch_id, last_error, created_at, updated_at
                 FROM pipeline_runs WHERE pipeline_id = ?1",
                params![pipeline_id],
                Self::parse_pipeline_run_row,
            )
            .map_err(Into::into)
    }

    pub fn list_recoverable_pipeline_runs(&self) -> StoreResult<Vec<PipelineRun>> {
        let mut statement = self.conn.prepare(
            "SELECT pipeline_id, config_json, source_kind, stage, status, task_dir,
                    batch_id, last_error, created_at, updated_at
             FROM pipeline_runs
             WHERE status IN ('failed', 'interrupted') AND batch_id IS NULL
             ORDER BY updated_at DESC",
        )?;
        let runs = statement
            .query_map([], Self::parse_pipeline_run_row)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(runs)
    }

    /// 应用启动时把上次未正常结束的 running 任务标记为 interrupted。
    pub fn mark_running_pipeline_runs_interrupted(&self) -> StoreResult<usize> {
        let changed = self.conn.execute(
            "UPDATE pipeline_runs
             SET status = 'interrupted',
                 last_error = '应用在任务完成前退出；可从最后一个已验证检查点恢复',
                 updated_at = ?1
             WHERE status = 'running' AND batch_id IS NULL",
            params![Self::now_text()],
        )?;
        Ok(changed)
    }

    pub fn mark_pipeline_running(&self, pipeline_id: &str) -> StoreResult<()> {
        let changed = self.conn.execute(
            "UPDATE pipeline_runs
             SET status = 'running', last_error = NULL, updated_at = ?2
             WHERE pipeline_id = ?1
               AND status IN ('running', 'failed', 'interrupted')
               AND batch_id IS NULL",
            params![pipeline_id, Self::now_text()],
        )?;
        if changed == 0 {
            return Err(StoreError::Validation(
                "pipeline is not recoverable".to_string(),
            ));
        }
        Ok(())
    }

    pub fn update_pipeline_checkpoint(&self, pipeline_id: &str, stage: &str) -> StoreResult<()> {
        Self::validate_pipeline_labels(stage, "running")?;
        let changed = self.conn.execute(
            "UPDATE pipeline_runs
             SET stage = ?2, status = 'running', last_error = NULL, updated_at = ?3
             WHERE pipeline_id = ?1 AND batch_id IS NULL",
            params![pipeline_id, stage, Self::now_text()],
        )?;
        if changed == 0 {
            return Err(StoreError::NotFound(format!("Pipeline {pipeline_id}")));
        }
        Ok(())
    }

    pub fn mark_pipeline_failed(&self, pipeline_id: &str, message: &str) -> StoreResult<()> {
        let safe_message: String = message.chars().take(500).collect();
        let changed = self.conn.execute(
            "UPDATE pipeline_runs
             SET status = 'failed', last_error = ?2, updated_at = ?3
             WHERE pipeline_id = ?1 AND batch_id IS NULL",
            params![pipeline_id, safe_message, Self::now_text()],
        )?;
        if changed == 0 {
            return Err(StoreError::NotFound(format!("Pipeline {pipeline_id}")));
        }
        Ok(())
    }

    /// 用户主动安全停止：保留最后一个已验证检查点，允许之后恢复。
    pub fn mark_pipeline_interrupted(&self, pipeline_id: &str, message: &str) -> StoreResult<()> {
        let safe_message: String = message.chars().take(500).collect();
        let changed = self.conn.execute(
            "UPDATE pipeline_runs
             SET status = 'interrupted', last_error = ?2, updated_at = ?3
             WHERE pipeline_id = ?1 AND status = 'running' AND batch_id IS NULL",
            params![pipeline_id, safe_message, Self::now_text()],
        )?;
        if changed == 0 {
            return Err(StoreError::Validation(
                "pipeline is not running or is already completed".to_string(),
            ));
        }
        Ok(())
    }

    /// 单一事务写入批次、全部发票、归组和流水线完成标记。
    /// 如果同一 pipeline 已完成，则校验批次仍存在并幂等返回原 batch_id。
    pub fn store_pipeline_batch_atomic(
        &self,
        pipeline_id: &str,
        name: &str,
        month: &str,
        target_batch_id: Option<i64>,
        invoices: &[ReportedInvoice],
        grouping: &IndexedBatchGrouping,
    ) -> StoreResult<i64> {
        self.store_pipeline_batch_atomic_with_documents(
            pipeline_id,
            name,
            month,
            target_batch_id,
            invoices,
            grouping,
            &[],
        )
    }

    /// 与 `store_pipeline_batch_atomic` 相同，并把无法自动归属的材料一并写入批次待办。
    #[allow(clippy::too_many_arguments)]
    pub fn store_pipeline_batch_atomic_with_documents(
        &self,
        pipeline_id: &str,
        name: &str,
        month: &str,
        target_batch_id: Option<i64>,
        invoices: &[ReportedInvoice],
        grouping: &IndexedBatchGrouping,
        pending_documents: &[NewPendingInvoiceDocument],
    ) -> StoreResult<i64> {
        self.store_pipeline_batch_atomic_with_email_ledger(
            pipeline_id,
            name,
            month,
            target_batch_id,
            invoices,
            grouping,
            pending_documents,
            &[],
        )
    }

    /// 原子保存批次产物，并把邮件及其逻辑附件与发票/材料数据库主键关联。
    #[allow(clippy::too_many_arguments)]
    pub fn store_pipeline_batch_atomic_with_email_ledger(
        &self,
        pipeline_id: &str,
        name: &str,
        month: &str,
        target_batch_id: Option<i64>,
        invoices: &[ReportedInvoice],
        grouping: &IndexedBatchGrouping,
        pending_documents: &[NewPendingInvoiceDocument],
        email_messages: &[NewEmailImportMessage],
    ) -> StoreResult<i64> {
        let transaction = self.conn.unchecked_transaction()?;
        let existing = transaction
            .query_row(
                "SELECT status, batch_id FROM pipeline_runs WHERE pipeline_id = ?1",
                params![pipeline_id],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<i64>>(1)?)),
            )
            .optional()?;
        let Some((status, existing_batch_id)) = existing else {
            return Err(StoreError::NotFound(format!("Pipeline {pipeline_id}")));
        };
        if let Some(batch_id) = existing_batch_id {
            if status != "completed" {
                return Err(StoreError::Validation(
                    "pipeline has a batch but is not completed".to_string(),
                ));
            }
            let exists: i64 = transaction.query_row(
                "SELECT COUNT(*) FROM batches WHERE id = ?1",
                params![batch_id],
                |row| row.get(0),
            )?;
            if exists != 1 {
                return Err(StoreError::Validation(
                    "completed pipeline batch is missing".to_string(),
                ));
            }
            return Ok(batch_id);
        }
        if status != "running" {
            return Err(StoreError::Validation(
                "pipeline must be running before final storage".to_string(),
            ));
        }

        let now = Self::now_text();
        let batch_id = if let Some(batch_id) = target_batch_id {
            let target_status: i32 = transaction
                .query_row(
                    "SELECT status FROM batches WHERE id = ?1",
                    params![batch_id],
                    |row| row.get(0),
                )
                .optional()?
                .ok_or_else(|| StoreError::NotFound(format!("Batch {batch_id}")))?;
            if target_status != BatchStatus::Draft.to_i32() {
                return Err(StoreError::Validation(
                    "target batch must be in draft review".to_string(),
                ));
            }
            batch_id
        } else {
            transaction.execute(
                "INSERT INTO batches (
                    name, month, status, total_amount, invoice_count, created_at, updated_at
                 ) VALUES (?1, ?2, 0, '0', 0, ?3, ?3)",
                params![name, month, now],
            )?;
            transaction.last_insert_rowid()
        };
        let mut invoice_ids = Vec::with_capacity(invoices.len());
        let mut preexisting_invoice_ids = HashSet::new();
        let mut created_invoice_ids = HashSet::new();
        for invoice in invoices {
            let exact_primary = if invoice.invoice_number.trim().is_empty() {
                None
            } else {
                transaction
                    .query_row(
                        "SELECT existing.id, expense.id
                         FROM reported_invoices existing
                         JOIN expense_items expense ON expense.primary_invoice_id = existing.id
                         WHERE existing.batch_id = ?1
                           AND existing.invoice_number = ?2
                           AND existing.issue_date = ?3
                           AND existing.amount = ?4
                         ORDER BY existing.id LIMIT 1",
                        params![
                            batch_id,
                            invoice.invoice_number,
                            invoice.issue_date.format("%Y-%m-%d").to_string(),
                            invoice.amount.to_string(),
                        ],
                        |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
                    )
                    .optional()?
            };
            if let Some((primary_invoice_id, expense_item_id)) = exact_primary {
                let original_name = Path::new(&invoice.file_path)
                    .file_name()
                    .and_then(|value| value.to_str())
                    .unwrap_or("duplicate-invoice")
                    .to_string();
                transaction.execute(
                    "INSERT OR IGNORE INTO invoice_documents (
                        batch_id, expense_item_id, source_invoice_id, role, file_path,
                        original_name, mime_type, sha256, created_at
                     ) VALUES (?1, ?2, NULL, 'duplicate_copy', ?3, ?4, ?5, NULL, ?6)",
                    params![
                        batch_id,
                        expense_item_id,
                        invoice.file_path,
                        original_name,
                        Self::mime_type_for_path(Path::new(&invoice.file_path)),
                        now,
                    ],
                )?;
                // Keep one entry per input so persisted grouping indexes remain valid. Repeated
                // members map to the same aggregate root and are ignored by the group PK below.
                invoice_ids.push(primary_invoice_id);
                // 同一次导入刚创建的主记录不是“历史已有记录”。把它放进该集合会让
                // 后续组误复用第一个组，进而把整月市内消费吞并进差旅行程。
                if !created_invoice_ids.contains(&primary_invoice_id) {
                    preexisting_invoice_ids.insert(primary_invoice_id);
                }
                continue;
            }
            transaction.execute(
                "INSERT INTO reported_invoices (
                    batch_id, invoice_number, issue_date, amount, tax_amount,
                    buyer_name, seller_name, ticket_type, city, departure_time, checkin_date,
                    file_path, created_at, updated_at, verification_result,
                    is_duplicate, duplicate_reason
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12,
                           ?13, ?13, ?14, ?15, ?16)",
                params![
                    batch_id,
                    invoice.invoice_number,
                    invoice.issue_date.format("%Y-%m-%d").to_string(),
                    invoice.amount.to_string(),
                    invoice.tax_amount.as_ref().map(ToString::to_string),
                    invoice.buyer_name,
                    invoice.seller_name,
                    invoice.ticket_type.to_str(),
                    invoice.city,
                    invoice
                        .departure_time
                        .as_ref()
                        .map(|value| value.format("%Y-%m-%d %H:%M:%S").to_string()),
                    invoice
                        .checkin_date
                        .as_ref()
                        .map(|value| value.format("%Y-%m-%d").to_string()),
                    invoice.file_path,
                    now,
                    invoice.verification_result,
                    if invoice.is_duplicate { 1 } else { 0 },
                    invoice.duplicate_reason,
                ],
            )?;
            let invoice_id = transaction.last_insert_rowid();
            Self::ensure_expense_item_for_invoice(&transaction, invoice_id)?;
            invoice_ids.push(invoice_id);
            created_invoice_ids.insert(invoice_id);
        }

        let mut pending_document_ids = Vec::with_capacity(pending_documents.len());
        for document in pending_documents {
            if !matches!(
                document.proposed_role.as_str(),
                "itinerary" | "detail" | "supporting"
            ) || document.file_path.trim().is_empty()
                || document.original_name.trim().is_empty()
                || document.detection_reason.trim().is_empty()
            {
                return Err(StoreError::Validation(
                    "invalid pending invoice document".to_string(),
                ));
            }
            transaction.execute(
                "INSERT OR IGNORE INTO pending_invoice_documents (
                    batch_id, proposed_role, file_path, original_name, mime_type,
                    sha256, detection_reason, status, assigned_expense_item_id,
                    created_at, updated_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'pending', NULL, ?8, ?8)",
                params![
                    batch_id,
                    document.proposed_role,
                    document.file_path,
                    document.original_name,
                    document.mime_type,
                    document.sha256,
                    document.detection_reason,
                    now,
                ],
            )?;
            let pending_document_id = transaction.query_row(
                "SELECT id FROM pending_invoice_documents
                 WHERE batch_id = ?1 AND file_path = ?2",
                params![batch_id, document.file_path],
                |row| row.get::<_, i64>(0),
            )?;
            if let Some(input_index) = document.auto_assign_invoice_index {
                let invoice_id = invoice_ids.get(input_index).copied().ok_or_else(|| {
                    StoreError::Validation(
                        "pending document auto assignment index is out of range".to_string(),
                    )
                })?;
                let expense_item_id: i64 = transaction.query_row(
                    "SELECT id FROM expense_items
                     WHERE batch_id = ?1 AND primary_invoice_id = ?2",
                    params![batch_id, invoice_id],
                    |row| row.get(0),
                )?;
                transaction.execute(
                    "INSERT OR IGNORE INTO invoice_documents (
                        batch_id, expense_item_id, source_invoice_id,
                        source_pending_document_id, role, file_path, original_name,
                        mime_type, sha256, created_at
                     ) VALUES (?1, ?2, NULL, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                    params![
                        batch_id,
                        expense_item_id,
                        pending_document_id,
                        document.proposed_role,
                        document.file_path,
                        document.original_name,
                        document.mime_type,
                        document.sha256,
                        now,
                    ],
                )?;
                transaction.execute(
                    "UPDATE pending_invoice_documents
                     SET status = 'attached', assigned_expense_item_id = ?2,
                         updated_at = ?3 WHERE id = ?1",
                    params![pending_document_id, expense_item_id, now],
                )?;
            }
            pending_document_ids.push(pending_document_id);
        }

        let existing_ambiguities = transaction
            .query_row(
                "SELECT ambiguities_json FROM batch_grouping WHERE batch_id = ?1",
                params![batch_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        let group_index_offset: i64 = transaction.query_row(
            "SELECT COALESCE(MAX(group_index), -1) + 1
             FROM invoice_groups WHERE batch_id = ?1",
            params![batch_id],
            |row| row.get(0),
        )?;
        if let Some(existing_ambiguities) = existing_ambiguities {
            let mut combined =
                serde_json::from_str::<Vec<serde_json::Value>>(&existing_ambiguities)
                    .unwrap_or_else(|_| vec![serde_json::json!({"legacy": existing_ambiguities})]);
            combined.extend(
                serde_json::from_str::<Vec<serde_json::Value>>(&grouping.ambiguities_json)
                    .unwrap_or_else(|_| {
                        vec![serde_json::json!({"import": grouping.ambiguities_json.clone()})]
                    }),
            );
            let combined_json = serde_json::to_string(&combined).map_err(|error| {
                StoreError::Internal(format!("serialize combined ambiguities: {error}"))
            })?;
            transaction.execute(
                "UPDATE batch_grouping SET rule_version = ?2, home_cities_json = ?3,
                    overall_confidence = MIN(overall_confidence, ?4),
                    ambiguities_json = ?5, created_at = ?6
                 WHERE batch_id = ?1",
                params![
                    batch_id,
                    grouping.rule_version,
                    grouping.home_cities_json,
                    grouping.overall_confidence,
                    combined_json,
                    now,
                ],
            )?;
        } else {
            transaction.execute(
                "INSERT INTO batch_grouping (
                    batch_id, rule_version, home_cities_json, overall_confidence,
                    ambiguities_json, created_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    batch_id,
                    grouping.rule_version,
                    grouping.home_cities_json,
                    grouping.overall_confidence,
                    grouping.ambiguities_json,
                    now,
                ],
            )?;
        }
        for group in &grouping.groups {
            let mut mapped_members = Vec::with_capacity(group.members.len());
            let mut seen_invoice_ids = HashSet::new();
            for member in &group.members {
                let invoice_id = *invoice_ids.get(member.input_index).ok_or_else(|| {
                    StoreError::Validation(format!(
                        "group member index {} is out of range",
                        member.input_index
                    ))
                })?;
                if seen_invoice_ids.insert(invoice_id) {
                    mapped_members.push((
                        invoice_id,
                        member.input_index as i64,
                        member.match_reason.as_str(),
                    ));
                }
            }

            // A duplicate-only import must not create another trip/local group. If the same
            // invoice already anchors a persisted group, reuse that group for any genuinely
            // new members found in the same import instead of splitting one trip in two.
            let mut existing_group_id = None;
            for (invoice_id, _, _) in &mapped_members {
                if !preexisting_invoice_ids.contains(invoice_id) {
                    continue;
                }
                existing_group_id = transaction
                    .query_row(
                        "SELECT m.group_id
                         FROM invoice_group_members m
                         JOIN invoice_groups g ON g.id = m.group_id
                         WHERE m.invoice_id = ?1 AND g.batch_id = ?2
                         ORDER BY m.group_id LIMIT 1",
                        params![invoice_id, batch_id],
                        |row| row.get::<_, i64>(0),
                    )
                    .optional()?;
                if existing_group_id.is_some() {
                    break;
                }
            }
            let has_created_member = mapped_members
                .iter()
                .any(|(invoice_id, _, _)| created_invoice_ids.contains(invoice_id));
            if existing_group_id.is_none() && !has_created_member {
                continue;
            }

            let group_id = if let Some(group_id) = existing_group_id {
                transaction.execute(
                    "UPDATE invoice_groups
                     SET start_date = MIN(start_date, ?2),
                         end_date = MAX(end_date, ?3),
                         requires_review = MAX(requires_review, ?4)
                     WHERE id = ?1 AND batch_id = ?5",
                    params![
                        group_id,
                        group.start_date,
                        group.end_date,
                        if group.requires_review { 1 } else { 0 },
                        batch_id,
                    ],
                )?;
                group_id
            } else {
                transaction.execute(
                    "INSERT INTO invoice_groups (
                        batch_id, group_index, kind, title, start_date, end_date,
                        confidence, requires_review, evidence_json
                     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                    params![
                        batch_id,
                        group_index_offset + group.group_index as i64,
                        group.kind,
                        group.title,
                        group.start_date,
                        group.end_date,
                        group.confidence,
                        if group.requires_review { 1 } else { 0 },
                        group.evidence_json,
                    ],
                )?;
                transaction.last_insert_rowid()
            };
            for (invoice_id, input_index, match_reason) in mapped_members {
                transaction.execute(
                    "INSERT OR IGNORE INTO invoice_group_members (
                        group_id, invoice_id, input_index, match_reason
                     ) VALUES (?1, ?2, ?3, ?4)",
                    params![group_id, invoice_id, input_index, match_reason],
                )?;
            }
        }
        transaction.execute(
            "UPDATE expense_items
             SET trip_group_id = (
                 SELECT m.group_id FROM invoice_group_members m
                 WHERE m.invoice_id = expense_items.primary_invoice_id
                 ORDER BY m.group_id LIMIT 1
             ), updated_at = ?2
             WHERE batch_id = ?1",
            params![batch_id, now],
        )?;
        Self::update_batch_stats_for_connection(&transaction, batch_id)?;
        Self::store_email_import_ledger_for_connection(
            &transaction,
            batch_id,
            pipeline_id,
            email_messages,
            &invoice_ids,
            &pending_document_ids,
            &now,
        )?;
        let changed = transaction.execute(
            "UPDATE pipeline_runs
             SET stage = 'review', status = 'completed', batch_id = ?2,
                 last_error = NULL, updated_at = ?3
             WHERE pipeline_id = ?1 AND status = 'running' AND batch_id IS NULL",
            params![pipeline_id, batch_id, now],
        )?;
        if changed != 1 {
            return Err(StoreError::Validation(
                "pipeline completion state changed concurrently".to_string(),
            ));
        }
        transaction.commit()?;
        Ok(batch_id)
    }

    /// 邮件范围内没有可自动解析文件时，仍然原子保存链接、通知、失败和跳过结果，
    /// 避免“没有发票附件”导致整段邮件审计记录消失。
    pub fn complete_pipeline_with_email_ledger_only(
        &self,
        pipeline_id: &str,
        name: &str,
        month: &str,
        target_batch_id: Option<i64>,
        messages: &[NewEmailImportMessage],
    ) -> StoreResult<i64> {
        if messages.is_empty() {
            return Err(StoreError::Validation(
                "email ledger cannot be empty".to_string(),
            ));
        }
        let transaction = self.conn.unchecked_transaction()?;
        let (status, existing_batch_id) = transaction
            .query_row(
                "SELECT status, batch_id FROM pipeline_runs WHERE pipeline_id = ?1",
                params![pipeline_id],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<i64>>(1)?)),
            )
            .optional()?
            .ok_or_else(|| StoreError::NotFound(format!("Pipeline {pipeline_id}")))?;
        if let Some(batch_id) = existing_batch_id {
            if status == "completed" {
                return Ok(batch_id);
            }
            return Err(StoreError::Validation(
                "pipeline has an unexpected batch before completion".to_string(),
            ));
        }
        if status != "running" {
            return Err(StoreError::Validation(
                "pipeline must be running before email ledger storage".to_string(),
            ));
        }
        let now = Self::now_text();
        let batch_id = if let Some(batch_id) = target_batch_id {
            Self::ensure_batch_draft(&transaction, batch_id)?;
            batch_id
        } else {
            transaction.execute(
                "INSERT INTO batches (
                    name, month, status, total_amount, invoice_count, created_at, updated_at
                 ) VALUES (?1, ?2, 0, '0', 0, ?3, ?3)",
                params![name, month, now],
            )?;
            transaction.last_insert_rowid()
        };
        Self::store_email_import_ledger_for_connection(
            &transaction,
            batch_id,
            pipeline_id,
            messages,
            &[],
            &[],
            &now,
        )?;
        let changed = transaction.execute(
            "UPDATE pipeline_runs
             SET stage = 'review', status = 'completed', batch_id = ?2,
                 last_error = NULL, updated_at = ?3
             WHERE pipeline_id = ?1 AND status = 'running' AND batch_id IS NULL",
            params![pipeline_id, batch_id, now],
        )?;
        if changed != 1 {
            return Err(StoreError::Validation(
                "pipeline completion state changed concurrently".to_string(),
            ));
        }
        transaction.commit()?;
        Ok(batch_id)
    }

    #[allow(clippy::too_many_arguments)]
    fn store_email_import_ledger_for_connection(
        transaction: &Transaction<'_>,
        batch_id: i64,
        pipeline_id: &str,
        messages: &[NewEmailImportMessage],
        invoice_ids: &[i64],
        pending_document_ids: &[i64],
        now: &str,
    ) -> StoreResult<()> {
        for message in messages {
            let message_id = if let Some(existing_message_id) = message.existing_message_id {
                let existing_batch_id = transaction
                    .query_row(
                        "SELECT batch_id FROM email_import_messages WHERE id = ?1",
                        params![existing_message_id],
                        |row| row.get::<_, i64>(0),
                    )
                    .optional()?
                    .ok_or_else(|| {
                        StoreError::NotFound(format!("Email import message {existing_message_id}"))
                    })?;
                if existing_batch_id != batch_id {
                    return Err(StoreError::Validation(
                        "manual email supplement must target the same batch".to_string(),
                    ));
                }
                existing_message_id
            } else {
                if message.mailbox_folder.trim().is_empty()
                    || message.sender.chars().count() > 500
                    || message.subject.chars().count() > 1_000
                    || !matches!(
                        message.initial_status.as_str(),
                        "imported"
                            | "needs_attachment_review"
                            | "manual_download"
                            | "needs_confirmation"
                            | "not_invoice"
                            | "failed"
                    )
                {
                    return Err(StoreError::Validation(
                        "invalid email import message".to_string(),
                    ));
                }
                transaction.execute(
                    "INSERT INTO email_import_messages (
                        batch_id, pipeline_id, mailbox_folder, uid, message_id_sha256,
                        sender, subject, received_at, status, resolution_status,
                        error_category, created_at, updated_at, resolved_at
                     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 'open', ?10, ?11, ?11, NULL)",
                    params![
                        batch_id,
                        pipeline_id,
                        message.mailbox_folder,
                        message.uid,
                        message.message_id_sha256,
                        message.sender,
                        message.subject,
                        message.received_at,
                        message.initial_status,
                        message.error_category,
                        now,
                    ],
                )?;
                transaction.last_insert_rowid()
            };

            for attachment in &message.attachments {
                if attachment.original_name.trim().is_empty()
                    || attachment.original_name.chars().count() > 500
                    || attachment.byte_len < 0
                    || !matches!(
                        attachment.role_hint.as_str(),
                        "invoice" | "itinerary" | "detail" | "supporting" | "unknown"
                    )
                {
                    return Err(StoreError::Validation(
                        "invalid email import attachment".to_string(),
                    ));
                }
                let reported_invoice_id = attachment
                    .invoice_input_index
                    .map(|index| {
                        invoice_ids.get(index).copied().ok_or_else(|| {
                            StoreError::Validation(
                                "email attachment invoice index is out of range".to_string(),
                            )
                        })
                    })
                    .transpose()?;
                let pending_document_id = attachment
                    .pending_document_index
                    .map(|index| {
                        pending_document_ids.get(index).copied().ok_or_else(|| {
                            StoreError::Validation(
                                "email attachment material index is out of range".to_string(),
                            )
                        })
                    })
                    .transpose()?;
                if reported_invoice_id.is_some() && pending_document_id.is_some() {
                    return Err(StoreError::Validation(
                        "email attachment cannot map to invoice and material together".to_string(),
                    ));
                }
                let (status, role_hint, reason) = if reported_invoice_id.is_some() {
                    if attachment.is_content_duplicate {
                        (
                            "duplicate".to_string(),
                            "invoice".to_string(),
                            "same_content_as_imported_invoice".to_string(),
                        )
                    } else {
                        (
                            "invoice".to_string(),
                            "invoice".to_string(),
                            "parsed_invoice".to_string(),
                        )
                    }
                } else if let Some(pending_document_id) = pending_document_id {
                    let proposed_role: String = transaction.query_row(
                        "SELECT proposed_role FROM pending_invoice_documents WHERE id = ?1",
                        params![pending_document_id],
                        |row| row.get(0),
                    )?;
                    (
                        "supporting".to_string(),
                        proposed_role,
                        attachment.reason.clone(),
                    )
                } else {
                    if !matches!(
                        attachment.status.as_str(),
                        "duplicate" | "not_invoice" | "unsupported" | "failed"
                    ) {
                        return Err(StoreError::Validation(
                            "unaccounted email attachment candidate".to_string(),
                        ));
                    }
                    (
                        attachment.status.clone(),
                        attachment.role_hint.clone(),
                        attachment.reason.clone(),
                    )
                };
                transaction.execute(
                    "INSERT INTO email_import_attachments (
                        message_id, content_sha256, original_name, container_name, mime_type,
                        byte_len, status, role_hint, reason, reported_invoice_id,
                        pending_document_id, manual_import, created_at, updated_at
                     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?13)",
                    params![
                        message_id,
                        attachment.content_sha256,
                        attachment.original_name,
                        attachment.container_name,
                        attachment.mime_type,
                        attachment.byte_len,
                        status,
                        role_hint,
                        reason,
                        reported_invoice_id,
                        pending_document_id,
                        if attachment.manual_import { 1 } else { 0 },
                        now,
                    ],
                )?;
            }

            let (invoice_count, supporting_count, problem_count): (i64, i64, i64) =
                transaction.query_row(
                    "SELECT
                        COALESCE(SUM(CASE WHEN status IN ('invoice', 'duplicate')
                                             AND reported_invoice_id IS NOT NULL THEN 1 ELSE 0 END), 0),
                        COALESCE(SUM(CASE WHEN status = 'supporting' THEN 1 ELSE 0 END), 0),
                        COALESCE(SUM(CASE WHEN status IN ('unsupported', 'failed') THEN 1 ELSE 0 END), 0)
                     FROM email_import_attachments WHERE message_id = ?1",
                    params![message_id],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )?;
            let current_status: String = transaction.query_row(
                "SELECT status FROM email_import_messages WHERE id = ?1",
                params![message_id],
                |row| row.get(0),
            )?;
            let final_status = if invoice_count > 0 {
                "imported"
            } else if supporting_count > 0 || problem_count > 0 {
                "needs_attachment_review"
            } else {
                current_status.as_str()
            };
            let automatically_resolved = matches!(final_status, "imported" | "not_invoice");
            transaction.execute(
                "UPDATE email_import_messages
                 SET status = ?2,
                     resolution_status = CASE
                         WHEN ?3 = 1 THEN 'resolved'
                         WHEN resolution_status = 'resolved' THEN 'open'
                         ELSE resolution_status END,
                     resolved_at = CASE WHEN ?3 = 1 THEN ?4 ELSE NULL END,
                     updated_at = ?4
                 WHERE id = ?1",
                params![
                    message_id,
                    final_status,
                    if automatically_resolved { 1 } else { 0 },
                    now,
                ],
            )?;
        }
        Ok(())
    }

    fn validate_pipeline_labels(stage: &str, status: &str) -> StoreResult<()> {
        if !matches!(
            stage,
            "created" | "collected" | "parsed" | "deduped" | "grouped" | "review"
        ) || !matches!(status, "running" | "failed" | "interrupted" | "completed")
        {
            return Err(StoreError::Validation(
                "invalid pipeline stage or status".to_string(),
            ));
        }
        Ok(())
    }

    fn parse_pipeline_run_row(row: &Row<'_>) -> rusqlite::Result<PipelineRun> {
        Ok(PipelineRun {
            pipeline_id: row.get(0)?,
            config_json: row.get(1)?,
            source_kind: row.get(2)?,
            stage: row.get(3)?,
            status: row.get(4)?,
            task_dir: row.get(5)?,
            batch_id: row.get(6)?,
            last_error: row.get(7)?,
            created_at: row.get(8)?,
            updated_at: row.get(9)?,
        })
    }

    fn now_text() -> String {
        Utc::now()
            .naive_utc()
            .format("%Y-%m-%d %H:%M:%S")
            .to_string()
    }

    /// 为兼容发票事实创建一条目标系统无关的费用项和主发票材料。
    /// 该函数幂等，可用于 schema 迁移和每次新增发票后的增量同步。
    fn ensure_expense_item_for_invoice(
        connection: &Connection,
        invoice_id: i64,
    ) -> StoreResult<i64> {
        let invoice = connection
            .query_row(
                "SELECT i.batch_id, i.ticket_type, i.issue_date, i.departure_time,
                        i.checkin_date, i.seller_name, i.city, i.amount, i.tax_amount,
                        i.file_path, i.is_duplicate,
                        EXISTS(SELECT 1 FROM excluded_invoices e WHERE e.invoice_id = i.id),
                        (SELECT m.group_id FROM invoice_group_members m
                         WHERE m.invoice_id = i.id ORDER BY m.group_id LIMIT 1)
                 FROM reported_invoices i WHERE i.id = ?1",
                params![invoice_id],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, Option<String>>(3)?,
                        row.get::<_, Option<String>>(4)?,
                        row.get::<_, Option<String>>(5)?,
                        row.get::<_, Option<String>>(6)?,
                        row.get::<_, String>(7)?,
                        row.get::<_, Option<String>>(8)?,
                        row.get::<_, String>(9)?,
                        row.get::<_, i64>(10)? != 0,
                        row.get::<_, i64>(11)? != 0,
                        row.get::<_, Option<i64>>(12)?,
                    ))
                },
            )
            .optional()?
            .ok_or_else(|| StoreError::NotFound(format!("Invoice {invoice_id}")))?;

        let (transaction_date, transaction_date_source, confirmed) =
            if let Some(value) = invoice.3.as_deref().and_then(|value| value.get(..10)) {
                (value.to_string(), "departure_time".to_string(), true)
            } else if let Some(value) = invoice.4.clone() {
                (value, "checkin_date".to_string(), true)
            } else {
                (
                    invoice.2.clone(),
                    "invoice_issue_date_candidate".to_string(),
                    false,
                )
            };
        let location = ExpenseLocation {
            city_name: invoice.6.clone(),
            city_code: None,
            province_name: None,
            province_code: None,
            country_code: Some("CN".to_string()),
        };
        let location_json = serde_json::to_string(&location).map_err(|error| {
            StoreError::Internal(format!("serialize expense location: {error}"))
        })?;
        let tax_details = invoice
            .8
            .as_deref()
            .and_then(|value| Decimal::from_str(value).ok())
            .map(|amount| {
                vec![ExpenseTaxDetail {
                    amount,
                    rate: None,
                    source: "invoice_tax_amount".to_string(),
                }]
            })
            .unwrap_or_default();
        let tax_details_json = serde_json::to_string(&tax_details)
            .map_err(|error| StoreError::Internal(format!("serialize expense tax: {error}")))?;
        let category_confirmed = invoice.1 != "other";
        let category_source = if category_confirmed {
            "parser.classification"
        } else {
            "unclassified"
        };
        let provenance_json = serde_json::json!({
            "category_code": category_source,
            "transaction_date": transaction_date_source,
            "counterparty_name": "invoice.seller_name",
            "location": "invoice.city",
            "gross_amount": "invoice.amount",
            "currency_code": "default.CNY"
        })
        .to_string();
        let inclusion_status = if invoice.11 {
            "excluded"
        } else if invoice.10 {
            "duplicate_suspect"
        } else {
            "included"
        };
        let now = Self::now_text();
        connection.execute(
            "INSERT OR IGNORE INTO expense_items (
                batch_id, primary_invoice_id, model_version, category_code,
                category_source, category_confirmed,
                transaction_date, transaction_date_source, transaction_date_confirmed,
                description, counterparty_name, location_json, payment_method,
                gross_amount, currency_code, tax_details_json, trip_group_id,
                inclusion_status, provenance_json, created_at, updated_at
             ) VALUES (?1, ?2, 1, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?9, ?10, 'unknown',
                       ?11, 'CNY', ?12, ?13, ?14, ?15, ?16, ?16)",
            params![
                invoice.0,
                invoice_id,
                invoice.1,
                category_source,
                if category_confirmed { 1 } else { 0 },
                transaction_date,
                transaction_date_source,
                if confirmed { 1 } else { 0 },
                invoice.5.unwrap_or_default(),
                location_json,
                invoice.7,
                tax_details_json,
                invoice.12,
                inclusion_status,
                provenance_json,
                now,
            ],
        )?;
        let expense_item_id: i64 = connection.query_row(
            "SELECT id FROM expense_items WHERE primary_invoice_id = ?1",
            params![invoice_id],
            |row| row.get(0),
        )?;
        let original_name = Path::new(&invoice.9)
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("invoice")
            .to_string();
        let mime_type = Self::mime_type_for_path(Path::new(&invoice.9));
        connection.execute(
            "INSERT OR IGNORE INTO invoice_documents (
                batch_id, expense_item_id, source_invoice_id, role, file_path,
                original_name, mime_type, sha256, created_at
             ) VALUES (?1, ?2, ?3, 'main_invoice', ?4, ?5, ?6, NULL, ?7)",
            params![
                invoice.0,
                expense_item_id,
                invoice_id,
                invoice.9,
                original_name,
                mime_type,
                now,
            ],
        )?;
        Ok(expense_item_id)
    }

    fn mime_type_for_path(path: &Path) -> Option<&'static str> {
        match path
            .extension()
            .and_then(|value| value.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase()
            .as_str()
        {
            "pdf" => Some("application/pdf"),
            "ofd" => Some("application/ofd"),
            "xml" => Some("application/xml"),
            "png" => Some("image/png"),
            "jpg" | "jpeg" => Some("image/jpeg"),
            "webp" => Some("image/webp"),
            "bmp" => Some("image/bmp"),
            _ => None,
        }
    }

    /// 创建批次
    pub fn create_batch(&self, name: &str, month: &str) -> StoreResult<i64> {
        let now = Utc::now()
            .naive_utc()
            .format("%Y-%m-%d %H:%M:%S")
            .to_string();

        self.conn.execute(
            "INSERT INTO batches (name, month, status, total_amount, invoice_count, created_at, updated_at)
             VALUES (?1, ?2, 0, '0', 0, ?3, ?4)",
            params![name, month, now, now],
        )?;

        Ok(self.conn.last_insert_rowid())
    }

    /// 获取批次
    pub fn get_batch(&self, id: i64) -> StoreResult<Batch> {
        let batch = self.conn.query_row(
            "SELECT id, name, month, status, total_amount, invoice_count, created_at, updated_at, submitted_at, approved_at, completed_at, rejected_at
             FROM batches WHERE id = ?1",
            params![id],
            Self::parse_batch_row,
        )?;

        Ok(batch)
    }

    /// 列出所有批次
    pub fn list_batches(&self) -> StoreResult<Vec<Batch>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, month, status, total_amount, invoice_count, created_at, updated_at, submitted_at, approved_at, completed_at, rejected_at
             FROM batches ORDER BY created_at DESC"
        )?;

        let batches = stmt
            .query_map([], Self::parse_batch_row)?
            .collect::<Result<Vec<_>, _>>()?;

        Ok(batches)
    }

    /// 按月份列出批次
    pub fn list_batches_by_month(&self, month: &str) -> StoreResult<Vec<Batch>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, month, status, total_amount, invoice_count, created_at, updated_at, submitted_at, approved_at, completed_at, rejected_at
             FROM batches WHERE month = ?1 ORDER BY created_at DESC"
        )?;

        let batches = stmt
            .query_map(params![month], Self::parse_batch_row)?
            .collect::<Result<Vec<_>, _>>()?;

        Ok(batches)
    }

    /// 验证状态转换是否合法
    fn is_valid_transition(from: &BatchStatus, to: &BatchStatus) -> bool {
        use BatchStatus::*;
        matches!(
            (from, to),
            (Draft, Submitted)
                | (Draft, Rejected)
                | (Submitted, Approved)
                | (Submitted, Rejected)
                | (Approved, Completed)
                | (Approved, Rejected)
        )
    }

    /// 转换批次状态（带验证）
    pub fn transition_batch_status(&self, id: i64, new_status: BatchStatus) -> StoreResult<()> {
        // 1. 查询当前状态
        let current_batch = self.get_batch(id)?;

        // 2. 验证转换是否合法
        if !Self::is_valid_transition(&current_batch.status, &new_status) {
            return Err(StoreError::InvalidStateTransition {
                from: format!("{:?}", current_batch.status),
                to: format!("{:?}", new_status),
            });
        }

        // 3. 准备更新的时间戳
        let now = Utc::now()
            .naive_utc()
            .format("%Y-%m-%d %H:%M:%S")
            .to_string();

        // 4. 根据目标状态设置相应的时间戳
        let (submitted_at, approved_at, completed_at, rejected_at) = match new_status {
            BatchStatus::Submitted => (Some(now.clone()), None, None, None),
            BatchStatus::Approved => (
                current_batch
                    .submitted_at
                    .map(|dt| dt.format("%Y-%m-%d %H:%M:%S").to_string()),
                Some(now.clone()),
                None,
                None,
            ),
            BatchStatus::Completed => (
                current_batch
                    .submitted_at
                    .map(|dt| dt.format("%Y-%m-%d %H:%M:%S").to_string()),
                current_batch
                    .approved_at
                    .map(|dt| dt.format("%Y-%m-%d %H:%M:%S").to_string()),
                Some(now.clone()),
                None,
            ),
            BatchStatus::Rejected => (
                current_batch
                    .submitted_at
                    .map(|dt| dt.format("%Y-%m-%d %H:%M:%S").to_string()),
                current_batch
                    .approved_at
                    .map(|dt| dt.format("%Y-%m-%d %H:%M:%S").to_string()),
                current_batch
                    .completed_at
                    .map(|dt| dt.format("%Y-%m-%d %H:%M:%S").to_string()),
                Some(now.clone()),
            ),
            BatchStatus::Draft => (None, None, None, None), // 不应该到达这里
        };

        // 5. 更新数据库
        let rows = self.conn.execute(
            "UPDATE batches SET status = ?1, updated_at = ?2, submitted_at = ?3, approved_at = ?4, completed_at = ?5, rejected_at = ?6 WHERE id = ?7",
            params![new_status.to_i32(), now, submitted_at, approved_at, completed_at, rejected_at, id],
        )?;

        if rows == 0 {
            return Err(StoreError::NotFound(format!("Batch {}", id)));
        }

        Ok(())
    }

    /// 更新批次状态（已废弃，使用 transition_batch_status 替代）
    #[deprecated(note = "Use transition_batch_status instead for state validation")]
    pub fn update_batch_status(&self, id: i64, status: BatchStatus) -> StoreResult<()> {
        let now = Utc::now()
            .naive_utc()
            .format("%Y-%m-%d %H:%M:%S")
            .to_string();

        let submitted_at = if status == BatchStatus::Submitted {
            Some(now.clone())
        } else {
            None
        };

        let rows = self.conn.execute(
            "UPDATE batches SET status = ?1, updated_at = ?2, submitted_at = ?3 WHERE id = ?4",
            params![status.to_i32(), now, submitted_at, id],
        )?;

        if rows == 0 {
            return Err(StoreError::NotFound(format!("Batch {}", id)));
        }

        Ok(())
    }

    /// 删除批次（级联删除发票）
    pub fn delete_batch(&self, id: i64) -> StoreResult<()> {
        let transaction = self.conn.unchecked_transaction()?;
        let rows = transaction.execute("DELETE FROM batches WHERE id = ?1", params![id])?;

        if rows == 0 {
            return Err(StoreError::NotFound(format!("Batch {}", id)));
        }

        // 历史批次是重复结论的依赖。删除来源后，仅对仍处于草稿、且没有用户
        // 重复审核决定的批次重算系统生成的历史命中；用户结论绝不自动覆盖。
        let affected_batches = {
            let mut statement = transaction.prepare(
                "SELECT DISTINCT current.batch_id
                 FROM reported_invoices current
                 JOIN batches batch ON batch.id = current.batch_id AND batch.status = 0
                 WHERE current.is_duplicate <> 0
                   AND (current.duplicate_reason LIKE '发票号与历史台账一致%'
                        OR current.duplicate_reason LIKE '金额、日期和票种与历史台账一致%')
                   AND NOT EXISTS (
                       SELECT 1 FROM review_actions action
                       WHERE action.batch_id = current.batch_id
                         AND action.action_type = 'duplicate_resolved'
                         AND action.undone_at IS NULL
                   )
                   AND NOT EXISTS (
                       SELECT 1 FROM reported_invoices other
                       WHERE other.batch_id <> current.batch_id
                         AND (trim(other.invoice_number) = trim(current.invoice_number)
                              OR (other.amount = current.amount
                                  AND other.issue_date = current.issue_date
                                  AND other.ticket_type = current.ticket_type))
                   )",
            )?;
            let values = statement
                .query_map([], |row| row.get::<_, i64>(0))?
                .collect::<Result<Vec<_>, _>>()?;
            values
        };
        let now = Self::now_text();
        for batch_id in affected_batches {
            transaction.execute(
                "UPDATE reported_invoices AS current
                 SET is_duplicate = 0, duplicate_reason = NULL, updated_at = ?2
                 WHERE current.batch_id = ?1
                   AND current.is_duplicate <> 0
                   AND (current.duplicate_reason LIKE '发票号与历史台账一致%'
                        OR current.duplicate_reason LIKE '金额、日期和票种与历史台账一致%')
                   AND NOT EXISTS (
                       SELECT 1 FROM reported_invoices other
                       WHERE other.batch_id <> current.batch_id
                         AND (trim(other.invoice_number) = trim(current.invoice_number)
                              OR (other.amount = current.amount
                                  AND other.issue_date = current.issue_date
                                  AND other.ticket_type = current.ticket_type))
                   )",
                params![batch_id, now],
            )?;
            transaction.execute(
                "UPDATE expense_items
                 SET inclusion_status = 'included', updated_at = ?2
                 WHERE batch_id = ?1 AND inclusion_status = 'duplicate_suspect'
                   AND EXISTS (
                       SELECT 1 FROM reported_invoices invoice
                       WHERE invoice.id = expense_items.primary_invoice_id
                         AND invoice.is_duplicate = 0
                   )",
                params![batch_id, now],
            )?;
            Self::update_batch_stats_for_connection(&transaction, batch_id)?;
        }
        transaction.commit()?;

        Ok(())
    }

    /// 清空草稿批次中尚未经过用户审核的自动分析结果，以便基于永久来源缓存重跑。
    /// 来源收集任务和冻结的 `batch_collection_import_items` 均保留；原件目录也不在
    /// 数据库事务中删除，因此失败时仍可从迁移备份或旧流水线目录恢复。
    pub fn reset_draft_batch_automatic_analysis(&self, batch_id: i64) -> StoreResult<()> {
        let transaction = self.conn.unchecked_transaction()?;
        let status = transaction
            .query_row(
                "SELECT status FROM batches WHERE id = ?1",
                params![batch_id],
                |row| row.get::<_, i32>(0),
            )
            .optional()?
            .ok_or_else(|| StoreError::NotFound(format!("Batch {batch_id}")))?;
        if status != BatchStatus::Draft.to_i32() {
            return Err(StoreError::Validation(
                "only a draft batch can be reanalyzed".to_string(),
            ));
        }
        let protected_state: i64 = transaction.query_row(
            "SELECT
                (SELECT COUNT(*) FROM review_actions
                 WHERE batch_id = ?1 AND undone_at IS NULL)
              + (SELECT COUNT(*) FROM batch_review_snapshots WHERE batch_id = ?1)
              + (SELECT COUNT(*) FROM delivery_tasks WHERE batch_id = ?1)
              + (SELECT COUNT(*) FROM concur_upload_sessions WHERE batch_id = ?1)",
            params![batch_id],
            |row| row.get(0),
        )?;
        if protected_state != 0 {
            return Err(StoreError::Validation(
                "batch already contains user review or delivery state".to_string(),
            ));
        }
        let running_pipeline_count: i64 = transaction.query_row(
            "SELECT COUNT(*) FROM pipeline_runs
             WHERE batch_id = ?1 AND status = 'running'",
            params![batch_id],
            |row| row.get(0),
        )?;
        if running_pipeline_count != 0 {
            return Err(StoreError::Validation(
                "batch has a running pipeline".to_string(),
            ));
        }

        transaction.execute(
            "UPDATE batch_collection_imports
             SET status = 'pending', pipeline_id = NULL, updated_at = ?2
             WHERE batch_id = ?1",
            params![batch_id, Self::now_text()],
        )?;
        transaction.execute(
            "DELETE FROM pipeline_runs WHERE batch_id = ?1",
            params![batch_id],
        )?;
        transaction.execute(
            "DELETE FROM batch_grouping WHERE batch_id = ?1",
            params![batch_id],
        )?;
        transaction.execute(
            "DELETE FROM pending_invoice_documents WHERE batch_id = ?1",
            params![batch_id],
        )?;
        transaction.execute(
            "DELETE FROM reported_invoices WHERE batch_id = ?1",
            params![batch_id],
        )?;
        transaction.execute(
            "UPDATE batches
             SET total_amount = '0', invoice_count = 0, updated_at = ?2
             WHERE id = ?1",
            params![batch_id, Self::now_text()],
        )?;
        transaction.commit()?;
        Ok(())
    }

    /// 添加发票到批次
    pub fn add_invoice(&self, invoice: &ReportedInvoice) -> StoreResult<i64> {
        let now = Utc::now()
            .naive_utc()
            .format("%Y-%m-%d %H:%M:%S")
            .to_string();

        self.conn.execute(
            "INSERT INTO reported_invoices (
                batch_id, invoice_number, issue_date, amount, tax_amount,
                buyer_name, seller_name, ticket_type, city, departure_time, checkin_date,
                file_path, created_at, updated_at, verification_result, is_duplicate, duplicate_reason
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)",
            params![
                invoice.batch_id,
                invoice.invoice_number,
                invoice.issue_date.format("%Y-%m-%d").to_string(),
                invoice.amount.to_string(),
                invoice.tax_amount.as_ref().map(|d| d.to_string()),
                invoice.buyer_name,
                invoice.seller_name,
                invoice.ticket_type.to_str(),
                invoice.city,
                invoice.departure_time.as_ref().map(|dt| dt.format("%Y-%m-%d %H:%M:%S").to_string()),
                invoice.checkin_date.as_ref().map(|d| d.format("%Y-%m-%d").to_string()),
                invoice.file_path,
                now,
                now,
                invoice.verification_result,
                if invoice.is_duplicate { 1 } else { 0 },
                invoice.duplicate_reason,
            ],
        )?;

        let invoice_id = self.conn.last_insert_rowid();
        Self::ensure_expense_item_for_invoice(&self.conn, invoice_id)?;

        // 更新批次统计
        self.update_batch_stats(invoice.batch_id)?;

        Ok(invoice_id)
    }

    /// 获取批次的所有发票
    pub fn list_invoices_by_batch(&self, batch_id: i64) -> StoreResult<Vec<ReportedInvoice>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, batch_id, invoice_number, issue_date, amount, tax_amount,
                    buyer_name, seller_name, ticket_type, city, departure_time, checkin_date,
                    file_path, created_at, updated_at, verification_result, is_duplicate, duplicate_reason
             FROM reported_invoices WHERE batch_id = ?1 ORDER BY issue_date"
        )?;

        let invoices = stmt
            .query_map(params![batch_id], Self::parse_invoice_row)?
            .collect::<Result<Vec<_>, _>>()?;

        Ok(invoices)
    }

    /// 获取批次中未被人工排除、可进入提交和输出的发票。
    pub fn list_reimbursable_invoices_by_batch(
        &self,
        batch_id: i64,
    ) -> StoreResult<Vec<ReportedInvoice>> {
        let mut statement = self.conn.prepare(
            "SELECT id, batch_id, invoice_number, issue_date, amount, tax_amount,
                    buyer_name, seller_name, ticket_type, city, departure_time, checkin_date,
                    file_path, created_at, updated_at, verification_result, is_duplicate, duplicate_reason
             FROM reported_invoices i
             WHERE i.batch_id = ?1
               AND i.is_duplicate = 0
               AND NOT EXISTS (
                   SELECT 1 FROM excluded_invoices e WHERE e.invoice_id = i.id
               )
             ORDER BY issue_date, id",
        )?;
        let invoices = statement
            .query_map(params![batch_id], Self::parse_invoice_row)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(invoices)
    }

    /// 返回批次中由用户明确排除的发票 ID。
    pub fn list_excluded_invoice_ids(&self, batch_id: i64) -> StoreResult<Vec<i64>> {
        let mut statement = self.conn.prepare(
            "SELECT e.invoice_id
             FROM excluded_invoices e
             JOIN reported_invoices i ON i.id = e.invoice_id
             WHERE i.batch_id = ?1
             ORDER BY e.invoice_id",
        )?;
        let ids = statement
            .query_map(params![batch_id], |row| row.get(0))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(ids)
    }

    pub fn is_invoice_excluded(&self, invoice_id: i64) -> StoreResult<bool> {
        self.conn
            .query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM excluded_invoices WHERE invoice_id = ?1
                 )",
                params![invoice_id],
                |row| row.get::<_, i64>(0),
            )
            .map(|value| value != 0)
            .map_err(Into::into)
    }

    /// 按发票号码查找已报销记录（全库范围，跨批次查重）
    ///
    /// 返回 `None` 表示该发票尚未被任何批次使用。
    /// 注意：`query_row` 无行时返回 `QueryReturnedNoRows`（会落进 `StoreError::Database`），
    /// 因此必须用 `optional()` 把"查不到"与"查询出错"区分开。
    pub fn find_invoice_by_number(
        &self,
        invoice_number: &str,
    ) -> StoreResult<Option<ReportedInvoice>> {
        let result = self.conn.query_row(
            "SELECT id, batch_id, invoice_number, issue_date, amount, tax_amount,
                    buyer_name, seller_name, ticket_type, city, departure_time, checkin_date,
                    file_path, created_at, updated_at, verification_result, is_duplicate, duplicate_reason
             FROM reported_invoices WHERE invoice_number = ?1",
            params![invoice_number],
            Self::parse_invoice_row,
        ).optional()?;

        Ok(result)
    }

    /// 按主键获取发票记录。返回 `None` 表示记录不存在。
    ///
    /// 与 `find_invoice_by_number` 同理，用 `optional()` 区分"查不到"与"查询出错"。
    pub fn get_invoice(&self, id: i64) -> StoreResult<Option<ReportedInvoice>> {
        let result = self.conn.query_row(
            "SELECT id, batch_id, invoice_number, issue_date, amount, tax_amount,
                    buyer_name, seller_name, ticket_type, city, departure_time, checkin_date,
                    file_path, created_at, updated_at, verification_result, is_duplicate, duplicate_reason
             FROM reported_invoices WHERE id = ?1",
            params![id],
            Self::parse_invoice_row,
        ).optional()?;

        Ok(result)
    }

    /// 删除发票
    pub fn delete_invoice(&self, id: i64) -> StoreResult<()> {
        // 先获取 batch_id
        let batch_id: i64 = self.conn.query_row(
            "SELECT batch_id FROM reported_invoices WHERE id = ?1",
            params![id],
            |row| row.get(0),
        )?;

        let rows = self
            .conn
            .execute("DELETE FROM reported_invoices WHERE id = ?1", params![id])?;

        if rows == 0 {
            return Err(StoreError::NotFound(format!("Invoice {}", id)));
        }

        // 更新批次统计
        self.update_batch_stats(batch_id)?;
        Ok(())
    }

    /// 清除发票的重复标记（用户确认不是重复后调用）
    pub fn clear_duplicate_flag(&self, id: i64) -> StoreResult<()> {
        let rows = self.conn.execute(
            "UPDATE reported_invoices SET is_duplicate = 0, duplicate_reason = NULL WHERE id = ?1",
            params![id],
        )?;

        if rows == 0 {
            return Err(StoreError::NotFound(format!("Invoice {}", id)));
        }

        Ok(())
    }

    /// 按多字段组合查找疑似重复（不含自身 id）
    ///
    /// 匹配规则：发票号完全一致 **或** (金额+日期+票种) 三项一致
    ///
    /// # 参数
    /// - `invoice_number`: 发票号
    /// - `amount`: 金额
    /// - `issue_date`: 开票日期
    /// - `ticket_type`: 票据类型（字符串形式，如 "rail"）
    /// - `exclude_id`: 排除指定 ID（用于编辑场景，避免匹配自身）
    ///
    /// # 返回
    /// 匹配的发票列表（可能为空）
    pub fn find_potential_duplicates(
        &self,
        invoice_number: &str,
        amount: &Decimal,
        issue_date: &NaiveDate,
        ticket_type: &str,
        exclude_id: Option<i64>,
    ) -> StoreResult<Vec<ReportedInvoice>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, batch_id, invoice_number, issue_date, amount, tax_amount,
                    buyer_name, seller_name, ticket_type, city, departure_time, checkin_date,
                    file_path, created_at, updated_at, verification_result, is_duplicate, duplicate_reason
             FROM reported_invoices
             WHERE (invoice_number = ?1
                    OR (amount = ?2 AND issue_date = ?3 AND ticket_type = ?4))
               AND (?5 IS NULL OR id != ?5)"
        )?;

        let invoices = stmt
            .query_map(
                params![
                    invoice_number,
                    amount.to_string(),
                    issue_date.format("%Y-%m-%d").to_string(),
                    ticket_type,
                    exclude_id,
                ],
                Self::parse_invoice_row,
            )?
            .collect::<Result<Vec<_>, _>>()?;

        Ok(invoices)
    }

    /// 原子替换一个批次的归组快照。成员必须引用该批次中已经入库的发票。
    pub fn replace_batch_grouping(&self, grouping: &NewBatchGrouping) -> StoreResult<()> {
        let transaction = self.conn.unchecked_transaction()?;
        Self::replace_batch_grouping_for_connection(&transaction, grouping)?;
        transaction.commit()?;
        Ok(())
    }

    /// 人工触发重新分析归组；新旧快照进入审核历史并可顺序撤销。
    pub fn replace_batch_grouping_with_audit(
        &self,
        grouping: &NewBatchGrouping,
    ) -> StoreResult<()> {
        self.apply_review_mutation(
            grouping.batch_id,
            "grouping_recomputed",
            "重新解析行程票并计算归组",
            |transaction| Self::replace_batch_grouping_for_connection(transaction, grouping),
        )
    }

    fn replace_batch_grouping_for_connection(
        connection: &Connection,
        grouping: &NewBatchGrouping,
    ) -> StoreResult<()> {
        connection.execute(
            "DELETE FROM batch_grouping WHERE batch_id = ?1",
            params![grouping.batch_id],
        )?;
        connection.execute(
            "INSERT INTO batch_grouping (
                batch_id, rule_version, home_cities_json, overall_confidence,
                ambiguities_json, created_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, datetime('now'))",
            params![
                grouping.batch_id,
                grouping.rule_version,
                grouping.home_cities_json,
                grouping.overall_confidence,
                grouping.ambiguities_json,
            ],
        )?;

        let mut assigned_invoice_ids = HashSet::new();
        for group in &grouping.groups {
            connection.execute(
                "INSERT INTO invoice_groups (
                    batch_id, group_index, kind, title, start_date, end_date,
                    confidence, requires_review, evidence_json
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    grouping.batch_id,
                    group.group_index as i64,
                    group.kind,
                    group.title,
                    group.start_date,
                    group.end_date,
                    group.confidence,
                    if group.requires_review { 1 } else { 0 },
                    group.evidence_json,
                ],
            )?;
            let group_id = connection.last_insert_rowid();
            for member in &group.members {
                if !assigned_invoice_ids.insert(member.invoice_id) {
                    return Err(StoreError::Validation(format!(
                        "invoice {} cannot belong to multiple groups",
                        member.invoice_id
                    )));
                }
                let member_batch: i64 = connection.query_row(
                    "SELECT batch_id FROM reported_invoices WHERE id = ?1",
                    params![member.invoice_id],
                    |row| row.get(0),
                )?;
                if member_batch != grouping.batch_id {
                    return Err(StoreError::Validation(format!(
                        "invoice {} does not belong to batch {}",
                        member.invoice_id, grouping.batch_id
                    )));
                }
                connection.execute(
                    "INSERT INTO invoice_group_members (
                        group_id, invoice_id, input_index, match_reason
                     ) VALUES (?1, ?2, ?3, ?4)",
                    params![
                        group_id,
                        member.invoice_id,
                        member.input_index as i64,
                        member.match_reason,
                    ],
                )?;
            }
        }
        connection.execute(
            "UPDATE expense_items
             SET trip_group_id = (
                 SELECT member.group_id
                 FROM invoice_group_members member
                 WHERE member.invoice_id = expense_items.primary_invoice_id
                 ORDER BY member.group_id
                 LIMIT 1
             ), updated_at = ?2
             WHERE batch_id = ?1",
            params![grouping.batch_id, Self::now_text()],
        )?;
        Ok(())
    }

    /// 获取批次归组快照；手工创建的旧批次可以没有快照。
    pub fn get_batch_grouping(&self, batch_id: i64) -> StoreResult<Option<BatchGrouping>> {
        let header = self
            .conn
            .query_row(
                "SELECT rule_version, home_cities_json, overall_confidence,
                        ambiguities_json, created_at
                 FROM batch_grouping WHERE batch_id = ?1",
                params![batch_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, f32>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                    ))
                },
            )
            .optional()?;
        let Some((
            rule_version,
            home_cities_json,
            overall_confidence,
            ambiguities_json,
            created_at,
        )) = header
        else {
            return Ok(None);
        };

        let mut group_stmt = self.conn.prepare(
            "SELECT id, group_index, kind, title, start_date, end_date, confidence,
                    requires_review, evidence_json
             FROM invoice_groups WHERE batch_id = ?1 ORDER BY group_index",
        )?;
        let group_rows = group_stmt
            .query_map(params![batch_id], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)? as usize,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, f32>(6)?,
                    row.get::<_, i64>(7)? != 0,
                    row.get::<_, String>(8)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;

        let mut groups = Vec::with_capacity(group_rows.len());
        for row in group_rows {
            let mut member_stmt = self.conn.prepare(
                "SELECT m.invoice_id, i.invoice_number, m.input_index, m.match_reason
                 FROM invoice_group_members m
                 JOIN reported_invoices i ON i.id = m.invoice_id
                 WHERE m.group_id = ?1 ORDER BY m.input_index",
            )?;
            let members = member_stmt
                .query_map(params![row.0], |member| {
                    Ok(InvoiceGroupMember {
                        invoice_id: member.get(0)?,
                        invoice_number: member.get(1)?,
                        input_index: member.get::<_, i64>(2)? as usize,
                        match_reason: member.get(3)?,
                    })
                })?
                .collect::<Result<Vec<_>, _>>()?;
            groups.push(InvoiceGroup {
                id: row.0,
                group_index: row.1,
                kind: row.2,
                title: row.3,
                start_date: row.4,
                end_date: row.5,
                confidence: row.6,
                requires_review: row.7,
                evidence_json: row.8,
                members,
            });
        }

        Ok(Some(BatchGrouping {
            batch_id,
            rule_version,
            home_cities_json,
            overall_confidence,
            ambiguities_json,
            created_at,
            groups,
        }))
    }

    /// 获取应用设置
    pub fn get_setting(&self, key: &str) -> StoreResult<Option<String>> {
        let result = self
            .conn
            .query_row(
                "SELECT value FROM settings WHERE key = ?1",
                params![key],
                |row| row.get(0),
            )
            .optional()?;
        Ok(result)
    }

    /// 设置应用配置（INSERT OR REPLACE）
    pub fn set_setting(&self, key: &str, value: &str) -> StoreResult<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO settings (key, value, updated_at)
             VALUES (?1, ?2, datetime('now'))",
            params![key, value],
        )?;
        Ok(())
    }

    /// 获取所有设置
    pub fn get_all_settings(&self) -> StoreResult<std::collections::HashMap<String, String>> {
        let mut stmt = self.conn.prepare("SELECT key, value FROM settings")?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;

        let mut map = std::collections::HashMap::new();
        for row in rows {
            let (k, v) = row?;
            map.insert(k, v);
        }
        Ok(map)
    }

    /// 更新批次统计信息（总金额和发票数量）
    fn update_batch_stats(&self, batch_id: i64) -> StoreResult<()> {
        Self::update_batch_stats_for_connection(&self.conn, batch_id)
    }

    /// 解析批次行
    fn parse_batch_row(row: &Row) -> Result<Batch, rusqlite::Error> {
        let status_i32: i32 = row.get(3)?;
        let status = BatchStatus::from_i32(status_i32).ok_or_else(|| {
            rusqlite::Error::FromSqlConversionFailure(
                3,
                rusqlite::types::Type::Integer,
                Box::new(StoreError::Internal(format!(
                    "Invalid batch status: {}",
                    status_i32
                ))),
            )
        })?;

        let total_amount_str: String = row.get(4)?;
        let total_amount = Decimal::from_str(&total_amount_str).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(4, rusqlite::types::Type::Text, Box::new(e))
        })?;

        Ok(Batch {
            id: row.get(0)?,
            name: row.get(1)?,
            month: row.get(2)?,
            status,
            total_amount,
            invoice_count: row.get(5)?,
            created_at: NaiveDateTime::parse_from_str(
                &row.get::<_, String>(6)?,
                "%Y-%m-%d %H:%M:%S",
            )
            .map_err(|e| {
                rusqlite::Error::FromSqlConversionFailure(
                    6,
                    rusqlite::types::Type::Text,
                    Box::new(e),
                )
            })?,
            updated_at: NaiveDateTime::parse_from_str(
                &row.get::<_, String>(7)?,
                "%Y-%m-%d %H:%M:%S",
            )
            .map_err(|e| {
                rusqlite::Error::FromSqlConversionFailure(
                    7,
                    rusqlite::types::Type::Text,
                    Box::new(e),
                )
            })?,
            submitted_at: row
                .get::<_, Option<String>>(8)?
                .and_then(|s| NaiveDateTime::parse_from_str(&s, "%Y-%m-%d %H:%M:%S").ok()),
            approved_at: row
                .get::<_, Option<String>>(9)?
                .and_then(|s| NaiveDateTime::parse_from_str(&s, "%Y-%m-%d %H:%M:%S").ok()),
            completed_at: row
                .get::<_, Option<String>>(10)?
                .and_then(|s| NaiveDateTime::parse_from_str(&s, "%Y-%m-%d %H:%M:%S").ok()),
            rejected_at: row
                .get::<_, Option<String>>(11)?
                .and_then(|s| NaiveDateTime::parse_from_str(&s, "%Y-%m-%d %H:%M:%S").ok()),
        })
    }

    /// 解析发票行
    fn parse_invoice_row(row: &Row) -> Result<ReportedInvoice, rusqlite::Error> {
        let amount_str: String = row.get(4)?;
        let amount = Decimal::from_str(&amount_str).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(4, rusqlite::types::Type::Text, Box::new(e))
        })?;

        let tax_amount = row
            .get::<_, Option<String>>(5)?
            .and_then(|s| Decimal::from_str(&s).ok());

        let ticket_type_str: String = row.get(8)?;
        let ticket_type = TicketType::from_db_str(&ticket_type_str).ok_or_else(|| {
            rusqlite::Error::FromSqlConversionFailure(
                8,
                rusqlite::types::Type::Text,
                Box::new(StoreError::Internal(format!(
                    "Invalid ticket type: {}",
                    ticket_type_str
                ))),
            )
        })?;

        let is_duplicate_int: i32 = row.get(16)?;

        Ok(ReportedInvoice {
            id: row.get(0)?,
            batch_id: row.get(1)?,
            invoice_number: row.get(2)?,
            issue_date: NaiveDate::parse_from_str(&row.get::<_, String>(3)?, "%Y-%m-%d").map_err(
                |e| {
                    rusqlite::Error::FromSqlConversionFailure(
                        3,
                        rusqlite::types::Type::Text,
                        Box::new(e),
                    )
                },
            )?,
            amount,
            tax_amount,
            buyer_name: row.get(6)?,
            seller_name: row.get(7)?,
            ticket_type,
            city: row.get(9)?,
            departure_time: row
                .get::<_, Option<String>>(10)?
                .and_then(|s| NaiveDateTime::parse_from_str(&s, "%Y-%m-%d %H:%M:%S").ok()),
            checkin_date: row
                .get::<_, Option<String>>(11)?
                .and_then(|s| NaiveDate::parse_from_str(&s, "%Y-%m-%d").ok()),
            file_path: row.get(12)?,
            created_at: NaiveDateTime::parse_from_str(
                &row.get::<_, String>(13)?,
                "%Y-%m-%d %H:%M:%S",
            )
            .map_err(|e| {
                rusqlite::Error::FromSqlConversionFailure(
                    13,
                    rusqlite::types::Type::Text,
                    Box::new(e),
                )
            })?,
            updated_at: NaiveDateTime::parse_from_str(
                &row.get::<_, String>(14)?,
                "%Y-%m-%d %H:%M:%S",
            )
            .map_err(|e| {
                rusqlite::Error::FromSqlConversionFailure(
                    14,
                    rusqlite::types::Type::Text,
                    Box::new(e),
                )
            })?,
            verification_result: row.get(15)?,
            is_duplicate: is_duplicate_int != 0,
            duplicate_reason: row.get(17)?,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ReviewSnapshot {
    invoices: Vec<ReportedInvoice>,
    #[serde(default)]
    excluded_invoice_ids: Vec<i64>,
    #[serde(default)]
    expense_items: Vec<ExpenseItem>,
    #[serde(default)]
    pending_documents: Vec<PendingInvoiceDocument>,
    grouping: Option<BatchGrouping>,
}

impl LedgerDb {
    pub fn update_invoice_review_fields(
        &self,
        invoice_id: i64,
        update: &InvoiceReviewUpdate,
    ) -> StoreResult<()> {
        Self::validate_invoice_review_update(update)?;
        let batch_id: i64 = self
            .conn
            .query_row(
                "SELECT batch_id FROM reported_invoices WHERE id = ?1",
                params![invoice_id],
                |row| row.get(0),
            )
            .optional()?
            .ok_or_else(|| StoreError::NotFound(format!("Invoice {invoice_id}")))?;
        self.apply_review_mutation(
            batch_id,
            "invoice_fields_updated",
            "修改发票字段",
            |transaction| {
                let changed = transaction.execute(
                    "UPDATE reported_invoices SET
                        invoice_number = ?2, issue_date = ?3, amount = ?4, tax_amount = ?5,
                        buyer_name = ?6, seller_name = ?7, ticket_type = ?8, city = ?9,
                        departure_time = ?10, checkin_date = ?11, updated_at = ?12
                     WHERE id = ?1 AND batch_id = ?13",
                    params![
                        invoice_id,
                        update.invoice_number.trim(),
                        update.issue_date.format("%Y-%m-%d").to_string(),
                        update.amount.to_string(),
                        update.tax_amount.as_ref().map(ToString::to_string),
                        Self::optional_trimmed(update.buyer_name.as_deref()),
                        Self::optional_trimmed(update.seller_name.as_deref()),
                        update.ticket_type.to_str(),
                        Self::optional_trimmed(update.city.as_deref()),
                        update
                            .departure_time
                            .as_ref()
                            .map(|value| value.format("%Y-%m-%d %H:%M:%S").to_string()),
                        update
                            .checkin_date
                            .as_ref()
                            .map(|value| value.format("%Y-%m-%d").to_string()),
                        Self::now_text(),
                        batch_id,
                    ],
                )?;
                if changed != 1 {
                    return Err(StoreError::NotFound(format!("Invoice {invoice_id}")));
                }
                Ok(())
            },
        )
    }

    pub fn resolve_duplicate_with_audit(&self, invoice_id: i64) -> StoreResult<()> {
        let batch_id: i64 = self
            .conn
            .query_row(
                "SELECT batch_id FROM reported_invoices WHERE id = ?1",
                params![invoice_id],
                |row| row.get(0),
            )
            .optional()?
            .ok_or_else(|| StoreError::NotFound(format!("Invoice {invoice_id}")))?;
        self.apply_review_mutation(
            batch_id,
            "duplicate_resolved",
            "人工确认非重复",
            |transaction| {
                let changed = transaction.execute(
                    "UPDATE reported_invoices
                     SET is_duplicate = 0, duplicate_reason = NULL, updated_at = ?2
                     WHERE id = ?1 AND batch_id = ?3 AND is_duplicate != 0",
                    params![invoice_id, Self::now_text(), batch_id],
                )?;
                if changed != 1 {
                    return Err(StoreError::Validation(
                        "invoice is not marked duplicate".to_string(),
                    ));
                }
                // A previous explicit "confirmed duplicate" decision is superseded by this
                // decision. Preserve unrelated manual exclusions.
                transaction.execute(
                    "DELETE FROM excluded_invoices
                     WHERE invoice_id = ?1 AND reason = 'confirmed_duplicate'",
                    params![invoice_id],
                )?;
                transaction.execute(
                    "UPDATE expense_items
                     SET inclusion_status = CASE
                         WHEN EXISTS(
                             SELECT 1 FROM excluded_invoices e
                             WHERE e.invoice_id = ?1
                         ) THEN 'excluded' ELSE 'included' END,
                         updated_at = ?2
                     WHERE primary_invoice_id = ?1",
                    params![invoice_id, Self::now_text()],
                )?;
                Ok(())
            },
        )
    }

    pub fn confirm_duplicate_with_audit(&self, invoice_id: i64) -> StoreResult<()> {
        let (batch_id, is_duplicate): (i64, bool) = self
            .conn
            .query_row(
                "SELECT batch_id, is_duplicate != 0 FROM reported_invoices WHERE id = ?1",
                params![invoice_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?
            .ok_or_else(|| StoreError::NotFound(format!("Invoice {invoice_id}")))?;
        if !is_duplicate {
            return Err(StoreError::Validation(
                "only a duplicate candidate can be confirmed".to_string(),
            ));
        }
        self.apply_review_mutation(
            batch_id,
            "duplicate_confirmed",
            "人工确认重复，保持不计入总额",
            |transaction| {
                let already_confirmed: bool = transaction.query_row(
                    "SELECT EXISTS(
                        SELECT 1 FROM excluded_invoices
                        WHERE invoice_id = ?1 AND reason = 'confirmed_duplicate'
                     )",
                    params![invoice_id],
                    |row| row.get(0),
                )?;
                if already_confirmed {
                    return Err(StoreError::Validation(
                        "duplicate is already confirmed".to_string(),
                    ));
                }
                transaction.execute(
                    "INSERT INTO excluded_invoices (invoice_id, reason, excluded_at)
                     VALUES (?1, 'confirmed_duplicate', ?2)
                     ON CONFLICT(invoice_id) DO UPDATE SET
                         reason = 'confirmed_duplicate', excluded_at = excluded.excluded_at",
                    params![invoice_id, Self::now_text()],
                )?;
                transaction.execute(
                    "UPDATE expense_items SET inclusion_status = 'excluded', updated_at = ?2
                     WHERE primary_invoice_id = ?1",
                    params![invoice_id, Self::now_text()],
                )?;
                Ok(())
            },
        )
    }

    /// 人工排除/恢复发票。操作不删除原件，且纳入审核快照和顺序撤销。
    pub fn set_invoice_excluded_with_audit(
        &self,
        invoice_id: i64,
        excluded: bool,
    ) -> StoreResult<()> {
        let batch_id: i64 = self
            .conn
            .query_row(
                "SELECT batch_id FROM reported_invoices WHERE id = ?1",
                params![invoice_id],
                |row| row.get(0),
            )
            .optional()?
            .ok_or_else(|| StoreError::NotFound(format!("Invoice {invoice_id}")))?;
        let (action_type, summary) = if excluded {
            ("invoice_excluded", "排除发票，不进入报销输出")
        } else {
            ("invoice_restored", "恢复发票，重新进入报销输出")
        };
        self.apply_review_mutation(batch_id, action_type, summary, |transaction| {
            let changed = if excluded {
                transaction.execute(
                    "INSERT OR IGNORE INTO excluded_invoices (
                            invoice_id, reason, excluded_at
                         ) VALUES (?1, 'manual_review', ?2)",
                    params![invoice_id, Self::now_text()],
                )?
            } else {
                transaction.execute(
                    "DELETE FROM excluded_invoices WHERE invoice_id = ?1",
                    params![invoice_id],
                )?
            };
            if changed != 1 {
                let message = if excluded {
                    "invoice is already excluded"
                } else {
                    "invoice is not excluded"
                };
                return Err(StoreError::Validation(message.to_string()));
            }
            transaction.execute(
                "UPDATE expense_items
                 SET inclusion_status = CASE
                     WHEN ?2 != 0 THEN 'excluded'
                     WHEN EXISTS(
                         SELECT 1 FROM reported_invoices i
                         WHERE i.id = ?1 AND i.is_duplicate != 0
                     ) THEN 'duplicate_suspect'
                     ELSE 'included' END,
                     updated_at = ?3
                 WHERE primary_invoice_id = ?1",
                params![invoice_id, if excluded { 1 } else { 0 }, Self::now_text()],
            )?;
            transaction.execute(
                "UPDATE invoice_groups
                 SET requires_review = 1
                 WHERE id IN (
                     SELECT group_id FROM invoice_group_members WHERE invoice_id = ?1
                 )",
                params![invoice_id],
            )?;
            Ok(())
        })
    }

    pub fn create_manual_invoice_group(
        &self,
        batch_id: i64,
        kind: &str,
        title: &str,
        start_date: &str,
        end_date: &str,
    ) -> StoreResult<i64> {
        if !matches!(kind, "business_trip" | "local_month") {
            return Err(StoreError::Validation(
                "manual group kind must be business_trip or local_month".to_string(),
            ));
        }
        let title = title.trim();
        let start = NaiveDate::parse_from_str(start_date, "%Y-%m-%d")
            .map_err(|_| StoreError::Validation("invalid manual group start date".to_string()))?;
        let end = NaiveDate::parse_from_str(end_date, "%Y-%m-%d")
            .map_err(|_| StoreError::Validation("invalid manual group end date".to_string()))?;
        if title.is_empty() || title.chars().count() > 100 || end < start {
            return Err(StoreError::Validation(
                "invalid manual group title or date range".to_string(),
            ));
        }
        self.apply_review_mutation(
            batch_id,
            "group_created",
            "新建人工归组",
            |transaction| {
                transaction.execute(
                    "INSERT OR IGNORE INTO batch_grouping (
                        batch_id, rule_version, home_cities_json, overall_confidence,
                        ambiguities_json, created_at
                     ) VALUES (?1, 'manual-v1', '[]', 1.0, '[]', ?2)",
                    params![batch_id, Self::now_text()],
                )?;
                let group_index: i64 = transaction.query_row(
                    "SELECT COALESCE(MAX(group_index), -1) + 1
                     FROM invoice_groups WHERE batch_id = ?1",
                    params![batch_id],
                    |row| row.get(0),
                )?;
                transaction.execute(
                    "INSERT INTO invoice_groups (
                        batch_id, group_index, kind, title, start_date, end_date,
                        confidence, requires_review, evidence_json
                     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 1.0,
                               CASE WHEN ?3 = 'business_trip' THEN 1 ELSE 0 END, ?7)",
                    params![
                        batch_id,
                        group_index,
                        kind,
                        title,
                        start_date,
                        end_date,
                        r#"{"source":"manual_review"}"#,
                    ],
                )?;
                Ok(transaction.last_insert_rowid())
            },
        )
    }

    pub fn move_invoice_to_group(
        &self,
        batch_id: i64,
        invoice_id: i64,
        target_group_id: i64,
    ) -> StoreResult<()> {
        self.apply_review_mutation(
            batch_id,
            "invoice_group_moved",
            "调整发票归组",
            |transaction| {
                let invoice_batch: i64 = transaction
                    .query_row(
                        "SELECT batch_id FROM reported_invoices WHERE id = ?1",
                        params![invoice_id],
                        |row| row.get(0),
                    )
                    .optional()?
                    .ok_or_else(|| StoreError::NotFound(format!("Invoice {invoice_id}")))?;
                let group_batch: i64 = transaction
                    .query_row(
                        "SELECT batch_id FROM invoice_groups WHERE id = ?1",
                        params![target_group_id],
                        |row| row.get(0),
                    )
                    .optional()?
                    .ok_or_else(|| StoreError::NotFound(format!("Group {target_group_id}")))?;
                if invoice_batch != batch_id || group_batch != batch_id {
                    return Err(StoreError::Validation(
                        "invoice and group must belong to the same batch".to_string(),
                    ));
                }
                let source_group_id: Option<i64> = transaction
                    .query_row(
                        "SELECT group_id FROM invoice_group_members
                         WHERE invoice_id = ?1 ORDER BY group_id LIMIT 1",
                        params![invoice_id],
                        |row| row.get(0),
                    )
                    .optional()?;
                let stored_input_index: Option<i64> = transaction
                    .query_row(
                        "SELECT input_index FROM invoice_group_members
                         WHERE invoice_id = ?1 ORDER BY group_id LIMIT 1",
                        params![invoice_id],
                        |row| row.get(0),
                    )
                    .optional()?;
                let input_index = if let Some(input_index) = stored_input_index {
                    input_index
                } else {
                    transaction.query_row(
                        "SELECT COUNT(*) FROM reported_invoices
                         WHERE batch_id = ?1 AND id < ?2",
                        params![batch_id, invoice_id],
                        |row| row.get(0),
                    )?
                };
                transaction.execute(
                    "DELETE FROM invoice_group_members
                     WHERE invoice_id = ?1
                       AND group_id IN (SELECT id FROM invoice_groups WHERE batch_id = ?2)",
                    params![invoice_id, batch_id],
                )?;
                transaction.execute(
                    "INSERT INTO invoice_group_members (
                        group_id, invoice_id, input_index, match_reason
                     ) VALUES (?1, ?2, ?3, '人工审核：用户调整归组')",
                    params![target_group_id, invoice_id, input_index],
                )?;
                transaction.execute(
                    "UPDATE expense_items SET trip_group_id = ?2, updated_at = ?3
                     WHERE primary_invoice_id = ?1",
                    params![invoice_id, target_group_id, Self::now_text()],
                )?;
                if let Some(source_group_id) = source_group_id.filter(|id| *id != target_group_id) {
                    transaction.execute(
                        "DELETE FROM invoice_groups
                         WHERE id = ?1 AND batch_id = ?2
                           AND NOT EXISTS (
                               SELECT 1 FROM invoice_group_members WHERE group_id = ?1
                           )",
                        params![source_group_id, batch_id],
                    )?;
                }
                Ok(())
            },
        )
    }

    pub fn merge_invoice_groups(
        &self,
        batch_id: i64,
        source_group_id: i64,
        target_group_id: i64,
    ) -> StoreResult<()> {
        if source_group_id == target_group_id {
            return Err(StoreError::Validation(
                "source and target groups must differ".to_string(),
            ));
        }
        self.apply_review_mutation(
            batch_id,
            "groups_merged",
            "合并发票归组",
            |transaction| {
                let count: i64 = transaction.query_row(
                    "SELECT COUNT(*) FROM invoice_groups
                     WHERE batch_id = ?1 AND id IN (?2, ?3)",
                    params![batch_id, source_group_id, target_group_id],
                    |row| row.get(0),
                )?;
                if count != 2 {
                    return Err(StoreError::Validation(
                        "both groups must belong to the batch".to_string(),
                    ));
                }
                transaction.execute(
                    "DELETE FROM invoice_group_members AS source
                     WHERE source.group_id = ?2
                       AND EXISTS (
                           SELECT 1 FROM invoice_group_members target
                           WHERE target.group_id = ?1
                             AND target.invoice_id = source.invoice_id
                       )",
                    params![target_group_id, source_group_id],
                )?;
                transaction.execute(
                    "UPDATE invoice_group_members
                     SET group_id = ?1, match_reason = '人工审核：合并归组'
                     WHERE group_id = ?2",
                    params![target_group_id, source_group_id],
                )?;
                transaction.execute(
                    "UPDATE invoice_groups SET
                        start_date = MIN(start_date, (SELECT start_date FROM invoice_groups WHERE id = ?2)),
                        end_date = MAX(end_date, (SELECT end_date FROM invoice_groups WHERE id = ?2)),
                        requires_review = 1,
                        evidence_json = ?3
                     WHERE id = ?1 AND batch_id = ?4",
                    params![
                        target_group_id,
                        source_group_id,
                        r#"{"source":"manual_merge"}"#,
                        batch_id,
                    ],
                )?;
                transaction.execute(
                    "DELETE FROM invoice_groups WHERE id = ?1 AND batch_id = ?2",
                    params![source_group_id, batch_id],
                )?;
                transaction.execute(
                    "UPDATE expense_items SET trip_group_id = ?2, updated_at = ?3
                     WHERE batch_id = ?1 AND trip_group_id = ?4",
                    params![batch_id, target_group_id, Self::now_text(), source_group_id],
                )?;
                Ok(())
            },
        )
    }

    pub fn set_invoice_group_transport_evidence(
        &self,
        batch_id: i64,
        group_id: i64,
        status: &str,
    ) -> StoreResult<()> {
        if !matches!(status, "missing" | "company_paid" | "not_required") {
            return Err(StoreError::Validation(
                "invalid transport evidence status".to_string(),
            ));
        }
        self.apply_review_mutation(
            batch_id,
            "group_transport_evidence_updated",
            "更新出差交通凭证情况",
            |transaction| {
                let (kind, evidence_raw) = transaction
                    .query_row(
                        "SELECT kind, evidence_json FROM invoice_groups
                         WHERE id = ?1 AND batch_id = ?2",
                        params![group_id, batch_id],
                        |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
                    )
                    .optional()?
                    .ok_or_else(|| StoreError::NotFound(format!("InvoiceGroup {group_id}")))?;
                if kind != "business_trip" {
                    return Err(StoreError::Validation(
                        "transport evidence only applies to business trip groups".to_string(),
                    ));
                }
                let mut evidence = serde_json::from_str::<serde_json::Value>(&evidence_raw)
                    .unwrap_or_else(|_| serde_json::json!({}));
                let object = evidence.as_object_mut().ok_or_else(|| {
                    StoreError::Validation("group evidence is invalid".to_string())
                })?;
                object.insert(
                    "transportEvidenceStatus".to_string(),
                    serde_json::Value::String(status.to_string()),
                );
                let updated = serde_json::to_string(&evidence).map_err(|error| {
                    StoreError::Internal(format!("serialize group evidence: {error}"))
                })?;
                transaction.execute(
                    "UPDATE invoice_groups
                     SET evidence_json = ?3, requires_review = 1
                     WHERE id = ?1 AND batch_id = ?2",
                    params![group_id, batch_id, updated],
                )?;
                if matches!(status, "company_paid" | "not_required") {
                    let member_indexes = transaction
                        .prepare(
                            "SELECT input_index FROM invoice_group_members WHERE group_id = ?1",
                        )?
                        .query_map(params![group_id], |row| row.get::<_, i64>(0))?
                        .collect::<Result<HashSet<_>, _>>()?;
                    let ambiguity_raw: String = transaction.query_row(
                        "SELECT ambiguities_json FROM batch_grouping WHERE batch_id = ?1",
                        params![batch_id],
                        |row| row.get(0),
                    )?;
                    let mut ambiguity_value: serde_json::Value =
                        serde_json::from_str(&ambiguity_raw).map_err(|_| {
                            StoreError::Validation("grouping ambiguities are invalid".to_string())
                        })?;
                    let ambiguity_list = ambiguity_value.as_array_mut().ok_or_else(|| {
                        StoreError::Validation("grouping ambiguities are invalid".to_string())
                    })?;
                    ambiguity_list.retain(|ambiguity| {
                        let is_missing_transport =
                            ambiguity.get("kind").and_then(serde_json::Value::as_str)
                                == Some("MissingTransportEvidence");
                        let belongs_to_group = ambiguity
                            .get("involved_invoice_ids")
                            .and_then(serde_json::Value::as_array)
                            .is_some_and(|indexes| {
                                !indexes.is_empty()
                                    && indexes.iter().all(|index| {
                                        index
                                            .as_i64()
                                            .is_some_and(|value| member_indexes.contains(&value))
                                    })
                            });
                        !(is_missing_transport && belongs_to_group)
                    });
                    let updated_ambiguities =
                        serde_json::to_string(&ambiguity_value).map_err(|error| {
                            StoreError::Internal(format!("serialize grouping ambiguities: {error}"))
                        })?;
                    transaction.execute(
                        "UPDATE batch_grouping SET ambiguities_json = ?2 WHERE batch_id = ?1",
                        params![batch_id, updated_ambiguities],
                    )?;
                }
                Ok(())
            },
        )
    }

    pub fn confirm_batch_grouping(&self, batch_id: i64) -> StoreResult<()> {
        self.apply_review_mutation(
            batch_id,
            "grouping_confirmed",
            "确认归组审核完成",
            |transaction| {
                let missing_anchor_count =
                    Self::business_trip_groups_missing_anchor(transaction, batch_id)?;
                if missing_anchor_count > 0 {
                    return Err(StoreError::Validation(
                        "business trip group lacks a transport evidence decision".to_string(),
                    ));
                }
                let changed = transaction.execute(
                    "UPDATE batch_grouping
                     SET ambiguities_json = '[]'
                     WHERE batch_id = ?1 AND (
                        ambiguities_json != '[]' OR EXISTS (
                            SELECT 1 FROM invoice_groups
                            WHERE batch_id = ?1 AND requires_review != 0
                        )
                     )",
                    params![batch_id],
                )?;
                transaction.execute(
                    "UPDATE invoice_groups SET requires_review = 0 WHERE batch_id = ?1",
                    params![batch_id],
                )?;
                if changed == 0 {
                    return Err(StoreError::Validation(
                        "grouping has no pending review".to_string(),
                    ));
                }
                Ok(())
            },
        )
    }

    /// Confirm one group after the user has reviewed its route and expense membership.
    /// Ambiguities that only reference invoices in this group are accepted together with
    /// the current assignment, so the UI can advance group by group without leaving a
    /// second, disconnected confirmation queue behind.
    pub fn confirm_invoice_group(&self, batch_id: i64, group_id: i64) -> StoreResult<()> {
        self.apply_review_mutation(
            batch_id,
            "invoice_group_confirmed",
            "确认单个归组",
            |transaction| {
                let group_state = transaction
                    .query_row(
                        "SELECT kind, requires_review, evidence_json FROM invoice_groups
                         WHERE id = ?1 AND batch_id = ?2",
                        params![group_id, batch_id],
                        |row| {
                            Ok((
                                row.get::<_, String>(0)?,
                                row.get::<_, i64>(1)?,
                                row.get::<_, String>(2)?,
                            ))
                        },
                    )
                    .optional()?
                    .ok_or_else(|| StoreError::NotFound(format!("InvoiceGroup {group_id}")))?;
                if group_state.1 == 0 {
                    return Err(StoreError::Validation(
                        "invoice group has no pending review".to_string(),
                    ));
                }
                if group_state.0 == "business_trip" {
                    let accepted_without_personal_ticket = matches!(
                        Self::group_transport_evidence_status(&group_state.2).as_deref(),
                        Some("company_paid" | "not_required")
                    );
                    if !Self::invoice_group_has_route_anchor(transaction, group_id)?
                        && !accepted_without_personal_ticket
                    {
                        return Err(StoreError::Validation(
                            "business trip group requires a transport evidence decision"
                                .to_string(),
                        ));
                    }
                }

                let member_indexes = transaction
                    .prepare("SELECT input_index FROM invoice_group_members WHERE group_id = ?1")?
                    .query_map(params![group_id], |row| row.get::<_, i64>(0))?
                    .collect::<Result<HashSet<_>, _>>()?;
                let ambiguity_raw: String = transaction.query_row(
                    "SELECT ambiguities_json FROM batch_grouping WHERE batch_id = ?1",
                    params![batch_id],
                    |row| row.get(0),
                )?;
                let mut ambiguity_value: serde_json::Value = serde_json::from_str(&ambiguity_raw)
                    .map_err(|_| {
                    StoreError::Validation("grouping ambiguities are invalid".to_string())
                })?;
                let ambiguity_list = ambiguity_value.as_array_mut().ok_or_else(|| {
                    StoreError::Validation("grouping ambiguities are invalid".to_string())
                })?;
                ambiguity_list.retain(|ambiguity| {
                    !ambiguity
                        .get("involved_invoice_ids")
                        .and_then(serde_json::Value::as_array)
                        .is_some_and(|indexes| {
                            !indexes.is_empty()
                                && indexes.iter().all(|index| {
                                    index
                                        .as_i64()
                                        .is_some_and(|value| member_indexes.contains(&value))
                                })
                        })
                });
                let updated_ambiguities =
                    serde_json::to_string(&ambiguity_value).map_err(|error| {
                        StoreError::Internal(format!("serialize grouping ambiguities: {error}"))
                    })?;
                transaction.execute(
                    "UPDATE batch_grouping SET ambiguities_json = ?2 WHERE batch_id = ?1",
                    params![batch_id, updated_ambiguities],
                )?;
                transaction.execute(
                    "UPDATE invoice_groups SET requires_review = 0
                     WHERE id = ?1 AND batch_id = ?2",
                    params![group_id, batch_id],
                )?;
                Ok(())
            },
        )
    }

    fn business_trip_groups_missing_anchor(
        connection: &Connection,
        batch_id: i64,
    ) -> StoreResult<i64> {
        let groups = connection
            .prepare(
                "SELECT id, evidence_json FROM invoice_groups
                 WHERE batch_id = ?1 AND kind = 'business_trip'",
            )?
            .query_map(params![batch_id], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        let mut missing = 0i64;
        for (group_id, evidence) in groups {
            let accepted_without_personal_ticket = matches!(
                Self::group_transport_evidence_status(&evidence).as_deref(),
                Some("company_paid" | "not_required")
            );
            if !Self::invoice_group_has_route_anchor(connection, group_id)?
                && !accepted_without_personal_ticket
            {
                missing += 1;
            }
        }
        Ok(missing)
    }

    fn group_transport_evidence_status(raw: &str) -> Option<String> {
        serde_json::from_str::<serde_json::Value>(raw)
            .ok()?
            .get("transportEvidenceStatus")?
            .as_str()
            .map(str::to_string)
    }

    fn invoice_group_has_route_anchor(connection: &Connection, group_id: i64) -> StoreResult<bool> {
        let anchor_count: i64 = connection.query_row(
            "SELECT COUNT(*) FROM invoice_group_members member
             JOIN reported_invoices invoice ON invoice.id = member.invoice_id
             JOIN expense_items expense
               ON expense.primary_invoice_id = invoice.id
              AND expense.inclusion_status = 'included'
             WHERE member.group_id = ?1
               AND (
                 (invoice.ticket_type IN ('rail', 'flight')
                  AND member.match_reason NOT LIKE '%不作为路线节点%')
                 OR EXISTS (
                   SELECT 1 FROM invoice_documents document
                   WHERE document.expense_item_id = expense.id
                     AND document.role = 'itinerary'
                 )
               )",
            params![group_id],
            |row| row.get(0),
        )?;
        Ok(anchor_count > 0)
    }

    pub fn list_review_actions(&self, batch_id: i64) -> StoreResult<Vec<ReviewAction>> {
        let mut statement = self.conn.prepare(
            "SELECT id, batch_id, action_type, summary, created_at, undone_at
             FROM review_actions WHERE batch_id = ?1 ORDER BY id DESC LIMIT 100",
        )?;
        let actions = statement
            .query_map(params![batch_id], |row| {
                Ok(ReviewAction {
                    id: row.get(0)?,
                    batch_id: row.get(1)?,
                    action_type: row.get(2)?,
                    summary: row.get(3)?,
                    created_at: row.get(4)?,
                    undone_at: row.get(5)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(actions)
    }

    pub fn undo_last_review_action(&self, batch_id: i64) -> StoreResult<ReviewAction> {
        let transaction = self.conn.unchecked_transaction()?;
        Self::ensure_batch_draft(&transaction, batch_id)?;
        let action = transaction
            .query_row(
                "SELECT id, batch_id, action_type, summary, before_json, after_json,
                        created_at, undone_at
                 FROM review_actions
                 WHERE batch_id = ?1 AND undone_at IS NULL
                 ORDER BY id DESC LIMIT 1",
                params![batch_id],
                |row| {
                    Ok((
                        ReviewAction {
                            id: row.get(0)?,
                            batch_id: row.get(1)?,
                            action_type: row.get(2)?,
                            summary: row.get(3)?,
                            created_at: row.get(6)?,
                            undone_at: row.get(7)?,
                        },
                        row.get::<_, String>(4)?,
                        row.get::<_, String>(5)?,
                    ))
                },
            )
            .optional()?
            .ok_or_else(|| StoreError::Validation("no review action to undo".to_string()))?;
        let current = Self::review_snapshot(&transaction, batch_id)?;
        let current_json = Self::snapshot_json(&current)?;
        if current_json != action.2 {
            return Err(StoreError::Validation(
                "current review state differs from the last action".to_string(),
            ));
        }
        let before: ReviewSnapshot = serde_json::from_str(&action.1)
            .map_err(|error| StoreError::Internal(format!("invalid review snapshot: {error}")))?;
        Self::restore_review_snapshot(&transaction, batch_id, &before)?;
        let now = Self::now_text();
        transaction.execute(
            "UPDATE review_actions SET undone_at = ?2 WHERE id = ?1 AND undone_at IS NULL",
            params![action.0.id, now],
        )?;
        transaction.commit()?;
        Ok(ReviewAction {
            undone_at: Some(now),
            ..action.0
        })
    }

    fn apply_review_mutation<T, F>(
        &self,
        batch_id: i64,
        action_type: &str,
        summary: &str,
        mutate: F,
    ) -> StoreResult<T>
    where
        F: FnOnce(&rusqlite::Transaction<'_>) -> StoreResult<T>,
    {
        let transaction = self.conn.unchecked_transaction()?;
        Self::ensure_batch_draft(&transaction, batch_id)?;
        let before = Self::review_snapshot(&transaction, batch_id)?;
        let result = mutate(&transaction)?;
        Self::update_batch_stats_for_connection(&transaction, batch_id)?;
        let after = Self::review_snapshot(&transaction, batch_id)?;
        let before_json = Self::snapshot_json(&before)?;
        let after_json = Self::snapshot_json(&after)?;
        if before_json == after_json {
            return Err(StoreError::Validation(
                "review action did not change state".to_string(),
            ));
        }
        transaction.execute(
            "INSERT INTO review_actions (
                batch_id, action_type, summary, before_json, after_json, created_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                batch_id,
                action_type,
                summary,
                before_json,
                after_json,
                Self::now_text(),
            ],
        )?;
        transaction.commit()?;
        Ok(result)
    }

    fn validate_invoice_review_update(update: &InvoiceReviewUpdate) -> StoreResult<()> {
        if update.invoice_number.trim().is_empty()
            || update.invoice_number.chars().count() > 64
            || update.amount < Decimal::ZERO
            || update
                .tax_amount
                .is_some_and(|tax| tax < Decimal::ZERO || tax > update.amount)
            || [
                update.buyer_name.as_deref(),
                update.seller_name.as_deref(),
                update.city.as_deref(),
            ]
            .into_iter()
            .flatten()
            .any(|value| value.chars().count() > 200)
        {
            return Err(StoreError::Validation(
                "invalid invoice review fields".to_string(),
            ));
        }
        Ok(())
    }

    fn optional_trimmed(value: Option<&str>) -> Option<String> {
        value
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    }

    fn expense_items_for_connection(
        connection: &Connection,
        batch_id: i64,
    ) -> StoreResult<Vec<ExpenseItem>> {
        let rows = {
            let mut statement = connection.prepare(
                "SELECT id, batch_id, primary_invoice_id, model_version, category_code,
                        transaction_date, transaction_date_source, transaction_date_confirmed,
                        description, counterparty_name, location_json, payment_method,
                        gross_amount, currency_code, tax_details_json, trip_group_id,
                        inclusion_status, provenance_json, category_source,
                        category_confirmed, created_at, updated_at
                 FROM expense_items WHERE batch_id = ?1
                 ORDER BY transaction_date, id",
            )?;
            let collected = statement
                .query_map(params![batch_id], |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, i32>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, String>(5)?,
                        row.get::<_, String>(6)?,
                        row.get::<_, i64>(7)? != 0,
                        row.get::<_, String>(8)?,
                        row.get::<_, String>(9)?,
                        row.get::<_, String>(10)?,
                        row.get::<_, String>(11)?,
                        row.get::<_, String>(12)?,
                        row.get::<_, String>(13)?,
                        row.get::<_, String>(14)?,
                        row.get::<_, Option<i64>>(15)?,
                        row.get::<_, String>(16)?,
                        row.get::<_, String>(17)?,
                        row.get::<_, String>(18)?,
                        row.get::<_, i64>(19)? != 0,
                        row.get::<_, String>(20)?,
                        row.get::<_, String>(21)?,
                    ))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            collected
        };
        let mut result = Vec::with_capacity(rows.len());
        for row in rows {
            let transaction_date = NaiveDate::parse_from_str(&row.5, "%Y-%m-%d")
                .map_err(|error| StoreError::Internal(format!("invalid expense date: {error}")))?;
            let gross_amount = Decimal::from_str(&row.12).map_err(|error| {
                StoreError::Internal(format!("invalid expense gross amount: {error}"))
            })?;
            let location: ExpenseLocation = serde_json::from_str(&row.10).map_err(|error| {
                StoreError::Internal(format!("invalid expense location: {error}"))
            })?;
            let tax_details: Vec<ExpenseTaxDetail> =
                serde_json::from_str(&row.14).map_err(|error| {
                    StoreError::Internal(format!("invalid expense tax details: {error}"))
                })?;
            let documents = Self::documents_for_expense(connection, row.0)?;
            result.push(ExpenseItem {
                id: row.0,
                batch_id: row.1,
                primary_invoice_id: row.2,
                model_version: row.3,
                category_code: row.4,
                category_source: row.18,
                category_confirmed: row.19,
                transaction_date,
                transaction_date_source: row.6,
                transaction_date_confirmed: row.7,
                description: row.8,
                counterparty_name: row.9,
                location,
                payment_method: row.11,
                gross_amount,
                currency_code: row.13,
                tax_details,
                trip_group_id: row.15,
                inclusion_status: row.16,
                provenance_json: row.17,
                documents,
                created_at: row.20,
                updated_at: row.21,
            });
        }
        Ok(result)
    }

    fn documents_for_expense(
        connection: &Connection,
        expense_item_id: i64,
    ) -> StoreResult<Vec<InvoiceDocument>> {
        let mut statement = connection.prepare(
            "SELECT id, batch_id, expense_item_id, source_invoice_id,
                    source_pending_document_id, role, file_path, original_name,
                    mime_type, sha256, created_at
             FROM invoice_documents WHERE expense_item_id = ?1
             ORDER BY CASE role
                 WHEN 'main_invoice' THEN 0 WHEN 'itinerary' THEN 1
                 WHEN 'detail' THEN 2 WHEN 'supporting' THEN 3 ELSE 4 END, id",
        )?;
        let documents = statement
            .query_map(params![expense_item_id], |row| {
                Ok(InvoiceDocument {
                    id: row.get(0)?,
                    batch_id: row.get(1)?,
                    expense_item_id: row.get(2)?,
                    source_invoice_id: row.get(3)?,
                    source_pending_document_id: row.get(4)?,
                    role: row.get(5)?,
                    file_path: row.get(6)?,
                    original_name: row.get(7)?,
                    mime_type: row.get(8)?,
                    sha256: row.get(9)?,
                    created_at: row.get(10)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(documents)
    }

    pub fn get_invoice_document(&self, document_id: i64) -> StoreResult<Option<InvoiceDocument>> {
        self.conn
            .query_row(
                "SELECT id, batch_id, expense_item_id, source_invoice_id,
                        source_pending_document_id, role, file_path, original_name,
                        mime_type, sha256, created_at
                 FROM invoice_documents WHERE id = ?1",
                params![document_id],
                |row| {
                    Ok(InvoiceDocument {
                        id: row.get(0)?,
                        batch_id: row.get(1)?,
                        expense_item_id: row.get(2)?,
                        source_invoice_id: row.get(3)?,
                        source_pending_document_id: row.get(4)?,
                        role: row.get(5)?,
                        file_path: row.get(6)?,
                        original_name: row.get(7)?,
                        mime_type: row.get(8)?,
                        sha256: row.get(9)?,
                        created_at: row.get(10)?,
                    })
                },
            )
            .optional()
            .map_err(Into::into)
    }

    /// 修复已丢失的主发票原件引用。调用方必须先校验内容并复制到稳定数据目录。
    pub fn repair_invoice_original_file(
        &self,
        invoice_id: i64,
        file_path: &str,
        original_name: &str,
        sha256: &str,
    ) -> StoreResult<()> {
        let batch_id: i64 = self
            .conn
            .query_row(
                "SELECT batch_id FROM reported_invoices WHERE id = ?1",
                params![invoice_id],
                |row| row.get(0),
            )
            .optional()?
            .ok_or_else(|| StoreError::NotFound(format!("ReportedInvoice {invoice_id}")))?;
        self.apply_review_mutation(
            batch_id,
            "invoice_original_relinked",
            "重新关联缺失的主发票原件",
            |transaction| {
                let now = Self::now_text();
                let changed = transaction.execute(
                    "UPDATE reported_invoices SET file_path = ?2, updated_at = ?3
                     WHERE id = ?1 AND batch_id = ?4",
                    params![invoice_id, file_path, now, batch_id],
                )?;
                if changed != 1 {
                    return Err(StoreError::Validation(
                        "invoice changed while relinking original file".to_string(),
                    ));
                }
                transaction.execute(
                    "UPDATE invoice_documents
                     SET file_path = ?2, original_name = ?3, mime_type = ?4, sha256 = ?5
                     WHERE batch_id = ?1 AND source_invoice_id = ?6 AND role = 'main_invoice'",
                    params![
                        batch_id,
                        file_path,
                        original_name,
                        Self::mime_type_for_path(Path::new(file_path)),
                        sha256,
                        invoice_id,
                    ],
                )?;
                Ok(())
            },
        )
    }

    /// 修复费用材料引用；若材料是主发票，同时更新其票据原件路径。
    pub fn repair_invoice_document_file(
        &self,
        document_id: i64,
        file_path: &str,
        original_name: &str,
        sha256: &str,
    ) -> StoreResult<()> {
        let (batch_id, role, source_invoice_id): (i64, String, Option<i64>) = self
            .conn
            .query_row(
                "SELECT batch_id, role, source_invoice_id
                 FROM invoice_documents WHERE id = ?1",
                params![document_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?
            .ok_or_else(|| StoreError::NotFound(format!("InvoiceDocument {document_id}")))?;
        self.apply_review_mutation(
            batch_id,
            "expense_document_relinked",
            "重新关联缺失的费用材料",
            |transaction| {
                let changed = transaction.execute(
                    "UPDATE invoice_documents
                     SET file_path = ?2, original_name = ?3, mime_type = ?4, sha256 = ?5
                     WHERE id = ?1 AND batch_id = ?6",
                    params![
                        document_id,
                        file_path,
                        original_name,
                        Self::mime_type_for_path(Path::new(file_path)),
                        sha256,
                        batch_id,
                    ],
                )?;
                if changed != 1 {
                    return Err(StoreError::Validation(
                        "document changed while relinking file".to_string(),
                    ));
                }
                if role == "main_invoice" {
                    if let Some(invoice_id) = source_invoice_id {
                        transaction.execute(
                            "UPDATE reported_invoices SET file_path = ?2, updated_at = ?3
                             WHERE id = ?1 AND batch_id = ?4",
                            params![invoice_id, file_path, Self::now_text(), batch_id],
                        )?;
                    }
                }
                Ok(())
            },
        )
    }

    /// 修复尚未挂载材料的文件引用。
    pub fn repair_pending_document_file(
        &self,
        document_id: i64,
        file_path: &str,
        original_name: &str,
        sha256: &str,
    ) -> StoreResult<()> {
        let batch_id: i64 = self
            .conn
            .query_row(
                "SELECT batch_id FROM pending_invoice_documents WHERE id = ?1",
                params![document_id],
                |row| row.get(0),
            )
            .optional()?
            .ok_or_else(|| StoreError::NotFound(format!("PendingInvoiceDocument {document_id}")))?;
        self.apply_review_mutation(
            batch_id,
            "pending_document_relinked",
            "重新关联缺失的待挂载材料",
            |transaction| {
                let changed = transaction.execute(
                    "UPDATE pending_invoice_documents
                     SET file_path = ?2, original_name = ?3, mime_type = ?4,
                         sha256 = ?5, updated_at = ?6
                     WHERE id = ?1 AND batch_id = ?7",
                    params![
                        document_id,
                        file_path,
                        original_name,
                        Self::mime_type_for_path(Path::new(file_path)),
                        sha256,
                        Self::now_text(),
                        batch_id,
                    ],
                )?;
                if changed != 1 {
                    return Err(StoreError::Validation(
                        "pending document changed while relinking file".to_string(),
                    ));
                }
                Ok(())
            },
        )
    }

    fn parse_pending_invoice_document_row(
        row: &Row<'_>,
    ) -> rusqlite::Result<PendingInvoiceDocument> {
        Ok(PendingInvoiceDocument {
            id: row.get(0)?,
            batch_id: row.get(1)?,
            proposed_role: row.get(2)?,
            file_path: row.get(3)?,
            original_name: row.get(4)?,
            mime_type: row.get(5)?,
            sha256: row.get(6)?,
            detection_reason: row.get(7)?,
            status: row.get(8)?,
            assigned_expense_item_id: row.get(9)?,
            created_at: row.get(10)?,
            updated_at: row.get(11)?,
        })
    }

    fn pending_documents_for_connection(
        connection: &Connection,
        batch_id: i64,
    ) -> StoreResult<Vec<PendingInvoiceDocument>> {
        let mut statement = connection.prepare(
            "SELECT id, batch_id, proposed_role, file_path, original_name,
                    mime_type, sha256, detection_reason, status,
                    assigned_expense_item_id, created_at, updated_at
             FROM pending_invoice_documents WHERE batch_id = ?1
             ORDER BY CASE status WHEN 'pending' THEN 0 WHEN 'attached' THEN 1 ELSE 2 END, id",
        )?;
        let documents = statement
            .query_map(params![batch_id], Self::parse_pending_invoice_document_row)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(documents)
    }

    pub fn list_pending_invoice_documents(
        &self,
        batch_id: i64,
    ) -> StoreResult<Vec<PendingInvoiceDocument>> {
        Self::pending_documents_for_connection(&self.conn, batch_id)
    }

    pub fn get_pending_invoice_document(
        &self,
        document_id: i64,
    ) -> StoreResult<Option<PendingInvoiceDocument>> {
        self.conn
            .query_row(
                "SELECT id, batch_id, proposed_role, file_path, original_name,
                        mime_type, sha256, detection_reason, status,
                        assigned_expense_item_id, created_at, updated_at
                 FROM pending_invoice_documents WHERE id = ?1",
                params![document_id],
                Self::parse_pending_invoice_document_row,
            )
            .optional()
            .map_err(Into::into)
    }

    fn email_attachments_for_message(
        connection: &Connection,
        message_id: i64,
    ) -> StoreResult<Vec<EmailImportAttachment>> {
        let mut statement = connection.prepare(
            "SELECT id, message_id, content_sha256, original_name, container_name,
                    mime_type, byte_len, status, role_hint, reason,
                    reported_invoice_id, pending_document_id, manual_import,
                    created_at, updated_at
             FROM email_import_attachments WHERE message_id = ?1 ORDER BY id",
        )?;
        let rows = statement
            .query_map(params![message_id], |row| {
                Ok(EmailImportAttachment {
                    id: row.get(0)?,
                    message_id: row.get(1)?,
                    content_sha256: row.get(2)?,
                    original_name: row.get(3)?,
                    container_name: row.get(4)?,
                    mime_type: row.get(5)?,
                    byte_len: row.get(6)?,
                    status: row.get(7)?,
                    role_hint: row.get(8)?,
                    reason: row.get(9)?,
                    reported_invoice_id: row.get(10)?,
                    pending_document_id: row.get(11)?,
                    manual_import: row.get::<_, i64>(12)? != 0,
                    created_at: row.get(13)?,
                    updated_at: row.get(14)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// 返回批次邮件处理台账。没有记录的历史批次返回空列表。
    pub fn list_email_import_messages(
        &self,
        batch_id: i64,
    ) -> StoreResult<Vec<EmailImportMessage>> {
        let rows = {
            let mut statement = self.conn.prepare(
                "SELECT id, batch_id, pipeline_id, mailbox_folder, uid,
                        message_id_sha256, sender, subject, received_at, status,
                        resolution_status, error_category, created_at, updated_at, resolved_at
                 FROM email_import_messages WHERE batch_id = ?1
                 ORDER BY COALESCE(received_at, created_at), id",
            )?;
            let collected = statement
                .query_map(params![batch_id], |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, i64>(4)?,
                        row.get::<_, Option<String>>(5)?,
                        row.get::<_, String>(6)?,
                        row.get::<_, String>(7)?,
                        row.get::<_, Option<String>>(8)?,
                        row.get::<_, String>(9)?,
                        row.get::<_, String>(10)?,
                        row.get::<_, Option<String>>(11)?,
                        row.get::<_, String>(12)?,
                        row.get::<_, String>(13)?,
                        row.get::<_, Option<String>>(14)?,
                    ))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            collected
        };
        let mut messages = Vec::with_capacity(rows.len());
        for row in rows {
            messages.push(EmailImportMessage {
                id: row.0,
                batch_id: row.1,
                pipeline_id: row.2,
                mailbox_folder: row.3,
                uid: row.4,
                message_id_sha256: row.5,
                sender: row.6,
                subject: row.7,
                received_at: row.8,
                status: row.9,
                resolution_status: row.10,
                error_category: row.11,
                created_at: row.12,
                updated_at: row.13,
                resolved_at: row.14,
                attachments: Self::email_attachments_for_message(&self.conn, row.0)?,
            });
        }
        Ok(messages)
    }

    /// 标记邮件人工处置结果。语义分类不被覆盖，便于以后重新核对。
    pub fn resolve_email_import_message(&self, message_id: i64, action: &str) -> StoreResult<()> {
        let resolution = match action {
            "resolve" => "resolved",
            "ignore" => "ignored",
            "reopen" => "open",
            _ => {
                return Err(StoreError::Validation(
                    "invalid email ledger resolution action".to_string(),
                ))
            }
        };
        let (batch_id, batch_status): (i64, i32) = self
            .conn
            .query_row(
                "SELECT m.batch_id, b.status
                 FROM email_import_messages m JOIN batches b ON b.id = m.batch_id
                 WHERE m.id = ?1",
                params![message_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?
            .ok_or_else(|| StoreError::NotFound(format!("Email import message {message_id}")))?;
        if batch_status != BatchStatus::Draft.to_i32() {
            return Err(StoreError::Validation(
                "email ledger can only be changed while batch is in draft".to_string(),
            ));
        }
        let now = Self::now_text();
        let changed = self.conn.execute(
            "UPDATE email_import_messages
             SET resolution_status = ?2,
                 resolved_at = CASE WHEN ?2 = 'open' THEN NULL ELSE ?3 END,
                 updated_at = ?3
             WHERE id = ?1 AND batch_id = ?4",
            params![message_id, resolution, now, batch_id],
        )?;
        if changed != 1 {
            return Err(StoreError::Validation(
                "email ledger message changed concurrently".to_string(),
            ));
        }
        Ok(())
    }

    fn unresolved_actionable_email_count_for_connection(
        connection: &Connection,
        batch_id: i64,
    ) -> StoreResult<i64> {
        connection
            .query_row(
                "SELECT COUNT(*) FROM email_import_messages
                 WHERE batch_id = ?1 AND resolution_status = 'open'
                   AND status IN ('materials_only', 'manual_download', 'needs_confirmation', 'failed')",
                params![batch_id],
                |row| row.get(0),
            )
            .map_err(Into::into)
    }

    pub fn unresolved_actionable_email_count(&self, batch_id: i64) -> StoreResult<i64> {
        Self::unresolved_actionable_email_count_for_connection(&self.conn, batch_id)
    }

    fn parse_email_collection_task_row(row: &Row<'_>) -> rusqlite::Result<EmailCollectionTask> {
        Ok(EmailCollectionTask {
            id: row.get(0)?,
            name: row.get(1)?,
            account_email: row.get(2)?,
            mailbox_folder: row.get(3)?,
            date_start: row.get(4)?,
            date_end: row.get(5)?,
            status: row.get(6)?,
            review_status: row.get(7)?,
            pipeline_id: row.get(8)?,
            last_error_category: row.get(9)?,
            scanned_message_count: row.get(10)?,
            candidate_file_count: row.get(11)?,
            actionable_message_count: row.get(12)?,
            created_at: row.get(13)?,
            updated_at: row.get(14)?,
            completed_at: row.get(15)?,
        })
    }

    /// 创建独立邮件收集任务。任务只描述来源，不创建报销批次。
    pub fn create_email_collection_task(
        &self,
        name: &str,
        account_email: &str,
        date_start: &str,
        date_end: &str,
    ) -> StoreResult<i64> {
        let name = name.trim();
        let account_email = account_email.trim();
        if name.is_empty()
            || name.chars().count() > 100
            || account_email.is_empty()
            || account_email.chars().count() > 254
            || date_start.len() != 10
            || date_end.len() != 10
            || date_start >= date_end
        {
            return Err(StoreError::Validation(
                "invalid email collection task configuration".to_string(),
            ));
        }
        let now = Self::now_text();
        self.conn.execute(
            "INSERT INTO email_collection_tasks (
                name, account_email, mailbox_folder, date_start, date_end, status,
                review_status, created_at, updated_at
             ) VALUES (?1, ?2, 'INBOX', ?3, ?4, 'created', 'open', ?5, ?5)",
            params![name, account_email, date_start, date_end, now],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn get_email_collection_task(&self, task_id: i64) -> StoreResult<EmailCollectionTask> {
        self.conn
            .query_row(
                "SELECT id, name, account_email, mailbox_folder, date_start, date_end,
                        status, review_status, pipeline_id, last_error_category,
                        scanned_message_count, candidate_file_count,
                        actionable_message_count, created_at, updated_at, completed_at
                 FROM email_collection_tasks WHERE id = ?1",
                params![task_id],
                Self::parse_email_collection_task_row,
            )
            .optional()?
            .ok_or_else(|| StoreError::NotFound(format!("Email collection task {task_id}")))
    }

    pub fn list_email_collection_tasks(&self) -> StoreResult<Vec<EmailCollectionTask>> {
        let mut statement = self.conn.prepare(
            "SELECT id, name, account_email, mailbox_folder, date_start, date_end,
                    status, review_status, pipeline_id, last_error_category,
                    scanned_message_count, candidate_file_count,
                    actionable_message_count, created_at, updated_at, completed_at
             FROM email_collection_tasks ORDER BY updated_at DESC, id DESC",
        )?;
        let tasks = statement
            .query_map([], Self::parse_email_collection_task_row)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(tasks)
    }

    /// 删除尚未被报销批次引用的本地邮件收集任务。
    ///
    /// 邮件、附件和审核快照通过外键级联删除；一旦任务形成批次导入快照，
    /// 必须保留来源台账，避免破坏已导入数据的可追溯关系。
    pub fn delete_email_collection_task(&self, task_id: i64) -> StoreResult<()> {
        let transaction = self.conn.unchecked_transaction()?;
        let status: Option<String> = transaction
            .query_row(
                "SELECT status FROM email_collection_tasks WHERE id = ?1",
                params![task_id],
                |row| row.get(0),
            )
            .optional()?;
        let Some(status) = status else {
            return Err(StoreError::NotFound(format!(
                "Email collection task {task_id}"
            )));
        };
        if status == "collecting" {
            return Err(StoreError::Validation(
                "正在收集的任务不能删除，请等待完成或重启后再试".to_string(),
            ));
        }
        let import_count: i64 = transaction.query_row(
            "SELECT COUNT(*) FROM batch_collection_imports WHERE task_id = ?1",
            params![task_id],
            |row| row.get(0),
        )?;
        if import_count > 0 {
            return Err(StoreError::Validation(
                "该收集任务已导入报销批次，为保护来源记录不能删除".to_string(),
            ));
        }
        let changed = transaction.execute(
            "DELETE FROM email_collection_tasks WHERE id = ?1",
            params![task_id],
        )?;
        if changed != 1 {
            return Err(StoreError::NotFound(format!(
                "Email collection task {task_id}"
            )));
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn mark_email_collection_started(
        &self,
        task_id: i64,
        pipeline_id: &str,
    ) -> StoreResult<()> {
        let changed = self.conn.execute(
            "UPDATE email_collection_tasks
             SET status = 'collecting', review_status = 'open', pipeline_id = ?2,
                 last_error_category = NULL, completed_at = NULL, updated_at = ?3
             WHERE id = ?1 AND status IN ('created', 'failed', 'interrupted')",
            params![task_id, pipeline_id, Self::now_text()],
        )?;
        if changed != 1 {
            return Err(StoreError::Validation(
                "collection task is not ready to start or retry".to_string(),
            ));
        }
        Ok(())
    }

    pub fn mark_email_collection_failed(
        &self,
        task_id: i64,
        error_category: &str,
    ) -> StoreResult<()> {
        let category: String = error_category.chars().take(100).collect();
        let changed = self.conn.execute(
            "UPDATE email_collection_tasks
             SET status = 'failed', last_error_category = ?2, updated_at = ?3
             WHERE id = ?1 AND status = 'collecting'",
            params![task_id, category, Self::now_text()],
        )?;
        if changed != 1 {
            return Err(StoreError::Validation(
                "collection task is not running".to_string(),
            ));
        }
        Ok(())
    }

    pub fn mark_collecting_email_collection_tasks_interrupted(&self) -> StoreResult<usize> {
        let now = Self::now_text();
        self.conn
            .execute(
                "UPDATE email_collection_tasks
                 SET status = 'interrupted', last_error_category = 'application_restarted',
                     updated_at = ?1 WHERE status = 'collecting'",
                params![now],
            )
            .map_err(Into::into)
    }

    /// 原子保存逐封邮件收集终态。正文和链接来自本次只读下载生成的审核快照；
    /// 授权码永远不接受为输入。
    pub fn store_email_collection_results(
        &self,
        task_id: i64,
        messages: &[NewCollectedEmailMessage],
    ) -> StoreResult<()> {
        let transaction = self.conn.unchecked_transaction()?;
        let status: Option<String> = transaction
            .query_row(
                "SELECT status FROM email_collection_tasks WHERE id = ?1",
                params![task_id],
                |row| row.get(0),
            )
            .optional()?;
        match status.as_deref() {
            None => {
                return Err(StoreError::NotFound(format!(
                    "Email collection task {task_id}"
                )))
            }
            Some("collecting") => {}
            _ => {
                return Err(StoreError::Validation(
                    "collection task is not accepting results".to_string(),
                ))
            }
        }

        transaction.execute(
            "DELETE FROM collected_email_messages WHERE task_id = ?1",
            params![task_id],
        )?;
        let now = Self::now_text();
        for message in messages {
            if !matches!(
                message.status.as_str(),
                "has_candidates"
                    | "materials_only"
                    | "manual_download"
                    | "needs_confirmation"
                    | "not_relevant"
                    | "failed"
            ) {
                return Err(StoreError::Validation(
                    "invalid collected email status".to_string(),
                ));
            }
            transaction.execute(
                "INSERT INTO collected_email_messages (
                    task_id, mailbox_folder, uid, message_id_sha256, sender, subject,
                    received_at, status, resolution_status, error_category, resolved_at,
                    review_sender_name, review_sender_address, review_body_text,
                    review_body_truncated, review_analyzed_at, created_at, updated_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8,
                           CASE WHEN ?8 = 'not_relevant' THEN 'ignored' ELSE 'open' END,
                           ?9, CASE WHEN ?8 = 'not_relevant' THEN ?15 ELSE NULL END,
                           ?10, ?11, ?12, ?13, ?14, ?15, ?15)",
                params![
                    task_id,
                    message.mailbox_folder,
                    message.uid,
                    message.message_id_sha256,
                    message.sender,
                    message.subject,
                    message.received_at,
                    message.status,
                    message.error_category,
                    message
                        .review
                        .as_ref()
                        .and_then(|review| review.sender_name.as_deref()),
                    message
                        .review
                        .as_ref()
                        .and_then(|review| review.sender_address.as_deref()),
                    message
                        .review
                        .as_ref()
                        .map(|review| review.body_text.as_str()),
                    i64::from(
                        message
                            .review
                            .as_ref()
                            .is_some_and(|review| review.body_truncated),
                    ),
                    message.review.as_ref().map(|_| now.as_str()),
                    now,
                ],
            )?;
            let message_id = transaction.last_insert_rowid();
            if let Some(review) = &message.review {
                Self::validate_collected_email_review(review)?;
                for (position, link) in review.links.iter().enumerate() {
                    transaction.execute(
                        "INSERT INTO collected_email_links (
                            message_id, position, label, host, url, scheme, created_at, updated_at
                         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)",
                        params![
                            message_id,
                            position as i64,
                            link.label,
                            link.host,
                            link.url,
                            link.scheme,
                            now,
                        ],
                    )?;
                }
            }
            for attachment in &message.attachments {
                if !matches!(
                    attachment.status.as_str(),
                    "candidate" | "supporting_candidate" | "filtered" | "unsupported" | "failed"
                ) || !matches!(
                    attachment.role_hint.as_str(),
                    "invoice" | "itinerary" | "detail" | "supporting" | "unknown"
                ) {
                    return Err(StoreError::Validation(
                        "invalid collected attachment classification".to_string(),
                    ));
                }
                transaction.execute(
                    "INSERT INTO collected_email_attachments (
                        message_id, content_sha256, original_name, container_name,
                        mime_type, byte_len, status, role_hint, reason, stored_path,
                        manual_import, created_at, updated_at
                     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?12)",
                    params![
                        message_id,
                        attachment.content_sha256,
                        attachment.original_name,
                        attachment.container_name,
                        attachment.mime_type,
                        attachment.byte_len,
                        attachment.status,
                        attachment.role_hint,
                        attachment.reason,
                        attachment.stored_path,
                        i64::from(attachment.manual_import),
                        now,
                    ],
                )?;
            }
        }

        let candidate_count: i64 = transaction.query_row(
            "SELECT COUNT(*) FROM collected_email_attachments a
             JOIN collected_email_messages m ON m.id = a.message_id
             WHERE m.task_id = ?1 AND a.stored_path IS NOT NULL
               AND a.status IN ('candidate', 'supporting_candidate')
               AND a.user_excluded = 0",
            params![task_id],
            |row| row.get(0),
        )?;
        let actionable_count: i64 = transaction.query_row(
            "SELECT COUNT(*) FROM collected_email_messages
             WHERE task_id = ?1 AND resolution_status = 'open'
               AND status IN ('materials_only', 'manual_download', 'needs_confirmation', 'failed')",
            params![task_id],
            |row| row.get(0),
        )?;
        transaction.execute(
            "UPDATE email_collection_tasks
             SET status = 'review', scanned_message_count = ?2,
                 candidate_file_count = ?3, actionable_message_count = ?4,
                 last_error_category = NULL, updated_at = ?5
             WHERE id = ?1 AND status = 'collecting'",
            params![
                task_id,
                messages.len() as i64,
                candidate_count,
                actionable_count,
                now
            ],
        )?;
        transaction.commit()?;
        Ok(())
    }

    fn validate_collected_email_review(
        review: &NewCollectedEmailReviewSnapshot,
    ) -> StoreResult<()> {
        if review.body_text.chars().count() > 100 * 1024
            || review.links.len() > 20
            || review
                .sender_name
                .as_ref()
                .is_some_and(|value| value.chars().count() > 500)
            || review
                .sender_address
                .as_ref()
                .is_some_and(|value| value.chars().count() > 500)
        {
            return Err(StoreError::Validation(
                "invalid collected email review snapshot".to_string(),
            ));
        }
        for link in &review.links {
            let expected_prefix = format!("{}://", link.scheme);
            if !matches!(link.scheme.as_str(), "http" | "https")
                || link.label.trim().is_empty()
                || link.label.chars().count() > 120
                || link.host.trim().is_empty()
                || link.host.chars().count() > 253
                || link.url.chars().count() > 4096
                || !link.url.to_ascii_lowercase().starts_with(&expected_prefix)
            {
                return Err(StoreError::Validation(
                    "invalid collected email review link".to_string(),
                ));
            }
        }
        Ok(())
    }

    fn collected_email_links_for_message(
        connection: &Connection,
        message_id: i64,
    ) -> StoreResult<Vec<CollectedEmailLink>> {
        let mut statement = connection.prepare(
            "SELECT id, message_id, position, label, host, url, scheme, created_at, updated_at
             FROM collected_email_links WHERE message_id = ?1 ORDER BY position, id",
        )?;
        let links = statement
            .query_map(params![message_id], |row| {
                Ok(CollectedEmailLink {
                    id: row.get(0)?,
                    message_id: row.get(1)?,
                    position: row.get(2)?,
                    label: row.get(3)?,
                    host: row.get(4)?,
                    url: row.get(5)?,
                    scheme: row.get(6)?,
                    created_at: row.get(7)?,
                    updated_at: row.get(8)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()
            .map_err(StoreError::from)?;
        Ok(links)
    }

    /// 读取收集阶段已经保存的正文与链接；不会访问邮箱。
    pub fn get_collected_email_review_snapshot(
        &self,
        message_id: i64,
    ) -> StoreResult<Option<CollectedEmailReviewSnapshot>> {
        let row = self
            .conn
            .query_row(
                "SELECT review_sender_name, review_sender_address, review_body_text,
                        review_body_truncated, review_analyzed_at
                 FROM collected_email_messages WHERE id = ?1",
                params![message_id],
                |row| {
                    Ok((
                        row.get::<_, Option<String>>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, i64>(3)? != 0,
                        row.get::<_, Option<String>>(4)?,
                    ))
                },
            )
            .optional()?
            .ok_or_else(|| StoreError::NotFound(format!("Collected email message {message_id}")))?;
        let (Some(body_text), Some(analyzed_at)) = (row.2, row.4) else {
            return Ok(None);
        };
        Ok(Some(CollectedEmailReviewSnapshot {
            message_id,
            sender_name: row.0,
            sender_address: row.1,
            body_text,
            body_truncated: row.3,
            analyzed_at,
            links: Self::collected_email_links_for_message(&self.conn, message_id)?,
        }))
    }

    /// 用户明确要求重新分析时，原子替换这一封邮件的正文和链接快照。
    pub fn replace_collected_email_review_snapshot(
        &self,
        message_id: i64,
        review: &NewCollectedEmailReviewSnapshot,
    ) -> StoreResult<()> {
        Self::validate_collected_email_review(review)?;
        let transaction = self.conn.unchecked_transaction()?;
        let now = Self::now_text();
        let changed = transaction.execute(
            "UPDATE collected_email_messages
             SET review_sender_name = ?2, review_sender_address = ?3,
                 review_body_text = ?4, review_body_truncated = ?5,
                 review_analyzed_at = ?6, updated_at = ?6
             WHERE id = ?1",
            params![
                message_id,
                review.sender_name,
                review.sender_address,
                review.body_text,
                i64::from(review.body_truncated),
                now,
            ],
        )?;
        if changed != 1 {
            return Err(StoreError::NotFound(format!(
                "Collected email message {message_id}"
            )));
        }
        transaction.execute(
            "DELETE FROM collected_email_links WHERE message_id = ?1",
            params![message_id],
        )?;
        for (position, link) in review.links.iter().enumerate() {
            transaction.execute(
                "INSERT INTO collected_email_links (
                    message_id, position, label, host, url, scheme, created_at, updated_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)",
                params![
                    message_id,
                    position as i64,
                    link.label,
                    link.host,
                    link.url,
                    link.scheme,
                    now,
                ],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    /// 用户明确要求重新分析时，原子替换该邮件的自动采集结果。用户手工补充的
    /// 文件始终保留；已经冻结到报销批次的自动附件不能被静默替换。
    pub fn replace_collected_email_analysis(
        &self,
        message_id: i64,
        message: &NewCollectedEmailMessage,
    ) -> StoreResult<()> {
        if !matches!(
            message.status.as_str(),
            "has_candidates"
                | "materials_only"
                | "manual_download"
                | "needs_confirmation"
                | "not_relevant"
                | "failed"
        ) {
            return Err(StoreError::Validation(
                "invalid collected email status".to_string(),
            ));
        }
        let review = message.review.as_ref().ok_or_else(|| {
            StoreError::Validation("reanalyzed email must include a review snapshot".to_string())
        })?;
        Self::validate_collected_email_review(review)?;
        for attachment in &message.attachments {
            if !matches!(
                attachment.status.as_str(),
                "candidate" | "supporting_candidate" | "filtered" | "unsupported" | "failed"
            ) || !matches!(
                attachment.role_hint.as_str(),
                "invoice" | "itinerary" | "detail" | "supporting" | "unknown"
            ) || attachment.manual_import
            {
                return Err(StoreError::Validation(
                    "invalid reanalyzed email attachment".to_string(),
                ));
            }
        }

        let transaction = self.conn.unchecked_transaction()?;
        let task_id: i64 = transaction
            .query_row(
                "SELECT task_id FROM collected_email_messages WHERE id = ?1",
                params![message_id],
                |row| row.get(0),
            )
            .optional()?
            .ok_or_else(|| StoreError::NotFound(format!("Collected email message {message_id}")))?;
        let task_status: String = transaction.query_row(
            "SELECT status FROM email_collection_tasks WHERE id = ?1",
            params![task_id],
            |row| row.get(0),
        )?;
        if !matches!(task_status.as_str(), "review" | "completed") {
            return Err(StoreError::Validation(
                "collection task is not available for reanalysis".to_string(),
            ));
        }
        let used_count: i64 = transaction.query_row(
            "SELECT COUNT(*) FROM batch_collection_import_items i
             JOIN collected_email_attachments a ON a.id = i.attachment_id
             WHERE a.message_id = ?1 AND a.manual_import = 0",
            params![message_id],
            |row| row.get(0),
        )?;
        if used_count > 0 {
            return Err(StoreError::Validation(
                "collected attachments already frozen into a batch cannot be reanalyzed"
                    .to_string(),
            ));
        }

        let now = Self::now_text();
        transaction.execute(
            "DELETE FROM collected_email_attachments
             WHERE message_id = ?1 AND manual_import = 0",
            params![message_id],
        )?;
        transaction.execute(
            "DELETE FROM collected_email_links WHERE message_id = ?1",
            params![message_id],
        )?;
        transaction.execute(
            "UPDATE collected_email_messages
             SET message_id_sha256 = ?2, sender = ?3, subject = ?4, status = ?5,
                 resolution_status = CASE WHEN ?5 = 'not_relevant' THEN 'ignored' ELSE 'open' END,
                 error_category = ?6,
                 resolved_at = CASE WHEN ?5 = 'not_relevant' THEN ?11 ELSE NULL END,
                 review_sender_name = ?7, review_sender_address = ?8,
                 review_body_text = ?9, review_body_truncated = ?10,
                 review_analyzed_at = ?11, updated_at = ?11
             WHERE id = ?1",
            params![
                message_id,
                message.message_id_sha256,
                message.sender,
                message.subject,
                message.status,
                message.error_category,
                review.sender_name,
                review.sender_address,
                review.body_text,
                i64::from(review.body_truncated),
                now,
            ],
        )?;
        for (position, link) in review.links.iter().enumerate() {
            transaction.execute(
                "INSERT INTO collected_email_links (
                    message_id, position, label, host, url, scheme, created_at, updated_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)",
                params![
                    message_id,
                    position as i64,
                    link.label,
                    link.host,
                    link.url,
                    link.scheme,
                    now,
                ],
            )?;
        }
        for attachment in &message.attachments {
            transaction.execute(
                "INSERT INTO collected_email_attachments (
                    message_id, content_sha256, original_name, container_name,
                    mime_type, byte_len, status, role_hint, reason, stored_path,
                    manual_import, created_at, updated_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, 0, ?11, ?11)",
                params![
                    message_id,
                    attachment.content_sha256,
                    attachment.original_name,
                    attachment.container_name,
                    attachment.mime_type,
                    attachment.byte_len,
                    attachment.status,
                    attachment.role_hint,
                    attachment.reason,
                    attachment.stored_path,
                    now,
                ],
            )?;
        }
        let candidate_count: i64 = transaction.query_row(
            "SELECT COUNT(*) FROM collected_email_attachments a
             JOIN collected_email_messages m ON m.id = a.message_id
             WHERE m.task_id = ?1 AND a.stored_path IS NOT NULL
               AND a.status IN ('candidate', 'supporting_candidate')
               AND a.user_excluded = 0",
            params![task_id],
            |row| row.get(0),
        )?;
        let actionable_count: i64 = transaction.query_row(
            "SELECT COUNT(*) FROM collected_email_messages
             WHERE task_id = ?1 AND resolution_status = 'open'
               AND status IN ('materials_only', 'manual_download', 'needs_confirmation', 'failed')",
            params![task_id],
            |row| row.get(0),
        )?;
        transaction.execute(
            "UPDATE email_collection_tasks
             SET status = 'review', review_status = 'open', completed_at = NULL,
                 candidate_file_count = ?2, actionable_message_count = ?3,
                 updated_at = ?4 WHERE id = ?1",
            params![task_id, candidate_count, actionable_count, now],
        )?;
        transaction.commit()?;
        Ok(())
    }

    /// 读取用户点击的单个链接。完整 URL 始终停留在 Rust 后端。
    pub fn get_collected_email_link(
        &self,
        message_id: i64,
        link_id: i64,
    ) -> StoreResult<CollectedEmailLink> {
        self.conn
            .query_row(
                "SELECT id, message_id, position, label, host, url, scheme, created_at, updated_at
                 FROM collected_email_links WHERE id = ?1 AND message_id = ?2",
                params![link_id, message_id],
                |row| {
                    Ok(CollectedEmailLink {
                        id: row.get(0)?,
                        message_id: row.get(1)?,
                        position: row.get(2)?,
                        label: row.get(3)?,
                        host: row.get(4)?,
                        url: row.get(5)?,
                        scheme: row.get(6)?,
                        created_at: row.get(7)?,
                        updated_at: row.get(8)?,
                    })
                },
            )
            .optional()?
            .ok_or_else(|| {
                StoreError::NotFound(format!(
                    "Collected email link {link_id} for message {message_id}"
                ))
            })
    }

    /// 保存用户对本地图片执行的二维码分析。链接只在可信后端持久化；二维码为
    /// 图片主体时，该图片退出批次候选并转为下载指引。普通“账单中包含二维码”
    /// 只增加标记和链接，不丢失原材料角色。
    pub fn store_collected_attachment_qr_analysis(
        &self,
        attachment_id: i64,
        links: &[NewCollectedEmailLink],
        qr_dominant: bool,
    ) -> StoreResult<()> {
        Self::validate_collected_email_review(&NewCollectedEmailReviewSnapshot {
            sender_name: None,
            sender_address: None,
            body_text: String::new(),
            body_truncated: false,
            links: links.to_vec(),
        })?;
        let transaction = self.conn.unchecked_transaction()?;
        let attachment: Option<(i64, i64, Option<String>)> = transaction
            .query_row(
                "SELECT a.message_id, m.task_id, a.stored_path
                 FROM collected_email_attachments a
                 JOIN collected_email_messages m ON m.id = a.message_id
                 WHERE a.id = ?1",
                params![attachment_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        let (message_id, task_id, stored_path) = attachment.ok_or_else(|| {
            StoreError::NotFound(format!("Collected email attachment {attachment_id}"))
        })?;
        if stored_path.is_none() {
            return Err(StoreError::Validation(
                "QR analysis requires a locally stored attachment".to_string(),
            ));
        }
        let task_status: String = transaction.query_row(
            "SELECT status FROM email_collection_tasks WHERE id = ?1",
            params![task_id],
            |row| row.get(0),
        )?;
        if !matches!(task_status.as_str(), "review" | "completed") {
            return Err(StoreError::Validation(
                "collection task is not available for QR review".to_string(),
            ));
        }

        let now = Self::now_text();
        let mut next_position: i64 = transaction.query_row(
            "SELECT COALESCE(MAX(position), -1) + 1 FROM collected_email_links
             WHERE message_id = ?1",
            params![message_id],
            |row| row.get(0),
        )?;
        let mut link_count: i64 = transaction.query_row(
            "SELECT COUNT(*) FROM collected_email_links WHERE message_id = ?1",
            params![message_id],
            |row| row.get(0),
        )?;
        for link in links {
            let exists: bool = transaction.query_row(
                "SELECT EXISTS(SELECT 1 FROM collected_email_links
                 WHERE message_id = ?1 AND url = ?2)",
                params![message_id, link.url],
                |row| row.get(0),
            )?;
            if exists || link_count >= 20 {
                continue;
            }
            transaction.execute(
                "INSERT INTO collected_email_links (
                    message_id, position, label, host, url, scheme, created_at, updated_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)",
                params![
                    message_id,
                    next_position,
                    link.label,
                    link.host,
                    link.url,
                    link.scheme,
                    now,
                ],
            )?;
            next_position += 1;
            link_count += 1;
        }

        let reason = if links.is_empty() {
            "attachment_qr_no_browser_url"
        } else if qr_dominant {
            "attachment_qr_manual_download"
        } else {
            "attachment_contains_qr_link"
        };
        if qr_dominant {
            transaction.execute(
                "UPDATE collected_email_attachments
                 SET status = 'filtered', role_hint = 'supporting', reason = ?2,
                     user_excluded = 0, user_excluded_at = NULL, updated_at = ?3
                 WHERE id = ?1",
                params![attachment_id, reason, now],
            )?;
        } else {
            transaction.execute(
                "UPDATE collected_email_attachments SET reason = ?2, updated_at = ?3
                 WHERE id = ?1",
                params![attachment_id, reason, now],
            )?;
        }

        let main_count: i64 = transaction.query_row(
            "SELECT COUNT(*) FROM collected_email_attachments
             WHERE message_id = ?1 AND stored_path IS NOT NULL AND user_excluded = 0
               AND status = 'candidate' AND role_hint IN ('invoice', 'unknown')",
            params![message_id],
            |row| row.get(0),
        )?;
        let supporting_count: i64 = transaction.query_row(
            "SELECT COUNT(*) FROM collected_email_attachments
             WHERE message_id = ?1 AND stored_path IS NOT NULL AND user_excluded = 0
               AND status = 'supporting_candidate'",
            params![message_id],
            |row| row.get(0),
        )?;
        let old_status: String = transaction.query_row(
            "SELECT status FROM collected_email_messages WHERE id = ?1",
            params![message_id],
            |row| row.get(0),
        )?;
        let status = if old_status == "failed" {
            "failed"
        } else if main_count > 0 {
            "has_candidates"
        } else if !links.is_empty() {
            "manual_download"
        } else if supporting_count > 0 {
            "materials_only"
        } else {
            "needs_confirmation"
        };
        transaction.execute(
            "UPDATE collected_email_messages
             SET status = ?2, resolution_status = 'open', resolved_at = NULL,
                 review_body_text = COALESCE(review_body_text, ''),
                 review_analyzed_at = COALESCE(review_analyzed_at, ?3), updated_at = ?3
             WHERE id = ?1",
            params![message_id, status, now],
        )?;

        let candidate_count: i64 = transaction.query_row(
            "SELECT COUNT(*) FROM collected_email_attachments a
             JOIN collected_email_messages m ON m.id = a.message_id
             WHERE m.task_id = ?1 AND a.stored_path IS NOT NULL
               AND a.status IN ('candidate', 'supporting_candidate')
               AND a.user_excluded = 0",
            params![task_id],
            |row| row.get(0),
        )?;
        let actionable_count: i64 = transaction.query_row(
            "SELECT COUNT(*) FROM collected_email_messages
             WHERE task_id = ?1 AND resolution_status = 'open'
               AND status IN ('materials_only', 'manual_download', 'needs_confirmation', 'failed')",
            params![task_id],
            |row| row.get(0),
        )?;
        transaction.execute(
            "UPDATE email_collection_tasks
             SET status = 'review', review_status = 'open', completed_at = NULL,
                 candidate_file_count = ?2, actionable_message_count = ?3, updated_at = ?4
             WHERE id = ?1",
            params![task_id, candidate_count, actionable_count, now],
        )?;
        transaction.commit()?;
        Ok(())
    }

    fn collected_email_attachments_for_message(
        connection: &Connection,
        message_id: i64,
    ) -> StoreResult<Vec<CollectedEmailAttachment>> {
        let rows = {
            let mut statement = connection.prepare(
                "SELECT id, message_id, content_sha256, original_name, container_name,
                         mime_type, byte_len, status, role_hint, reason, stored_path,
                         manual_import, user_excluded, user_excluded_at, created_at, updated_at
                  FROM collected_email_attachments WHERE message_id = ?1 ORDER BY id",
            )?;
            let collected = statement
                .query_map(params![message_id], |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, Option<String>>(4)?,
                        row.get::<_, Option<String>>(5)?,
                        row.get::<_, i64>(6)?,
                        row.get::<_, String>(7)?,
                        row.get::<_, String>(8)?,
                        row.get::<_, String>(9)?,
                        row.get::<_, Option<String>>(10)?,
                        row.get::<_, i64>(11)? != 0,
                        row.get::<_, i64>(12)? != 0,
                        row.get::<_, Option<String>>(13)?,
                        row.get::<_, String>(14)?,
                        row.get::<_, String>(15)?,
                    ))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            collected
        };
        let mut attachments = Vec::with_capacity(rows.len());
        for row in rows {
            let usage = {
                let mut statement = connection.prepare(
                    "SELECT DISTINCT b.id, b.name
                     FROM batch_collection_import_items i
                     JOIN batch_collection_imports bi ON bi.id = i.import_id
                     JOIN batches b ON b.id = bi.batch_id
                     WHERE i.attachment_id = ?1 AND bi.status != 'failed'
                     ORDER BY b.id",
                )?;
                let collected = statement
                    .query_map(params![row.0], |usage_row| {
                        Ok((usage_row.get::<_, i64>(0)?, usage_row.get::<_, String>(1)?))
                    })?
                    .collect::<Result<Vec<_>, _>>()?;
                collected
            };
            attachments.push(CollectedEmailAttachment {
                id: row.0,
                message_id: row.1,
                content_sha256: row.2,
                original_name: row.3,
                container_name: row.4,
                mime_type: row.5,
                byte_len: row.6,
                status: row.7,
                role_hint: row.8,
                reason: row.9,
                stored_path: row.10,
                manual_import: row.11,
                user_excluded: row.12,
                user_excluded_at: row.13,
                used_batch_ids: usage.iter().map(|value| value.0).collect(),
                used_batch_names: usage.into_iter().map(|value| value.1).collect(),
                created_at: row.14,
                updated_at: row.15,
            });
        }
        Ok(attachments)
    }

    pub fn list_collected_email_messages(
        &self,
        task_id: i64,
    ) -> StoreResult<Vec<CollectedEmailMessage>> {
        self.get_email_collection_task(task_id)?;
        let rows = {
            let mut statement = self.conn.prepare(
                "SELECT id, task_id, mailbox_folder, uid, message_id_sha256, sender,
                        subject, received_at, status, resolution_status, error_category,
                        created_at, updated_at, resolved_at
                 FROM collected_email_messages WHERE task_id = ?1
                 ORDER BY CASE WHEN received_at IS NULL OR received_at = '' THEN 1 ELSE 0 END,
                          COALESCE(received_at, created_at) DESC, id DESC",
            )?;
            let collected = statement
                .query_map(params![task_id], |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, i64>(3)?,
                        row.get::<_, Option<String>>(4)?,
                        row.get::<_, String>(5)?,
                        row.get::<_, String>(6)?,
                        row.get::<_, Option<String>>(7)?,
                        row.get::<_, String>(8)?,
                        row.get::<_, String>(9)?,
                        row.get::<_, Option<String>>(10)?,
                        row.get::<_, String>(11)?,
                        row.get::<_, String>(12)?,
                        row.get::<_, Option<String>>(13)?,
                    ))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            collected
        };
        let mut messages = Vec::with_capacity(rows.len());
        for row in rows {
            messages.push(CollectedEmailMessage {
                id: row.0,
                task_id: row.1,
                mailbox_folder: row.2,
                uid: row.3,
                message_id_sha256: row.4,
                sender: row.5,
                subject: row.6,
                received_at: row.7,
                status: row.8,
                resolution_status: row.9,
                error_category: row.10,
                created_at: row.11,
                updated_at: row.12,
                resolved_at: row.13,
                attachments: Self::collected_email_attachments_for_message(&self.conn, row.0)?,
            });
        }
        Ok(messages)
    }

    pub fn get_collected_email_message(
        &self,
        message_id: i64,
    ) -> StoreResult<CollectedEmailMessage> {
        let task_id = self.collected_email_message_task_id(message_id)?;
        self.list_collected_email_messages(task_id)?
            .into_iter()
            .find(|message| message.id == message_id)
            .ok_or_else(|| StoreError::NotFound(format!("Collected email message {message_id}")))
    }

    pub fn get_collected_email_attachment(
        &self,
        attachment_id: i64,
    ) -> StoreResult<CollectedEmailAttachment> {
        let message_id = self
            .conn
            .query_row(
                "SELECT message_id FROM collected_email_attachments WHERE id = ?1",
                params![attachment_id],
                |row| row.get::<_, i64>(0),
            )
            .optional()?
            .ok_or_else(|| {
                StoreError::NotFound(format!("Collected email attachment {attachment_id}"))
            })?;
        self.get_collected_email_message(message_id)?
            .attachments
            .into_iter()
            .find(|attachment| attachment.id == attachment_id)
            .ok_or_else(|| {
                StoreError::NotFound(format!("Collected email attachment {attachment_id}"))
            })
    }

    /// 在来源审核中人工排除或恢复一个已下载候选附件。文件始终保留用于追溯，
    /// 但排除后的附件不计入候选数量，也不能进入后续报销批次。
    pub fn set_collected_email_attachment_excluded(
        &self,
        attachment_id: i64,
        excluded: bool,
    ) -> StoreResult<()> {
        let transaction = self.conn.unchecked_transaction()?;
        let attachment: Option<(i64, i64, String, bool, Option<String>)> = transaction
            .query_row(
                "SELECT a.message_id, m.task_id, a.status, a.user_excluded, a.stored_path
                 FROM collected_email_attachments a
                 JOIN collected_email_messages m ON m.id = a.message_id
                 WHERE a.id = ?1",
                params![attachment_id],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get::<_, i64>(3)? != 0,
                        row.get(4)?,
                    ))
                },
            )
            .optional()?;
        let (message_id, task_id, status, was_excluded, stored_path) =
            attachment.ok_or_else(|| {
                StoreError::NotFound(format!("Collected email attachment {attachment_id}"))
            })?;
        if !matches!(
            status.as_str(),
            "candidate" | "supporting_candidate" | "filtered"
        ) || stored_path.is_none()
        {
            return Err(StoreError::Validation(
                "only a locally stored collected attachment can be excluded".to_string(),
            ));
        }
        let task_status: String = transaction.query_row(
            "SELECT status FROM email_collection_tasks WHERE id = ?1",
            params![task_id],
            |row| row.get(0),
        )?;
        if !matches!(task_status.as_str(), "review" | "completed") {
            return Err(StoreError::Validation(
                "collection task is not in review".to_string(),
            ));
        }
        if was_excluded == excluded {
            return Ok(());
        }

        let now = Self::now_text();
        transaction.execute(
            "UPDATE collected_email_attachments
             SET user_excluded = ?2,
                 user_excluded_at = CASE WHEN ?2 = 1 THEN ?3 ELSE NULL END,
                 updated_at = ?3
             WHERE id = ?1",
            params![attachment_id, i64::from(excluded), now],
        )?;
        transaction.execute(
            "UPDATE collected_email_messages
             SET resolution_status = 'open', resolved_at = NULL, updated_at = ?2
             WHERE id = ?1",
            params![message_id, now],
        )?;
        let candidate_count: i64 = transaction.query_row(
            "SELECT COUNT(*) FROM collected_email_attachments a
             JOIN collected_email_messages m ON m.id = a.message_id
             WHERE m.task_id = ?1 AND a.stored_path IS NOT NULL
               AND a.status IN ('candidate', 'supporting_candidate')
               AND a.user_excluded = 0",
            params![task_id],
            |row| row.get(0),
        )?;
        transaction.execute(
            "UPDATE email_collection_tasks
             SET status = 'review', review_status = 'open', completed_at = NULL,
                 candidate_file_count = ?2, updated_at = ?3
             WHERE id = ?1",
            params![task_id, candidate_count, now],
        )?;
        transaction.commit()?;
        self.refresh_email_collection_review_state(task_id)?;
        Ok(())
    }

    pub fn collected_email_message_task_id(&self, message_id: i64) -> StoreResult<i64> {
        self.conn
            .query_row(
                "SELECT task_id FROM collected_email_messages WHERE id = ?1",
                params![message_id],
                |row| row.get(0),
            )
            .optional()?
            .ok_or_else(|| StoreError::NotFound(format!("Collected email message {message_id}")))
    }

    pub fn resolve_collected_email_message(
        &self,
        message_id: i64,
        action: &str,
    ) -> StoreResult<()> {
        let resolution = match action {
            "resolve" => "resolved",
            "ignore" => "ignored",
            "reopen" => "open",
            _ => {
                return Err(StoreError::Validation(
                    "invalid collection review action".to_string(),
                ))
            }
        };
        let task_id: i64 = self
            .conn
            .query_row(
                "SELECT task_id FROM collected_email_messages WHERE id = ?1",
                params![message_id],
                |row| row.get(0),
            )
            .optional()?
            .ok_or_else(|| StoreError::NotFound(format!("Collected email message {message_id}")))?;
        let task = self.get_email_collection_task(task_id)?;
        if !matches!(task.status.as_str(), "review" | "completed") {
            return Err(StoreError::Validation(
                "collection task is not in review".to_string(),
            ));
        }
        let now = Self::now_text();
        self.conn.execute(
            "UPDATE collected_email_messages
             SET resolution_status = ?2,
                 resolved_at = CASE WHEN ?2 = 'open' THEN NULL ELSE ?3 END,
                 updated_at = ?3 WHERE id = ?1",
            params![message_id, resolution, now],
        )?;
        self.refresh_email_collection_review_state(task_id)?;
        Ok(())
    }

    pub fn add_collected_email_attachment(
        &self,
        message_id: i64,
        attachment: &NewCollectedEmailAttachment,
    ) -> StoreResult<i64> {
        if !matches!(
            attachment.status.as_str(),
            "candidate" | "supporting_candidate"
        ) || attachment.stored_path.as_deref().unwrap_or("").is_empty()
        {
            return Err(StoreError::Validation(
                "manual supplement must be a stored candidate file".to_string(),
            ));
        }
        let task_id: i64 = self
            .conn
            .query_row(
                "SELECT task_id FROM collected_email_messages WHERE id = ?1",
                params![message_id],
                |row| row.get(0),
            )
            .optional()?
            .ok_or_else(|| StoreError::NotFound(format!("Collected email message {message_id}")))?;
        let task = self.get_email_collection_task(task_id)?;
        if !matches!(task.status.as_str(), "review" | "completed") {
            return Err(StoreError::Validation(
                "collection task is not in review".to_string(),
            ));
        }
        let now = Self::now_text();
        let transaction = self.conn.unchecked_transaction()?;
        transaction.execute(
            "INSERT INTO collected_email_attachments (
                message_id, content_sha256, original_name, container_name, mime_type,
                byte_len, status, role_hint, reason, stored_path, manual_import,
                created_at, updated_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, 1, ?11, ?11)",
            params![
                message_id,
                attachment.content_sha256,
                attachment.original_name,
                attachment.container_name,
                attachment.mime_type,
                attachment.byte_len,
                attachment.status,
                attachment.role_hint,
                attachment.reason,
                attachment.stored_path,
                now,
            ],
        )?;
        let attachment_id = transaction.last_insert_rowid();
        transaction.execute(
            "UPDATE collected_email_messages
             SET resolution_status = 'resolved', resolved_at = ?2, updated_at = ?2
             WHERE id = ?1",
            params![message_id, now],
        )?;
        transaction.execute(
            "UPDATE email_collection_tasks
             SET status = 'review', review_status = 'open', completed_at = NULL,
                 candidate_file_count = candidate_file_count + 1, updated_at = ?2
             WHERE id = ?1",
            params![task_id, now],
        )?;
        transaction.commit()?;
        self.refresh_email_collection_review_state(task_id)?;
        Ok(attachment_id)
    }

    fn refresh_email_collection_review_state(&self, task_id: i64) -> StoreResult<()> {
        let actionable: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM collected_email_messages
             WHERE task_id = ?1 AND resolution_status = 'open'
               AND status IN ('materials_only', 'manual_download', 'needs_confirmation', 'failed')",
            params![task_id],
            |row| row.get(0),
        )?;
        self.conn.execute(
            "UPDATE email_collection_tasks
             SET actionable_message_count = ?2,
                 review_status = CASE WHEN ?2 = 0 AND status = 'completed'
                                      THEN 'completed' ELSE 'open' END,
                 updated_at = ?3 WHERE id = ?1",
            params![task_id, actionable, Self::now_text()],
        )?;
        Ok(())
    }

    pub fn complete_email_collection_review(&self, task_id: i64) -> StoreResult<()> {
        let task = self.get_email_collection_task(task_id)?;
        if !matches!(task.status.as_str(), "review" | "completed") {
            return Err(StoreError::Validation(
                "collection task is not ready for completion".to_string(),
            ));
        }
        let actionable: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM collected_email_messages
             WHERE task_id = ?1 AND resolution_status = 'open'
               AND status IN ('materials_only', 'manual_download', 'needs_confirmation', 'failed')",
            params![task_id],
            |row| row.get(0),
        )?;
        if actionable > 0 {
            return Err(StoreError::Validation(format!(
                "{actionable} actionable collection messages remain unresolved"
            )));
        }
        let now = Self::now_text();
        self.conn.execute(
            "UPDATE email_collection_tasks
             SET status = 'completed', review_status = 'completed',
                 actionable_message_count = 0, completed_at = ?2, updated_at = ?2
             WHERE id = ?1",
            params![task_id, now],
        )?;
        Ok(())
    }

    /// 冻结一次“收集材料 → 批次”的选择。原任务以后变化不会改写本快照。
    pub fn create_batch_collection_import(
        &self,
        batch_id: i64,
        task_id: i64,
        attachment_ids: &[i64],
    ) -> StoreResult<i64> {
        if attachment_ids.is_empty() {
            return Err(StoreError::Validation(
                "at least one collected attachment is required".to_string(),
            ));
        }
        let unique_ids: HashSet<i64> = attachment_ids.iter().copied().collect();
        if unique_ids.len() != attachment_ids.len() {
            return Err(StoreError::Validation(
                "duplicate collected attachment selection".to_string(),
            ));
        }
        let transaction = self.conn.unchecked_transaction()?;
        let batch_status: Option<i32> = transaction
            .query_row(
                "SELECT status FROM batches WHERE id = ?1",
                params![batch_id],
                |row| row.get(0),
            )
            .optional()?;
        if batch_status != Some(BatchStatus::Draft.to_i32()) {
            return Err(StoreError::Validation(
                "collection materials can only be imported into a draft batch".to_string(),
            ));
        }
        let task_status: Option<String> = transaction
            .query_row(
                "SELECT status FROM email_collection_tasks WHERE id = ?1",
                params![task_id],
                |row| row.get(0),
            )
            .optional()?;
        if !matches!(task_status.as_deref(), Some("review" | "completed")) {
            return Err(StoreError::Validation(
                "collection task is not available for batch import".to_string(),
            ));
        }

        let now = Self::now_text();
        transaction.execute(
            "INSERT INTO batch_collection_imports (
                batch_id, task_id, status, created_at, updated_at
             ) VALUES (?1, ?2, 'pending', ?3, ?3)",
            params![batch_id, task_id, now],
        )?;
        let import_id = transaction.last_insert_rowid();
        for attachment_id in attachment_ids {
            let attachment: Option<(Option<String>, String)> = transaction
                .query_row(
                    "SELECT a.content_sha256, a.original_name
                     FROM collected_email_attachments a
                     JOIN collected_email_messages m ON m.id = a.message_id
                      WHERE a.id = ?1 AND m.task_id = ?2
                        AND a.status IN ('candidate', 'supporting_candidate')
                        AND a.user_excluded = 0
                        AND a.stored_path IS NOT NULL AND a.stored_path != ''",
                    params![attachment_id, task_id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()?;
            let (sha256, original_name) = attachment.ok_or_else(|| {
                StoreError::Validation(format!(
                    "collected attachment {attachment_id} is not importable"
                ))
            })?;
            transaction.execute(
                "INSERT INTO batch_collection_import_items (
                    import_id, attachment_id, source_sha256, original_name, created_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![import_id, attachment_id, sha256, original_name, now],
            )?;
        }
        transaction.commit()?;
        Ok(import_id)
    }

    pub fn link_batch_collection_import_pipeline(
        &self,
        import_id: i64,
        pipeline_id: &str,
    ) -> StoreResult<()> {
        let changed = self.conn.execute(
            "UPDATE batch_collection_imports
             SET pipeline_id = ?2, status = 'processing', updated_at = ?3
             WHERE id = ?1 AND status IN ('pending', 'failed')
               AND EXISTS (
                   SELECT 1 FROM pipeline_runs p
                   WHERE p.pipeline_id = ?2 AND p.source_kind = 'collection_import'
               )",
            params![import_id, pipeline_id, Self::now_text()],
        )?;
        if changed != 1 {
            return Err(StoreError::Validation(
                "collection import cannot be linked to this pipeline".to_string(),
            ));
        }
        Ok(())
    }

    pub fn collection_import_file_paths(
        &self,
        import_id: i64,
        expected_batch_id: i64,
    ) -> StoreResult<Vec<String>> {
        let status: Option<(i64, String)> = self
            .conn
            .query_row(
                "SELECT batch_id, status FROM batch_collection_imports WHERE id = ?1",
                params![import_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let Some((batch_id, status)) = status else {
            return Err(StoreError::NotFound(format!(
                "Collection import {import_id}"
            )));
        };
        if batch_id != expected_batch_id
            || !matches!(status.as_str(), "pending" | "processing" | "failed")
        {
            return Err(StoreError::Validation(
                "collection import does not belong to this draft batch".to_string(),
            ));
        }
        let mut statement = self.conn.prepare(
            "SELECT a.stored_path
             FROM batch_collection_import_items i
             JOIN collected_email_attachments a ON a.id = i.attachment_id
             WHERE i.import_id = ?1 ORDER BY i.id",
        )?;
        let paths = statement
            .query_map(params![import_id], |row| row.get::<_, Option<String>>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        if paths.is_empty() || paths.iter().any(Option::is_none) {
            return Err(StoreError::Validation(
                "collection import references unavailable files".to_string(),
            ));
        }
        Ok(paths.into_iter().flatten().collect())
    }

    pub fn list_batch_collection_imports(
        &self,
        batch_id: i64,
    ) -> StoreResult<Vec<BatchCollectionImport>> {
        let mut statement = self.conn.prepare(
            "SELECT bi.id, bi.batch_id, bi.task_id, t.name, bi.status, bi.pipeline_id,
                    COUNT(i.id), bi.created_at, bi.updated_at
             FROM batch_collection_imports bi
             JOIN email_collection_tasks t ON t.id = bi.task_id
             LEFT JOIN batch_collection_import_items i ON i.import_id = bi.id
             WHERE bi.batch_id = ?1
             GROUP BY bi.id, bi.batch_id, bi.task_id, t.name, bi.status,
                      bi.pipeline_id, bi.created_at, bi.updated_at
             ORDER BY bi.id DESC",
        )?;
        let imports = statement
            .query_map(params![batch_id], |row| {
                Ok(BatchCollectionImport {
                    id: row.get(0)?,
                    batch_id: row.get(1)?,
                    task_id: row.get(2)?,
                    task_name: row.get(3)?,
                    status: row.get(4)?,
                    pipeline_id: row.get(5)?,
                    item_count: row.get(6)?,
                    created_at: row.get(7)?,
                    updated_at: row.get(8)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(imports)
    }

    pub fn mark_batch_collection_import_completed(&self, pipeline_id: &str) -> StoreResult<()> {
        self.conn.execute(
            "UPDATE batch_collection_imports SET status = 'completed', updated_at = ?2
             WHERE pipeline_id = ?1 AND status IN ('processing', 'failed')",
            params![pipeline_id, Self::now_text()],
        )?;
        Ok(())
    }

    pub fn mark_batch_collection_import_failed(&self, pipeline_id: &str) -> StoreResult<()> {
        self.conn.execute(
            "UPDATE batch_collection_imports SET status = 'failed', updated_at = ?2
             WHERE pipeline_id = ?1 AND status = 'processing'",
            params![pipeline_id, Self::now_text()],
        )?;
        Ok(())
    }

    fn validate_expense_item_update(update: &ExpenseItemUpdate) -> StoreResult<()> {
        let valid_category = matches!(
            update.category_code.as_str(),
            "rail" | "flight" | "hotel" | "city_transport" | "meal" | "courier_logistics" | "other"
        );
        let valid_payment = matches!(
            update.payment_method.as_str(),
            "unknown" | "personal_card" | "corporate_card" | "cash" | "other"
        );
        let currency = update.currency_code.trim().to_ascii_uppercase();
        if !valid_category {
            return Err(StoreError::Validation("费用类型不受支持".to_string()));
        }
        if !valid_payment {
            return Err(StoreError::Validation("付款方式不受支持".to_string()));
        }
        if update.gross_amount < Decimal::ZERO {
            return Err(StoreError::Validation("实际报销金额不能小于 0".to_string()));
        }
        if currency.len() != 3 || !currency.chars().all(|value| value.is_ascii_uppercase()) {
            return Err(StoreError::Validation(
                "币种必须是 3 位大写字母".to_string(),
            ));
        }
        if update.description.chars().count() > 500 {
            return Err(StoreError::Validation(
                "业务说明不能超过 500 个字符".to_string(),
            ));
        }
        if update.counterparty_name.chars().count() > 200 {
            return Err(StoreError::Validation(
                "交易方名称不能超过 200 个字符".to_string(),
            ));
        }
        if update
            .tax_details
            .iter()
            .any(|tax| tax.amount < Decimal::ZERO)
        {
            return Err(StoreError::Validation("费用税额不能小于 0".to_string()));
        }
        if update
            .tax_details
            .iter()
            .any(|tax| tax.amount > update.gross_amount)
        {
            return Err(StoreError::Validation(
                "费用税额不能大于实际报销金额".to_string(),
            ));
        }
        Ok(())
    }

    fn ensure_batch_draft(connection: &Connection, batch_id: i64) -> StoreResult<()> {
        let status: i32 = connection
            .query_row(
                "SELECT status FROM batches WHERE id = ?1",
                params![batch_id],
                |row| row.get(0),
            )
            .optional()?
            .ok_or_else(|| StoreError::NotFound(format!("Batch {batch_id}")))?;
        if status != BatchStatus::Draft.to_i32() {
            return Err(StoreError::Validation(
                "only draft batches can be reviewed".to_string(),
            ));
        }
        Ok(())
    }

    fn review_snapshot(connection: &Connection, batch_id: i64) -> StoreResult<ReviewSnapshot> {
        let mut invoice_statement = connection.prepare(
            "SELECT id, batch_id, invoice_number, issue_date, amount, tax_amount,
                    buyer_name, seller_name, ticket_type, city, departure_time, checkin_date,
                    file_path, created_at, updated_at, verification_result,
                    is_duplicate, duplicate_reason
             FROM reported_invoices WHERE batch_id = ?1 ORDER BY id",
        )?;
        let invoices = invoice_statement
            .query_map(params![batch_id], Self::parse_invoice_row)?
            .collect::<Result<Vec<_>, _>>()?;
        let mut excluded_statement = connection.prepare(
            "SELECT e.invoice_id
             FROM excluded_invoices e
             JOIN reported_invoices i ON i.id = e.invoice_id
             WHERE i.batch_id = ?1 ORDER BY e.invoice_id",
        )?;
        let excluded_invoice_ids = excluded_statement
            .query_map(params![batch_id], |row| row.get(0))?
            .collect::<Result<Vec<_>, _>>()?;
        let expense_items = Self::expense_items_for_connection(connection, batch_id)?;
        let pending_documents = Self::pending_documents_for_connection(connection, batch_id)?;
        let grouping = Self::grouping_for_connection(connection, batch_id)?;
        Ok(ReviewSnapshot {
            invoices,
            excluded_invoice_ids,
            expense_items,
            pending_documents,
            grouping,
        })
    }

    fn grouping_for_connection(
        connection: &Connection,
        batch_id: i64,
    ) -> StoreResult<Option<BatchGrouping>> {
        let header = connection
            .query_row(
                "SELECT rule_version, home_cities_json, overall_confidence,
                        ambiguities_json, created_at
                 FROM batch_grouping WHERE batch_id = ?1",
                params![batch_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, f32>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                    ))
                },
            )
            .optional()?;
        let Some(header) = header else {
            return Ok(None);
        };
        let mut statement = connection.prepare(
            "SELECT id, group_index, kind, title, start_date, end_date, confidence,
                    requires_review, evidence_json
             FROM invoice_groups WHERE batch_id = ?1 ORDER BY group_index, id",
        )?;
        let rows = statement
            .query_map(params![batch_id], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)? as usize,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, f32>(6)?,
                    row.get::<_, i64>(7)? != 0,
                    row.get::<_, String>(8)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        let mut groups = Vec::with_capacity(rows.len());
        for row in rows {
            let mut members_statement = connection.prepare(
                "SELECT m.invoice_id, i.invoice_number, m.input_index, m.match_reason
                 FROM invoice_group_members m
                 JOIN reported_invoices i ON i.id = m.invoice_id
                 WHERE m.group_id = ?1 ORDER BY m.input_index, m.invoice_id",
            )?;
            let members = members_statement
                .query_map(params![row.0], |member| {
                    Ok(InvoiceGroupMember {
                        invoice_id: member.get(0)?,
                        invoice_number: member.get(1)?,
                        input_index: member.get::<_, i64>(2)? as usize,
                        match_reason: member.get(3)?,
                    })
                })?
                .collect::<Result<Vec<_>, _>>()?;
            groups.push(InvoiceGroup {
                id: row.0,
                group_index: row.1,
                kind: row.2,
                title: row.3,
                start_date: row.4,
                end_date: row.5,
                confidence: row.6,
                requires_review: row.7,
                evidence_json: row.8,
                members,
            });
        }
        Ok(Some(BatchGrouping {
            batch_id,
            rule_version: header.0,
            home_cities_json: header.1,
            overall_confidence: header.2,
            ambiguities_json: header.3,
            created_at: header.4,
            groups,
        }))
    }

    fn restore_review_snapshot(
        connection: &Connection,
        batch_id: i64,
        snapshot: &ReviewSnapshot,
    ) -> StoreResult<()> {
        let current_ids: Vec<i64> = {
            let mut statement = connection
                .prepare("SELECT id FROM reported_invoices WHERE batch_id = ?1 ORDER BY id")?;
            let ids = statement
                .query_map(params![batch_id], |row| row.get(0))?
                .collect::<Result<Vec<_>, _>>()?;
            ids
        };
        let snapshot_ids: Vec<i64> = snapshot.invoices.iter().map(|invoice| invoice.id).collect();
        if current_ids != snapshot_ids {
            return Err(StoreError::Validation(
                "invoice set changed; review snapshot cannot be restored".to_string(),
            ));
        }
        for invoice in &snapshot.invoices {
            connection.execute(
                "UPDATE reported_invoices SET
                    invoice_number = ?2, issue_date = ?3, amount = ?4, tax_amount = ?5,
                    buyer_name = ?6, seller_name = ?7, ticket_type = ?8, city = ?9,
                    departure_time = ?10, checkin_date = ?11, file_path = ?12,
                    created_at = ?13, updated_at = ?14, verification_result = ?15,
                    is_duplicate = ?16, duplicate_reason = ?17
                 WHERE id = ?1 AND batch_id = ?18",
                params![
                    invoice.id,
                    invoice.invoice_number,
                    invoice.issue_date.format("%Y-%m-%d").to_string(),
                    invoice.amount.to_string(),
                    invoice.tax_amount.as_ref().map(ToString::to_string),
                    invoice.buyer_name,
                    invoice.seller_name,
                    invoice.ticket_type.to_str(),
                    invoice.city,
                    invoice
                        .departure_time
                        .as_ref()
                        .map(|value| value.format("%Y-%m-%d %H:%M:%S").to_string()),
                    invoice
                        .checkin_date
                        .as_ref()
                        .map(|value| value.format("%Y-%m-%d").to_string()),
                    invoice.file_path,
                    invoice.created_at.format("%Y-%m-%d %H:%M:%S").to_string(),
                    invoice.updated_at.format("%Y-%m-%d %H:%M:%S").to_string(),
                    invoice.verification_result,
                    if invoice.is_duplicate { 1 } else { 0 },
                    invoice.duplicate_reason,
                    batch_id,
                ],
            )?;
        }
        connection.execute(
            "DELETE FROM excluded_invoices
             WHERE invoice_id IN (
                 SELECT id FROM reported_invoices WHERE batch_id = ?1
             )",
            params![batch_id],
        )?;
        for invoice_id in &snapshot.excluded_invoice_ids {
            connection.execute(
                "INSERT INTO excluded_invoices (invoice_id, reason, excluded_at)
                 VALUES (?1, 'restored_review_snapshot', ?2)",
                params![invoice_id, Self::now_text()],
            )?;
        }
        connection.execute(
            "DELETE FROM batch_grouping WHERE batch_id = ?1",
            params![batch_id],
        )?;
        if let Some(grouping) = &snapshot.grouping {
            connection.execute(
                "INSERT INTO batch_grouping (
                    batch_id, rule_version, home_cities_json, overall_confidence,
                    ambiguities_json, created_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    batch_id,
                    grouping.rule_version,
                    grouping.home_cities_json,
                    grouping.overall_confidence,
                    grouping.ambiguities_json,
                    grouping.created_at,
                ],
            )?;
            for group in &grouping.groups {
                connection.execute(
                    "INSERT INTO invoice_groups (
                        id, batch_id, group_index, kind, title, start_date, end_date,
                        confidence, requires_review, evidence_json
                     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                    params![
                        group.id,
                        batch_id,
                        group.group_index as i64,
                        group.kind,
                        group.title,
                        group.start_date,
                        group.end_date,
                        group.confidence,
                        if group.requires_review { 1 } else { 0 },
                        group.evidence_json,
                    ],
                )?;
                for member in &group.members {
                    connection.execute(
                        "INSERT INTO invoice_group_members (
                            group_id, invoice_id, input_index, match_reason
                         ) VALUES (?1, ?2, ?3, ?4)",
                        params![
                            group.id,
                            member.invoice_id,
                            member.input_index as i64,
                            member.match_reason,
                        ],
                    )?;
                }
            }
        }
        if !snapshot.expense_items.is_empty() {
            let current_expense_ids = {
                let mut statement = connection
                    .prepare("SELECT id FROM expense_items WHERE batch_id = ?1 ORDER BY id")?;
                let ids = statement
                    .query_map(params![batch_id], |row| row.get::<_, i64>(0))?
                    .collect::<Result<Vec<_>, _>>()?;
                ids
            };
            let mut snapshot_expense_ids = snapshot
                .expense_items
                .iter()
                .map(|expense| expense.id)
                .collect::<Vec<_>>();
            snapshot_expense_ids.sort_unstable();
            if current_expense_ids != snapshot_expense_ids {
                return Err(StoreError::Validation(
                    "expense set changed; review snapshot cannot be restored".to_string(),
                ));
            }
            connection.execute(
                "DELETE FROM invoice_documents
                 WHERE expense_item_id IN (
                     SELECT id FROM expense_items WHERE batch_id = ?1
                 )",
                params![batch_id],
            )?;
            for expense in &snapshot.expense_items {
                let location_json = serde_json::to_string(&expense.location).map_err(|error| {
                    StoreError::Internal(format!("serialize expense location: {error}"))
                })?;
                let tax_details_json =
                    serde_json::to_string(&expense.tax_details).map_err(|error| {
                        StoreError::Internal(format!("serialize expense tax: {error}"))
                    })?;
                connection.execute(
                    "UPDATE expense_items SET model_version = ?2, category_code = ?3,
                        transaction_date = ?4, transaction_date_source = ?5,
                        transaction_date_confirmed = ?6, description = ?7,
                        counterparty_name = ?8, location_json = ?9, payment_method = ?10,
                        gross_amount = ?11, currency_code = ?12, tax_details_json = ?13,
                        trip_group_id = ?14, inclusion_status = ?15, provenance_json = ?16,
                        category_source = ?17, category_confirmed = ?18,
                        created_at = ?19, updated_at = ?20
                     WHERE id = ?1 AND batch_id = ?21",
                    params![
                        expense.id,
                        expense.model_version,
                        expense.category_code,
                        expense.transaction_date.format("%Y-%m-%d").to_string(),
                        expense.transaction_date_source,
                        if expense.transaction_date_confirmed {
                            1
                        } else {
                            0
                        },
                        expense.description,
                        expense.counterparty_name,
                        location_json,
                        expense.payment_method,
                        expense.gross_amount.to_string(),
                        expense.currency_code,
                        tax_details_json,
                        expense.trip_group_id,
                        expense.inclusion_status,
                        expense.provenance_json,
                        expense.category_source,
                        if expense.category_confirmed { 1 } else { 0 },
                        expense.created_at,
                        expense.updated_at,
                        batch_id,
                    ],
                )?;
                for document in &expense.documents {
                    connection.execute(
                        "INSERT INTO invoice_documents (
                            id, batch_id, expense_item_id, source_invoice_id,
                            source_pending_document_id, role, file_path, original_name,
                            mime_type, sha256, created_at
                         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                        params![
                            document.id,
                            batch_id,
                            expense.id,
                            document.source_invoice_id,
                            document.source_pending_document_id,
                            document.role,
                            document.file_path,
                            document.original_name,
                            document.mime_type,
                            document.sha256,
                            document.created_at,
                        ],
                    )?;
                }
            }
        }
        if !snapshot.pending_documents.is_empty() {
            let current_pending_ids = {
                let mut statement = connection.prepare(
                    "SELECT id FROM pending_invoice_documents WHERE batch_id = ?1 ORDER BY id",
                )?;
                let ids = statement
                    .query_map(params![batch_id], |row| row.get::<_, i64>(0))?
                    .collect::<Result<Vec<_>, _>>()?;
                ids
            };
            let mut snapshot_pending_ids = snapshot
                .pending_documents
                .iter()
                .map(|document| document.id)
                .collect::<Vec<_>>();
            snapshot_pending_ids.sort_unstable();
            if current_pending_ids != snapshot_pending_ids {
                return Err(StoreError::Validation(
                    "pending document set changed; review snapshot cannot be restored".to_string(),
                ));
            }
            for document in &snapshot.pending_documents {
                connection.execute(
                    "UPDATE pending_invoice_documents SET proposed_role = ?2,
                        status = ?3, assigned_expense_item_id = ?4, updated_at = ?5
                     WHERE id = ?1 AND batch_id = ?6",
                    params![
                        document.id,
                        document.proposed_role,
                        document.status,
                        document.assigned_expense_item_id,
                        document.updated_at,
                        batch_id,
                    ],
                )?;
            }
        }
        Self::update_batch_stats_for_connection(connection, batch_id)
    }

    fn update_batch_stats_for_connection(
        connection: &Connection,
        batch_id: i64,
    ) -> StoreResult<()> {
        let has_expense_items: i64 = connection.query_row(
            "SELECT EXISTS(
                SELECT 1 FROM sqlite_master
                WHERE type = 'table' AND name = 'expense_items'
             )",
            [],
            |row| row.get(0),
        )?;
        let amounts = if has_expense_items != 0 {
            let mut statement = connection.prepare(
                "SELECT gross_amount FROM expense_items
                 WHERE batch_id = ?1 AND inclusion_status = 'included'
                 ORDER BY id",
            )?;
            let collected = statement
                .query_map(params![batch_id], |row| row.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?;
            collected
        } else {
            let mut statement = connection.prepare(
                "SELECT amount FROM reported_invoices i
                 WHERE i.batch_id = ?1
                   AND i.is_duplicate = 0
                   AND NOT EXISTS (
                       SELECT 1 FROM excluded_invoices e WHERE e.invoice_id = i.id
                   )
                 ORDER BY id",
            )?;
            let collected = statement
                .query_map(params![batch_id], |row| row.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?;
            collected
        };
        let total = amounts.iter().try_fold(Decimal::ZERO, |sum, amount| {
            Decimal::from_str(amount)
                .map(|value| sum + value)
                .map_err(|error| StoreError::Internal(format!("invalid stored amount: {error}")))
        })?;
        let invoice_count = i64::try_from(amounts.len()).map_err(|error| {
            StoreError::Internal(format!("invoice count exceeds database range: {error}"))
        })?;
        connection.execute(
            "UPDATE batches SET total_amount = ?2, invoice_count = ?3, updated_at = ?4
             WHERE id = ?1",
            params![batch_id, total.to_string(), invoice_count, Self::now_text()],
        )?;
        Ok(())
    }

    fn snapshot_json(snapshot: &ReviewSnapshot) -> StoreResult<String> {
        serde_json::to_string(snapshot)
            .map_err(|error| StoreError::Internal(format!("serialize review snapshot: {error}")))
    }

    fn reimbursable_snapshot_invoices(snapshot: &ReviewSnapshot) -> Vec<ReportedInvoice> {
        let excluded: HashSet<i64> = snapshot.excluded_invoice_ids.iter().copied().collect();
        snapshot
            .invoices
            .iter()
            .filter(|invoice| !invoice.is_duplicate && !excluded.contains(&invoice.id))
            .cloned()
            .collect()
    }

    fn included_snapshot_expenses(snapshot: &ReviewSnapshot) -> Vec<ExpenseItem> {
        snapshot
            .expense_items
            .iter()
            .filter(|expense| expense.inclusion_status == "included")
            .cloned()
            .collect()
    }

    fn parse_batch_review_snapshot_row(row: &Row<'_>) -> rusqlite::Result<BatchReviewSnapshot> {
        let total_amount_text: String = row.get(5)?;
        let total_amount = Decimal::from_str(&total_amount_text).map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                5,
                rusqlite::types::Type::Text,
                Box::new(error),
            )
        })?;
        Ok(BatchReviewSnapshot {
            id: row.get(0)?,
            batch_id: row.get(1)?,
            version: row.get(2)?,
            content_sha256: row.get(3)?,
            invoice_count: row.get(4)?,
            total_amount,
            created_at: row.get(6)?,
            invalidated_at: row.get(7)?,
        })
    }

    fn parse_delivery_task_row(row: &Row<'_>) -> rusqlite::Result<DeliveryTask> {
        Ok(DeliveryTask {
            id: row.get(0)?,
            batch_id: row.get(1)?,
            review_snapshot_id: row.get(2)?,
            kind: row.get(3)?,
            status: row.get(4)?,
            output_path: row.get(5)?,
            last_error: row.get(6)?,
            created_at: row.get(7)?,
            updated_at: row.get(8)?,
            completed_at: row.get(9)?,
        })
    }

    /// 冻结草稿审核结果。后续交付只能读取该版本，而不能重新读取草稿表。
    pub fn complete_batch_review(&self, batch_id: i64) -> StoreResult<BatchReviewSnapshot> {
        let transaction = self.conn.unchecked_transaction()?;
        Self::ensure_batch_draft(&transaction, batch_id)?;
        Self::update_batch_stats_for_connection(&transaction, batch_id)?;

        let content = Self::review_snapshot(&transaction, batch_id)?;
        let included_expenses = Self::included_snapshot_expenses(&content);
        if included_expenses.is_empty() {
            return Err(StoreError::Validation(
                "batch has no reimbursable invoices".to_string(),
            ));
        }
        if included_expenses
            .iter()
            .any(|expense| !expense.transaction_date_confirmed)
        {
            return Err(StoreError::Validation(
                "batch contains an unconfirmed transaction date".to_string(),
            ));
        }
        if included_expenses
            .iter()
            .any(|expense| !expense.category_confirmed)
        {
            return Err(StoreError::Validation(
                "batch contains an unconfirmed expense category".to_string(),
            ));
        }
        if content
            .pending_documents
            .iter()
            .any(|document| document.status == "pending")
        {
            return Err(StoreError::Validation(
                "batch contains an unresolved pending document".to_string(),
            ));
        }
        // 邮件来源审核属于独立收集任务生命周期。批次审核只校验已经导入的
        // 发票、费用、配套材料和归组，不再被旧版批次内邮件台账阻断。
        if content.grouping.as_ref().is_some_and(|grouping| {
            serde_json::from_str::<Vec<serde_json::Value>>(&grouping.ambiguities_json)
                .map(|ambiguities| !ambiguities.is_empty())
                .unwrap_or(true)
                || grouping.groups.iter().any(|group| group.requires_review)
        }) {
            return Err(StoreError::Validation(
                "batch grouping still requires review".to_string(),
            ));
        }
        if content.grouping.is_none() {
            return Err(StoreError::Validation(
                "batch has no grouping snapshot".to_string(),
            ));
        }
        if Self::business_trip_groups_missing_anchor(&transaction, batch_id)? > 0 {
            return Err(StoreError::Validation(
                "business trip group lacks a transport evidence decision".to_string(),
            ));
        }

        let invoice_count = i32::try_from(included_expenses.len()).map_err(|error| {
            StoreError::Internal(format!("invoice count exceeds supported range: {error}"))
        })?;
        let total_amount = included_expenses
            .iter()
            .fold(Decimal::ZERO, |sum, expense| sum + expense.gross_amount);
        let content_json = Self::snapshot_json(&content)?;
        let content_sha256 = format!("{:x}", Sha256::digest(content_json.as_bytes()));
        let version: i32 = transaction.query_row(
            "SELECT COALESCE(MAX(version), 0) + 1
             FROM batch_review_snapshots WHERE batch_id = ?1",
            params![batch_id],
            |row| row.get(0),
        )?;
        let now = Self::now_text();
        transaction.execute(
            "INSERT INTO batch_review_snapshots (
                batch_id, version, content_json, content_sha256, invoice_count,
                total_amount, created_at, invalidated_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, NULL)",
            params![
                batch_id,
                version,
                content_json,
                content_sha256,
                invoice_count,
                total_amount.to_string(),
                now,
            ],
        )?;
        let snapshot_id = transaction.last_insert_rowid();
        transaction.execute(
            "UPDATE batches
             SET status = ?2, submitted_at = ?3, updated_at = ?3,
                 approved_at = NULL, completed_at = NULL, rejected_at = NULL
             WHERE id = ?1",
            params![batch_id, BatchStatus::Submitted.to_i32(), now],
        )?;
        let snapshot = transaction.query_row(
            "SELECT id, batch_id, version, content_sha256, invoice_count,
                    total_amount, created_at, invalidated_at
             FROM batch_review_snapshots WHERE id = ?1",
            params![snapshot_id],
            Self::parse_batch_review_snapshot_row,
        )?;
        transaction.commit()?;
        Ok(snapshot)
    }

    pub fn get_active_batch_review_snapshot(
        &self,
        batch_id: i64,
    ) -> StoreResult<Option<BatchReviewSnapshot>> {
        self.conn
            .query_row(
                "SELECT id, batch_id, version, content_sha256, invoice_count,
                        total_amount, created_at, invalidated_at
                 FROM batch_review_snapshots
                 WHERE batch_id = ?1 AND invalidated_at IS NULL",
                params![batch_id],
                Self::parse_batch_review_snapshot_row,
            )
            .optional()
            .map_err(Into::into)
    }

    /// 读取活动快照中冻结的可报销发票。
    pub fn get_active_snapshot_invoices(
        &self,
        batch_id: i64,
    ) -> StoreResult<(BatchReviewSnapshot, Vec<ReportedInvoice>)> {
        let snapshot = self
            .get_active_batch_review_snapshot(batch_id)?
            .ok_or_else(|| StoreError::Validation("batch has no active review snapshot".into()))?;
        let content_json: String = self.conn.query_row(
            "SELECT content_json FROM batch_review_snapshots
             WHERE id = ?1 AND batch_id = ?2 AND invalidated_at IS NULL",
            params![snapshot.id, batch_id],
            |row| row.get(0),
        )?;
        let digest = format!("{:x}", Sha256::digest(content_json.as_bytes()));
        if digest != snapshot.content_sha256 {
            return Err(StoreError::Internal(
                "review snapshot digest mismatch".to_string(),
            ));
        }
        let content: ReviewSnapshot = serde_json::from_str(&content_json)
            .map_err(|error| StoreError::Internal(format!("invalid review snapshot: {error}")))?;
        Ok((snapshot, Self::reimbursable_snapshot_invoices(&content)))
    }

    /// 读取活动快照中冻结的稳定本地费用项及其全部材料挂载。
    pub fn get_active_snapshot_expenses(
        &self,
        batch_id: i64,
    ) -> StoreResult<(BatchReviewSnapshot, Vec<ExpenseItem>)> {
        let snapshot = self
            .get_active_batch_review_snapshot(batch_id)?
            .ok_or_else(|| StoreError::Validation("batch has no active review snapshot".into()))?;
        let content_json: String = self.conn.query_row(
            "SELECT content_json FROM batch_review_snapshots
             WHERE id = ?1 AND batch_id = ?2 AND invalidated_at IS NULL",
            params![snapshot.id, batch_id],
            |row| row.get(0),
        )?;
        let digest = format!("{:x}", Sha256::digest(content_json.as_bytes()));
        if digest != snapshot.content_sha256 {
            return Err(StoreError::Internal(
                "review snapshot digest mismatch".to_string(),
            ));
        }
        let content: ReviewSnapshot = serde_json::from_str(&content_json)
            .map_err(|error| StoreError::Internal(format!("invalid review snapshot: {error}")))?;
        if content.expense_items.is_empty() {
            return Err(StoreError::Validation(
                "review snapshot predates stable expense model".to_string(),
            ));
        }
        Ok((snapshot, Self::included_snapshot_expenses(&content)))
    }

    /// 回到可编辑状态。旧快照和交付历史保留，但旧快照不再允许发起新交付。
    pub fn reopen_batch_review(&self, batch_id: i64) -> StoreResult<()> {
        let transaction = self.conn.unchecked_transaction()?;
        let status: i32 = transaction
            .query_row(
                "SELECT status FROM batches WHERE id = ?1",
                params![batch_id],
                |row| row.get(0),
            )
            .optional()?
            .ok_or_else(|| StoreError::NotFound(format!("Batch {batch_id}")))?;
        if status == BatchStatus::Draft.to_i32() {
            return Err(StoreError::Validation(
                "batch review is already open".to_string(),
            ));
        }
        let running_count: i64 = transaction.query_row(
            "SELECT COUNT(*) FROM delivery_tasks
             WHERE batch_id = ?1 AND status = 'running'",
            params![batch_id],
            |row| row.get(0),
        )?;
        if running_count > 0 {
            return Err(StoreError::Validation(
                "batch has a running delivery task".to_string(),
            ));
        }
        let now = Self::now_text();
        transaction.execute(
            "UPDATE batch_review_snapshots SET invalidated_at = ?2
             WHERE batch_id = ?1 AND invalidated_at IS NULL",
            params![batch_id, now],
        )?;
        transaction.execute(
            "UPDATE batches SET status = ?2, updated_at = ?3,
                submitted_at = NULL, approved_at = NULL, completed_at = NULL, rejected_at = NULL
             WHERE id = ?1",
            params![batch_id, BatchStatus::Draft.to_i32(), now],
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn start_delivery_task(&self, batch_id: i64, kind: &str) -> StoreResult<DeliveryTask> {
        if !matches!(kind, "excel" | "pdf" | "concur") {
            return Err(StoreError::Validation("invalid delivery kind".to_string()));
        }
        let snapshot = self
            .get_active_batch_review_snapshot(batch_id)?
            .ok_or_else(|| StoreError::Validation("batch has no active review snapshot".into()))?;
        let now = Self::now_text();
        self.conn.execute(
            "INSERT INTO delivery_tasks (
                batch_id, review_snapshot_id, kind, status, output_path,
                last_error, created_at, updated_at, completed_at
             ) VALUES (?1, ?2, ?3, 'running', NULL, NULL, ?4, ?4, NULL)
             ON CONFLICT(review_snapshot_id, kind) DO UPDATE SET
                status = CASE
                    WHEN delivery_tasks.status = 'succeeded' THEN 'succeeded'
                    ELSE 'running'
                END,
                last_error = CASE
                    WHEN delivery_tasks.status = 'succeeded' THEN delivery_tasks.last_error
                    ELSE NULL
                END,
                updated_at = excluded.updated_at",
            params![batch_id, snapshot.id, kind, now],
        )?;
        self.conn
            .query_row(
                "SELECT id, batch_id, review_snapshot_id, kind, status, output_path,
                    last_error, created_at, updated_at, completed_at
             FROM delivery_tasks WHERE review_snapshot_id = ?1 AND kind = ?2",
                params![snapshot.id, kind],
                Self::parse_delivery_task_row,
            )
            .map_err(Into::into)
    }

    pub fn finish_delivery_task(
        &self,
        task_id: i64,
        output_path: Option<&str>,
        error: Option<&str>,
    ) -> StoreResult<DeliveryTask> {
        let now = Self::now_text();
        let status = if error.is_some() {
            "failed"
        } else {
            "succeeded"
        };
        let changed = self.conn.execute(
            "UPDATE delivery_tasks SET status = ?2, output_path = ?3,
                    last_error = ?4, updated_at = ?5, completed_at = ?5
             WHERE id = ?1",
            params![task_id, status, output_path, error, now],
        )?;
        if changed != 1 {
            return Err(StoreError::NotFound(format!("DeliveryTask {task_id}")));
        }
        self.conn
            .query_row(
                "SELECT id, batch_id, review_snapshot_id, kind, status, output_path,
                    last_error, created_at, updated_at, completed_at
             FROM delivery_tasks WHERE id = ?1",
                params![task_id],
                Self::parse_delivery_task_row,
            )
            .map_err(Into::into)
    }

    pub fn list_delivery_tasks(&self, batch_id: i64) -> StoreResult<Vec<DeliveryTask>> {
        let mut statement = self.conn.prepare(
            "SELECT id, batch_id, review_snapshot_id, kind, status, output_path,
                    last_error, created_at, updated_at, completed_at
             FROM delivery_tasks WHERE batch_id = ?1 ORDER BY id DESC",
        )?;
        let tasks = statement
            .query_map(params![batch_id], Self::parse_delivery_task_row)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(tasks)
    }

    pub fn list_expense_items_by_batch(&self, batch_id: i64) -> StoreResult<Vec<ExpenseItem>> {
        Self::expense_items_for_connection(&self.conn, batch_id)
    }

    pub fn apply_detected_expense_categories_with_audit(
        &self,
        batch_id: i64,
        detections: &[ExpenseCategoryDetection],
    ) -> StoreResult<usize> {
        if detections.is_empty()
            || detections.iter().any(|detection| {
                !matches!(
                    detection.category_code.as_str(),
                    "rail"
                        | "flight"
                        | "hotel"
                        | "city_transport"
                        | "meal"
                        | "courier_logistics"
                        | "other"
                ) || !matches!(
                    detection.source.as_str(),
                    "parser.reanalysis" | "merchant_name.suggestion"
                )
            })
        {
            return Err(StoreError::Validation(
                "invalid expense category detections".to_string(),
            ));
        }
        self.apply_review_mutation(
            batch_id,
            "expense_categories_reanalyzed",
            "重新识别费用类型",
            |transaction| {
                let mut changed = 0usize;
                for detection in detections {
                    let row_changed = transaction.execute(
                        "UPDATE expense_items
                         SET category_code = ?3, category_source = ?4,
                             category_confirmed = ?5, updated_at = ?6
                         WHERE id = ?1 AND batch_id = ?2 AND category_source != 'manual_review'",
                        params![
                            detection.expense_item_id,
                            batch_id,
                            detection.category_code,
                            detection.source,
                            if detection.confirmed { 1 } else { 0 },
                            Self::now_text(),
                        ],
                    )?;
                    if row_changed == 0 {
                        continue;
                    }
                    changed += 1;
                    if detection.category_code == "other" {
                        transaction.execute(
                            "UPDATE reported_invoices SET ticket_type = 'other', updated_at = ?3
                             WHERE batch_id = ?1 AND id = (
                               SELECT primary_invoice_id FROM expense_items WHERE id = ?2
                             )",
                            params![batch_id, detection.expense_item_id, Self::now_text()],
                        )?;
                    } else if detection.confirmed {
                        transaction.execute(
                            "UPDATE reported_invoices SET ticket_type = ?3, updated_at = ?4
                             WHERE batch_id = ?1 AND id = (
                               SELECT primary_invoice_id FROM expense_items WHERE id = ?2
                             ) AND ticket_type = 'other'",
                            params![
                                batch_id,
                                detection.expense_item_id,
                                detection.category_code,
                                Self::now_text(),
                            ],
                        )?;
                    }
                }
                Ok(changed)
            },
        )
    }

    pub fn get_expense_item(&self, expense_item_id: i64) -> StoreResult<Option<ExpenseItem>> {
        let batch_id = self
            .conn
            .query_row(
                "SELECT batch_id FROM expense_items WHERE id = ?1",
                params![expense_item_id],
                |row| row.get::<_, i64>(0),
            )
            .optional()?;
        let Some(batch_id) = batch_id else {
            return Ok(None);
        };
        Ok(Self::expense_items_for_connection(&self.conn, batch_id)?
            .into_iter()
            .find(|expense| expense.id == expense_item_id))
    }

    pub fn update_expense_item_with_audit(
        &self,
        expense_item_id: i64,
        update: &ExpenseItemUpdate,
    ) -> StoreResult<ExpenseItem> {
        Self::validate_expense_item_update(update)?;
        let batch_id: i64 = self
            .conn
            .query_row(
                "SELECT batch_id FROM expense_items WHERE id = ?1",
                params![expense_item_id],
                |row| row.get(0),
            )
            .optional()?
            .ok_or_else(|| StoreError::NotFound(format!("ExpenseItem {expense_item_id}")))?;
        self.apply_review_mutation(
            batch_id,
            "expense_item_updated",
            "修改本地费用字段",
            |transaction| {
                let location_json = serde_json::to_string(&update.location).map_err(|error| {
                    StoreError::Internal(format!("serialize expense location: {error}"))
                })?;
                let tax_details_json =
                    serde_json::to_string(&update.tax_details).map_err(|error| {
                        StoreError::Internal(format!("serialize expense tax: {error}"))
                    })?;
                let provenance_json = serde_json::json!({
                    "reviewed_by": "user",
                    "reviewed_at": Self::now_text(),
                    "category_source": if update.category_confirmed { "manual_review" } else { "unclassified" },
                    "transaction_date_source": "manual_review"
                })
                .to_string();
                let changed = transaction.execute(
                    "UPDATE expense_items SET category_code = ?2,
                        category_source = CASE WHEN ?3 <> 0 THEN 'manual_review' ELSE 'unclassified' END,
                        category_confirmed = ?3, transaction_date = ?4,
                        transaction_date_source = 'manual_review',
                        transaction_date_confirmed = ?5, description = ?6,
                        counterparty_name = ?7, location_json = ?8, payment_method = ?9,
                        gross_amount = ?10, currency_code = ?11, tax_details_json = ?12,
                        provenance_json = ?13, updated_at = ?14
                     WHERE id = ?1 AND batch_id = ?15",
                    params![
                        expense_item_id,
                        update.category_code,
                        if update.category_confirmed { 1 } else { 0 },
                        update.transaction_date.format("%Y-%m-%d").to_string(),
                        if update.transaction_date_confirmed {
                            1
                        } else {
                            0
                        },
                        update.description.trim(),
                        update.counterparty_name.trim(),
                        location_json,
                        update.payment_method,
                        update.gross_amount.to_string(),
                        update.currency_code.trim().to_ascii_uppercase(),
                        tax_details_json,
                        provenance_json,
                        Self::now_text(),
                        batch_id,
                    ],
                )?;
                if changed != 1 {
                    return Err(StoreError::NotFound(format!(
                        "ExpenseItem {expense_item_id}"
                    )));
                }
                Ok(())
            },
        )?;
        Self::expense_items_for_connection(&self.conn, batch_id)?
            .into_iter()
            .find(|expense| expense.id == expense_item_id)
            .ok_or_else(|| StoreError::NotFound(format!("ExpenseItem {expense_item_id}")))
    }

    /// 将行程单/结账单提供的实际日期和城市写回稳定费用模型。
    /// 不改变费用类型、金额或计入状态；用户已经手工确认的日期不会被覆盖。
    pub fn apply_supporting_document_facts_with_audit(
        &self,
        expense_item_id: i64,
        transaction_date: NaiveDate,
        city_name: Option<&str>,
    ) -> StoreResult<()> {
        let city_name = Self::optional_trimmed(city_name);
        if city_name
            .as_ref()
            .is_some_and(|value| value.chars().count() > 100)
        {
            return Err(StoreError::Validation(
                "supporting document city is too long".to_string(),
            ));
        }
        let (batch_id, location_json, date_source): (i64, String, String) = self
            .conn
            .query_row(
                "SELECT batch_id, location_json, transaction_date_source
                 FROM expense_items WHERE id = ?1",
                params![expense_item_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?
            .ok_or_else(|| StoreError::NotFound(format!("ExpenseItem {expense_item_id}")))?;
        if date_source == "manual_review" {
            return Ok(());
        }
        let mut location =
            serde_json::from_str::<ExpenseLocation>(&location_json).unwrap_or_default();
        if city_name.is_some() {
            // 非人工确认字段应以行程单/结账单中的实际发生城市为准；这也会纠正
            // PDF 文本层拆行造成的“周三”被误读为城市“三”等自动解析错误。
            location.city_name = city_name;
        }
        let location_json = serde_json::to_string(&location)
            .map_err(|error| StoreError::Internal(format!("serialize location: {error}")))?;
        self.apply_review_mutation(
            batch_id,
            "supporting_document_facts_applied",
            "采用配套材料中的实际日期和城市",
            |transaction| {
                transaction.execute(
                    "UPDATE expense_items
                     SET transaction_date = ?2,
                         transaction_date_source = 'supporting_document',
                         transaction_date_confirmed = 1,
                         location_json = ?3, updated_at = ?4
                     WHERE id = ?1 AND batch_id = ?5
                       AND transaction_date_source <> 'manual_review'",
                    params![
                        expense_item_id,
                        transaction_date.format("%Y-%m-%d").to_string(),
                        location_json,
                        Self::now_text(),
                        batch_id,
                    ],
                )?;
                Ok(())
            },
        )
    }

    pub fn assign_pending_invoice_document_with_audit(
        &self,
        pending_document_id: i64,
        expense_item_id: i64,
        role: &str,
    ) -> StoreResult<InvoiceDocument> {
        if !matches!(
            role,
            "itinerary" | "detail" | "supporting" | "duplicate_copy"
        ) {
            return Err(StoreError::Validation(
                "invalid pending document role".to_string(),
            ));
        }
        let pending = self
            .conn
            .query_row(
                "SELECT id, batch_id, proposed_role, file_path, original_name,
                        mime_type, sha256, detection_reason, status,
                        assigned_expense_item_id, created_at, updated_at
                 FROM pending_invoice_documents WHERE id = ?1",
                params![pending_document_id],
                Self::parse_pending_invoice_document_row,
            )
            .optional()?
            .ok_or_else(|| {
                StoreError::NotFound(format!("PendingInvoiceDocument {pending_document_id}"))
            })?;
        let target_batch_id: i64 = self
            .conn
            .query_row(
                "SELECT batch_id FROM expense_items WHERE id = ?1",
                params![expense_item_id],
                |row| row.get(0),
            )
            .optional()?
            .ok_or_else(|| StoreError::NotFound(format!("ExpenseItem {expense_item_id}")))?;
        if pending.batch_id != target_batch_id || pending.status != "pending" {
            return Err(StoreError::Validation(
                "pending document is not available for this expense".to_string(),
            ));
        }
        let document_id = self.apply_review_mutation(
            pending.batch_id,
            "pending_document_attached",
            "将待挂载材料归入费用",
            |transaction| {
                transaction.execute(
                    "INSERT INTO invoice_documents (
                        batch_id, expense_item_id, source_invoice_id,
                        source_pending_document_id, role, file_path, original_name,
                        mime_type, sha256, created_at
                     ) VALUES (?1, ?2, NULL, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                    params![
                        pending.batch_id,
                        expense_item_id,
                        pending.id,
                        role,
                        pending.file_path,
                        pending.original_name,
                        pending.mime_type,
                        pending.sha256,
                        Self::now_text(),
                    ],
                )?;
                let document_id = transaction.last_insert_rowid();
                let changed = transaction.execute(
                    "UPDATE pending_invoice_documents
                     SET proposed_role = CASE WHEN ?2 = 'duplicate_copy'
                                              THEN proposed_role ELSE ?2 END,
                         status = 'attached',
                         assigned_expense_item_id = ?3, updated_at = ?4
                     WHERE id = ?1 AND status = 'pending'",
                    params![pending.id, role, expense_item_id, Self::now_text()],
                )?;
                if changed != 1 {
                    return Err(StoreError::Validation(
                        "pending document changed concurrently".to_string(),
                    ));
                }
                Ok(document_id)
            },
        )?;
        Self::documents_for_expense(&self.conn, expense_item_id)?
            .into_iter()
            .find(|document| document.id == document_id)
            .ok_or_else(|| StoreError::NotFound(format!("InvoiceDocument {document_id}")))
    }

    /// Convert a user-confirmed Didi itinerary into an expense aggregate when the tax invoice
    /// exists only on paper. The itinerary remains an itinerary attachment; no electronic main
    /// invoice is invented. If grouping already exists, a review-required group is appended so
    /// the new expense can never silently bypass grouping review.
    pub fn convert_pending_itinerary_to_expense(
        &self,
        pending_document_id: i64,
        invoice: &ReportedInvoice,
        itinerary_end_date: NaiveDate,
    ) -> StoreResult<ExpenseItem> {
        if invoice.ticket_type != TicketType::CityTransport
            || invoice.amount <= Decimal::ZERO
            || !invoice.invoice_number.trim().is_empty()
            || itinerary_end_date < invoice.issue_date
        {
            return Err(StoreError::Validation(
                "invalid itinerary expense fields".to_string(),
            ));
        }

        let transaction = self.conn.unchecked_transaction()?;
        Self::ensure_batch_draft(&transaction, invoice.batch_id)?;
        let pending = transaction
            .query_row(
                "SELECT id, batch_id, proposed_role, file_path, original_name,
                        mime_type, sha256, detection_reason, status,
                        assigned_expense_item_id, created_at, updated_at
                 FROM pending_invoice_documents WHERE id = ?1",
                params![pending_document_id],
                Self::parse_pending_invoice_document_row,
            )
            .optional()?
            .ok_or_else(|| {
                StoreError::NotFound(format!("PendingInvoiceDocument {pending_document_id}"))
            })?;
        if pending.batch_id != invoice.batch_id
            || pending.status != "pending"
            || pending.proposed_role != "itinerary"
            || pending.file_path != invoice.file_path
        {
            return Err(StoreError::Validation(
                "pending itinerary is not available for conversion".to_string(),
            ));
        }
        if let Some(sha256) = pending.sha256.as_deref().filter(|value| !value.is_empty()) {
            let already_used: i64 = transaction.query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM invoice_documents document
                    JOIN expense_items expense ON expense.id = document.expense_item_id
                    WHERE expense.batch_id = ?1 AND document.sha256 = ?2
                 )",
                params![invoice.batch_id, sha256],
                |row| row.get(0),
            )?;
            if already_used != 0 {
                return Err(StoreError::Validation(
                    "itinerary is already attached to an expense".to_string(),
                ));
            }
        }

        let now = Self::now_text();
        transaction.execute(
            "INSERT INTO reported_invoices (
                batch_id, invoice_number, issue_date, amount, tax_amount,
                buyer_name, seller_name, ticket_type, city, departure_time, checkin_date,
                file_path, created_at, updated_at, verification_result,
                is_duplicate, duplicate_reason
             ) VALUES (?1, '', ?2, ?3, NULL, NULL, ?4, 'city_transport', ?5, ?6, NULL,
                       ?7, ?8, ?8, NULL, 0, NULL)",
            params![
                invoice.batch_id,
                invoice.issue_date.format("%Y-%m-%d").to_string(),
                invoice.amount.to_string(),
                invoice.seller_name,
                invoice.city,
                invoice
                    .departure_time
                    .as_ref()
                    .map(|value| value.format("%Y-%m-%d %H:%M:%S").to_string()),
                invoice.file_path,
                now,
            ],
        )?;
        let invoice_id = transaction.last_insert_rowid();
        let expense_item_id = Self::ensure_expense_item_for_invoice(&transaction, invoice_id)?;
        let provenance_json = serde_json::json!({
            "category_code": "supporting_document.didi_itinerary",
            "transaction_date": "itinerary.start_date",
            "counterparty_name": "itinerary.provider",
            "location": "itinerary.city",
            "gross_amount": "itinerary.total_amount",
            "currency_code": "default.CNY",
            "main_invoice": "paper_not_imported"
        })
        .to_string();
        transaction.execute(
            "UPDATE expense_items
             SET category_code = 'city_transport',
                 category_source = 'supporting_document.didi_itinerary',
                 category_confirmed = 1,
                 transaction_date = ?2,
                 transaction_date_source = 'itinerary.start_date',
                 transaction_date_confirmed = 1,
                 description = '滴滴出行行程单（纸质发票未导入）',
                 counterparty_name = '滴滴出行',
                 provenance_json = ?3,
                 updated_at = ?4
             WHERE id = ?1",
            params![
                expense_item_id,
                invoice.issue_date.format("%Y-%m-%d").to_string(),
                provenance_json,
                now,
            ],
        )?;
        let document_changed = transaction.execute(
            "UPDATE invoice_documents
             SET source_pending_document_id = ?3, role = 'itinerary',
                 original_name = ?4, mime_type = ?5, sha256 = ?6
             WHERE batch_id = ?1 AND expense_item_id = ?2
               AND source_invoice_id = ?7 AND role = 'main_invoice'",
            params![
                invoice.batch_id,
                expense_item_id,
                pending.id,
                pending.original_name,
                pending.mime_type,
                pending.sha256,
                invoice_id,
            ],
        )?;
        if document_changed != 1 {
            return Err(StoreError::Validation(
                "itinerary expense attachment was not created".to_string(),
            ));
        }
        let pending_changed = transaction.execute(
            "UPDATE pending_invoice_documents
             SET status = 'attached', assigned_expense_item_id = ?2, updated_at = ?3
             WHERE id = ?1 AND status = 'pending'",
            params![pending.id, expense_item_id, now],
        )?;
        if pending_changed != 1 {
            return Err(StoreError::Validation(
                "pending itinerary changed concurrently".to_string(),
            ));
        }

        let has_grouping: i64 = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM batch_grouping WHERE batch_id = ?1)",
            params![invoice.batch_id],
            |row| row.get(0),
        )?;
        if has_grouping != 0 {
            let group_index: i64 = transaction.query_row(
                "SELECT COALESCE(MAX(group_index), -1) + 1
                 FROM invoice_groups WHERE batch_id = ?1",
                params![invoice.batch_id],
                |row| row.get(0),
            )?;
            let title = format!(
                "待归组 · 滴滴出行 {}",
                invoice.issue_date.format("%Y-%m-%d")
            );
            transaction.execute(
                "INSERT INTO invoice_groups (
                    batch_id, group_index, kind, title, start_date, end_date,
                    confidence, requires_review, evidence_json
                 ) VALUES (?1, ?2, 'needs_review', ?3, ?4, ?5, 0.5, 1, ?6)",
                params![
                    invoice.batch_id,
                    group_index,
                    title,
                    invoice.issue_date.format("%Y-%m-%d").to_string(),
                    itinerary_end_date.format("%Y-%m-%d").to_string(),
                    serde_json::json!({"source": "didi_itinerary_conversion"}).to_string(),
                ],
            )?;
            let group_id = transaction.last_insert_rowid();
            let input_index: i64 = transaction.query_row(
                "SELECT COALESCE(MAX(member.input_index), -1) + 1
                 FROM invoice_group_members member
                 JOIN invoice_groups grouped ON grouped.id = member.group_id
                 WHERE grouped.batch_id = ?1",
                params![invoice.batch_id],
                |row| row.get(0),
            )?;
            transaction.execute(
                "INSERT INTO invoice_group_members (group_id, invoice_id, input_index, match_reason)
                 VALUES (?1, ?2, ?3, '滴滴行程单转费用后待确认归组')",
                params![group_id, invoice_id, input_index],
            )?;
            transaction.execute(
                "UPDATE expense_items SET trip_group_id = ?2, updated_at = ?3 WHERE id = ?1",
                params![expense_item_id, group_id, now],
            )?;
        }

        Self::update_batch_stats_for_connection(&transaction, invoice.batch_id)?;
        transaction.commit()?;
        self.get_expense_item(expense_item_id)?
            .ok_or_else(|| StoreError::NotFound(format!("ExpenseItem {expense_item_id}")))
    }

    pub fn ignore_pending_invoice_document_with_audit(
        &self,
        pending_document_id: i64,
    ) -> StoreResult<()> {
        let batch_id: i64 = self
            .conn
            .query_row(
                "SELECT batch_id FROM pending_invoice_documents
                 WHERE id = ?1 AND status = 'pending'",
                params![pending_document_id],
                |row| row.get(0),
            )
            .optional()?
            .ok_or_else(|| {
                StoreError::NotFound(format!("PendingInvoiceDocument {pending_document_id}"))
            })?;
        self.apply_review_mutation(
            batch_id,
            "pending_document_ignored",
            "确认待挂载文件与报销无关",
            |transaction| {
                let changed = transaction.execute(
                    "UPDATE pending_invoice_documents
                     SET status = 'ignored', assigned_expense_item_id = NULL, updated_at = ?2
                     WHERE id = ?1 AND status = 'pending'",
                    params![pending_document_id, Self::now_text()],
                )?;
                if changed != 1 {
                    return Err(StoreError::Validation(
                        "pending document changed concurrently".to_string(),
                    ));
                }
                Ok(())
            },
        )
    }

    pub fn add_expense_document_with_audit(
        &self,
        expense_item_id: i64,
        role: &str,
        file_path: &str,
        original_name: &str,
        mime_type: Option<&str>,
        sha256: Option<&str>,
    ) -> StoreResult<InvoiceDocument> {
        if !matches!(
            role,
            "itinerary" | "detail" | "supporting" | "duplicate_copy"
        ) || file_path.trim().is_empty()
            || original_name.trim().is_empty()
            || original_name.chars().count() > 255
        {
            return Err(StoreError::Validation(
                "invalid expense document".to_string(),
            ));
        }
        let batch_id: i64 = self
            .conn
            .query_row(
                "SELECT batch_id FROM expense_items WHERE id = ?1",
                params![expense_item_id],
                |row| row.get(0),
            )
            .optional()?
            .ok_or_else(|| StoreError::NotFound(format!("ExpenseItem {expense_item_id}")))?;
        let document_id = self.apply_review_mutation(
            batch_id,
            "expense_document_attached",
            "挂载费用配套材料",
            |transaction| {
                transaction.execute(
                    "INSERT INTO invoice_documents (
                        batch_id, expense_item_id, source_invoice_id, role, file_path,
                        original_name, mime_type, sha256, created_at
                     ) VALUES (?1, ?2, NULL, ?3, ?4, ?5, ?6, ?7, ?8)",
                    params![
                        batch_id,
                        expense_item_id,
                        role,
                        file_path,
                        original_name,
                        mime_type,
                        sha256,
                        Self::now_text(),
                    ],
                )?;
                Ok(transaction.last_insert_rowid())
            },
        )?;
        Self::documents_for_expense(&self.conn, expense_item_id)?
            .into_iter()
            .find(|document| document.id == document_id)
            .ok_or_else(|| StoreError::NotFound(format!("InvoiceDocument {document_id}")))
    }

    pub fn remove_expense_document_with_audit(&self, document_id: i64) -> StoreResult<()> {
        let (batch_id, expense_item_id, role, pending_document_id): (
            i64,
            i64,
            String,
            Option<i64>,
        ) = self
            .conn
            .query_row(
                "SELECT batch_id, expense_item_id, role, source_pending_document_id
                 FROM invoice_documents WHERE id = ?1",
                params![document_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()?
            .ok_or_else(|| StoreError::NotFound(format!("InvoiceDocument {document_id}")))?;
        if role == "main_invoice" {
            return Err(StoreError::Validation(
                "main invoice document cannot be detached".to_string(),
            ));
        }
        self.apply_review_mutation(
            batch_id,
            "expense_document_detached",
            "移除费用配套材料挂载（原文件保留）",
            |transaction| {
                let changed = transaction.execute(
                    "DELETE FROM invoice_documents WHERE id = ?1 AND expense_item_id = ?2",
                    params![document_id, expense_item_id],
                )?;
                if changed != 1 {
                    return Err(StoreError::NotFound(format!(
                        "InvoiceDocument {document_id}"
                    )));
                }
                if let Some(pending_document_id) = pending_document_id {
                    transaction.execute(
                        "UPDATE pending_invoice_documents
                         SET status = 'pending', assigned_expense_item_id = NULL, updated_at = ?2
                         WHERE id = ?1",
                        params![pending_document_id, Self::now_text()],
                    )?;
                }
                Ok(())
            },
        )
    }

    /// 将误解析为独立费用的配套材料重新挂到目标费用，并把来源费用排除出总额。
    /// 原记录和原文件均保留，便于审核、追溯和撤销。
    pub fn reclassify_invoice_as_supporting_document_with_audit(
        &self,
        source_invoice_id: i64,
        target_expense_item_id: i64,
    ) -> StoreResult<InvoiceDocument> {
        let source = self
            .conn
            .query_row(
                "SELECT i.batch_id, d.file_path, d.original_name, d.mime_type, d.sha256, e.id
                 FROM reported_invoices i
                 JOIN expense_items e ON e.primary_invoice_id = i.id
                 JOIN invoice_documents d ON d.expense_item_id = e.id
                    AND d.role = 'main_invoice'
                 WHERE i.id = ?1",
                params![source_invoice_id],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, Option<String>>(3)?,
                        row.get::<_, Option<String>>(4)?,
                        row.get::<_, i64>(5)?,
                    ))
                },
            )
            .optional()?
            .ok_or_else(|| StoreError::NotFound(format!("Invoice {source_invoice_id}")))?;
        let target_batch_id: i64 = self
            .conn
            .query_row(
                "SELECT batch_id FROM expense_items WHERE id = ?1",
                params![target_expense_item_id],
                |row| row.get(0),
            )
            .optional()?
            .ok_or_else(|| StoreError::NotFound(format!("ExpenseItem {target_expense_item_id}")))?;
        if source.0 != target_batch_id || source.5 == target_expense_item_id {
            return Err(StoreError::Validation(
                "source and target must be different expenses in the same batch".to_string(),
            ));
        }

        if self.is_invoice_excluded(source_invoice_id)? {
            if let Some(document) = Self::documents_for_expense(&self.conn, target_expense_item_id)?
                .into_iter()
                .find(|document| {
                    document.source_invoice_id == Some(source_invoice_id)
                        && document.role == "supporting"
                })
            {
                return Ok(document);
            }
        }

        let document_id = self.apply_review_mutation(
            source.0,
            "parsed_document_reclassified_as_supporting",
            "将误解析的配套材料挂到主费用并排除来源费用",
            |transaction| {
                transaction.execute(
                    "INSERT INTO invoice_documents (
                        batch_id, expense_item_id, source_invoice_id, role, file_path,
                        original_name, mime_type, sha256, created_at
                     ) VALUES (?1, ?2, ?3, 'supporting', ?4, ?5, ?6, ?7, ?8)
                     ON CONFLICT(expense_item_id, file_path) DO UPDATE SET
                        source_invoice_id = excluded.source_invoice_id,
                        role = 'supporting'",
                    params![
                        source.0,
                        target_expense_item_id,
                        source_invoice_id,
                        source.1,
                        source.2,
                        source.3,
                        source.4,
                        Self::now_text(),
                    ],
                )?;
                let document_id: i64 = transaction.query_row(
                    "SELECT id FROM invoice_documents
                     WHERE expense_item_id = ?1 AND file_path = ?2",
                    params![target_expense_item_id, source.1],
                    |row| row.get(0),
                )?;
                transaction.execute(
                    "INSERT INTO excluded_invoices (invoice_id, reason, excluded_at)
                     VALUES (?1, 'supporting_document', ?2)
                     ON CONFLICT(invoice_id) DO UPDATE SET
                        reason = 'supporting_document', excluded_at = excluded.excluded_at",
                    params![source_invoice_id, Self::now_text()],
                )?;
                transaction.execute(
                    "UPDATE expense_items SET inclusion_status = 'excluded', updated_at = ?2
                     WHERE primary_invoice_id = ?1",
                    params![source_invoice_id, Self::now_text()],
                )?;
                Ok(document_id)
            },
        )?;
        Self::documents_for_expense(&self.conn, target_expense_item_id)?
            .into_iter()
            .find(|document| document.id == document_id)
            .ok_or_else(|| StoreError::NotFound(format!("InvoiceDocument {document_id}")))
    }

    /// 将疑似重复发票的原件挂到目标费用项，作为重复来源副本。
    /// 来源费用仍保持 `duplicate_suspect`，因此不会被计入批次金额。
    pub fn link_duplicate_invoice_to_expense_with_audit(
        &self,
        source_invoice_id: i64,
        target_expense_item_id: i64,
    ) -> StoreResult<InvoiceDocument> {
        let source = self
            .conn
            .query_row(
                "SELECT i.batch_id, i.is_duplicate, d.file_path, d.original_name,
                        d.mime_type, d.sha256, e.id
                 FROM reported_invoices i
                 JOIN expense_items e ON e.primary_invoice_id = i.id
                 JOIN invoice_documents d ON d.expense_item_id = e.id
                    AND d.role = 'main_invoice'
                 WHERE i.id = ?1",
                params![source_invoice_id],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)? != 0,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, Option<String>>(4)?,
                        row.get::<_, Option<String>>(5)?,
                        row.get::<_, i64>(6)?,
                    ))
                },
            )
            .optional()?
            .ok_or_else(|| StoreError::NotFound(format!("Invoice {source_invoice_id}")))?;
        let target_batch_id: i64 = self
            .conn
            .query_row(
                "SELECT batch_id FROM expense_items WHERE id = ?1",
                params![target_expense_item_id],
                |row| row.get(0),
            )
            .optional()?
            .ok_or_else(|| StoreError::NotFound(format!("ExpenseItem {target_expense_item_id}")))?;
        if !source.1 || source.0 != target_batch_id || source.6 == target_expense_item_id {
            return Err(StoreError::Validation(
                "source must be a duplicate invoice in the same batch".to_string(),
            ));
        }
        let document_id = self.apply_review_mutation(
            source.0,
            "duplicate_document_linked",
            "将疑似重复原件挂载为来源副本",
            |transaction| {
                transaction.execute(
                    "INSERT INTO invoice_documents (
                        batch_id, expense_item_id, source_invoice_id, role, file_path,
                        original_name, mime_type, sha256, created_at
                     ) VALUES (?1, ?2, ?3, 'duplicate_copy', ?4, ?5, ?6, ?7, ?8)",
                    params![
                        source.0,
                        target_expense_item_id,
                        source_invoice_id,
                        source.2,
                        source.3,
                        source.4,
                        source.5,
                        Self::now_text(),
                    ],
                )?;
                Ok(transaction.last_insert_rowid())
            },
        )?;
        Self::documents_for_expense(&self.conn, target_expense_item_id)?
            .into_iter()
            .find(|document| document.id == document_id)
            .ok_or_else(|| StoreError::NotFound(format!("InvoiceDocument {document_id}")))
    }

    fn parse_concur_mapping_profile_row(row: &Row<'_>) -> rusqlite::Result<ConcurMappingProfile> {
        Ok(ConcurMappingProfile {
            id: row.get(0)?,
            name: row.get(1)?,
            company_label: row.get(2)?,
            version: row.get(3)?,
            status: row.get(4)?,
            adapter_kind: row.get(5)?,
            field_rules_json: row.get(6)?,
            expense_type_map_json: row.get(7)?,
            location_map_json: row.get(8)?,
            payment_type_map_json: row.get(9)?,
            vat_rate_map_json: row.get(10)?,
            required_fields_json: row.get(11)?,
            custom_fields_json: row.get(12)?,
            created_at: row.get(13)?,
            updated_at: row.get(14)?,
        })
    }

    pub fn list_concur_mapping_profiles(&self) -> StoreResult<Vec<ConcurMappingProfile>> {
        let mut statement = self.conn.prepare(
            "SELECT id, name, company_label, version, status, adapter_kind,
                    field_rules_json, expense_type_map_json, location_map_json, payment_type_map_json,
                    vat_rate_map_json, required_fields_json, custom_fields_json,
                    created_at, updated_at
             FROM concur_mapping_profiles WHERE status = 'active'
             ORDER BY name, version DESC",
        )?;
        let profiles = statement
            .query_map([], Self::parse_concur_mapping_profile_row)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(profiles)
    }

    fn validate_string_map_json(value: &str, label: &str) -> StoreResult<()> {
        serde_json::from_str::<std::collections::BTreeMap<String, String>>(value)
            .map(|_| ())
            .map_err(|_| StoreError::Validation(format!("{label} must be a string map")))
    }

    pub fn save_concur_mapping_profile(
        &self,
        input: &ConcurMappingProfileInput,
    ) -> StoreResult<ConcurMappingProfile> {
        let name = input.name.trim();
        let company_label = input.company_label.trim();
        if name.is_empty()
            || name.chars().count() > 100
            || company_label.is_empty()
            || company_label.chars().count() > 100
            || !matches!(input.adapter_kind.as_str(), "ui_assisted" | "api")
        {
            return Err(StoreError::Validation(
                "invalid Concur mapping profile".to_string(),
            ));
        }
        Self::validate_string_map_json(&input.expense_type_map_json, "expense type map")?;
        Self::validate_string_map_json(&input.location_map_json, "location map")?;
        Self::validate_string_map_json(&input.payment_type_map_json, "payment type map")?;
        Self::validate_string_map_json(&input.vat_rate_map_json, "VAT rate map")?;
        serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(&input.field_rules_json)
            .map_err(|_| StoreError::Validation("field rules must be an object".to_string()))?;
        serde_json::from_str::<Vec<String>>(&input.required_fields_json)
            .map_err(|_| StoreError::Validation("required fields must be an array".to_string()))?;
        let custom_fields = serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(
            &input.custom_fields_json,
        )
        .map_err(|_| StoreError::Validation("custom fields must be an object".to_string()))?;
        for section in ["report_fields", "expense_fields"] {
            if let Some(value) = custom_fields.get(section) {
                if !value.is_object() {
                    return Err(StoreError::Validation(format!(
                        "custom field section {section} must be an object"
                    )));
                }
            }
        }

        let transaction = self.conn.unchecked_transaction()?;
        if let Some(profile_id) = input.profile_id {
            let changed = transaction.execute(
                "UPDATE concur_mapping_profiles SET status = 'archived', updated_at = ?2
                 WHERE id = ?1 AND status = 'active'",
                params![profile_id, Self::now_text()],
            )?;
            if changed != 1 {
                return Err(StoreError::NotFound(format!(
                    "ConcurMappingProfile {profile_id}"
                )));
            }
        }
        let version: i32 = transaction.query_row(
            "SELECT COALESCE(MAX(version), 0) + 1
             FROM concur_mapping_profiles WHERE name = ?1",
            params![name],
            |row| row.get(0),
        )?;
        let now = Self::now_text();
        transaction.execute(
            "INSERT INTO concur_mapping_profiles (
                name, company_label, version, status, adapter_kind,
                field_rules_json, expense_type_map_json, location_map_json,
                payment_type_map_json, vat_rate_map_json, required_fields_json, custom_fields_json,
                created_at, updated_at
             ) VALUES (?1, ?2, ?3, 'active', ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?12)",
            params![
                name,
                company_label,
                version,
                input.adapter_kind,
                input.field_rules_json,
                input.expense_type_map_json,
                input.location_map_json,
                input.payment_type_map_json,
                input.vat_rate_map_json,
                input.required_fields_json,
                input.custom_fields_json,
                now,
            ],
        )?;
        let id = transaction.last_insert_rowid();
        let profile = transaction.query_row(
            "SELECT id, name, company_label, version, status, adapter_kind,
                    field_rules_json, expense_type_map_json, location_map_json, payment_type_map_json,
                    vat_rate_map_json, required_fields_json, custom_fields_json,
                    created_at, updated_at
             FROM concur_mapping_profiles WHERE id = ?1",
            params![id],
            Self::parse_concur_mapping_profile_row,
        )?;
        transaction.commit()?;
        Ok(profile)
    }

    fn parse_concur_upload_session_row(row: &Row<'_>) -> rusqlite::Result<ConcurUploadSession> {
        let report_date_text: String = row.get(6)?;
        let report_date =
            NaiveDate::parse_from_str(&report_date_text, "%Y-%m-%d").map_err(|error| {
                rusqlite::Error::FromSqlConversionFailure(
                    6,
                    rusqlite::types::Type::Text,
                    Box::new(error),
                )
            })?;
        Ok(ConcurUploadSession {
            id: row.get(0)?,
            batch_id: row.get(1)?,
            review_snapshot_id: row.get(2)?,
            mapping_profile_id: row.get(3)?,
            mapping_profile_version: row.get(4)?,
            report_name: row.get(5)?,
            report_date,
            comment: row.get(7)?,
            status: row.get(8)?,
            idempotency_key: row.get(9)?,
            external_report_id: row.get(10)?,
            upload_overrides_json: row.get(11)?,
            mapped_payload_json: row.get(12)?,
            gaps_json: row.get(13)?,
            last_error: row.get(14)?,
            created_at: row.get(15)?,
            updated_at: row.get(16)?,
        })
    }

    pub fn list_concur_upload_sessions(
        &self,
        batch_id: i64,
    ) -> StoreResult<Vec<ConcurUploadSession>> {
        let mut statement = self.conn.prepare(
            "SELECT id, batch_id, review_snapshot_id, mapping_profile_id,
                    mapping_profile_version, report_name, report_date, comment,
                    status, idempotency_key, external_report_id, upload_overrides_json,
                    mapped_payload_json, gaps_json, last_error, created_at, updated_at
             FROM concur_upload_sessions WHERE batch_id = ?1 ORDER BY id DESC",
        )?;
        let sessions = statement
            .query_map(params![batch_id], Self::parse_concur_upload_session_row)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(sessions)
    }

    pub fn get_concur_upload_status(
        &self,
        session_id: i64,
    ) -> StoreResult<Option<ConcurUploadStatus>> {
        let session = self
            .conn
            .query_row(
                "SELECT id, batch_id, review_snapshot_id, mapping_profile_id,
                        mapping_profile_version, report_name, report_date, comment,
                        status, idempotency_key, external_report_id, upload_overrides_json,
                        mapped_payload_json, gaps_json, last_error, created_at, updated_at
                 FROM concur_upload_sessions WHERE id = ?1",
                params![session_id],
                Self::parse_concur_upload_session_row,
            )
            .optional()?;
        let Some(session) = session else {
            return Ok(None);
        };
        let item_rows = {
            let mut statement = self.conn.prepare(
                "SELECT id, expense_item_id, status, idempotency_key,
                        mapped_payload_json, external_expense_id, attempt_count,
                        last_error, last_verified_at, updated_at
                 FROM concur_upload_items WHERE session_id = ?1 ORDER BY id",
            )?;
            let collected = statement
                .query_map(params![session_id], |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, Option<String>>(5)?,
                        row.get::<_, i32>(6)?,
                        row.get::<_, Option<String>>(7)?,
                        row.get::<_, Option<String>>(8)?,
                        row.get::<_, String>(9)?,
                    ))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            collected
        };
        let mut items = Vec::with_capacity(item_rows.len());
        for row in item_rows {
            let mut statement = self.conn.prepare(
                "SELECT id, document_id, status, idempotency_key,
                        external_attachment_id, attempt_count, last_error,
                        last_verified_at, updated_at
                 FROM concur_upload_attachments WHERE upload_item_id = ?1 ORDER BY id",
            )?;
            let attachments = statement
                .query_map(params![row.0], |attachment| {
                    Ok(ConcurUploadAttachmentState {
                        id: attachment.get(0)?,
                        document_id: attachment.get(1)?,
                        status: attachment.get(2)?,
                        idempotency_key: attachment.get(3)?,
                        external_attachment_id: attachment.get(4)?,
                        attempt_count: attachment.get(5)?,
                        last_error: attachment.get(6)?,
                        last_verified_at: attachment.get(7)?,
                        updated_at: attachment.get(8)?,
                    })
                })?
                .collect::<Result<Vec<_>, _>>()?;
            items.push(ConcurUploadItemState {
                id: row.0,
                expense_item_id: row.1,
                status: row.2,
                idempotency_key: row.3,
                mapped_payload_json: row.4,
                external_expense_id: row.5,
                attempt_count: row.6,
                last_error: row.7,
                last_verified_at: row.8,
                updated_at: row.9,
                attachments,
            });
        }
        Ok(Some(ConcurUploadStatus { session, items }))
    }

    /// External Concur calls are not transactionally coupled to ledger.db. If the process exits
    /// while a call is in flight, a retry could create a duplicate report, expense, or attachment.
    /// Convert every in-flight row to an explicit verification gate on startup instead.
    pub(crate) fn recover_interrupted_concur_uploads(&self) -> StoreResult<()> {
        let now = Self::now_text();
        let reason = "上次 Concur 草稿写入被中断，外部结果未知；请先在 Concur 核对，不能直接重试";
        self.conn.execute(
            "UPDATE concur_upload_attachments
             SET status = 'needs_verification', last_error = ?1, updated_at = ?2
             WHERE status = 'running'",
            params![reason, now],
        )?;
        self.conn.execute(
            "UPDATE concur_upload_items
             SET status = 'needs_verification', last_error = ?1, updated_at = ?2
             WHERE status = 'running'",
            params![reason, now],
        )?;
        self.conn.execute(
            "UPDATE concur_upload_sessions
             SET status = 'needs_verification', last_error = ?1, updated_at = ?2
             WHERE status = 'running'
                OR EXISTS (
                    SELECT 1 FROM concur_upload_items item
                    WHERE item.session_id = concur_upload_sessions.id
                      AND item.status = 'needs_verification'
                )
                OR EXISTS (
                    SELECT 1
                    FROM concur_upload_items item
                    JOIN concur_upload_attachments attachment
                      ON attachment.upload_item_id = item.id
                    WHERE item.session_id = concur_upload_sessions.id
                      AND attachment.status = 'needs_verification'
                )",
            params![reason, now],
        )?;
        Ok(())
    }

    fn validate_external_concur_id(value: &str, label: &str) -> StoreResult<String> {
        let value = value.trim();
        if value.is_empty() || value.chars().count() > 512 || value.chars().any(char::is_control) {
            return Err(StoreError::Validation(format!(
                "invalid external Concur {label} id"
            )));
        }
        Ok(value.to_string())
    }

    fn concur_target_value_present(value: &serde_json::Value) -> bool {
        match value {
            serde_json::Value::Null => false,
            serde_json::Value::String(value) => !value.trim().is_empty(),
            serde_json::Value::Array(value) => !value.is_empty(),
            serde_json::Value::Object(value) => !value.is_empty(),
            serde_json::Value::Bool(_) | serde_json::Value::Number(_) => true,
        }
    }

    fn validate_concur_attempt_error(value: &str) -> StoreResult<String> {
        let value = value.trim();
        if value.is_empty() || value.chars().count() > 2_000 {
            return Err(StoreError::Validation(
                "invalid Concur attempt error".to_string(),
            ));
        }
        Ok(value.to_string())
    }

    fn refresh_concur_upload_session_status(
        connection: &Connection,
        session_id: i64,
    ) -> StoreResult<()> {
        let (current_status, external_report_id): (String, Option<String>) = connection
            .query_row(
                "SELECT status, external_report_id FROM concur_upload_sessions WHERE id = ?1",
                params![session_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?
            .ok_or_else(|| StoreError::NotFound(format!("ConcurUploadSession {session_id}")))?;
        let (item_count, item_running, item_created, item_failed, item_unknown): (
            i64,
            i64,
            i64,
            i64,
            i64,
        ) = connection.query_row(
            "SELECT COUNT(*),
                    SUM(CASE WHEN status = 'running' THEN 1 ELSE 0 END),
                    SUM(CASE WHEN status = 'created' THEN 1 ELSE 0 END),
                    SUM(CASE WHEN status = 'failed' THEN 1 ELSE 0 END),
                    SUM(CASE WHEN status = 'needs_verification' THEN 1 ELSE 0 END)
             FROM concur_upload_items WHERE session_id = ?1",
            params![session_id],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get::<_, Option<i64>>(1)?.unwrap_or(0),
                    row.get::<_, Option<i64>>(2)?.unwrap_or(0),
                    row.get::<_, Option<i64>>(3)?.unwrap_or(0),
                    row.get::<_, Option<i64>>(4)?.unwrap_or(0),
                ))
            },
        )?;
        let (
            attachment_count,
            attachment_running,
            attachment_uploaded,
            attachment_failed,
            attachment_unknown,
        ): (i64, i64, i64, i64, i64) = connection.query_row(
            "SELECT COUNT(*),
                    SUM(CASE WHEN attachment.status = 'running' THEN 1 ELSE 0 END),
                    SUM(CASE WHEN attachment.status = 'uploaded' THEN 1 ELSE 0 END),
                    SUM(CASE WHEN attachment.status = 'failed' THEN 1 ELSE 0 END),
                    SUM(CASE WHEN attachment.status = 'needs_verification' THEN 1 ELSE 0 END)
             FROM concur_upload_items item
             JOIN concur_upload_attachments attachment ON attachment.upload_item_id = item.id
             WHERE item.session_id = ?1",
            params![session_id],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get::<_, Option<i64>>(1)?.unwrap_or(0),
                    row.get::<_, Option<i64>>(2)?.unwrap_or(0),
                    row.get::<_, Option<i64>>(3)?.unwrap_or(0),
                    row.get::<_, Option<i64>>(4)?.unwrap_or(0),
                ))
            },
        )?;

        let next_status = if item_unknown > 0 || attachment_unknown > 0 {
            "needs_verification"
        } else if external_report_id.is_none() {
            match current_status.as_str() {
                "running" => "running",
                "failed" => "failed",
                "needs_verification" => "needs_verification",
                "preflight" => "preflight",
                _ => "ready",
            }
        } else if item_count > 0
            && item_created == item_count
            && attachment_uploaded == attachment_count
        {
            "draft_created"
        } else if item_running > 0 || attachment_running > 0 {
            "running"
        } else if item_failed > 0
            || attachment_failed > 0
            || item_created > 0
            || attachment_uploaded > 0
        {
            "partial"
        } else {
            // The report exists, but no expense has been created yet.
            "partial"
        };
        let now = Self::now_text();
        connection.execute(
            "UPDATE concur_upload_sessions
             SET status = ?2,
                 last_error = CASE WHEN ?2 = 'draft_created' THEN NULL ELSE last_error END,
                 updated_at = ?3
             WHERE id = ?1",
            params![session_id, next_status, now],
        )?;
        Ok(())
    }

    /// Reserve report creation before performing the external request. A `running` row must be
    /// resolved by the startup recovery gate if the process exits before the result is persisted.
    pub fn reserve_concur_report_creation(&self, session_id: i64) -> StoreResult<()> {
        let transaction = self.conn.unchecked_transaction()?;
        let (status, external_report_id): (String, Option<String>) = transaction
            .query_row(
                "SELECT status, external_report_id FROM concur_upload_sessions WHERE id = ?1",
                params![session_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?
            .ok_or_else(|| StoreError::NotFound(format!("ConcurUploadSession {session_id}")))?;
        if external_report_id.is_some() {
            return Ok(());
        }
        if status == "running" {
            return Err(StoreError::Validation(
                "Concur report creation is already running".to_string(),
            ));
        }
        if status == "needs_verification" {
            return Err(StoreError::Validation(
                "Concur report result requires verification before retry".to_string(),
            ));
        }
        if !matches!(status.as_str(), "ready" | "failed") {
            return Err(StoreError::Validation(format!(
                "Concur report cannot be created from status {status}"
            )));
        }
        let changed = transaction.execute(
            "UPDATE concur_upload_sessions
             SET status = 'running', last_error = NULL, updated_at = ?2
             WHERE id = ?1 AND status IN ('ready', 'failed') AND external_report_id IS NULL",
            params![session_id, Self::now_text()],
        )?;
        if changed != 1 {
            return Err(StoreError::Validation(
                "Concur report session changed concurrently".to_string(),
            ));
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn mark_concur_report_created(
        &self,
        session_id: i64,
        external_report_id: &str,
    ) -> StoreResult<()> {
        let external_report_id = Self::validate_external_concur_id(external_report_id, "report")?;
        let transaction = self.conn.unchecked_transaction()?;
        let changed = transaction.execute(
            "UPDATE concur_upload_sessions
             SET external_report_id = ?2, last_error = NULL, updated_at = ?3
             WHERE id = ?1 AND status = 'running' AND external_report_id IS NULL",
            params![session_id, external_report_id, Self::now_text()],
        )?;
        if changed != 1 {
            return Err(StoreError::Validation(
                "Concur report was not reserved for creation".to_string(),
            ));
        }
        Self::refresh_concur_upload_session_status(&transaction, session_id)?;
        transaction.commit()?;
        Ok(())
    }

    pub fn mark_concur_report_attempt_failed(
        &self,
        session_id: i64,
        error: &str,
        result_unknown: bool,
    ) -> StoreResult<()> {
        let error = Self::validate_concur_attempt_error(error)?;
        let status = if result_unknown {
            "needs_verification"
        } else {
            "failed"
        };
        let changed = self.conn.execute(
            "UPDATE concur_upload_sessions
             SET status = ?2, last_error = ?3, updated_at = ?4
             WHERE id = ?1 AND status = 'running' AND external_report_id IS NULL",
            params![session_id, status, error, Self::now_text()],
        )?;
        if changed != 1 {
            return Err(StoreError::Validation(
                "Concur report was not reserved for creation".to_string(),
            ));
        }
        Ok(())
    }

    pub fn reserve_concur_expense_creation(&self, upload_item_id: i64) -> StoreResult<()> {
        let transaction = self.conn.unchecked_transaction()?;
        let (session_id, status, external_expense_id, external_report_id): (
            i64,
            String,
            Option<String>,
            Option<String>,
        ) = transaction
            .query_row(
                "SELECT item.session_id, item.status, item.external_expense_id,
                        session.external_report_id
                 FROM concur_upload_items item
                 JOIN concur_upload_sessions session ON session.id = item.session_id
                 WHERE item.id = ?1",
                params![upload_item_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()?
            .ok_or_else(|| StoreError::NotFound(format!("ConcurUploadItem {upload_item_id}")))?;
        if external_expense_id.is_some() || status == "created" {
            return Ok(());
        }
        if external_report_id.is_none() {
            return Err(StoreError::Validation(
                "Concur report must exist before creating expenses".to_string(),
            ));
        }
        if status == "needs_verification" {
            return Err(StoreError::Validation(
                "Concur expense result requires verification before retry".to_string(),
            ));
        }
        if !matches!(status.as_str(), "pending" | "failed") {
            return Err(StoreError::Validation(format!(
                "Concur expense cannot be created from status {status}"
            )));
        }
        let changed = transaction.execute(
            "UPDATE concur_upload_items
             SET status = 'running', attempt_count = attempt_count + 1,
                 last_error = NULL, updated_at = ?2
             WHERE id = ?1 AND status IN ('pending', 'failed')",
            params![upload_item_id, Self::now_text()],
        )?;
        if changed != 1 {
            return Err(StoreError::Validation(
                "Concur expense item changed concurrently".to_string(),
            ));
        }
        Self::refresh_concur_upload_session_status(&transaction, session_id)?;
        transaction.commit()?;
        Ok(())
    }

    pub fn mark_concur_expense_created(
        &self,
        upload_item_id: i64,
        external_expense_id: &str,
    ) -> StoreResult<()> {
        let external_expense_id =
            Self::validate_external_concur_id(external_expense_id, "expense")?;
        let transaction = self.conn.unchecked_transaction()?;
        let session_id: i64 = transaction
            .query_row(
                "SELECT session_id FROM concur_upload_items WHERE id = ?1",
                params![upload_item_id],
                |row| row.get(0),
            )
            .optional()?
            .ok_or_else(|| StoreError::NotFound(format!("ConcurUploadItem {upload_item_id}")))?;
        let changed = transaction.execute(
            "UPDATE concur_upload_items
             SET status = 'created', external_expense_id = ?2, last_error = NULL,
                 last_verified_at = ?3, updated_at = ?3
             WHERE id = ?1 AND status = 'running' AND external_expense_id IS NULL",
            params![upload_item_id, external_expense_id, Self::now_text()],
        )?;
        if changed != 1 {
            return Err(StoreError::Validation(
                "Concur expense was not reserved for creation".to_string(),
            ));
        }
        Self::refresh_concur_upload_session_status(&transaction, session_id)?;
        transaction.commit()?;
        Ok(())
    }

    pub fn mark_concur_expense_attempt_failed(
        &self,
        upload_item_id: i64,
        error: &str,
        result_unknown: bool,
    ) -> StoreResult<()> {
        let error = Self::validate_concur_attempt_error(error)?;
        let transaction = self.conn.unchecked_transaction()?;
        let session_id: i64 = transaction
            .query_row(
                "SELECT session_id FROM concur_upload_items WHERE id = ?1",
                params![upload_item_id],
                |row| row.get(0),
            )
            .optional()?
            .ok_or_else(|| StoreError::NotFound(format!("ConcurUploadItem {upload_item_id}")))?;
        let status = if result_unknown {
            "needs_verification"
        } else {
            "failed"
        };
        let changed = transaction.execute(
            "UPDATE concur_upload_items
             SET status = ?2, last_error = ?3, updated_at = ?4
             WHERE id = ?1 AND status = 'running'",
            params![upload_item_id, status, error, Self::now_text()],
        )?;
        if changed != 1 {
            return Err(StoreError::Validation(
                "Concur expense was not reserved for creation".to_string(),
            ));
        }
        Self::refresh_concur_upload_session_status(&transaction, session_id)?;
        transaction.commit()?;
        Ok(())
    }

    pub fn reserve_concur_attachment_upload(&self, attachment_id: i64) -> StoreResult<()> {
        let transaction = self.conn.unchecked_transaction()?;
        let (session_id, status, external_attachment_id, external_expense_id): (
            i64,
            String,
            Option<String>,
            Option<String>,
        ) = transaction
            .query_row(
                "SELECT item.session_id, attachment.status, attachment.external_attachment_id,
                        item.external_expense_id
                 FROM concur_upload_attachments attachment
                 JOIN concur_upload_items item ON item.id = attachment.upload_item_id
                 WHERE attachment.id = ?1",
                params![attachment_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()?
            .ok_or_else(|| {
                StoreError::NotFound(format!("ConcurUploadAttachment {attachment_id}"))
            })?;
        if external_attachment_id.is_some() || status == "uploaded" {
            return Ok(());
        }
        if external_expense_id.is_none() {
            return Err(StoreError::Validation(
                "Concur expense must exist before uploading attachments".to_string(),
            ));
        }
        if status == "needs_verification" {
            return Err(StoreError::Validation(
                "Concur attachment result requires verification before retry".to_string(),
            ));
        }
        if !matches!(status.as_str(), "pending" | "failed") {
            return Err(StoreError::Validation(format!(
                "Concur attachment cannot be uploaded from status {status}"
            )));
        }
        let changed = transaction.execute(
            "UPDATE concur_upload_attachments
             SET status = 'running', attempt_count = attempt_count + 1,
                 last_error = NULL, updated_at = ?2
             WHERE id = ?1 AND status IN ('pending', 'failed')",
            params![attachment_id, Self::now_text()],
        )?;
        if changed != 1 {
            return Err(StoreError::Validation(
                "Concur attachment changed concurrently".to_string(),
            ));
        }
        Self::refresh_concur_upload_session_status(&transaction, session_id)?;
        transaction.commit()?;
        Ok(())
    }

    pub fn mark_concur_attachment_uploaded(
        &self,
        attachment_id: i64,
        external_attachment_id: &str,
    ) -> StoreResult<()> {
        let external_attachment_id =
            Self::validate_external_concur_id(external_attachment_id, "attachment")?;
        let transaction = self.conn.unchecked_transaction()?;
        let session_id: i64 = transaction
            .query_row(
                "SELECT item.session_id
                 FROM concur_upload_attachments attachment
                 JOIN concur_upload_items item ON item.id = attachment.upload_item_id
                 WHERE attachment.id = ?1",
                params![attachment_id],
                |row| row.get(0),
            )
            .optional()?
            .ok_or_else(|| {
                StoreError::NotFound(format!("ConcurUploadAttachment {attachment_id}"))
            })?;
        let changed = transaction.execute(
            "UPDATE concur_upload_attachments
             SET status = 'uploaded', external_attachment_id = ?2, last_error = NULL,
                 last_verified_at = ?3, updated_at = ?3
             WHERE id = ?1 AND status = 'running' AND external_attachment_id IS NULL",
            params![attachment_id, external_attachment_id, Self::now_text()],
        )?;
        if changed != 1 {
            return Err(StoreError::Validation(
                "Concur attachment was not reserved for upload".to_string(),
            ));
        }
        Self::refresh_concur_upload_session_status(&transaction, session_id)?;
        transaction.commit()?;
        Ok(())
    }

    pub fn mark_concur_attachment_attempt_failed(
        &self,
        attachment_id: i64,
        error: &str,
        result_unknown: bool,
    ) -> StoreResult<()> {
        let error = Self::validate_concur_attempt_error(error)?;
        let transaction = self.conn.unchecked_transaction()?;
        let session_id: i64 = transaction
            .query_row(
                "SELECT item.session_id
                 FROM concur_upload_attachments attachment
                 JOIN concur_upload_items item ON item.id = attachment.upload_item_id
                 WHERE attachment.id = ?1",
                params![attachment_id],
                |row| row.get(0),
            )
            .optional()?
            .ok_or_else(|| {
                StoreError::NotFound(format!("ConcurUploadAttachment {attachment_id}"))
            })?;
        let status = if result_unknown {
            "needs_verification"
        } else {
            "failed"
        };
        let changed = transaction.execute(
            "UPDATE concur_upload_attachments
             SET status = ?2, last_error = ?3, updated_at = ?4
             WHERE id = ?1 AND status = 'running'",
            params![attachment_id, status, error, Self::now_text()],
        )?;
        if changed != 1 {
            return Err(StoreError::Validation(
                "Concur attachment was not reserved for upload".to_string(),
            ));
        }
        Self::refresh_concur_upload_session_status(&transaction, session_id)?;
        transaction.commit()?;
        Ok(())
    }

    /// Resolve an ambiguous external result only after the user has checked Concur. Supplying an
    /// external id confirms that the object exists; omitting it confirms that creation did not
    /// happen and makes the row safely retryable.
    pub fn resolve_concur_report_verification(
        &self,
        session_id: i64,
        external_report_id: Option<&str>,
    ) -> StoreResult<()> {
        let external_report_id = external_report_id
            .map(|value| Self::validate_external_concur_id(value, "report"))
            .transpose()?;
        let transaction = self.conn.unchecked_transaction()?;
        let persisted_external_report_id: Option<String> = transaction
            .query_row(
                "SELECT external_report_id FROM concur_upload_sessions WHERE id = ?1",
                params![session_id],
                |row| row.get(0),
            )
            .optional()?
            .ok_or_else(|| StoreError::NotFound(format!("ConcurUploadSession {session_id}")))?;
        if persisted_external_report_id.is_some() {
            return Err(StoreError::Validation(
                "the report is known; resolve the ambiguous expense or attachment instead"
                    .to_string(),
            ));
        }
        let started_children: i64 = transaction.query_row(
            "SELECT COUNT(*) FROM concur_upload_items
             WHERE session_id = ?1 AND status NOT IN ('pending', 'failed')",
            params![session_id],
            |row| row.get(0),
        )?;
        if external_report_id.is_none() && started_children > 0 {
            return Err(StoreError::Validation(
                "cannot mark report absent after expense creation has started".to_string(),
            ));
        }
        let now = Self::now_text();
        let (next_status, error) = if external_report_id.is_some() {
            ("partial", None)
        } else {
            (
                "ready",
                Some("用户核对 Concur 后确认报销单未创建，可安全重试"),
            )
        };
        let changed = transaction.execute(
            "UPDATE concur_upload_sessions
             SET status = ?2, external_report_id = ?3, last_error = ?4, updated_at = ?5
             WHERE id = ?1 AND status = 'needs_verification' AND external_report_id IS NULL",
            params![session_id, next_status, external_report_id, error, now],
        )?;
        if changed != 1 {
            return Err(StoreError::Validation(
                "only a Concur report requiring verification can be resolved".to_string(),
            ));
        }
        Self::refresh_concur_upload_session_status(&transaction, session_id)?;
        transaction.commit()?;
        Ok(())
    }

    pub fn resolve_concur_expense_verification(
        &self,
        upload_item_id: i64,
        external_expense_id: Option<&str>,
    ) -> StoreResult<()> {
        let external_expense_id = external_expense_id
            .map(|value| Self::validate_external_concur_id(value, "expense"))
            .transpose()?;
        let transaction = self.conn.unchecked_transaction()?;
        let session_id: i64 = transaction
            .query_row(
                "SELECT session_id FROM concur_upload_items WHERE id = ?1",
                params![upload_item_id],
                |row| row.get(0),
            )
            .optional()?
            .ok_or_else(|| StoreError::NotFound(format!("ConcurUploadItem {upload_item_id}")))?;
        let started_attachments: i64 = transaction.query_row(
            "SELECT COUNT(*) FROM concur_upload_attachments
             WHERE upload_item_id = ?1 AND status NOT IN ('pending', 'failed')",
            params![upload_item_id],
            |row| row.get(0),
        )?;
        if external_expense_id.is_none() && started_attachments > 0 {
            return Err(StoreError::Validation(
                "cannot mark expense absent after attachment upload has started".to_string(),
            ));
        }
        let now = Self::now_text();
        let (next_status, error) = if external_expense_id.is_some() {
            ("created", None)
        } else {
            (
                "failed",
                Some("用户核对 Concur 后确认费用未创建，可安全重试"),
            )
        };
        let changed = transaction.execute(
            "UPDATE concur_upload_items
             SET status = ?2, external_expense_id = ?3, last_error = ?4,
                 last_verified_at = ?5, updated_at = ?5
             WHERE id = ?1 AND status = 'needs_verification'",
            params![upload_item_id, next_status, external_expense_id, error, now],
        )?;
        if changed != 1 {
            return Err(StoreError::Validation(
                "only a Concur expense requiring verification can be resolved".to_string(),
            ));
        }
        Self::refresh_concur_upload_session_status(&transaction, session_id)?;
        transaction.commit()?;
        Ok(())
    }

    pub fn resolve_concur_attachment_verification(
        &self,
        attachment_id: i64,
        external_attachment_id: Option<&str>,
    ) -> StoreResult<()> {
        let external_attachment_id = external_attachment_id
            .map(|value| Self::validate_external_concur_id(value, "attachment"))
            .transpose()?;
        let transaction = self.conn.unchecked_transaction()?;
        let session_id: i64 = transaction
            .query_row(
                "SELECT item.session_id
                 FROM concur_upload_attachments attachment
                 JOIN concur_upload_items item ON item.id = attachment.upload_item_id
                 WHERE attachment.id = ?1",
                params![attachment_id],
                |row| row.get(0),
            )
            .optional()?
            .ok_or_else(|| {
                StoreError::NotFound(format!("ConcurUploadAttachment {attachment_id}"))
            })?;
        let now = Self::now_text();
        let (next_status, error) = if external_attachment_id.is_some() {
            ("uploaded", None)
        } else {
            (
                "failed",
                Some("用户核对 Concur 后确认附件未上传，可安全重试"),
            )
        };
        let changed = transaction.execute(
            "UPDATE concur_upload_attachments
             SET status = ?2, external_attachment_id = ?3, last_error = ?4,
                 last_verified_at = ?5, updated_at = ?5
             WHERE id = ?1 AND status = 'needs_verification'",
            params![
                attachment_id,
                next_status,
                external_attachment_id,
                error,
                now
            ],
        )?;
        if changed != 1 {
            return Err(StoreError::Validation(
                "only a Concur attachment requiring verification can be resolved".to_string(),
            ));
        }
        Self::refresh_concur_upload_session_status(&transaction, session_id)?;
        transaction.commit()?;
        Ok(())
    }

    pub fn prepare_concur_upload(
        &self,
        batch_id: i64,
        profile_id: i64,
        report_name: &str,
        report_date: NaiveDate,
        comment: &str,
        upload_overrides_json: &str,
    ) -> StoreResult<ConcurUploadPreflight> {
        let report_name = report_name.trim();
        if report_name.is_empty()
            || report_name.chars().count() > 200
            || comment.chars().count() > 500
        {
            return Err(StoreError::Validation(
                "invalid Concur report fields".to_string(),
            ));
        }
        let upload_overrides = serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(
            upload_overrides_json,
        )
        .map_err(|_| StoreError::Validation("upload overrides must be an object".to_string()))?;
        let snapshot = self
            .get_active_batch_review_snapshot(batch_id)?
            .ok_or_else(|| StoreError::Validation("batch has no active review snapshot".into()))?;
        let content_json: String = self.conn.query_row(
            "SELECT content_json FROM batch_review_snapshots
             WHERE id = ?1 AND invalidated_at IS NULL",
            params![snapshot.id],
            |row| row.get(0),
        )?;
        let digest = format!("{:x}", Sha256::digest(content_json.as_bytes()));
        if digest != snapshot.content_sha256 {
            return Err(StoreError::Internal(
                "review snapshot digest mismatch".to_string(),
            ));
        }
        let content: ReviewSnapshot = serde_json::from_str(&content_json)
            .map_err(|error| StoreError::Internal(format!("invalid review snapshot: {error}")))?;
        if content.expense_items.is_empty() {
            return Err(StoreError::Validation(
                "review snapshot predates stable expense model; reopen and complete review again"
                    .to_string(),
            ));
        }
        let profile = self
            .conn
            .query_row(
                "SELECT id, name, company_label, version, status, adapter_kind,
                        field_rules_json, expense_type_map_json, location_map_json, payment_type_map_json,
                        vat_rate_map_json, required_fields_json, custom_fields_json,
                        created_at, updated_at
                 FROM concur_mapping_profiles WHERE id = ?1 AND status = 'active'",
                params![profile_id],
                Self::parse_concur_mapping_profile_row,
            )
            .optional()?
            .ok_or_else(|| StoreError::NotFound(format!("ConcurMappingProfile {profile_id}")))?;
        let expense_type_map = serde_json::from_str::<std::collections::BTreeMap<String, String>>(
            &profile.expense_type_map_json,
        )
        .map_err(|error| StoreError::Internal(format!("invalid expense type map: {error}")))?;
        let location_map = serde_json::from_str::<std::collections::BTreeMap<String, String>>(
            &profile.location_map_json,
        )
        .map_err(|error| StoreError::Internal(format!("invalid location map: {error}")))?;
        let payment_map = serde_json::from_str::<std::collections::BTreeMap<String, String>>(
            &profile.payment_type_map_json,
        )
        .map_err(|error| StoreError::Internal(format!("invalid payment map: {error}")))?;
        let vat_rate_map = serde_json::from_str::<std::collections::BTreeMap<String, String>>(
            &profile.vat_rate_map_json,
        )
        .map_err(|error| StoreError::Internal(format!("invalid VAT rate map: {error}")))?;
        let configured_required = serde_json::from_str::<Vec<String>>(
            &profile.required_fields_json,
        )
        .map_err(|error| StoreError::Internal(format!("invalid required fields: {error}")))?;
        let custom_fields = serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(
            &profile.custom_fields_json,
        )
        .map_err(|error| StoreError::Internal(format!("invalid custom fields: {error}")))?;
        let report_custom_fields = custom_fields
            .get("report_fields")
            .and_then(serde_json::Value::as_object)
            .cloned()
            .unwrap_or_default();
        let expense_custom_fields = custom_fields
            .get("expense_fields")
            .and_then(serde_json::Value::as_object)
            .cloned()
            .unwrap_or_else(|| {
                // v10 profiles stored one flat object. Preserve those values as expense-level
                // target constants when the new explicit sections are absent.
                if custom_fields.contains_key("report_fields") {
                    serde_json::Map::new()
                } else {
                    custom_fields.clone()
                }
            });
        let mut required_fields: HashSet<String> = [
            "expense_type",
            "transaction_date",
            "vendor_name",
            "purchase_city",
            "amount",
            "vat_amount",
            "vat_rate",
        ]
        .into_iter()
        .map(str::to_string)
        .collect();
        let mut required_report_fields = Vec::new();
        for field in configured_required {
            if let Some(field) = field.strip_prefix("report.") {
                required_report_fields.push(field.to_string());
            } else {
                required_fields.insert(field);
            }
        }

        let included = content
            .expense_items
            .iter()
            .filter(|expense| expense.inclusion_status == "included")
            .collect::<Vec<_>>();
        if included.is_empty() {
            return Err(StoreError::Validation(
                "review snapshot has no included expenses".to_string(),
            ));
        }
        let mut gaps = Vec::new();
        let mut report_fields = report_custom_fields;
        report_fields.insert("name".into(), report_name.into());
        report_fields.insert(
            "date".into(),
            report_date.format("%Y-%m-%d").to_string().into(),
        );
        report_fields.insert("comment".into(), comment.into());
        for field in required_report_fields {
            let present = report_fields
                .get(&field)
                .map(Self::concur_target_value_present)
                .unwrap_or(false);
            if !present {
                gaps.push(ConcurMappingGap {
                    scope: "target_override".into(),
                    expense_item_id: None,
                    field_key: format!("report.{field}"),
                    message: format!("报销单必填目标字段 {field} 尚未配置"),
                    resolution: "configure_profile".into(),
                });
            }
        }
        let mut expenses = Vec::with_capacity(included.len());
        for expense in included {
            let mut fields = expense_custom_fields.clone();
            let expense_key = expense.id.to_string();
            let override_fields = upload_overrides
                .get(&expense_key)
                .and_then(serde_json::Value::as_object);
            let override_value =
                |key: &str| override_fields.and_then(|values| values.get(key)).cloned();
            if let Some(value) = expense_type_map.get(&expense.category_code) {
                fields.insert("expense_type_id".into(), value.clone().into());
            } else if let Some(value) = override_value("expense_type_id") {
                fields.insert("expense_type_id".into(), value);
            } else {
                gaps.push(ConcurMappingGap {
                    scope: "mapping_profile".into(),
                    expense_item_id: Some(expense.id),
                    field_key: "expense_type".into(),
                    message: format!(
                        "费用分类 {} 尚未映射到 Concur 费用类型",
                        expense.category_code
                    ),
                    resolution: "configure_profile".into(),
                });
            }
            if !expense.category_confirmed {
                gaps.push(ConcurMappingGap {
                    scope: "expense_fact".into(),
                    expense_item_id: Some(expense.id),
                    field_key: "expense_type".into(),
                    message: "费用类型尚未由高置信规则或用户确认".into(),
                    resolution: "return_to_expense_review".into(),
                });
            }
            fields.insert(
                "transaction_date".into(),
                expense
                    .transaction_date
                    .format("%Y-%m-%d")
                    .to_string()
                    .into(),
            );
            if !expense.transaction_date_confirmed {
                gaps.push(ConcurMappingGap {
                    scope: "expense_fact".into(),
                    expense_item_id: Some(expense.id),
                    field_key: "transaction_date".into(),
                    message: "实际发生日期仍是开票日期候选，尚未由用户确认".into(),
                    resolution: "return_to_expense_review".into(),
                });
            }
            if expense.counterparty_name.trim().is_empty() {
                gaps.push(ConcurMappingGap {
                    scope: "expense_fact".into(),
                    expense_item_id: Some(expense.id),
                    field_key: "vendor_name".into(),
                    message: "交易对方为空".into(),
                    resolution: "return_to_expense_review".into(),
                });
            } else {
                fields.insert(
                    "vendor_name".into(),
                    expense.counterparty_name.clone().into(),
                );
            }
            if let Some(city) = expense.location.city_name.as_deref() {
                if let Some(value) = location_map.get(city) {
                    fields.insert("purchase_city_id".into(), value.clone().into());
                } else if let Some(value) = override_value("purchase_city_id") {
                    fields.insert("purchase_city_id".into(), value);
                } else {
                    gaps.push(ConcurMappingGap {
                        scope: "mapping_profile".into(),
                        expense_item_id: Some(expense.id),
                        field_key: "purchase_city".into(),
                        message: format!("城市 {city} 尚未映射到 Concur 地点选项"),
                        resolution: "configure_profile".into(),
                    });
                }
            } else {
                gaps.push(ConcurMappingGap {
                    scope: "expense_fact".into(),
                    expense_item_id: Some(expense.id),
                    field_key: "purchase_city".into(),
                    message: "费用地点缺少城市".into(),
                    resolution: "return_to_expense_review".into(),
                });
            }
            fields.insert("amount".into(), expense.gross_amount.to_string().into());
            fields.insert("currency".into(), expense.currency_code.clone().into());
            if !expense.description.trim().is_empty() {
                fields.insert(
                    "business_purpose".into(),
                    expense.description.clone().into(),
                );
            }
            if let Some(value) = payment_map.get(&expense.payment_method) {
                fields.insert("payment_type_id".into(), value.clone().into());
            } else if let Some(value) = override_value("payment_type_id") {
                fields.insert("payment_type_id".into(), value);
            } else if required_fields.contains("payment_type") {
                gaps.push(ConcurMappingGap {
                    scope: "mapping_profile".into(),
                    expense_item_id: Some(expense.id),
                    field_key: "payment_type".into(),
                    message: "付款方式尚未映射到 Concur 选项".into(),
                    resolution: "configure_profile".into(),
                });
            }
            if expense.tax_details.is_empty() {
                if let Some(value) = override_value("vat_amount") {
                    fields.insert("vat_amount".into(), value);
                }
                if let Some(value) = override_value("vat_rate_ids") {
                    fields.insert("vat_rate_ids".into(), value);
                }
                if (required_fields.contains("vat_amount") && !fields.contains_key("vat_amount"))
                    || (required_fields.contains("vat_rate")
                        && !fields.contains_key("vat_rate_ids"))
                {
                    gaps.push(ConcurMappingGap {
                        scope: "target_override".into(),
                        expense_item_id: Some(expense.id),
                        field_key: "vat_amount".into(),
                        message: "票面税项缺失，不能无依据填 0".into(),
                        resolution: "return_to_expense_review_or_override".into(),
                    });
                }
            } else {
                let total_tax = expense
                    .tax_details
                    .iter()
                    .fold(Decimal::ZERO, |sum, tax| sum + tax.amount);
                fields.insert("vat_amount".into(), total_tax.to_string().into());
                let mut rate_ids = Vec::new();
                for tax in &expense.tax_details {
                    if let Some(rate) = tax.rate {
                        let key = rate.normalize().to_string();
                        if let Some(value) = vat_rate_map.get(&key) {
                            rate_ids.push(value.clone());
                        } else if let Some(value) = override_value("vat_rate_ids") {
                            fields.insert("vat_rate_ids".into(), value);
                        } else if required_fields.contains("vat_rate") {
                            gaps.push(ConcurMappingGap {
                                scope: "mapping_profile".into(),
                                expense_item_id: Some(expense.id),
                                field_key: "vat_rate".into(),
                                message: format!("税率 {key} 尚未映射到 Concur VAT 选项"),
                                resolution: "configure_profile".into(),
                            });
                        }
                    } else if required_fields.contains("vat_rate") {
                        gaps.push(ConcurMappingGap {
                            scope: "expense_fact".into(),
                            expense_item_id: Some(expense.id),
                            field_key: "vat_rate".into(),
                            message: "票面税率缺失".into(),
                            resolution: "return_to_expense_review_or_override".into(),
                        });
                    }
                }
                if !rate_ids.is_empty() {
                    fields.insert("vat_rate_ids".into(), serde_json::json!(rate_ids));
                }
            }
            if let Some(overrides) = override_fields {
                for (key, value) in overrides {
                    fields.insert(key.clone(), value.clone());
                }
            }
            for required_field in &required_fields {
                let target_key = match required_field.as_str() {
                    "expense_type" => "expense_type_id",
                    "purchase_city" => "purchase_city_id",
                    "payment_type" => "payment_type_id",
                    "vat_rate" => "vat_rate_ids",
                    value => value,
                };
                let present = fields
                    .get(target_key)
                    .map(Self::concur_target_value_present)
                    .unwrap_or(false);
                let already_reported = gaps.iter().any(|gap| {
                    gap.expense_item_id == Some(expense.id)
                        && gap.field_key == required_field.as_str()
                });
                if !present && !already_reported {
                    gaps.push(ConcurMappingGap {
                        scope: "target_override".into(),
                        expense_item_id: Some(expense.id),
                        field_key: required_field.clone(),
                        message: format!("Concur 必填目标字段 {required_field} 缺少值"),
                        resolution: "configure_profile_or_override".into(),
                    });
                }
            }
            if !expense
                .documents
                .iter()
                .any(|document| document.role == "main_invoice")
            {
                gaps.push(ConcurMappingGap {
                    scope: "attachment".into(),
                    expense_item_id: Some(expense.id),
                    field_key: "receipt_attachment".into(),
                    message: "缺少主发票原件".into(),
                    resolution: "return_to_expense_review".into(),
                });
            }
            expenses.push(MappedExpensePayload {
                expense_item_id: expense.id,
                target_fields_json: serde_json::Value::Object(fields).to_string(),
                attachment_document_ids: expense
                    .documents
                    .iter()
                    .map(|document| document.id)
                    .collect(),
            });
        }
        let payload_json = serde_json::json!({
            "report": report_fields,
            "expenses": expenses,
            "mapping_profile": {
                "id": profile.id,
                "version": profile.version,
                "adapter_kind": profile.adapter_kind
            },
            "upload_overrides": upload_overrides
        })
        .to_string();
        let gaps_json = serde_json::to_string(&gaps)
            .map_err(|error| StoreError::Internal(format!("serialize Concur gaps: {error}")))?;
        let idempotency_material = format!(
            "{}|{}|{}|{}|{}|{}|{}",
            snapshot.content_sha256,
            profile.id,
            profile.version,
            report_name,
            report_date.format("%Y-%m-%d"),
            comment,
            upload_overrides_json
        );
        let idempotency_key = format!(
            "concur-upload-{:x}",
            Sha256::digest(idempotency_material.as_bytes())
        );
        let status = if gaps.is_empty() {
            "ready"
        } else {
            "preflight"
        };
        let transaction = self.conn.unchecked_transaction()?;
        transaction.execute(
            "INSERT INTO concur_upload_sessions (
                batch_id, review_snapshot_id, mapping_profile_id, mapping_profile_version,
                report_name, report_date, comment, status, idempotency_key,
                external_report_id, upload_overrides_json, mapped_payload_json,
                gaps_json, last_error,
                created_at, updated_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, NULL, ?10, ?11,
                       ?12, NULL, ?13, ?13)
             ON CONFLICT(idempotency_key) DO UPDATE SET
                upload_overrides_json = excluded.upload_overrides_json,
                mapped_payload_json = excluded.mapped_payload_json,
                gaps_json = excluded.gaps_json,
                status = CASE
                    WHEN concur_upload_sessions.status IN ('preflight', 'ready', 'failed')
                    THEN excluded.status ELSE concur_upload_sessions.status END,
                last_error = CASE
                    WHEN concur_upload_sessions.status IN ('preflight', 'ready', 'failed')
                    THEN NULL ELSE concur_upload_sessions.last_error END,
                updated_at = excluded.updated_at",
            params![
                batch_id,
                snapshot.id,
                profile.id,
                profile.version,
                report_name,
                report_date.format("%Y-%m-%d").to_string(),
                comment,
                status,
                idempotency_key,
                upload_overrides_json,
                payload_json,
                gaps_json,
                Self::now_text(),
            ],
        )?;
        let session = transaction.query_row(
            "SELECT id, batch_id, review_snapshot_id, mapping_profile_id,
                    mapping_profile_version, report_name, report_date, comment,
                    status, idempotency_key, external_report_id, upload_overrides_json,
                    mapped_payload_json, gaps_json, last_error, created_at, updated_at
             FROM concur_upload_sessions WHERE idempotency_key = ?1",
            params![idempotency_key],
            Self::parse_concur_upload_session_row,
        )?;
        if matches!(session.status.as_str(), "preflight" | "ready" | "failed") {
            transaction.execute(
                "DELETE FROM concur_upload_items WHERE session_id = ?1",
                params![session.id],
            )?;
            for expense in &expenses {
                let item_key = format!(
                    "{}-expense-{}",
                    session.idempotency_key, expense.expense_item_id
                );
                transaction.execute(
                    "INSERT INTO concur_upload_items (
                        session_id, expense_item_id, status, idempotency_key,
                        mapped_payload_json, external_expense_id, attempt_count,
                        last_error, last_verified_at, updated_at
                     ) VALUES (?1, ?2, 'pending', ?3, ?4, NULL, 0, NULL, NULL, ?5)",
                    params![
                        session.id,
                        expense.expense_item_id,
                        item_key,
                        expense.target_fields_json,
                        Self::now_text(),
                    ],
                )?;
                let upload_item_id = transaction.last_insert_rowid();
                for document_id in &expense.attachment_document_ids {
                    let attachment_key =
                        format!("{}-document-{document_id}", session.idempotency_key);
                    transaction.execute(
                        "INSERT INTO concur_upload_attachments (
                            upload_item_id, document_id, status, idempotency_key,
                            external_attachment_id, attempt_count, last_error,
                            last_verified_at, updated_at
                         ) VALUES (?1, ?2, 'pending', ?3, NULL, 0, NULL, NULL, ?4)",
                        params![
                            upload_item_id,
                            document_id,
                            attachment_key,
                            Self::now_text(),
                        ],
                    )?;
                }
            }
        }
        transaction.commit()?;
        Ok(ConcurUploadPreflight {
            ready: gaps.is_empty(),
            session,
            expenses,
            gaps,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_and_get_batch() {
        let db = LedgerDb::new(":memory:").unwrap();

        let id = db.create_batch("2026年7月出差", "2026-07").unwrap();
        let batch = db.get_batch(id).unwrap();

        assert_eq!(batch.name, "2026年7月出差");
        assert_eq!(batch.month, "2026-07");
        assert_eq!(batch.status, BatchStatus::Draft);
        assert_eq!(batch.invoice_count, 0);
    }

    #[test]
    fn update_batch_status() {
        let db = LedgerDb::new(":memory:").unwrap();

        let id = db.create_batch("测试批次", "2026-07").unwrap();
        db.transition_batch_status(id, BatchStatus::Submitted)
            .unwrap();

        let batch = db.get_batch(id).unwrap();
        assert_eq!(batch.status, BatchStatus::Submitted);
        assert!(batch.submitted_at.is_some());
    }

    #[test]
    fn add_invoice_updates_batch_stats() {
        let db = LedgerDb::new(":memory:").unwrap();

        let batch_id = db.create_batch("测试批次", "2026-07").unwrap();

        let invoice = ReportedInvoice {
            id: 0,
            batch_id,
            invoice_number: "12345678901234567890".to_string(),
            issue_date: NaiveDate::from_ymd_opt(2026, 7, 15).unwrap(),
            amount: Decimal::from_str("100.50").unwrap(),
            tax_amount: None,
            buyer_name: None,
            seller_name: Some("测试商家".to_string()),
            ticket_type: TicketType::Rail,
            city: Some("北京".to_string()),
            departure_time: None,
            checkin_date: None,
            file_path: "/path/to/invoice.xml".to_string(),
            created_at: Utc::now().naive_utc(),
            updated_at: Utc::now().naive_utc(),
            verification_result: None,
            is_duplicate: false,
            duplicate_reason: None,
        };

        db.add_invoice(&invoice).unwrap();

        let batch = db.get_batch(batch_id).unwrap();
        assert_eq!(batch.invoice_count, 1);
        assert_eq!(batch.total_amount, Decimal::from_str("100.50").unwrap());
    }

    #[test]
    fn list_invoices_by_batch() {
        let db = LedgerDb::new(":memory:").unwrap();

        let batch_id = db.create_batch("测试批次", "2026-07").unwrap();

        let invoice1 = ReportedInvoice {
            id: 0,
            batch_id,
            invoice_number: "11111111111111111111".to_string(),
            issue_date: NaiveDate::from_ymd_opt(2026, 7, 10).unwrap(),
            amount: Decimal::from_str("50.00").unwrap(),
            tax_amount: None,
            buyer_name: None,
            seller_name: None,
            ticket_type: TicketType::Meal,
            city: None,
            departure_time: None,
            checkin_date: None,
            file_path: "/path/to/invoice1.xml".to_string(),
            created_at: Utc::now().naive_utc(),
            updated_at: Utc::now().naive_utc(),
            verification_result: None,
            is_duplicate: false,
            duplicate_reason: None,
        };

        let invoice2 = ReportedInvoice {
            id: 0,
            batch_id,
            invoice_number: "22222222222222222222".to_string(),
            issue_date: NaiveDate::from_ymd_opt(2026, 7, 15).unwrap(),
            amount: Decimal::from_str("150.00").unwrap(),
            tax_amount: None,
            buyer_name: None,
            seller_name: None,
            ticket_type: TicketType::Hotel,
            city: None,
            departure_time: None,
            checkin_date: None,
            file_path: "/path/to/invoice2.xml".to_string(),
            created_at: Utc::now().naive_utc(),
            updated_at: Utc::now().naive_utc(),
            verification_result: None,
            is_duplicate: false,
            duplicate_reason: None,
        };

        db.add_invoice(&invoice1).unwrap();
        db.add_invoice(&invoice2).unwrap();

        let invoices = db.list_invoices_by_batch(batch_id).unwrap();
        assert_eq!(invoices.len(), 2);
    }

    #[test]
    fn delete_invoice_updates_stats() {
        let db = LedgerDb::new(":memory:").unwrap();

        let batch_id = db.create_batch("测试批次", "2026-07").unwrap();

        let invoice = ReportedInvoice {
            id: 0,
            batch_id,
            invoice_number: "12345678901234567890".to_string(),
            issue_date: NaiveDate::from_ymd_opt(2026, 7, 15).unwrap(),
            amount: Decimal::from_str("100.00").unwrap(),
            tax_amount: None,
            buyer_name: None,
            seller_name: None,
            ticket_type: TicketType::Rail,
            city: None,
            departure_time: None,
            checkin_date: None,
            file_path: "/path/to/invoice.xml".to_string(),
            created_at: Utc::now().naive_utc(),
            updated_at: Utc::now().naive_utc(),
            verification_result: None,
            is_duplicate: false,
            duplicate_reason: None,
        };

        let invoice_id = db.add_invoice(&invoice).unwrap();
        db.delete_invoice(invoice_id).unwrap();

        let batch = db.get_batch(batch_id).unwrap();
        assert_eq!(batch.invoice_count, 0);
    }

    #[test]
    fn find_invoice_by_number_returns_existing_record() {
        let db = LedgerDb::new(":memory:").unwrap();
        let batch_id = db.create_batch("测试批次", "2026-07").unwrap();

        let invoice = ReportedInvoice {
            id: 0,
            batch_id,
            invoice_number: "12345678901234567890".to_string(),
            issue_date: NaiveDate::from_ymd_opt(2026, 7, 15).unwrap(),
            amount: Decimal::from_str("88.80").unwrap(),
            tax_amount: None,
            buyer_name: None,
            seller_name: None,
            ticket_type: TicketType::Flight,
            city: None,
            departure_time: None,
            checkin_date: None,
            file_path: "/path/to/invoice.xml".to_string(),
            created_at: Utc::now().naive_utc(),
            updated_at: Utc::now().naive_utc(),
            verification_result: None,
            is_duplicate: false,
            duplicate_reason: None,
        };

        let invoice_id = db.add_invoice(&invoice).unwrap();

        let found = db.find_invoice_by_number("12345678901234567890").unwrap();
        let found = found.expect("应找到已入库的发票");

        assert_eq!(found.id, invoice_id);
        assert_eq!(found.batch_id, batch_id);
        assert_eq!(found.amount, Decimal::from_str("88.80").unwrap());
        assert_eq!(found.ticket_type, TicketType::Flight);
    }

    #[test]
    fn find_invoice_by_number_returns_none_when_absent() {
        let db = LedgerDb::new(":memory:").unwrap();
        db.create_batch("测试批次", "2026-07").unwrap();

        // 无行时必须是 Ok(None)，不能是 Err(QueryReturnedNoRows)
        let result = db.find_invoice_by_number("00000000000000000000");
        assert!(result.is_ok(), "查不到记录不应返回错误");
        assert!(result.unwrap().is_none());
    }

    // ========== 状态转换测试 ==========

    #[test]
    fn test_transition_draft_to_submitted() {
        let db = LedgerDb::new(":memory:").unwrap();
        let id = db.create_batch("测试批次", "2026-07").unwrap();

        db.transition_batch_status(id, BatchStatus::Submitted)
            .unwrap();

        let batch = db.get_batch(id).unwrap();
        assert_eq!(batch.status, BatchStatus::Submitted);
        assert!(batch.submitted_at.is_some());
        assert!(batch.approved_at.is_none());
        assert!(batch.completed_at.is_none());
        assert!(batch.rejected_at.is_none());
    }

    #[test]
    fn test_transition_draft_to_rejected() {
        let db = LedgerDb::new(":memory:").unwrap();
        let id = db.create_batch("测试批次", "2026-07").unwrap();

        db.transition_batch_status(id, BatchStatus::Rejected)
            .unwrap();

        let batch = db.get_batch(id).unwrap();
        assert_eq!(batch.status, BatchStatus::Rejected);
        assert!(batch.rejected_at.is_some());
        assert!(batch.submitted_at.is_none());
    }

    #[test]
    fn test_transition_submitted_to_approved() {
        let db = LedgerDb::new(":memory:").unwrap();
        let id = db.create_batch("测试批次", "2026-07").unwrap();

        db.transition_batch_status(id, BatchStatus::Submitted)
            .unwrap();
        db.transition_batch_status(id, BatchStatus::Approved)
            .unwrap();

        let batch = db.get_batch(id).unwrap();
        assert_eq!(batch.status, BatchStatus::Approved);
        assert!(batch.submitted_at.is_some());
        assert!(batch.approved_at.is_some());
        assert!(batch.completed_at.is_none());
        assert!(batch.rejected_at.is_none());
    }

    #[test]
    fn test_transition_submitted_to_rejected() {
        let db = LedgerDb::new(":memory:").unwrap();
        let id = db.create_batch("测试批次", "2026-07").unwrap();

        db.transition_batch_status(id, BatchStatus::Submitted)
            .unwrap();
        db.transition_batch_status(id, BatchStatus::Rejected)
            .unwrap();

        let batch = db.get_batch(id).unwrap();
        assert_eq!(batch.status, BatchStatus::Rejected);
        assert!(batch.submitted_at.is_some());
        assert!(batch.rejected_at.is_some());
    }

    #[test]
    fn test_transition_approved_to_completed() {
        let db = LedgerDb::new(":memory:").unwrap();
        let id = db.create_batch("测试批次", "2026-07").unwrap();

        db.transition_batch_status(id, BatchStatus::Submitted)
            .unwrap();
        db.transition_batch_status(id, BatchStatus::Approved)
            .unwrap();
        db.transition_batch_status(id, BatchStatus::Completed)
            .unwrap();

        let batch = db.get_batch(id).unwrap();
        assert_eq!(batch.status, BatchStatus::Completed);
        assert!(batch.submitted_at.is_some());
        assert!(batch.approved_at.is_some());
        assert!(batch.completed_at.is_some());
        assert!(batch.rejected_at.is_none());
    }

    #[test]
    fn test_transition_approved_to_rejected() {
        let db = LedgerDb::new(":memory:").unwrap();
        let id = db.create_batch("测试批次", "2026-07").unwrap();

        db.transition_batch_status(id, BatchStatus::Submitted)
            .unwrap();
        db.transition_batch_status(id, BatchStatus::Approved)
            .unwrap();
        db.transition_batch_status(id, BatchStatus::Rejected)
            .unwrap();

        let batch = db.get_batch(id).unwrap();
        assert_eq!(batch.status, BatchStatus::Rejected);
        assert!(batch.submitted_at.is_some());
        assert!(batch.approved_at.is_some());
        assert!(batch.rejected_at.is_some());
    }

    #[test]
    fn test_reject_draft_to_completed() {
        let db = LedgerDb::new(":memory:").unwrap();
        let id = db.create_batch("测试批次", "2026-07").unwrap();

        let result = db.transition_batch_status(id, BatchStatus::Completed);
        assert!(result.is_err());
        match result {
            Err(StoreError::InvalidStateTransition { from, to }) => {
                assert_eq!(from, "Draft");
                assert_eq!(to, "Completed");
            }
            _ => panic!("Expected InvalidStateTransition error"),
        }
    }

    #[test]
    fn test_reject_draft_to_approved() {
        let db = LedgerDb::new(":memory:").unwrap();
        let id = db.create_batch("测试批次", "2026-07").unwrap();

        let result = db.transition_batch_status(id, BatchStatus::Approved);
        assert!(result.is_err());
        match result {
            Err(StoreError::InvalidStateTransition { from, to }) => {
                assert_eq!(from, "Draft");
                assert_eq!(to, "Approved");
            }
            _ => panic!("Expected InvalidStateTransition error"),
        }
    }

    #[test]
    fn test_reject_submitted_to_completed() {
        let db = LedgerDb::new(":memory:").unwrap();
        let id = db.create_batch("测试批次", "2026-07").unwrap();

        db.transition_batch_status(id, BatchStatus::Submitted)
            .unwrap();
        let result = db.transition_batch_status(id, BatchStatus::Completed);

        assert!(result.is_err());
        match result {
            Err(StoreError::InvalidStateTransition { from, to }) => {
                assert_eq!(from, "Submitted");
                assert_eq!(to, "Completed");
            }
            _ => panic!("Expected InvalidStateTransition error"),
        }
    }

    #[test]
    fn test_reject_completed_to_any_state() {
        let db = LedgerDb::new(":memory:").unwrap();
        let id = db.create_batch("测试批次", "2026-07").unwrap();

        db.transition_batch_status(id, BatchStatus::Submitted)
            .unwrap();
        db.transition_batch_status(id, BatchStatus::Approved)
            .unwrap();
        db.transition_batch_status(id, BatchStatus::Completed)
            .unwrap();

        // 尝试从 Completed 转换到任何状态都应该失败
        let result = db.transition_batch_status(id, BatchStatus::Draft);
        assert!(result.is_err());

        let result = db.transition_batch_status(id, BatchStatus::Submitted);
        assert!(result.is_err());

        let result = db.transition_batch_status(id, BatchStatus::Approved);
        assert!(result.is_err());
    }

    #[test]
    fn test_reject_rejected_to_any_state() {
        let db = LedgerDb::new(":memory:").unwrap();
        let id = db.create_batch("测试批次", "2026-07").unwrap();

        db.transition_batch_status(id, BatchStatus::Rejected)
            .unwrap();

        // 尝试从 Rejected 转换到任何状态都应该失败
        let result = db.transition_batch_status(id, BatchStatus::Draft);
        assert!(result.is_err());

        let result = db.transition_batch_status(id, BatchStatus::Submitted);
        assert!(result.is_err());

        let result = db.transition_batch_status(id, BatchStatus::Approved);
        assert!(result.is_err());

        let result = db.transition_batch_status(id, BatchStatus::Completed);
        assert!(result.is_err());
    }

    #[test]
    fn test_reject_same_state_transition() {
        let db = LedgerDb::new(":memory:").unwrap();
        let id = db.create_batch("测试批次", "2026-07").unwrap();

        let result = db.transition_batch_status(id, BatchStatus::Draft);
        assert!(result.is_err());
    }

    #[test]
    fn test_transition_nonexistent_batch() {
        let db = LedgerDb::new(":memory:").unwrap();

        let result = db.transition_batch_status(99999, BatchStatus::Submitted);
        assert!(result.is_err());
        // get_batch() 返回 Database 错误（QueryReturnedNoRows）
        match result {
            Err(StoreError::Database(_)) => {}
            _ => panic!("Expected Database error for nonexistent batch"),
        }
    }

    #[test]
    fn test_timestamp_preservation() {
        let db = LedgerDb::new(":memory:").unwrap();
        let id = db.create_batch("测试批次", "2026-07").unwrap();

        db.transition_batch_status(id, BatchStatus::Submitted)
            .unwrap();
        let batch1 = db.get_batch(id).unwrap();
        let submitted_at = batch1.submitted_at.unwrap();

        // 等待一小段时间确保时间戳不同
        std::thread::sleep(std::time::Duration::from_millis(10));

        db.transition_batch_status(id, BatchStatus::Approved)
            .unwrap();
        let batch2 = db.get_batch(id).unwrap();

        // submitted_at 应该保持不变
        assert_eq!(batch2.submitted_at.unwrap(), submitted_at);
        // approved_at 应该是新的
        assert!(batch2.approved_at.is_some());
    }

    #[test]
    fn test_full_happy_path_lifecycle() {
        let db = LedgerDb::new(":memory:").unwrap();
        let id = db.create_batch("完整流程测试", "2026-07").unwrap();

        // Draft → Submitted
        db.transition_batch_status(id, BatchStatus::Submitted)
            .unwrap();
        let batch = db.get_batch(id).unwrap();
        assert_eq!(batch.status, BatchStatus::Submitted);
        assert!(batch.submitted_at.is_some());

        // Submitted → Approved
        db.transition_batch_status(id, BatchStatus::Approved)
            .unwrap();
        let batch = db.get_batch(id).unwrap();
        assert_eq!(batch.status, BatchStatus::Approved);
        assert!(batch.approved_at.is_some());

        // Approved → Completed
        db.transition_batch_status(id, BatchStatus::Completed)
            .unwrap();
        let batch = db.get_batch(id).unwrap();
        assert_eq!(batch.status, BatchStatus::Completed);
        assert!(batch.completed_at.is_some());

        // 验证所有时间戳都存在
        assert!(batch.submitted_at.is_some());
        assert!(batch.approved_at.is_some());
        assert!(batch.completed_at.is_some());
        assert!(batch.rejected_at.is_none());
    }

    // ========== 新字段与去重测试 ==========

    #[test]
    fn test_new_fields_in_invoice() {
        let db = LedgerDb::new(":memory:").unwrap();
        let batch_id = db.create_batch("测试批次", "2026-07").unwrap();

        let invoice = ReportedInvoice {
            id: 0,
            batch_id,
            invoice_number: "12345678901234567890".to_string(),
            issue_date: NaiveDate::from_ymd_opt(2026, 7, 15).unwrap(),
            amount: Decimal::from_str("100.00").unwrap(),
            tax_amount: None,
            buyer_name: None,
            seller_name: None,
            ticket_type: TicketType::Rail,
            city: None,
            departure_time: None,
            checkin_date: None,
            file_path: "/path/to/invoice.xml".to_string(),
            created_at: Utc::now().naive_utc(),
            updated_at: Utc::now().naive_utc(),
            verification_result: Some("valid".to_string()),
            is_duplicate: true,
            duplicate_reason: Some("发票号完全一致".to_string()),
        };

        let invoice_id = db.add_invoice(&invoice).unwrap();

        // 通过 get_invoice 读取
        let retrieved = db.get_invoice(invoice_id).unwrap().expect("发票应存在");
        assert_eq!(retrieved.verification_result, Some("valid".to_string()));
        assert!(retrieved.is_duplicate);
        assert_eq!(
            retrieved.duplicate_reason,
            Some("发票号完全一致".to_string())
        );

        // 通过 list_invoices_by_batch 读取
        let invoices = db.list_invoices_by_batch(batch_id).unwrap();
        assert_eq!(invoices.len(), 1);
        assert_eq!(invoices[0].verification_result, Some("valid".to_string()));
        assert!(invoices[0].is_duplicate);
    }

    #[test]
    fn test_find_potential_duplicates_by_invoice_number() {
        let db = LedgerDb::new(":memory:").unwrap();
        let batch_id = db.create_batch("测试批次", "2026-07").unwrap();

        let invoice = ReportedInvoice {
            id: 0,
            batch_id,
            invoice_number: "12345678901234567890".to_string(),
            issue_date: NaiveDate::from_ymd_opt(2026, 7, 15).unwrap(),
            amount: Decimal::from_str("100.00").unwrap(),
            tax_amount: None,
            buyer_name: None,
            seller_name: None,
            ticket_type: TicketType::Rail,
            city: None,
            departure_time: None,
            checkin_date: None,
            file_path: "/path/to/invoice.xml".to_string(),
            created_at: Utc::now().naive_utc(),
            updated_at: Utc::now().naive_utc(),
            verification_result: None,
            is_duplicate: false,
            duplicate_reason: None,
        };

        db.add_invoice(&invoice).unwrap();

        // 按相同发票号查询（金额、日期、票种不同）
        let duplicates = db
            .find_potential_duplicates(
                "12345678901234567890",
                &Decimal::from_str("200.00").unwrap(), // 不同金额
                &NaiveDate::from_ymd_opt(2026, 7, 20).unwrap(), // 不同日期
                "flight",                              // 不同票种
                None,
            )
            .unwrap();

        assert_eq!(duplicates.len(), 1);
        assert_eq!(duplicates[0].invoice_number, "12345678901234567890");
    }

    #[test]
    fn test_find_potential_duplicates_by_three_fields() {
        let db = LedgerDb::new(":memory:").unwrap();
        let batch_id = db.create_batch("测试批次", "2026-07").unwrap();

        let invoice = ReportedInvoice {
            id: 0,
            batch_id,
            invoice_number: "11111111111111111111".to_string(),
            issue_date: NaiveDate::from_ymd_opt(2026, 7, 15).unwrap(),
            amount: Decimal::from_str("100.00").unwrap(),
            tax_amount: None,
            buyer_name: None,
            seller_name: None,
            ticket_type: TicketType::Rail,
            city: None,
            departure_time: None,
            checkin_date: None,
            file_path: "/path/to/invoice.xml".to_string(),
            created_at: Utc::now().naive_utc(),
            updated_at: Utc::now().naive_utc(),
            verification_result: None,
            is_duplicate: false,
            duplicate_reason: None,
        };

        db.add_invoice(&invoice).unwrap();

        // 按金额+日期+票种查询（发票号不同）
        let duplicates = db
            .find_potential_duplicates(
                "22222222222222222222", // 不同发票号
                &Decimal::from_str("100.00").unwrap(),
                &NaiveDate::from_ymd_opt(2026, 7, 15).unwrap(),
                "rail",
                None,
            )
            .unwrap();

        assert_eq!(duplicates.len(), 1);
        assert_eq!(duplicates[0].invoice_number, "11111111111111111111");
    }

    #[test]
    fn test_find_potential_duplicates_exclude_self() {
        let db = LedgerDb::new(":memory:").unwrap();
        let batch_id = db.create_batch("测试批次", "2026-07").unwrap();

        let invoice = ReportedInvoice {
            id: 0,
            batch_id,
            invoice_number: "12345678901234567890".to_string(),
            issue_date: NaiveDate::from_ymd_opt(2026, 7, 15).unwrap(),
            amount: Decimal::from_str("100.00").unwrap(),
            tax_amount: None,
            buyer_name: None,
            seller_name: None,
            ticket_type: TicketType::Rail,
            city: None,
            departure_time: None,
            checkin_date: None,
            file_path: "/path/to/invoice.xml".to_string(),
            created_at: Utc::now().naive_utc(),
            updated_at: Utc::now().naive_utc(),
            verification_result: None,
            is_duplicate: false,
            duplicate_reason: None,
        };

        let invoice_id = db.add_invoice(&invoice).unwrap();

        // 查询自身，排除自己
        let duplicates = db
            .find_potential_duplicates(
                "12345678901234567890",
                &Decimal::from_str("100.00").unwrap(),
                &NaiveDate::from_ymd_opt(2026, 7, 15).unwrap(),
                "rail",
                Some(invoice_id),
            )
            .unwrap();

        assert_eq!(duplicates.len(), 0); // 应排除自身
    }

    #[test]
    fn test_find_potential_duplicates_no_match() {
        let db = LedgerDb::new(":memory:").unwrap();
        let batch_id = db.create_batch("测试批次", "2026-07").unwrap();

        let invoice = ReportedInvoice {
            id: 0,
            batch_id,
            invoice_number: "12345678901234567890".to_string(),
            issue_date: NaiveDate::from_ymd_opt(2026, 7, 15).unwrap(),
            amount: Decimal::from_str("100.00").unwrap(),
            tax_amount: None,
            buyer_name: None,
            seller_name: None,
            ticket_type: TicketType::Rail,
            city: None,
            departure_time: None,
            checkin_date: None,
            file_path: "/path/to/invoice.xml".to_string(),
            created_at: Utc::now().naive_utc(),
            updated_at: Utc::now().naive_utc(),
            verification_result: None,
            is_duplicate: false,
            duplicate_reason: None,
        };

        db.add_invoice(&invoice).unwrap();

        // 完全不匹配的查询
        let duplicates = db
            .find_potential_duplicates(
                "99999999999999999999",
                &Decimal::from_str("200.00").unwrap(),
                &NaiveDate::from_ymd_opt(2026, 7, 20).unwrap(),
                "flight",
                None,
            )
            .unwrap();

        assert_eq!(duplicates.len(), 0);
    }

    #[test]
    fn test_find_potential_duplicates_multiple_matches() {
        let db = LedgerDb::new(":memory:").unwrap();
        let batch_id = db.create_batch("测试批次", "2026-07").unwrap();

        // 添加两张票：相同金额、日期、票种，不同发票号
        let invoice1 = ReportedInvoice {
            id: 0,
            batch_id,
            invoice_number: "11111111111111111111".to_string(),
            issue_date: NaiveDate::from_ymd_opt(2026, 7, 15).unwrap(),
            amount: Decimal::from_str("100.00").unwrap(),
            tax_amount: None,
            buyer_name: None,
            seller_name: None,
            ticket_type: TicketType::Rail,
            city: None,
            departure_time: None,
            checkin_date: None,
            file_path: "/path/to/invoice1.xml".to_string(),
            created_at: Utc::now().naive_utc(),
            updated_at: Utc::now().naive_utc(),
            verification_result: None,
            is_duplicate: false,
            duplicate_reason: None,
        };

        let invoice2 = ReportedInvoice {
            id: 0,
            batch_id,
            invoice_number: "22222222222222222222".to_string(),
            issue_date: NaiveDate::from_ymd_opt(2026, 7, 15).unwrap(),
            amount: Decimal::from_str("100.00").unwrap(),
            tax_amount: None,
            buyer_name: None,
            seller_name: None,
            ticket_type: TicketType::Rail,
            city: None,
            departure_time: None,
            checkin_date: None,
            file_path: "/path/to/invoice2.xml".to_string(),
            created_at: Utc::now().naive_utc(),
            updated_at: Utc::now().naive_utc(),
            verification_result: None,
            is_duplicate: false,
            duplicate_reason: None,
        };

        db.add_invoice(&invoice1).unwrap();
        db.add_invoice(&invoice2).unwrap();

        // 查询第三张同样的票
        let duplicates = db
            .find_potential_duplicates(
                "33333333333333333333",
                &Decimal::from_str("100.00").unwrap(),
                &NaiveDate::from_ymd_opt(2026, 7, 15).unwrap(),
                "rail",
                None,
            )
            .unwrap();

        assert_eq!(duplicates.len(), 2);
    }

    #[test]
    fn test_database_migration_from_v0_to_v6() {
        use rusqlite::Connection;
        use tempfile::tempdir;

        // 创建临时目录
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("test.db");

        // 步骤1：创建旧版数据库（无新字段）
        {
            let conn = Connection::open(&db_path).unwrap();
            conn.execute("PRAGMA foreign_keys = ON", []).unwrap();

            // 创建旧版 reported_invoices 表（无新字段）
            conn.execute(
                "CREATE TABLE reported_invoices (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    batch_id INTEGER NOT NULL,
                    invoice_number TEXT NOT NULL,
                    issue_date TEXT NOT NULL,
                    amount TEXT NOT NULL,
                    tax_amount TEXT,
                    buyer_name TEXT,
                    seller_name TEXT,
                    ticket_type TEXT NOT NULL,
                    city TEXT,
                    departure_time TEXT,
                    checkin_date TEXT,
                    file_path TEXT NOT NULL,
                    created_at TEXT NOT NULL,
                    updated_at TEXT NOT NULL
                )",
                [],
            )
            .unwrap();

            // 创建 batches 表
            conn.execute(
                "CREATE TABLE batches (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    name TEXT NOT NULL,
                    month TEXT NOT NULL,
                    status INTEGER NOT NULL DEFAULT 0,
                    total_amount TEXT NOT NULL,
                    invoice_count INTEGER NOT NULL DEFAULT 0,
                    created_at TEXT NOT NULL,
                    updated_at TEXT NOT NULL,
                    submitted_at TEXT,
                    approved_at TEXT,
                    completed_at TEXT,
                    rejected_at TEXT
                )",
                [],
            )
            .unwrap();

            // 插入旧数据
            let now = "2026-07-15 12:00:00";
            conn.execute(
                "INSERT INTO batches (name, month, status, total_amount, invoice_count, created_at, updated_at)
                 VALUES ('旧批次', '2026-07', 0, '0', 0, ?1, ?2)",
                params![now, now],
            ).unwrap();

            conn.execute(
                "INSERT INTO reported_invoices (
                    batch_id, invoice_number, issue_date, amount, tax_amount,
                    buyer_name, seller_name, ticket_type, city, departure_time, checkin_date,
                    file_path, created_at, updated_at
                 ) VALUES (1, '12345678901234567890', '2026-07-15', '100.00', NULL,
                           NULL, NULL, 'rail', NULL, NULL, NULL,
                           '/path/to/invoice.xml', ?1, ?2)",
                params![now, now],
            )
            .unwrap();

            // user_version 保持为 0
            let version: i32 = conn
                .query_row("PRAGMA user_version", [], |row| row.get(0))
                .unwrap();
            assert_eq!(version, 0);
        }

        // 步骤2：用 LedgerDb::new 打开，触发迁移
        let db = LedgerDb::new(&db_path).unwrap();

        // 步骤3：验证迁移成功
        let version: i32 = db
            .conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, LEDGER_SCHEMA_VERSION);

        let concur_table_count: i64 = db
            .conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master
                 WHERE type = 'table' AND name IN ('concur_send_sessions', 'concur_send_items')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(concur_table_count, 2);

        // 步骤4：验证旧数据可读，新字段有默认值
        let invoice = db.get_invoice(1).unwrap().expect("旧数据应存在");
        assert_eq!(invoice.invoice_number, "12345678901234567890");
        assert_eq!(invoice.verification_result, None);
        assert!(!invoice.is_duplicate); // 默认 0
        assert_eq!(invoice.duplicate_reason, None);

        // 步骤5：验证可以插入带新字段的数据
        let new_invoice = ReportedInvoice {
            id: 0,
            batch_id: 1,
            invoice_number: "99999999999999999999".to_string(),
            issue_date: NaiveDate::from_ymd_opt(2026, 7, 20).unwrap(),
            amount: Decimal::from_str("200.00").unwrap(),
            tax_amount: None,
            buyer_name: None,
            seller_name: None,
            ticket_type: TicketType::Flight,
            city: None,
            departure_time: None,
            checkin_date: None,
            file_path: "/path/to/new.xml".to_string(),
            created_at: Utc::now().naive_utc(),
            updated_at: Utc::now().naive_utc(),
            verification_result: Some("valid".to_string()),
            is_duplicate: true,
            duplicate_reason: Some("测试重复".to_string()),
        };

        let new_id = db.add_invoice(&new_invoice).unwrap();
        let retrieved = db.get_invoice(new_id).unwrap().expect("新数据应存在");
        assert_eq!(retrieved.verification_result, Some("valid".to_string()));
        assert!(retrieved.is_duplicate);
        assert_eq!(retrieved.duplicate_reason, Some("测试重复".to_string()));
    }

    #[test]
    fn old_schema_open_creates_verified_snapshot_before_migration() {
        let temp_dir = tempfile::tempdir().unwrap();
        let db_path = temp_dir.path().join("ledger.db");
        let snapshot_directory = temp_dir.path().join("migration-backups");
        {
            let connection = Connection::open(&db_path).unwrap();
            connection
                .execute_batch(
                    r#"CREATE TABLE batches (
                        id INTEGER PRIMARY KEY AUTOINCREMENT,
                        name TEXT NOT NULL,
                        month TEXT NOT NULL,
                        status INTEGER NOT NULL DEFAULT 0,
                        total_amount TEXT NOT NULL,
                        invoice_count INTEGER NOT NULL DEFAULT 0,
                        created_at TEXT NOT NULL,
                        updated_at TEXT NOT NULL,
                        submitted_at TEXT,
                        approved_at TEXT,
                        completed_at TEXT,
                        rejected_at TEXT
                    );
                    CREATE TABLE reported_invoices (
                        id INTEGER PRIMARY KEY AUTOINCREMENT,
                        batch_id INTEGER NOT NULL,
                        invoice_number TEXT NOT NULL,
                        issue_date TEXT NOT NULL,
                        amount TEXT NOT NULL,
                        tax_amount TEXT,
                        buyer_name TEXT,
                        seller_name TEXT,
                        ticket_type TEXT NOT NULL,
                        city TEXT,
                        departure_time TEXT,
                        checkin_date TEXT,
                        file_path TEXT NOT NULL,
                        created_at TEXT NOT NULL,
                        updated_at TEXT NOT NULL,
                        FOREIGN KEY (batch_id) REFERENCES batches(id) ON DELETE CASCADE
                    );
                    INSERT INTO batches (
                        id, name, month, status, total_amount, invoice_count, created_at, updated_at
                    ) VALUES (1, 'snapshot-source', '2026-06', 0, '88.00', 1, 'before', 'before');
                    INSERT INTO reported_invoices (
                        id, batch_id, invoice_number, issue_date, amount, ticket_type,
                        file_path, created_at, updated_at
                    ) VALUES (1, 1, '12345678901234567890', '2026-06-01', '88.00',
                              'rail', 'C:/snapshot-source.pdf', 'before', 'before');
                    PRAGMA user_version = 0;"#,
                )
                .unwrap();
        }

        let (migrated, snapshot) =
            LedgerDb::new_with_migration_snapshot(&db_path, &snapshot_directory).unwrap();
        let snapshot = snapshot.expect("old schema must create a migration snapshot");
        assert!(snapshot.is_file());
        assert!(snapshot.starts_with(&snapshot_directory));
        let migrated_version: i32 = migrated
            .conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(migrated_version, LEDGER_SCHEMA_VERSION);
        let migrated_path: String = migrated
            .conn
            .query_row(
                "SELECT file_path FROM reported_invoices WHERE id = 1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(migrated_path, "C:/snapshot-source.pdf");

        let snapshot_connection = Connection::open(&snapshot).unwrap();
        let snapshot_version: i32 = snapshot_connection
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(snapshot_version, 0);
        let snapshot_integrity: String = snapshot_connection
            .query_row("PRAGMA integrity_check", [], |row| row.get(0))
            .unwrap();
        assert_eq!(snapshot_integrity, "ok");
        let old_schema: String = snapshot_connection
            .query_row(
                "SELECT sql FROM sqlite_master
                 WHERE type = 'table' AND name = 'reported_invoices'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(!old_schema.contains("verification_result"));
        let preserved_path: String = snapshot_connection
            .query_row(
                "SELECT file_path FROM reported_invoices WHERE id = 1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(preserved_path, "C:/snapshot-source.pdf");
        drop(snapshot_connection);
        drop(migrated);

        let (_, second_snapshot) =
            LedgerDb::new_with_migration_snapshot(&db_path, &snapshot_directory).unwrap();
        assert!(second_snapshot.is_none());
        assert_eq!(std::fs::read_dir(&snapshot_directory).unwrap().count(), 1);
    }

    #[test]
    fn migration_failure_rolls_back_original_database_byte_for_byte() {
        use std::fs;

        let temp_dir = tempfile::tempdir().unwrap();
        let db_path = temp_dir.path().join("migration-failure.db");
        {
            let connection = Connection::open(&db_path).unwrap();
            connection
                .execute_batch(
                    r#"CREATE TABLE batches (
                        id INTEGER PRIMARY KEY AUTOINCREMENT,
                        name TEXT NOT NULL,
                        month TEXT NOT NULL,
                        status INTEGER NOT NULL DEFAULT 0,
                        total_amount TEXT NOT NULL,
                        invoice_count INTEGER NOT NULL DEFAULT 0,
                        created_at TEXT NOT NULL,
                        updated_at TEXT NOT NULL,
                        submitted_at TEXT,
                        approved_at TEXT,
                        completed_at TEXT,
                        rejected_at TEXT
                    );
                    CREATE TABLE reported_invoices (
                        id INTEGER PRIMARY KEY AUTOINCREMENT,
                        batch_id INTEGER NOT NULL,
                        invoice_number TEXT NOT NULL,
                        issue_date TEXT NOT NULL,
                        amount TEXT NOT NULL,
                        tax_amount TEXT,
                        buyer_name TEXT,
                        seller_name TEXT,
                        ticket_type TEXT NOT NULL,
                        city TEXT,
                        departure_time TEXT,
                        checkin_date TEXT,
                        file_path TEXT NOT NULL,
                        created_at TEXT NOT NULL,
                        updated_at TEXT NOT NULL,
                        verification_result TEXT,
                        is_duplicate INTEGER DEFAULT 0,
                        duplicate_reason TEXT,
                        FOREIGN KEY (batch_id) REFERENCES batches(id) ON DELETE CASCADE
                    );
                    CREATE TABLE batch_grouping (
                        batch_id INTEGER PRIMARY KEY,
                        rule_version TEXT NOT NULL,
                        home_cities_json TEXT NOT NULL,
                        overall_confidence REAL NOT NULL,
                        ambiguities_json TEXT NOT NULL,
                        created_at TEXT NOT NULL,
                        FOREIGN KEY (batch_id) REFERENCES batches(id) ON DELETE CASCADE
                    );
                    CREATE TABLE invoice_groups (
                        id INTEGER PRIMARY KEY AUTOINCREMENT,
                        batch_id INTEGER NOT NULL,
                        group_index INTEGER NOT NULL,
                        kind TEXT NOT NULL,
                        title TEXT NOT NULL,
                        start_date TEXT NOT NULL,
                        end_date TEXT NOT NULL,
                        confidence REAL NOT NULL,
                        requires_review INTEGER NOT NULL DEFAULT 0,
                        evidence_json TEXT NOT NULL,
                        UNIQUE(batch_id, group_index),
                        FOREIGN KEY (batch_id) REFERENCES batch_grouping(batch_id) ON DELETE CASCADE
                    );
                    CREATE TABLE invoice_group_members (
                        group_id INTEGER NOT NULL,
                        invoice_id INTEGER NOT NULL,
                        input_index INTEGER NOT NULL,
                        match_reason TEXT NOT NULL,
                        PRIMARY KEY (group_id, invoice_id),
                        FOREIGN KEY (group_id) REFERENCES invoice_groups(id) ON DELETE CASCADE,
                        FOREIGN KEY (invoice_id) REFERENCES reported_invoices(id) ON DELETE CASCADE
                    );
                    CREATE TABLE pipeline_runs (
                        pipeline_id TEXT PRIMARY KEY,
                        config_json TEXT NOT NULL,
                        source_kind TEXT NOT NULL,
                        stage TEXT NOT NULL,
                        status TEXT NOT NULL,
                        task_dir TEXT NOT NULL,
                        batch_id INTEGER UNIQUE,
                        last_error TEXT,
                        created_at TEXT NOT NULL,
                        updated_at TEXT NOT NULL,
                        FOREIGN KEY (batch_id) REFERENCES batches(id) ON DELETE SET NULL
                    );
                    CREATE TABLE review_actions (
                        id INTEGER PRIMARY KEY AUTOINCREMENT,
                        batch_id INTEGER NOT NULL,
                        action_type TEXT NOT NULL,
                        summary TEXT NOT NULL,
                        before_json TEXT NOT NULL,
                        after_json TEXT NOT NULL,
                        created_at TEXT NOT NULL,
                        undone_at TEXT,
                        FOREIGN KEY (batch_id) REFERENCES batches(id) ON DELETE CASCADE
                    );
                    CREATE TABLE excluded_invoices (
                        invoice_id INTEGER PRIMARY KEY,
                        reason TEXT NOT NULL,
                        excluded_at TEXT NOT NULL,
                        FOREIGN KEY (invoice_id) REFERENCES reported_invoices(id) ON DELETE CASCADE
                    );
                    CREATE TABLE settings (
                        key TEXT PRIMARY KEY NOT NULL,
                        value TEXT NOT NULL,
                        updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
                    );
                    INSERT INTO batches (
                        id, name, month, status, total_amount, invoice_count, created_at, updated_at
                    ) VALUES (1, 'preserve-batch', '2026-06', 0, '88.00', 1, 'before', 'before');
                    INSERT INTO reported_invoices (
                        id, batch_id, invoice_number, issue_date, amount, ticket_type,
                        file_path, created_at, updated_at
                    ) VALUES (1, 1, '12345678901234567890', '2026-06-01', '88.00',
                              'rail', 'C:/preserve.pdf', 'before', 'before');
                    INSERT INTO settings (key, value, updated_at)
                    VALUES ('sentinel', 'preserve', 'before');
                    PRAGMA user_version = 5;"#,
                )
                .unwrap();
        }
        let original_bytes = fs::read(&db_path).unwrap();

        let connection = Connection::open(&db_path).unwrap();
        connection.execute("PRAGMA foreign_keys = ON", []).unwrap();
        let db = LedgerDb { conn: connection };
        let error = db
            .init_schema_with_migration_hook(|point| {
                if point == "after_v6_ddl_before_version" {
                    Err(StoreError::Internal(
                        "injected migration failure".to_string(),
                    ))
                } else {
                    Ok(())
                }
            })
            .unwrap_err();
        assert!(error.to_string().contains("injected migration failure"));
        drop(db);

        assert_eq!(fs::read(&db_path).unwrap(), original_bytes);
        let connection = Connection::open(&db_path).unwrap();
        let version: i32 = connection
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, 5);
        let integrity: String = connection
            .query_row("PRAGMA integrity_check", [], |row| row.get(0))
            .unwrap();
        assert_eq!(integrity, "ok");
        let v6_table_count: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master
                 WHERE type = 'table' AND name IN ('concur_send_sessions', 'concur_send_items')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(v6_table_count, 0);
        let rolled_back_index_count: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'index'
                 AND name IN ('idx_batches_month', 'idx_invoices_batch_id', 'idx_invoices_number')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(rolled_back_index_count, 0);
        let preserved: (String, String, String) = connection
            .query_row(
                "SELECT b.name, i.file_path, s.value
                 FROM batches b
                 JOIN reported_invoices i ON i.batch_id = b.id
                 JOIN settings s ON s.key = 'sentinel'
                 WHERE b.id = 1 AND i.id = 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(
            preserved,
            (
                "preserve-batch".to_string(),
                "C:/preserve.pdf".to_string(),
                "preserve".to_string(),
            )
        );
        drop(connection);

        let migrated = LedgerDb::new(&db_path).unwrap();
        let migrated_version: i32 = migrated
            .conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(migrated_version, LEDGER_SCHEMA_VERSION);
        migrated.integrity_check().unwrap();
        assert_eq!(
            migrated.get_setting("sentinel").unwrap().as_deref(),
            Some("preserve")
        );
    }

    #[test]
    fn v18_migration_downgrades_unreliable_legacy_invalid_signatures() {
        let temp_dir = tempfile::tempdir().unwrap();
        let db_path = temp_dir.path().join("v17-signature-status.db");
        let invoice_id = {
            let db = LedgerDb::new(&db_path).unwrap();
            let batch_id = db.create_batch("旧验签状态", "2026-06").unwrap();
            let mut invoice = grouping_test_invoice(batch_id, "26112000000000000018", "18.00");
            invoice.verification_result = Some("invalid".to_string());
            let invoice_id = db.add_invoice(&invoice).unwrap();
            db.conn.execute("PRAGMA user_version = 17", []).unwrap();
            invoice_id
        };

        let migrated = LedgerDb::new(&db_path).unwrap();
        let invoice = migrated.get_invoice(invoice_id).unwrap().unwrap();
        assert_eq!(invoice.verification_result.as_deref(), Some("unsupported"));
        let version: i32 = migrated
            .conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, LEDGER_SCHEMA_VERSION);
    }

    #[test]
    fn v19_migration_preserves_delivery_history_and_allows_pdf_tasks() {
        let temp_dir = tempfile::tempdir().unwrap();
        let db_path = temp_dir.path().join("v18-delivery-kind.db");
        let batch_id = {
            let db = LedgerDb::new(&db_path).unwrap();
            let batch_id = db.create_batch("历史交付记录", "2026-06").unwrap();
            let now = LedgerDb::now_text();
            db.conn
                .execute(
                    "INSERT INTO batch_review_snapshots (
                        batch_id, version, content_json, content_sha256, invoice_count,
                        total_amount, created_at, invalidated_at
                     ) VALUES (?1, 1, '{}', 'test-digest', 1, '10.00', ?2, NULL)",
                    params![batch_id, now],
                )
                .unwrap();
            let snapshot_id = db.conn.last_insert_rowid();
            db.conn
                .execute(
                    "INSERT INTO delivery_tasks (
                        batch_id, review_snapshot_id, kind, status, output_path,
                        last_error, created_at, updated_at, completed_at
                     ) VALUES (?1, ?2, 'excel', 'succeeded', 'C:/test.xlsx',
                               NULL, ?3, ?3, ?3)",
                    params![batch_id, snapshot_id, now],
                )
                .unwrap();
            db.conn.execute("PRAGMA user_version = 18", []).unwrap();
            batch_id
        };

        let migrated = LedgerDb::new(&db_path).unwrap();
        let tasks = migrated.list_delivery_tasks(batch_id).unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].kind, "excel");
        let pdf_task = migrated.start_delivery_task(batch_id, "pdf").unwrap();
        assert_eq!(pdf_task.kind, "pdf");
        assert_eq!(pdf_task.status, "running");
        let version: i32 = migrated
            .conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, LEDGER_SCHEMA_VERSION);
    }

    #[test]
    fn future_schema_is_rejected_before_any_current_schema_write() {
        let temp_dir = tempfile::tempdir().unwrap();
        let db_path = temp_dir.path().join("future.db");
        {
            let connection = Connection::open(&db_path).unwrap();
            connection
                .execute_batch(&format!(
                    "CREATE TABLE future_only (
                         id INTEGER PRIMARY KEY,
                         payload TEXT NOT NULL
                     );
                     INSERT INTO future_only (id, payload) VALUES (1, 'preserve');
                     PRAGMA user_version = {};",
                    LEDGER_SCHEMA_VERSION + 1
                ))
                .unwrap();
        }

        let error = match LedgerDb::new(&db_path) {
            Ok(_) => panic!("future schema must be rejected"),
            Err(error) => error,
        };
        assert!(error.to_string().contains(&format!(
            "Unknown database version: {}",
            LEDGER_SCHEMA_VERSION + 1
        )));

        let connection = Connection::open(&db_path).unwrap();
        let version: i32 = connection
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, LEDGER_SCHEMA_VERSION + 1);
        let payload: String = connection
            .query_row("SELECT payload FROM future_only WHERE id = 1", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(payload, "preserve");
        let current_table_count: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'batches'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(current_table_count, 0);
    }

    #[test]
    fn v7_migration_marks_later_same_batch_numbers_without_overriding_review() {
        let temp_dir = tempfile::tempdir().unwrap();
        let db_path = temp_dir.path().join("v6-duplicate-backfill.db");
        let (first_id, repeated_id, reviewed_first_id, reviewed_repeated_id) = {
            let db = LedgerDb::new(&db_path).unwrap();

            let batch_id = db.create_batch("待补标", "2026-06").unwrap();
            let first_id = db
                .add_invoice(&grouping_test_invoice(
                    batch_id,
                    "26112000000000000001",
                    "88.00",
                ))
                .unwrap();
            let repeated_id = db
                .add_invoice(&grouping_test_invoice(
                    batch_id,
                    "26112000000000000001",
                    "88.00",
                ))
                .unwrap();

            let reviewed_batch_id = db.create_batch("已人工判断", "2026-06").unwrap();
            let reviewed_first_id = db
                .add_invoice(&grouping_test_invoice(
                    reviewed_batch_id,
                    "26112000000000000002",
                    "99.00",
                ))
                .unwrap();
            let mut reviewed_duplicate =
                grouping_test_invoice(reviewed_batch_id, "26112000000000000002", "99.00");
            reviewed_duplicate.is_duplicate = true;
            reviewed_duplicate.duplicate_reason = Some("测试重复".to_string());
            let reviewed_repeated_id = db.add_invoice(&reviewed_duplicate).unwrap();
            db.resolve_duplicate_with_audit(reviewed_repeated_id)
                .unwrap();

            db.conn.execute("PRAGMA user_version = 6", []).unwrap();
            (
                first_id,
                repeated_id,
                reviewed_first_id,
                reviewed_repeated_id,
            )
        };

        let migrated = LedgerDb::new(&db_path).unwrap();
        let version: i32 = migrated
            .conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, LEDGER_SCHEMA_VERSION);

        let first = migrated.get_invoice(first_id).unwrap().unwrap();
        let repeated = migrated.get_invoice(repeated_id).unwrap().unwrap();
        assert!(!first.is_duplicate);
        assert!(repeated.is_duplicate);
        assert!(repeated
            .duplicate_reason
            .as_deref()
            .unwrap()
            .contains("同一批次"));

        // 已经存在有效“人工确认非重复”记录的批次不得被迁移重新标记。
        assert!(
            !migrated
                .get_invoice(reviewed_first_id)
                .unwrap()
                .unwrap()
                .is_duplicate
        );
        assert!(
            !migrated
                .get_invoice(reviewed_repeated_id)
                .unwrap()
                .unwrap()
                .is_duplicate
        );
    }

    #[test]
    fn backup_database_inspection_is_read_only() {
        let temp_dir = tempfile::tempdir().unwrap();
        let db_path = temp_dir.path().join("inspect-read-only.db");
        {
            let db = LedgerDb::new(&db_path).unwrap();
            db.set_setting("sentinel", "preserve").unwrap();
        }
        let before = std::fs::read(&db_path).unwrap();

        assert_eq!(
            LedgerDb::inspect_existing_database(&db_path).unwrap(),
            LEDGER_SCHEMA_VERSION
        );
        assert_eq!(std::fs::read(&db_path).unwrap(), before);
    }

    #[test]
    fn settings_crud_operations() {
        let db = LedgerDb::new(":memory:").unwrap();

        // 设置不存在时返回 None
        assert_eq!(db.get_setting("home_city").unwrap(), None);

        // 写入设置
        db.set_setting("home_city", "北京").unwrap();
        assert_eq!(
            db.get_setting("home_city").unwrap(),
            Some("北京".to_string())
        );

        // 更新设置
        db.set_setting("home_city", "上海").unwrap();
        assert_eq!(
            db.get_setting("home_city").unwrap(),
            Some("上海".to_string())
        );

        // 写入多个设置
        db.set_setting("grouping_config", r#"{"weekend_days":[0,6]}"#)
            .unwrap();
        let all = db.get_all_settings().unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all.get("home_city"), Some(&"上海".to_string()));
        assert!(all.get("grouping_config").unwrap().contains("weekend_days"));
    }

    fn grouping_test_invoice(batch_id: i64, number: &str, amount: &str) -> ReportedInvoice {
        ReportedInvoice {
            id: 0,
            batch_id,
            invoice_number: number.to_string(),
            issue_date: NaiveDate::from_ymd_opt(2026, 6, 1).unwrap(),
            amount: Decimal::from_str(amount).unwrap(),
            tax_amount: None,
            buyer_name: Some("测试用户".to_string()),
            seller_name: Some("测试商户".to_string()),
            ticket_type: TicketType::Rail,
            city: Some("北京".to_string()),
            departure_time: None,
            checkin_date: None,
            file_path: "C:/test/invoice.xml".to_string(),
            created_at: Utc::now().naive_utc(),
            updated_at: Utc::now().naive_utc(),
            verification_result: Some("valid".to_string()),
            is_duplicate: false,
            duplicate_reason: None,
        }
    }

    #[test]
    fn expense_category_is_separate_and_unknown_requires_confirmation() {
        let db = LedgerDb::new(":memory:").unwrap();
        let batch_id = db.create_batch("费用分类确认", "2026-06").unwrap();
        let mut invoice = grouping_test_invoice(batch_id, "10000000000000000900", "88.00");
        invoice.ticket_type = TicketType::Other;
        let invoice_id = db.add_invoice(&invoice).unwrap();
        let expense = db
            .list_expense_items_by_batch(batch_id)
            .unwrap()
            .into_iter()
            .find(|item| item.primary_invoice_id == invoice_id)
            .unwrap();
        assert_eq!(expense.category_code, "other");
        assert_eq!(expense.category_source, "unclassified");
        assert!(!expense.category_confirmed);

        let updated = db
            .update_expense_item_with_audit(
                expense.id,
                &ExpenseItemUpdate {
                    category_code: "meal".to_string(),
                    category_confirmed: true,
                    transaction_date: expense.transaction_date,
                    transaction_date_confirmed: true,
                    description: expense.description.clone(),
                    counterparty_name: expense.counterparty_name.clone(),
                    location: expense.location.clone(),
                    payment_method: expense.payment_method.clone(),
                    gross_amount: expense.gross_amount,
                    currency_code: expense.currency_code.clone(),
                    tax_details: expense.tax_details.clone(),
                },
            )
            .unwrap();
        assert_eq!(updated.category_code, "meal");
        assert_eq!(updated.category_source, "manual_review");
        assert!(updated.category_confirmed);
    }

    #[test]
    fn courier_logistics_expense_can_be_reviewed_and_saved() {
        let db = LedgerDb::new(":memory:").unwrap();
        let batch_id = db.create_batch("快递费用保存", "2026-06").unwrap();
        let mut invoice = grouping_test_invoice(batch_id, "10000000000000000902", "15.00");
        invoice.ticket_type = TicketType::CourierLogistics;
        let invoice_id = db.add_invoice(&invoice).unwrap();
        let expense = db
            .list_expense_items_by_batch(batch_id)
            .unwrap()
            .into_iter()
            .find(|item| item.primary_invoice_id == invoice_id)
            .unwrap();

        let updated = db
            .update_expense_item_with_audit(
                expense.id,
                &ExpenseItemUpdate {
                    category_code: "courier_logistics".to_string(),
                    category_confirmed: true,
                    transaction_date: expense.transaction_date,
                    transaction_date_confirmed: true,
                    description: "寄送报销材料".to_string(),
                    counterparty_name: expense.counterparty_name.clone(),
                    location: expense.location.clone(),
                    payment_method: expense.payment_method.clone(),
                    gross_amount: expense.gross_amount,
                    currency_code: expense.currency_code.clone(),
                    tax_details: expense.tax_details.clone(),
                },
            )
            .unwrap();

        assert_eq!(updated.category_code, "courier_logistics");
        assert!(updated.category_confirmed);
        assert_eq!(updated.description, "寄送报销材料");
    }

    #[test]
    fn expense_and_invoice_field_reviews_do_not_reopen_confirmed_grouping() {
        use crate::models::{ExpenseCategoryDetection, NewInvoiceGroup, NewInvoiceGroupMember};

        let db = LedgerDb::new(":memory:").unwrap();
        let batch_id = db.create_batch("费用与归组解耦", "2026-06").unwrap();
        let mut invoice = grouping_test_invoice(batch_id, "10000000000000000901", "88.00");
        invoice.ticket_type = TicketType::Other;
        let invoice_id = db.add_invoice(&invoice).unwrap();
        let expense_id = db.list_expense_items_by_batch(batch_id).unwrap()[0].id;
        db.replace_batch_grouping(&NewBatchGrouping {
            batch_id,
            rule_version: "deterministic-v1".to_string(),
            home_cities_json: "[\"北京\"]".to_string(),
            overall_confidence: 1.0,
            ambiguities_json: "[]".to_string(),
            groups: vec![NewInvoiceGroup {
                group_index: 0,
                kind: "local_month".to_string(),
                title: "6 月市内消费".to_string(),
                start_date: "2026-06-01".to_string(),
                end_date: "2026-06-30".to_string(),
                confidence: 1.0,
                requires_review: false,
                evidence_json: "{}".to_string(),
                members: vec![NewInvoiceGroupMember {
                    invoice_id,
                    input_index: 0,
                    match_reason: "月份匹配".to_string(),
                }],
            }],
        })
        .unwrap();
        let grouping_stays_confirmed =
            || !db.get_batch_grouping(batch_id).unwrap().unwrap().groups[0].requires_review;

        db.apply_detected_expense_categories_with_audit(
            batch_id,
            &[ExpenseCategoryDetection {
                expense_item_id: expense_id,
                category_code: "meal".to_string(),
                source: "parser.reanalysis".to_string(),
                confirmed: true,
            }],
        )
        .unwrap();
        assert!(grouping_stays_confirmed());

        db.apply_supporting_document_facts_with_audit(
            expense_id,
            NaiveDate::from_ymd_opt(2026, 6, 2).unwrap(),
            Some("北京"),
        )
        .unwrap();
        assert!(grouping_stays_confirmed());

        let expense = db.get_expense_item(expense_id).unwrap().unwrap();
        db.update_expense_item_with_audit(
            expense_id,
            &ExpenseItemUpdate {
                category_code: "meal".to_string(),
                category_confirmed: true,
                transaction_date: expense.transaction_date,
                transaction_date_confirmed: true,
                description: expense.description,
                counterparty_name: expense.counterparty_name,
                location: expense.location,
                payment_method: expense.payment_method,
                gross_amount: expense.gross_amount,
                currency_code: expense.currency_code,
                tax_details: expense.tax_details,
            },
        )
        .unwrap();
        assert!(grouping_stays_confirmed());

        db.update_invoice_review_fields(
            invoice_id,
            &InvoiceReviewUpdate {
                invoice_number: invoice.invoice_number,
                issue_date: invoice.issue_date,
                amount: invoice.amount,
                tax_amount: invoice.tax_amount,
                buyer_name: invoice.buyer_name,
                seller_name: Some("人工确认商户".to_string()),
                ticket_type: TicketType::Meal,
                city: Some("北京".to_string()),
                departure_time: None,
                checkin_date: None,
            },
        )
        .unwrap();
        assert!(grouping_stays_confirmed());
    }

    #[test]
    fn parsed_supporting_document_is_linked_and_excluded_from_expense_total() {
        let db = LedgerDb::new(":memory:").unwrap();
        let batch_id = db.create_batch("配套材料纠正", "2026-06").unwrap();
        let mut invoice = grouping_test_invoice(batch_id, "26132000001995539716", "453.05");
        invoice.file_path = "C:/test/hotel-invoice.pdf".to_string();
        let invoice_id = db.add_invoice(&invoice).unwrap();
        let mut folio = grouping_test_invoice(batch_id, "0010574", "453.05");
        folio.file_path = "C:/test/hotel-folio.pdf".to_string();
        let folio_id = db.add_invoice(&folio).unwrap();
        let target = db
            .list_expense_items_by_batch(batch_id)
            .unwrap()
            .into_iter()
            .find(|expense| expense.primary_invoice_id == invoice_id)
            .unwrap();

        let document = db
            .reclassify_invoice_as_supporting_document_with_audit(folio_id, target.id)
            .unwrap();

        assert_eq!(document.role, "supporting");
        assert_eq!(document.source_invoice_id, Some(folio_id));
        assert!(db.is_invoice_excluded(folio_id).unwrap());
        let folio_expense = db
            .list_expense_items_by_batch(batch_id)
            .unwrap()
            .into_iter()
            .find(|expense| expense.primary_invoice_id == folio_id)
            .unwrap();
        assert_eq!(folio_expense.inclusion_status, "excluded");
    }

    #[test]
    fn supporting_document_city_corrects_non_manual_parser_value() {
        let db = LedgerDb::new(":memory:").unwrap();
        let batch_id = db.create_batch("行程城市纠正", "2026-06").unwrap();
        let mut invoice = grouping_test_invoice(batch_id, "26127000000321875212", "151.94");
        invoice.ticket_type = TicketType::CityTransport;
        invoice.city = Some("三".to_string());
        let invoice_id = db.add_invoice(&invoice).unwrap();
        let expense = db
            .list_expense_items_by_batch(batch_id)
            .unwrap()
            .into_iter()
            .find(|expense| expense.primary_invoice_id == invoice_id)
            .unwrap();

        db.apply_supporting_document_facts_with_audit(
            expense.id,
            NaiveDate::from_ymd_opt(2026, 6, 24).unwrap(),
            Some("邢台"),
        )
        .unwrap();

        let updated = db
            .list_expense_items_by_batch(batch_id)
            .unwrap()
            .into_iter()
            .find(|item| item.id == expense.id)
            .unwrap();
        assert_eq!(updated.location.city_name.as_deref(), Some("邢台"));
        assert_eq!(updated.transaction_date.to_string(), "2026-06-24");
    }

    #[test]
    fn deleting_historical_batch_recomputes_unreviewed_duplicate_dependencies() {
        let db = LedgerDb::new(":memory:").unwrap();
        let historical_batch = db.create_batch("历史批次", "2026-05").unwrap();
        let draft_batch = db.create_batch("当前草稿", "2026-06").unwrap();
        db.add_invoice(&grouping_test_invoice(
            historical_batch,
            "10000000000000000901",
            "88.00",
        ))
        .unwrap();
        let mut duplicate = grouping_test_invoice(draft_batch, "10000000000000000901", "88.00");
        duplicate.is_duplicate = true;
        duplicate.duplicate_reason = Some("发票号与历史台账一致（命中 1 条）".to_string());
        let duplicate_id = db.add_invoice(&duplicate).unwrap();
        assert!(db.get_invoice(duplicate_id).unwrap().unwrap().is_duplicate);
        assert_eq!(db.get_batch(draft_batch).unwrap().invoice_count, 0);

        db.delete_batch(historical_batch).unwrap();

        let restored = db.get_invoice(duplicate_id).unwrap().unwrap();
        assert!(!restored.is_duplicate);
        assert!(restored.duplicate_reason.is_none());
        let expense = db
            .list_expense_items_by_batch(draft_batch)
            .unwrap()
            .into_iter()
            .find(|expense| expense.primary_invoice_id == duplicate_id)
            .unwrap();
        assert_eq!(expense.inclusion_status, "included");
        let batch = db.get_batch(draft_batch).unwrap();
        assert_eq!(batch.invoice_count, 1);
        assert_eq!(batch.total_amount, Decimal::from_str("88.00").unwrap());
    }

    #[test]
    fn unreviewed_draft_analysis_can_be_reset_but_reviewed_data_is_protected() {
        let db = LedgerDb::new(":memory:").unwrap();
        let batch_id = db.create_batch("待重新分析", "2026-06").unwrap();
        let invoice_id = db
            .add_invoice(&grouping_test_invoice(
                batch_id,
                "10000000000000000911",
                "66.00",
            ))
            .unwrap();
        db.reset_draft_batch_automatic_analysis(batch_id).unwrap();
        assert!(db.list_invoices_by_batch(batch_id).unwrap().is_empty());
        assert!(db.list_expense_items_by_batch(batch_id).unwrap().is_empty());
        assert_eq!(db.get_batch(batch_id).unwrap().invoice_count, 0);

        let reviewed_id = db
            .add_invoice(&grouping_test_invoice(
                batch_id,
                "10000000000000000912",
                "77.00",
            ))
            .unwrap();
        db.update_invoice_review_fields(
            reviewed_id,
            &InvoiceReviewUpdate {
                invoice_number: "10000000000000000912".to_string(),
                issue_date: NaiveDate::from_ymd_opt(2026, 6, 1).unwrap(),
                amount: Decimal::from_str("77.00").unwrap(),
                tax_amount: None,
                buyer_name: Some("测试用户".to_string()),
                seller_name: Some("人工确认商户".to_string()),
                ticket_type: TicketType::Rail,
                city: Some("北京".to_string()),
                departure_time: None,
                checkin_date: None,
            },
        )
        .unwrap();
        assert!(matches!(
            db.reset_draft_batch_automatic_analysis(batch_id),
            Err(StoreError::Validation(_))
        ));
        assert!(db.get_invoice(reviewed_id).unwrap().is_some());
        assert!(db.get_invoice(invoice_id).unwrap().is_none());
    }

    #[test]
    fn review_edits_keep_stable_expense_amount_independent_and_undo_are_audited() {
        use crate::models::{NewInvoiceGroup, NewInvoiceGroupMember};

        let db = LedgerDb::new(":memory:").unwrap();
        let batch_id = db.create_batch("审核闭环", "2026-06").unwrap();
        let first_id = db
            .add_invoice(&grouping_test_invoice(
                batch_id,
                "10000000000000000011",
                "100.00",
            ))
            .unwrap();
        let second_id = db
            .add_invoice(&grouping_test_invoice(
                batch_id,
                "10000000000000000012",
                "200.00",
            ))
            .unwrap();
        db.replace_batch_grouping(&NewBatchGrouping {
            batch_id,
            rule_version: "deterministic-v1".to_string(),
            home_cities_json: "[\"北京\"]".to_string(),
            overall_confidence: 0.72,
            ambiguities_json: "[\"待确认归组\"]".to_string(),
            groups: vec![
                NewInvoiceGroup {
                    group_index: 0,
                    kind: "business_trip".to_string(),
                    title: "第一组".to_string(),
                    start_date: "2026-06-01".to_string(),
                    end_date: "2026-06-02".to_string(),
                    confidence: 0.9,
                    requires_review: false,
                    evidence_json: "{}".to_string(),
                    members: vec![NewInvoiceGroupMember {
                        invoice_id: first_id,
                        input_index: 0,
                        match_reason: "初始归组".to_string(),
                    }],
                },
                NewInvoiceGroup {
                    group_index: 1,
                    kind: "needs_review".to_string(),
                    title: "第二组".to_string(),
                    start_date: "2026-06-03".to_string(),
                    end_date: "2026-06-04".to_string(),
                    confidence: 0.5,
                    requires_review: true,
                    evidence_json: "{}".to_string(),
                    members: vec![NewInvoiceGroupMember {
                        invoice_id: second_id,
                        input_index: 1,
                        match_reason: "待确认".to_string(),
                    }],
                },
            ],
        })
        .unwrap();

        db.update_invoice_review_fields(
            first_id,
            &InvoiceReviewUpdate {
                invoice_number: "10000000000000000011".to_string(),
                issue_date: NaiveDate::from_ymd_opt(2026, 6, 2).unwrap(),
                amount: Decimal::from_str("120.00").unwrap(),
                tax_amount: Some(Decimal::from_str("10.00").unwrap()),
                buyer_name: Some("测试用户".to_string()),
                seller_name: Some("新商户".to_string()),
                ticket_type: TicketType::Hotel,
                city: Some("上海".to_string()),
                departure_time: None,
                checkin_date: Some(NaiveDate::from_ymd_opt(2026, 6, 2).unwrap()),
            },
        )
        .unwrap();
        assert_eq!(
            db.get_batch(batch_id).unwrap().total_amount,
            Decimal::from_str("300.00").unwrap()
        );
        let grouping_after_invoice_edit = db.get_batch_grouping(batch_id).unwrap().unwrap();
        assert!(!grouping_after_invoice_edit.groups[0].requires_review);
        assert!(grouping_after_invoice_edit.groups[1].requires_review);
        assert_eq!(
            db.list_review_actions(batch_id).unwrap()[0].action_type,
            "invoice_fields_updated"
        );

        db.undo_last_review_action(batch_id).unwrap();
        assert_eq!(
            db.get_invoice(first_id).unwrap().unwrap().amount,
            Decimal::from_str("100.00").unwrap()
        );
        assert_eq!(
            db.get_batch(batch_id).unwrap().total_amount,
            Decimal::from_str("300.00").unwrap()
        );
        let restored = db.get_batch_grouping(batch_id).unwrap().unwrap();
        assert!(!restored.groups[0].requires_review);
        assert!(restored.groups[1].requires_review);

        let manual_group_id = db
            .create_manual_invoice_group(
                batch_id,
                "business_trip",
                "人工拆分组",
                "2026-06-01",
                "2026-06-04",
            )
            .unwrap();
        db.move_invoice_to_group(batch_id, first_id, manual_group_id)
            .unwrap();
        let moved = db.get_batch_grouping(batch_id).unwrap().unwrap();
        assert!(moved
            .groups
            .iter()
            .find(|group| group.id == manual_group_id)
            .unwrap()
            .members
            .iter()
            .any(|member| member.invoice_id == first_id));
        let second_group_id = moved
            .groups
            .iter()
            .find(|group| {
                group
                    .members
                    .iter()
                    .any(|member| member.invoice_id == second_id)
            })
            .unwrap()
            .id;
        db.merge_invoice_groups(batch_id, second_group_id, manual_group_id)
            .unwrap();
        let merged = db.get_batch_grouping(batch_id).unwrap().unwrap();
        assert_eq!(merged.groups.len(), 1);
        assert_eq!(
            merged
                .groups
                .iter()
                .find(|group| group.id == manual_group_id)
                .unwrap()
                .members
                .len(),
            2
        );

        db.confirm_batch_grouping(batch_id).unwrap();
        let confirmed = db.get_batch_grouping(batch_id).unwrap().unwrap();
        assert_eq!(confirmed.ambiguities_json, "[]");
        assert!(confirmed.groups.iter().all(|group| !group.requires_review));
        db.undo_last_review_action(batch_id).unwrap();
        assert!(db
            .get_batch_grouping(batch_id)
            .unwrap()
            .unwrap()
            .groups
            .iter()
            .any(|group| group.requires_review));

        let actions = db.list_review_actions(batch_id).unwrap();
        assert_eq!(actions.len(), 5);
        assert_eq!(
            actions
                .iter()
                .filter(|action| action.undone_at.is_some())
                .count(),
            2
        );
    }

    #[test]
    fn business_trip_group_requires_transport_evidence_decision_before_confirmation() {
        let db = LedgerDb::new(":memory:").unwrap();
        let batch_id = db.create_batch("差旅行程锚点", "2026-06").unwrap();
        let mut meal = grouping_test_invoice(batch_id, "10000000000000000020", "80.00");
        meal.ticket_type = TicketType::Meal;
        let meal_id = db.add_invoice(&meal).unwrap();
        let group_id = db
            .create_manual_invoice_group(
                batch_id,
                "business_trip",
                "只有餐费的差旅组",
                "2026-06-01",
                "2026-06-01",
            )
            .unwrap();
        db.move_invoice_to_group(batch_id, meal_id, group_id)
            .unwrap();

        let per_group_error = db.confirm_invoice_group(batch_id, group_id).unwrap_err();
        assert!(matches!(
            per_group_error,
            StoreError::Validation(message)
                if message == "business trip group requires a transport evidence decision"
        ));
        let error = db.confirm_batch_grouping(batch_id).unwrap_err();
        assert!(matches!(
            error,
            StoreError::Validation(message)
                if message == "business trip group lacks a transport evidence decision"
        ));
        assert!(
            db.get_batch_grouping(batch_id)
                .unwrap()
                .unwrap()
                .groups
                .iter()
                .find(|group| group.id == group_id)
                .unwrap()
                .requires_review
        );

        db.set_invoice_group_transport_evidence(batch_id, group_id, "company_paid")
            .unwrap();
        db.confirm_invoice_group(batch_id, group_id).unwrap();
        assert!(
            !db.get_batch_grouping(batch_id)
                .unwrap()
                .unwrap()
                .groups
                .iter()
                .find(|group| group.id == group_id)
                .unwrap()
                .requires_review
        );
    }

    #[test]
    fn single_group_confirmation_clears_its_ambiguity_and_is_undoable() {
        use crate::models::{NewInvoiceGroup, NewInvoiceGroupMember};

        let db = LedgerDb::new(":memory:").unwrap();
        let batch_id = db.create_batch("逐组确认", "2026-06").unwrap();
        let invoice_id = db
            .add_invoice(&grouping_test_invoice(
                batch_id,
                "10000000000000000030",
                "130.00",
            ))
            .unwrap();
        db.replace_batch_grouping(&NewBatchGrouping {
            batch_id,
            rule_version: "deterministic-v1".to_string(),
            home_cities_json: "[\"北京\"]".to_string(),
            overall_confidence: 0.8,
            ambiguities_json:
                "[{\"description\":\"accept current group\",\"involved_invoice_ids\":[0]}]"
                    .to_string(),
            groups: vec![NewInvoiceGroup {
                group_index: 0,
                kind: "business_trip".to_string(),
                title: "张家口".to_string(),
                start_date: "2026-06-01".to_string(),
                end_date: "2026-06-01".to_string(),
                confidence: 0.8,
                requires_review: true,
                evidence_json: "{}".to_string(),
                members: vec![NewInvoiceGroupMember {
                    invoice_id,
                    input_index: 0,
                    match_reason: "route".to_string(),
                }],
            }],
        })
        .unwrap();
        let group_id = db.get_batch_grouping(batch_id).unwrap().unwrap().groups[0].id;

        db.confirm_invoice_group(batch_id, group_id).unwrap();
        let confirmed = db.get_batch_grouping(batch_id).unwrap().unwrap();
        assert!(!confirmed.groups[0].requires_review);
        assert_eq!(confirmed.ambiguities_json, "[]");
        assert_eq!(
            db.list_review_actions(batch_id).unwrap()[0].action_type,
            "invoice_group_confirmed"
        );

        db.undo_last_review_action(batch_id).unwrap();
        let restored = db.get_batch_grouping(batch_id).unwrap().unwrap();
        assert!(restored.groups[0].requires_review);
        assert!(restored.ambiguities_json.contains("accept current group"));
    }

    #[test]
    fn transport_decision_removes_resolved_missing_evidence_ambiguity() {
        use crate::models::{NewInvoiceGroup, NewInvoiceGroupMember};

        let db = LedgerDb::new(":memory:").unwrap();
        let batch_id = db.create_batch("公司购票出差", "2026-06").unwrap();
        let mut meal = grouping_test_invoice(batch_id, "10000000000000000031", "80.00");
        meal.ticket_type = TicketType::Meal;
        let invoice_id = db.add_invoice(&meal).unwrap();
        db.replace_batch_grouping(&NewBatchGrouping {
            batch_id,
            rule_version: "deterministic-v2".to_string(),
            home_cities_json: "[\"北京\"]".to_string(),
            overall_confidence: 0.8,
            ambiguities_json: "[{\"kind\":\"MissingTransportEvidence\",\"description\":\"缺少个人交通票\",\"involved_invoice_ids\":[0],\"candidates\":[]}]".to_string(),
            groups: vec![NewInvoiceGroup {
                group_index: 0,
                kind: "business_trip".to_string(),
                title: "上海出差".to_string(),
                start_date: "2026-06-01".to_string(),
                end_date: "2026-06-02".to_string(),
                confidence: 0.8,
                requires_review: true,
                evidence_json: "{\"transportEvidenceStatus\":\"missing\"}".to_string(),
                members: vec![NewInvoiceGroupMember {
                    invoice_id,
                    input_index: 0,
                    match_reason: "异地住宿候选".to_string(),
                }],
            }],
        })
        .unwrap();
        let group_id = db.get_batch_grouping(batch_id).unwrap().unwrap().groups[0].id;

        db.set_invoice_group_transport_evidence(batch_id, group_id, "company_paid")
            .unwrap();

        let grouping = db.get_batch_grouping(batch_id).unwrap().unwrap();
        assert_eq!(grouping.ambiguities_json, "[]");
        assert!(grouping.groups[0].evidence_json.contains("company_paid"));
    }

    #[test]
    fn confirming_one_group_keeps_ambiguity_shared_with_another_group() {
        use crate::models::{NewInvoiceGroup, NewInvoiceGroupMember};

        let db = LedgerDb::new(":memory:").unwrap();
        let batch_id = db.create_batch("跨组判断", "2026-06").unwrap();
        let first_id = db
            .add_invoice(&grouping_test_invoice(
                batch_id,
                "10000000000000000032",
                "40.00",
            ))
            .unwrap();
        let second_id = db
            .add_invoice(&grouping_test_invoice(
                batch_id,
                "10000000000000000033",
                "50.00",
            ))
            .unwrap();
        let make_group = |group_index, title: &str, invoice_id, input_index| NewInvoiceGroup {
            group_index,
            kind: "local_month".to_string(),
            title: title.to_string(),
            start_date: "2026-06-01".to_string(),
            end_date: "2026-06-30".to_string(),
            confidence: 0.8,
            requires_review: true,
            evidence_json: "{}".to_string(),
            members: vec![NewInvoiceGroupMember {
                invoice_id,
                input_index,
                match_reason: "月份匹配".to_string(),
            }],
        };
        db.replace_batch_grouping(&NewBatchGrouping {
            batch_id,
            rule_version: "deterministic-v2".to_string(),
            home_cities_json: "[\"北京\"]".to_string(),
            overall_confidence: 0.8,
            ambiguities_json: "[{\"kind\":\"MultipleTripMatch\",\"description\":\"跨组判断\",\"involved_invoice_ids\":[0,1],\"candidates\":[]}]".to_string(),
            groups: vec![
                make_group(0, "第一组", first_id, 0),
                make_group(1, "第二组", second_id, 1),
            ],
        })
        .unwrap();
        let first_group_id = db.get_batch_grouping(batch_id).unwrap().unwrap().groups[0].id;

        db.confirm_invoice_group(batch_id, first_group_id).unwrap();

        let grouping = db.get_batch_grouping(batch_id).unwrap().unwrap();
        assert!(grouping.ambiguities_json.contains("跨组判断"));
        assert!(!grouping.groups[0].requires_review);
        assert!(grouping.groups[1].requires_review);
    }

    #[test]
    fn detected_categories_keep_merchant_matches_unconfirmed() {
        use crate::models::ExpenseCategoryDetection;

        let db = LedgerDb::new(":memory:").unwrap();
        let batch_id = db.create_batch("费用类型重识别", "2026-06").unwrap();
        let mut invoice = grouping_test_invoice(batch_id, "10000000000000000040", "80.00");
        invoice.ticket_type = TicketType::Other;
        let invoice_id = db.add_invoice(&invoice).unwrap();
        let expense_id = db.list_expense_items_by_batch(batch_id).unwrap()[0].id;

        db.apply_detected_expense_categories_with_audit(
            batch_id,
            &[ExpenseCategoryDetection {
                expense_item_id: expense_id,
                category_code: "meal".to_string(),
                source: "merchant_name.suggestion".to_string(),
                confirmed: false,
            }],
        )
        .unwrap();
        let suggestion = db.list_expense_items_by_batch(batch_id).unwrap().remove(0);
        assert_eq!(suggestion.category_code, "meal");
        assert!(!suggestion.category_confirmed);
        assert_eq!(suggestion.category_source, "merchant_name.suggestion");
        assert_eq!(
            db.get_invoice(invoice_id).unwrap().unwrap().ticket_type,
            TicketType::Other
        );
    }

    #[test]
    fn reanalysis_can_revoke_a_stale_automatic_category() {
        use crate::models::ExpenseCategoryDetection;

        let db = LedgerDb::new(":memory:").unwrap();
        let batch_id = db.create_batch("撤销旧自动分类", "2026-06").unwrap();
        let mut invoice = grouping_test_invoice(batch_id, "10000000000000000041", "80.00");
        invoice.ticket_type = TicketType::Other;
        let invoice_id = db.add_invoice(&invoice).unwrap();
        let expense_id = db.list_expense_items_by_batch(batch_id).unwrap()[0].id;

        db.apply_detected_expense_categories_with_audit(
            batch_id,
            &[ExpenseCategoryDetection {
                expense_item_id: expense_id,
                category_code: "meal".to_string(),
                source: "parser.reanalysis".to_string(),
                confirmed: true,
            }],
        )
        .unwrap();
        db.apply_detected_expense_categories_with_audit(
            batch_id,
            &[ExpenseCategoryDetection {
                expense_item_id: expense_id,
                category_code: "other".to_string(),
                source: "parser.reanalysis".to_string(),
                confirmed: false,
            }],
        )
        .unwrap();

        let expense = db.list_expense_items_by_batch(batch_id).unwrap().remove(0);
        assert_eq!(expense.category_code, "other");
        assert_eq!(expense.category_source, "parser.reanalysis");
        assert!(!expense.category_confirmed);
        assert_eq!(
            db.get_invoice(invoice_id).unwrap().unwrap().ticket_type,
            TicketType::Other
        );
    }

    #[test]
    fn audited_grouping_replacement_refreshes_expense_group_references_and_undoes() {
        use crate::models::{NewInvoiceGroup, NewInvoiceGroupMember};

        let db = LedgerDb::new(":memory:").unwrap();
        let batch_id = db.create_batch("重新分析归组", "2026-06").unwrap();
        let first_id = db
            .add_invoice(&grouping_test_invoice(
                batch_id,
                "10000000000000000031",
                "100.00",
            ))
            .unwrap();
        let second_id = db
            .add_invoice(&grouping_test_invoice(
                batch_id,
                "10000000000000000032",
                "200.00",
            ))
            .unwrap();

        let make_grouping = |title: &str, members: Vec<(i64, usize)>| NewBatchGrouping {
            batch_id,
            rule_version: "test-route-recompute".to_string(),
            home_cities_json: "[\"北京\"]".to_string(),
            overall_confidence: 0.8,
            ambiguities_json: "[]".to_string(),
            groups: vec![NewInvoiceGroup {
                group_index: 0,
                kind: "business_trip".to_string(),
                title: title.to_string(),
                start_date: "2026-06-01".to_string(),
                end_date: "2026-06-02".to_string(),
                confidence: 0.8,
                requires_review: true,
                evidence_json: "{}".to_string(),
                members: members
                    .into_iter()
                    .map(|(invoice_id, input_index)| NewInvoiceGroupMember {
                        invoice_id,
                        input_index,
                        match_reason: "测试归组".to_string(),
                    })
                    .collect(),
            }],
        };

        db.replace_batch_grouping(&make_grouping("旧归组", vec![(first_id, 0)]))
            .unwrap();
        let old_group_id = db.get_batch_grouping(batch_id).unwrap().unwrap().groups[0].id;
        let old_expenses = db.list_expense_items_by_batch(batch_id).unwrap();
        assert_eq!(
            old_expenses
                .iter()
                .find(|expense| expense.primary_invoice_id == first_id)
                .unwrap()
                .trip_group_id,
            Some(old_group_id)
        );
        assert_eq!(
            old_expenses
                .iter()
                .find(|expense| expense.primary_invoice_id == second_id)
                .unwrap()
                .trip_group_id,
            None
        );

        db.replace_batch_grouping_with_audit(&make_grouping(
            "新归组",
            vec![(first_id, 0), (second_id, 1)],
        ))
        .unwrap();
        let new_group_id = db.get_batch_grouping(batch_id).unwrap().unwrap().groups[0].id;
        assert_ne!(new_group_id, old_group_id);
        assert!(db
            .list_expense_items_by_batch(batch_id)
            .unwrap()
            .iter()
            .all(|expense| expense.trip_group_id == Some(new_group_id)));
        assert_eq!(
            db.list_review_actions(batch_id).unwrap()[0].action_type,
            "grouping_recomputed"
        );

        db.undo_last_review_action(batch_id).unwrap();
        let restored_expenses = db.list_expense_items_by_batch(batch_id).unwrap();
        assert_eq!(
            restored_expenses
                .iter()
                .find(|expense| expense.primary_invoice_id == first_id)
                .unwrap()
                .trip_group_id,
            Some(old_group_id)
        );
        assert_eq!(
            restored_expenses
                .iter()
                .find(|expense| expense.primary_invoice_id == second_id)
                .unwrap()
                .trip_group_id,
            None
        );
    }

    #[test]
    fn missing_main_original_relink_updates_invoice_document_and_is_undoable() {
        let db = LedgerDb::new(":memory:").unwrap();
        let batch_id = db.create_batch("原件恢复", "2026-06").unwrap();
        let invoice_id = db
            .add_invoice(&grouping_test_invoice(
                batch_id,
                "10000000000000000041",
                "100.00",
            ))
            .unwrap();
        let expense = db
            .list_expense_items_by_batch(batch_id)
            .unwrap()
            .into_iter()
            .find(|expense| expense.primary_invoice_id == invoice_id)
            .unwrap();
        let main_document_id = expense
            .documents
            .iter()
            .find(|document| document.role == "main_invoice")
            .unwrap()
            .id;

        db.repair_invoice_original_file(
            invoice_id,
            "C:/stable/recovered.xml",
            "invoice.xml",
            "abc123",
        )
        .unwrap();
        assert_eq!(
            db.get_invoice(invoice_id).unwrap().unwrap().file_path,
            "C:/stable/recovered.xml"
        );
        let document = db.get_invoice_document(main_document_id).unwrap().unwrap();
        assert_eq!(document.file_path, "C:/stable/recovered.xml");
        assert_eq!(document.sha256.as_deref(), Some("abc123"));
        assert_eq!(
            db.list_review_actions(batch_id).unwrap()[0].action_type,
            "invoice_original_relinked"
        );

        db.undo_last_review_action(batch_id).unwrap();
        assert_eq!(
            db.get_invoice(invoice_id).unwrap().unwrap().file_path,
            "C:/test/invoice.xml"
        );
        assert_eq!(
            db.get_invoice_document(main_document_id)
                .unwrap()
                .unwrap()
                .file_path,
            "C:/test/invoice.xml"
        );
    }

    #[test]
    fn duplicate_resolution_is_audited_and_undoable() {
        let db = LedgerDb::new(":memory:").unwrap();
        let batch_id = db.create_batch("重复项审核", "2026-06").unwrap();
        let mut invoice = grouping_test_invoice(batch_id, "10000000000000000021", "88.00");
        invoice.is_duplicate = true;
        invoice.duplicate_reason = Some("测试重复".to_string());
        let invoice_id = db.add_invoice(&invoice).unwrap();

        db.resolve_duplicate_with_audit(invoice_id).unwrap();
        assert!(!db.get_invoice(invoice_id).unwrap().unwrap().is_duplicate);
        db.undo_last_review_action(batch_id).unwrap();
        let restored = db.get_invoice(invoice_id).unwrap().unwrap();
        assert!(restored.is_duplicate);
        assert_eq!(restored.duplicate_reason.as_deref(), Some("测试重复"));
    }

    #[test]
    fn exclusion_restore_and_undo_update_reimbursable_totals() {
        use crate::models::{NewInvoiceGroup, NewInvoiceGroupMember};

        let db = LedgerDb::new(":memory:").unwrap();
        let batch_id = db.create_batch("排除审核", "2026-06").unwrap();
        let excluded_id = db
            .add_invoice(&grouping_test_invoice(
                batch_id,
                "10000000000000000041",
                "88.00",
            ))
            .unwrap();
        let kept_id = db
            .add_invoice(&grouping_test_invoice(
                batch_id,
                "10000000000000000042",
                "12.00",
            ))
            .unwrap();
        db.replace_batch_grouping(&NewBatchGrouping {
            batch_id,
            rule_version: "deterministic-v1".to_string(),
            home_cities_json: "[\"北京\"]".to_string(),
            overall_confidence: 0.9,
            ambiguities_json: "[]".to_string(),
            groups: vec![NewInvoiceGroup {
                group_index: 0,
                kind: "local_month".to_string(),
                title: "六月费用".to_string(),
                start_date: "2026-06-01".to_string(),
                end_date: "2026-06-30".to_string(),
                confidence: 0.9,
                requires_review: false,
                evidence_json: "{}".to_string(),
                members: vec![
                    NewInvoiceGroupMember {
                        invoice_id: excluded_id,
                        input_index: 0,
                        match_reason: "月份匹配".to_string(),
                    },
                    NewInvoiceGroupMember {
                        invoice_id: kept_id,
                        input_index: 1,
                        match_reason: "月份匹配".to_string(),
                    },
                ],
            }],
        })
        .unwrap();

        db.set_invoice_excluded_with_audit(excluded_id, true)
            .unwrap();
        assert!(db.is_invoice_excluded(excluded_id).unwrap());
        assert_eq!(
            db.list_excluded_invoice_ids(batch_id).unwrap(),
            vec![excluded_id]
        );
        assert_eq!(
            db.list_reimbursable_invoices_by_batch(batch_id)
                .unwrap()
                .into_iter()
                .map(|invoice| invoice.id)
                .collect::<Vec<_>>(),
            vec![kept_id]
        );
        let batch = db.get_batch(batch_id).unwrap();
        assert_eq!(batch.invoice_count, 1);
        assert_eq!(batch.total_amount, Decimal::from_str("12.00").unwrap());
        assert_eq!(
            db.list_review_actions(batch_id).unwrap()[0].action_type,
            "invoice_excluded"
        );
        assert!(db.get_batch_grouping(batch_id).unwrap().unwrap().groups[0].requires_review);

        db.set_invoice_excluded_with_audit(excluded_id, false)
            .unwrap();
        assert!(!db.is_invoice_excluded(excluded_id).unwrap());
        let batch = db.get_batch(batch_id).unwrap();
        assert_eq!(batch.invoice_count, 2);
        assert_eq!(batch.total_amount, Decimal::from_str("100.00").unwrap());

        db.undo_last_review_action(batch_id).unwrap();
        assert!(db.is_invoice_excluded(excluded_id).unwrap());
        assert_eq!(
            db.get_batch(batch_id).unwrap().total_amount,
            Decimal::from_str("12.00").unwrap()
        );
        db.undo_last_review_action(batch_id).unwrap();
        assert!(!db.is_invoice_excluded(excluded_id).unwrap());
        assert_eq!(
            db.get_batch(batch_id).unwrap().total_amount,
            Decimal::from_str("100.00").unwrap()
        );
        assert!(!db.get_batch_grouping(batch_id).unwrap().unwrap().groups[0].requires_review);

        db.transition_batch_status(batch_id, BatchStatus::Submitted)
            .unwrap();
        assert!(matches!(
            db.set_invoice_excluded_with_audit(excluded_id, true),
            Err(StoreError::Validation(_))
        ));
    }

    #[test]
    fn review_mutations_are_rejected_after_submission() {
        let db = LedgerDb::new(":memory:").unwrap();
        let batch_id = db.create_batch("已提交批次", "2026-06").unwrap();
        let invoice_id = db
            .add_invoice(&grouping_test_invoice(
                batch_id,
                "10000000000000000031",
                "88.00",
            ))
            .unwrap();
        db.transition_batch_status(batch_id, BatchStatus::Submitted)
            .unwrap();
        let invoice = db.get_invoice(invoice_id).unwrap().unwrap();
        let result = db.update_invoice_review_fields(
            invoice_id,
            &InvoiceReviewUpdate {
                invoice_number: invoice.invoice_number,
                issue_date: invoice.issue_date,
                amount: invoice.amount,
                tax_amount: invoice.tax_amount,
                buyer_name: invoice.buyer_name,
                seller_name: invoice.seller_name,
                ticket_type: invoice.ticket_type,
                city: Some("上海".to_string()),
                departure_time: invoice.departure_time,
                checkin_date: invoice.checkin_date,
            },
        );
        assert!(matches!(result, Err(StoreError::Validation(_))));
        assert!(db.list_review_actions(batch_id).unwrap().is_empty());
    }

    #[test]
    fn persists_traceable_grouping_snapshot_and_cascades_with_batch() {
        use crate::models::{NewInvoiceGroup, NewInvoiceGroupMember};

        let db = LedgerDb::new(":memory:").unwrap();
        let batch_id = db.create_batch("归组测试", "2026-06").unwrap();
        let invoice_id = db
            .add_invoice(&grouping_test_invoice(
                batch_id,
                "10000000000000000001",
                "88.00",
            ))
            .unwrap();
        db.replace_batch_grouping(&NewBatchGrouping {
            batch_id,
            rule_version: "deterministic-v1".to_string(),
            home_cities_json: "[\"北京\"]".to_string(),
            overall_confidence: 0.92,
            ambiguities_json: "[]".to_string(),
            groups: vec![NewInvoiceGroup {
                group_index: 0,
                kind: "business_trip".to_string(),
                title: "北京—上海出差".to_string(),
                start_date: "2026-06-01".to_string(),
                end_date: "2026-06-03".to_string(),
                confidence: 0.92,
                requires_review: false,
                evidence_json: "{\"cities\":[\"北京\",\"上海\"]}".to_string(),
                members: vec![NewInvoiceGroupMember {
                    invoice_id,
                    input_index: 0,
                    match_reason: "交通票日期落在行程区间".to_string(),
                }],
            }],
        })
        .unwrap();

        let saved = db.get_batch_grouping(batch_id).unwrap().unwrap();
        assert_eq!(saved.rule_version, "deterministic-v1");
        assert_eq!(saved.groups.len(), 1);
        assert_eq!(saved.groups[0].members[0].invoice_id, invoice_id);
        assert!(saved.groups[0].members[0].match_reason.contains("行程区间"));

        db.delete_batch(batch_id).unwrap();
        assert!(db.get_batch_grouping(batch_id).unwrap().is_none());
    }

    #[test]
    fn grouping_rejects_member_from_another_batch_atomically() {
        use crate::models::{NewInvoiceGroup, NewInvoiceGroupMember};

        let db = LedgerDb::new(":memory:").unwrap();
        let first = db.create_batch("A", "2026-06").unwrap();
        let second = db.create_batch("B", "2026-06").unwrap();
        let foreign_invoice = db
            .add_invoice(&grouping_test_invoice(
                second,
                "10000000000000000002",
                "99.00",
            ))
            .unwrap();
        let result = db.replace_batch_grouping(&NewBatchGrouping {
            batch_id: first,
            rule_version: "deterministic-v1".to_string(),
            home_cities_json: "[]".to_string(),
            overall_confidence: 0.5,
            ambiguities_json: "[]".to_string(),
            groups: vec![NewInvoiceGroup {
                group_index: 0,
                kind: "needs_review".to_string(),
                title: "待复核".to_string(),
                start_date: "2026-06-01".to_string(),
                end_date: "2026-06-01".to_string(),
                confidence: 0.5,
                requires_review: true,
                evidence_json: "{}".to_string(),
                members: vec![NewInvoiceGroupMember {
                    invoice_id: foreign_invoice,
                    input_index: 0,
                    match_reason: "测试".to_string(),
                }],
            }],
        });
        assert!(matches!(result, Err(StoreError::Validation(_))));
        assert!(db.get_batch_grouping(first).unwrap().is_none());
    }

    fn indexed_grouping(member_index: usize) -> IndexedBatchGrouping {
        use crate::models::{IndexedInvoiceGroup, IndexedInvoiceGroupMember};
        IndexedBatchGrouping {
            rule_version: "deterministic-v1".to_string(),
            home_cities_json: "[\"北京\"]".to_string(),
            overall_confidence: 0.9,
            ambiguities_json: "[]".to_string(),
            groups: vec![IndexedInvoiceGroup {
                group_index: 0,
                kind: "local_month".to_string(),
                title: "2026 年 6 月市内消费".to_string(),
                start_date: "2026-06-01".to_string(),
                end_date: "2026-06-30".to_string(),
                confidence: 0.9,
                requires_review: false,
                evidence_json: "{}".to_string(),
                members: vec![IndexedInvoiceGroupMember {
                    input_index: member_index,
                    match_reason: "确定性规则".to_string(),
                }],
            }],
        }
    }

    #[test]
    fn pipeline_run_is_marked_interrupted_and_can_resume() {
        let db = LedgerDb::new(":memory:").unwrap();
        let pipeline_id = "11111111-1111-4111-8111-111111111111";
        db.create_pipeline_run(
            pipeline_id,
            r#"{"batch_name":"测试"}"#,
            "local",
            "C:/data/temp/task",
        )
        .unwrap();
        db.update_pipeline_checkpoint(pipeline_id, "parsed")
            .unwrap();
        assert_eq!(db.mark_running_pipeline_runs_interrupted().unwrap(), 1);

        let interrupted = db.get_pipeline_run(pipeline_id).unwrap();
        assert_eq!(interrupted.stage, "parsed");
        assert_eq!(interrupted.status, "interrupted");
        assert_eq!(db.list_recoverable_pipeline_runs().unwrap().len(), 1);

        db.mark_pipeline_running(pipeline_id).unwrap();
        db.mark_pipeline_interrupted(pipeline_id, "用户已安全停止")
            .unwrap();
        let cancelled = db.get_pipeline_run(pipeline_id).unwrap();
        assert_eq!(cancelled.status, "interrupted");
        assert_eq!(cancelled.stage, "parsed");
        assert_eq!(cancelled.last_error.as_deref(), Some("用户已安全停止"));

        db.mark_pipeline_running(pipeline_id).unwrap();
        db.mark_pipeline_failed(pipeline_id, "模拟失败").unwrap();
        let failed = db.get_pipeline_run(pipeline_id).unwrap();
        assert_eq!(failed.status, "failed");
        assert_eq!(failed.last_error.as_deref(), Some("模拟失败"));
    }

    #[test]
    fn final_pipeline_storage_is_atomic_and_idempotent() {
        let db = LedgerDb::new(":memory:").unwrap();
        let pipeline_id = "22222222-2222-4222-8222-222222222222";
        db.create_pipeline_run(pipeline_id, "{}", "local", "C:/task")
            .unwrap();
        db.update_pipeline_checkpoint(pipeline_id, "grouped")
            .unwrap();
        let invoice = grouping_test_invoice(0, "10000000000000000009", "88.00");
        let batch_id = db
            .store_pipeline_batch_atomic(
                pipeline_id,
                "原子批次",
                "2026-06",
                None,
                std::slice::from_ref(&invoice),
                &indexed_grouping(0),
            )
            .unwrap();

        let run = db.get_pipeline_run(pipeline_id).unwrap();
        assert_eq!(run.status, "completed");
        assert_eq!(run.stage, "review");
        assert_eq!(run.batch_id, Some(batch_id));
        let batch = db.get_batch(batch_id).unwrap();
        assert_eq!(batch.invoice_count, 1);
        assert_eq!(batch.total_amount, Decimal::from_str("88.00").unwrap());
        let grouping = db.get_batch_grouping(batch_id).unwrap().unwrap();
        assert_eq!(grouping.groups[0].members[0].input_index, 0);

        let repeated = db
            .store_pipeline_batch_atomic(
                pipeline_id,
                "原子批次",
                "2026-06",
                None,
                &[invoice],
                &indexed_grouping(0),
            )
            .unwrap();
        assert_eq!(repeated, batch_id);
        assert_eq!(db.list_batches().unwrap().len(), 1);
    }

    #[test]
    fn same_import_alternate_document_cannot_merge_two_expense_groups() {
        use crate::models::{IndexedInvoiceGroup, IndexedInvoiceGroupMember};

        let db = LedgerDb::new(":memory:").unwrap();
        let pipeline_id = "23232323-2323-4232-8232-232323232323";
        db.create_pipeline_run(pipeline_id, "{}", "local", "C:/task")
            .unwrap();
        let first = grouping_test_invoice(0, "10000000000000000101", "88.00");
        let second = grouping_test_invoice(0, "10000000000000000102", "99.00");
        let mut first_alternate = first.clone();
        first_alternate.file_path = "C:/test/invoice-copy.pdf".to_string();
        let grouping = IndexedBatchGrouping {
            rule_version: "deterministic-v1".to_string(),
            home_cities_json: "[\"北京\"]".to_string(),
            overall_confidence: 0.9,
            ambiguities_json: "[]".to_string(),
            groups: vec![
                IndexedInvoiceGroup {
                    group_index: 0,
                    kind: "business_trip".to_string(),
                    title: "第一行程".to_string(),
                    start_date: "2026-06-01".to_string(),
                    end_date: "2026-06-02".to_string(),
                    confidence: 0.9,
                    requires_review: false,
                    evidence_json: "{}".to_string(),
                    members: vec![IndexedInvoiceGroupMember {
                        input_index: 0,
                        match_reason: "唯一费用".to_string(),
                    }],
                },
                IndexedInvoiceGroup {
                    group_index: 1,
                    kind: "local_month".to_string(),
                    title: "六月市内消费".to_string(),
                    start_date: "2026-06-01".to_string(),
                    end_date: "2026-06-30".to_string(),
                    confidence: 0.9,
                    requires_review: false,
                    evidence_json: "{}".to_string(),
                    members: vec![
                        IndexedInvoiceGroupMember {
                            input_index: 1,
                            match_reason: "唯一费用".to_string(),
                        },
                        // 旧归组检查点可能仍把同票另一格式放进第二组；存储层必须
                        // 忽略该冲突，不能借此把两个组整体合并。
                        IndexedInvoiceGroupMember {
                            input_index: 2,
                            match_reason: "同票另一格式".to_string(),
                        },
                    ],
                },
            ],
        };

        let batch_id = db
            .store_pipeline_batch_atomic(
                pipeline_id,
                "同票多格式归组",
                "2026-06",
                None,
                &[first, second, first_alternate],
                &grouping,
            )
            .unwrap();

        let saved = db.get_batch_grouping(batch_id).unwrap().unwrap();
        assert_eq!(saved.groups.len(), 2);
        assert_eq!(saved.groups[0].members.len(), 1);
        assert_eq!(saved.groups[1].members.len(), 1);
        let expenses = db.list_expense_items_by_batch(batch_id).unwrap();
        assert_eq!(expenses.len(), 2);
        assert_eq!(
            expenses
                .iter()
                .map(|expense| expense.trip_group_id)
                .collect::<std::collections::HashSet<_>>()
                .len(),
            2
        );
        let first_documents = LedgerDb::documents_for_expense(&db.conn, expenses[0].id).unwrap();
        assert_eq!(first_documents.len(), 2);
    }

    #[test]
    fn invalid_group_member_rolls_back_entire_pipeline_batch() {
        let db = LedgerDb::new(":memory:").unwrap();
        let pipeline_id = "33333333-3333-4333-8333-333333333333";
        db.create_pipeline_run(pipeline_id, "{}", "local", "C:/task")
            .unwrap();
        let invoice = grouping_test_invoice(0, "10000000000000000010", "99.00");
        let result = db.store_pipeline_batch_atomic(
            pipeline_id,
            "应回滚",
            "2026-06",
            None,
            &[invoice],
            &indexed_grouping(7),
        );
        assert!(matches!(result, Err(StoreError::Validation(_))));
        assert!(db.list_batches().unwrap().is_empty());
        let run = db.get_pipeline_run(pipeline_id).unwrap();
        assert_eq!(run.status, "running");
        assert_eq!(run.batch_id, None);
    }

    fn email_attachment(
        name: &str,
        role_hint: &str,
        invoice_input_index: Option<usize>,
        pending_document_index: Option<usize>,
        manual_import: bool,
    ) -> crate::models::NewEmailImportAttachment {
        crate::models::NewEmailImportAttachment {
            content_sha256: Some(format!("sha256-{name}")),
            original_name: name.to_string(),
            container_name: None,
            mime_type: Some("application/pdf".to_string()),
            byte_len: 128,
            status: "not_invoice".to_string(),
            role_hint: role_hint.to_string(),
            reason: "test-fixture".to_string(),
            is_content_duplicate: false,
            invoice_input_index,
            pending_document_index,
            manual_import,
        }
    }

    fn email_message(
        existing_message_id: Option<i64>,
        uid: i64,
        initial_status: &str,
        attachments: Vec<crate::models::NewEmailImportAttachment>,
    ) -> NewEmailImportMessage {
        NewEmailImportMessage {
            existing_message_id,
            mailbox_folder: "INBOX".to_string(),
            uid,
            message_id_sha256: Some(format!("message-sha256-{uid}")),
            sender: "invoice@example.test".to_string(),
            subject: "电子发票和行程单".to_string(),
            received_at: Some("2026-06-15 08:30:00".to_string()),
            initial_status: initial_status.to_string(),
            error_category: None,
            attachments,
        }
    }

    #[test]
    fn email_ledger_keeps_same_message_invoice_and_supporting_material_together() {
        let db = LedgerDb::new(":memory:").unwrap();
        let pipeline_id = "44444444-4444-4444-8444-444444444444";
        db.create_pipeline_run(pipeline_id, "{}", "email", "C:/task")
            .unwrap();
        let invoice = grouping_test_invoice(0, "10000000000000000021", "128.00");
        let pending = NewPendingInvoiceDocument {
            proposed_role: "itinerary".to_string(),
            file_path: "C:/task/itinerary.pdf".to_string(),
            original_name: "行程单.pdf".to_string(),
            mime_type: Some("application/pdf".to_string()),
            sha256: Some("sha256-itinerary".to_string()),
            detection_reason: "疑似行程单".to_string(),
            auto_assign_invoice_index: None,
        };
        let message = email_message(
            None,
            101,
            "needs_attachment_review",
            vec![
                email_attachment("发票.pdf", "invoice", Some(0), None, false),
                email_attachment("行程单.pdf", "itinerary", None, Some(0), false),
            ],
        );

        let batch_id = db
            .store_pipeline_batch_atomic_with_email_ledger(
                pipeline_id,
                "邮件材料包",
                "2026-06",
                None,
                &[invoice],
                &indexed_grouping(0),
                &[pending],
                &[message],
            )
            .unwrap();

        let messages = db.list_email_import_messages(batch_id).unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].status, "imported");
        assert_eq!(messages[0].resolution_status, "resolved");
        assert_eq!(messages[0].attachments.len(), 2);
        assert!(messages[0]
            .attachments
            .iter()
            .all(|item| item.message_id == messages[0].id));
        assert!(messages[0]
            .attachments
            .iter()
            .any(|item| item.status == "invoice" && item.reported_invoice_id.is_some()));
        assert!(messages[0].attachments.iter().any(|item| {
            item.status == "supporting"
                && item.role_hint == "itinerary"
                && item.pending_document_id.is_some()
        }));
    }

    #[test]
    fn uniquely_matched_pending_document_is_attached_atomically() {
        let db = LedgerDb::new(":memory:").unwrap();
        let pipeline_id = "45454545-4545-4545-8545-454545454545";
        db.create_pipeline_run(pipeline_id, "{}", "local", "C:/task")
            .unwrap();
        let invoice = grouping_test_invoice(0, "10000000000000000031", "35.60");
        let pending = NewPendingInvoiceDocument {
            proposed_role: "itinerary".to_string(),
            file_path: "C:/task/didi-itinerary.pdf".to_string(),
            original_name: "滴滴出行行程报销单.pdf".to_string(),
            mime_type: Some("application/pdf".to_string()),
            sha256: Some("sha256-didi-itinerary".to_string()),
            detection_reason: "itinerary_detected".to_string(),
            auto_assign_invoice_index: Some(0),
        };

        let batch_id = db
            .store_pipeline_batch_atomic_with_documents(
                pipeline_id,
                "自动挂载",
                "2026-06",
                None,
                &[invoice],
                &indexed_grouping(0),
                &[pending],
            )
            .unwrap();

        let pending = db.list_pending_invoice_documents(batch_id).unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].status, "attached");
        let expense = db.list_expense_items_by_batch(batch_id).unwrap().remove(0);
        assert!(expense.documents.iter().any(|document| {
            document.role == "itinerary"
                && document.source_pending_document_id == Some(pending[0].id)
        }));
    }

    #[test]
    fn pending_didi_itinerary_can_create_expense_without_inventing_main_invoice() {
        let db = LedgerDb::new(":memory:").unwrap();
        let batch_id = db.create_batch("滴滴纸质发票", "2026-06").unwrap();
        let now = "2026-06-30 12:00:00";
        db.conn
            .execute(
                "INSERT INTO pending_invoice_documents (
                    batch_id, proposed_role, file_path, original_name, mime_type,
                    sha256, detection_reason, status, assigned_expense_item_id,
                    created_at, updated_at
                 ) VALUES (?1, 'itinerary', ?2, ?3, 'application/pdf', ?4,
                           'itinerary_detected', 'pending', NULL, ?5, ?5)",
                params![
                    batch_id,
                    "C:/task/didi-only-itinerary.pdf",
                    "滴滴出行行程报销单.pdf",
                    "sha256-didi-only-itinerary",
                    now,
                ],
            )
            .unwrap();
        let pending_id = db.conn.last_insert_rowid();
        let start_date = NaiveDate::from_ymd_opt(2026, 6, 18).unwrap();
        let mut invoice = grouping_test_invoice(batch_id, "", "88.00");
        invoice.issue_date = start_date;
        invoice.ticket_type = TicketType::CityTransport;
        invoice.seller_name = Some("滴滴出行".to_string());
        invoice.city = Some("北京".to_string());
        invoice.departure_time = start_date.and_hms_opt(0, 0, 0);
        invoice.file_path = "C:/task/didi-only-itinerary.pdf".to_string();

        let expense = db
            .convert_pending_itinerary_to_expense(pending_id, &invoice, start_date)
            .unwrap();

        assert_eq!(expense.category_code, "city_transport");
        assert!(expense.category_confirmed);
        assert_eq!(expense.transaction_date, start_date);
        assert!(expense.transaction_date_confirmed);
        assert_eq!(expense.counterparty_name, "滴滴出行");
        assert_eq!(expense.gross_amount, Decimal::from_str("88.00").unwrap());
        assert!(expense
            .documents
            .iter()
            .any(|document| document.role == "itinerary"
                && document.source_pending_document_id == Some(pending_id)));
        assert!(expense
            .documents
            .iter()
            .all(|document| document.role != "main_invoice"));
        let pending = db
            .get_pending_invoice_document(pending_id)
            .unwrap()
            .unwrap();
        assert_eq!(pending.status, "attached");
        assert_eq!(pending.assigned_expense_item_id, Some(expense.id));
        let batch = db.get_batch(batch_id).unwrap();
        assert_eq!(batch.invoice_count, 1);
        assert_eq!(batch.total_amount, Decimal::from_str("88.00").unwrap());
        assert!(db
            .convert_pending_itinerary_to_expense(pending_id, &invoice, start_date)
            .is_err());
    }

    #[test]
    fn manual_download_supplement_reuses_message_and_resolves_actionable_item() {
        let db = LedgerDb::new(":memory:").unwrap();
        let first_pipeline = "55555555-5555-4555-8555-555555555555";
        db.create_pipeline_run(first_pipeline, "{}", "email", "C:/first")
            .unwrap();
        let batch_id = db
            .complete_pipeline_with_email_ledger_only(
                first_pipeline,
                "需下载",
                "2026-06",
                None,
                &[email_message(None, 202, "manual_download", vec![])],
            )
            .unwrap();
        let original = db.list_email_import_messages(batch_id).unwrap();
        assert_eq!(original.len(), 1);
        assert_eq!(db.unresolved_actionable_email_count(batch_id).unwrap(), 1);

        let second_pipeline = "66666666-6666-4666-8666-666666666666";
        db.create_pipeline_run(second_pipeline, "{}", "local", "C:/second")
            .unwrap();
        let invoice = grouping_test_invoice(0, "10000000000000000022", "66.00");
        let supplement = email_message(
            Some(original[0].id),
            0,
            "needs_confirmation",
            vec![email_attachment(
                "用户下载发票.pdf",
                "invoice",
                Some(0),
                None,
                true,
            )],
        );
        db.store_pipeline_batch_atomic_with_email_ledger(
            second_pipeline,
            "需下载",
            "2026-06",
            Some(batch_id),
            &[invoice],
            &indexed_grouping(0),
            &[],
            &[supplement],
        )
        .unwrap();

        let updated = db.list_email_import_messages(batch_id).unwrap();
        assert_eq!(updated.len(), 1);
        assert_eq!(updated[0].id, original[0].id);
        assert_eq!(updated[0].status, "imported");
        assert_eq!(updated[0].resolution_status, "resolved");
        assert_eq!(updated[0].attachments.len(), 1);
        assert!(updated[0].attachments[0].manual_import);
        assert!(updated[0].attachments[0].reported_invoice_id.is_some());
        assert_eq!(db.unresolved_actionable_email_count(batch_id).unwrap(), 0);
    }

    #[test]
    fn actionable_email_can_be_resolved_and_reopened_without_changing_semantic_status() {
        let db = LedgerDb::new(":memory:").unwrap();
        let pipeline_id = "77777777-7777-4777-8777-777777777777";
        db.create_pipeline_run(pipeline_id, "{}", "email", "C:/task")
            .unwrap();
        let batch_id = db
            .complete_pipeline_with_email_ledger_only(
                pipeline_id,
                "待确认",
                "2026-06",
                None,
                &[email_message(None, 303, "needs_confirmation", vec![])],
            )
            .unwrap();
        let message_id = db.list_email_import_messages(batch_id).unwrap()[0].id;

        db.resolve_email_import_message(message_id, "ignore")
            .unwrap();
        let ignored = db.list_email_import_messages(batch_id).unwrap();
        assert_eq!(ignored[0].status, "needs_confirmation");
        assert_eq!(ignored[0].resolution_status, "ignored");
        assert_eq!(db.unresolved_actionable_email_count(batch_id).unwrap(), 0);

        db.resolve_email_import_message(message_id, "reopen")
            .unwrap();
        let reopened = db.list_email_import_messages(batch_id).unwrap();
        assert_eq!(reopened[0].status, "needs_confirmation");
        assert_eq!(reopened[0].resolution_status, "open");
        assert_eq!(db.unresolved_actionable_email_count(batch_id).unwrap(), 1);
    }

    fn collected_message(
        uid: i64,
        status: &str,
        stored_path: Option<&str>,
    ) -> NewCollectedEmailMessage {
        NewCollectedEmailMessage {
            mailbox_folder: "INBOX".to_string(),
            uid,
            message_id_sha256: Some(format!("collection-message-{uid}")),
            sender: "invoice@example.test".to_string(),
            subject: "发票与行程材料".to_string(),
            received_at: Some("2026-06-15 08:30".to_string()),
            status: status.to_string(),
            error_category: None,
            review: None,
            attachments: stored_path
                .map(|path| NewCollectedEmailAttachment {
                    content_sha256: Some(format!("collection-file-{uid}")),
                    original_name: "发票.pdf".to_string(),
                    container_name: None,
                    mime_type: Some("application/pdf".to_string()),
                    byte_len: 128,
                    status: "candidate".to_string(),
                    role_hint: "invoice".to_string(),
                    reason: "test-fixture".to_string(),
                    stored_path: Some(path.to_string()),
                    manual_import: false,
                })
                .into_iter()
                .collect(),
        }
    }

    #[test]
    fn email_collection_task_deletion_preserves_active_and_imported_sources() {
        let db = LedgerDb::new(":memory:").unwrap();

        let disposable = db
            .create_email_collection_task(
                "可删除任务",
                "user@example.test",
                "2026-06-01",
                "2026-07-01",
            )
            .unwrap();
        db.delete_email_collection_task(disposable).unwrap();
        assert!(matches!(
            db.get_email_collection_task(disposable),
            Err(StoreError::NotFound(_))
        ));

        let collecting = db
            .create_email_collection_task(
                "正在收集",
                "user@example.test",
                "2026-06-01",
                "2026-07-01",
            )
            .unwrap();
        db.mark_email_collection_started(collecting, "deletion-active-task")
            .unwrap();
        assert!(matches!(
            db.delete_email_collection_task(collecting),
            Err(StoreError::Validation(message)) if message.contains("正在收集")
        ));

        let imported = db
            .create_email_collection_task(
                "已导入任务",
                "user@example.test",
                "2026-06-01",
                "2026-07-01",
            )
            .unwrap();
        db.mark_email_collection_started(imported, "deletion-imported-task")
            .unwrap();
        db.store_email_collection_results(
            imported,
            &[collected_message(
                901,
                "has_candidates",
                Some("collection-files/task-imported/invoice.pdf"),
            )],
        )
        .unwrap();
        let attachment_id =
            db.list_collected_email_messages(imported).unwrap()[0].attachments[0].id;
        let batch_id = db.create_batch("导入来源保护", "2026-06").unwrap();
        db.create_batch_collection_import(batch_id, imported, &[attachment_id])
            .unwrap();
        assert!(matches!(
            db.delete_email_collection_task(imported),
            Err(StoreError::Validation(message)) if message.contains("已导入报销批次")
        ));
        assert!(db.get_email_collection_task(imported).is_ok());
    }

    #[test]
    fn collected_email_review_snapshot_is_persistent_and_replaceable() {
        let db = LedgerDb::new(":memory:").unwrap();
        let task_id = db
            .create_email_collection_task(
                "链接持久化",
                "user@example.test",
                "2026-06-01",
                "2026-07-01",
            )
            .unwrap();
        db.mark_email_collection_started(task_id, "collection-review-cache")
            .unwrap();
        let mut message = collected_message(42, "manual_download", None);
        message.review = Some(NewCollectedEmailReviewSnapshot {
            sender_name: Some("开票平台".to_string()),
            sender_address: Some("billing@example.test".to_string()),
            body_text: "点击查看发票".to_string(),
            body_truncated: false,
            links: vec![crate::models::NewCollectedEmailLink {
                label: "查看发票".to_string(),
                host: "u.example.test".to_string(),
                url: "http://u.example.test/token".to_string(),
                scheme: "http".to_string(),
            }],
        });
        db.store_email_collection_results(task_id, &[message])
            .unwrap();

        let message_id = db.list_collected_email_messages(task_id).unwrap()[0].id;
        let first = db
            .get_collected_email_review_snapshot(message_id)
            .unwrap()
            .unwrap();
        assert_eq!(first.body_text, "点击查看发票");
        assert_eq!(first.links.len(), 1);
        assert_eq!(first.links[0].url, "http://u.example.test/token");
        let old_link_id = first.links[0].id;

        db.replace_collected_email_review_snapshot(
            message_id,
            &NewCollectedEmailReviewSnapshot {
                sender_name: Some("开票平台".to_string()),
                sender_address: Some("billing@example.test".to_string()),
                body_text: "重新分析后的正文".to_string(),
                body_truncated: false,
                links: vec![crate::models::NewCollectedEmailLink {
                    label: "下载发票".to_string(),
                    host: "download.example.test".to_string(),
                    url: "https://download.example.test/new-token".to_string(),
                    scheme: "https".to_string(),
                }],
            },
        )
        .unwrap();

        assert!(db
            .get_collected_email_link(message_id, old_link_id)
            .is_err());
        let replaced = db
            .get_collected_email_review_snapshot(message_id)
            .unwrap()
            .unwrap();
        assert_eq!(replaced.body_text, "重新分析后的正文");
        assert_eq!(replaced.links[0].host, "download.example.test");
    }

    #[test]
    fn qr_analysis_persists_safe_link_and_only_filters_qr_dominant_image() {
        let db = LedgerDb::new(":memory:").unwrap();
        let task_id = db
            .create_email_collection_task(
                "二维码材料",
                "user@example.test",
                "2026-06-01",
                "2026-07-01",
            )
            .unwrap();
        db.mark_email_collection_started(task_id, "collection-qr-analysis")
            .unwrap();

        let mut qr_message = collected_message(
            81,
            "has_candidates",
            Some("collection-files/task-1/download-qr.png"),
        );
        qr_message.attachments[0].original_name = "download-qr.png".to_string();
        qr_message.attachments[0].mime_type = Some("image/png".to_string());
        qr_message.review = Some(NewCollectedEmailReviewSnapshot {
            sender_name: None,
            sender_address: Some("billing@example.test".to_string()),
            body_text: "请扫描二维码下载发票".to_string(),
            body_truncated: false,
            links: vec![],
        });

        let mut bill_message = collected_message(
            82,
            "has_candidates",
            Some("collection-files/task-1/hotel-bill.png"),
        );
        bill_message.attachments[0].original_name = "hotel-bill.png".to_string();
        bill_message.attachments[0].mime_type = Some("image/png".to_string());
        bill_message.review = qr_message.review.clone();
        db.store_email_collection_results(task_id, &[qr_message, bill_message])
            .unwrap();

        let messages = db.list_collected_email_messages(task_id).unwrap();
        let qr = messages.iter().find(|message| message.uid == 81).unwrap();
        let bill = messages.iter().find(|message| message.uid == 82).unwrap();
        db.store_collected_attachment_qr_analysis(
            qr.attachments[0].id,
            &[NewCollectedEmailLink {
                label: "打开图片中提取的二维码地址".to_string(),
                host: "invoice.example.test".to_string(),
                url: "https://invoice.example.test/download/token".to_string(),
                scheme: "https".to_string(),
            }],
            true,
        )
        .unwrap();
        db.store_collected_attachment_qr_analysis(
            bill.attachments[0].id,
            &[NewCollectedEmailLink {
                label: "打开图片中提取的二维码地址".to_string(),
                host: "hotel.example.test".to_string(),
                url: "https://hotel.example.test/bill/token".to_string(),
                scheme: "https".to_string(),
            }],
            false,
        )
        .unwrap();

        let qr_after = db.get_collected_email_message(qr.id).unwrap();
        assert_eq!(qr_after.status, "manual_download");
        assert_eq!(qr_after.attachments[0].status, "filtered");
        assert_eq!(qr_after.attachments[0].role_hint, "supporting");
        assert_eq!(
            qr_after.attachments[0].reason,
            "attachment_qr_manual_download"
        );
        assert_eq!(
            db.get_collected_email_review_snapshot(qr.id)
                .unwrap()
                .unwrap()
                .links[0]
                .host,
            "invoice.example.test"
        );

        let bill_after = db.get_collected_email_message(bill.id).unwrap();
        assert_eq!(bill_after.status, "has_candidates");
        assert_eq!(bill_after.attachments[0].status, "candidate");
        assert_eq!(bill_after.attachments[0].role_hint, "invoice");
        assert_eq!(
            bill_after.attachments[0].reason,
            "attachment_contains_qr_link"
        );
    }

    #[test]
    fn explicit_reanalysis_replaces_automatic_attachments_and_reopens_message() {
        let db = LedgerDb::new(":memory:").unwrap();
        let task_id = db
            .create_email_collection_task(
                "附件重新分析",
                "user@example.test",
                "2026-06-01",
                "2026-07-01",
            )
            .unwrap();
        db.mark_email_collection_started(task_id, "collection-reanalysis")
            .unwrap();
        let mut original = collected_message(
            77,
            "has_candidates",
            Some("collection-files/task-1/corrupt.pdf"),
        );
        original.review = Some(NewCollectedEmailReviewSnapshot {
            sender_name: None,
            sender_address: Some("billing@example.test".to_string()),
            body_text: "旧正文".to_string(),
            body_truncated: false,
            links: vec![],
        });
        db.store_email_collection_results(task_id, &[original])
            .unwrap();
        let before = db.list_collected_email_messages(task_id).unwrap();
        let message_id = before[0].id;
        let old_attachment_id = before[0].attachments[0].id;
        db.resolve_collected_email_message(message_id, "resolve")
            .unwrap();

        let mut replacement = collected_message(
            77,
            "has_candidates",
            Some("collection-files/task-1/original-bytes.pdf"),
        );
        replacement.attachments[0].content_sha256 = Some("original-byte-hash".to_string());
        replacement.review = Some(NewCollectedEmailReviewSnapshot {
            sender_name: Some("开票平台".to_string()),
            sender_address: Some("billing@example.test".to_string()),
            body_text: "重新分析正文".to_string(),
            body_truncated: false,
            links: vec![],
        });
        db.replace_collected_email_analysis(message_id, &replacement)
            .unwrap();

        assert!(db
            .get_collected_email_attachment(old_attachment_id)
            .is_err());
        let after = db.get_collected_email_message(message_id).unwrap();
        assert_eq!(after.resolution_status, "open");
        assert_eq!(after.attachments.len(), 1);
        assert_eq!(
            after.attachments[0].content_sha256.as_deref(),
            Some("original-byte-hash")
        );
        assert_eq!(
            after.attachments[0].stored_path.as_deref(),
            Some("collection-files/task-1/original-bytes.pdf")
        );
        assert_eq!(
            db.get_collected_email_review_snapshot(message_id)
                .unwrap()
                .unwrap()
                .body_text,
            "重新分析正文"
        );
    }

    #[test]
    fn independent_collection_task_requires_actionable_review_before_completion() {
        let db = LedgerDb::new(":memory:").unwrap();
        let task_id = db
            .create_email_collection_task(
                "六月邮件收集",
                "user@example.test",
                "2026-06-01",
                "2026-07-01",
            )
            .unwrap();
        db.mark_email_collection_started(task_id, "collection-run-1")
            .unwrap();
        db.store_email_collection_results(
            task_id,
            &[
                collected_message(1, "has_candidates", Some("C:/materials/invoice.pdf")),
                collected_message(2, "manual_download", None),
            ],
        )
        .unwrap();

        let task = db.get_email_collection_task(task_id).unwrap();
        assert_eq!(task.status, "review");
        assert_eq!(task.scanned_message_count, 2);
        assert_eq!(task.candidate_file_count, 1);
        assert_eq!(task.actionable_message_count, 1);
        assert!(db.complete_email_collection_review(task_id).is_err());

        let messages = db.list_collected_email_messages(task_id).unwrap();
        let manual = messages
            .iter()
            .find(|message| message.status == "manual_download")
            .unwrap();
        db.resolve_collected_email_message(manual.id, "ignore")
            .unwrap();
        db.complete_email_collection_review(task_id).unwrap();
        let completed = db.get_email_collection_task(task_id).unwrap();
        assert_eq!(completed.status, "completed");
        assert_eq!(completed.review_status, "completed");
    }

    #[test]
    fn user_excluded_collection_attachment_is_retained_but_not_importable() {
        let db = LedgerDb::new(":memory:").unwrap();
        let task_id = db
            .create_email_collection_task(
                "无效附件审核",
                "user@example.test",
                "2026-06-01",
                "2026-07-01",
            )
            .unwrap();
        db.mark_email_collection_started(task_id, "collection-exclusion")
            .unwrap();
        db.store_email_collection_results(
            task_id,
            &[collected_message(
                9,
                "has_candidates",
                Some("C:/materials/header.jpg"),
            )],
        )
        .unwrap();
        db.complete_email_collection_review(task_id).unwrap();

        let attachment_id = db.list_collected_email_messages(task_id).unwrap()[0].attachments[0].id;
        db.set_collected_email_attachment_excluded(attachment_id, true)
            .unwrap();

        let excluded = db.get_collected_email_attachment(attachment_id).unwrap();
        assert!(excluded.user_excluded);
        assert!(excluded.user_excluded_at.is_some());
        assert_eq!(
            excluded.stored_path.as_deref(),
            Some("C:/materials/header.jpg")
        );
        let reopened_task = db.get_email_collection_task(task_id).unwrap();
        assert_eq!(reopened_task.status, "review");
        assert_eq!(reopened_task.review_status, "open");
        assert_eq!(reopened_task.candidate_file_count, 0);
        let batch_id = db.create_batch("排除附件批次", "2026-06").unwrap();
        assert!(db
            .create_batch_collection_import(batch_id, task_id, &[attachment_id])
            .is_err());

        db.set_collected_email_attachment_excluded(attachment_id, false)
            .unwrap();
        let restored = db.get_collected_email_attachment(attachment_id).unwrap();
        assert!(!restored.user_excluded);
        assert!(restored.user_excluded_at.is_none());
        assert_eq!(
            db.get_email_collection_task(task_id)
                .unwrap()
                .candidate_file_count,
            1
        );
        db.create_batch_collection_import(batch_id, task_id, &[attachment_id])
            .unwrap();
    }

    #[test]
    fn collected_messages_are_newest_first_with_missing_dates_last() {
        let db = LedgerDb::new(":memory:").unwrap();
        let task_id = db
            .create_email_collection_task(
                "排序检查",
                "user@example.test",
                "2026-06-01",
                "2026-07-01",
            )
            .unwrap();
        db.mark_email_collection_started(task_id, "collection-sort")
            .unwrap();
        let mut older = collected_message(1, "not_relevant", None);
        older.received_at = Some("2026-06-01 08:00".to_string());
        let mut newer = collected_message(2, "not_relevant", None);
        newer.received_at = Some("2026-06-30 20:00".to_string());
        let mut missing = collected_message(3, "not_relevant", None);
        missing.received_at = None;
        db.store_email_collection_results(task_id, &[older, missing, newer])
            .unwrap();

        let messages = db.list_collected_email_messages(task_id).unwrap();
        assert_eq!(
            messages
                .iter()
                .map(|message| message.uid)
                .collect::<Vec<_>>(),
            vec![2, 1, 3]
        );
        assert_eq!(
            db.get_collected_email_message(messages[0].id).unwrap().uid,
            2
        );
    }

    #[test]
    fn batch_collection_import_is_immutable_and_reports_cross_batch_usage() {
        let db = LedgerDb::new(":memory:").unwrap();
        let task_id = db
            .create_email_collection_task(
                "可复用材料",
                "user@example.test",
                "2026-06-01",
                "2026-07-01",
            )
            .unwrap();
        db.mark_email_collection_started(task_id, "collection-run-2")
            .unwrap();
        db.store_email_collection_results(
            task_id,
            &[collected_message(
                3,
                "has_candidates",
                Some("C:/materials/stable.pdf"),
            )],
        )
        .unwrap();
        db.complete_email_collection_review(task_id).unwrap();
        let attachment_id = db.list_collected_email_messages(task_id).unwrap()[0].attachments[0].id;
        let first_batch = db.create_batch("第一批", "2026-06").unwrap();
        let second_batch = db.create_batch("第二批", "2026-06").unwrap();

        let import_id = db
            .create_batch_collection_import(first_batch, task_id, &[attachment_id])
            .unwrap();
        let pipeline_id = "88888888-8888-4888-8888-888888888888";
        db.create_pipeline_run(pipeline_id, "{}", "collection_import", "C:/task")
            .unwrap();
        db.link_batch_collection_import_pipeline(import_id, pipeline_id)
            .unwrap();
        assert_eq!(
            db.collection_import_file_paths(import_id, first_batch)
                .unwrap(),
            vec!["C:/materials/stable.pdf".to_string()]
        );
        assert!(db
            .collection_import_file_paths(import_id, second_batch)
            .is_err());

        let attachment = &db.list_collected_email_messages(task_id).unwrap()[0].attachments[0];
        assert_eq!(attachment.used_batch_ids, vec![first_batch]);
        assert_eq!(attachment.used_batch_names, vec!["第一批".to_string()]);
        db.mark_batch_collection_import_completed(pipeline_id)
            .unwrap();
        assert_eq!(
            db.list_batch_collection_imports(first_batch).unwrap()[0].status,
            "completed"
        );
    }
}
