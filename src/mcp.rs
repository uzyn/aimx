use crate::cli::SendArgs;
use crate::config::Config;
use crate::frontmatter::InboundFrontmatter;
use crate::mailbox;
use crate::send;
use crate::send_protocol::{
    self, ErrCode, MailboxCrudRequest, MarkFolder, MarkRequest, SendRequest,
};

use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{ServerCapabilities, ServerInfo};
use rmcp::{ServerHandler, ServiceExt, schemars, tool, tool_handler, tool_router};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct AimxMcpServer {
    /// CLI `--data-dir` / `AIMX_DATA_DIR` override. When `Some`, it
    /// supersedes `config.data_dir` for all storage operations; when
    /// `None`, the value from `/etc/aimx/config.toml` is used.
    data_dir_override: Option<PathBuf>,
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
    #[schemars(description = "Which folder to read: \"inbox\" (default) or \"sent\"")]
    pub folder: Option<String>,
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
    #[schemars(description = "Email ID (e.g. 2025-06-15-120000-hello)")]
    pub id: String,
    #[schemars(description = "Which folder to read: \"inbox\" (default) or \"sent\"")]
    pub folder: Option<String>,
}

#[derive(Serialize, Deserialize, schemars::JsonSchema)]
pub struct EmailMarkParams {
    #[schemars(description = "Mailbox name")]
    pub mailbox: String,
    #[schemars(description = "Email ID (e.g. 2025-06-15-120000-hello)")]
    pub id: String,
    #[schemars(description = "Which folder to target: \"inbox\" (default) or \"sent\"")]
    pub folder: Option<String>,
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
    pub fn new(data_dir_override: Option<PathBuf>) -> Self {
        Self {
            data_dir_override,
            tool_router: Self::tool_router(),
        }
    }

    fn load_config(&self) -> Result<Config, String> {
        Config::load_resolved_with_data_dir(self.data_dir_override.as_deref())
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
        // Prefer the daemon UDS path — the daemon atomically rewrites
        // config.toml and hot-swaps its in-memory Config so a following
        // inbound mail routes correctly with no restart. Fall back to
        // direct on-disk edit only when the socket isn't reachable
        // (daemon stopped, first-time setup, etc.).
        match submit_mailbox_crud_via_daemon(&params.name, true) {
            Ok(()) => Ok(format!("Mailbox '{}' created successfully.", params.name)),
            Err(MailboxCrudFallback::SocketMissing) => {
                let config = self.load_config()?;
                mailbox::create_mailbox(&config, &params.name).map_err(|e| e.to_string())?;
                Ok(format!(
                    "Mailbox '{}' created successfully (daemon not running — restart aimx to apply the change).",
                    params.name
                ))
            }
            Err(MailboxCrudFallback::Daemon(msg)) => Err(msg),
        }
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
            .map(|(name, total, unread, sent_count, registered)| {
                let suffix = if *registered { "" } else { " (unregistered)" };
                format!("{name}: {total} messages ({unread} unread), {sent_count} sent{suffix}")
            })
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
        // Daemon-side delete refuses when the inbox/sent directories
        // still contain files (returns ERR NONEMPTY). MCP deliberately
        // does NOT gain a force variant — destructive wipes stay on the
        // CLI where operators see prompts and can't be triggered
        // remotely by an agent. On NONEMPTY we rewrite the daemon's
        // error into a structured hint that names the exact CLI command
        // (S48-6). The fallback direct-on-disk path runs only when the
        // daemon is unreachable.
        match submit_mailbox_crud_via_daemon(&params.name, false) {
            Ok(()) => Ok(format!(
                "Mailbox '{0}' deleted. Empty `inbox/{0}/` and `sent/{0}/` \
                 directories remain on disk — run `rmdir` to tidy up if desired.",
                params.name
            )),
            Err(MailboxCrudFallback::SocketMissing) => {
                let config = self.load_config()?;
                mailbox::delete_mailbox(&config, &params.name).map_err(|e| e.to_string())?;
                Ok(format!(
                    "Mailbox '{}' deleted (daemon not running — restart aimx to apply the change).",
                    params.name
                ))
            }
            Err(MailboxCrudFallback::Daemon(msg)) => {
                Err(rewrite_nonempty_error_for_mcp(&params.name, &msg))
            }
        }
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

        let folder = resolve_folder(params.folder.as_deref())?;
        let mailbox_dir = folder_dir(&config, &params.mailbox, folder);
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

        let folder = resolve_folder(params.folder.as_deref())?;
        let mailbox_dir = folder_dir(&config, &params.mailbox, folder);
        let filepath = resolve_email_path(&mailbox_dir, &params.id).ok_or_else(|| {
            format!(
                "Email '{}' not found in mailbox '{}'.",
                params.id, params.mailbox
            )
        })?;

        std::fs::read_to_string(&filepath).map_err(|e| format!("Failed to read email: {e}"))
    }

    #[tool(name = "email_mark_read", description = "Mark an email as read")]
    fn email_mark_read(
        &self,
        Parameters(params): Parameters<EmailMarkParams>,
    ) -> Result<String, String> {
        validate_email_id(&params.id)?;
        let folder = resolve_folder(params.folder.as_deref())?;
        // Route through the daemon — mailbox files are root-owned and
        // the MCP process runs as the invoking user.
        submit_mark_via_daemon(&params.mailbox, &params.id, folder, true)?;
        Ok(format!("Email '{}' marked as read.", params.id))
    }

    #[tool(name = "email_mark_unread", description = "Mark an email as unread")]
    fn email_mark_unread(
        &self,
        Parameters(params): Parameters<EmailMarkParams>,
    ) -> Result<String, String> {
        validate_email_id(&params.id)?;
        let folder = resolve_folder(params.folder.as_deref())?;
        submit_mark_via_daemon(&params.mailbox, &params.id, folder, false)?;
        Ok(format!("Email '{}' marked as unread.", params.id))
    }

    #[tool(
        name = "email_send",
        description = "Submit an email for DKIM signing and delivery via the aimx daemon"
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

        submit_via_daemon(&args)
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

        let mailbox_dir = config.inbox_dir(&params.mailbox);
        let filepath = resolve_email_path(&mailbox_dir, &params.id).ok_or_else(|| {
            format!(
                "Email '{}' not found in mailbox '{}'.",
                params.id, params.mailbox
            )
        })?;

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
            let refs = send::build_references(meta.references.as_deref(), &meta.message_id);
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

        submit_via_daemon(&args)
    }
}

/// Compose `args` into an `AIMX/1 SEND` request and submit it to
/// `aimx serve` over the UDS. MCP, like `aimx send`, does not sign or
/// deliver mail directly — everything goes through the daemon. The
/// request frame carries no `From-Mailbox:` header; the daemon parses
/// `From:` out of the composed body itself and resolves the sender
/// mailbox against its in-memory Config.
fn submit_via_daemon(args: &SendArgs) -> Result<String, String> {
    let composed = send::compose_message(args).map_err(|e| e.to_string())?;
    let request = SendRequest {
        body: composed.message,
    };
    let socket = crate::serve::send_socket_path();

    let rt = tokio::runtime::Handle::try_current();
    let outcome = match rt {
        Ok(handle) => {
            tokio::task::block_in_place(|| handle.block_on(send::submit_request(&socket, &request)))
        }
        Err(_) => {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|e| format!("Failed to create tokio runtime: {e}"))?;
            rt.block_on(send::submit_request(&socket, &request))
        }
    };

    let outcome = outcome.map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            "aimx daemon not running — start with 'sudo systemctl start aimx'".to_string()
        } else {
            format!(
                "Failed to connect to aimx daemon at {}: {e}",
                socket.display()
            )
        }
    })?;

    match outcome {
        send::SubmitOutcome::Ok { message_id } => Ok(format!(
            "Email sent to {}. Message-ID: {message_id}",
            args.to
        )),
        send::SubmitOutcome::Err { code, reason } => Err(format!("[{}] {reason}", code.as_str())),
        send::SubmitOutcome::Malformed(reason) => {
            Err(format!("Malformed response from aimx daemon: {reason}"))
        }
    }
}

/// Submit a `MARK-READ` or `MARK-UNREAD` request to the daemon over UDS.
/// MCP's `email_mark_read` / `email_mark_unread` tools route through this
/// path so the non-root MCP process doesn't need write access to the
/// root-owned mailbox files.
fn submit_mark_via_daemon(
    mailbox: &str,
    id: &str,
    folder: Folder,
    read: bool,
) -> Result<(), String> {
    let folder = match folder {
        Folder::Inbox => MarkFolder::Inbox,
        Folder::Sent => MarkFolder::Sent,
    };
    let request = MarkRequest {
        mailbox: mailbox.to_string(),
        id: id.to_string(),
        folder,
        read,
    };
    let socket = crate::serve::send_socket_path();

    let rt = tokio::runtime::Handle::try_current();
    let outcome = match rt {
        Ok(handle) => {
            tokio::task::block_in_place(|| handle.block_on(submit_mark_request(&socket, &request)))
        }
        Err(_) => {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|e| format!("Failed to create tokio runtime: {e}"))?;
            rt.block_on(submit_mark_request(&socket, &request))
        }
    };

    let outcome = outcome.map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            "aimx daemon not running — start with 'sudo systemctl start aimx'".to_string()
        } else {
            format!(
                "Failed to connect to aimx daemon at {}: {e}",
                socket.display()
            )
        }
    })?;

    match outcome {
        MarkOutcome::Ok => Ok(()),
        MarkOutcome::Err { code, reason } => Err(format!("[{}] {reason}", code.as_str())),
        MarkOutcome::Malformed(reason) => {
            Err(format!("Malformed response from aimx daemon: {reason}"))
        }
    }
}

#[derive(Debug)]
enum MarkOutcome {
    Ok,
    Err { code: ErrCode, reason: String },
    Malformed(String),
}

/// Outcome of a mailbox CRUD submission that didn't succeed via UDS.
/// Tracks socket-missing distinctly from daemon-side errors so the MCP
/// tool can decide whether to fall back to the direct on-disk edit or
/// surface the daemon's reason verbatim.
pub(crate) enum MailboxCrudFallback {
    /// Socket not present / not connectable (daemon stopped, socket
    /// cleaned up, first-time setup). Callers fall back to direct edit.
    SocketMissing,
    /// Daemon connected and answered but reported an error (validation,
    /// NONEMPTY, IO, etc.). Caller should surface this verbatim.
    Daemon(String),
}

/// Submit a `MAILBOX-CREATE` / `MAILBOX-DELETE` request over UDS.
pub(crate) fn submit_mailbox_crud_via_daemon(
    name: &str,
    create: bool,
) -> Result<(), MailboxCrudFallback> {
    let request = MailboxCrudRequest {
        name: name.to_string(),
        create,
    };
    let socket = crate::serve::send_socket_path();

    let rt = tokio::runtime::Handle::try_current();
    let io_result: Result<MarkOutcome, std::io::Error> = match rt {
        Ok(handle) => tokio::task::block_in_place(|| {
            handle.block_on(submit_mailbox_crud_request(&socket, &request))
        }),
        Err(_) => {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|e| {
                    MailboxCrudFallback::Daemon(format!("Failed to create tokio runtime: {e}"))
                })?;
            rt.block_on(submit_mailbox_crud_request(&socket, &request))
        }
    };

    match io_result {
        Ok(MarkOutcome::Ok) => Ok(()),
        Ok(MarkOutcome::Err { code, reason }) => Err(MailboxCrudFallback::Daemon(format!(
            "[{}] {reason}",
            code.as_str()
        ))),
        Ok(MarkOutcome::Malformed(reason)) => Err(MailboxCrudFallback::Daemon(format!(
            "Malformed response from aimx daemon: {reason}"
        ))),
        Err(e) => {
            if is_socket_missing(&e) {
                Err(MailboxCrudFallback::SocketMissing)
            } else {
                Err(MailboxCrudFallback::Daemon(format!(
                    "Failed to connect to aimx daemon at {}: {e}",
                    socket.display()
                )))
            }
        }
    }
}

async fn submit_mailbox_crud_request(
    socket_path: &std::path::Path,
    request: &MailboxCrudRequest,
) -> Result<MarkOutcome, std::io::Error> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let stream = tokio::net::UnixStream::connect(socket_path).await?;
    let (mut reader, mut writer) = stream.into_split();

    send_protocol::write_mailbox_crud_request(&mut writer, request).await?;
    writer.shutdown().await.ok();

    let mut buf = Vec::with_capacity(128);
    reader.read_to_end(&mut buf).await?;

    Ok(parse_ack_response(&buf))
}

/// `true` when the I/O error indicates the daemon socket is not reachable
/// (not present, connection refused, permission denied). Callers use this
/// to decide whether to fall back to a direct on-disk edit.
fn is_socket_missing(e: &std::io::Error) -> bool {
    matches!(
        e.kind(),
        std::io::ErrorKind::NotFound
            | std::io::ErrorKind::ConnectionRefused
            | std::io::ErrorKind::PermissionDenied
    )
}

async fn submit_mark_request(
    socket_path: &std::path::Path,
    request: &MarkRequest,
) -> Result<MarkOutcome, std::io::Error> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let stream = tokio::net::UnixStream::connect(socket_path).await?;
    let (mut reader, mut writer) = stream.into_split();

    send_protocol::write_mark_request(&mut writer, request).await?;
    writer.shutdown().await.ok();

    let mut buf = Vec::with_capacity(128);
    reader.read_to_end(&mut buf).await?;

    Ok(parse_ack_response(&buf))
}

fn parse_ack_response(buf: &[u8]) -> MarkOutcome {
    let text = match std::str::from_utf8(buf) {
        Ok(s) => s,
        Err(_) => return MarkOutcome::Malformed("response is not UTF-8".to_string()),
    };
    let line = text.lines().next().unwrap_or("").trim_end_matches('\r');
    if line.is_empty() {
        return MarkOutcome::Malformed("empty response from daemon".to_string());
    }
    let rest = match line.strip_prefix("AIMX/1 ") {
        Some(r) => r,
        None => return MarkOutcome::Malformed(format!("unexpected response: {line:?}")),
    };
    // MARK verbs use a bare `AIMX/1 OK` ack — any trailing payload is a
    // protocol violation. Reject it as Malformed rather than silently
    // succeeding so the client notices if the daemon ever drifts.
    if rest == "OK" {
        return MarkOutcome::Ok;
    }
    if rest.starts_with("OK ") {
        return MarkOutcome::Malformed(format!("unexpected payload after OK: {line:?}"));
    }
    if let Some(err_body) = rest.strip_prefix("ERR ") {
        let (code_str, reason) = err_body.split_once(' ').unwrap_or((err_body, ""));
        let code = match code_str {
            "MAILBOX" => ErrCode::Mailbox,
            "DOMAIN" => ErrCode::Domain,
            "SIGN" => ErrCode::Sign,
            "DELIVERY" => ErrCode::Delivery,
            "TEMP" => ErrCode::Temp,
            "MALFORMED" => ErrCode::Malformed,
            "PROTOCOL" => ErrCode::Protocol,
            "NOTFOUND" => ErrCode::NotFound,
            "IO" => ErrCode::Io,
            // MAILBOX-CRUD verbs report these codes.
            "VALIDATION" => ErrCode::Validation,
            "NONEMPTY" => ErrCode::NonEmpty,
            _ => {
                return MarkOutcome::Malformed(format!(
                    "unknown ERR code {code_str:?} in response"
                ));
            }
        };
        return MarkOutcome::Err {
            code,
            reason: reason.trim().to_string(),
        };
    }
    MarkOutcome::Malformed(format!("unexpected response: {line:?}"))
}

#[tool_handler]
impl ServerHandler for AimxMcpServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_instructions("AIMX email server - manage mailboxes and emails for AI agents")
    }
}

pub async fn run(data_dir: Option<&std::path::Path>) -> Result<(), Box<dyn std::error::Error>> {
    let server = AimxMcpServer::new(data_dir.map(|p| p.to_path_buf()));
    let transport = rmcp::transport::io::stdio();

    let service = server
        .serve(transport)
        .await
        .map_err(|e| format!("Failed to start MCP server: {e}"))?;

    service.waiting().await?;
    Ok(())
}

/// Return `(name, total, unread, sent_count, registered)` for every mailbox
/// the daemon knows about — both registered ones in `config.mailboxes` and
/// stray `inbox/<name>/` directories left by backup restores or
/// out-of-band tooling. Sorted by name.
fn list_mailboxes_with_unread(config: &Config) -> Vec<(String, usize, usize, usize, bool)> {
    let mut result: Vec<(String, usize, usize, usize, bool)> =
        mailbox::discover_mailbox_names(config)
            .into_iter()
            .map(|name| {
                let dir = config.inbox_dir(&name);
                let emails = list_emails(&dir).unwrap_or_default();
                let total = emails.len();
                let unread = emails.iter().filter(|e| !e.read).count();
                let sent_count = mailbox::count_messages(&config.sent_dir(&name));
                let registered = mailbox::is_registered(config, &name);
                (name, total, unread, sent_count, registered)
            })
            .collect();
    result.sort_by(|a, b| a.0.cmp(&b.0));
    result
}

/// List the emails in a single folder. Handles both flat `<stem>.md`
/// entries and Zola-style bundle directories containing `<stem>.md`.
pub fn list_emails(
    mailbox_dir: &std::path::Path,
) -> Result<Vec<InboundFrontmatter>, Box<dyn std::error::Error>> {
    let mut emails = Vec::new();

    let entries = match std::fs::read_dir(mailbox_dir) {
        Ok(e) => e,
        Err(_) => return Ok(emails),
    };

    for entry in entries.filter_map(|e| e.ok()) {
        let path = entry.path();
        if path.is_dir() {
            // Bundle: read the `<stem>/<stem>.md` inside.
            if let Some(stem) = path.file_name().and_then(|f| f.to_str()) {
                let md = path.join(format!("{stem}.md"));
                if md.exists() {
                    let content = std::fs::read_to_string(&md)?;
                    if let Some(meta) = parse_frontmatter(&content) {
                        emails.push(meta);
                    }
                }
            }
        } else if path.extension().is_some_and(|ext| ext == "md") {
            let content = std::fs::read_to_string(&path)?;
            if let Some(meta) = parse_frontmatter(&content) {
                emails.push(meta);
            }
        }
    }

    emails.sort_by(|a, b| a.date.cmp(&b.date));
    Ok(emails)
}

/// Resolve the on-disk path for an email by ID. Returns `Some(path)` when
/// the email exists either as a flat `<id>.md` or as a bundle
/// `<id>/<id>.md` inside `mailbox_dir`.
pub fn resolve_email_path(mailbox_dir: &std::path::Path, id: &str) -> Option<PathBuf> {
    let flat = mailbox_dir.join(format!("{id}.md"));
    if flat.exists() {
        return Some(flat);
    }
    let bundle_md = mailbox_dir.join(id).join(format!("{id}.md"));
    if bundle_md.exists() {
        return Some(bundle_md);
    }
    None
}

/// Inbound/outbound folder selector for MCP tools.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Folder {
    Inbox,
    Sent,
}

/// Parse an MCP `folder` argument. Default is `inbox`.
pub fn resolve_folder(raw: Option<&str>) -> Result<Folder, String> {
    match raw {
        None | Some("inbox") => Ok(Folder::Inbox),
        Some("sent") => Ok(Folder::Sent),
        Some(other) => Err(format!(
            "Invalid folder '{other}': expected \"inbox\" or \"sent\"."
        )),
    }
}

fn folder_dir(config: &Config, mailbox: &str, folder: Folder) -> PathBuf {
    match folder {
        Folder::Inbox => config.inbox_dir(mailbox),
        Folder::Sent => config.sent_dir(mailbox),
    }
}

pub fn parse_frontmatter(content: &str) -> Option<InboundFrontmatter> {
    let parts: Vec<&str> = content.splitn(3, "+++").collect();
    if parts.len() < 3 {
        return None;
    }
    let toml_str = parts[1].trim();
    toml::from_str(toml_str).ok()
}

pub fn filter_emails(
    emails: Vec<InboundFrontmatter>,
    unread: Option<bool>,
    from: Option<&str>,
    since: Option<&str>,
    subject: Option<&str>,
) -> Result<Vec<InboundFrontmatter>, String> {
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

/// Direct-on-disk frontmatter rewrite. Not called by the MCP tool
/// handlers themselves — they route through the daemon via UDS so the
/// non-root MCP process doesn't need write access to root-owned mailbox
/// files. Retained for unit-test coverage of the frontmatter
/// read/rewrite flow; exercised at the protocol level by
/// `state_handler::tests` and at the MCP level by integration tests.
#[cfg(test)]
fn set_read_status(
    config: &Config,
    mailbox: &str,
    folder: Folder,
    id: &str,
    read: bool,
) -> Result<(), String> {
    validate_email_id(id)?;

    if !config.mailboxes.contains_key(mailbox) {
        return Err(format!("Mailbox '{mailbox}' does not exist."));
    }

    let mailbox_dir = folder_dir(config, mailbox, folder);
    let filepath = resolve_email_path(&mailbox_dir, id)
        .ok_or_else(|| format!("Email '{id}' not found in mailbox '{mailbox}'."))?;

    let content =
        std::fs::read_to_string(&filepath).map_err(|e| format!("Failed to read email: {e}"))?;

    let parts: Vec<&str> = content.splitn(3, "+++").collect();
    if parts.len() < 3 {
        return Err("Invalid email format.".to_string());
    }

    let toml_str = parts[1].trim();
    let mut meta: InboundFrontmatter =
        toml::from_str(toml_str).map_err(|e| format!("Failed to parse frontmatter: {e}"))?;

    meta.read = read;

    let new_toml =
        toml::to_string(&meta).map_err(|e| format!("Failed to serialize frontmatter: {e}"))?;
    let body = parts[2];

    let mut result = String::new();
    result.push_str("+++\n");
    result.push_str(&new_toml);
    result.push_str("+++");
    result.push_str(body);

    std::fs::write(&filepath, result).map_err(|e| format!("Failed to write email: {e}"))?;

    Ok(())
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

/// Rewrite the daemon's `[NONEMPTY]` error into an MCP-friendly hint
/// that names the exact CLI command. The MCP `mailbox_delete` tool
/// deliberately does not gain a force variant — destructive wipes stay
/// on the CLI where the operator sees prompts and the request can't be
/// triggered remotely. Non-NONEMPTY daemon errors pass through verbatim.
pub(crate) fn rewrite_nonempty_error_for_mcp(name: &str, msg: &str) -> String {
    if !msg.contains("[NONEMPTY]") {
        return msg.to_string();
    }
    let (inbox_files, sent_files) = parse_nonempty_counts(msg);
    format!(
        "Cannot delete mailbox '{name}' — inbox: {inbox_files} files, sent: {sent_files} files. \
         MCP `mailbox_delete` does not wipe mail; run `sudo aimx mailboxes delete --force {name}` \
         on the host to wipe and remove."
    )
}

/// Parse `(N in inbox, M in sent)` out of the daemon's NONEMPTY reason
/// string. Returns `(0, 0)` when the format doesn't match — the caller
/// still gets the hint with zeroed counts, which is better than 500ing.
fn parse_nonempty_counts(msg: &str) -> (usize, usize) {
    let mut inbox = 0usize;
    let mut sent = 0usize;
    if let Some(idx) = msg.find(" in inbox") {
        inbox = msg[..idx]
            .rsplit(|c: char| !c.is_ascii_digit())
            .find(|s| !s.is_empty())
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
    }
    if let Some(idx) = msg.find(" in sent") {
        sent = msg[..idx]
            .rsplit(|c: char| !c.is_ascii_digit())
            .find(|s| !s.is_empty())
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
    }
    (inbox, sent)
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
                trust: None,
                trusted_senders: None,
            },
        );
        mailboxes.insert(
            "alice".to_string(),
            MailboxConfig {
                address: "alice@test.com".to_string(),
                on_receive: vec![],
                trust: None,
                trusted_senders: None,
            },
        );
        Config {
            domain: "test.com".to_string(),
            data_dir: tmp.to_path_buf(),
            dkim_selector: "aimx".to_string(),
            trust: "none".to_string(),
            trusted_senders: vec![],
            mailboxes,
            verify_host: None,
            enable_ipv6: false,
        }
    }

    fn setup_config(config: &Config) -> crate::config::test_env::ConfigDirOverride {
        std::fs::create_dir_all(&config.data_dir).unwrap();
        let guard = crate::config::test_env::ConfigDirOverride::set(&config.data_dir);
        config.save(&crate::config::config_path()).unwrap();
        guard
    }

    fn create_test_email(dir: &std::path::Path, id: &str, meta: &InboundFrontmatter) {
        std::fs::create_dir_all(dir).unwrap();
        let toml_str = toml::to_string(meta).unwrap();
        let content = format!("+++\n{toml_str}+++\n\nThis is the body of email {id}.\n");
        std::fs::write(dir.join(format!("{id}.md")), content).unwrap();
    }

    fn sample_meta(id: &str, from: &str, subject: &str, read: bool) -> InboundFrontmatter {
        InboundFrontmatter {
            id: id.to_string(),
            message_id: format!("<{id}@test.com>"),
            thread_id: "0123456789abcdef".to_string(),
            from: from.to_string(),
            to: "alice@test.com".to_string(),
            cc: None,
            reply_to: None,
            delivered_to: "alice@test.com".to_string(),
            subject: subject.to_string(),
            date: "2025-06-01T12:00:00Z".to_string(),
            received_at: "2025-06-01T12:00:01Z".to_string(),
            received_from_ip: None,
            size_bytes: 100,
            in_reply_to: None,
            references: None,
            attachments: vec![],
            list_id: None,
            auto_submitted: None,
            dkim: "none".to_string(),
            spf: "none".to_string(),
            dmarc: "none".to_string(),
            trusted: "none".to_string(),
            mailbox: "alice".to_string(),
            read,
            labels: vec![],
        }
    }

    #[test]
    fn parse_frontmatter_valid() {
        let meta = sample_meta("2025-06-01-001", "sender@example.com", "Hello", false);
        let toml_str = toml::to_string(&meta).unwrap();
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

    fn inbox_of(tmp: &std::path::Path, name: &str) -> std::path::PathBuf {
        tmp.join("inbox").join(name)
    }

    #[test]
    fn set_read_status_marks_read() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(tmp.path());
        let _cfg_guard = setup_config(&config);

        let meta = sample_meta("2025-06-01-001", "sender@test.com", "Test", false);
        create_test_email(&inbox_of(tmp.path(), "alice"), "2025-06-01-001", &meta);

        set_read_status(&config, "alice", Folder::Inbox, "2025-06-01-001", true).unwrap();

        let content =
            std::fs::read_to_string(inbox_of(tmp.path(), "alice").join("2025-06-01-001.md"))
                .unwrap();
        let parsed = parse_frontmatter(&content).unwrap();
        assert!(parsed.read);
    }

    #[test]
    fn set_read_status_marks_unread() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(tmp.path());
        let _cfg_guard = setup_config(&config);

        let meta = sample_meta("2025-06-01-001", "sender@test.com", "Test", true);
        create_test_email(&inbox_of(tmp.path(), "alice"), "2025-06-01-001", &meta);

        set_read_status(&config, "alice", Folder::Inbox, "2025-06-01-001", false).unwrap();

        let content =
            std::fs::read_to_string(inbox_of(tmp.path(), "alice").join("2025-06-01-001.md"))
                .unwrap();
        let parsed = parse_frontmatter(&content).unwrap();
        assert!(!parsed.read);
    }

    #[test]
    fn set_read_status_nonexistent_mailbox() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(tmp.path());
        let _cfg_guard = setup_config(&config);

        let result = set_read_status(
            &config,
            "nonexistent",
            Folder::Inbox,
            "2025-06-01-001",
            true,
        );
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("does not exist"));
    }

    #[test]
    fn set_read_status_nonexistent_email() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(tmp.path());
        let _cfg_guard = setup_config(&config);

        std::fs::create_dir_all(inbox_of(tmp.path(), "alice")).unwrap();
        let result = set_read_status(&config, "alice", Folder::Inbox, "nonexistent", true);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("not found"));
    }

    #[test]
    fn set_read_status_reads_sent_folder_when_requested() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(tmp.path());
        let _cfg_guard = setup_config(&config);

        let meta = sample_meta("2025-06-02-001", "sender@test.com", "Outbound", false);
        let sent = tmp.path().join("sent").join("alice");
        create_test_email(&sent, "2025-06-02-001", &meta);

        set_read_status(&config, "alice", Folder::Sent, "2025-06-02-001", true).unwrap();

        let content = std::fs::read_to_string(sent.join("2025-06-02-001.md")).unwrap();
        let parsed = parse_frontmatter(&content).unwrap();
        assert!(parsed.read);
    }

    #[test]
    fn resolve_folder_default_is_inbox() {
        assert_eq!(resolve_folder(None).unwrap(), Folder::Inbox);
        assert_eq!(resolve_folder(Some("inbox")).unwrap(), Folder::Inbox);
    }

    #[test]
    fn resolve_folder_sent() {
        assert_eq!(resolve_folder(Some("sent")).unwrap(), Folder::Sent);
    }

    #[test]
    fn resolve_folder_rejects_unknown() {
        let err = resolve_folder(Some("drafts")).unwrap_err();
        assert!(err.contains("Invalid folder"));
        assert!(err.contains("drafts"));
    }

    #[test]
    fn list_emails_reads_bundle_markdown() {
        // Bundle `<stem>/<stem>.md` is indexed alongside flat `.md` files.
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("alice");
        std::fs::create_dir_all(&dir).unwrap();

        let meta_flat = sample_meta("2025-06-01-001-flat", "a@test.com", "Flat", false);
        create_test_email(&dir, "2025-06-01-001-flat", &meta_flat);

        let bundle_stem = "2025-06-01-002-bundle";
        let bundle = dir.join(bundle_stem);
        std::fs::create_dir_all(&bundle).unwrap();
        let meta_bundle = sample_meta(bundle_stem, "b@test.com", "Bundled", true);
        let toml_str = toml::to_string_pretty(&meta_bundle).unwrap();
        let content = format!("+++\n{toml_str}+++\n\nBundle body.\n");
        std::fs::write(bundle.join(format!("{bundle_stem}.md")), content).unwrap();
        std::fs::write(bundle.join("att.txt"), "attached").unwrap();

        let emails = list_emails(&dir).unwrap();
        assert_eq!(emails.len(), 2);
        let ids: Vec<&str> = emails.iter().map(|e| e.id.as_str()).collect();
        assert!(ids.contains(&"2025-06-01-001-flat"));
        assert!(ids.contains(&bundle_stem));
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
        let _cfg_guard = setup_config(&config);

        let meta1 = sample_meta("2025-06-01-001", "a@test.com", "A", false);
        let meta2 = sample_meta("2025-06-01-002", "b@test.com", "B", true);
        create_test_email(&inbox_of(tmp.path(), "alice"), "2025-06-01-001", &meta1);
        create_test_email(&inbox_of(tmp.path(), "alice"), "2025-06-01-002", &meta2);

        let result = list_mailboxes_with_unread(&config);
        let alice = result.iter().find(|m| m.0 == "alice").unwrap();
        assert_eq!(alice.1, 2); // total inbox
        assert_eq!(alice.2, 1); // unread
        assert_eq!(alice.3, 0); // sent count
        assert!(alice.4, "alice is registered in config");
    }

    #[test]
    fn list_mailboxes_with_unread_surfaces_unregistered_dir() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(tmp.path());
        let _cfg_guard = setup_config(&config);

        // Stray inbox dir created out-of-band (e.g. backup restore) with
        // no entry in `config.mailboxes`.
        let stray = inbox_of(tmp.path(), "stray");
        std::fs::create_dir_all(&stray).unwrap();
        let meta = sample_meta("2025-06-01-100", "x@test.com", "X", true);
        create_test_email(&stray, "2025-06-01-100", &meta);

        let result = list_mailboxes_with_unread(&config);
        let stray_row = result
            .iter()
            .find(|m| m.0 == "stray")
            .expect("stray dir must surface in mailbox_list");
        assert_eq!(stray_row.1, 1); // total inbox
        assert_eq!(stray_row.2, 0); // unread (meta.read=true)
        assert_eq!(stray_row.3, 0); // sent
        assert!(!stray_row.4, "stray is not registered in config");

        // catchall and alice still surface as registered.
        assert!(result.iter().any(|m| m.0 == "catchall" && m.4));
        assert!(result.iter().any(|m| m.0 == "alice" && m.4));
    }

    #[test]
    fn list_emails_nonexistent_dir() {
        let result = list_emails(std::path::Path::new("/nonexistent/path")).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn mcp_server_has_correct_tool_count() {
        let tmp = TempDir::new().unwrap();
        let server = AimxMcpServer::new(Some(tmp.path().to_path_buf()));
        let tools = server.tool_router.list_all();
        assert_eq!(tools.len(), 9);
    }

    #[test]
    fn mcp_server_tool_names() {
        let tmp = TempDir::new().unwrap();
        let server = AimxMcpServer::new(Some(tmp.path().to_path_buf()));
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
    fn parse_ack_response_accepts_bare_ok() {
        assert!(matches!(
            parse_ack_response(b"AIMX/1 OK\n"),
            MarkOutcome::Ok
        ));
        // Trailing CR is tolerated (line-end normalisation).
        assert!(matches!(
            parse_ack_response(b"AIMX/1 OK\r\n"),
            MarkOutcome::Ok
        ));
    }

    #[test]
    fn parse_ack_response_rejects_trailing_payload_on_ok() {
        // MARK verbs use a bare `AIMX/1 OK` ack; any trailing payload is
        // a protocol violation and must surface as Malformed, not Ok.
        match parse_ack_response(b"AIMX/1 OK extra junk\n") {
            MarkOutcome::Malformed(reason) => {
                assert!(
                    reason.contains("unexpected payload after OK"),
                    "reason was {reason:?}"
                );
            }
            other => panic!("expected Malformed, got {other:?}"),
        }
    }

    #[test]
    fn parse_ack_response_parses_err_code() {
        match parse_ack_response(b"AIMX/1 ERR NOTFOUND email missing\n") {
            MarkOutcome::Err { code, reason } => {
                assert_eq!(code, ErrCode::NotFound);
                assert_eq!(reason, "email missing");
            }
            other => panic!("expected Err, got {other:?}"),
        }
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
        let _cfg_guard = setup_config(&config);

        std::fs::create_dir_all(inbox_of(tmp.path(), "alice")).unwrap();
        let result = set_read_status(&config, "alice", Folder::Inbox, "../../etc/passwd", true);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("invalid characters"));
    }

    // ----- S48-6 NONEMPTY → CLI hint rewrite ---------------------------

    #[test]
    fn rewrite_nonempty_error_carries_counts_and_cli_command() {
        // Mirrors the daemon's actual reason format from
        // mailbox_handler.rs:handle_delete.
        let daemon = "[NONEMPTY] mailbox 'orders' has 7 files (5 in inbox, 2 in sent); \
                      archive or remove them first";
        let hint = rewrite_nonempty_error_for_mcp("orders", daemon);
        assert!(hint.contains("Cannot delete mailbox 'orders'"), "{hint}");
        assert!(hint.contains("inbox: 5 files"), "{hint}");
        assert!(hint.contains("sent: 2 files"), "{hint}");
        assert!(
            hint.contains("sudo aimx mailboxes delete --force orders"),
            "MCP hint must spell out the exact CLI command operators should run: {hint}"
        );
    }

    #[test]
    fn rewrite_nonempty_error_passes_non_nonempty_through() {
        // VALIDATION / IO / etc. errors should reach the agent verbatim.
        let other = "[VALIDATION] mailbox name contains invalid character '/'";
        assert_eq!(rewrite_nonempty_error_for_mcp("orders", other), other);
    }

    #[test]
    fn rewrite_nonempty_error_falls_back_to_zero_counts_when_format_unexpected() {
        // If the daemon ever changes its phrasing, the MCP hint must
        // still render rather than 500 — counts default to 0.
        let weird = "[NONEMPTY] mailbox has files";
        let hint = rewrite_nonempty_error_for_mcp("orders", weird);
        assert!(hint.contains("inbox: 0 files"), "{hint}");
        assert!(hint.contains("sent: 0 files"), "{hint}");
        assert!(
            hint.contains("aimx mailboxes delete --force orders"),
            "{hint}"
        );
    }
}
