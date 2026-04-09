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

    let id = generate_file_id(&mailbox_dir);
    let filename = format!("{id}.md");
    let filepath = mailbox_dir.join(&filename);

    let (dkim_result, spf_result) = verify_auth(raw, rcpt);

    let meta = EmailMetadata {
        id: id.clone(),
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

    let content = format_markdown(&meta, &body);
    std::fs::write(&filepath, content)?;

    if let Some(mailbox_config) = config.mailboxes.get(&mailbox) {
        let ctx = TriggerContext {
            filepath: &filepath,
            metadata: &meta,
        };
        channel::execute_triggers(mailbox_config, &ctx);
    }

    Ok(())
}

fn verify_auth(raw: &[u8], rcpt: &str) -> (String, String) {
    let rt = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(_) => return ("none".to_string(), "none".to_string()),
    };

    rt.block_on(async {
        let dkim_result = verify_dkim_async(raw).await;
        let spf_result = verify_spf_async(raw, rcpt).await;
        (dkim_result, spf_result)
    })
}

async fn verify_dkim_async(raw: &[u8]) -> String {
    let auth_msg = match mail_auth::AuthenticatedMessage::parse(raw) {
        Some(msg) => msg,
        None => return "none".to_string(),
    };

    if auth_msg.dkim_headers.is_empty() {
        return "none".to_string();
    }

    let resolver = match mail_auth::Resolver::new_system_conf() {
        Ok(r) => r,
        Err(_) => match mail_auth::Resolver::new_cloudflare() {
            Ok(r) => r,
            Err(_) => return "none".to_string(),
        },
    };

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

async fn verify_spf_async(raw: &[u8], rcpt: &str) -> String {
    let ip = match extract_received_ip(raw) {
        Some(ip) => ip,
        None => return "none".to_string(),
    };

    let sender_domain = rcpt.split('@').nth(1).unwrap_or("");
    let mail_from = extract_mail_from(raw).unwrap_or_default();
    let helo_domain = mail_from.split('@').nth(1).unwrap_or(sender_domain);

    if helo_domain.is_empty() {
        return "none".to_string();
    }

    let resolver = match mail_auth::Resolver::new_system_conf() {
        Ok(r) => r,
        Err(_) => match mail_auth::Resolver::new_cloudflare() {
            Ok(r) => r,
            Err(_) => return "none".to_string(),
        },
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

fn extract_received_ip(raw: &[u8]) -> Option<IpAddr> {
    let header_section = std::str::from_utf8(raw).ok()?;
    for line in header_section.lines() {
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

fn generate_file_id(mailbox_dir: &Path) -> String {
    let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
    let mut counter = 1u32;

    loop {
        let candidate = format!("{today}-{counter:03}");
        let path = mailbox_dir.join(format!("{candidate}.md"));
        if !path.exists() {
            return candidate;
        }
        counter += 1;
    }
}

fn format_markdown(meta: &EmailMetadata, body: &str) -> String {
    let yaml = serde_yaml::to_string(meta).unwrap_or_default();
    let mut result = String::new();
    result.push_str("---\n");
    result.push_str(&yaml);
    result.push_str("---\n\n");
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

        let parts: Vec<&str> = content.splitn(3, "---").collect();
        assert!(parts.len() >= 3);
        let yaml_str = parts[1].trim();
        let parsed: serde_yaml::Value = serde_yaml::from_str(yaml_str).unwrap();
        let map = parsed.as_mapping().unwrap();

        let from_val = map
            .get(&serde_yaml::Value::String("from".to_string()))
            .unwrap()
            .as_str()
            .unwrap();
        assert_eq!(from_val, "sender@example.com");

        let subject_val = map
            .get(&serde_yaml::Value::String("subject".to_string()))
            .unwrap()
            .as_str()
            .unwrap();
        assert_eq!(subject_val, "Hello");

        let read_val = map
            .get(&serde_yaml::Value::String("read".to_string()))
            .unwrap();
        assert_eq!(read_val, &serde_yaml::Value::Bool(false));

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
    fn frontmatter_valid_yaml() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(tmp.path());
        ingest_email(&config, "alice@test.com", plain_text_eml()).unwrap();

        let entries: Vec<_> = std::fs::read_dir(tmp.path().join("alice"))
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().is_some_and(|ext| ext == "md"))
            .collect();
        let content = std::fs::read_to_string(entries[0].path()).unwrap();

        let parts: Vec<&str> = content.splitn(3, "---").collect();
        assert!(parts.len() >= 3);
        let yaml_str = parts[1].trim();
        let yaml: serde_yaml::Value = serde_yaml::from_str(yaml_str).unwrap();
        let map = yaml.as_mapping().unwrap();

        assert!(map.contains_key(&serde_yaml::Value::String("id".to_string())));
        assert!(map.contains_key(&serde_yaml::Value::String("message_id".to_string())));
        assert!(map.contains_key(&serde_yaml::Value::String("from".to_string())));
        assert!(map.contains_key(&serde_yaml::Value::String("to".to_string())));
        assert!(map.contains_key(&serde_yaml::Value::String("subject".to_string())));
        assert!(map.contains_key(&serde_yaml::Value::String("date".to_string())));
        assert!(map.contains_key(&serde_yaml::Value::String("in_reply_to".to_string())));
        assert!(map.contains_key(&serde_yaml::Value::String("references".to_string())));
        assert!(map.contains_key(&serde_yaml::Value::String("attachments".to_string())));
        assert!(map.contains_key(&serde_yaml::Value::String("mailbox".to_string())));
        assert!(map.contains_key(&serde_yaml::Value::String("read".to_string())));

        let read_val = map
            .get(&serde_yaml::Value::String("read".to_string()))
            .unwrap();
        assert_eq!(read_val, &serde_yaml::Value::Bool(false));
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
        let parts: Vec<&str> = md_content.splitn(3, "---").collect();
        let yaml_str = parts[1].trim();
        let parsed: serde_yaml::Value = serde_yaml::from_str(yaml_str).unwrap();
        let map = parsed.as_mapping().unwrap();
        let attachments = map
            .get(&serde_yaml::Value::String("attachments".to_string()))
            .unwrap()
            .as_sequence()
            .unwrap();
        assert_eq!(attachments.len(), 1);
        let att = attachments[0].as_mapping().unwrap();
        let filename = att
            .get(&serde_yaml::Value::String("filename".to_string()))
            .unwrap()
            .as_str()
            .unwrap();
        assert_eq!(filename, "notes.txt");
        let path_val = att
            .get(&serde_yaml::Value::String("path".to_string()))
            .unwrap()
            .as_str()
            .unwrap();
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
        let parts: Vec<&str> = md_content.splitn(3, "---").collect();
        let yaml_str = parts[1].trim();
        let parsed: serde_yaml::Value = serde_yaml::from_str(yaml_str).unwrap();
        let map = parsed.as_mapping().unwrap();
        let attachments = map
            .get(&serde_yaml::Value::String("attachments".to_string()))
            .unwrap()
            .as_sequence()
            .unwrap();
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
    fn serde_yaml_handles_special_characters() {
        let meta = EmailMetadata {
            id: "2025-01-01-001".to_string(),
            message_id: "<test@example.com>".to_string(),
            from: "test\n---\ninjected: true".to_string(),
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

        let yaml = serde_yaml::to_string(&meta).unwrap();
        let parsed: serde_yaml::Value = serde_yaml::from_str(&yaml).unwrap();
        let map = parsed.as_mapping().unwrap();
        let from = map
            .get(&serde_yaml::Value::String("from".to_string()))
            .unwrap();
        assert_eq!(from.as_str().unwrap(), "test\n---\ninjected: true");
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
        let parts: Vec<&str> = content.splitn(3, "---").collect();
        let yaml_str = parts[1].trim();
        let parsed: serde_yaml::Value = serde_yaml::from_str(yaml_str).unwrap();
        let map = parsed.as_mapping().unwrap();

        let dkim = map
            .get(&serde_yaml::Value::String("dkim".to_string()))
            .unwrap()
            .as_str()
            .unwrap();
        assert_eq!(dkim, "none");

        let spf = map
            .get(&serde_yaml::Value::String("spf".to_string()))
            .unwrap()
            .as_str()
            .unwrap();
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
        let parts: Vec<&str> = content.splitn(3, "---").collect();
        let yaml_str = parts[1].trim();
        let parsed: serde_yaml::Value = serde_yaml::from_str(yaml_str).unwrap();
        let map = parsed.as_mapping().unwrap();

        assert!(map.contains_key(&serde_yaml::Value::String("dkim".to_string())));
        assert!(map.contains_key(&serde_yaml::Value::String("spf".to_string())));
    }
}
