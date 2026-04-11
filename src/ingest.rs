use crate::channel::{self, TriggerContext};
use crate::config::Config;
use mail_parser::{MessageParser, MimeHeaders};
use serde::Serialize;
use std::io::Read;
use std::net::IpAddr;
use std::path::Path;

#[derive(Debug, Clone, Serialize, serde::Deserialize)]
pub struct EmailMetadata {
    pub id: String,
    pub message_id: String,
    pub from: String,
    pub to: String,
    pub subject: String,
    pub date: String,
    pub in_reply_to: String,
    pub references: String,
    pub attachments: Vec<AttachmentMeta>,
    pub mailbox: String,
    pub read: bool,
    #[serde(default = "default_auth_result")]
    pub dkim: String,
    #[serde(default = "default_auth_result")]
    pub spf: String,
}

fn default_auth_result() -> String {
    "none".to_string()
}

#[derive(Debug, Clone, Serialize, serde::Deserialize)]
pub struct AttachmentMeta {
    pub filename: String,
    pub content_type: String,
    pub size: usize,
    pub path: String,
}

pub fn run(
    rcpt: &str,
    data_dir: Option<&std::path::Path>,
) -> Result<(), Box<dyn std::error::Error>> {
    let config = match data_dir {
        Some(dir) => Config::load_from_data_dir(dir)?,
        None => Config::load_default()?,
    };
    let mut raw = Vec::new();
    std::io::stdin().read_to_end(&mut raw)?;
    ingest_email(&config, rcpt, &raw)
}

pub fn ingest_email(
    config: &Config,
    rcpt: &str,
    raw: &[u8],
) -> Result<(), Box<dyn std::error::Error>> {
    let local_part = extract_local_part(rcpt);
    let mailbox = config.resolve_mailbox(local_part);
    let mailbox_dir = config.mailbox_dir(&mailbox);

    std::fs::create_dir_all(&mailbox_dir)?;

    let message = MessageParser::default()
        .parse(raw)
        .ok_or("Failed to parse email")?;

    let from = message
        .from()
        .and_then(|a| a.first())
        .map(|a| {
            a.address()
                .map(|addr| match a.name() {
                    Some(name) => format!("{name} <{addr}>"),
                    None => addr.to_string(),
                })
                .unwrap_or_default()
        })
        .unwrap_or_default();

    let to = message
        .to()
        .and_then(|a| a.first())
        .map(|a| {
            a.address()
                .map(|addr| match a.name() {
                    Some(name) => format!("{name} <{addr}>"),
                    None => addr.to_string(),
                })
                .unwrap_or_default()
        })
        .unwrap_or_default();

    let subject = message.subject().unwrap_or("(no subject)").to_string();

    let date = message
        .date()
        .map(|d| d.to_rfc3339())
        .unwrap_or_else(|| chrono::Utc::now().to_rfc3339());

    let message_id = message.message_id().unwrap_or("").to_string();

    let in_reply_to = message
        .in_reply_to()
        .as_text()
        .unwrap_or_default()
        .to_string();

    let references = message
        .references()
        .as_text()
        .unwrap_or_default()
        .to_string();

    let body = extract_body(&message);

    let attachments = extract_attachments(&message, &mailbox_dir)?;

    let (dkim_result, spf_result) = verify_auth(raw, rcpt);

    let meta_template = EmailMetadata {
        id: String::new(), // placeholder, set after atomic creation
        message_id,
        from,
        to,
        subject,
        date,
        in_reply_to,
        references,
        attachments,
        mailbox: mailbox.clone(),
        read: false,
        dkim: dkim_result,
        spf: spf_result,
    };

    let (_id, filepath, meta) = create_file_atomic(&mailbox_dir, &meta_template, &body)?;

    if let Some(mailbox_config) = config.mailboxes.get(&mailbox) {
        let ctx = TriggerContext {
            filepath: &filepath,
            metadata: &meta,
        };
        channel::execute_triggers(mailbox_config, &ctx);
    }

    Ok(())
}

fn create_resolver() -> Option<mail_auth::Resolver> {
    mail_auth::Resolver::new_system_conf()
        .or_else(|_| mail_auth::Resolver::new_cloudflare())
        .ok()
}

fn verify_auth(raw: &[u8], rcpt: &str) -> (String, String) {
    let rt = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(_) => return ("none".to_string(), "none".to_string()),
    };

    let resolver = match create_resolver() {
        Some(r) => r,
        None => return ("none".to_string(), "none".to_string()),
    };

    rt.block_on(async {
        let dkim_result = verify_dkim_async(raw, &resolver).await;
        let spf_result = verify_spf_async(raw, rcpt, &resolver).await;
        (dkim_result, spf_result)
    })
}

async fn verify_dkim_async(raw: &[u8], resolver: &mail_auth::Resolver) -> String {
    let auth_msg = match mail_auth::AuthenticatedMessage::parse(raw) {
        Some(msg) => msg,
        None => return "none".to_string(),
    };

    if auth_msg.dkim_headers.is_empty() {
        return "none".to_string();
    }

    let results = resolver.verify_dkim(&auth_msg).await;

    if results.is_empty() {
        return "none".to_string();
    }

    for output in &results {
        if matches!(output.result(), mail_auth::DkimResult::Pass) {
            return "pass".to_string();
        }
    }

    "fail".to_string()
}

pub fn spf_domain(mail_from: &str) -> Option<&str> {
    let domain = mail_from.split('@').nth(1).unwrap_or("");
    if domain.is_empty() {
        None
    } else {
        Some(domain)
    }
}

async fn verify_spf_async(raw: &[u8], _rcpt: &str, resolver: &mail_auth::Resolver) -> String {
    let ip = match extract_received_ip(raw) {
        Some(ip) => ip,
        None => return "none".to_string(),
    };

    let mail_from = extract_mail_from(raw).unwrap_or_default();

    let helo_domain = match spf_domain(&mail_from) {
        Some(d) => d,
        None => return "none".to_string(),
    };

    let spf_output = resolver
        .verify_spf_sender(ip, helo_domain, helo_domain, &mail_from)
        .await;

    match spf_output.result() {
        mail_auth::SpfResult::Pass => "pass".to_string(),
        mail_auth::SpfResult::Fail => "fail".to_string(),
        mail_auth::SpfResult::SoftFail => "fail".to_string(),
        mail_auth::SpfResult::None => "none".to_string(),
        _ => "fail".to_string(),
    }
}

fn unfold_headers(raw: &str) -> String {
    let mut result = String::with_capacity(raw.len());
    for line in raw.lines() {
        if line.starts_with(' ') || line.starts_with('\t') {
            result.push(' ');
            result.push_str(line.trim());
        } else {
            if !result.is_empty() {
                result.push('\n');
            }
            result.push_str(line);
        }
    }
    result
}

fn extract_received_ip(raw: &[u8]) -> Option<IpAddr> {
    let header_section = std::str::from_utf8(raw).ok()?;
    let unfolded = unfold_headers(header_section);
    for line in unfolded.lines() {
        if line.is_empty() {
            break;
        }
        if (line.starts_with("Received:") || line.starts_with("received:"))
            && let Some(ip) = parse_ip_from_received(line)
        {
            return Some(ip);
        }
    }
    None
}

fn parse_ip_from_received(line: &str) -> Option<IpAddr> {
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '[' {
            let candidate: String = chars.by_ref().take_while(|&ch| ch != ']').collect();
            if let Ok(ip) = candidate.parse::<IpAddr>()
                && !ip.is_loopback()
            {
                return Some(ip);
            }
        }
    }

    for word in line.split_whitespace() {
        let trimmed = word.trim_matches(|c: char| !c.is_ascii_digit() && c != '.' && c != ':');
        if let Ok(ip) = trimmed.parse::<IpAddr>()
            && !ip.is_loopback()
        {
            return Some(ip);
        }
    }

    None
}

fn extract_mail_from(raw: &[u8]) -> Option<String> {
    let header_section = std::str::from_utf8(raw).ok()?;
    for line in header_section.lines() {
        if line.is_empty() {
            break;
        }
        if line.to_lowercase().starts_with("from:") {
            let addr = line[5..].trim();
            if let Some(start) = addr.find('<')
                && let Some(end) = addr.find('>')
            {
                return Some(addr[start + 1..end].to_string());
            }
            return Some(addr.to_string());
        }
    }
    None
}

fn extract_local_part(rcpt: &str) -> &str {
    rcpt.split('@').next().unwrap_or(rcpt)
}

fn extract_body(message: &mail_parser::Message) -> String {
    if let Some(text) = message.body_text(0) {
        return text.to_string();
    }

    if let Some(html) = message.body_html(0) {
        return html2text::from_read(html.as_bytes(), 80).unwrap_or_else(|_| html.to_string());
    }

    String::new()
}

fn extract_attachments(
    message: &mail_parser::Message,
    mailbox_dir: &Path,
) -> Result<Vec<AttachmentMeta>, Box<dyn std::error::Error>> {
    let mut result = Vec::new();
    let attachments_dir = mailbox_dir.join("attachments");

    for attachment in message.attachments() {
        let raw_name = match attachment.attachment_name() {
            Some(name) => name.to_string(),
            None => continue,
        };

        let filename = Path::new(&raw_name)
            .file_name()
            .and_then(|f| f.to_str())
            .unwrap_or("")
            .to_string();

        if filename.is_empty() {
            continue;
        }

        std::fs::create_dir_all(&attachments_dir)?;

        let content_type = attachment
            .content_type()
            .map(|ct| {
                let main = ct.ctype();
                match ct.subtype() {
                    Some(sub) => format!("{main}/{sub}"),
                    None => main.to_string(),
                }
            })
            .unwrap_or_else(|| "application/octet-stream".to_string());

        let body = attachment.contents();
        let dest_filename = deduplicate_filename(&attachments_dir, &filename);
        let dest_path = attachments_dir.join(&dest_filename);

        std::fs::write(&dest_path, body)?;

        let relative_path = format!("attachments/{dest_filename}");

        result.push(AttachmentMeta {
            filename: dest_filename,
            content_type,
            size: body.len(),
            path: relative_path,
        });
    }

    Ok(result)
}

fn deduplicate_filename(dir: &Path, filename: &str) -> String {
    if !dir.join(filename).exists() {
        return filename.to_string();
    }

    let stem = Path::new(filename)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(filename);
    let ext = Path::new(filename).extension().and_then(|s| s.to_str());

    for i in 1.. {
        let candidate = match ext {
            Some(e) => format!("{stem}-{i}.{e}"),
            None => format!("{stem}-{i}"),
        };
        if !dir.join(&candidate).exists() {
            return candidate;
        }
    }

    unreachable!()
}

fn create_file_atomic(
    mailbox_dir: &Path,
    meta_template: &EmailMetadata,
    body: &str,
) -> Result<(String, std::path::PathBuf, EmailMetadata), Box<dyn std::error::Error>> {
    use std::fs::OpenOptions;
    use std::io::Write;

    let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
    let mut counter = 1u32;

    const MAX_COUNTER: u32 = 999_999;

    loop {
        if counter > MAX_COUNTER {
            return Err(format!(
                "Exhausted {MAX_COUNTER} file ID candidates for {today} in {}",
                mailbox_dir.display()
            )
            .into());
        }

        let candidate = format!("{today}-{counter:03}");
        let path = mailbox_dir.join(format!("{candidate}.md"));

        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(mut file) => {
                let mut meta = meta_template.clone();
                meta.id = candidate.clone();
                let content = format_markdown(&meta, body);
                file.write_all(content.as_bytes())?;
                return Ok((candidate, path, meta));
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                counter += 1;
                continue;
            }
            Err(e) => return Err(e.into()),
        }
    }
}

fn format_markdown(meta: &EmailMetadata, body: &str) -> String {
    let toml_str = toml::to_string_pretty(meta).expect("EmailMetadata must serialize to TOML");
    let mut result = String::new();
    result.push_str("+++\n");
    result.push_str(&toml_str);
    result.push_str("+++\n\n");
    result.push_str(body);

    if !body.ends_with('\n') {
        result.push('\n');
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, MailboxConfig};
    use std::collections::HashMap;
    use tempfile::TempDir;

    fn test_config(tmp: &Path) -> Config {
        let mut mailboxes = HashMap::new();
        mailboxes.insert(
            "catchall".to_string(),
            MailboxConfig {
                address: "*@test.com".to_string(),
                on_receive: vec![],
                trust: "none".to_string(),
                trusted_senders: vec![],
            },
        );
        mailboxes.insert(
            "alice".to_string(),
            MailboxConfig {
                address: "alice@test.com".to_string(),
                on_receive: vec![],
                trust: "none".to_string(),
                trusted_senders: vec![],
            },
        );
        Config {
            domain: "test.com".to_string(),
            data_dir: tmp.to_path_buf(),
            dkim_selector: "dkim".to_string(),
            mailboxes,
            verify_host: None,
        }
    }

    fn plain_text_eml() -> &'static [u8] {
        b"From: sender@example.com\r\n\
          To: alice@test.com\r\n\
          Subject: Hello\r\n\
          Date: Thu, 01 Jan 2025 12:00:00 +0000\r\n\
          Message-ID: <abc123@example.com>\r\n\
          \r\n\
          This is a plain text email.\r\n"
    }

    fn html_only_eml() -> &'static [u8] {
        b"From: sender@example.com\r\n\
          To: alice@test.com\r\n\
          Subject: HTML Email\r\n\
          Date: Thu, 01 Jan 2025 12:00:00 +0000\r\n\
          Message-ID: <html123@example.com>\r\n\
          Content-Type: text/html; charset=utf-8\r\n\
          \r\n\
          <html><body><h1>Hello</h1><p>World</p></body></html>\r\n"
    }

    fn multipart_eml() -> &'static [u8] {
        b"From: sender@example.com\r\n\
          To: alice@test.com\r\n\
          Subject: Multipart\r\n\
          Date: Thu, 01 Jan 2025 12:00:00 +0000\r\n\
          Message-ID: <multi123@example.com>\r\n\
          MIME-Version: 1.0\r\n\
          Content-Type: multipart/alternative; boundary=\"boundary42\"\r\n\
          \r\n\
          --boundary42\r\n\
          Content-Type: text/plain; charset=utf-8\r\n\
          \r\n\
          Plain text part.\r\n\
          --boundary42\r\n\
          Content-Type: text/html; charset=utf-8\r\n\
          \r\n\
          <html><body><p>HTML part.</p></body></html>\r\n\
          --boundary42--\r\n"
    }

    fn attachment_eml() -> &'static [u8] {
        b"From: sender@example.com\r\n\
          To: alice@test.com\r\n\
          Subject: With Attachment\r\n\
          Date: Thu, 01 Jan 2025 12:00:00 +0000\r\n\
          Message-ID: <att123@example.com>\r\n\
          MIME-Version: 1.0\r\n\
          Content-Type: multipart/mixed; boundary=\"mixbound\"\r\n\
          \r\n\
          --mixbound\r\n\
          Content-Type: text/plain; charset=utf-8\r\n\
          \r\n\
          Email with attachment.\r\n\
          --mixbound\r\n\
          Content-Type: text/plain; name=\"notes.txt\"\r\n\
          Content-Disposition: attachment; filename=\"notes.txt\"\r\n\
          \r\n\
          These are my notes.\r\n\
          --mixbound--\r\n"
    }

    fn multi_attachment_eml() -> &'static [u8] {
        b"From: sender@example.com\r\n\
          To: alice@test.com\r\n\
          Subject: Two Attachments\r\n\
          Date: Thu, 01 Jan 2025 12:00:00 +0000\r\n\
          Message-ID: <att456@example.com>\r\n\
          MIME-Version: 1.0\r\n\
          Content-Type: multipart/mixed; boundary=\"mixbound2\"\r\n\
          \r\n\
          --mixbound2\r\n\
          Content-Type: text/plain; charset=utf-8\r\n\
          \r\n\
          Two attachments.\r\n\
          --mixbound2\r\n\
          Content-Type: text/plain; name=\"file.txt\"\r\n\
          Content-Disposition: attachment; filename=\"file.txt\"\r\n\
          \r\n\
          File one content.\r\n\
          --mixbound2\r\n\
          Content-Type: application/octet-stream; name=\"data.bin\"\r\n\
          Content-Disposition: attachment; filename=\"data.bin\"\r\n\
          \r\n\
          binary data here\r\n\
          --mixbound2--\r\n"
    }

    #[test]
    fn ingest_plain_text() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(tmp.path());
        ingest_email(&config, "alice@test.com", plain_text_eml()).unwrap();

        let entries: Vec<_> = std::fs::read_dir(tmp.path().join("alice"))
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().is_some_and(|ext| ext == "md"))
            .collect();
        assert_eq!(entries.len(), 1);

        let content = std::fs::read_to_string(entries[0].path()).unwrap();

        let parts: Vec<&str> = content.splitn(3, "+++").collect();
        assert!(parts.len() >= 3);
        let toml_str = parts[1].trim();
        let parsed: toml::Value = toml::from_str(toml_str).unwrap();
        let table = parsed.as_table().unwrap();

        let from_val = table.get("from").unwrap().as_str().unwrap();
        assert_eq!(from_val, "sender@example.com");

        let subject_val = table.get("subject").unwrap().as_str().unwrap();
        assert_eq!(subject_val, "Hello");

        let read_val = table.get("read").unwrap();
        assert_eq!(read_val, &toml::Value::Boolean(false));

        assert!(content.contains("This is a plain text email."));
    }

    #[test]
    fn ingest_html_only() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(tmp.path());
        ingest_email(&config, "alice@test.com", html_only_eml()).unwrap();

        let entries: Vec<_> = std::fs::read_dir(tmp.path().join("alice"))
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().is_some_and(|ext| ext == "md"))
            .collect();
        let content = std::fs::read_to_string(entries[0].path()).unwrap();
        assert!(content.contains("Hello"));
        assert!(content.contains("World"));
        assert!(!content.contains("<html>"));
    }

    #[test]
    fn ingest_multipart_prefers_text() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(tmp.path());
        ingest_email(&config, "alice@test.com", multipart_eml()).unwrap();

        let entries: Vec<_> = std::fs::read_dir(tmp.path().join("alice"))
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().is_some_and(|ext| ext == "md"))
            .collect();
        let content = std::fs::read_to_string(entries[0].path()).unwrap();
        assert!(content.contains("Plain text part."));
        assert!(!content.contains("<html>"));
    }

    #[test]
    fn ingest_routes_unknown_to_catchall() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(tmp.path());
        ingest_email(&config, "unknown@test.com", plain_text_eml()).unwrap();

        assert!(tmp.path().join("catchall").exists());
        let entries: Vec<_> = std::fs::read_dir(tmp.path().join("catchall"))
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().is_some_and(|ext| ext == "md"))
            .collect();
        assert_eq!(entries.len(), 1);
    }

    #[test]
    fn frontmatter_valid_toml() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(tmp.path());
        ingest_email(&config, "alice@test.com", plain_text_eml()).unwrap();

        let entries: Vec<_> = std::fs::read_dir(tmp.path().join("alice"))
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().is_some_and(|ext| ext == "md"))
            .collect();
        let content = std::fs::read_to_string(entries[0].path()).unwrap();

        let parts: Vec<&str> = content.splitn(3, "+++").collect();
        assert!(parts.len() >= 3);
        let toml_str = parts[1].trim();
        let parsed: toml::Value = toml::from_str(toml_str).unwrap();
        let table = parsed.as_table().unwrap();

        assert!(table.contains_key("id"));
        assert!(table.contains_key("message_id"));
        assert!(table.contains_key("from"));
        assert!(table.contains_key("to"));
        assert!(table.contains_key("subject"));
        assert!(table.contains_key("date"));
        assert!(table.contains_key("in_reply_to"));
        assert!(table.contains_key("references"));
        assert!(table.contains_key("attachments"));
        assert!(table.contains_key("mailbox"));
        assert!(table.contains_key("read"));

        let read_val = table.get("read").unwrap();
        assert_eq!(read_val, &toml::Value::Boolean(false));
    }

    #[test]
    fn file_naming_increments() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(tmp.path());

        ingest_email(&config, "alice@test.com", plain_text_eml()).unwrap();
        ingest_email(&config, "alice@test.com", plain_text_eml()).unwrap();

        let entries: Vec<_> = std::fs::read_dir(tmp.path().join("alice"))
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().is_some_and(|ext| ext == "md"))
            .collect();
        assert_eq!(entries.len(), 2);
    }

    #[test]
    fn attachment_extracted() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(tmp.path());
        ingest_email(&config, "alice@test.com", attachment_eml()).unwrap();

        let att_path = tmp.path().join("alice/attachments/notes.txt");
        assert!(att_path.exists());
        let content = std::fs::read_to_string(&att_path).unwrap();
        assert!(content.contains("These are my notes."));

        let entries: Vec<_> = std::fs::read_dir(tmp.path().join("alice"))
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().is_some_and(|ext| ext == "md"))
            .collect();
        let md_content = std::fs::read_to_string(entries[0].path()).unwrap();
        let parts: Vec<&str> = md_content.splitn(3, "+++").collect();
        let toml_str = parts[1].trim();
        let parsed: toml::Value = toml::from_str(toml_str).unwrap();
        let table = parsed.as_table().unwrap();
        let attachments = table.get("attachments").unwrap().as_array().unwrap();
        assert_eq!(attachments.len(), 1);
        let att = attachments[0].as_table().unwrap();
        let filename = att.get("filename").unwrap().as_str().unwrap();
        assert_eq!(filename, "notes.txt");
        let path_val = att.get("path").unwrap().as_str().unwrap();
        assert_eq!(path_val, "attachments/notes.txt");
    }

    #[test]
    fn multiple_attachments() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(tmp.path());
        ingest_email(&config, "alice@test.com", multi_attachment_eml()).unwrap();

        let att_dir = tmp.path().join("alice/attachments");
        assert!(att_dir.join("file.txt").exists());
        assert!(att_dir.join("data.bin").exists());
    }

    #[test]
    fn duplicate_attachment_filenames() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(tmp.path());

        ingest_email(&config, "alice@test.com", attachment_eml()).unwrap();
        ingest_email(&config, "alice@test.com", attachment_eml()).unwrap();

        let att_dir = tmp.path().join("alice/attachments");
        assert!(att_dir.join("notes.txt").exists());
        assert!(att_dir.join("notes-1.txt").exists());
    }

    #[test]
    fn no_attachments() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(tmp.path());
        ingest_email(&config, "alice@test.com", plain_text_eml()).unwrap();

        let att_dir = tmp.path().join("alice/attachments");
        assert!(!att_dir.exists());

        let entries: Vec<_> = std::fs::read_dir(tmp.path().join("alice"))
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().is_some_and(|ext| ext == "md"))
            .collect();
        let md_content = std::fs::read_to_string(entries[0].path()).unwrap();
        let parts: Vec<&str> = md_content.splitn(3, "+++").collect();
        let toml_str = parts[1].trim();
        let parsed: toml::Value = toml::from_str(toml_str).unwrap();
        let table = parsed.as_table().unwrap();
        let attachments = table.get("attachments").unwrap().as_array().unwrap();
        assert!(attachments.is_empty());
    }

    #[test]
    fn extract_local_part_works() {
        assert_eq!(extract_local_part("alice@test.com"), "alice");
        assert_eq!(extract_local_part("bob"), "bob");
    }

    #[test]
    fn attachment_path_traversal_sanitized() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(tmp.path());

        let eml = b"From: sender@example.com\r\n\
            To: alice@test.com\r\n\
            Subject: Malicious\r\n\
            Date: Thu, 01 Jan 2025 12:00:00 +0000\r\n\
            Message-ID: <evil@example.com>\r\n\
            MIME-Version: 1.0\r\n\
            Content-Type: multipart/mixed; boundary=\"evilbound\"\r\n\
            \r\n\
            --evilbound\r\n\
            Content-Type: text/plain; charset=utf-8\r\n\
            \r\n\
            Body.\r\n\
            --evilbound\r\n\
            Content-Type: text/plain; name=\"../../../etc/cron.d/evil\"\r\n\
            Content-Disposition: attachment; filename=\"../../../etc/cron.d/evil\"\r\n\
            \r\n\
            malicious content\r\n\
            --evilbound--\r\n";

        ingest_email(&config, "alice@test.com", eml).unwrap();

        let att_dir = tmp.path().join("alice/attachments");
        assert!(att_dir.join("evil").exists());
        assert!(!tmp.path().join("etc").exists());
    }

    #[test]
    fn toml_handles_special_characters() {
        let meta = EmailMetadata {
            id: "2025-01-01-001".to_string(),
            message_id: "<test@example.com>".to_string(),
            from: "test\n+++\ninjected: true".to_string(),
            to: "to@test.com".to_string(),
            subject: "colons: and #hashes".to_string(),
            date: "2025-01-01T00:00:00Z".to_string(),
            in_reply_to: "".to_string(),
            references: "".to_string(),
            attachments: vec![],
            mailbox: "catchall".to_string(),
            read: false,
            dkim: "none".to_string(),
            spf: "none".to_string(),
        };

        let toml_str = toml::to_string_pretty(&meta).unwrap();
        let parsed: toml::Value = toml::from_str(&toml_str).unwrap();
        let table = parsed.as_table().unwrap();
        let from = table.get("from").unwrap();
        assert_eq!(from.as_str().unwrap(), "test\n+++\ninjected: true");
    }

    #[test]
    fn unsigned_email_has_dkim_none_spf_none() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(tmp.path());
        ingest_email(&config, "alice@test.com", plain_text_eml()).unwrap();

        let entries: Vec<_> = std::fs::read_dir(tmp.path().join("alice"))
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().is_some_and(|ext| ext == "md"))
            .collect();
        let content = std::fs::read_to_string(entries[0].path()).unwrap();
        let parts: Vec<&str> = content.splitn(3, "+++").collect();
        let toml_str = parts[1].trim();
        let parsed: toml::Value = toml::from_str(toml_str).unwrap();
        let table = parsed.as_table().unwrap();

        let dkim = table.get("dkim").unwrap().as_str().unwrap();
        assert_eq!(dkim, "none");

        let spf = table.get("spf").unwrap().as_str().unwrap();
        assert_eq!(spf, "none");
    }

    #[test]
    fn parse_ip_from_received_bracketed() {
        let line = "Received: from mail.example.com (mail.example.com [192.168.1.100])";
        let ip = parse_ip_from_received(line);
        assert_eq!(ip, Some("192.168.1.100".parse().unwrap()));
    }

    #[test]
    fn parse_ip_from_received_skips_loopback() {
        let line = "Received: from localhost ([127.0.0.1])";
        let ip = parse_ip_from_received(line);
        assert!(ip.is_none());
    }

    #[test]
    fn parse_ip_from_received_ipv6() {
        let line = "Received: from mail.example.com ([2001:db8::1])";
        let ip = parse_ip_from_received(line);
        assert_eq!(ip, Some("2001:db8::1".parse().unwrap()));
    }

    #[test]
    fn extract_mail_from_basic() {
        let raw = b"From: sender@example.com\r\nTo: test@test.com\r\n\r\nBody\r\n";
        let result = extract_mail_from(raw);
        assert_eq!(result, Some("sender@example.com".to_string()));
    }

    #[test]
    fn extract_mail_from_with_display_name() {
        let raw = b"From: Alice <alice@example.com>\r\nTo: test@test.com\r\n\r\nBody\r\n";
        let result = extract_mail_from(raw);
        assert_eq!(result, Some("alice@example.com".to_string()));
    }

    #[test]
    fn extract_mail_from_missing() {
        let raw = b"To: test@test.com\r\n\r\nBody\r\n";
        let result = extract_mail_from(raw);
        assert!(result.is_none());
    }

    #[test]
    fn frontmatter_includes_dkim_spf_fields() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(tmp.path());
        ingest_email(&config, "alice@test.com", plain_text_eml()).unwrap();

        let entries: Vec<_> = std::fs::read_dir(tmp.path().join("alice"))
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().is_some_and(|ext| ext == "md"))
            .collect();
        let content = std::fs::read_to_string(entries[0].path()).unwrap();
        let parts: Vec<&str> = content.splitn(3, "+++").collect();
        let toml_str = parts[1].trim();
        let parsed: toml::Value = toml::from_str(toml_str).unwrap();
        let table = parsed.as_table().unwrap();

        assert!(table.contains_key("dkim"));
        assert!(table.contains_key("spf"));
    }

    #[test]
    fn extract_received_ip_folded_header() {
        let raw = b"Received: from mail.example.com (mail.example.com\r\n\t[203.0.113.50]) by mx.local with ESMTP\r\n\r\nBody\r\n";
        let ip = extract_received_ip(raw);
        assert_eq!(ip, Some("203.0.113.50".parse().unwrap()));
    }

    #[test]
    fn extract_received_ip_folded_with_spaces() {
        let raw = b"Received: from mail.example.com (mail.example.com\r\n    [198.51.100.25]) by mx.local\r\n\r\nBody\r\n";
        let ip = extract_received_ip(raw);
        assert_eq!(ip, Some("198.51.100.25".parse().unwrap()));
    }

    #[test]
    fn unfold_headers_preserves_single_line() {
        let input = "Received: from mail.example.com ([192.168.1.1])";
        let result = unfold_headers(input);
        assert_eq!(result, input);
    }

    #[test]
    fn unfold_headers_joins_continuation() {
        let input = "Received: from mail.example.com\n\t([192.168.1.1])";
        let result = unfold_headers(input);
        assert!(result.contains("Received: from mail.example.com ([192.168.1.1])"));
    }

    #[test]
    fn gmail_dkim_fixture_parses_correctly() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(tmp.path());
        let raw = include_bytes!("../tests/fixtures/gmail_dkim_signed.eml");

        ingest_email(&config, "agent@test.com", raw).unwrap();

        let catchall_dir = tmp.path().join("catchall");
        let entries: Vec<_> = std::fs::read_dir(&catchall_dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().is_some_and(|ext| ext == "md"))
            .collect();
        assert_eq!(entries.len(), 1);

        let content = std::fs::read_to_string(entries[0].path()).unwrap();
        assert!(content.contains("subject = \"Test DKIM signed email from Gmail\""));
        assert!(content.contains("from = \"Test User <testuser@gmail.com>\""));
        assert!(content.contains("CAB1234567890abcdef@mail.gmail.com"));
    }

    #[test]
    fn gmail_dkim_fixture_has_dkim_headers() {
        let raw = include_bytes!("../tests/fixtures/gmail_dkim_signed.eml");
        let auth_msg = mail_auth::AuthenticatedMessage::parse(raw);
        assert!(auth_msg.is_some());
        let auth_msg = auth_msg.unwrap();
        assert!(
            !auth_msg.dkim_headers.is_empty(),
            "Gmail fixture should have DKIM headers"
        );
    }

    #[test]
    fn gmail_dkim_fixture_extracts_received_ip() {
        let raw = include_bytes!("../tests/fixtures/gmail_dkim_signed.eml");
        let ip = extract_received_ip(raw);
        assert!(ip.is_some());
        assert_eq!(ip.unwrap().to_string(), "209.85.128.182");
    }

    #[test]
    fn spf_fallback_uses_from_domain_first() {
        let eml = b"From: sender@sender-domain.com\r\n\
            Received: from mx.sender-domain.com ([203.0.113.5])\r\n\
            To: agent@recipient.com\r\n\
            Subject: SPF test\r\n\
            \r\n\
            body\r\n";

        let mail_from = extract_mail_from(eml).unwrap_or_default();
        let from_domain = mail_from.split('@').nth(1).unwrap_or("");
        assert_eq!(from_domain, "sender-domain.com");
    }

    #[test]
    fn spf_fallback_uses_rcpt_domain_when_no_from() {
        let eml = b"Received: from mx.unknown.com ([203.0.113.5])\r\n\
            To: agent@recipient.com\r\n\
            Subject: SPF test\r\n\
            \r\n\
            body\r\n";

        let mail_from = extract_mail_from(eml).unwrap_or_default();
        let from_domain = mail_from.split('@').nth(1).unwrap_or("");
        let rcpt = "agent@recipient.com";

        let helo_domain = if !from_domain.is_empty() {
            from_domain
        } else {
            rcpt.split('@').nth(1).unwrap_or("")
        };
        assert_eq!(helo_domain, "recipient.com");
    }

    #[test]
    fn atomic_file_creation_retries_on_collision() {
        let tmp = TempDir::new().unwrap();
        let mailbox_dir = tmp.path().join("alice");
        std::fs::create_dir_all(&mailbox_dir).unwrap();

        // Pre-create a file with today's date and counter 001
        let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
        let existing = mailbox_dir.join(format!("{today}-001.md"));
        std::fs::write(&existing, "pre-existing file").unwrap();

        let meta = EmailMetadata {
            id: String::new(),
            message_id: "<test@test.com>".to_string(),
            from: "sender@test.com".to_string(),
            to: "alice@test.com".to_string(),
            subject: "Collision test".to_string(),
            date: "2025-01-01T00:00:00Z".to_string(),
            in_reply_to: "".to_string(),
            references: "".to_string(),
            attachments: vec![],
            mailbox: "alice".to_string(),
            read: false,
            dkim: "none".to_string(),
            spf: "none".to_string(),
        };

        let (id, _path, _meta) = create_file_atomic(&mailbox_dir, &meta, "body").unwrap();
        assert_eq!(id, format!("{today}-002"));

        // Pre-existing file should not be overwritten
        let content = std::fs::read_to_string(&existing).unwrap();
        assert_eq!(content, "pre-existing file");
    }

    #[test]
    fn atomic_file_creation_two_rapid_calls_produce_different_ids() {
        let tmp = TempDir::new().unwrap();
        let mailbox_dir = tmp.path().join("alice");
        std::fs::create_dir_all(&mailbox_dir).unwrap();

        let meta = EmailMetadata {
            id: String::new(),
            message_id: "<test@test.com>".to_string(),
            from: "sender@test.com".to_string(),
            to: "alice@test.com".to_string(),
            subject: "Rapid test".to_string(),
            date: "2025-01-01T00:00:00Z".to_string(),
            in_reply_to: "".to_string(),
            references: "".to_string(),
            attachments: vec![],
            mailbox: "alice".to_string(),
            read: false,
            dkim: "none".to_string(),
            spf: "none".to_string(),
        };

        let (id1, _, _) = create_file_atomic(&mailbox_dir, &meta, "body1").unwrap();
        let (id2, _, _) = create_file_atomic(&mailbox_dir, &meta, "body2").unwrap();
        assert_ne!(id1, id2);
    }

    #[test]
    fn spf_domain_with_valid_sender() {
        assert_eq!(spf_domain("user@example.com"), Some("example.com"));
    }

    #[test]
    fn spf_domain_empty_sender_returns_none() {
        assert_eq!(spf_domain(""), None);
    }

    #[test]
    fn spf_domain_no_at_returns_none() {
        assert_eq!(spf_domain("nodomain"), None);
    }

    #[test]
    fn spf_domain_empty_domain_part_returns_none() {
        assert_eq!(spf_domain("user@"), None);
    }
}
