use anyhow::{Context, Result};
use std::env;
use std::fs;
use std::path::Path;

use invoice_collect::config::{DateRange, ImapConfig};
use invoice_collect::extract::extract_email;
use invoice_collect::imap_client::Session;

fn main() -> Result<()> {
    // Load .env.local
    let env_path = Path::new("/home/holo/work-tools/.env.local");
    if env_path.exists() {
        let env_content = fs::read_to_string(env_path)?;
        for line in env_content.lines() {
            if line.trim().is_empty() || line.trim().starts_with('#') {
                continue;
            }
            if let Some((key, value)) = line.split_once('=') {
                env::set_var(key.trim(), value.trim());
            }
        }
    }

    let username = "879455187@qq.com";
    let cfg = ImapConfig::from_env(username)?;

    let mut session = Session::connect(&cfg)?;
    let range = DateRange::parse("2026-06-01", "2026-07-01")?;
    let _uids = session.search_range("INBOX", &range)?;

    // Fetch additional ZIP samples
    let zip_uids = vec![614, 620];

    for uid in zip_uids {
        println!("\n=== UID {} ===", uid);

        match session.fetch_raw(uid) {
            Ok(raw) => {
                match extract_email(&raw) {
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
                }
            }
            Err(e) => println!("Fetch error: {}", e),
        }
    }

    Ok(())
}
