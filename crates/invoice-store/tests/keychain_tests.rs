use invoice_store::keychain::get_or_create_master_key;

#[test]
fn master_key_persists_in_keychain() {
    let key1 = get_or_create_master_key().expect("Failed to get key first time");
    let key2 = get_or_create_master_key().expect("Failed to get key second time");

    assert_eq!(key1.len(), 32, "Key should be 32 bytes");
    assert_eq!(key1, key2, "Key should be stable across calls");
}

#[test]
fn multiple_calls_return_same_key() {
    let keys: Vec<[u8; 32]> = (0..5)
        .map(|_| get_or_create_master_key().expect("Failed to get key"))
        .collect();

    // 所有密钥应该相同
    for key in &keys[1..] {
        assert_eq!(&keys[0], key, "All keys should be identical");
    }
}
