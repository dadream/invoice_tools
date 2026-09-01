use rusqlite::{Connection, OpenFlags};
use std::env;
use std::error::Error;
use std::fs;
use std::path::Path;

fn main() -> Result<(), Box<dyn Error>> {
    let mut args = env::args().skip(1);
    let command = args
        .next()
        .ok_or("usage: migration_probe <create-v5|inspect> <ledger.db>")?;
    let path = args.next().ok_or("missing ledger.db path")?;
    if args.next().is_some() {
        return Err("unexpected extra arguments".into());
    }
    match command.as_str() {
        "create-v5" => create_v5(Path::new(&path)),
        "inspect" => inspect(Path::new(&path)),
        _ => Err(format!("unknown command: {command}").into()),
    }
}

fn create_v5(path: &Path) -> Result<(), Box<dyn Error>> {
    if path.exists() {
        return Err(format!(
            "refusing to overwrite existing database: {}",
            path.display()
        )
        .into());
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let connection = Connection::open(path)?;
    connection.execute_batch(
        r#"
        PRAGMA foreign_keys = ON;
        CREATE TABLE batches (
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
        CREATE TABLE settings (
            key TEXT PRIMARY KEY NOT NULL,
            value TEXT NOT NULL,
            updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
        );
        INSERT INTO batches (
            id, name, month, status, total_amount, invoice_count, created_at, updated_at
        ) VALUES (1, 'packaged-migration-sentinel', '2026-06', 0, '88.00', 1, 'before', 'before');
        INSERT INTO reported_invoices (
            id, batch_id, invoice_number, issue_date, amount, ticket_type,
            file_path, created_at, updated_at
        ) VALUES (
            1, 1, '12345678901234567890', '2026-06-01', '88.00', 'rail',
            'C:/synthetic/migration-sentinel.pdf', 'before', 'before'
        );
        INSERT INTO settings (key, value, updated_at)
        VALUES ('migration_probe', 'preserve', 'before');
        PRAGMA user_version = 5;
        "#,
    )?;
    connection.close().map_err(|(_, error)| error)?;
    inspect(path)
}

fn inspect(path: &Path) -> Result<(), Box<dyn Error>> {
    let connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let version: i32 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    let integrity: String = connection.query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
    let batch_name: String =
        connection.query_row("SELECT name FROM batches WHERE id = 1", [], |row| {
            row.get(0)
        })?;
    let invoice_number: String = connection.query_row(
        "SELECT invoice_number FROM reported_invoices WHERE id = 1",
        [],
        |row| row.get(0),
    )?;
    let setting: String = connection.query_row(
        "SELECT value FROM settings WHERE key = 'migration_probe'",
        [],
        |row| row.get(0),
    )?;
    let concur_tables: i64 = connection.query_row(
        "SELECT COUNT(*) FROM sqlite_master
         WHERE type = 'table' AND name IN ('concur_send_sessions', 'concur_send_items')",
        [],
        |row| row.get(0),
    )?;
    println!("version={version}");
    println!("integrity={integrity}");
    println!("batch_name={batch_name}");
    println!("invoice_number={invoice_number}");
    println!("setting={setting}");
    println!("concur_tables={concur_tables}");
    Ok(())
}
