//! AES-256-GCM 加密/解密模块
//!
//! 用于加密邮箱凭证，采用 AES-256-GCM 认证加密。
//! 密文格式: [12-byte nonce][ciphertext][16-byte tag]

use aes_gcm::{
    aead::{Aead, AeadCore, KeyInit, OsRng},
    Aes256Gcm, Nonce,
};
use crate::{StoreError, StoreResult};

const NONCE_SIZE: usize = 12; // GCM 标准 nonce 大小
const TAG_SIZE: usize = 16;   // GCM 标准 tag 大小

/// 加密明文字符串
///
/// # Arguments
///
/// * `plaintext` - 要加密的明文（通常是邮箱密码）
/// * `key` - 32 字节 AES-256 密钥
///
/// # Returns
///
/// 加密后的数据，格式为: [nonce(12) || ciphertext || tag(16)]
///
/// # Errors
///
/// - `StoreError::Crypto`: 加密失败
pub fn encrypt(plaintext: &str, key: &[u8; 32]) -> StoreResult<Vec<u8>> {
    let cipher = Aes256Gcm::new(key.into());

    // 生成随机 nonce
    let nonce = Aes256Gcm::generate_nonce(&mut OsRng);

    // 加密
    let ciphertext = cipher
        .encrypt(&nonce, plaintext.as_bytes())
        .map_err(|e| StoreError::Crypto(format!("Encryption failed: {}", e)))?;

    // 组装: nonce + ciphertext (already includes tag)
    let mut result = Vec::with_capacity(NONCE_SIZE + ciphertext.len());
    result.extend_from_slice(&nonce);
    result.extend_from_slice(&ciphertext);

    Ok(result)
}

/// 解密密文
///
/// # Arguments
///
/// * `ciphertext` - 加密数据，格式为: [nonce(12) || ciphertext || tag(16)]
/// * `key` - 32 字节 AES-256 密钥
///
/// # Returns
///
/// 解密后的明文字符串
///
/// # Errors
///
/// - `StoreError::Crypto`: 解密失败或数据格式错误
pub fn decrypt(ciphertext: &[u8], key: &[u8; 32]) -> StoreResult<String> {
    if ciphertext.len() < NONCE_SIZE + TAG_SIZE {
        return Err(StoreError::Crypto(format!(
            "Invalid ciphertext length: expected at least {}, got {}",
            NONCE_SIZE + TAG_SIZE,
            ciphertext.len()
        )));
    }

    let cipher = Aes256Gcm::new(key.into());

    // 提取 nonce 和密文
    let nonce = Nonce::from_slice(&ciphertext[..NONCE_SIZE]);
    let encrypted_data = &ciphertext[NONCE_SIZE..];

    // 解密
    let plaintext_bytes = cipher
        .decrypt(nonce, encrypted_data)
        .map_err(|e| StoreError::Crypto(format!("Decryption failed: {}", e)))?;

    // 转换为 UTF-8 字符串
    String::from_utf8(plaintext_bytes)
        .map_err(|e| StoreError::Crypto(format!("Invalid UTF-8 in decrypted data: {}", e)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encrypt_decrypt_roundtrip() {
        let key = [42u8; 32];
        let plaintext = "my-secret-password";

        let ciphertext = encrypt(plaintext, &key).unwrap();
        let decrypted = decrypt(&ciphertext, &key).unwrap();

        assert_eq!(plaintext, decrypted);
    }

    #[test]
    fn different_keys_fail_decryption() {
        let key1 = [1u8; 32];
        let key2 = [2u8; 32];
        let plaintext = "secret";

        let ciphertext = encrypt(plaintext, &key1).unwrap();
        let result = decrypt(&ciphertext, &key2);

        assert!(result.is_err());
    }

    #[test]
    fn ciphertext_includes_nonce_and_tag() {
        let key = [99u8; 32];
        let plaintext = "test";

        let ciphertext = encrypt(plaintext, &key).unwrap();

        // 密文长度 = nonce(12) + plaintext.len() + tag(16)
        assert!(ciphertext.len() >= NONCE_SIZE + TAG_SIZE + plaintext.len());
    }

    #[test]
    fn encrypting_same_plaintext_produces_different_ciphertext() {
        let key = [7u8; 32];
        let plaintext = "same-text";

        let ciphertext1 = encrypt(plaintext, &key).unwrap();
        let ciphertext2 = encrypt(plaintext, &key).unwrap();

        // 由于 nonce 随机，密文应该不同
        assert_ne!(ciphertext1, ciphertext2);

        // 但解密后都是相同的明文
        assert_eq!(decrypt(&ciphertext1, &key).unwrap(), plaintext);
        assert_eq!(decrypt(&ciphertext2, &key).unwrap(), plaintext);
    }

    #[test]
    fn invalid_ciphertext_fails() {
        let key = [11u8; 32];
        let invalid = vec![0u8; 10]; // 太短

        let result = decrypt(&invalid, &key);
        assert!(result.is_err());
    }
}
