use std::collections::HashSet;

use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension, Row};

use crate::ledger_db::LedgerDb;
use crate::models::{
    BatchStatus, ConcurReserveOutcome, ConcurSendItem, ConcurSendSession, NewConcurSendItem,
};
use crate::{StoreError, StoreResult};

impl LedgerDb {
    pub(crate) fn recover_interrupted_concur_sends(&self) -> StoreResult<()> {
        let now = now_text();
        self.conn.execute(
            "UPDATE concur_send_items
             SET status = 'unknown',
                 last_error = '上次发送被中断，结果未知；核对 Concur 后再处理',
                 updated_at = ?1
             WHERE status = 'sending'",
            params![now],
        )?;
        self.conn.execute(
            "UPDATE concur_send_sessions
             SET trial_status = 'unknown', updated_at = ?1
             WHERE trial_status = 'sending'",
            params![now],
        )?;
        Ok(())
    }

    /// 固定批次、发件地址、Concur 收件地址、试发票据和附件哈希。
    /// 已存在计划只有完全一致时才视为幂等，防止试发后静默更换收件人或附件。
    pub fn initialize_concur_send_session(
        &self,
        batch_id: i64,
        sender_email: &str,
        recipient_email: &str,
        trial_invoice_id: i64,
        items: &[NewConcurSendItem],
    ) -> StoreResult<()> {
        validate_email_shape(sender_email, "sender")?;
        validate_email_shape(recipient_email, "recipient")?;
        if items.is_empty() {
            return Err(StoreError::Validation(
                "Concur send plan must contain at least one item".to_string(),
            ));
        }
        if !items.iter().any(|item| item.invoice_id == trial_invoice_id) {
            return Err(StoreError::Validation(
                "trial invoice must be present in the send plan".to_string(),
            ));
        }
        let mut invoice_ids = HashSet::new();
        let mut idempotency_keys = HashSet::new();
        for item in items {
            validate_concur_send_item(item)?;
            if !invoice_ids.insert(item.invoice_id)
                || !idempotency_keys.insert(item.idempotency_key.to_ascii_lowercase())
            {
                return Err(StoreError::Validation(
                    "Concur send plan contains duplicate invoice or idempotency key".to_string(),
                ));
            }
        }

        let transaction = self.conn.unchecked_transaction()?;
        let batch_status: Option<i32> = transaction
            .query_row(
                "SELECT status FROM batches WHERE id = ?1",
                params![batch_id],
                |row| row.get(0),
            )
            .optional()?;
        if !matches!(
            batch_status.and_then(BatchStatus::from_i32),
            Some(BatchStatus::Approved | BatchStatus::Completed)
        ) {
            return Err(StoreError::Validation(
                "only approved or completed batches can be sent to Concur".to_string(),
            ));
        }

        for item in items {
            let eligible: bool = transaction.query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM reported_invoices invoice
                    WHERE invoice.id = ?1 AND invoice.batch_id = ?2
                      AND NOT EXISTS (
                          SELECT 1 FROM excluded_invoices excluded
                          WHERE excluded.invoice_id = invoice.id
                      )
                 )",
                params![item.invoice_id, batch_id],
                |row| row.get(0),
            )?;
            if !eligible {
                return Err(StoreError::Validation(format!(
                    "invoice {} is not an eligible member of the batch",
                    item.invoice_id
                )));
            }
        }

        let existing = transaction
            .query_row(
                "SELECT batch_id, sender_email, recipient_email, trial_invoice_id,
                        trial_status, confirmed_behavior, confirmed_at, created_at, updated_at
                 FROM concur_send_sessions WHERE batch_id = ?1",
                params![batch_id],
                map_concur_send_session,
            )
            .optional()?;
        let now = now_text();
        if let Some(existing) = existing {
            if existing.sender_email != sender_email
                || existing.recipient_email != recipient_email
                || existing.trial_invoice_id != trial_invoice_id
            {
                return Err(StoreError::Validation(
                    "existing Concur trial configuration cannot be changed".to_string(),
                ));
            }
            let persisted_item_count: usize = transaction.query_row(
                "SELECT COUNT(*) FROM concur_send_items WHERE batch_id = ?1",
                params![batch_id],
                |row| row.get(0),
            )?;
            if persisted_item_count != items.len() {
                return Err(StoreError::Validation(
                    "existing Concur attachment set cannot be changed".to_string(),
                ));
            }
        } else {
            transaction.execute(
                "INSERT INTO concur_send_sessions (
                    batch_id, sender_email, recipient_email, trial_invoice_id,
                    trial_status, created_at, updated_at
                 ) VALUES (?1, ?2, ?3, ?4, 'not_started', ?5, ?5)",
                params![
                    batch_id,
                    sender_email,
                    recipient_email,
                    trial_invoice_id,
                    now
                ],
            )?;
        }

        for item in items {
            let existing = transaction
                .query_row(
                    "SELECT batch_id, invoice_id, idempotency_key, attachment_name,
                            attachment_sha256, status, attempt_count, last_error,
                            message_id, sent_at, updated_at
                     FROM concur_send_items WHERE batch_id = ?1 AND invoice_id = ?2",
                    params![batch_id, item.invoice_id],
                    map_concur_send_item,
                )
                .optional()?;
            let normalized_key = item.idempotency_key.to_ascii_lowercase();
            let normalized_hash = item.attachment_sha256.to_ascii_uppercase();
            if let Some(existing) = existing {
                if existing.idempotency_key != normalized_key
                    || existing.attachment_name != item.attachment_name
                    || existing.attachment_sha256 != normalized_hash
                {
                    return Err(StoreError::Validation(
                        "existing Concur send item does not match the reviewed attachment"
                            .to_string(),
                    ));
                }
            } else {
                transaction.execute(
                    "INSERT INTO concur_send_items (
                        batch_id, invoice_id, idempotency_key, attachment_name,
                        attachment_sha256, status, updated_at
                     ) VALUES (?1, ?2, ?3, ?4, ?5, 'pending', ?6)",
                    params![
                        batch_id,
                        item.invoice_id,
                        normalized_key,
                        item.attachment_name,
                        normalized_hash,
                        now
                    ],
                )?;
            }
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn get_concur_send_session(&self, batch_id: i64) -> StoreResult<Option<ConcurSendSession>> {
        self.conn
            .query_row(
                "SELECT batch_id, sender_email, recipient_email, trial_invoice_id,
                        trial_status, confirmed_behavior, confirmed_at, created_at, updated_at
                 FROM concur_send_sessions WHERE batch_id = ?1",
                params![batch_id],
                map_concur_send_session,
            )
            .optional()
            .map_err(StoreError::from)
    }

    pub fn list_concur_send_items(&self, batch_id: i64) -> StoreResult<Vec<ConcurSendItem>> {
        let mut statement = self.conn.prepare(
            "SELECT batch_id, invoice_id, idempotency_key, attachment_name,
                    attachment_sha256, status, attempt_count, last_error,
                    message_id, sent_at, updated_at
             FROM concur_send_items WHERE batch_id = ?1 ORDER BY invoice_id",
        )?;
        let items = statement
            .query_map(params![batch_id], map_concur_send_item)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(items)
    }

    pub fn reserve_concur_send_item(
        &self,
        batch_id: i64,
        invoice_id: i64,
        is_trial: bool,
    ) -> StoreResult<ConcurReserveOutcome> {
        let transaction = self.conn.unchecked_transaction()?;
        let session = transaction
            .query_row(
                "SELECT batch_id, sender_email, recipient_email, trial_invoice_id,
                        trial_status, confirmed_behavior, confirmed_at, created_at, updated_at
                 FROM concur_send_sessions WHERE batch_id = ?1",
                params![batch_id],
                map_concur_send_session,
            )
            .optional()?
            .ok_or_else(|| StoreError::NotFound("Concur send session".to_string()))?;
        if is_trial {
            if invoice_id != session.trial_invoice_id {
                return Err(StoreError::Validation(
                    "only the configured trial invoice can be sent before confirmation".to_string(),
                ));
            }
            if session.trial_status == "unknown" {
                return Err(StoreError::Validation(
                    "trial send result is unknown; resolve it before retrying".to_string(),
                ));
            }
            if session.trial_status == "confirmed" {
                let item = get_concur_send_item_from(&transaction, batch_id, invoice_id)?;
                return Ok(ConcurReserveOutcome::AlreadySent(item));
            }
        } else if session.trial_status != "confirmed" {
            return Err(StoreError::Validation(
                "bulk Concur send requires a confirmed trial".to_string(),
            ));
        }

        let item = get_concur_send_item_from(&transaction, batch_id, invoice_id)?;
        match item.status.as_str() {
            "sent" => return Ok(ConcurReserveOutcome::AlreadySent(item)),
            "sending" => return Ok(ConcurReserveOutcome::InProgress),
            "unknown" => {
                return Err(StoreError::Validation(
                    "send result is unknown; resolve it before retrying".to_string(),
                ));
            }
            "pending" | "failed" => {}
            _ => {
                return Err(StoreError::Internal(
                    "invalid persisted Concur send status".to_string(),
                ));
            }
        }
        let now = now_text();
        let changed = transaction.execute(
            "UPDATE concur_send_items
             SET status = 'sending', attempt_count = attempt_count + 1,
                 last_error = NULL, updated_at = ?3
             WHERE batch_id = ?1 AND invoice_id = ?2 AND status IN ('pending', 'failed')",
            params![batch_id, invoice_id, now],
        )?;
        if changed != 1 {
            return Ok(ConcurReserveOutcome::InProgress);
        }
        if is_trial {
            transaction.execute(
                "UPDATE concur_send_sessions
                 SET trial_status = 'sending', updated_at = ?2 WHERE batch_id = ?1",
                params![batch_id, now],
            )?;
        }
        let item = get_concur_send_item_from(&transaction, batch_id, invoice_id)?;
        transaction.commit()?;
        Ok(ConcurReserveOutcome::Reserved(item))
    }

    /// Atomically reserves all 1-5 receipts in one bulk SMTP message.
    /// If any item is no longer pending/failed, the transaction rolls back the whole group.
    pub fn reserve_concur_send_group(
        &self,
        batch_id: i64,
        invoice_ids: &[i64],
    ) -> StoreResult<Vec<ConcurSendItem>> {
        validate_invoice_group(invoice_ids)?;
        let transaction = self.conn.unchecked_transaction()?;
        let trial_status: String = transaction
            .query_row(
                "SELECT trial_status FROM concur_send_sessions WHERE batch_id = ?1",
                params![batch_id],
                |row| row.get(0),
            )
            .optional()?
            .ok_or_else(|| StoreError::NotFound("Concur send session".to_string()))?;
        if trial_status != "confirmed" {
            return Err(StoreError::Validation(
                "bulk Concur send requires a confirmed trial".to_string(),
            ));
        }
        for invoice_id in invoice_ids {
            let item = get_concur_send_item_from(&transaction, batch_id, *invoice_id)?;
            if !matches!(item.status.as_str(), "pending" | "failed") {
                return Err(StoreError::Validation(format!(
                    "invoice {invoice_id} cannot be reserved because its status is {}",
                    item.status
                )));
            }
        }
        let now = now_text();
        for invoice_id in invoice_ids {
            let changed = transaction.execute(
                "UPDATE concur_send_items
                 SET status = 'sending', attempt_count = attempt_count + 1,
                     last_error = NULL, updated_at = ?3
                 WHERE batch_id = ?1 AND invoice_id = ?2 AND status IN ('pending', 'failed')",
                params![batch_id, invoice_id, now],
            )?;
            if changed != 1 {
                return Err(StoreError::Validation(
                    "Concur send group changed concurrently; no attachment was reserved"
                        .to_string(),
                ));
            }
        }
        let mut reserved = Vec::with_capacity(invoice_ids.len());
        for invoice_id in invoice_ids {
            reserved.push(get_concur_send_item_from(
                &transaction,
                batch_id,
                *invoice_id,
            )?);
        }
        transaction.commit()?;
        Ok(reserved)
    }

    pub fn mark_concur_items_sent(
        &self,
        batch_id: i64,
        invoice_ids: &[i64],
        message_id: &str,
    ) -> StoreResult<()> {
        validate_invoice_group(invoice_ids)?;
        if message_id.trim().is_empty()
            || message_id.chars().count() > 998
            || message_id.chars().any(char::is_control)
        {
            return Err(StoreError::Validation(
                "invalid SMTP message id".to_string(),
            ));
        }
        let transaction = self.conn.unchecked_transaction()?;
        let now = now_text();
        for invoice_id in invoice_ids {
            let changed = transaction.execute(
                "UPDATE concur_send_items
                 SET status = 'sent', message_id = ?3, sent_at = ?4,
                     last_error = NULL, updated_at = ?4
                 WHERE batch_id = ?1 AND invoice_id = ?2 AND status = 'sending'",
                params![batch_id, invoice_id, message_id, now],
            )?;
            if changed != 1 {
                return Err(StoreError::Validation(format!(
                    "invoice {invoice_id} was not reserved for sending"
                )));
            }
        }
        update_trial_status_if_present(&transaction, batch_id, invoice_ids, "sent", &now)?;
        transaction.commit()?;
        Ok(())
    }

    pub fn mark_concur_items_unknown(
        &self,
        batch_id: i64,
        invoice_ids: &[i64],
        reason: &str,
    ) -> StoreResult<()> {
        self.mark_concur_items_after_attempt(batch_id, invoice_ids, "unknown", reason)
    }

    pub fn mark_concur_items_failed(
        &self,
        batch_id: i64,
        invoice_ids: &[i64],
        reason: &str,
    ) -> StoreResult<()> {
        self.mark_concur_items_after_attempt(batch_id, invoice_ids, "failed", reason)
    }

    fn mark_concur_items_after_attempt(
        &self,
        batch_id: i64,
        invoice_ids: &[i64],
        status: &str,
        reason: &str,
    ) -> StoreResult<()> {
        validate_invoice_group(invoice_ids)?;
        validate_send_error(reason)?;
        if !matches!(status, "failed" | "unknown") {
            return Err(StoreError::Internal(
                "invalid Concur attempt result".to_string(),
            ));
        }
        let transaction = self.conn.unchecked_transaction()?;
        let now = now_text();
        for invoice_id in invoice_ids {
            let changed = transaction.execute(
                "UPDATE concur_send_items
                 SET status = ?3, last_error = ?4, updated_at = ?5
                 WHERE batch_id = ?1 AND invoice_id = ?2 AND status = 'sending'",
                params![batch_id, invoice_id, status, reason, now],
            )?;
            if changed != 1 {
                return Err(StoreError::Validation(format!(
                    "invoice {invoice_id} was not reserved for sending"
                )));
            }
        }
        update_trial_status_if_present(&transaction, batch_id, invoice_ids, status, &now)?;
        transaction.commit()?;
        Ok(())
    }

    pub fn resolve_concur_unknown_item(
        &self,
        batch_id: i64,
        invoice_id: i64,
        delivered: bool,
    ) -> StoreResult<()> {
        let transaction = self.conn.unchecked_transaction()?;
        let now = now_text();
        let next_status = if delivered { "sent" } else { "failed" };
        let changed = transaction.execute(
            "UPDATE concur_send_items
             SET status = ?3,
                 last_error = CASE WHEN ?3 = 'failed' THEN '用户确认未送达，可安全重试' ELSE NULL END,
                 sent_at = CASE WHEN ?3 = 'sent' THEN ?4 ELSE sent_at END,
                 updated_at = ?4
             WHERE batch_id = ?1 AND invoice_id = ?2 AND status = 'unknown'",
            params![batch_id, invoice_id, next_status, now],
        )?;
        if changed != 1 {
            return Err(StoreError::Validation(
                "only an unknown Concur send can be resolved manually".to_string(),
            ));
        }
        update_trial_status_if_present(&transaction, batch_id, &[invoice_id], next_status, &now)?;
        transaction.commit()?;
        Ok(())
    }

    pub fn confirm_concur_trial(&self, batch_id: i64, behavior: &str) -> StoreResult<()> {
        if !matches!(behavior, "receipt_library" | "expenseit") {
            return Err(StoreError::Validation(
                "invalid Concur trial behavior".to_string(),
            ));
        }
        let transaction = self.conn.unchecked_transaction()?;
        let session = transaction
            .query_row(
                "SELECT batch_id, sender_email, recipient_email, trial_invoice_id,
                        trial_status, confirmed_behavior, confirmed_at, created_at, updated_at
                 FROM concur_send_sessions WHERE batch_id = ?1",
                params![batch_id],
                map_concur_send_session,
            )
            .optional()?
            .ok_or_else(|| StoreError::NotFound("Concur send session".to_string()))?;
        if session.trial_status == "confirmed" {
            if session.confirmed_behavior.as_deref() == Some(behavior) {
                return Ok(());
            }
            return Err(StoreError::Validation(
                "confirmed Concur behavior cannot be changed".to_string(),
            ));
        }
        if session.trial_status != "sent" {
            return Err(StoreError::Validation(
                "trial receipt must be confirmed as delivered before bulk send".to_string(),
            ));
        }
        let now = now_text();
        transaction.execute(
            "UPDATE concur_send_sessions
             SET trial_status = 'confirmed', confirmed_behavior = ?2,
                 confirmed_at = ?3, updated_at = ?3 WHERE batch_id = ?1",
            params![batch_id, behavior, now],
        )?;
        transaction.commit()?;
        Ok(())
    }
}

fn get_concur_send_item_from(
    connection: &Connection,
    batch_id: i64,
    invoice_id: i64,
) -> StoreResult<ConcurSendItem> {
    connection
        .query_row(
            "SELECT batch_id, invoice_id, idempotency_key, attachment_name,
                    attachment_sha256, status, attempt_count, last_error,
                    message_id, sent_at, updated_at
             FROM concur_send_items WHERE batch_id = ?1 AND invoice_id = ?2",
            params![batch_id, invoice_id],
            map_concur_send_item,
        )
        .optional()?
        .ok_or_else(|| StoreError::NotFound("Concur send item".to_string()))
}

fn update_trial_status_if_present(
    connection: &Connection,
    batch_id: i64,
    invoice_ids: &[i64],
    status: &str,
    now: &str,
) -> StoreResult<()> {
    let trial_invoice_id: i64 = connection.query_row(
        "SELECT trial_invoice_id FROM concur_send_sessions WHERE batch_id = ?1",
        params![batch_id],
        |row| row.get(0),
    )?;
    if invoice_ids.contains(&trial_invoice_id) {
        connection.execute(
            "UPDATE concur_send_sessions
             SET trial_status = ?2, updated_at = ?3 WHERE batch_id = ?1",
            params![batch_id, status, now],
        )?;
    }
    Ok(())
}

fn map_concur_send_session(row: &Row<'_>) -> rusqlite::Result<ConcurSendSession> {
    Ok(ConcurSendSession {
        batch_id: row.get(0)?,
        sender_email: row.get(1)?,
        recipient_email: row.get(2)?,
        trial_invoice_id: row.get(3)?,
        trial_status: row.get(4)?,
        confirmed_behavior: row.get(5)?,
        confirmed_at: row.get(6)?,
        created_at: row.get(7)?,
        updated_at: row.get(8)?,
    })
}

fn map_concur_send_item(row: &Row<'_>) -> rusqlite::Result<ConcurSendItem> {
    Ok(ConcurSendItem {
        batch_id: row.get(0)?,
        invoice_id: row.get(1)?,
        idempotency_key: row.get(2)?,
        attachment_name: row.get(3)?,
        attachment_sha256: row.get(4)?,
        status: row.get(5)?,
        attempt_count: row.get(6)?,
        last_error: row.get(7)?,
        message_id: row.get(8)?,
        sent_at: row.get(9)?,
        updated_at: row.get(10)?,
    })
}

fn validate_email_shape(value: &str, label: &str) -> StoreResult<()> {
    if value.trim() != value
        || value.chars().count() > 320
        || value.chars().any(char::is_control)
        || value.matches('@').count() != 1
    {
        return Err(StoreError::Validation(format!(
            "invalid Concur {label} email"
        )));
    }
    Ok(())
}

fn validate_concur_send_item(item: &NewConcurSendItem) -> StoreResult<()> {
    if item.invoice_id <= 0
        || item.idempotency_key.len() != 64
        || !item
            .idempotency_key
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
        || item.attachment_sha256.len() != 64
        || !item
            .attachment_sha256
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
        || item.attachment_name.trim() != item.attachment_name
        || item.attachment_name.is_empty()
        || item.attachment_name.chars().count() > 180
        || item.attachment_name.chars().any(char::is_control)
        || item.attachment_name.contains(['/', '\\'])
    {
        return Err(StoreError::Validation(
            "invalid Concur send item metadata".to_string(),
        ));
    }
    Ok(())
}

fn validate_invoice_group(invoice_ids: &[i64]) -> StoreResult<()> {
    if invoice_ids.is_empty()
        || invoice_ids.len() > 5
        || invoice_ids.iter().any(|invoice_id| *invoice_id <= 0)
        || invoice_ids.iter().collect::<HashSet<_>>().len() != invoice_ids.len()
    {
        return Err(StoreError::Validation(
            "one Concur message must contain 1 to 5 unique attachments".to_string(),
        ));
    }
    Ok(())
}

fn validate_send_error(reason: &str) -> StoreResult<()> {
    if reason.trim().is_empty()
        || reason.chars().count() > 500
        || reason.chars().any(char::is_control)
    {
        return Err(StoreError::Validation(
            "invalid Concur send error message".to_string(),
        ));
    }
    Ok(())
}

fn now_text() -> String {
    Utc::now()
        .naive_utc()
        .format("%Y-%m-%d %H:%M:%S")
        .to_string()
}

#[cfg(test)]
mod tests {
    use chrono::NaiveDate;
    use rust_decimal::Decimal;

    use super::*;
    use crate::models::{ReportedInvoice, TicketType};

    fn add_synthetic_invoices(db: &LedgerDb, batch_id: i64, count: usize) -> Vec<i64> {
        (1..=count)
            .map(|index| {
                let invoice = ReportedInvoice {
                    id: 0,
                    batch_id,
                    invoice_number: format!("{index:020}"),
                    issue_date: NaiveDate::from_ymd_opt(2026, 7, 15).unwrap(),
                    amount: Decimal::new(10_000 + index as i64, 2),
                    tax_amount: None,
                    buyer_name: Some("Synthetic Buyer".to_string()),
                    seller_name: Some("Synthetic Seller".to_string()),
                    ticket_type: TicketType::Other,
                    city: Some("Beijing".to_string()),
                    departure_time: None,
                    checkin_date: None,
                    file_path: format!(r"C:\synthetic\receipt-{index}.pdf"),
                    created_at: Utc::now().naive_utc(),
                    updated_at: Utc::now().naive_utc(),
                    verification_result: Some("valid".to_string()),
                    is_duplicate: false,
                    duplicate_reason: None,
                };
                db.add_invoice(&invoice).unwrap()
            })
            .collect()
    }

    fn plan_for(invoice_ids: &[i64]) -> Vec<NewConcurSendItem> {
        invoice_ids
            .iter()
            .enumerate()
            .map(|(index, invoice_id)| NewConcurSendItem {
                invoice_id: *invoice_id,
                idempotency_key: format!("{:064x}", index + 1),
                attachment_name: format!("receipt-{}.pdf", index + 1),
                attachment_sha256: format!("{:064X}", index + 101),
            })
            .collect()
    }

    fn approved_fixture(db: &LedgerDb, count: usize) -> (i64, Vec<i64>, Vec<NewConcurSendItem>) {
        let batch_id = db
            .create_batch("Synthetic Concur batch", "2026-07")
            .unwrap();
        let invoice_ids = add_synthetic_invoices(db, batch_id, count);
        db.transition_batch_status(batch_id, BatchStatus::Submitted)
            .unwrap();
        db.transition_batch_status(batch_id, BatchStatus::Approved)
            .unwrap();
        let items = plan_for(&invoice_ids);
        (batch_id, invoice_ids, items)
    }

    #[test]
    fn concur_trial_must_be_confirmed_before_bulk_and_sent_items_are_idempotent() {
        let db = LedgerDb::new(":memory:").unwrap();
        let (batch_id, invoice_ids, items) = approved_fixture(&db, 2);
        db.initialize_concur_send_session(
            batch_id,
            "sender@example.test",
            "receipts@concur.example",
            invoice_ids[0],
            &items,
        )
        .unwrap();

        let reserved = db
            .reserve_concur_send_item(batch_id, invoice_ids[0], true)
            .unwrap();
        assert!(matches!(
            reserved,
            ConcurReserveOutcome::Reserved(ref item)
                if item.status == "sending" && item.attempt_count == 1
        ));
        let bulk_error = db
            .reserve_concur_send_item(batch_id, invoice_ids[1], false)
            .unwrap_err();
        assert!(bulk_error.to_string().contains("confirmed trial"));

        db.mark_concur_items_sent(batch_id, &[invoice_ids[0]], "<trial@example.test>")
            .unwrap();
        assert_eq!(
            db.get_concur_send_session(batch_id)
                .unwrap()
                .unwrap()
                .trial_status,
            "sent"
        );
        db.confirm_concur_trial(batch_id, "receipt_library")
            .unwrap();

        let trial_again = db
            .reserve_concur_send_item(batch_id, invoice_ids[0], true)
            .unwrap();
        assert!(matches!(
            trial_again,
            ConcurReserveOutcome::AlreadySent(ref item) if item.attempt_count == 1
        ));

        let bulk = db
            .reserve_concur_send_item(batch_id, invoice_ids[1], false)
            .unwrap();
        assert!(matches!(bulk, ConcurReserveOutcome::Reserved(_)));
        db.mark_concur_items_sent(batch_id, &[invoice_ids[1]], "<bulk@example.test>")
            .unwrap();
        let bulk_again = db
            .reserve_concur_send_item(batch_id, invoice_ids[1], false)
            .unwrap();
        assert!(matches!(
            bulk_again,
            ConcurReserveOutcome::AlreadySent(ref item) if item.attempt_count == 1
        ));
    }

    #[test]
    fn concur_configuration_and_reviewed_attachment_set_are_immutable() {
        let db = LedgerDb::new(":memory:").unwrap();
        let (batch_id, invoice_ids, items) = approved_fixture(&db, 2);
        db.initialize_concur_send_session(
            batch_id,
            "sender@example.test",
            "receipts@concur.example",
            invoice_ids[0],
            &items,
        )
        .unwrap();

        let changed_sender = db
            .initialize_concur_send_session(
                batch_id,
                "other@example.test",
                "receipts@concur.example",
                invoice_ids[0],
                &items,
            )
            .unwrap_err();
        assert!(changed_sender.to_string().contains("cannot be changed"));

        let removed_attachment = db
            .initialize_concur_send_session(
                batch_id,
                "sender@example.test",
                "receipts@concur.example",
                invoice_ids[0],
                &items[..1],
            )
            .unwrap_err();
        assert!(removed_attachment
            .to_string()
            .contains("attachment set cannot be changed"));
    }

    #[test]
    fn interrupted_send_becomes_unknown_and_requires_manual_resolution() {
        let temp_dir = tempfile::tempdir().unwrap();
        let db_path = temp_dir.path().join("ledger.db");
        let (batch_id, trial_invoice_id) = {
            let db = LedgerDb::new(&db_path).unwrap();
            let (batch_id, invoice_ids, items) = approved_fixture(&db, 1);
            db.initialize_concur_send_session(
                batch_id,
                "sender@example.test",
                "receipts@concur.example",
                invoice_ids[0],
                &items,
            )
            .unwrap();
            db.reserve_concur_send_item(batch_id, invoice_ids[0], true)
                .unwrap();
            (batch_id, invoice_ids[0])
        };

        let db = LedgerDb::new(&db_path).unwrap();
        let session = db.get_concur_send_session(batch_id).unwrap().unwrap();
        assert_eq!(session.trial_status, "unknown");
        assert_eq!(
            db.list_concur_send_items(batch_id).unwrap()[0].status,
            "unknown"
        );
        assert!(db
            .reserve_concur_send_item(batch_id, trial_invoice_id, true)
            .unwrap_err()
            .to_string()
            .contains("resolve"));

        db.resolve_concur_unknown_item(batch_id, trial_invoice_id, false)
            .unwrap();
        let retried = db
            .reserve_concur_send_item(batch_id, trial_invoice_id, true)
            .unwrap();
        assert!(matches!(
            retried,
            ConcurReserveOutcome::Reserved(ref item) if item.attempt_count == 2
        ));
        db.mark_concur_items_sent(batch_id, &[trial_invoice_id], "<retry@example.test>")
            .unwrap();
        db.confirm_concur_trial(batch_id, "expenseit").unwrap();
        assert_eq!(
            db.get_concur_send_session(batch_id)
                .unwrap()
                .unwrap()
                .confirmed_behavior
                .as_deref(),
            Some("expenseit")
        );
    }

    #[test]
    fn draft_and_excluded_invoices_cannot_enter_concur_plan() {
        let db = LedgerDb::new(":memory:").unwrap();
        let draft_batch_id = db.create_batch("Draft", "2026-07").unwrap();
        let draft_ids = add_synthetic_invoices(&db, draft_batch_id, 1);
        let draft_items = plan_for(&draft_ids);
        assert!(db
            .initialize_concur_send_session(
                draft_batch_id,
                "sender@example.test",
                "receipts@concur.example",
                draft_ids[0],
                &draft_items,
            )
            .unwrap_err()
            .to_string()
            .contains("approved or completed"));

        let excluded_batch_id = db.create_batch("Excluded", "2026-07").unwrap();
        let excluded_ids = add_synthetic_invoices(&db, excluded_batch_id, 2);
        db.set_invoice_excluded_with_audit(excluded_ids[1], true)
            .unwrap();
        db.transition_batch_status(excluded_batch_id, BatchStatus::Submitted)
            .unwrap();
        db.transition_batch_status(excluded_batch_id, BatchStatus::Approved)
            .unwrap();
        let excluded_items = plan_for(&excluded_ids);
        assert!(db
            .initialize_concur_send_session(
                excluded_batch_id,
                "sender@example.test",
                "receipts@concur.example",
                excluded_ids[0],
                &excluded_items,
            )
            .unwrap_err()
            .to_string()
            .contains("not an eligible member"));
    }

    #[test]
    fn bulk_message_group_is_reserved_atomically_after_trial_confirmation() {
        let db = LedgerDb::new(":memory:").unwrap();
        let (batch_id, invoice_ids, items) = approved_fixture(&db, 3);
        db.initialize_concur_send_session(
            batch_id,
            "sender@example.test",
            "receipts@concur.example",
            invoice_ids[0],
            &items,
        )
        .unwrap();
        db.reserve_concur_send_item(batch_id, invoice_ids[0], true)
            .unwrap();
        db.mark_concur_items_sent(batch_id, &[invoice_ids[0]], "<trial@example.test>")
            .unwrap();
        db.confirm_concur_trial(batch_id, "receipt_library")
            .unwrap();

        let reserved = db
            .reserve_concur_send_group(batch_id, &invoice_ids[1..])
            .unwrap();
        assert_eq!(reserved.len(), 2);
        assert!(reserved
            .iter()
            .all(|item| item.status == "sending" && item.attempt_count == 1));
        assert!(db
            .reserve_concur_send_group(batch_id, &invoice_ids[1..])
            .unwrap_err()
            .to_string()
            .contains("cannot be reserved"));
        db.mark_concur_items_sent(batch_id, &invoice_ids[1..], "<bulk@example.test>")
            .unwrap();
    }

    #[test]
    fn one_message_rejects_more_than_five_attachments() {
        let db = LedgerDb::new(":memory:").unwrap();
        let error = db
            .mark_concur_items_sent(1, &[1, 2, 3, 4, 5, 6], "<too-many@example.test>")
            .unwrap_err();
        assert!(error.to_string().contains("1 to 5"));
    }
}
