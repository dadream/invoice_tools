//! ledger.db 管理模块
//!
//! 管理报销批次和已报销发票记录

use rusqlite::{params, Connection, OptionalExtension, Row};
use chrono::{Utc, NaiveDate, NaiveDateTime};
use rust_decimal::Decimal;
use std::path::Path;
use std::str::FromStr;

use crate::{
    models::{Batch, BatchStatus, ReportedInvoice, TicketType},
    StoreError, StoreResult,
};

/// ledger.db 管理器
pub struct LedgerDb {
    conn: Connection,
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

    /// 初始化数据库表结构
    fn init_schema(&self) -> StoreResult<()> {
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

        // 数据库迁移：user_version 0 → 1（添加验签与去重字段）
        self.migrate_schema()?;

        Ok(())
    }

    /// 执行数据库迁移
    fn migrate_schema(&self) -> StoreResult<()> {
        let version: i32 = self.conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;

        match version {
            0 => {
                // 检查字段是否已存在（避免重复迁移）
                let column_exists: Result<String, _> = self.conn.query_row(
                    "SELECT sql FROM sqlite_master WHERE type='table' AND name='reported_invoices'",
                    [],
                    |row| row.get(0)
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
            }
            1 => {
                // 当前版本，无需迁移
            }
            _ => {
                return Err(StoreError::Internal(format!(
                    "Unknown database version: {}",
                    version
                )));
            }
        }

        Ok(())
    }

    /// 创建批次
    pub fn create_batch(&self, name: &str, month: &str) -> StoreResult<i64> {
        let now = Utc::now().naive_utc().format("%Y-%m-%d %H:%M:%S").to_string();

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
        let now = Utc::now().naive_utc().format("%Y-%m-%d %H:%M:%S").to_string();

        // 4. 根据目标状态设置相应的时间戳
        let (submitted_at, approved_at, completed_at, rejected_at) = match new_status {
            BatchStatus::Submitted => (Some(now.clone()), None, None, None),
            BatchStatus::Approved => (current_batch.submitted_at.map(|dt| dt.format("%Y-%m-%d %H:%M:%S").to_string()), Some(now.clone()), None, None),
            BatchStatus::Completed => (
                current_batch.submitted_at.map(|dt| dt.format("%Y-%m-%d %H:%M:%S").to_string()),
                current_batch.approved_at.map(|dt| dt.format("%Y-%m-%d %H:%M:%S").to_string()),
                Some(now.clone()),
                None
            ),
            BatchStatus::Rejected => (
                current_batch.submitted_at.map(|dt| dt.format("%Y-%m-%d %H:%M:%S").to_string()),
                current_batch.approved_at.map(|dt| dt.format("%Y-%m-%d %H:%M:%S").to_string()),
                current_batch.completed_at.map(|dt| dt.format("%Y-%m-%d %H:%M:%S").to_string()),
                Some(now.clone())
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
        let now = Utc::now().naive_utc().format("%Y-%m-%d %H:%M:%S").to_string();

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
        let rows = self.conn.execute("DELETE FROM batches WHERE id = ?1", params![id])?;

        if rows == 0 {
            return Err(StoreError::NotFound(format!("Batch {}", id)));
        }

        Ok(())
    }

    /// 添加发票到批次
    pub fn add_invoice(&self, invoice: &ReportedInvoice) -> StoreResult<i64> {
        let now = Utc::now().naive_utc().format("%Y-%m-%d %H:%M:%S").to_string();

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

    /// 按发票号码查找已报销记录（全库范围，跨批次查重）
    ///
    /// 返回 `None` 表示该发票尚未被任何批次使用。
    /// 注意：`query_row` 无行时返回 `QueryReturnedNoRows`（会落进 `StoreError::Database`），
    /// 因此必须用 `optional()` 把"查不到"与"查询出错"区分开。
    pub fn find_invoice_by_number(&self, invoice_number: &str) -> StoreResult<Option<ReportedInvoice>> {
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

        let rows = self.conn.execute(
            "DELETE FROM reported_invoices WHERE id = ?1",
            params![id],
        )?;

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

    /// 更新批次统计信息（总金额和发票数量）
    fn update_batch_stats(&self, batch_id: i64) -> StoreResult<()> {
        // 先获取所有金额，手动求和（避免 SQL 类型转换问题）
        let mut stmt = self.conn.prepare(
            "SELECT amount FROM reported_invoices WHERE batch_id = ?1"
        )?;

        let amounts: Vec<String> = stmt
            .query_map(params![batch_id], |row| row.get(0))?
            .collect::<Result<Vec<_>, _>>()?;

        let count = amounts.len() as i32;
        let total: Decimal = amounts.iter()
            .filter_map(|s| Decimal::from_str(s).ok())
            .sum();

        let now = Utc::now().naive_utc().format("%Y-%m-%d %H:%M:%S").to_string();

        self.conn.execute(
            "UPDATE batches SET invoice_count = ?1, total_amount = ?2, updated_at = ?3 WHERE id = ?4",
            params![count, total.to_string(), now, batch_id],
        )?;

        Ok(())
    }

    /// 解析批次行
    fn parse_batch_row(row: &Row) -> Result<Batch, rusqlite::Error> {
        let status_i32: i32 = row.get(3)?;
        let status = BatchStatus::from_i32(status_i32)
            .ok_or_else(|| rusqlite::Error::FromSqlConversionFailure(3, rusqlite::types::Type::Integer, Box::new(StoreError::Internal(format!("Invalid batch status: {}", status_i32)))))?;

        let total_amount_str: String = row.get(4)?;
        let total_amount = Decimal::from_str(&total_amount_str)
            .map_err(|e| rusqlite::Error::FromSqlConversionFailure(4, rusqlite::types::Type::Text, Box::new(e)))?;

        Ok(Batch {
            id: row.get(0)?,
            name: row.get(1)?,
            month: row.get(2)?,
            status,
            total_amount,
            invoice_count: row.get(5)?,
            created_at: NaiveDateTime::parse_from_str(&row.get::<_, String>(6)?, "%Y-%m-%d %H:%M:%S")
                .map_err(|e| rusqlite::Error::FromSqlConversionFailure(6, rusqlite::types::Type::Text, Box::new(e)))?,
            updated_at: NaiveDateTime::parse_from_str(&row.get::<_, String>(7)?, "%Y-%m-%d %H:%M:%S")
                .map_err(|e| rusqlite::Error::FromSqlConversionFailure(7, rusqlite::types::Type::Text, Box::new(e)))?,
            submitted_at: row.get::<_, Option<String>>(8)?
                .and_then(|s| NaiveDateTime::parse_from_str(&s, "%Y-%m-%d %H:%M:%S").ok()),
            approved_at: row.get::<_, Option<String>>(9)?
                .and_then(|s| NaiveDateTime::parse_from_str(&s, "%Y-%m-%d %H:%M:%S").ok()),
            completed_at: row.get::<_, Option<String>>(10)?
                .and_then(|s| NaiveDateTime::parse_from_str(&s, "%Y-%m-%d %H:%M:%S").ok()),
            rejected_at: row.get::<_, Option<String>>(11)?
                .and_then(|s| NaiveDateTime::parse_from_str(&s, "%Y-%m-%d %H:%M:%S").ok()),
        })
    }

    /// 解析发票行
    fn parse_invoice_row(row: &Row) -> Result<ReportedInvoice, rusqlite::Error> {
        let amount_str: String = row.get(4)?;
        let amount = Decimal::from_str(&amount_str)
            .map_err(|e| rusqlite::Error::FromSqlConversionFailure(4, rusqlite::types::Type::Text, Box::new(e)))?;

        let tax_amount = row.get::<_, Option<String>>(5)?
            .and_then(|s| Decimal::from_str(&s).ok());

        let ticket_type_str: String = row.get(8)?;
        let ticket_type = TicketType::from_str(&ticket_type_str)
            .ok_or_else(|| rusqlite::Error::FromSqlConversionFailure(8, rusqlite::types::Type::Text, Box::new(StoreError::Internal(format!("Invalid ticket type: {}", ticket_type_str)))))?;

        let is_duplicate_int: i32 = row.get(16)?;

        Ok(ReportedInvoice {
            id: row.get(0)?,
            batch_id: row.get(1)?,
            invoice_number: row.get(2)?,
            issue_date: NaiveDate::parse_from_str(&row.get::<_, String>(3)?, "%Y-%m-%d")
                .map_err(|e| rusqlite::Error::FromSqlConversionFailure(3, rusqlite::types::Type::Text, Box::new(e)))?,
            amount,
            tax_amount,
            buyer_name: row.get(6)?,
            seller_name: row.get(7)?,
            ticket_type,
            city: row.get(9)?,
            departure_time: row.get::<_, Option<String>>(10)?
                .and_then(|s| NaiveDateTime::parse_from_str(&s, "%Y-%m-%d %H:%M:%S").ok()),
            checkin_date: row.get::<_, Option<String>>(11)?
                .and_then(|s| NaiveDate::parse_from_str(&s, "%Y-%m-%d").ok()),
            file_path: row.get(12)?,
            created_at: NaiveDateTime::parse_from_str(&row.get::<_, String>(13)?, "%Y-%m-%d %H:%M:%S")
                .map_err(|e| rusqlite::Error::FromSqlConversionFailure(13, rusqlite::types::Type::Text, Box::new(e)))?,
            updated_at: NaiveDateTime::parse_from_str(&row.get::<_, String>(14)?, "%Y-%m-%d %H:%M:%S")
                .map_err(|e| rusqlite::Error::FromSqlConversionFailure(14, rusqlite::types::Type::Text, Box::new(e)))?,
            verification_result: row.get(15)?,
            is_duplicate: is_duplicate_int != 0,
            duplicate_reason: row.get(17)?,
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
        db.update_batch_status(id, BatchStatus::Submitted).unwrap();

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

        db.transition_batch_status(id, BatchStatus::Submitted).unwrap();

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

        db.transition_batch_status(id, BatchStatus::Rejected).unwrap();

        let batch = db.get_batch(id).unwrap();
        assert_eq!(batch.status, BatchStatus::Rejected);
        assert!(batch.rejected_at.is_some());
        assert!(batch.submitted_at.is_none());
    }

    #[test]
    fn test_transition_submitted_to_approved() {
        let db = LedgerDb::new(":memory:").unwrap();
        let id = db.create_batch("测试批次", "2026-07").unwrap();

        db.transition_batch_status(id, BatchStatus::Submitted).unwrap();
        db.transition_batch_status(id, BatchStatus::Approved).unwrap();

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

        db.transition_batch_status(id, BatchStatus::Submitted).unwrap();
        db.transition_batch_status(id, BatchStatus::Rejected).unwrap();

        let batch = db.get_batch(id).unwrap();
        assert_eq!(batch.status, BatchStatus::Rejected);
        assert!(batch.submitted_at.is_some());
        assert!(batch.rejected_at.is_some());
    }

    #[test]
    fn test_transition_approved_to_completed() {
        let db = LedgerDb::new(":memory:").unwrap();
        let id = db.create_batch("测试批次", "2026-07").unwrap();

        db.transition_batch_status(id, BatchStatus::Submitted).unwrap();
        db.transition_batch_status(id, BatchStatus::Approved).unwrap();
        db.transition_batch_status(id, BatchStatus::Completed).unwrap();

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

        db.transition_batch_status(id, BatchStatus::Submitted).unwrap();
        db.transition_batch_status(id, BatchStatus::Approved).unwrap();
        db.transition_batch_status(id, BatchStatus::Rejected).unwrap();

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

        db.transition_batch_status(id, BatchStatus::Submitted).unwrap();
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

        db.transition_batch_status(id, BatchStatus::Submitted).unwrap();
        db.transition_batch_status(id, BatchStatus::Approved).unwrap();
        db.transition_batch_status(id, BatchStatus::Completed).unwrap();

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

        db.transition_batch_status(id, BatchStatus::Rejected).unwrap();

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
            Err(StoreError::Database(_)) => {},
            _ => panic!("Expected Database error for nonexistent batch"),
        }
    }

    #[test]
    fn test_timestamp_preservation() {
        let db = LedgerDb::new(":memory:").unwrap();
        let id = db.create_batch("测试批次", "2026-07").unwrap();

        db.transition_batch_status(id, BatchStatus::Submitted).unwrap();
        let batch1 = db.get_batch(id).unwrap();
        let submitted_at = batch1.submitted_at.unwrap();

        // 等待一小段时间确保时间戳不同
        std::thread::sleep(std::time::Duration::from_millis(10));

        db.transition_batch_status(id, BatchStatus::Approved).unwrap();
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
        db.transition_batch_status(id, BatchStatus::Submitted).unwrap();
        let batch = db.get_batch(id).unwrap();
        assert_eq!(batch.status, BatchStatus::Submitted);
        assert!(batch.submitted_at.is_some());

        // Submitted → Approved
        db.transition_batch_status(id, BatchStatus::Approved).unwrap();
        let batch = db.get_batch(id).unwrap();
        assert_eq!(batch.status, BatchStatus::Approved);
        assert!(batch.approved_at.is_some());

        // Approved → Completed
        db.transition_batch_status(id, BatchStatus::Completed).unwrap();
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
        assert_eq!(retrieved.is_duplicate, true);
        assert_eq!(retrieved.duplicate_reason, Some("发票号完全一致".to_string()));

        // 通过 list_invoices_by_batch 读取
        let invoices = db.list_invoices_by_batch(batch_id).unwrap();
        assert_eq!(invoices.len(), 1);
        assert_eq!(invoices[0].verification_result, Some("valid".to_string()));
        assert_eq!(invoices[0].is_duplicate, true);
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
        let duplicates = db.find_potential_duplicates(
            "12345678901234567890",
            &Decimal::from_str("200.00").unwrap(), // 不同金额
            &NaiveDate::from_ymd_opt(2026, 7, 20).unwrap(), // 不同日期
            "flight", // 不同票种
            None,
        ).unwrap();

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
        let duplicates = db.find_potential_duplicates(
            "22222222222222222222", // 不同发票号
            &Decimal::from_str("100.00").unwrap(),
            &NaiveDate::from_ymd_opt(2026, 7, 15).unwrap(),
            "rail",
            None,
        ).unwrap();

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
        let duplicates = db.find_potential_duplicates(
            "12345678901234567890",
            &Decimal::from_str("100.00").unwrap(),
            &NaiveDate::from_ymd_opt(2026, 7, 15).unwrap(),
            "rail",
            Some(invoice_id),
        ).unwrap();

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
        let duplicates = db.find_potential_duplicates(
            "99999999999999999999",
            &Decimal::from_str("200.00").unwrap(),
            &NaiveDate::from_ymd_opt(2026, 7, 20).unwrap(),
            "flight",
            None,
        ).unwrap();

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
        let duplicates = db.find_potential_duplicates(
            "33333333333333333333",
            &Decimal::from_str("100.00").unwrap(),
            &NaiveDate::from_ymd_opt(2026, 7, 15).unwrap(),
            "rail",
            None,
        ).unwrap();

        assert_eq!(duplicates.len(), 2);
    }

    #[test]
    fn test_database_migration_from_v0_to_v1() {
        use rusqlite::Connection;
        use std::path::PathBuf;
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
            ).unwrap();

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
            ).unwrap();

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
            ).unwrap();

            // user_version 保持为 0
            let version: i32 = conn.query_row("PRAGMA user_version", [], |row| row.get(0)).unwrap();
            assert_eq!(version, 0);
        }

        // 步骤2：用 LedgerDb::new 打开，触发迁移
        let db = LedgerDb::new(&db_path).unwrap();

        // 步骤3：验证迁移成功
        let version: i32 = db.conn.query_row("PRAGMA user_version", [], |row| row.get(0)).unwrap();
        assert_eq!(version, 1);

        // 步骤4：验证旧数据可读，新字段有默认值
        let invoice = db.get_invoice(1).unwrap().expect("旧数据应存在");
        assert_eq!(invoice.invoice_number, "12345678901234567890");
        assert_eq!(invoice.verification_result, None);
        assert_eq!(invoice.is_duplicate, false); // 默认 0
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
        assert_eq!(retrieved.is_duplicate, true);
        assert_eq!(retrieved.duplicate_reason, Some("测试重复".to_string()));
    }
}
