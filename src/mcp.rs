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
    #[schemars(description = "Linux user that owns this mailbox's storage under \
                       /var/lib/aimx/{inbox,sent}/<name>/. Defaults to the \
                       mailbox name. Must resolve via getpwnam.")]
    pub owner: Option<String>,
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
    #[schemars(
        description = "Message-ID of the email being replied to (sets In-Reply-To header for threading). \
                       Required to enable threading: without reply_to, the references field is silently ignored and no threading headers are emitted. \
                       When set, References is built automatically unless overridden by the references field."
    )]
    pub reply_to: Option<String>,
    #[schemars(
        description = "Full References header chain (space-separated Message-IDs) for threading. \
                       Only applied when reply_to is also set. Supplied alone, it is silently ignored."
    )]
    pub references: Option<String>,
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
            .map(|(cfg, _warnings)| cfg)
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
        // Prefer the daemon UDS path. The daemon atomically rewrites
        // config.toml and hot-swaps its in-memory Config so a following
        // inbound mail routes correctly with no restart. Fall back to
        // direct on-disk edit only when the socket isn't reachable
        // (daemon stopped, first-time setup, etc.).
        let owner_val = params.owner.clone().unwrap_or_else(|| params.name.clone());
        match submit_mailbox_crud_via_daemon(&params.name, true, Some(&owner_val)) {
            Ok(()) => Ok(format!("Mailbox '{}' created successfully.", params.name)),
            Err(MailboxCrudFallback::SocketMissing) => {
                let config = self.load_config()?;
                mailbox::create_mailbox(&config, &params.name, &owner_val)
                    .map_err(|e| e.to_string())?;
                Ok(format!(
                    "Mailbox '{}' created successfully (daemon not running. Restart aimx to apply the change).",
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
        // does NOT gain a force variant. Destructive wipes stay on the
        // CLI where operators see prompts and can't be triggered
        // remotely by an agent. On NONEMPTY we rewrite the daemon's
        // error into a structured hint that names the exact CLI command
        // The fallback direct-on-disk path runs only when the
        // daemon is unreachable.
        match submit_mailbox_crud_via_daemon(&params.name, false, None) {
            Ok(()) => Ok(format!(
                "Mailbox '{0}' deleted. Empty `inbox/{0}/` and `sent/{0}/` \
                 directories remain on disk. Run `rmdir` to tidy up if desired.",
                params.name
            )),
            Err(MailboxCrudFallback::SocketMissing) => {
                let config = self.load_config()?;
                mailbox::delete_mailbox(&config, &params.name).map_err(|e| e.to_string())?;
                Ok(format!(
                    "Mailbox '{}' deleted (daemon not running. Restart aimx to apply the change).",
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
        // Read paths audit — `email_read` goes through
        // the strict resolver so a planted symlink or escape path
        // cannot exfiltrate another mailbox's mail via `email_read`,
        // not just via the MARK verbs.
        let filepath = resolve_email_path_strict(&mailbox_dir, &params.id).ok_or_else(|| {
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
        // Route through the daemon; mailbox files are root-owned and
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

        let args = build_send_args(params, from_address);

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
        // `email_reply` reads the parent message to
        // inherit threading headers; route through the strict resolver
        // so a symlink cannot leak another mailbox's message into a
        // reply composition.
        let filepath = resolve_email_path_strict(&mailbox_dir, &params.id).ok_or_else(|| {
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

/// Build `SendArgs` from `EmailSendParams` and the resolved sender
/// address. Pure and side-effect-free so it can be unit-tested without
/// the daemon; keeps the `params.reply_to` / `params.references`
/// forwarding explicit (see the deserialization tests below).
fn build_send_args(params: EmailSendParams, from_address: &str) -> SendArgs {
    SendArgs {
        from: from_address.to_string(),
        to: params.to,
        subject: params.subject,
        body: params.body,
        reply_to: params.reply_to,
        references: params.references,
        attachments: params.attachments.unwrap_or_default(),
    }
}

/// Compose `args` into an `AIMX/1 SEND` request and submit it to
/// `aimx serve` over the UDS. MCP, like `aimx send`, does not sign or
/// deliver mail directly. Everything goes through the daemon. The
/// request frame carries no `From-Mailbox:` header; the daemon parses
/// `From:` out of the composed body itself and resolves the sender
/// mailbox against its in-memory Config.
fn submit_via_daemon(args: &SendArgs) -> Result<String, String> {
    let composed = send::compose_message(args).map_err(|e| e.to_string())?;
    let request = SendRequest {
        body: composed.message,
    };
    let socket = crate::serve::aimx_socket_path();

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
            "aimx daemon not running. Start with 'sudo systemctl start aimx'".to_string()
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
    let socket = crate::serve::aimx_socket_path();

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
            "aimx daemon not running. Start with 'sudo systemctl start aimx'".to_string()
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
pub(crate) enum MarkOutcome {
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
/// `owner` is required on CREATE; ignored on DELETE.
pub(crate) fn submit_mailbox_crud_via_daemon(
    name: &str,
    create: bool,
    owner: Option<&str>,
) -> Result<(), MailboxCrudFallback> {
    let request = MailboxCrudRequest {
        name: name.to_string(),
        create,
        owner: owner.map(|s| s.to_string()),
    };
    let socket = crate::serve::aimx_socket_path();

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
pub(crate) fn is_socket_missing(e: &std::io::Error) -> bool {
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

pub(crate) fn parse_ack_response(buf: &[u8]) -> MarkOutcome {
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
    // MARK verbs use a bare `AIMX/1 OK` ack. Any trailing payload is a
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
        // Prefer the structured `Code:` header if the daemon
        // emitted one, fall back to the legacy inline token.
        let header = parse_ack_code_header(text);
        let code = match (header, ErrCode::from_str(code_str)) {
            (Some(h), _) => h,
            (None, Some(inline)) => inline,
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

fn parse_ack_code_header(text: &str) -> Option<ErrCode> {
    for line in text.lines().skip(1) {
        let line = line.trim_end_matches('\r');
        if line.is_empty() {
            break;
        }
        if let Some(v) = line.strip_prefix("Code: ") {
            return ErrCode::from_str(v.trim());
        }
        if let Some(v) = line.strip_prefix("Code:") {
            return ErrCode::from_str(v.trim());
        }
    }
    None
}

#[tool_handler]
impl ServerHandler for AimxMcpServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_instructions("aimx email server. Manage mailboxes and emails for AI agents")
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
/// the daemon knows about: both registered ones in `config.mailboxes` and
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

/// Strict email-path resolver used by every read/rewrite verb in the
/// daemon. Returns `Some(path)` only when every condition holds:
///
/// - The candidate exists as a regular file (flat `<id>.md` or bundle
///   inner `<id>/<id>.md`).
/// - Neither the candidate nor (for the bundle case) the bundle
///   directory is a symlink. `symlink_metadata` is used so symlinks are
///   never followed during the check.
/// - The canonicalized candidate path is a descendant of the
///   canonicalized `mailbox_dir`. This defends against `..`-style
///   escapes even if upstream validation is bypassed.
///
/// Rejections return `None` (not an error) so upstream handlers keep
/// emitting the existing `NotFound` shape — a differentiated error
/// would leak whether the target exists in a sibling mailbox
/// (PRD §6.1).
///
/// Duplicate layouts (a flat `<id>.md` symlink AND a sibling bundle
/// `<id>/<id>.md`) are a tampering signal and also reject: the strict
/// resolver never silently falls back from a rejected flat candidate
/// to the bundle form.
pub fn resolve_email_path_strict(mailbox_dir: &std::path::Path, id: &str) -> Option<PathBuf> {
    let mailbox_canon = std::fs::canonicalize(mailbox_dir).ok()?;

    let flat = mailbox_dir.join(format!("{id}.md"));
    match std::fs::symlink_metadata(&flat) {
        Ok(md) => {
            // Flat candidate exists on disk. Reject any non-regular
            // entry (symlink, dir, device, fifo). If the flat candidate
            // is tampered with, we do NOT fall back to the bundle form
            // — the duplicate itself is suspicious.
            if !md.file_type().is_file() {
                return None;
            }
            let candidate = std::fs::canonicalize(&flat).ok()?;
            if !candidate.starts_with(&mailbox_canon) {
                return None;
            }
            return Some(flat);
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            // Fall through to bundle lookup.
        }
        Err(_) => return None,
    }

    let bundle_dir = mailbox_dir.join(id);
    match std::fs::symlink_metadata(&bundle_dir) {
        Ok(md) => {
            if !md.file_type().is_dir() {
                return None;
            }
        }
        Err(_) => return None,
    }
    let bundle_md = bundle_dir.join(format!("{id}.md"));
    match std::fs::symlink_metadata(&bundle_md) {
        Ok(md) => {
            if !md.file_type().is_file() {
                return None;
            }
        }
        Err(_) => return None,
    }
    let candidate = std::fs::canonicalize(&bundle_md).ok()?;
    if !candidate.starts_with(&mailbox_canon) {
        return None;
    }
    Some(bundle_md)
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
/// handlers themselves. They route through the daemon via UDS so the
/// non-root MCP process doesn't need write access to root-owned mailbox
/// files. Retained for unit-test coverage of the frontmatter
/// read/rewrite flow; exercised at the protocol level by
/// `state_handler::tests` and at the MCP level by integration tests.
#[cfg(test)]
#[allow(dead_code)]
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
    let filepath = resolve_email_path_strict(&mailbox_dir, id)
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
/// deliberately does not gain a force variant. Destructive wipes stay
/// on the CLI where the operator sees prompts and the request can't be
/// triggered remotely. Non-NONEMPTY daemon errors pass through verbatim.
pub(crate) fn rewrite_nonempty_error_for_mcp(name: &str, msg: &str) -> String {
    if !msg.contains("[NONEMPTY]") {
        return msg.to_string();
    }
    let (inbox_files, sent_files) = parse_nonempty_counts(msg);
    format!(
        "Cannot delete mailbox '{name}'. inbox: {inbox_files} files, sent: {sent_files} files. \
         MCP `mailbox_delete` does not wipe mail. Run `sudo aimx mailboxes delete --force {name}` \
         on the host to wipe and remove."
    )
}

/// Parse `(N in inbox, M in sent)` out of the daemon's NONEMPTY reason
/// string. Returns `(0, 0)` when the format doesn't match; the caller
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
