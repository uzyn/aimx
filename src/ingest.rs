use crate::channel::{self, TriggerContext};
use crate::config::Config;
use crate::slug::{allocate_filename, slugify};
use mail_parser::{MessageParser, MimeHeaders};
use serde::Serialize;
use std::io::Read;
use std::net::IpAddr;
use std::path::{Path, PathBuf};

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

pub fn run(rcpt: &str, config: Config) -> Result<(), Box<dyn std::error::Error>> {
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
    let inbox_dir = config.inbox_dir(&mailbox);

    std::fs::create_dir_all(&inbox_dir)?;

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

    // Collect attachments as in-memory metadata + payload so we can decide
    // between flat and bundle layouts before committing any file to disk.
    let prepared_attachments = prepare_attachments(&message);
    let has_attachments = !prepared_attachments.is_empty();

    let (dkim_result, spf_result) = verify_auth(raw, rcpt);

    let slug = slugify(&subject);
    let timestamp = chrono::Utc::now();
    let md_path = allocate_filename(&inbox_dir, timestamp, &slug, has_attachments);
    let parent_dir = md_path
        .parent()
        .ok_or("allocate_filename returned a rootless path")?
        .to_path_buf();

    // For bundle layouts the parent is `<stem>/`; for flat layouts it is
    // the mailbox directory itself (already created above). Either way,
    // `create_dir_all` is idempotent and cheap.
    std::fs::create_dir_all(&parent_dir)?;

    // Write attachments first; if one fails we want to bubble the error
    // before writing the `.md` so callers never see a half-written bundle.
    let attachments = write_attachments(&parent_dir, prepared_attachments).inspect_err(|_| {
        // Best-effort cleanup of a freshly created bundle directory.
        if has_attachments && parent_dir != inbox_dir {
            let _ = std::fs::remove_dir_all(&parent_dir);
        }
    })?;

    let id = md_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or_default()
        .to_string();

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

    write_markdown(&md_path, &meta, &body)?;

    if let Some(mailbox_config) = config.mailboxes.get(&mailbox) {
        let ctx = TriggerContext {
            filepath: &md_path,
            metadata: &meta,
        };
        channel::execute_triggers(mailbox_config, &ctx);
    }

    Ok(())
}

fn create_resolver() -> Option<mail_auth::MessageAuthenticator> {
    mail_auth::MessageAuthenticator::new_system_conf()
        .or_else(|_| mail_auth::MessageAuthenticator::new_cloudflare())
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

async fn verify_dkim_async(raw: &[u8], resolver: &mail_auth::MessageAuthenticator) -> String {
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

async fn verify_spf_async(
    raw: &[u8],
    _rcpt: &str,
    resolver: &mail_auth::MessageAuthenticator,
) -> String {
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
        .verify_spf(mail_auth::spf::verify::SpfParameters::verify_mail_from(
            ip,
            helo_domain,
            helo_domain,
            &mail_from,
        ))
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

/// An attachment extracted from the parsed MIME message but not yet written
/// to disk. Lives long enough to decide between flat and bundle layouts
/// before any file I/O happens.
struct PreparedAttachment {
    filename: String,
    content_type: String,
    body: Vec<u8>,
}

fn prepare_attachments(message: &mail_parser::Message) -> Vec<PreparedAttachment> {
    let mut result = Vec::new();

    for attachment in message.attachments() {
        let raw_name = match attachment.attachment_name() {
            Some(name) => name.to_string(),
            None => continue,
        };

        // Strip any path components the sender may have smuggled in;
        // `file_name` on a traversal string like `../../etc/foo` returns
        // just `foo`.
        let filename = Path::new(&raw_name)
            .file_name()
            .and_then(|f| f.to_str())
            .unwrap_or("")
            .to_string();

        if filename.is_empty() {
            continue;
        }

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

        result.push(PreparedAttachment {
            filename,
            content_type,
            body: attachment.contents().to_vec(),
        });
    }

    result
}

/// Write each attachment as a sibling of the `.md` file inside the bundle
/// directory. Duplicate filenames are disambiguated with `-1`, `-2`, …
/// before the extension. The returned `AttachmentMeta` entries carry the
/// on-disk filename (without any bundle prefix — each attachment is a
/// sibling of the `.md`, so the relative path is just the filename).
fn write_attachments(
    bundle_dir: &Path,
    attachments: Vec<PreparedAttachment>,
) -> Result<Vec<AttachmentMeta>, Box<dyn std::error::Error>> {
    let mut result = Vec::with_capacity(attachments.len());

    for att in attachments {
        let dest_filename = deduplicate_filename(bundle_dir, &att.filename);
        let dest_path = bundle_dir.join(&dest_filename);
        std::fs::write(&dest_path, &att.body)?;

        result.push(AttachmentMeta {
            filename: dest_filename.clone(),
            content_type: att.content_type,
            size: att.body.len(),
            path: dest_filename,
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

fn write_markdown(
    path: &Path,
    meta: &EmailMetadata,
    body: &str,
) -> Result<PathBuf, Box<dyn std::error::Error>> {
    use std::fs::OpenOptions;
    use std::io::Write;

    // `allocate_filename` already reserved this path by scanning the
    // directory, but we still open with `create_new` so two in-flight
    // ingests racing on the same subject at the same UTC second collide
    // noisily instead of silently overwriting.
    let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
    let content = format_markdown(meta, body);
    file.write_all(content.as_bytes())?;
    Ok(path.to_path_buf())
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
            enable_ipv6: false,
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

    /// Walk the inbox mailbox dir and return every `.md` file, descending
    /// into bundle directories when they exist. Ordering is by filename.
    fn collect_md_files(mailbox_dir: &Path) -> Vec<std::path::PathBuf> {
        let mut result: Vec<std::path::PathBuf> = Vec::new();
        let entries = match std::fs::read_dir(mailbox_dir) {
            Ok(e) => e,
            Err(_) => return result,
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                // Bundle directory: expect exactly one sibling `.md` with
                // the same stem as the directory.
                if let Some(stem) = path.file_name().and_then(|f| f.to_str()) {
                    let md = path.join(format!("{stem}.md"));
                    if md.exists() {
                        result.push(md);
                    }
                }
            } else if path.extension().is_some_and(|ext| ext == "md") {
                result.push(path);
            }
        }
        result.sort();
        result
    }

    fn inbox(tmp: &Path, name: &str) -> std::path::PathBuf {
        tmp.join("inbox").join(name)
    }

    #[test]
    fn ingest_plain_text() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(tmp.path());
        ingest_email(&config, "alice@test.com", plain_text_eml()).unwrap();

        let entries = collect_md_files(&inbox(tmp.path(), "alice"));
        assert_eq!(entries.len(), 1);

        let content = std::fs::read_to_string(&entries[0]).unwrap();

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
    fn ingest_writes_to_inbox_subdir() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(tmp.path());
        ingest_email(&config, "alice@test.com", plain_text_eml()).unwrap();

        // New Sprint 36 layout: inbox/<mailbox>/ instead of <mailbox>/.
        assert!(tmp.path().join("inbox").join("alice").exists());
        assert!(!tmp.path().join("alice").exists());
    }

    #[test]
    fn ingest_filename_uses_utc_slug_format() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(tmp.path());
        ingest_email(&config, "alice@test.com", plain_text_eml()).unwrap();

        let entries = collect_md_files(&inbox(tmp.path(), "alice"));
        let name = entries[0]
            .file_name()
            .unwrap()
            .to_string_lossy()
            .to_string();
        // "Hello" subject → "hello" slug.
        assert!(
            name.ends_with("-hello.md"),
            "expected slug-suffixed filename, got {name}"
        );
        // Matches YYYY-MM-DD-HHMMSS-<slug>.md shape.
        let stem = name.trim_end_matches(".md");
        let parts: Vec<&str> = stem.splitn(5, '-').collect();
        assert_eq!(parts.len(), 5, "expected 5 dash-segments in {stem}");
        assert_eq!(parts[0].len(), 4);
        assert_eq!(parts[1].len(), 2);
        assert_eq!(parts[2].len(), 2);
        assert_eq!(parts[3].len(), 6);
    }

    #[test]
    fn ingest_html_only() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(tmp.path());
        ingest_email(&config, "alice@test.com", html_only_eml()).unwrap();

        let entries = collect_md_files(&inbox(tmp.path(), "alice"));
        let content = std::fs::read_to_string(&entries[0]).unwrap();
        assert!(content.contains("Hello"));
        assert!(content.contains("World"));
        assert!(!content.contains("<html>"));
    }

    #[test]
    fn ingest_multipart_prefers_text() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(tmp.path());
        ingest_email(&config, "alice@test.com", multipart_eml()).unwrap();

        let entries = collect_md_files(&inbox(tmp.path(), "alice"));
        let content = std::fs::read_to_string(&entries[0]).unwrap();
        assert!(content.contains("Plain text part."));
        assert!(!content.contains("<html>"));
    }

    #[test]
    fn ingest_routes_unknown_to_catchall() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(tmp.path());
        ingest_email(&config, "unknown@test.com", plain_text_eml()).unwrap();

        assert!(inbox(tmp.path(), "catchall").exists());
        let entries = collect_md_files(&inbox(tmp.path(), "catchall"));
        assert_eq!(entries.len(), 1);
    }

    #[test]
    fn frontmatter_valid_toml() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(tmp.path());
        ingest_email(&config, "alice@test.com", plain_text_eml()).unwrap();

        let entries = collect_md_files(&inbox(tmp.path(), "alice"));
        let content = std::fs::read_to_string(&entries[0]).unwrap();

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

        // Two ingests of the same subject within the same UTC second: the
        // second must land on a distinct path thanks to the `-2` suffix.
        let entries = collect_md_files(&inbox(tmp.path(), "alice"));
        assert_eq!(entries.len(), 2, "got entries: {entries:?}");
        assert_ne!(entries[0], entries[1]);
    }

    #[test]
    fn attachment_creates_bundle_directory() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(tmp.path());
        ingest_email(&config, "alice@test.com", attachment_eml()).unwrap();

        let alice = inbox(tmp.path(), "alice");
        // Top-level `attachments/` is GONE (Sprint 36).
        assert!(!alice.join("attachments").exists());

        // Exactly one bundle directory should have been created.
        let bundles: Vec<_> = std::fs::read_dir(&alice)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().is_dir())
            .collect();
        assert_eq!(bundles.len(), 1);
        let bundle = bundles[0].path();
        let stem = bundle.file_name().unwrap().to_string_lossy().to_string();

        // The bundle contains <stem>.md AND the attachment as a sibling.
        let md = bundle.join(format!("{stem}.md"));
        assert!(md.exists(), "bundle md {md:?} should exist");
        let att_path = bundle.join("notes.txt");
        assert!(att_path.exists(), "attachment at {att_path:?} should exist");

        let content = std::fs::read_to_string(&att_path).unwrap();
        assert!(content.contains("These are my notes."));

        let md_content = std::fs::read_to_string(&md).unwrap();
        let parts: Vec<&str> = md_content.splitn(3, "+++").collect();
        let toml_str = parts[1].trim();
        let parsed: toml::Value = toml::from_str(toml_str).unwrap();
        let table = parsed.as_table().unwrap();
        let attachments = table.get("attachments").unwrap().as_array().unwrap();
        assert_eq!(attachments.len(), 1);
        let att = attachments[0].as_table().unwrap();
        assert_eq!(att.get("filename").unwrap().as_str().unwrap(), "notes.txt");
        // Bundle-relative path has no `attachments/` prefix any more.
        assert_eq!(att.get("path").unwrap().as_str().unwrap(), "notes.txt");
    }

    #[test]
    fn multiple_attachments_in_single_bundle() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(tmp.path());
        ingest_email(&config, "alice@test.com", multi_attachment_eml()).unwrap();

        let alice = inbox(tmp.path(), "alice");
        let bundles: Vec<_> = std::fs::read_dir(&alice)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().is_dir())
            .collect();
        assert_eq!(bundles.len(), 1);
        let bundle = bundles[0].path();
        assert!(bundle.join("file.txt").exists());
        assert!(bundle.join("data.bin").exists());
    }

    #[test]
    fn duplicate_attachment_filenames_are_bundle_scoped() {
        // Two emails with the same filename land in two distinct bundle
        // directories, so the attachment names don't collide — no `-1`
        // suffix needed inside either bundle.
        let tmp = TempDir::new().unwrap();
        let config = test_config(tmp.path());

        ingest_email(&config, "alice@test.com", attachment_eml()).unwrap();
        ingest_email(&config, "alice@test.com", attachment_eml()).unwrap();

        let alice = inbox(tmp.path(), "alice");
        let bundles: Vec<_> = std::fs::read_dir(&alice)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().is_dir())
            .collect();
        assert_eq!(bundles.len(), 2, "expected two separate bundles");

        for b in bundles {
            assert!(b.path().join("notes.txt").exists());
        }
    }

    #[test]
    fn no_attachments_flat_markdown() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(tmp.path());
        ingest_email(&config, "alice@test.com", plain_text_eml()).unwrap();

        let alice = inbox(tmp.path(), "alice");
        // No bundle directory, no legacy attachments dir.
        let dirs: Vec<_> = std::fs::read_dir(&alice)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().is_dir())
            .collect();
        assert!(dirs.is_empty(), "flat layout must not create directories");

        let entries = collect_md_files(&alice);
        let md_content = std::fs::read_to_string(&entries[0]).unwrap();
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

        // Path traversal in the attachment filename collapses to `evil`
        // inside the bundle; nothing escapes the mailbox dir.
        let alice = inbox(tmp.path(), "alice");
        let bundles: Vec<_> = std::fs::read_dir(&alice)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().is_dir())
            .collect();
        assert_eq!(bundles.len(), 1);
        let bundle = bundles[0].path();
        assert!(bundle.join("evil").exists());
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

        let entries = collect_md_files(&inbox(tmp.path(), "alice"));
        let content = std::fs::read_to_string(&entries[0]).unwrap();
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

        let entries = collect_md_files(&inbox(tmp.path(), "alice"));
        let content = std::fs::read_to_string(&entries[0]).unwrap();
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

        let entries = collect_md_files(&inbox(tmp.path(), "catchall"));
        assert_eq!(entries.len(), 1);

        let content = std::fs::read_to_string(&entries[0]).unwrap();
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
    fn rapid_same_subject_ingests_get_unique_paths() {
        // Two ingests with identical subjects landing in the same UTC
        // second must not overwrite each other; `allocate_filename` picks
        // the `-2` suffix for the second one.
        let tmp = TempDir::new().unwrap();
        let config = test_config(tmp.path());
        ingest_email(&config, "alice@test.com", plain_text_eml()).unwrap();
        ingest_email(&config, "alice@test.com", plain_text_eml()).unwrap();

        let entries = collect_md_files(&inbox(tmp.path(), "alice"));
        assert_eq!(entries.len(), 2);
        assert_ne!(entries[0], entries[1]);
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
