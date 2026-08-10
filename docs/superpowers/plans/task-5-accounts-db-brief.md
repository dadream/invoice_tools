# Task 5: accounts.db 管理模块

**Goal:** 实现 accounts.db 的表创建和 CRUD 接口，管理邮箱账号和加密凭证

**Files:**
- Create: `crates/invoice-store/src/accounts_db.rs`
- Modify: `crates/invoice-store/src/lib.rs` (导出 accounts_db 模块)
- Create: `crates/invoice-store/tests/accounts_db_tests.rs`

**Interfaces:**
- `AccountsDb::new(db_path)` - 打开或创建数据库
- `create_account(email, imap_server, imap_port)` - 创建账号
- `get_account(id)` - 获取账号
- `list_accounts()` - 列出所有账号
- `update_account(account)` - 更新账号
- `delete_account(id)` - 删除账号
- `set_credential(account_id, password)` - 设置加密凭证
- `get_credential(account_id)` - 获取解密后的凭证

**Database Schema:**

```sql
-- accounts 表
CREATE TABLE accounts (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    email TEXT NOT NULL UNIQUE,
    imap_server TEXT NOT NULL,
    imap_port INTEGER NOT NULL,
    enabled INTEGER NOT NULL DEFAULT 1,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

-- credentials 表
CREATE TABLE credentials (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    account_id INTEGER NOT NULL UNIQUE,
    encrypted_password BLOB NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    FOREIGN KEY (account_id) REFERENCES accounts(id) ON DELETE CASCADE
);
```

**Implementation:**

## Step 1: 创建 accounts_db.rs 模块

创建 `crates/invoice-store/src/accounts_db.rs`。

由于代码较长，我将分步骤实现：

1. 数据库结构体和初始化
2. 账号 CRUD 操作
3. 凭证加密存储操作
4. 单元测试

## Step 2: 导出模块

编辑 `crates/invoice-store/src/lib.rs`。

## Step 3: 创建集成测试

创建 `crates/invoice-store/tests/accounts_db_tests.rs`。

## Step 4: 验证和提交

```bash
export PATH="$HOME/.cargo/bin:$PATH"
cargo test -p invoice-store
git commit -m "feat(store): implement accounts.db management"
```
