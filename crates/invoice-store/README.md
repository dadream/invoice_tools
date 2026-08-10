# invoice-store

本地数据存储层，管理账户凭证和报销批次数据。

## 概述

`invoice-store` 提供双数据库架构：
- **accounts.db**: 邮箱账户和加密凭证
- **ledger.db**: 报销批次和已报销发票记录

## 核心功能

### 1. 账户管理 (accounts_db)

```rust
use invoice_store::{AccountsDb, Account};

let db = AccountsDb::new("accounts.db")?;

// 创建账户
let account = Account {
    id: 0,
    email: "user@example.com".to_string(),
    display_name: Some("张三".to_string()),
    imap_server: "imap.example.com".to_string(),
    imap_port: 993,
    created_at: Utc::now().naive_utc(),
    updated_at: Utc::now().naive_utc(),
};
let account_id = db.create_account(&account)?;

// 存储加密凭证
db.set_credential(account_id, "password123")?;
```

### 2. 批次状态机 (ledger_db)

报销批次遵循严格的状态转换规则：

```
Draft → Submitted → Approved → Completed (正常流程)
  ↓         ↓          ↓
Rejected  Rejected  Rejected (驳回流程)
```

**状态说明：**
- **Draft**: 草稿，正在编辑
- **Submitted**: 已提交，等待审核
- **Approved**: 已批准，等待打款
- **Completed**: 已完成（已打款）
- **Rejected**: 已驳回

**使用示例：**

```rust
use invoice_store::{LedgerDb, BatchStatus};

let db = LedgerDb::new("ledger.db")?;

// 创建批次（自动处于 Draft 状态）
let batch_id = db.create_batch("2026年7月出差", "2026-07")?;

// 提交审核
db.transition_batch_status(batch_id, BatchStatus::Submitted)?;

// 批准
db.transition_batch_status(batch_id, BatchStatus::Approved)?;

// 完成（打款）
db.transition_batch_status(batch_id, BatchStatus::Completed)?;
```

**非法转换示例：**

```rust
// ❌ 不能从 Draft 直接跳到 Completed
let result = db.transition_batch_status(batch_id, BatchStatus::Completed);
assert!(matches!(result, Err(StoreError::InvalidStateTransition { .. })));

// ❌ 终态不能转换
db.transition_batch_status(batch_id, BatchStatus::Completed)?;
let result = db.transition_batch_status(batch_id, BatchStatus::Draft);
assert!(result.is_err());
```

**时间戳管理：**

系统自动管理 4 个时间戳字段：
- `submitted_at`: 提交时间
- `approved_at`: 批准时间
- `completed_at`: 完成时间
- `rejected_at`: 驳回时间

```rust
let batch = db.get_batch(batch_id)?;
println!("提交时间: {:?}", batch.submitted_at);
println!("批准时间: {:?}", batch.approved_at);
println!("完成时间: {:?}", batch.completed_at);
```

### 3. 发票管理

```rust
use invoice_store::{ReportedInvoice, TicketType};
use rust_decimal::Decimal;

// 添加发票到批次
let invoice = ReportedInvoice {
    id: 0,
    batch_id,
    invoice_number: "12345678901234567890".to_string(),
    issue_date: NaiveDate::from_ymd_opt(2026, 7, 15).unwrap(),
    amount: Decimal::from_str("350.00").unwrap(),
    tax_amount: Some(Decimal::from_str("31.50").unwrap()),
    buyer_name: Some("某某公司".to_string()),
    seller_name: Some("铁路客票".to_string()),
    ticket_type: TicketType::Rail,
    city: Some("北京".to_string()),
    departure_time: Some(NaiveDateTime::from_ymd_opt(2026, 7, 15).unwrap()
        .and_hms_opt(14, 30, 0).unwrap()),
    checkin_date: None,
    file_path: "/path/to/invoice.xml".to_string(),
    created_at: Utc::now().naive_utc(),
    updated_at: Utc::now().naive_utc(),
};

db.add_invoice(&invoice)?;

// 批次统计自动更新
let batch = db.get_batch(batch_id)?;
println!("发票数量: {}", batch.invoice_count);
println!("总金额: {}", batch.total_amount);
```

## 数据加密

凭证使用 AES-256-GCM 加密，密钥存储在系统 keychain 中：

```rust
use invoice_store::crypto::{encrypt, decrypt, get_master_key};

let key = get_master_key()?; // 从 keychain 获取或生成
let ciphertext = encrypt(b"sensitive-data", &key)?;
let plaintext = decrypt(&ciphertext, &key)?;
```

**安全特性：**
- 256 位主密钥
- 每次加密使用随机 nonce
- AEAD 认证加密
- 密钥持久化在 OS keychain（Linux: Secret Service, macOS: Keychain, Windows: Credential Manager）

## 错误处理

```rust
use invoice_store::{StoreError, StoreResult};

match db.transition_batch_status(batch_id, new_status) {
    Ok(()) => println!("状态转换成功"),
    Err(StoreError::InvalidStateTransition { from, to }) => {
        eprintln!("非法转换: {} -> {}", from, to);
    }
    Err(StoreError::Database(e)) => {
        eprintln!("数据库错误: {}", e);
    }
    Err(e) => {
        eprintln!("其他错误: {}", e);
    }
}
```

**错误类型：**
- `InvalidStateTransition`: 非法状态转换
- `Database`: SQLite 数据库错误
- `Crypto`: 加密/解密失败
- `Keychain`: Keychain 访问失败
- `Validation`: 数据验证失败
- `NotFound`: 资源不存在
- `Io`: I/O 操作失败

## 测试

```bash
# 运行所有测试
cargo test -p invoice-store

# 运行单个测试
cargo test -p invoice-store test_full_happy_path_lifecycle
```

**测试覆盖：**
- 38 个单元测试
- 20 个状态转换测试（覆盖所有合法和非法转换）
- 完整生命周期集成测试
- 时间戳保持测试
- 加密往返测试

## 架构决策

### 为什么使用双数据库？

1. **安全隔离**: 账户凭证与业务数据分离
2. **访问控制**: 不同的备份和权限策略
3. **清晰边界**: accounts.db 只读频繁，ledger.db 读写频繁

### 为什么使用状态机验证？

1. **数据完整性**: 防止无效的状态跳转（如 Draft → Completed）
2. **审计追踪**: 每个状态转换都有时间戳
3. **业务规则**: 强制执行报销审批流程

### 为什么时间戳字段分离？

1. **审计需求**: 独立记录每个状态变更时刻
2. **查询效率**: 可按提交时间、完成时间等独立查询
3. **状态恢复**: 可追溯批次在各状态的停留时长

## 依赖项

- `rusqlite`: SQLite 数据库
- `aes-gcm`: AES-256-GCM 加密
- `keyring`: 跨平台 keychain 访问
- `chrono`: 日期时间处理
- `rust_decimal`: 高精度十进制（避免浮点误差）
- `serde`: 序列化支持

## 许可证

同项目根目录
