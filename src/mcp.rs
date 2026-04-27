use crate::cli::SendArgs;
use crate::config::Config;
use crate::frontmatter::InboundFrontmatter;
use crate::mailbox;
use crate::send;
use crate::send_protocol::{
    self, ErrCode, HookCreateRequest, HookDeleteRequest, MailboxCrudRequest, MarkRequest,
    SendRequest,
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
    #[schemars(description = "Maximum number of rows to return. Default 50; \
                              values above 200 are silently clamped to 200.")]
    pub limit: Option<u32>,
    #[schemars(description = "Number of rows to skip from the start of the \
                              descending-by-filename listing. Default 0.")]
    pub offset: Option<u32>,
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
#[serde(deny_unknown_fields)]
pub struct EmailMarkParams {
    #[schemars(description = "Mailbox name")]
    pub mailbox: String,
    #[schemars(description = "Email ID (e.g. 2025-06-15-120000-hello)")]
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
    #[schemars(
        description = "Message-ID of the email being replied to (sets In-Reply-To header for threading). \
                       Required to enable threading: without reply_to, the references field is silently ignored and no threading headers are emitted. \
                       When set, References is built automatically unless overridden by the references field."
    )]
    pub reply_to: Option<String>,
    #[schemars(description = "Email body text")]
    pub body: String,
    #[schemars(description = "File paths to attach")]
    pub attachments: Option<Vec<String>>,
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
    #[schemars(description = "Hard subprocess timeout in seconds. Default 60, \
                       max 600. SIGTERM at the limit, SIGKILL 5s later.")]
    pub timeout_secs: Option<u32>,
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
        // The daemon is the single source of truth: it resolves the
        // caller via `SO_PEERCRED` and returns a JSON array of
        // mailboxes the uid owns. The MCP process never reads
        // root-owned `config.toml` and never runs its own authz
        // pre-flight — there are no mailboxes to authorize against
        // until the daemon answers.
        submit_mailbox_list_via_daemon()
    }

    #[tool(
        name = "email_list",
        description = "List emails in a mailbox, paginated by descending filename. \
                       Returns a JSON array; agents filter client-side."
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

        let limit = clamp_limit(params.limit);
        let offset = params.offset.unwrap_or(0) as usize;

        list_email_page_json(&mailbox_dir, folder, offset, limit).map_err(|e| e.to_string())
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

    #[tool(
        name = "email_mark_read",
        description = "Mark an inbox email as read. Sent-mail mark has no \
                       agent use case and is not supported."
    )]
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
        // Route through the daemon; mailbox files are root-owned and
        // the MCP process runs as the invoking user. The daemon
        // re-checks via SO_PEERCRED, so MCP's pre-flight authz is
        // defense in depth, not the security boundary.
        submit_mark_via_daemon(&params.mailbox, &params.id, true)?;
        Ok(format!("Email '{}' marked as read.", params.id))
    }

    #[tool(
        name = "email_mark_unread",
        description = "Mark an inbox email as unread. Sent-mail mark has no \
                       agent use case and is not supported."
    )]
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
        submit_mark_via_daemon(&params.mailbox, &params.id, false)?;
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

        let mut body = serde_json::json!({
            "cmd": params.cmd,
            "fire_on_untrusted": fire_on_untrusted,
            "type": "cmd",
        });
        if let Some(t) = params.timeout_secs {
            body["timeout_secs"] = serde_json::Value::Number(t.into());
        }
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
                    "timeout_secs": hook.timeout_secs,
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
        // to avoid leaking ownership of foreign mailboxes. The UDS wire
        // already returns canonical opaque text on auth failures; this
        // collapse keeps the MCP surface from leaking more than the wire.
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

        // If authorization fails (caller is not the mailbox's owner and
        // is not root), surface as `not found` rather than leaking the
        // foreign mailbox's name through a "caller does not own
        // mailbox 'X'" error.
        if self
            .authorize_mailbox(crate::auth::Action::HookCrud(mailbox_name.clone()), &config)
            .is_err()
        {
            return Err(format!("Hook '{}' not found", params.name));
        }

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
/// root-owned mailbox files. Targets the inbox unconditionally; the
/// sent folder has no agent use case.
fn submit_mark_via_daemon(mailbox: &str, id: &str, read: bool) -> Result<(), String> {
    let request = MarkRequest {
        mailbox: mailbox.to_string(),
        id: id.to_string(),
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

/// Submit an `AIMX/1 MAILBOX-LIST` to the daemon and return the JSON
/// body verbatim. The schema-mandated string output for the tool is
/// the JSON itself; no re-formatting happens here.
fn submit_mailbox_list_via_daemon() -> Result<String, String> {
    let socket = crate::serve::aimx_socket_path();

    let rt = tokio::runtime::Handle::try_current();
    let io_result: Result<Vec<u8>, std::io::Error> = match rt {
        Ok(handle) => {
            tokio::task::block_in_place(|| handle.block_on(submit_mailbox_list_request(&socket)))
        }
        Err(_) => {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|e| format!("Failed to create tokio runtime: {e}"))?;
            rt.block_on(submit_mailbox_list_request(&socket))
        }
    };

    let raw = io_result.map_err(|e| {
        if is_socket_missing(&e) {
            "aimx daemon not running. Start with 'sudo systemctl start aimx'".to_string()
        } else {
            format!(
                "Failed to connect to aimx daemon at {}: {e}",
                socket.display()
            )
        }
    })?;

    decode_mailbox_list_response(&raw)
}

/// Open the UDS, ship `AIMX/1 MAILBOX-LIST`, and return the raw
/// daemon response bytes. Parsing happens in
/// [`decode_mailbox_list_response`] so the I/O and codec layers stay
/// independently testable.
async fn submit_mailbox_list_request(
    socket_path: &std::path::Path,
) -> Result<Vec<u8>, std::io::Error> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let stream = tokio::net::UnixStream::connect(socket_path).await?;
    let (mut reader, mut writer) = stream.into_split();

    send_protocol::write_mailbox_list_request(&mut writer).await?;
    writer.shutdown().await.ok();

    let mut buf = Vec::with_capacity(1024);
    reader.read_to_end(&mut buf).await?;
    Ok(buf)
}

/// Decode the daemon's `MAILBOX-LIST` response. On `OK` returns the
/// raw JSON body string; on `ERR` formats the wire reason verbatim
/// the way every other MCP tool surfaces daemon-side errors.
fn decode_mailbox_list_response(buf: &[u8]) -> Result<String, String> {
    let text = std::str::from_utf8(buf).map_err(|_| "response is not UTF-8".to_string())?;
    let header_end = text
        .find("\n\n")
        .or_else(|| text.find("\r\n\r\n"))
        .ok_or_else(|| format!("malformed response (no header terminator): {text:?}"))?;
    // Locate body start (covers both `\n\n` and `\r\n\r\n`).
    let body_start = if text[header_end..].starts_with("\r\n\r\n") {
        header_end + 4
    } else {
        header_end + 2
    };
    let header_block = &text[..header_end];

    let mut lines = header_block.lines();
    let status_line = lines
        .next()
        .ok_or_else(|| "empty response from daemon".to_string())?
        .trim_end_matches('\r');
    let rest = status_line
        .strip_prefix("AIMX/1 ")
        .ok_or_else(|| format!("unexpected response: {status_line:?}"))?;

    if rest == "OK" {
        // OK frame must carry a Content-Length header and a body.
        let mut content_length: Option<usize> = None;
        for line in lines {
            let line = line.trim_end_matches('\r');
            if let Some(v) = line.strip_prefix("Content-Length:") {
                content_length = v
                    .trim()
                    .parse::<usize>()
                    .map(Some)
                    .map_err(|_| format!("invalid Content-Length: {line:?}"))?;
            }
        }
        let n =
            content_length.ok_or_else(|| "missing Content-Length on OK response".to_string())?;
        let body_bytes = &buf[body_start..];
        if body_bytes.len() < n {
            return Err(format!(
                "truncated body: expected {n} bytes, got {}",
                body_bytes.len()
            ));
        }
        return std::str::from_utf8(&body_bytes[..n])
            .map(|s| s.to_string())
            .map_err(|_| "JSON body is not UTF-8".to_string());
    }

    if let Some(err_body) = rest.strip_prefix("ERR ") {
        let (code_str, reason) = err_body.split_once(' ').unwrap_or((err_body, ""));
        let code = ErrCode::from_str(code_str)
            .map(|c| c.as_str().to_string())
            .unwrap_or_else(|| code_str.to_string());
        return Err(format!("[{code}] {}", reason.trim()));
    }

    Err(format!("unexpected response: {status_line:?}"))
}

/// Default page size when the caller omits `limit`.
const DEFAULT_LIMIT: u32 = 50;
/// Hard cap on `limit`; values above this are silently clamped.
const MAX_LIMIT: u32 = 200;

/// Resolve the effective `limit` for a page request. Missing → 50;
/// values above 200 silently clamp to 200; zero is allowed (returns an
/// empty page without reading any frontmatter).
fn clamp_limit(raw: Option<u32>) -> usize {
    let v = raw.unwrap_or(DEFAULT_LIMIT);
    v.min(MAX_LIMIT) as usize
}

/// Inbox row shape — matches the JSON output of `email_list` for
/// `folder = "inbox"`. `read` is always present and populated.
#[derive(Serialize)]
struct InboxListRow {
    id: String,
    from: String,
    to: String,
    subject: String,
    date: String,
    read: bool,
}

/// Sent row shape — matches the JSON output of `email_list` for
/// `folder = "sent"`. `read` is intentionally absent (agents never
/// mark sent mail read/unread); `delivery_status` is the value from
/// the outbound frontmatter, surfaced verbatim.
#[derive(Serialize)]
struct SentListRow {
    id: String,
    from: String,
    to: String,
    subject: String,
    date: String,
    delivery_status: String,
}

/// Minimal frontmatter projection used by `email_list`. Only fields
/// surfaced by `InboxListRow` / `SentListRow` are decoded — the rest
/// of the (potentially large) frontmatter block is skipped, keeping
/// the parse cost bounded even with many headers / attachments.
#[derive(Deserialize)]
struct EmailListFm {
    #[serde(default)]
    id: String,
    #[serde(default)]
    from: String,
    #[serde(default)]
    to: String,
    #[serde(default)]
    subject: String,
    #[serde(default)]
    date: String,
    #[serde(default)]
    read: bool,
    #[serde(default)]
    delivery_status: Option<String>,
}

/// Enumerate `(id, frontmatter_path)` pairs in `mailbox_dir` without
/// reading any file contents. Bundle directories surface as
/// `(<stem>, <stem>/<stem>.md)`; flat `.md` files as `(<stem>, <stem>.md)`.
/// Missing or unreadable directories return an empty list (the MCP tool
/// never errors on an empty mailbox).
fn enumerate_email_ids(mailbox_dir: &std::path::Path) -> std::io::Result<Vec<(String, PathBuf)>> {
    let mut ids: Vec<(String, PathBuf)> = Vec::new();
    let entries = match std::fs::read_dir(mailbox_dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(ids),
        Err(e) => return Err(e),
    };

    for entry in entries.filter_map(|e| e.ok()) {
        // Symlinks are silently skipped: `file_type()` does not follow them,
        // so a symlinked bundle dir / `.md` is neither `is_dir()` nor
        // `is_file()` and falls through. aimx never writes symlinks here;
        // if a backup tool restores one, it disappears from listings by
        // design (no path-escape via `read_email_frontmatter`).
        let file_type = match entry.file_type() {
            Ok(ft) => ft,
            Err(_) => continue,
        };
        let path = entry.path();
        if file_type.is_dir() {
            if let Some(stem) = path.file_name().and_then(|f| f.to_str()) {
                let md = path.join(format!("{stem}.md"));
                ids.push((stem.to_string(), md));
            }
        } else if file_type.is_file()
            && path.extension().is_some_and(|ext| ext == "md")
            && let Some(stem) = path.file_stem().and_then(|f| f.to_str())
        {
            ids.push((stem.to_string(), path));
        }
    }
    Ok(ids)
}

/// Build the JSON page response for `email_list`. Pass 1 enumerates ids
/// without parsing; pass 2 reads frontmatter only for the page slice.
/// Empty pages serialize to the literal `"[]"` string — never the old
/// `"No emails found."` text.
///
/// The returned page may be SHORTER than `limit` even when more ids
/// exist beyond `offset + limit`: the slice `[offset..end]` is fixed
/// before iteration, and a row whose frontmatter cannot be read
/// (`Ok(None)` — missing inner `.md` in a bundle, or content without
/// `+++` delimiters) is silently dropped without backfilling. This is
/// intentional graceful degradation; do not "fix" it by backfilling.
fn list_email_page_json(
    mailbox_dir: &std::path::Path,
    folder: Folder,
    offset: usize,
    limit: usize,
) -> Result<String, Box<dyn std::error::Error>> {
    let mut ids = enumerate_email_ids(mailbox_dir)?;
    // Filenames are `YYYY-MM-DD-HHMMSS-<slug>` so descending
    // lexicographic order is reverse chronological. Sort on the id
    // (filename stem) only — the path component is irrelevant to the
    // ordering invariant pinned by FR-B2.
    ids.sort_by(|a, b| b.0.cmp(&a.0));

    if offset >= ids.len() || limit == 0 {
        return Ok("[]".to_string());
    }
    let end = (offset + limit).min(ids.len());
    let page = &ids[offset..end];

    match folder {
        Folder::Inbox => {
            let mut rows: Vec<InboxListRow> = Vec::with_capacity(page.len());
            for (id, fm_path) in page {
                let Some(fm) = read_email_frontmatter(fm_path)? else {
                    continue;
                };
                rows.push(InboxListRow {
                    id: choose_id(id, &fm.id),
                    from: fm.from,
                    to: fm.to,
                    subject: fm.subject,
                    date: fm.date,
                    read: fm.read,
                });
            }
            Ok(serde_json::to_string(&rows)?)
        }
        Folder::Sent => {
            let mut rows: Vec<SentListRow> = Vec::with_capacity(page.len());
            for (id, fm_path) in page {
                let Some(fm) = read_email_frontmatter(fm_path)? else {
                    continue;
                };
                rows.push(SentListRow {
                    id: choose_id(id, &fm.id),
                    from: fm.from,
                    to: fm.to,
                    subject: fm.subject,
                    date: fm.date,
                    delivery_status: fm.delivery_status.unwrap_or_default(),
                });
            }
            Ok(serde_json::to_string(&rows)?)
        }
    }
}

/// Prefer the on-disk filename stem as the canonical id (it is the
/// value the agent passes to `email_read` / `email_mark_*`). Fall back
/// to the frontmatter `id` only when the filename could not be decoded
/// — protects the response shape against unicode-broken filenames.
fn choose_id(filename_stem: &str, fm_id: &str) -> String {
    if !filename_stem.is_empty() {
        filename_stem.to_string()
    } else {
        fm_id.to_string()
    }
}

/// Read and parse the frontmatter for one email. Increments the
/// test-only read counter so `email_list` perf tests can assert that
/// page-N reads at most N frontmatter blocks. A missing file (`<id>/<id>.md`
/// inside an empty bundle dir) returns `Ok(None)` so the caller can skip
/// it without short-circuiting the whole page.
fn read_email_frontmatter(
    path: &std::path::Path,
) -> Result<Option<EmailListFm>, Box<dyn std::error::Error>> {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(Box::new(e)),
    };
    #[cfg(test)]
    fm_read_count_inc();
    let parts: Vec<&str> = content.splitn(3, "+++").collect();
    if parts.len() < 3 {
        return Ok(None);
    }
    let toml_str = parts[1].trim();
    let fm: EmailListFm = toml::from_str(toml_str)?;
    Ok(Some(fm))
}

#[cfg(test)]
thread_local! {
    static FM_READ_COUNT: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
fn fm_read_count_inc() {
    FM_READ_COUNT.with(|c| c.set(c.get() + 1));
}

/// Reset the per-thread frontmatter-read counter and return its prior
/// value. Test harness only — production callers do not increment.
#[cfg(test)]
pub fn fm_read_count_reset() -> usize {
    FM_READ_COUNT.with(|c| {
        let v = c.get();
        c.set(0);
        v
    })
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

#[cfg(test)]
mod email_list_tests {
    use super::*;
    use tempfile::TempDir;

    fn write_inbox_md(dir: &std::path::Path, id: &str, from: &str, subject: &str, read: bool) {
        std::fs::create_dir_all(dir).unwrap();
        let body = format!(
            "+++\n\
             id = \"{id}\"\n\
             message_id = \"<{id}@test.com>\"\n\
             from = \"{from}\"\n\
             to = \"alice@test.com\"\n\
             subject = \"{subject}\"\n\
             date = \"2025-06-01T12:00:00Z\"\n\
             attachments = []\n\
             mailbox = \"alice\"\n\
             read = {read}\n\
             dkim = \"none\"\n\
             spf = \"none\"\n\
             +++\n\nBody.\n"
        );
        std::fs::write(dir.join(format!("{id}.md")), body).unwrap();
    }

    fn write_sent_md(dir: &std::path::Path, id: &str, status: &str) {
        std::fs::create_dir_all(dir).unwrap();
        let body = format!(
            "+++\n\
             id = \"{id}\"\n\
             message_id = \"<{id}@test.com>\"\n\
             from = \"alice@test.com\"\n\
             to = \"out@example.com\"\n\
             subject = \"S\"\n\
             date = \"2025-06-01T12:00:00Z\"\n\
             mailbox = \"alice\"\n\
             read = false\n\
             outbound = true\n\
             delivery_status = \"{status}\"\n\
             +++\n\nBody.\n"
        );
        std::fs::write(dir.join(format!("{id}.md")), body).unwrap();
    }

    #[test]
    fn page_reads_at_most_limit_frontmatter_blocks() {
        // Seed 10 inbox files; ask for limit=3, offset=2; expect exactly
        // 3 frontmatter reads (descending order), the rest untouched.
        let tmp = TempDir::new().unwrap();
        for i in 1..=10 {
            let id = format!("2025-06-01-{i:03}");
            write_inbox_md(tmp.path(), &id, "s@x.com", "S", false);
        }

        fm_read_count_reset();
        let json =
            list_email_page_json(tmp.path(), Folder::Inbox, 2, 3).expect("page returns JSON");
        let reads = fm_read_count_reset();
        assert_eq!(reads, 3, "page-3 must read exactly 3 frontmatter blocks");

        let rows: serde_json::Value = serde_json::from_str(&json).unwrap();
        let rows = rows.as_array().unwrap();
        assert_eq!(rows.len(), 3);
        // Descending: 10, 9, 8 — offset=2 skips 10 and 9, takes 8, 7, 6.
        assert_eq!(rows[0]["id"], "2025-06-01-008");
        assert_eq!(rows[1]["id"], "2025-06-01-007");
        assert_eq!(rows[2]["id"], "2025-06-01-006");
    }

    #[test]
    fn empty_mailbox_returns_empty_array_no_reads() {
        let tmp = TempDir::new().unwrap();
        std::fs::create_dir_all(tmp.path()).unwrap();
        fm_read_count_reset();
        let json =
            list_email_page_json(tmp.path(), Folder::Inbox, 0, 50).expect("page returns JSON");
        assert_eq!(fm_read_count_reset(), 0);
        assert_eq!(json, "[]");
    }

    #[test]
    fn missing_mailbox_dir_returns_empty_array() {
        // No directory created — list call must surface an empty page,
        // not an error.
        let tmp = TempDir::new().unwrap();
        let phantom = tmp.path().join("does-not-exist");
        let json = list_email_page_json(&phantom, Folder::Inbox, 0, 50).unwrap();
        assert_eq!(json, "[]");
    }

    #[test]
    fn offset_beyond_end_returns_empty_array() {
        let tmp = TempDir::new().unwrap();
        for i in 1..=3 {
            let id = format!("2025-06-01-{i:03}");
            write_inbox_md(tmp.path(), &id, "s@x.com", "S", false);
        }
        fm_read_count_reset();
        let json = list_email_page_json(tmp.path(), Folder::Inbox, 99, 50).unwrap();
        assert_eq!(fm_read_count_reset(), 0);
        assert_eq!(json, "[]");
    }

    #[test]
    fn sent_rows_omit_read_key() {
        let tmp = TempDir::new().unwrap();
        write_sent_md(tmp.path(), "2025-06-01-001", "delivered");
        write_sent_md(tmp.path(), "2025-06-01-002", "failed");

        let json = list_email_page_json(tmp.path(), Folder::Sent, 0, 50).unwrap();
        let rows: serde_json::Value = serde_json::from_str(&json).unwrap();
        let rows = rows.as_array().unwrap();
        assert_eq!(rows.len(), 2);
        for row in rows {
            let obj = row.as_object().expect("row is object");
            // `read` is gone — agents do not mark sent mail.
            assert!(
                !obj.contains_key("read"),
                "sent row must not carry `read` key: {obj:?}"
            );
            assert!(obj.contains_key("delivery_status"));
        }
        // Newest first, status pass-through verbatim.
        assert_eq!(rows[0]["id"], "2025-06-01-002");
        assert_eq!(rows[0]["delivery_status"], "failed");
        assert_eq!(rows[1]["delivery_status"], "delivered");
    }

    #[test]
    fn inbox_rows_carry_read_key() {
        let tmp = TempDir::new().unwrap();
        write_inbox_md(tmp.path(), "2025-06-01-001", "s@x.com", "S", false);
        write_inbox_md(tmp.path(), "2025-06-01-002", "s@x.com", "S", true);

        let json = list_email_page_json(tmp.path(), Folder::Inbox, 0, 50).unwrap();
        let rows: serde_json::Value = serde_json::from_str(&json).unwrap();
        let rows = rows.as_array().unwrap();
        assert_eq!(rows.len(), 2);
        for row in rows {
            let obj = row.as_object().expect("row is object");
            assert!(obj.contains_key("read"));
            assert!(
                !obj.contains_key("delivery_status"),
                "inbox row must not carry `delivery_status`"
            );
        }
    }

    #[test]
    fn ten_thousand_message_page_50_reads_at_most_50_blocks() {
        // Pin the perf claim: page-50 reads ≤ 50 frontmatter blocks
        // regardless of mailbox size. Wall-clock is recorded but only
        // fails on a generous bound — the cold-cache 50ms target is
        // documented in the algorithm comments, not asserted here.
        let tmp = TempDir::new().unwrap();
        for i in 0..10_000 {
            let id = format!("2025-06-01-{i:06}");
            // Cheaper write loop — same content shape as
            // `write_inbox_md` but inlined to avoid 10k function calls
            // hitting create_dir_all unnecessarily.
            let body = format!(
                "+++\nid = \"{id}\"\nmessage_id = \"<{id}@t>\"\nfrom = \"a@b.c\"\nto = \"a@b.c\"\nsubject = \"S\"\ndate = \"2025-06-01T00:00:00Z\"\nattachments = []\nmailbox = \"alice\"\nread = false\ndkim = \"none\"\nspf = \"none\"\n+++\n\nB.\n"
            );
            std::fs::write(tmp.path().join(format!("{id}.md")), body).unwrap();
        }

        fm_read_count_reset();
        let start = std::time::Instant::now();
        let json = list_email_page_json(tmp.path(), Folder::Inbox, 0, 50).unwrap();
        let elapsed = start.elapsed();
        let reads = fm_read_count_reset();

        assert!(reads <= 50, "page-50 read {reads} frontmatter blocks");
        let rows: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(rows.as_array().unwrap().len(), 50);

        // Soft assertion: page-50 on 10k must stay under 500ms (~10x
        // the <50ms cold-cache design target). Tight enough to catch
        // an order-of-magnitude regression on debug builds + warm CI
        // disk; loose enough to not flake on a stressed runner.
        eprintln!("page-50 on 10k messages: {elapsed:?} ({reads} fm reads)");
        assert!(
            elapsed.as_millis() < 500,
            "page-50 took {elapsed:?} — perf regression?"
        );
    }

    #[test]
    fn descending_sort_matches_reverse_insertion_order() {
        // Pin the FR-B2 invariant: filenames are
        // `YYYY-MM-DD-HHMMSS-<slug>` so descending lex == reverse
        // chronological. Seed 100 files with deliberately diverse
        // slugs (digits, lowercase, uppercase, punctuation we permit)
        // and assert sort order matches reverse insertion order — no
        // slug character can hoist an older message above a newer one.
        let tmp = TempDir::new().unwrap();
        let slug_chars = [
            "0", "1", "2", "9", "a", "b", "z", "A", "Z", "x-y", "x_y", "alpha", "ZZZ",
        ];
        let mut ids = Vec::new();
        for i in 0..100 {
            let slug = slug_chars[i % slug_chars.len()];
            let id = format!("2025-06-01-{:06}-{slug}", i);
            write_inbox_md(tmp.path(), &id, "s@x.com", "S", false);
            ids.push(id);
        }

        let json = list_email_page_json(tmp.path(), Folder::Inbox, 0, 100).unwrap();
        let rows: serde_json::Value = serde_json::from_str(&json).unwrap();
        let rows = rows.as_array().unwrap();
        assert_eq!(rows.len(), 100);

        let actual: Vec<String> = rows
            .iter()
            .map(|r| r["id"].as_str().unwrap().to_string())
            .collect();
        let mut expected = ids.clone();
        expected.reverse();
        assert_eq!(
            actual, expected,
            "descending lex order must match reverse insertion order"
        );
    }

    #[test]
    fn limit_clamps_to_max() {
        // Values above 200 silently clamp to 200; the schema-mandated
        // default is 50; missing → 50.
        assert_eq!(clamp_limit(None), 50);
        assert_eq!(clamp_limit(Some(0)), 0);
        assert_eq!(clamp_limit(Some(50)), 50);
        assert_eq!(clamp_limit(Some(200)), 200);
        assert_eq!(clamp_limit(Some(201)), 200);
        assert_eq!(clamp_limit(Some(u32::MAX)), 200);
    }

    #[test]
    fn bundle_dir_contributes_one_id_to_listing() {
        // Bundle layout: `<stem>/<stem>.md`. Listing must surface the
        // bundle as one id without recursing into the directory.
        let tmp = TempDir::new().unwrap();
        let bundle = tmp.path().join("2025-06-01-001-with-attach");
        std::fs::create_dir_all(&bundle).unwrap();
        let inner_id = "2025-06-01-001-with-attach";
        let body = format!(
            "+++\nid = \"{inner_id}\"\nmessage_id = \"<x@t>\"\nfrom = \"a@b.c\"\nto = \"a@b.c\"\nsubject = \"S\"\ndate = \"2025-06-01T00:00:00Z\"\nattachments = []\nmailbox = \"alice\"\nread = false\ndkim = \"none\"\nspf = \"none\"\n+++\n\nB.\n"
        );
        std::fs::write(bundle.join(format!("{inner_id}.md")), body).unwrap();
        // Sibling attachment file inside the bundle — must NOT be
        // listed as a separate id.
        std::fs::write(bundle.join("invoice.pdf"), b"not-mail").unwrap();

        // A flat `.md` next to the bundle.
        write_inbox_md(tmp.path(), "2025-06-01-002", "s@x.com", "S", false);

        let json = list_email_page_json(tmp.path(), Folder::Inbox, 0, 50).unwrap();
        let rows: serde_json::Value = serde_json::from_str(&json).unwrap();
        let rows = rows.as_array().unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["id"], "2025-06-01-002");
        assert_eq!(rows[1]["id"], "2025-06-01-001-with-attach");
    }

    #[test]
    fn bundle_without_inner_md_is_skipped_surrounding_rows_surface() {
        // Pin the graceful-skip contract documented on `list_email_page_json`:
        // a bundle dir whose inner `.md` is missing must not poison the
        // page — the row drops out, neighbours stay.
        let tmp = TempDir::new().unwrap();
        write_inbox_md(tmp.path(), "2025-06-01-001", "s@x.com", "S", false);
        // Empty bundle dir: the expected `<stem>/<stem>.md` is absent.
        std::fs::create_dir_all(tmp.path().join("2025-06-01-002-broken")).unwrap();
        write_inbox_md(tmp.path(), "2025-06-01-003", "s@x.com", "S", false);

        let json = list_email_page_json(tmp.path(), Folder::Inbox, 0, 50).unwrap();
        let rows: serde_json::Value = serde_json::from_str(&json).unwrap();
        let rows = rows.as_array().unwrap();
        assert_eq!(rows.len(), 2, "broken bundle drops; neighbours surface");
        assert_eq!(rows[0]["id"], "2025-06-01-003");
        assert_eq!(rows[1]["id"], "2025-06-01-001");
    }

    #[test]
    fn missing_frontmatter_delimiters_skips_row() {
        // A `.md` file without `+++` delimiters yields `Ok(None)` from
        // `read_email_frontmatter` (the `splitn(3) < 3` branch) — the
        // row is silently skipped, never errors.
        let tmp = TempDir::new().unwrap();
        write_inbox_md(tmp.path(), "2025-06-01-001", "s@x.com", "S", false);
        std::fs::write(
            tmp.path().join("2025-06-01-002.md"),
            "no frontmatter here, just body\n",
        )
        .unwrap();
        write_inbox_md(tmp.path(), "2025-06-01-003", "s@x.com", "S", false);

        let json = list_email_page_json(tmp.path(), Folder::Inbox, 0, 50).unwrap();
        let rows: serde_json::Value = serde_json::from_str(&json).unwrap();
        let rows = rows.as_array().unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["id"], "2025-06-01-003");
        assert_eq!(rows[1]["id"], "2025-06-01-001");
    }

    #[test]
    fn sent_row_with_missing_delivery_status_falls_back_to_empty_string() {
        // `delivery_status` is `Option<String>` on the partial decoder;
        // an outbound frontmatter without the field must surface as
        // `""` (verbatim, no `null`) on the JSON row.
        let tmp = TempDir::new().unwrap();
        let id = "2025-06-01-001";
        let body = format!(
            "+++\nid = \"{id}\"\nmessage_id = \"<{id}@t>\"\nfrom = \"alice@test.com\"\nto = \"out@example.com\"\nsubject = \"S\"\ndate = \"2025-06-01T12:00:00Z\"\nmailbox = \"alice\"\nread = false\noutbound = true\n+++\n\nB.\n"
        );
        std::fs::write(tmp.path().join(format!("{id}.md")), body).unwrap();

        let json = list_email_page_json(tmp.path(), Folder::Sent, 0, 50).unwrap();
        let rows: serde_json::Value = serde_json::from_str(&json).unwrap();
        let rows = rows.as_array().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["delivery_status"], "");
    }

    #[test]
    fn email_mark_params_rejects_stale_folder_arg() {
        // Symmetric to how `hook_create` hard-rejects a removed `stdin`
        // arg: a stale `folder` field on `email_mark_*` must surface as
        // a parse error, not a silent drop that mutates inbox while the
        // agent thinks it touched sent.
        let json = serde_json::json!({
            "mailbox": "alice",
            "id": "2025-06-15-120000-hello",
            "folder": "sent",
        });
        let err = match serde_json::from_value::<EmailMarkParams>(json) {
            Ok(_) => panic!("expected unknown-field error, got Ok"),
            Err(e) => e,
        };
        assert!(
            err.to_string().contains("unknown field"),
            "expected unknown-field error, got {err}"
        );
    }

    #[test]
    fn email_mark_params_accepts_canonical_shape() {
        let json = serde_json::json!({
            "mailbox": "alice",
            "id": "2025-06-15-120000-hello",
        });
        let params: EmailMarkParams =
            serde_json::from_value(json).expect("canonical mark params must parse");
        assert_eq!(params.mailbox, "alice");
        assert_eq!(params.id, "2025-06-15-120000-hello");
    }
}

#[cfg(test)]
mod schema_order_tests {
    //! Snapshot tests pinning the JSON-schema property order for every
    //! tool's `*Params` struct. `schemars` emits `properties` in struct
    //! declaration order, so reordering struct fields reorders the
    //! schema. Each test here asserts the exact key order an agent sees
    //! in the tool list — wire compatibility is unaffected (JSON params
    //! are name-keyed) but agents read these schemas top-to-bottom.

    use super::*;
    use schemars::schema_for;
    use serde_json::Value;

    fn property_keys<T: schemars::JsonSchema>() -> Vec<String> {
        let schema = schema_for!(T);
        let value: Value = serde_json::to_value(schema).expect("schema serializes to JSON");
        let props = value
            .get("properties")
            .and_then(|p| p.as_object())
            .expect("schema has a properties object");
        props.keys().cloned().collect()
    }

    #[test]
    fn email_list_params_property_order() {
        assert_eq!(
            property_keys::<EmailListParams>(),
            vec!["mailbox", "folder", "limit", "offset"],
        );
    }

    #[test]
    fn email_read_params_property_order() {
        assert_eq!(
            property_keys::<EmailReadParams>(),
            vec!["mailbox", "id", "folder"],
        );
    }

    #[test]
    fn email_send_params_property_order() {
        assert_eq!(
            property_keys::<EmailSendParams>(),
            vec![
                "from_mailbox",
                "to",
                "subject",
                "reply_to",
                "body",
                "attachments",
                "references",
            ],
        );
    }

    #[test]
    fn email_reply_params_property_order() {
        assert_eq!(
            property_keys::<EmailReplyParams>(),
            vec!["mailbox", "id", "body"],
        );
    }

    #[test]
    fn email_mark_params_property_order() {
        assert_eq!(property_keys::<EmailMarkParams>(), vec!["mailbox", "id"]);
    }

    #[test]
    fn hook_create_params_property_order() {
        assert_eq!(
            property_keys::<HookCreateParams>(),
            vec![
                "mailbox",
                "event",
                "cmd",
                "name",
                "timeout_secs",
                "fire_on_untrusted",
            ],
        );
    }

    #[test]
    fn hook_list_params_property_order() {
        assert_eq!(property_keys::<HookListParams>(), vec!["mailbox"]);
    }

    #[test]
    fn hook_delete_params_property_order() {
        assert_eq!(property_keys::<HookDeleteParams>(), vec!["name"]);
    }
}
