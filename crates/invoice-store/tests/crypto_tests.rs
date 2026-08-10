use invoice_store::crypto::{encrypt, decrypt};
use invoice_store::keychain::get_or_create_master_key;

#[test]
fn encrypt_decrypt_with_master_key() {
    let master_key = get_or_create_master_key().expect("Failed to get master key");

    let plaintext = "user@example.com:SecurePassword123!";
    let ciphertext = encrypt(plaintext, &master_key).expect("Encryption failed");
    let decrypted = decrypt(&ciphertext, &master_key).expect("Decryption failed");

    assert_eq!(plaintext, decrypted);
}

#[test]
fn encrypt_produces_different_output_each_time() {
    let master_key = get_or_create_master_key().expect("Failed to get master key");

    let plaintext = "constant-password";
    let ct1 = encrypt(plaintext, &master_key).expect("Encryption 1 failed");
    let ct2 = encrypt(plaintext, &master_key).expect("Encryption 2 failed");

    // 密文应该不同（随机 nonce）
    assert_ne!(ct1, ct2);

    // 但都能正确解密
    assert_eq!(decrypt(&ct1, &master_key).unwrap(), plaintext);
    assert_eq!(decrypt(&ct2, &master_key).unwrap(), plaintext);
}

#[test]
fn long_plaintext_encryption() {
    let master_key = get_or_create_master_key().expect("Failed to get master key");

    let long_plaintext = "a".repeat(1000);
    let ciphertext = encrypt(&long_plaintext, &master_key).expect("Encryption failed");
    let decrypted = decrypt(&ciphertext, &master_key).expect("Decryption failed");

    assert_eq!(long_plaintext, decrypted);
}
