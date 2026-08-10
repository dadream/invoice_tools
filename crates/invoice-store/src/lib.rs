//! Invoice storage module with encrypted credential management
//!
//! This crate provides SQLite-based storage for invoice data and encrypted email credentials.
//! It manages two databases:
//! - `accounts.db`: Email accounts, encrypted credentials, and settings
//! - `ledger.db`: Invoices, batches, and expense reports
//!
//! # Security
//!
//! Email credentials are encrypted using AES-256-GCM with keys derived from the system keychain.
//! Keys are never stored in application files or configuration.

use thiserror::Error;

pub mod keychain;
pub mod crypto;

/// Result type alias for store operations
pub type StoreResult<T> = Result<T, StoreError>;

/// Errors that can occur during store operations
#[derive(Error, Debug)]
pub enum StoreError {
    /// Database operation failed
    #[error("Database error: {0}")]
    Database(#[from] rusqlite::Error),

    /// Encryption or decryption failed
    #[error("Cryptographic operation failed: {0}")]
    Crypto(String),

    /// Keychain access failed
    #[error("Keychain error: {0}")]
    Keychain(#[from] keyring::Error),

    /// Data validation failed
    #[error("Validation error: {0}")]
    Validation(String),

    /// Resource not found
    #[error("Not found: {0}")]
    NotFound(String),

    /// I/O operation failed
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// Other errors
    #[error("Internal error: {0}")]
    Internal(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_types_can_be_created() {
        let _db_err = StoreError::Database(rusqlite::Error::InvalidQuery);
        let _crypto_err = StoreError::Crypto("encryption failed".to_string());
        let _validation_err = StoreError::Validation("invalid input".to_string());
        let _not_found_err = StoreError::NotFound("account".to_string());
        let _internal_err = StoreError::Internal("unexpected".to_string());
    }

    #[test]
    fn store_result_can_be_used() {
        let success: StoreResult<i32> = Ok(42);
        assert_eq!(success.unwrap(), 42);

        let failure: StoreResult<i32> = Err(StoreError::Internal("test".to_string()));
        assert!(failure.is_err());
    }
}
