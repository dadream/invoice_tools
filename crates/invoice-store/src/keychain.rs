//! Keychain 密钥派生模块
//!
//! 旧版凭据兼容模块使用本地稳定密钥文件；仅在密钥文件首次创建时尝试
//! 从系统 Keychain 迁移已有主密钥。MVP 运行路径不持久化邮箱授权码。

use crate::{StoreError, StoreResult};
use keyring::Entry;
use std::fs;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

const SERVICE_NAME: &str = "invoice-assistant";
const ACCOUNT_NAME: &str = "master-key";

/// 获取密钥文件路径（仅在 keyring 不可用时使用）
fn get_key_file_path() -> PathBuf {
    if let Some(root) = std::env::var_os("INVOICE_ASSISTANT_HOME") {
        return PathBuf::from(root).join("legacy-credential-master-key");
    }
    dirs::data_local_dir()
        .or_else(dirs::home_dir)
        .unwrap_or_else(|| PathBuf::from("."))
        .join("InvoiceAssistant")
        .join("Data")
        .join("legacy-credential-master-key")
}

/// 从系统 Keychain 获取或创建 AES-256-GCM 主密钥
///
/// 主密钥只用于兼容旧版加密凭据，并始终以本地密钥文件作为稳定来源。
/// 首次调用时生成 32 字节随机密钥。
///
/// # Returns
///
/// 32 字节密钥数组，用于 AES-256-GCM 加密
///
/// # Errors
///
/// - `StoreError::Keychain`: Keychain 访问失败
/// - `StoreError::Crypto`: 密钥格式无效
/// - `StoreError::Io`: 文件操作失败
pub fn get_or_create_master_key() -> StoreResult<[u8; 32]> {
    static KEY_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    let _guard = KEY_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|e| e.into_inner());

    try_file_storage()
}

/// 检测 keychain 是否可用且持久化（保留供未来使用）
#[allow(dead_code)]
fn is_keychain_available() -> bool {
    let test_service = "invoice-assistant-test";
    let test_account = "availability-check";

    let Ok(entry) = Entry::new(test_service, test_account) else {
        return false;
    };

    // 尝试写入测试值
    let test_value = "test";
    if entry.set_password(test_value).is_err() {
        return false;
    }

    // 验证能否读回
    let can_read = matches!(entry.get_password(), Ok(v) if v == test_value);

    // 清理测试数据
    let _ = entry.delete_credential();

    can_read
}

/// 尝试从系统 Keychain 获取或创建密钥
fn try_keychain() -> StoreResult<[u8; 32]> {
    let entry = Entry::new(SERVICE_NAME, ACCOUNT_NAME).map_err(StoreError::Keychain)?;

    match entry.get_password() {
        Ok(key_b64) => {
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
            let key = generate_random_key();
            let key_b64 = base64_encode(&key);
            entry.set_password(&key_b64).map_err(StoreError::Keychain)?;

            // 某些企业 Windows 环境会报告写入成功但无法立即读回。
            // 只有通过读回验证才接受 keychain，否则让调用方使用文件后备。
            let stored = entry.get_password().map_err(StoreError::Keychain)?;
            if stored != key_b64 {
                return Err(StoreError::Crypto(
                    "系统凭据存储未能稳定保存主密钥".to_string(),
                ));
            }
            Ok(key)
        }
        Err(e) => Err(StoreError::Keychain(e)),
    }
}

/// 从文件存储获取或创建密钥（Keychain 不可用时的后备方案）
fn try_file_storage() -> StoreResult<[u8; 32]> {
    let key_file = get_key_file_path();

    if key_file.exists() {
        // 读取现有密钥
        let key_b64 = fs::read_to_string(&key_file)?;
        let key_bytes = base64_decode(&key_b64)?;
        if key_bytes.len() != 32 {
            return Err(StoreError::Crypto(format!(
                "Invalid key length in file: expected 32, got {}",
                key_bytes.len()
            )));
        }
        let mut key = [0u8; 32];
        key.copy_from_slice(&key_bytes);
        Ok(key)
    } else {
        // 首次建立稳定文件时，非 Linux 平台尝试迁移旧 Keychain 密钥。
        // 无旧密钥或系统凭据服务不可靠时生成新密钥。
        #[cfg(not(target_os = "linux"))]
        let key = try_keychain().unwrap_or_else(|_| generate_random_key());

        #[cfg(target_os = "linux")]
        let key = generate_random_key();

        let key_b64 = base64_encode(&key);

        // 创建目录
        if let Some(parent) = key_file.parent() {
            fs::create_dir_all(parent)?;
        }

        // 写入文件（设置 0600 权限）
        fs::write(&key_file, &key_b64)?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = fs::metadata(&key_file)?.permissions();
            perms.set_mode(0o600);
            fs::set_permissions(&key_file, perms)?;
        }

        Ok(key)
    }
}

/// 生成 32 字节随机密钥
fn generate_random_key() -> [u8; 32] {
    use rand::RngCore;
    let mut key = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut key);
    key
}

/// Base64 编码
fn base64_encode(data: &[u8]) -> String {
    use base64::{engine::general_purpose::STANDARD, Engine};
    STANDARD.encode(data)
}

/// Base64 解码
fn base64_decode(s: &str) -> StoreResult<Vec<u8>> {
    use base64::{engine::general_purpose::STANDARD, Engine};
    STANDARD
        .decode(s)
        .map_err(|e| StoreError::Crypto(format!("Base64 decode failed: {}", e)))
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
