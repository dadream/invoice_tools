use anyhow::{Context, Result};
use std::env;
use std::fs;
use std::path::Path;

// Import from the invoice-collect crate
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

    // Get username from command line or use a default
    let args: Vec<String> = env::args().collect();
    let username = args
        .get(1)
        .cloned()
        .or_else(|| env::var("INVOICE_EMAIL_ADDRESS").ok())
        .context("Usage: investigate_emails <email_address>")?;

    let cfg = ImapConfig::from_env(&username)?;

    println!(
        "Connecting to {}:{} as {}",
        cfg.host, cfg.port, cfg.username
    );
    let mut session = Session::connect(&cfg)?;

    // Select INBOX first
    let _range = DateRange::parse("2026-06-01", "2026-07-01")?;
    let _uids = session.search_range("INBOX", &_range)?;
    println!("Found {} emails in INBOX for June 2026", _uids.len());

    // UIDs to investigate
    let zip_uid = 613; // Pick first from 613-624 range
    let link_uids = vec![572, 578, 629];

    println!("\n## ZIP Contents (sample: UID {})", zip_uid);
    println!("Fetching UID {}...", zip_uid);

    match session.fetch_raw(zip_uid) {
        Ok(raw) => {
            match extract_email(&raw) {
                Ok(email) => {
                    println!("Subject: {}", email.subject);
                    println!("From: {}", email.from);
                    println!("Attachments: {}", email.attachments.len());

                    for (i, att) in email.attachments.iter().enumerate() {
                        println!("\n  Attachment {}: {}", i + 1, att.filename);
                        println!("  Content-Type: {}", att.content_type);
                        println!("  Size: {} bytes", att.data.len());

                        // If it's a ZIP, extract and list contents
                        if att.filename.to_lowercase().ends_with(".zip") {
                            let zip_path = format!("/tmp/investigate_uid_{}.zip", zip_uid);
                            fs::write(&zip_path, &att.data)?;
                            println!("  Saved to: {}", zip_path);

                            // List ZIP contents using zipinfo or manual extraction
                            // Try zipinfo first, fallback to manual
                            let output = std::process::Command::new("zipinfo")
                                .arg("-1")
                                .arg(&zip_path)
                                .output();

                            match output {
                                Ok(out) if out.status.success() => {
                                    println!("\n  ZIP contents (from zipinfo):");
                                    println!("{}", String::from_utf8_lossy(&out.stdout));
                                }
                                _ => {
                                    // Try using zip crate or just report manual inspection needed
                                    println!("\n  ZIP saved but zipinfo not available.");
                                    println!(
                                        "  Manual inspection: Use `unzip -l {}` or `7z l {}`",
                                        zip_path, zip_path
                                    );
                                }
                            }
                        }
                    }
                }
                Err(e) => println!("Failed to parse email: {}", e),
            }
        }
        Err(e) => println!("Failed to fetch UID {}: {}", zip_uid, e),
    }

    // Now check the link-only emails
    println!("\n## Link-only Email Bodies\n");

    for uid in link_uids {
        println!("### UID {}", uid);

        match session.fetch_raw(uid) {
            Ok(raw) => {
                match extract_email(&raw) {
                    Ok(email) => {
                        println!("Sender: {}", email.from);
                        println!("Subject: {}", email.subject);
                        println!("Attachments: {}", email.attachments.len());

                        // Parse the email to get the body
                        if let Some(message) = mail_parser::MessageParser::default().parse(&raw) {
                            // Try to get text/plain body first
                            let mut body_text = String::new();

                            if let Some(text) = message.body_text(0) {
                                body_text = text.to_string();
                            } else if let Some(html) = message.body_html(0) {
                                // Basic HTML stripping - just show raw for now
                                body_text = html.to_string();
                            }

                            // Show first 800 chars of body
                            let excerpt: String = body_text.chars().take(800).collect();
                            println!("Body excerpt (first 800 chars):");
                            println!("{}", excerpt);

                            // Check for URLs
                            let has_download = body_text.contains("download")
                                || body_text.contains("下载")
                                || body_text.contains("发票")
                                || body_text.contains("http");

                            if has_download {
                                println!("\nContains download link: YES");
                                // Try to extract URLs
                                for line in body_text.lines() {
                                    if line.contains("http://") || line.contains("https://") {
                                        println!("URL found: {}", line.trim());
                                    }
                                }
                            } else {
                                println!("\nContains download link: NO");
                            }
                        }
                    }
                    Err(e) => println!("Failed to parse email: {}", e),
                }
            }
            Err(e) => println!("Failed to fetch UID {}: {}", uid, e),
        }

        println!();
    }

    Ok(())
}
