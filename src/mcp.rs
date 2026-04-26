use crate::cli::SendArgs;
use crate::config::Config;
use crate::frontmatter::InboundFrontmatter;
use crate::mailbox;
use crate::send;
use crate::send_protocol::{
    self, ErrCode, HookCreateRequest, HookDeleteRequest, MailboxCrudRequest, MarkFolder,
    MarkRequest, SendRequest,
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
    /// Effective uid of the process running `aimx mcp`. Captured at
    /// `new()` and passed into `auth::authorize` for every tool call.
    /// Agents inherit the operator's uid via the launching shell, so
    /// this is the agent's authorization principal.
    caller_uid: u32,
    #[allow(dead_code)]
    tool_router: ToolRouter<Self>,
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

#[derive(Serialize, Deserialize, schemars::JsonSchema)]
pub struct HookCreateParams {
    #[schemars(description = "Mailbox name to attach the hook to. Must be a \
                              mailbox you own.")]
    pub mailbox: String,
    #[schemars(description = "Event that triggers the hook. One of: \"on_receive\", \
                       \"after_send\".")]
    pub event: String,
    #[schemars(description = "Argv array exec'd when the hook fires. cmd[0] must \
                       be an absolute path; there is no shell wrapping. \
                       Spell out [\"/bin/sh\", \"-c\", \"...\"] explicitly when \
                       shell expansion is needed.")]
    pub cmd: Vec<String>,
    #[schemars(description = "Optional explicit hook name. When omitted, a stable \
                       12-char hex name is derived from (event, cmd, \
                       fire_on_untrusted).")]
    pub name: Option<String>,
    #[schemars(description = "Opt into firing on inbound emails the trust gate \
                       marks as not trusted. Only valid on event = \
                       \"on_receive\".")]
    pub fire_on_untrusted: Option<bool>,
}

#[derive(Serialize, Deserialize, schemars::JsonSchema)]
pub struct HookListParams {
    #[schemars(description = "Optional mailbox filter. When set, only hooks on \
                       this mailbox are listed. When omitted, lists hooks \
                       for every mailbox you own.")]
    pub mailbox: Option<String>,
}

#[derive(Serialize, Deserialize, schemars::JsonSchema)]
pub struct HookDeleteParams {
    #[schemars(description = "Hook name (explicit or derived). Only hooks on \
                       mailboxes you own can be deleted.")]
    pub name: String,
}

impl AimxMcpServer {
    pub fn new(data_dir_override: Option<PathBuf>) -> Self {
        Self::with_caller_uid(data_dir_override, crate::platform::current_euid())
    }

    /// Test-only constructor that lets the caller pin the authorization
    /// principal. Production code calls `new()`, which derives the uid
    /// from `geteuid()` at startup.
    #[cfg(test)]
    pub fn with_caller_uid_for_test(data_dir_override: Option<PathBuf>, caller_uid: u32) -> Self {
        Self::with_caller_uid(data_dir_override, caller_uid)
    }

    fn with_caller_uid(data_dir_override: Option<PathBuf>, caller_uid: u32) -> Self {
        Self {
            data_dir_override,
            caller_uid,
            tool_router: Self::tool_router(),
        }
    }

    fn load_config(&self) -> Result<Config, String> {
        Config::load_resolved_with_data_dir(self.data_dir_override.as_deref())
            .map(|(cfg, _warnings)| cfg)
            .map_err(|e| format!("Failed to load config: {e}"))
    }

    /// Return the auth predicate's verdict for `action` against the
    /// named mailbox. Mirrors the daemon-side helper so the auth gate
    /// runs the same way through CLI, daemon UDS, and MCP. The MCP
    /// surface returns errors as `String` per `rmcp` conventions.
    fn authorize_mailbox(
        &self,
        action: crate::auth::Action,
        config: &Config,
    ) -> Result<(), String> {
        let mailbox_name = match &action {
            crate::auth::Action::MailboxRead(n)
            | crate::auth::Action::MailboxSendAs(n)
            | crate::auth::Action::MarkReadWrite(n)
            | crate::auth::Action::HookCrud(n) => n.clone(),
            crate::auth::Action::MailboxCrud | crate::auth::Action::SystemCommand => String::new(),
        };
        let mb = if mailbox_name.is_empty() {
            None
        } else {
            config.mailboxes.get(&mailbox_name)
        };
        crate::auth::authorize(self.caller_uid, action, mb).map_err(|e| match e {
            crate::auth::AuthError::NotRoot => "not authorized: requires root".to_string(),
            crate::auth::AuthError::NotOwner { mailbox } => {
                format!("not authorized: caller does not own mailbox '{mailbox}'")
            }
            crate::auth::AuthError::NoSuchMailbox => {
                format!("not authorized: mailbox '{mailbox_name}' not found")
            }
        })
    }
}

#[tool_router]
impl AimxMcpServer {
    #[tool(
        name = "mailbox_list",
        description = "List mailboxes the caller owns, with message counts. \
                       Mailboxes the caller does not own are absent — root sees all."
    )]
    fn mailbox_list(&self) -> Result<String, String> {
        let config = self.load_config()?;
        let mailboxes = list_mailboxes_with_unread(&config);

        // Filter to caller-owned mailboxes for non-root callers.
        // Mailboxes the caller doesn't own are simply absent from the
        // output rather than returned with a "denied" tag. Root sees
        // the full set.
        let filtered: Vec<_> = if self.caller_uid == 0 {
            mailboxes
        } else {
            mailboxes
                .into_iter()
                .filter(|(name, _, _, _, _)| mailbox::caller_owns(&config, name, self.caller_uid))
                .collect()
        };

        if filtered.is_empty() {
            return Ok("No mailboxes configured.".to_string());
        }

        let result: Vec<String> = filtered
            .iter()
            .map(|(name, total, unread, sent_count, registered)| {
                let suffix = if *registered { "" } else { " (unregistered)" };
                format!("{name}: {total} messages ({unread} unread), {sent_count} sent{suffix}")
            })
            .collect();
        Ok(result.join("\n"))
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

        // Point operations on mailboxes the caller doesn't own return
        // the canonical "not authorized" error. This is distinct from
        // `mailbox_list`, which silently filters — point ops surface
        // an explicit failure so the agent doesn't loop on an empty
        // list it cannot interpret.
        self.authorize_mailbox(
            crate::auth::Action::MailboxRead(params.mailbox.clone()),
            &config,
        )?;

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
        self.authorize_mailbox(
            crate::auth::Action::MailboxRead(params.mailbox.clone()),
            &config,
        )?;

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
        let config = self.load_config()?;
        self.authorize_mailbox(
            crate::auth::Action::MarkReadWrite(params.mailbox.clone()),
            &config,
        )?;
        let folder = resolve_folder(params.folder.as_deref())?;
        // Route through the daemon; mailbox files are root-owned and
        // the MCP process runs as the invoking user. The daemon
        // re-checks via SO_PEERCRED, so MCP's pre-flight authz is
        // defense in depth, not the security boundary.
        submit_mark_via_daemon(&params.mailbox, &params.id, folder, true)?;
        Ok(format!("Email '{}' marked as read.", params.id))
    }

    #[tool(name = "email_mark_unread", description = "Mark an email as unread")]
    fn email_mark_unread(
        &self,
        Parameters(params): Parameters<EmailMarkParams>,
    ) -> Result<String, String> {
        validate_email_id(&params.id)?;
        let config = self.load_config()?;
        self.authorize_mailbox(
            crate::auth::Action::MarkReadWrite(params.mailbox.clone()),
            &config,
        )?;
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
        self.authorize_mailbox(
            crate::auth::Action::MailboxSendAs(params.from_mailbox.clone()),
            &config,
        )?;

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
        // `email_reply` reads the parent and submits a new outbound
        // message — both surfaces are scoped to caller-owned mailboxes.
        self.authorize_mailbox(
            crate::auth::Action::MailboxSendAs(params.mailbox.clone()),
            &config,
        )?;

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

    #[tool(
        name = "hook_create",
        description = "Create a hook on a mailbox you own. The hook runs \
                       as your Linux uid (the mailbox owner) when the \
                       configured event fires. Routes through the daemon \
                       UDS so the running config hot-swaps without a \
                       restart."
    )]
    fn hook_create(
        &self,
        Parameters(params): Parameters<HookCreateParams>,
    ) -> Result<String, String> {
        let config = self.load_config()?;
        if !config.mailboxes.contains_key(&params.mailbox) {
            return Err(format!("Mailbox '{}' does not exist.", params.mailbox));
        }
        // Authorize against the central predicate before any wire I/O so
        // a non-owner sees the canonical "not authorized" error rather
        // than the daemon's opaque rejection text.
        self.authorize_mailbox(
            crate::auth::Action::HookCrud(params.mailbox.clone()),
            &config,
        )?;

        if params.cmd.is_empty() {
            return Err("cmd must not be empty".to_string());
        }
        let fire_on_untrusted = params.fire_on_untrusted.unwrap_or(false);

        let body = serde_json::json!({
            "cmd": params.cmd,
            "fire_on_untrusted": fire_on_untrusted,
            "type": "cmd",
        });
        let body_bytes =
            serde_json::to_vec(&body).map_err(|e| format!("Failed to serialize hook body: {e}"))?;

        match submit_hook_create_via_daemon(
            &params.mailbox,
            &params.event,
            params.name.as_deref(),
            body_bytes,
        ) {
            Ok(()) => Ok(format!("Hook created on mailbox '{}'.", params.mailbox,)),
            Err(HookCrudFallback::SocketMissing) => {
                Err("aimx daemon not running. Start with 'sudo systemctl start aimx'".to_string())
            }
            Err(HookCrudFallback::Daemon(msg)) => Err(msg),
        }
    }

    #[tool(
        name = "hook_list",
        description = "List hooks on mailboxes you own. Pass `mailbox` to \
                       filter to a single mailbox you own; omit to list \
                       every hook on every mailbox you own."
    )]
    fn hook_list(&self, Parameters(params): Parameters<HookListParams>) -> Result<String, String> {
        let config = self.load_config()?;

        if let Some(name) = &params.mailbox {
            if !config.mailboxes.contains_key(name) {
                return Err(format!("Mailbox '{name}' does not exist."));
            }
            self.authorize_mailbox(crate::auth::Action::HookCrud(name.clone()), &config)?;
        }

        let mut rows: Vec<serde_json::Value> = Vec::new();
        for (mailbox_name, mb) in &config.mailboxes {
            // Filter by --mailbox, or by caller ownership for non-root.
            if let Some(f) = &params.mailbox
                && f != mailbox_name
            {
                continue;
            }
            if self.caller_uid != 0 && !mailbox::caller_owns(&config, mailbox_name, self.caller_uid)
            {
                continue;
            }
            for hook in &mb.hooks {
                rows.push(serde_json::json!({
                    "name": crate::hook::effective_hook_name(hook),
                    "mailbox": mailbox_name,
                    "event": hook.event.as_str(),
                    "cmd": hook.cmd,
                    "fire_on_untrusted": hook.fire_on_untrusted,
                }));
            }
        }
        rows.sort_by(|a, b| {
            a["mailbox"]
                .as_str()
                .unwrap_or("")
                .cmp(b["mailbox"].as_str().unwrap_or(""))
                .then_with(|| {
                    a["event"]
                        .as_str()
                        .unwrap_or("")
                        .cmp(b["event"].as_str().unwrap_or(""))
                })
                .then_with(|| {
                    a["name"]
                        .as_str()
                        .unwrap_or("")
                        .cmp(b["name"].as_str().unwrap_or(""))
                })
        });

        serde_json::to_string(&rows).map_err(|e| format!("Failed to serialize: {e}"))
    }

    #[tool(
        name = "hook_delete",
        description = "Delete a hook by name. Only hooks on mailboxes you \
                       own can be deleted."
    )]
    fn hook_delete(
        &self,
        Parameters(params): Parameters<HookDeleteParams>,
    ) -> Result<String, String> {
        // Resolve the hook to its mailbox so the auth predicate runs
        // against the right principal. Hidden mailboxes (owned by other
        // users) surface as "hook not found" — we deliberately do not
        // distinguish "exists but you don't own it" from "doesn't exist"
        // to avoid leaking ownership.
        let config = self.load_config()?;
        let mailbox_name = config
            .mailboxes
            .iter()
            .find_map(|(mb_name, mb)| {
                mb.hooks
                    .iter()
                    .find(|h| crate::hook::effective_hook_name(h) == params.name)
                    .map(|_| mb_name.clone())
            })
            .ok_or_else(|| format!("Hook '{}' not found", params.name))?;

        self.authorize_mailbox(crate::auth::Action::HookCrud(mailbox_name.clone()), &config)?;

        match submit_hook_delete_via_daemon(&params.name) {
            Ok(()) => Ok(format!("Hook '{}' deleted.", params.name)),
            Err(HookCrudFallback::SocketMissing) => {
                Err("aimx daemon not running. Start with 'sudo systemctl start aimx'".to_string())
            }
            Err(HookCrudFallback::Daemon(msg)) => Err(msg),
        }
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

/// Outcome of a `HOOK-CREATE` / `HOOK-DELETE` UDS submission. Mirrors
/// [`MailboxCrudFallback`] so callers can decide whether to fall back to
/// a direct on-disk edit (only when the caller is root) or surface the
/// daemon's reason verbatim.
pub(crate) enum HookCrudFallback {
    /// Socket not present / not connectable. Callers fall back to
    /// direct config.toml edit when running as root, error otherwise.
    SocketMissing,
    /// Daemon connected and answered but reported an error. Caller
    /// should surface this verbatim — includes ERR EACCES (not owner)
    /// and ERR ENOENT (no such hook / mailbox).
    Daemon(String),
}

/// Submit an `AIMX/1 HOOK-CREATE` to the daemon. The caller supplies the
/// JSON body (`{"cmd": [...], "fire_on_untrusted": <bool>, "type": "cmd"}`)
/// matching the daemon-side `HookCreateBody` shape.
pub(crate) fn submit_hook_create_via_daemon(
    mailbox: &str,
    event: &str,
    name: Option<&str>,
    body: Vec<u8>,
) -> Result<(), HookCrudFallback> {
    let request = HookCreateRequest {
        mailbox: mailbox.to_string(),
        event: event.to_string(),
        name: name.map(|s| s.to_string()),
        body,
    };
    let socket = crate::serve::aimx_socket_path();

    let rt = tokio::runtime::Handle::try_current();
    let io_result: Result<MarkOutcome, std::io::Error> = match rt {
        Ok(handle) => tokio::task::block_in_place(|| {
            handle.block_on(submit_hook_create_request(&socket, &request))
        }),
        Err(_) => {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|e| {
                    HookCrudFallback::Daemon(format!("Failed to create tokio runtime: {e}"))
                })?;
            rt.block_on(submit_hook_create_request(&socket, &request))
        }
    };

    map_hook_io_result(io_result, &socket)
}

/// Submit an `AIMX/1 HOOK-DELETE` to the daemon by effective name.
pub(crate) fn submit_hook_delete_via_daemon(name: &str) -> Result<(), HookCrudFallback> {
    let request = HookDeleteRequest {
        name: name.to_string(),
    };
    let socket = crate::serve::aimx_socket_path();

    let rt = tokio::runtime::Handle::try_current();
    let io_result: Result<MarkOutcome, std::io::Error> = match rt {
        Ok(handle) => tokio::task::block_in_place(|| {
            handle.block_on(submit_hook_delete_request(&socket, &request))
        }),
        Err(_) => {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|e| {
                    HookCrudFallback::Daemon(format!("Failed to create tokio runtime: {e}"))
                })?;
            rt.block_on(submit_hook_delete_request(&socket, &request))
        }
    };

    map_hook_io_result(io_result, &socket)
}

fn map_hook_io_result(
    io_result: Result<MarkOutcome, std::io::Error>,
    socket: &std::path::Path,
) -> Result<(), HookCrudFallback> {
    match io_result {
        Ok(MarkOutcome::Ok) => Ok(()),
        Ok(MarkOutcome::Err { code, reason }) => Err(HookCrudFallback::Daemon(format!(
            "[{}] {reason}",
            code.as_str()
        ))),
        Ok(MarkOutcome::Malformed(reason)) => Err(HookCrudFallback::Daemon(format!(
            "Malformed response from aimx daemon: {reason}"
        ))),
        Err(e) => {
            if is_socket_missing(&e) {
                Err(HookCrudFallback::SocketMissing)
            } else {
                Err(HookCrudFallback::Daemon(format!(
                    "Failed to connect to aimx daemon at {}: {e}",
                    socket.display()
                )))
            }
        }
    }
}

async fn submit_hook_create_request(
    socket_path: &std::path::Path,
    request: &HookCreateRequest,
) -> Result<MarkOutcome, std::io::Error> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let stream = tokio::net::UnixStream::connect(socket_path).await?;
    let (mut reader, mut writer) = stream.into_split();

    send_protocol::write_hook_create_request(&mut writer, request).await?;
    writer.shutdown().await.ok();

    let mut buf = Vec::with_capacity(128);
    reader.read_to_end(&mut buf).await?;

    Ok(parse_ack_response(&buf))
}

async fn submit_hook_delete_request(
    socket_path: &std::path::Path,
    request: &HookDeleteRequest,
) -> Result<MarkOutcome, std::io::Error> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let stream = tokio::net::UnixStream::connect(socket_path).await?;
    let (mut reader, mut writer) = stream.into_split();

    send_protocol::write_hook_delete_request(&mut writer, request).await?;
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

#[cfg(test)]
mod auth_tests {
    use super::*;
    use crate::auth::Action;
    use crate::config::MailboxConfig;
    use std::collections::HashMap;
    use tempfile::TempDir;

    /// Build a minimal in-memory `Config` whose `data_dir` points at a
    /// caller-supplied tempdir and whose mailboxes carry the named
    /// `(name, owner)` entries. Owners that are valid usernames on the
    /// host (`root` always is) flow through `owner_uid()`; orphan
    /// owners surface via the auth predicate's `NoSuchMailbox` arm
    /// (the predicate hides the orphan / ownership distinction).
    fn build_config(tmp: &std::path::Path, owners: &[(&str, &str)]) -> Config {
        let mut mailboxes = HashMap::new();
        for (name, owner) in owners {
            mailboxes.insert(
                (*name).into(),
                MailboxConfig {
                    address: format!("{name}@agent.example.com"),
                    owner: (*owner).into(),
                    hooks: vec![],
                    trust: None,
                    trusted_senders: None,
                    allow_root_catchall: false,
                },
            );
        }
        Config {
            domain: "agent.example.com".into(),
            data_dir: tmp.to_path_buf(),
            dkim_selector: "aimx".into(),
            trust: "none".into(),
            trusted_senders: vec![],
            mailboxes,
            verify_host: None,
            enable_ipv6: false,
            upgrade: None,
        }
    }

    #[test]
    fn root_caller_passes_authorize_mailbox_for_any_action() {
        let tmp = TempDir::new().unwrap();
        let config = build_config(tmp.path(), &[("alice", "root")]);
        let server = AimxMcpServer::with_caller_uid_for_test(None, 0);

        assert!(
            server
                .authorize_mailbox(Action::MailboxRead("alice".into()), &config)
                .is_ok()
        );
        assert!(
            server
                .authorize_mailbox(Action::MailboxSendAs("alice".into()), &config)
                .is_ok()
        );
        assert!(
            server
                .authorize_mailbox(Action::MarkReadWrite("alice".into()), &config)
                .is_ok()
        );
    }

    #[test]
    fn non_root_caller_rejected_when_mailbox_owner_does_not_resolve() {
        let tmp = TempDir::new().unwrap();
        let config = build_config(tmp.path(), &[("alice", "aimx-nonexistent-orphan-user")]);
        // Pick a uid that's almost certainly not 0 and not root —
        // exact value is irrelevant since the orphan-owner branch
        // collapses to NoSuchMailbox before the uid match runs.
        let server = AimxMcpServer::with_caller_uid_for_test(None, 1000);

        let err = server
            .authorize_mailbox(Action::MailboxRead("alice".into()), &config)
            .unwrap_err();
        // The auth predicate hides "orphan owner" vs. "wrong owner";
        // both surface as NoSuchMailbox to non-root callers.
        assert!(
            err.contains("not authorized"),
            "expected canonical not-authorized prefix: {err}"
        );
    }

    #[test]
    fn non_root_caller_rejected_when_mailbox_owned_by_root() {
        // Caller uid 1000 vs mailbox owner uid 0 → NotOwner.
        let tmp = TempDir::new().unwrap();
        let config = build_config(tmp.path(), &[("admin", "root")]);
        let server = AimxMcpServer::with_caller_uid_for_test(None, 1000);

        let err = server
            .authorize_mailbox(Action::MailboxRead("admin".into()), &config)
            .unwrap_err();
        assert!(err.contains("not authorized"), "{err}");
        assert!(err.contains("admin"), "{err}");
    }

    #[test]
    fn missing_mailbox_returns_not_authorized_not_found_for_non_root() {
        let tmp = TempDir::new().unwrap();
        let config = build_config(tmp.path(), &[]);
        let server = AimxMcpServer::with_caller_uid_for_test(None, 1000);

        let err = server
            .authorize_mailbox(Action::MailboxRead("missing".into()), &config)
            .unwrap_err();
        assert!(err.contains("not authorized"), "{err}");
    }
}
