//! ledger.db 管理模块
//!
//! 管理报销批次和已报销发票记录

use rusqlite::{params, Connection, Row};
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
                file_path, created_at, updated_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
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
                    file_path, created_at, updated_at
             FROM reported_invoices WHERE batch_id = ?1 ORDER BY issue_date"
        )?;

        let invoices = stmt
            .query_map(params![batch_id], Self::parse_invoice_row)?
            .collect::<Result<Vec<_>, _>>()?;

        Ok(invoices)
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
        };

        let invoice_id = db.add_invoice(&invoice).unwrap();
        db.delete_invoice(invoice_id).unwrap();

        let batch = db.get_batch(batch_id).unwrap();
        assert_eq!(batch.invoice_count, 0);
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
}
