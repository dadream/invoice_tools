# S0.4 加密存储模块实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 创建 `invoice-store` crate，提供 SQLite 双库存储、邮箱凭证 AES-256-GCM 加密、系统 Keychain 密钥派生，以及账号/发票/批次/台账的 CRUD 接口。

**Architecture:** 新建 `crates/invoice-store` crate，使用 `rusqlite` 管理 `accounts.db` 和 `ledger.db` 两个数据库；通过 `keyring` crate 从系统 Keychain 派生主密钥；使用 `aes-gcm` 加密邮箱凭证；提供类型安全的 CRUD API，所有金额字段使用 `Decimal`。

**Tech Stack:** Rust + rusqlite + aes-gcm + keyring + serde + rust_decimal

## Global Constraints

- 所有金额字段必须使用 `rust_decimal::Decimal`，禁止使用 `f64`（避免求和对账时的分位误差）
- 加密密钥从系统 Keychain 派生，不存储在应用内或配置文件中
- 数据库文件路径：`~/.invoice-assistant/accounts.db` 和 `~/.invoice-assistant/ledger.db`
- 邮箱凭证使用 AES-256-GCM 加密，nonce 和 tag 与密文一起存储
- 核心数据模型不含任何 Concur 或其他报销系统的概念
- 所有 CRUD 函数返回 `Result<T, StoreError>`，错误类型使用 `thiserror`
- 表结构支持未来迁移（每个表包含 `created_at` 和 `updated_at` 时间戳）

---

## File Structure

```
crates/invoice-store/
├── Cargo.toml
├── src/
│   ├── lib.rs              # 公开 API + 错误类型
│   ├── keychain.rs         # Keychain 密钥派生
│   ├── crypto.rs           # AES-256-GCM 加密/解密
│   ├── accounts_db.rs      # accounts.db 管理（账号、凭证、设置）
│   ├── ledger_db.rs        # ledger.db 管理（发票、批次、台账）
│   └── models.rs           # 数据库模型（Account, Credential, Batch, ReportedInvoice）
└── tests/
    ├── keychain_tests.rs   # Keychain 集成测试
    ├── crypto_tests.rs     # 加密往返测试
    ├── accounts_db_tests.rs
    └── ledger_db_tests.rs
```

---

### Task 1: Crate 骨架与错误类型

**Files:**
- Create: `crates/invoice-store/Cargo.toml`
- Create: `crates/invoice-store/src/lib.rs`
- Modify: `Cargo.toml:2` (添加 invoice-store 到 workspace members)

**Interfaces:**
- Produces: `StoreError` 错误类型，`StoreResult<T>` 类型别名

- [ ] **Step 1: 创建 Cargo.toml