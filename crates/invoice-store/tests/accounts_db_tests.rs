use invoice_store::accounts_db::AccountsDb;
use tempfile::TempDir;

#[test]
fn full_account_lifecycle() {
    let temp_dir = TempDir::new().unwrap();
    let db_path = temp_dir.path().join("accounts.db");
    let db = AccountsDb::new(&db_path).unwrap();

    // 创建账号
    let id = db
        .create_account("user@example.com", "imap.gmail.com", 993)
        .unwrap();
    assert!(id > 0);

    // 获取账号
    let account = db.get_account(id).unwrap();
    assert_eq!(account.email, "user@example.com");
    assert_eq!(account.imap_server, "imap.gmail.com");

    // 设置凭证
    db.set_credential(id, "SuperSecretPassword123!").unwrap();

    // 获取凭证
    let password = db.get_credential(id).unwrap();
    assert_eq!(password, "SuperSecretPassword123!");

    // 删除账号
    db.delete_account(id).unwrap();
    assert!(db.get_account(id).is_err());
}

#[test]
fn multiple_accounts_with_credentials() {
    let temp_dir = TempDir::new().unwrap();
    let db_path = temp_dir.path().join("accounts.db");
    let db = AccountsDb::new(&db_path).unwrap();

    // 创建多个账号
    let id1 = db
        .create_account("user1@example.com", "imap.example.com", 993)
        .unwrap();
    let id2 = db
        .create_account("user2@example.com", "imap.example.com", 993)
        .unwrap();

    // 设置不同的凭证
    db.set_credential(id1, "password1").unwrap();
    db.set_credential(id2, "password2").unwrap();

    // 验证凭证互不干扰
    assert_eq!(db.get_credential(id1).unwrap(), "password1");
    assert_eq!(db.get_credential(id2).unwrap(), "password2");

    // 列出所有账号
    let accounts = db.list_accounts().unwrap();
    assert_eq!(accounts.len(), 2);
}

#[test]
fn database_persistence() {
    let temp_dir = TempDir::new().unwrap();
    let db_path = temp_dir.path().join("accounts.db");

    let id = {
        let db = AccountsDb::new(&db_path).unwrap();
        let id = db
            .create_account("persistent@example.com", "imap.example.com", 993)
            .unwrap();
        db.set_credential(id, "persistent-password").unwrap();
        id
    };

    // 重新打开数据库
    let db = AccountsDb::new(&db_path).unwrap();
    let account = db.get_account(id).unwrap();
    assert_eq!(account.email, "persistent@example.com");

    let password = db.get_credential(id).unwrap();
    assert_eq!(password, "persistent-password");
}
