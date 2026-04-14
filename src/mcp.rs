use crate::cli::SendArgs;
use crate::config::Config;
use crate::dkim;
use crate::ingest::EmailMetadata;
use crate::mailbox;
use crate::send;

use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{ServerCapabilities, ServerInfo};
use rmcp::{ServerHandler, ServiceExt, schemars, tool, tool_handler, tool_router};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct AimxMcpServer {
    data_dir: PathBuf,
    #[allow(dead_code)]
    tool_router: ToolRouter<Self>,
}

#[derive(Serialize, Deserialize, schemars::JsonSchema)]
pub struct MailboxCreateParams {
    #[schemars(description = "Name of the mailbox to create (local part of email address)")]
    pub name: String,
}

#[derive(Serialize, Deserialize, schemars::JsonSchema)]
pub struct MailboxDeleteParams {
    #[schemars(description = "Name of the mailbox to delete")]
    pub name: String,
}

#[derive(Serialize, Deserialize, schemars::JsonSchema)]
pub struct EmailListParams {
    #[schemars(description = "Mailbox name to list emails from")]
    pub mailbox: String,
    #[schemars(description = "Filter to only unread emails")]
    pub unread: Option<bool>,
    #[schemars(description = "Filter by sender address (substring match)")]
    pub from: Option<String>,
    #[schemars(description = "Filter to emails since this datetime (RFC 3339 format)")]
    pub since: Option<String>,
    #[schemars(description = "Filter by subject (substring match, case-insensitive)")]
    pub subject: Option<String>,
}

#[derive(Serialize, Deserialize, schemars::JsonSchema)]
pub struct EmailReadParams {
    #[schemars(description = "Mailbox name")]
    pub mailbox: String,
    #[schemars(description = "Email ID (e.g. 2025-01-01-001)")]
    pub id: String,
}

#[derive(Serialize, Deserialize, schemars::JsonSchema)]
pub struct EmailMarkParams {
    #[schemars(description = "Mailbox name")]
    pub mailbox: String,
    #[schemars(description = "Email ID (e.g. 2025-01-01-001)")]
    pub id: String,
}

#[derive(Serialize, Deserialize, schemars::JsonSchema)]
pub struct EmailSendParams {
    #[schemars(description = "Mailbox name to send from")]
    pub from_mailbox: String,
    #[schemars(description = "Recipient email address")]
    pub to: String,
    #[schemars(description = "Email subject")]
    pub subject: String,
    #[schemars(description = "Email body text")]
    pub body: String,
    #[schemars(description = "File paths to attach")]
    pub attachments: Option<Vec<String>>,
}

#[derive(Serialize, Deserialize, schemars::JsonSchema)]
pub struct EmailReplyParams {
    #[schemars(description = "Mailbox name containing the email to reply to")]
    pub mailbox: String,
    #[schemars(description = "Email ID to reply to (e.g. 2025-01-01-001)")]
    pub id: String,
    #[schemars(description = "Reply body text")]
    pub body: String,
}

impl AimxMcpServer {
    pub fn new(data_dir: PathBuf) -> Self {
        Self {
            data_dir,
            tool_router: Self::tool_router(),
        }
    }

    fn load_config(&self) -> Result<Config, String> {
        Config::load_from_data_dir(&self.data_dir)
            .map_err(|e| format!("Failed to load config: {e}"))
    }
}

#[tool_router]
impl AimxMcpServer {
    #[tool(name = "mailbox_create", description = "Create a new mailbox")]
    fn mailbox_create(
        &self,
        Parameters(params): Parameters<MailboxCreateParams>,
    ) -> Result<String, String> {
        let config = self.load_config()?;
        mailbox::create_mailbox(&config, &params.name).map_err(|e| e.to_string())?;
        Ok(format!("Mailbox '{}' created successfully.", params.name))
    }

    #[tool(
        name = "mailbox_list",
        description = "List all mailboxes with message counts"
    )]
    fn mailbox_list(&self) -> Result<String, String> {
        let config = self.load_config()?;
        let mailboxes = list_mailboxes_with_unread(&config);

        if mailboxes.is_empty() {
            return Ok("No mailboxes configured.".to_string());
        }

        let result: Vec<String> = mailboxes
            .iter()
            .map(|(name, total, unread)| format!("{name}: {total} messages ({unread} unread)"))
            .collect();
        Ok(result.join("\n"))
    }

    #[tool(
        name = "mailbox_delete",
        description = "Delete a mailbox and all its emails"
    )]
    fn mailbox_delete(
        &self,
        Parameters(params): Parameters<MailboxDeleteParams>,
    ) -> Result<String, String> {
        let config = self.load_config()?;
        mailbox::delete_mailbox(&config, &params.name).map_err(|e| e.to_string())?;
        Ok(format!("Mailbox '{}' deleted.", params.name))
    }

    #[tool(
        name = "email_list",
        description = "List emails in a mailbox with optional filters"
    )]
    fn email_list(
        &self,
        Parameters(params): Parameters<EmailListParams>,
    ) -> Result<String, String> {
        let config = self.load_config()?;

        if !config.mailboxes.contains_key(&params.mailbox) {
            return Err(format!("Mailbox '{}' does not exist.", params.mailbox));
        }

        let mailbox_dir = config.mailbox_dir(&params.mailbox);
        let emails = list_emails(&mailbox_dir).map_err(|e| e.to_string())?;

        let filtered = filter_emails(
            emails,
            params.unread,
            params.from.as_deref(),
            params.since.as_deref(),
            params.subject.as_deref(),
        )?;

        if filtered.is_empty() {
            return Ok("No emails found.".to_string());
        }

        let result: Vec<String> = filtered
            .iter()
            .map(|meta| {
                let read_status = if meta.read { "read" } else { "unread" };
                format!(
                    "[{}] {} | From: {} | Subject: {} | Date: {}",
                    read_status, meta.id, meta.from, meta.subject, meta.date
                )
            })
            .collect();
        Ok(result.join("\n"))
    }

    #[tool(name = "email_read", description = "Read the full content of an email")]
    fn email_read(
        &self,
        Parameters(params): Parameters<EmailReadParams>,
    ) -> Result<String, String> {
        let config = self.load_config()?;
        validate_email_id(&params.id)?;

        if !config.mailboxes.contains_key(&params.mailbox) {
            return Err(format!("Mailbox '{}' does not exist.", params.mailbox));
        }

        let mailbox_dir = config.mailbox_dir(&params.mailbox);
        let filepath = mailbox_dir.join(format!("{}.md", params.id));

        if !filepath.exists() {
            return Err(format!(
                "Email '{}' not found in mailbox '{}'.",
                params.id, params.mailbox
            ));
        }

        std::fs::read_to_string(&filepath).map_err(|e| format!("Failed to read email: {e}"))
    }

    #[tool(name = "email_mark_read", description = "Mark an email as read")]
    fn email_mark_read(
        &self,
        Parameters(params): Parameters<EmailMarkParams>,
    ) -> Result<String, String> {
        validate_email_id(&params.id)?;
        let config = self.load_config()?;
        set_read_status(&config, &params.mailbox, &params.id, true)?;
        Ok(format!("Email '{}' marked as read.", params.id))
    }

    #[tool(name = "email_mark_unread", description = "Mark an email as unread")]
    fn email_mark_unread(
        &self,
        Parameters(params): Parameters<EmailMarkParams>,
    ) -> Result<String, String> {
        validate_email_id(&params.id)?;
        let config = self.load_config()?;
        set_read_status(&config, &params.mailbox, &params.id, false)?;
        Ok(format!("Email '{}' marked as unread.", params.id))
    }

    #[tool(
        name = "email_send",
        description = "Compose, DKIM-sign, and send an email"
    )]
    fn email_send(
        &self,
        Parameters(params): Parameters<EmailSendParams>,
    ) -> Result<String, String> {
        let config = self.load_config()?;

        if !config.mailboxes.contains_key(&params.from_mailbox) {
            return Err(format!("Mailbox '{}' does not exist.", params.from_mailbox));
        }

        let from_address = &config.mailboxes[&params.from_mailbox].address;
        if from_address.starts_with('*') {
            return Err(format!(
                "Cannot send from '{}': catchall mailbox has no valid sender address. Use a named mailbox.",
                params.from_mailbox
            ));
        }

        let args = SendArgs {
            from: from_address.clone(),
            to: params.to,
            subject: params.subject,
            body: params.body,
            reply_to: None,
            references: None,
            attachments: params.attachments.unwrap_or_default(),
        };

        let private_key = load_dkim_key(&config)?;
        let transport = send::LettreTransport::new(config.enable_ipv6);
        let dkim_info = Some((
            &private_key,
            config.domain.as_str(),
            config.dkim_selector.as_str(),
        ));

        let (message_id, server) =
            send::send_with_transport(&args, &transport, dkim_info).map_err(|e| e.to_string())?;

        Ok(format!(
            "Delivered to {server} for {}. Message-ID: {message_id}",
            args.to
        ))
    }

    #[tool(
        name = "email_reply",
        description = "Reply to an email with correct threading headers"
    )]
    fn email_reply(
        &self,
        Parameters(params): Parameters<EmailReplyParams>,
    ) -> Result<String, String> {
        let config = self.load_config()?;
        validate_email_id(&params.id)?;

        if !config.mailboxes.contains_key(&params.mailbox) {
            return Err(format!("Mailbox '{}' does not exist.", params.mailbox));
        }

        let mailbox_dir = config.mailbox_dir(&params.mailbox);
        let filepath = mailbox_dir.join(format!("{}.md", params.id));

        if !filepath.exists() {
            return Err(format!(
                "Email '{}' not found in mailbox '{}'.",
                params.id, params.mailbox
            ));
        }

        let content =
            std::fs::read_to_string(&filepath).map_err(|e| format!("Failed to read email: {e}"))?;

        let meta = parse_frontmatter(&content)
            .ok_or_else(|| "Failed to parse email frontmatter.".to_string())?;

        let from_address = &config.mailboxes[&params.mailbox].address;
        if from_address.starts_with('*') {
            return Err(format!(
                "Cannot reply from '{}': catchall mailbox has no valid sender address. Use a named mailbox.",
                params.mailbox
            ));
        }

        let reply_to_addr = &meta.from;
        let reply_to_email = extract_email_address(reply_to_addr);

        let subject = if meta.subject.starts_with("Re: ") {
            meta.subject.clone()
        } else {
            format!("Re: {}", meta.subject)
        };

        let (reply_to_id, references) = if meta.message_id.is_empty() {
            (None, None)
        } else {
            let refs = send::build_references(
                if meta.references.is_empty() {
                    None
                } else {
                    Some(&meta.references)
                },
                &meta.message_id,
            );
            (Some(meta.message_id.clone()), Some(refs))
        };

        let args = SendArgs {
            from: from_address.clone(),
            to: reply_to_email.to_string(),
            subject,
            body: params.body,
            reply_to: reply_to_id,
            references,
            attachments: vec![],
        };

        let private_key = load_dkim_key(&config)?;
        let transport = send::LettreTransport::new(config.enable_ipv6);
        let dkim_info = Some((
            &private_key,
            config.domain.as_str(),
            config.dkim_selector.as_str(),
        ));

        let (message_id, server) =
            send::send_with_transport(&args, &transport, dkim_info).map_err(|e| e.to_string())?;

        Ok(format!(
            "Delivered to {server} for {}. Message-ID: {message_id}",
            args.to
        ))
    }
}

#[tool_handler]
impl ServerHandler for AimxMcpServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_instructions("AIMX email server - manage mailboxes and emails for AI agents")
    }
}

pub async fn run(data_dir: Option<&std::path::Path>) -> Result<(), Box<dyn std::error::Error>> {
    let data_dir = data_dir
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("/var/lib/aimx"));

    let server = AimxMcpServer::new(data_dir);
    let transport = rmcp::transport::io::stdio();

    let service = server
        .serve(transport)
        .await
        .map_err(|e| format!("Failed to start MCP server: {e}"))?;

    service.waiting().await?;
    Ok(())
}

fn list_mailboxes_with_unread(config: &Config) -> Vec<(String, usize, usize)> {
    let mut result: Vec<(String, usize, usize)> = config
        .mailboxes
        .keys()
        .map(|name| {
            let dir = config.mailbox_dir(name);
            let emails = list_emails(&dir).unwrap_or_default();
            let total = emails.len();
            let unread = emails.iter().filter(|e| !e.read).count();
            (name.clone(), total, unread)
        })
        .collect();
    result.sort_by(|a, b| a.0.cmp(&b.0));
    result
}

pub fn list_emails(
    mailbox_dir: &std::path::Path,
) -> Result<Vec<EmailMetadata>, Box<dyn std::error::Error>> {
    let mut emails = Vec::new();

    let entries = match std::fs::read_dir(mailbox_dir) {
        Ok(e) => e,
        Err(_) => return Ok(emails),
    };

    for entry in entries.filter_map(|e| e.ok()) {
        let path = entry.path();
        if path.extension().is_some_and(|ext| ext == "md") {
            let content = std::fs::read_to_string(&path)?;
            if let Some(meta) = parse_frontmatter(&content) {
                emails.push(meta);
            }
        }
    }

    emails.sort_by(|a, b| a.date.cmp(&b.date));
    Ok(emails)
}

pub fn parse_frontmatter(content: &str) -> Option<EmailMetadata> {
    let parts: Vec<&str> = content.splitn(3, "+++").collect();
    if parts.len() < 3 {
        return None;
    }
    let toml_str = parts[1].trim();
    toml::from_str(toml_str).ok()
}

pub fn filter_emails(
    emails: Vec<EmailMetadata>,
    unread: Option<bool>,
    from: Option<&str>,
    since: Option<&str>,
    subject: Option<&str>,
) -> Result<Vec<EmailMetadata>, String> {
    let since_dt = match since {
        Some(s) => Some(
            chrono::DateTime::parse_from_rfc3339(s)
                .map(|dt| dt.with_timezone(&chrono::Utc))
                .map_err(|e| format!("Invalid 'since' datetime '{}': {}", s, e))?,
        ),
        None => None,
    };

    let result = emails
        .into_iter()
        .filter(|e| {
            if let Some(unread_filter) = unread {
                if unread_filter && e.read {
                    return false;
                }
                if !unread_filter && !e.read {
                    return false;
                }
            }
            if let Some(from_filter) = from
                && !e.from.to_lowercase().contains(&from_filter.to_lowercase())
            {
                return false;
            }
            if let Some(ref since_dt) = since_dt
                && let Ok(email_dt) = chrono::DateTime::parse_from_rfc3339(&e.date)
                && email_dt.with_timezone(&chrono::Utc) < *since_dt
            {
                return false;
            }
            if let Some(subject_filter) = subject
                && !e
                    .subject
                    .to_lowercase()
                    .contains(&subject_filter.to_lowercase())
            {
                return false;
            }
            true
        })
        .collect();
    Ok(result)
}

pub fn set_read_status(config: &Config, mailbox: &str, id: &str, read: bool) -> Result<(), String> {
    validate_email_id(id)?;

    if !config.mailboxes.contains_key(mailbox) {
        return Err(format!("Mailbox '{mailbox}' does not exist."));
    }

    let mailbox_dir = config.mailbox_dir(mailbox);
    let filepath = mailbox_dir.join(format!("{id}.md"));

    if !filepath.exists() {
        return Err(format!("Email '{id}' not found in mailbox '{mailbox}'."));
    }

    let content =
        std::fs::read_to_string(&filepath).map_err(|e| format!("Failed to read email: {e}"))?;

    let parts: Vec<&str> = content.splitn(3, "+++").collect();
    if parts.len() < 3 {
        return Err("Invalid email format.".to_string());
    }

    let toml_str = parts[1].trim();
    let mut meta: EmailMetadata =
        toml::from_str(toml_str).map_err(|e| format!("Failed to parse frontmatter: {e}"))?;

    meta.read = read;

    let new_toml = toml::to_string_pretty(&meta)
        .map_err(|e| format!("Failed to serialize frontmatter: {e}"))?;
    let body = parts[2];

    let mut result = String::new();
    result.push_str("+++\n");
    result.push_str(&new_toml);
    result.push_str("+++");
    result.push_str(body);

    std::fs::write(&filepath, result).map_err(|e| format!("Failed to write email: {e}"))?;

    Ok(())
}

fn load_dkim_key(config: &Config) -> Result<rsa::RsaPrivateKey, String> {
    dkim::load_private_key(&config.data_dir)
        .map_err(|e| format!("DKIM signing required but private key could not be loaded: {e}"))
}

fn validate_email_id(id: &str) -> Result<(), String> {
    if id.is_empty() {
        return Err("Email ID cannot be empty.".to_string());
    }
    if id.contains("..") || id.contains('/') || id.contains('\\') || id.contains('\0') {
        return Err("Email ID contains invalid characters.".to_string());
    }
    Ok(())
}

fn extract_email_address(addr: &str) -> &str {
    if let Some(start) = addr.find('<')
        && let Some(end) = addr.find('>')
    {
        return &addr[start + 1..end];
    }
    addr
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::MailboxConfig;
    use std::collections::HashMap;
    use tempfile::TempDir;

    fn test_config(tmp: &std::path::Path) -> Config {
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

    fn setup_config(config: &Config) {
        std::fs::create_dir_all(&config.data_dir).unwrap();
        let config_path = Config::config_path(&config.data_dir);
        config.save(&config_path).unwrap();
    }

    fn create_test_email(dir: &std::path::Path, id: &str, meta: &EmailMetadata) {
        std::fs::create_dir_all(dir).unwrap();
        let toml_str = toml::to_string_pretty(meta).unwrap();
        let content = format!("+++\n{toml_str}+++\n\nThis is the body of email {id}.\n");
        std::fs::write(dir.join(format!("{id}.md")), content).unwrap();
    }

    fn sample_meta(id: &str, from: &str, subject: &str, read: bool) -> EmailMetadata {
        EmailMetadata {
            id: id.to_string(),
            message_id: format!("<{id}@test.com>"),
            from: from.to_string(),
            to: "alice@test.com".to_string(),
            subject: subject.to_string(),
            date: "2025-06-01T12:00:00Z".to_string(),
            in_reply_to: "".to_string(),
            references: "".to_string(),
            attachments: vec![],
            mailbox: "alice".to_string(),
            read,
            dkim: "none".to_string(),
            spf: "none".to_string(),
        }
    }

    #[test]
    fn parse_frontmatter_valid() {
        let meta = sample_meta("2025-06-01-001", "sender@example.com", "Hello", false);
        let toml_str = toml::to_string_pretty(&meta).unwrap();
        let content = format!("+++\n{toml_str}+++\n\nBody text.\n");

        let parsed = parse_frontmatter(&content).unwrap();
        assert_eq!(parsed.id, "2025-06-01-001");
        assert_eq!(parsed.from, "sender@example.com");
        assert_eq!(parsed.subject, "Hello");
        assert!(!parsed.read);
    }

    #[test]
    fn parse_frontmatter_invalid() {
        let result = parse_frontmatter("no frontmatter here");
        assert!(result.is_none());
    }

    #[test]
    fn list_emails_empty_dir() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("empty");
        std::fs::create_dir_all(&dir).unwrap();

        let result = list_emails(&dir).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn list_emails_returns_sorted() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("alice");

        let mut meta1 = sample_meta("2025-06-01-001", "a@test.com", "First", false);
        meta1.date = "2025-06-01T10:00:00Z".to_string();

        let mut meta2 = sample_meta("2025-06-01-002", "b@test.com", "Second", true);
        meta2.date = "2025-06-01T11:00:00Z".to_string();

        create_test_email(&dir, "2025-06-01-001", &meta1);
        create_test_email(&dir, "2025-06-01-002", &meta2);

        let result = list_emails(&dir).unwrap();
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].id, "2025-06-01-001");
        assert_eq!(result[1].id, "2025-06-01-002");
    }

    #[test]
    fn filter_by_unread() {
        let emails = vec![
            sample_meta("001", "a@test.com", "A", false),
            sample_meta("002", "b@test.com", "B", true),
            sample_meta("003", "c@test.com", "C", false),
        ];

        let filtered = filter_emails(emails, Some(true), None, None, None).unwrap();
        assert_eq!(filtered.len(), 2);
        assert!(filtered.iter().all(|e| !e.read));
    }

    #[test]
    fn filter_by_read() {
        let emails = vec![
            sample_meta("001", "a@test.com", "A", false),
            sample_meta("002", "b@test.com", "B", true),
        ];

        let filtered = filter_emails(emails, Some(false), None, None, None).unwrap();
        assert_eq!(filtered.len(), 1);
        assert!(filtered[0].read);
    }

    #[test]
    fn filter_by_from() {
        let emails = vec![
            sample_meta("001", "alice@gmail.com", "A", false),
            sample_meta("002", "bob@yahoo.com", "B", false),
            sample_meta("003", "Alice Smith <alice@work.com>", "C", false),
        ];

        let filtered = filter_emails(emails, None, Some("alice"), None, None).unwrap();
        assert_eq!(filtered.len(), 2);
    }

    #[test]
    fn filter_by_subject() {
        let emails = vec![
            sample_meta("001", "a@test.com", "Meeting Tomorrow", false),
            sample_meta("002", "b@test.com", "Invoice #123", false),
            sample_meta("003", "c@test.com", "Re: meeting notes", false),
        ];

        let filtered = filter_emails(emails, None, None, None, Some("meeting")).unwrap();
        assert_eq!(filtered.len(), 2);
    }

    #[test]
    fn filter_by_since() {
        let mut e1 = sample_meta("001", "a@test.com", "Old", false);
        e1.date = "2025-01-01T00:00:00Z".to_string();

        let mut e2 = sample_meta("002", "b@test.com", "Recent", false);
        e2.date = "2025-06-15T00:00:00Z".to_string();

        let emails = vec![e1, e2];
        let filtered =
            filter_emails(emails, None, None, Some("2025-06-01T00:00:00Z"), None).unwrap();
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].id, "002");
    }

    #[test]
    fn filter_combined() {
        let mut e1 = sample_meta("001", "alice@gmail.com", "Meeting", false);
        e1.date = "2025-06-15T00:00:00Z".to_string();

        let mut e2 = sample_meta("002", "alice@gmail.com", "Invoice", false);
        e2.date = "2025-06-15T00:00:00Z".to_string();

        let mut e3 = sample_meta("003", "bob@yahoo.com", "Meeting", false);
        e3.date = "2025-06-15T00:00:00Z".to_string();

        let emails = vec![e1, e2, e3];
        let filtered = filter_emails(
            emails,
            Some(true),
            Some("alice"),
            Some("2025-06-01T00:00:00Z"),
            Some("meeting"),
        )
        .unwrap();
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].id, "001");
    }

    #[test]
    fn filter_no_filters_returns_all() {
        let emails = vec![
            sample_meta("001", "a@test.com", "A", false),
            sample_meta("002", "b@test.com", "B", true),
        ];

        let filtered = filter_emails(emails, None, None, None, None).unwrap();
        assert_eq!(filtered.len(), 2);
    }

    #[test]
    fn set_read_status_marks_read() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(tmp.path());
        setup_config(&config);

        let meta = sample_meta("2025-06-01-001", "sender@test.com", "Test", false);
        create_test_email(&tmp.path().join("alice"), "2025-06-01-001", &meta);

        set_read_status(&config, "alice", "2025-06-01-001", true).unwrap();

        let content = std::fs::read_to_string(tmp.path().join("alice/2025-06-01-001.md")).unwrap();
        let parsed = parse_frontmatter(&content).unwrap();
        assert!(parsed.read);
    }

    #[test]
    fn set_read_status_marks_unread() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(tmp.path());
        setup_config(&config);

        let meta = sample_meta("2025-06-01-001", "sender@test.com", "Test", true);
        create_test_email(&tmp.path().join("alice"), "2025-06-01-001", &meta);

        set_read_status(&config, "alice", "2025-06-01-001", false).unwrap();

        let content = std::fs::read_to_string(tmp.path().join("alice/2025-06-01-001.md")).unwrap();
        let parsed = parse_frontmatter(&content).unwrap();
        assert!(!parsed.read);
    }

    #[test]
    fn set_read_status_nonexistent_mailbox() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(tmp.path());
        setup_config(&config);

        let result = set_read_status(&config, "nonexistent", "2025-06-01-001", true);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("does not exist"));
    }

    #[test]
    fn set_read_status_nonexistent_email() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(tmp.path());
        setup_config(&config);

        std::fs::create_dir_all(tmp.path().join("alice")).unwrap();
        let result = set_read_status(&config, "alice", "nonexistent", true);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("not found"));
    }

    #[test]
    fn extract_email_address_with_name() {
        assert_eq!(
            extract_email_address("Alice <alice@test.com>"),
            "alice@test.com"
        );
    }

    #[test]
    fn extract_email_address_bare() {
        assert_eq!(extract_email_address("alice@test.com"), "alice@test.com");
    }

    #[test]
    fn list_mailboxes_with_unread_counts() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(tmp.path());
        setup_config(&config);

        let meta1 = sample_meta("2025-06-01-001", "a@test.com", "A", false);
        let meta2 = sample_meta("2025-06-01-002", "b@test.com", "B", true);
        create_test_email(&tmp.path().join("alice"), "2025-06-01-001", &meta1);
        create_test_email(&tmp.path().join("alice"), "2025-06-01-002", &meta2);

        let result = list_mailboxes_with_unread(&config);
        let alice = result.iter().find(|m| m.0 == "alice").unwrap();
        assert_eq!(alice.1, 2); // total
        assert_eq!(alice.2, 1); // unread
    }

    #[test]
    fn list_emails_nonexistent_dir() {
        let result = list_emails(std::path::Path::new("/nonexistent/path")).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn mcp_server_has_correct_tool_count() {
        let tmp = TempDir::new().unwrap();
        let server = AimxMcpServer::new(tmp.path().to_path_buf());
        let tools = server.tool_router.list_all();
        assert_eq!(tools.len(), 9);
    }

    #[test]
    fn mcp_server_tool_names() {
        let tmp = TempDir::new().unwrap();
        let server = AimxMcpServer::new(tmp.path().to_path_buf());
        let tools = server.tool_router.list_all();
        let names: Vec<&str> = tools.iter().map(|t| t.name.as_ref()).collect();

        assert!(names.contains(&"mailbox_create"));
        assert!(names.contains(&"mailbox_list"));
        assert!(names.contains(&"mailbox_delete"));
        assert!(names.contains(&"email_list"));
        assert!(names.contains(&"email_read"));
        assert!(names.contains(&"email_mark_read"));
        assert!(names.contains(&"email_mark_unread"));
        assert!(names.contains(&"email_send"));
        assert!(names.contains(&"email_reply"));
    }

    #[test]
    fn validate_email_id_rejects_path_traversal() {
        assert!(validate_email_id("../../etc/passwd").is_err());
        assert!(validate_email_id("foo/bar").is_err());
        assert!(validate_email_id("foo\\bar").is_err());
        assert!(validate_email_id("").is_err());
        assert!(validate_email_id("foo\0bar").is_err());
    }

    #[test]
    fn validate_email_id_accepts_valid() {
        assert!(validate_email_id("2025-06-01-001").is_ok());
        assert!(validate_email_id("2025-01-01-999").is_ok());
    }

    #[test]
    fn filter_emails_invalid_since_returns_error() {
        let emails = vec![sample_meta("001", "a@test.com", "A", false)];
        let result = filter_emails(emails, None, None, Some("not-a-date"), None);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Invalid 'since' datetime"));
    }

    #[test]
    fn set_read_status_rejects_path_traversal() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(tmp.path());
        setup_config(&config);

        std::fs::create_dir_all(tmp.path().join("alice")).unwrap();
        let result = set_read_status(&config, "alice", "../../etc/passwd", true);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("invalid characters"));
    }

    #[test]
    fn load_dkim_key_missing_returns_error() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(tmp.path());
        let result = load_dkim_key(&config);
        assert!(result.is_err());
        assert!(
            result.as_ref().unwrap_err().contains("DKIM"),
            "Error should mention DKIM: {}",
            result.unwrap_err()
        );
    }

    #[test]
    fn load_dkim_key_present_returns_ok() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(tmp.path());
        crate::dkim::generate_keypair(tmp.path(), false).unwrap();
        let result = load_dkim_key(&config);
        assert!(result.is_ok());
    }

    #[test]
    fn send_error_propagates_through_mcp_mapping() {
        use crate::cli::SendArgs;

        struct FailTransport;
        impl send::MailTransport for FailTransport {
            fn send(
                &self,
                _sender: &str,
                _recipient: &str,
                _message: &[u8],
            ) -> Result<String, Box<dyn std::error::Error>> {
                Err("Connection refused by mx.example.com".into())
            }
        }

        let args = SendArgs {
            from: "alice@test.com".to_string(),
            to: "bob@example.com".to_string(),
            subject: "Test".to_string(),
            body: "Body".to_string(),
            reply_to: None,
            references: None,
            attachments: vec![],
        };

        let result =
            send::send_with_transport(&args, &FailTransport, None).map_err(|e| e.to_string());

        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            err.contains("Connection refused"),
            "MCP-style error mapping should preserve delivery failure details: {err}"
        );
    }
}
