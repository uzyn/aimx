use crate::config::Config;
use mail_parser::{MessageParser, MimeHeaders};
use std::io::Read;
use std::path::Path;

#[derive(Debug, Clone)]
pub struct EmailMetadata {
    pub id: String,
    pub message_id: String,
    pub from: String,
    pub to: String,
    pub subject: String,
    pub date: String,
    pub in_reply_to: String,
    pub references: String,
    pub mailbox: String,
    pub attachments: Vec<AttachmentMeta>,
}

#[derive(Debug, Clone)]
pub struct AttachmentMeta {
    pub filename: String,
    pub content_type: String,
    pub size: usize,
    pub path: String,
}

pub fn run(rcpt: &str) -> Result<(), Box<dyn std::error::Error>> {
    let config = Config::load_default()?;
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

    let meta = EmailMetadata {
        id: id.clone(),
        message_id,
        from,
        to,
        subject,
        date,
        in_reply_to,
        references,
        mailbox: mailbox.clone(),
        attachments,
    };

    let content = format_markdown(&meta, &body);
    std::fs::write(&filepath, content)?;

    Ok(())
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

fn yaml_escape(s: &str) -> String {
    if s.contains(':')
        || s.contains('#')
        || s.contains('\'')
        || s.contains('"')
        || s.contains('\n')
        || s.contains('[')
        || s.contains(']')
        || s.contains('{')
        || s.contains('}')
        || s.contains('&')
        || s.contains('*')
        || s.contains('!')
        || s.contains('|')
        || s.contains('>')
        || s.contains('%')
        || s.contains('@')
        || s.starts_with(' ')
        || s.ends_with(' ')
    {
        format!(
            "\"{}\"",
            s.replace('\\', "\\\\")
                .replace('"', "\\\"")
                .replace('\n', "\\n")
                .replace('\r', "\\r")
        )
    } else {
        s.to_string()
    }
}

fn format_markdown(meta: &EmailMetadata, body: &str) -> String {
    let mut front = String::new();
    front.push_str("---\n");
    front.push_str(&format!("id: {}\n", yaml_escape(&meta.id)));
    front.push_str(&format!("message_id: {}\n", yaml_escape(&meta.message_id)));
    front.push_str(&format!("from: {}\n", yaml_escape(&meta.from)));
    front.push_str(&format!("to: {}\n", yaml_escape(&meta.to)));
    front.push_str(&format!("subject: {}\n", yaml_escape(&meta.subject)));
    front.push_str(&format!("date: {}\n", yaml_escape(&meta.date)));
    front.push_str(&format!(
        "in_reply_to: {}\n",
        yaml_escape(&meta.in_reply_to)
    ));
    front.push_str(&format!("references: {}\n", yaml_escape(&meta.references)));

    if meta.attachments.is_empty() {
        front.push_str("attachments: []\n");
    } else {
        front.push_str("attachments:\n");
        for att in &meta.attachments {
            front.push_str(&format!("  - filename: {}\n", yaml_escape(&att.filename)));
            front.push_str(&format!(
                "    content_type: {}\n",
                yaml_escape(&att.content_type)
            ));
            front.push_str(&format!("    size: {}\n", att.size));
            front.push_str(&format!("    path: {}\n", yaml_escape(&att.path)));
        }
    }

    front.push_str(&format!("mailbox: {}\n", yaml_escape(&meta.mailbox)));
    front.push_str("read: false\n");
    front.push_str("---\n\n");
    front.push_str(body);

    if !body.ends_with('\n') {
        front.push('\n');
    }

    front
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
            },
        );
        mailboxes.insert(
            "alice".to_string(),
            MailboxConfig {
                address: "alice@test.com".to_string(),
                on_receive: vec![],
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
        assert!(content.contains("from: \"sender@example.com\""));
        assert!(content.contains("subject: Hello"));
        assert!(content.contains("read: false"));
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
        assert!(md_content.contains("filename: notes.txt"));
        assert!(md_content.contains("path: attachments/notes.txt"));
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
        assert!(md_content.contains("attachments: []"));
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
    fn yaml_escape_newline_injection() {
        let escaped = yaml_escape("test\n---\ninjected: true");
        assert!(!escaped.contains('\n'));
        assert!(escaped.contains("\\n"));

        let yaml_str = format!("subject: {}\n", escaped);
        let parsed: serde_yaml::Value = serde_yaml::from_str(&yaml_str).unwrap();
        let val = parsed.as_mapping().unwrap();
        let subject = val
            .get(&serde_yaml::Value::String("subject".to_string()))
            .unwrap();
        assert_eq!(subject.as_str().unwrap(), "test\n---\ninjected: true");
    }
}
