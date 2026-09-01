use anyhow::Result;
use std::env;
use std::fs;

use invoice_collect::config::{DateRange, ImapConfig};
use invoice_collect::extract::extract_email;
use invoice_collect::imap_client::Session;

fn main() -> Result<()> {
    let username = env::var("INVOICE_IMAP_USERNAME")?;
    let cfg = ImapConfig::from_env(&username)?;

    let mut session = Session::connect(&cfg)?;
    let range = DateRange::parse("2026-06-01", "2026-07-01")?;
    let _uids = session.search_range("INBOX", &range)?;

    // Fetch additional ZIP samples
    let zip_uids = vec![614, 620];

    for uid in zip_uids {
        println!("\n=== UID {} ===", uid);

        match session.fetch_raw(uid) {
            Ok(raw) => match extract_email(&raw) {
                Ok(email) => {
                    for att in &email.attachments {
                        if att.filename.to_lowercase().ends_with(".zip") {
                            let zip_path = format!("/tmp/investigate_uid_{}.zip", uid);
                            fs::write(&zip_path, &att.data)?;
                            println!("Saved: {}", zip_path);
                            println!("Filename: {}", att.filename);
                            println!("Size: {} bytes", att.data.len());
                        }
                    }
                }
                Err(e) => println!("Parse error: {}", e),
            },
            Err(e) => println!("Fetch error: {}", e),
        }
    }

    Ok(())
}
