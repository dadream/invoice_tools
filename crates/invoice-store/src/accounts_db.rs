//! accounts.db 管理模块
//!
//! 管理邮箱账号和加密凭证

use rusqlite::{params, Connection, Row};
use chrono::{Utc, NaiveDateTime};
use std::path::Path;

use crate::{
    crypto, keychain,
    models::{Account, Credential},
    StoreError, StoreResult,
};

/// accounts.db 管理器
pub struct AccountsDb {
    conn: Connection,
}

impl AccountsDb {
    /// 打开或创建 accounts.db 数据库
    ///
    /// 如果数据库不存在，会自动创建表结构
    pub fn new<P: AsRef<Path>>(db_path: P) -> StoreResult<Self> {
        let conn = Connection::open(db_path)?;

        // 启用外键约束
        conn.execute("PRAGMA foreign_keys = ON", [])?;

        let db = Self { conn };
        db.init_schema()?;

        Ok(db)
    }

    /// 初始化数据库表结构
    fn init_schema(&self) -> StoreResult<()> {
        // 创建 accounts 表
        self.conn.execute(
            "CREATE TABLE IF NOT EXISTS accounts (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                email TEXT NOT NULL UNIQUE,
                imap_server TEXT NOT NULL,
                imap_port INTEGER NOT NULL,
                enabled INTEGER NOT NULL DEFAULT 1,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            )",
            [],
        )?;

        // 创建 credentials 表
        self.conn.execute(
            "CREATE TABLE IF NOT EXISTS credentials (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                account_id INTEGER NOT NULL UNIQUE,
                encrypted_password BLOB NOT NULL,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                FOREIGN KEY (account_id) REFERENCES accounts(id) ON DELETE CASCADE
            )",
            [],
        )?;

        Ok(())
    }

    /// 创建账号
    pub fn create_account(
        &self,
        email: &str,
        imap_server: &str,
        imap_port: u16,
    ) -> StoreResult<i64> {
        let now = Utc::now().naive_utc().format("%Y-%m-%d %H:%M:%S").to_string();

        self.conn.execute(
            "INSERT INTO accounts (email, imap_server, imap_port, enabled, created_at, updated_at)
             VALUES (?1, ?2, ?3, 1, ?4, ?5)",
            params![email, imap_server, imap_port, now, now],
        )?;

        Ok(self.conn.last_insert_rowid())
    }

    /// 获取账号
    pub fn get_account(&self, id: i64) -> StoreResult<Account> {
        let account = self.conn.query_row(
            "SELECT id, email, imap_server, imap_port, enabled, created_at, updated_at
             FROM accounts WHERE id = ?1",
            params![id],
            Self::parse_account_row,
        )?;

        Ok(account)
    }

    /// 根据邮箱地址获取账号
    pub fn get_account_by_email(&self, email: &str) -> StoreResult<Account> {
        let account = self.conn.query_row(
            "SELECT id, email, imap_server, imap_port, enabled, created_at, updated_at
             FROM accounts WHERE email = ?1",
            params![email],
            Self::parse_account_row,
        )?;

        Ok(account)
    }

    /// 列出所有账号
    pub fn list_accounts(&self) -> StoreResult<Vec<Account>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, email, imap_server, imap_port, enabled, created_at, updated_at
             FROM accounts ORDER BY id"
        )?;

        let accounts = stmt
            .query_map([], Self::parse_account_row)?
            .collect::<Result<Vec<_>, _>>()?;

        Ok(accounts)
    }

    /// 更新账号
    pub fn update_account(&self, account: &Account) -> StoreResult<()> {
        let now = Utc::now().naive_utc().format("%Y-%m-%d %H:%M:%S").to_string();

        let rows = self.conn.execute(
            "UPDATE accounts SET email = ?1, imap_server = ?2, imap_port = ?3,
             enabled = ?4, updated_at = ?5 WHERE id = ?6",
            params![
                account.email,
                account.imap_server,
                account.imap_port,
                account.enabled as i32,
                now,
                account.id
            ],
        )?;

        if rows == 0 {
            return Err(StoreError::NotFound(format!("Account {}", account.id)));
        }

        Ok(())
    }

    /// 删除账号（级联删除凭证）
    pub fn delete_account(&self, id: i64) -> StoreResult<()> {
        let rows = self.conn.execute("DELETE FROM accounts WHERE id = ?1", params![id])?;

        if rows == 0 {
            return Err(StoreError::NotFound(format!("Account {}", id)));
        }

        Ok(())
    }

    /// 设置加密凭证
    ///
    /// 使用主密钥加密密码后存储
    pub fn set_credential(&self, account_id: i64, password: &str) -> StoreResult<()> {
        // 获取主密钥
        let master_key = keychain::get_or_create_master_key()?;

        // 加密密码
        let encrypted_password = crypto::encrypt(password, &master_key)?;

        let now = Utc::now().naive_utc().format("%Y-%m-%d %H:%M:%S").to_string();

        // 尝试更新现有凭证
        let rows = self.conn.execute(
            "UPDATE credentials SET encrypted_password = ?1, updated_at = ?2
             WHERE account_id = ?3",
            params![encrypted_password, now, account_id],
        )?;

        // 如果不存在，插入新凭证
        if rows == 0 {
            self.conn.execute(
                "INSERT INTO credentials (account_id, encrypted_password, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4)",
                params![account_id, encrypted_password, now, now],
            )?;
        }

        Ok(())
    }

    /// 获取解密后的凭证
    pub fn get_credential(&self, account_id: i64) -> StoreResult<String> {
        // 获取加密凭证
        let encrypted_password: Vec<u8> = self.conn.query_row(
            "SELECT encrypted_password FROM credentials WHERE account_id = ?1",
            params![account_id],
            |row| row.get(0),
        )?;

        // 获取主密钥
        let master_key = keychain::get_or_create_master_key()?;

        // 解密
        let password = crypto::decrypt(&encrypted_password, &master_key)?;

        Ok(password)
    }

    /// 删除凭证
    pub fn delete_credential(&self, account_id: i64) -> StoreResult<()> {
        let rows = self.conn.execute(
            "DELETE FROM credentials WHERE account_id = ?1",
            params![account_id],
        )?;

        if rows == 0 {
            return Err(StoreError::NotFound(format!("Credential for account {}", account_id)));
        }

        Ok(())
    }

    /// 解析账号行
    fn parse_account_row(row: &Row) -> Result<Account, rusqlite::Error> {
        Ok(Account {
            id: row.get(0)?,
            email: row.get(1)?,
            imap_server: row.get(2)?,
            imap_port: row.get(3)?,
            enabled: row.get::<_, i32>(4)? != 0,
            created_at: NaiveDateTime::parse_from_str(&row.get::<_, String>(5)?, "%Y-%m-%d %H:%M:%S")
                .map_err(|e| rusqlite::Error::FromSqlConversionFailure(5, rusqlite::types::Type::Text, Box::new(e)))?,
            updated_at: NaiveDateTime::parse_from_str(&row.get::<_, String>(6)?, "%Y-%m-%d %H:%M:%S")
                .map_err(|e| rusqlite::Error::FromSqlConversionFailure(6, rusqlite::types::Type::Text, Box::new(e)))?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_and_get_account() {
        let db = AccountsDb::new(":memory:").unwrap();

        let id = db.create_account("test@example.com", "imap.example.com", 993).unwrap();
        let account = db.get_account(id).unwrap();

        assert_eq!(account.email, "test@example.com");
        assert_eq!(account.imap_server, "imap.example.com");
        assert_eq!(account.imap_port, 993);
        assert!(account.enabled);
    }

    #[test]
    fn list_accounts() {
        let db = AccountsDb::new(":memory:").unwrap();

        db.create_account("user1@example.com", "imap.example.com", 993).unwrap();
        db.create_account("user2@example.com", "imap.example.com", 993).unwrap();

        let accounts = db.list_accounts().unwrap();
        assert_eq!(accounts.len(), 2);
    }

    #[test]
    fn update_account() {
        let db = AccountsDb::new(":memory:").unwrap();

        let id = db.create_account("test@example.com", "imap.example.com", 993).unwrap();
        let mut account = db.get_account(id).unwrap();

        account.imap_port = 143;
        account.enabled = false;

        db.update_account(&account).unwrap();

        let updated = db.get_account(id).unwrap();
        assert_eq!(updated.imap_port, 143);
        assert!(!updated.enabled);
    }

    #[test]
    fn delete_account() {
        let db = AccountsDb::new(":memory:").unwrap();

        let id = db.create_account("test@example.com", "imap.example.com", 993).unwrap();
        db.delete_account(id).unwrap();

        let result = db.get_account(id);
        assert!(result.is_err());
    }

    #[test]
    fn set_and_get_credential() {
        let db = AccountsDb::new(":memory:").unwrap();

        let id = db.create_account("test@example.com", "imap.example.com", 993).unwrap();
        db.set_credential(id, "my-secret-password").unwrap();

        let password = db.get_credential(id).unwrap();
        assert_eq!(password, "my-secret-password");
    }

    #[test]
    fn credential_cascade_delete() {
        let db = AccountsDb::new(":memory:").unwrap();

        let id = db.create_account("test@example.com", "imap.example.com", 993).unwrap();
        db.set_credential(id, "password").unwrap();

        // 删除账号应该级联删除凭证
        db.delete_account(id).unwrap();

        let result = db.get_credential(id);
        assert!(result.is_err());
    }
}
