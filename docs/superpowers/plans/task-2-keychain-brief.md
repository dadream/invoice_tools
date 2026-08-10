# Task 2: Keychain 密钥派生模块

**Goal:** 实现从系统 Keychain 派生加密主密钥的功能

**Files:**
- Create: `crates/invoice-store/src/keychain.rs`
- Modify: `crates/invoice-store/src/lib.rs` (导出 keychain 模块)
- Create: `crates/invoice-store/tests/keychain_tests.rs`

**Interfaces:**
- Produces: `get_or_create_master_key() -> StoreResult<[u8; 32]>`
- Service: `invoice-assistant` (Keychain 服务名)
- Account: `master-key` (Keychain 账号名)

**Implementation Steps:**

## Step 1: 创建 keychain.rs 模块

创建 `crates/invoice-store/src/keychain.rs`，实现以下功能：

1. 使用 `keyring::Entry` 访问系统 Keychain
2. 服务名: `"invoice-assistant"`
3. 账号名: `"master-key"`
4. 如果密钥不存在，生成 32 字节随机密钥并存储
5. 如果密钥存在，读取并返回

```rust
use keyring::Entry;
use crate::{StoreError, StoreResult};

const SERVICE_NAME: &str = "invoice-assistant";
const ACCOUNT_NAME: &str = "master-key";

/// 从系统 Keychain 获取或创建 AES-256-GCM 主密钥
///
/// 主密钥用于加密邮箱凭证，存储在系统 Keychain 中，应用不持久化。
/// 首次调用时生成 32 字节随机密钥并存入 Keychain。
pub fn get_or_create_master_key() -> StoreResult<[u8; 32]> {
    let entry = Entry::new(SERVICE_NAME, ACCOUNT_NAME)?;
    
    match entry.get_password() {
        Ok(key_b64) => {
            // 从 base64 解码
            let key_bytes = base64_decode(&key_b64)?;
            if key_bytes.len() != 32 {
                return Err(StoreError::Crypto(format!(
                    "Invalid key length: expected 32, got {}", 
                    key_bytes.len()
                )));
            }
            let mut key = [0u8; 32];
            key.copy_from_slice(&key_bytes);
            Ok(key)
        }
        Err(keyring::Error::NoEntry) => {
            // 生成新密钥
            let key = generate_random_key();
            let key_b64 = base64_encode(&key);
            entry.set_password(&key_b64)?;
            Ok(key)
        }
        Err(e) => Err(e.into()),
    }
}

fn generate_random_key() -> [u8; 32] {
    use rand::RngCore;
    let mut key = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut key);
    key
}

fn base64_encode(data: &[u8]) -> String {
    use base64::{Engine, engine::general_purpose::STANDARD};
    STANDARD.encode(data)
}

fn base64_decode(s: &str) -> StoreResult<Vec<u8>> {
    use base64::{Engine, engine::general_purpose::STANDARD};
    STANDARD.decode(s).map_err(|e| StoreError::Crypto(format!("Base64 decode failed: {}", e)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_is_32_bytes() {
        let key = get_or_create_master_key().unwrap();
        assert_eq!(key.len(), 32);
    }

    #[test]
    fn key_is_stable_across_calls() {
        let key1 = get_or_create_master_key().unwrap();
        let key2 = get_or_create_master_key().unwrap();
        assert_eq!(key1, key2);
    }
}
```

## Step 2: 添加依赖

编辑 `crates/invoice-store/Cargo.toml`，添加：

```toml
rand = "0.8"
base64 = "0.22"
```

## Step 3: 导出模块

编辑 `crates/invoice-store/src/lib.rs`，在错误类型定义后添加：

```rust
pub mod keychain;
```

## Step 4: 创建集成测试

创建 `crates/invoice-store/tests/keychain_tests.rs`:

```rust
use invoice_store::keychain::get_or_create_master_key;

#[test]
fn master_key_persists_in_keychain() {
    let key1 = get_or_create_master_key().expect("Failed to get key first time");
    let key2 = get_or_create_master_key().expect("Failed to get key second time");
    
    assert_eq!(key1.len(), 32, "Key should be 32 bytes");
    assert_eq!(key1, key2, "Key should be stable across calls");
}
```

## Step 5: 验证

```bash
export PATH="$HOME/.cargo/bin:$PATH"
cargo test -p invoice-store
```

预期：所有测试通过（包括新增的 keychain 测试）

## Step 6: 提交

```bash
git add crates/invoice-store/src/keychain.rs \
        crates/invoice-store/src/lib.rs \
        crates/invoice-store/Cargo.toml \
        crates/invoice-store/tests/keychain_tests.rs
git commit -m "feat(store): implement keychain master key derivation"
```

## Success Criteria

- ✅ `get_or_create_master_key()` 返回 32 字节密钥
- ✅ 密钥在多次调用间保持稳定
- ✅ 密钥存储在系统 Keychain 中（不在应用文件）
- ✅ 所有测试通过
- ✅ 代码已提交
