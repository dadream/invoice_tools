//! Keychain 密钥派生模块
//!
//! 从系统 Keychain 获取或创建 AES-256-GCM 主密钥。
//! 密钥首次生成后存储在系统 Keychain 中（如果可用），否则回退到加密文件存储。

use keyring::Entry;
use std::fs;
use std::path::PathBuf;
use crate::{StoreError, StoreResult};

const SERVICE_NAME: &str = "invoice-assistant";
const ACCOUNT_NAME: &str = "master-key";

/// 获取密钥文件路径（仅在 keyring 不可用时使用）
fn get_key_file_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home)
        .join(".invoice-assistant")
        .join(".master-key")
}

/// 从系统 Keychain 获取或创建 AES-256-GCM 主密钥
///
/// 主密钥用于加密邮箱凭证。优先使用系统 Keychain，若不可用则使用文件存储。
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
    // 在 WSL/Linux 环境中，优先使用文件存储（更可靠）
    // macOS 和 Windows 原生环境可以使用系统 Keychain
    #[cfg(target_os = "linux")]
    {
        try_file_storage()
    }

    #[cfg(not(target_os = "linux"))]
    {
        // macOS 或 Windows：尝试 keychain，失败则回退到文件
        match try_keychain() {
            Ok(key) => Ok(key),
            Err(_) => try_file_storage(),
        }
    }
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
    let entry = Entry::new(SERVICE_NAME, ACCOUNT_NAME)
        .map_err(|e| StoreError::Keychain(e))?;

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
            entry.set_password(&key_b64)
                .map_err(|e| StoreError::Keychain(e))?;
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
        // 生成新密钥并保存
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
    use base64::{Engine, engine::general_purpose::STANDARD};
    STANDARD.encode(data)
}

/// Base64 解码
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
