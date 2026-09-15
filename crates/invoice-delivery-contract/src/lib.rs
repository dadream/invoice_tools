//! Target-neutral, immutable delivery input. No store or Concur dependency.
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Snapshot {
    pub batch_id: i64,
    pub snapshot_id: i64,
    pub fingerprint: String,
    pub name: String,
    pub expenses: Vec<Expense>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Expense {
    pub id: i64,
    pub group_id: i64,
    pub group_title: String,
    pub group_kind: String,
    /// Stable local fields only; amounts and ISO dates are lossless strings.
    pub fields: BTreeMap<String, String>,
    pub documents: Vec<Document>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Document {
    pub id: i64,
    pub role: String,
    pub path: String,
    pub name: String,
    pub sha256: String,
}
