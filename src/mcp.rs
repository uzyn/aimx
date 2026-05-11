use crate::cli::SendArgs;
#[cfg(test)]
use crate::config::Config;
use crate::frontmatter::InboundFrontmatter;
use crate::send;
use crate::send_protocol::{
    self, ErrCode, HookCreateRequest, HookDeleteRequest, MailboxLifecycleRequest, MarkRequest,
};

use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{ServerCapabilities, ServerInfo};
use rmcp::{ServerHandler, ServiceExt, schemars, tool, tool_handler, tool_router};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct AimxMcpServer {
    /// Effective uid of the process running `aimx mcp`. Captured at
    /// `new()` and read by the test-only `authorize_mailbox` helper.
    /// Production tools delegate authorization to the daemon (which
    /// resolves the caller via SO_PEERCRED and runs the central
    /// `authorize()` predicate) so this field is never read in shipped
    /// builds.
    #[allow(dead_code)]
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
    #[schemars(
        description = "Email body. Rendered as Markdown (CommonMark + GFM: tables, \
                       strikethrough, autolinks, task lists, footnotes) into \
                       multipart/alternative HTML+text by default. The recipient sees \
                       styled HTML on Gmail / Outlook / Apple Mail and the Markdown source \
                       on text-only clients. See the text_only field before opting out."
    )]
    pub body: String,
    #[schemars(description = "File paths to attach")]
    pub attachments: Option<Vec<String>>,
    #[schemars(
        description = "Full References header chain (space-separated Message-IDs) for threading. \
                       Only applied when reply_to is also set. Supplied alone, it is silently ignored."
    )]
    pub references: Option<String>,
    #[schemars(
        description = "DO NOT set true for prose, briefs, summaries, reports, or any body \
                       containing Markdown syntax — it ships the raw Markdown source as \
                       text/plain and the recipient sees `#` `**bold**` `-` markers instead \
                       of rendered HTML. Only set true for short transactional one-liners with \
                       no formatting (OTPs, verification codes, single-line confirmations) and \
                       existing scripts that must not change shape. Mutually exclusive with html_body."
    )]
    pub text_only: Option<bool>,
    #[schemars(
        description = "Custom HTML body shipped verbatim as the text/html part of \
                       a multipart/alternative. Use body for the text/plain fallback. \
                       Operator-supplied content; bypasses sanitization. Mutually \
                       exclusive with text_only."
    )]
    pub html_body: Option<String>,
}

#[derive(Serialize, Deserialize, schemars::JsonSchema)]
pub struct EmailReplyParams {
    #[schemars(description = "Mailbox name containing the email to reply to")]
    pub mailbox: String,
    #[schemars(description = "Email ID to reply to (e.g. 2025-01-01-001)")]
    pub id: String,
    #[schemars(
        description = "Reply body. Rendered as Markdown (CommonMark + GFM: tables, \
                       strikethrough, autolinks, task lists, footnotes) into \
                       multipart/alternative HTML+text by default. The recipient sees \
                       styled HTML on Gmail / Outlook / Apple Mail and the Markdown source \
                       on text-only clients. See the text_only field before opting out."
    )]
    pub body: String,
    #[schemars(
        description = "DO NOT set true for prose, briefs, summaries, reports, or any body \
                       containing Markdown syntax — it ships the raw Markdown source as \
                       text/plain and the recipient sees `#` `**bold**` `-` markers instead \
                       of rendered HTML. Only set true for short transactional one-liners with \
                       no formatting (OTPs, verification codes, single-line confirmations) and \
                       existing scripts that must not change shape. Mutually exclusive with html_body."
    )]
    pub text_only: Option<bool>,
    #[schemars(
        description = "Custom HTML body shipped verbatim as the text/html part of \
                       a multipart/alternative. Use body for the text/plain fallback. \
                       Operator-supplied content; bypasses sanitization. Mutually \
                       exclusive with text_only."
    )]
    pub html_body: Option<String>,
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
    #[schemars(description = "Default false. DO NOT set true unless the user \
                       explicitly asks the hook to fire on untrusted senders \
                       (phrases like \"fire on untrusted\", \"even from \
                       untrusted senders\", \"regardless of trust\"). Setting \
                       true lets ANY external sender — including spoofed-From \
                       spammers — trigger this hook's cmd, which is a real \
                       cost / RCE-shaped exposure when cmd invokes an LLM or \
                       shell. Assume the operator has already configured the \
                       mailbox's trust policy; with false, the hook fires \
                       only on inbound mail the daemon marks `trusted = \
                       \"true\"` (sender on the operator's allowlist AND \
                       DKIM passes). After creating the hook, tell the user \
                       that the cmd will fire on inbound mail from senders \
                       the operator has marked trusted, so the user knows \
                       what triggers it. Only valid on event = \
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

#[derive(Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MailboxCreateParams {
    #[schemars(description = "Mailbox name (local part of the resulting address). \
                       Must match `[a-z0-9._-]+` with no leading/trailing dot \
                       and no `..`. Reserved names (`catchall`, `aimx-catchall`) \
                       are rejected by the daemon.")]
    pub name: String,
}

#[derive(Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MailboxDeleteParams {
    #[schemars(description = "Mailbox name to delete. You must own it (the \
                       daemon checks via SO_PEERCRED).")]
    pub name: String,
    #[schemars(description = "When true, wipe `inbox/<name>/` and `sent/<name>/` \
                       contents before submitting the delete. When false (default), \
                       the daemon refuses non-empty mailboxes with a \
                       `[NONEMPTY]` error.")]
    pub force: Option<bool>,
}

impl AimxMcpServer {
    pub fn new() -> Self {
        Self::with_caller_uid(crate::platform::current_euid())
    }

    /// Test-only constructor that lets the caller pin the authorization
    /// principal. Production code calls `new()`, which derives the uid
    /// from `geteuid()` at startup.
    #[cfg(test)]
    pub fn with_caller_uid_for_test(caller_uid: u32) -> Self {
        Self::with_caller_uid(caller_uid)
    }

    fn with_caller_uid(caller_uid: u32) -> Self {
        Self {
            caller_uid,
            tool_router: Self::tool_router(),
        }
    }

    /// Return the auth predicate's verdict for `action` against the
    /// named mailbox. Mirrors the daemon-side helper so the auth gate
    /// runs the same way through CLI, daemon UDS, and MCP. The MCP
    /// surface returns errors as `String` per `rmcp` conventions.
    /// Now test-only: production tools delegate authz to the daemon
    /// over UDS (the daemon owns the central `authorize()` predicate
    /// and re-runs it on every wire request, so the MCP-side
    /// pre-flight is redundant).
    #[cfg(test)]
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
            crate::auth::Action::MailboxDelete { mailbox } => mailbox.clone(),
            crate::auth::Action::MailboxCreate { .. } | crate::auth::Action::SystemCommand => {
                String::new()
            }
        };
        // Derive the rendering verb from the action so a future change
        // that produces `OwnerMismatch` from a non-create predicate
        // doesn't render "cannot create a..." for, say, a delete.
        let verb = match &action {
            crate::auth::Action::MailboxCreate { .. } => Some("create"),
            crate::auth::Action::MailboxDelete { .. } => Some("delete"),
            _ => None,
        };
        let mb = if mailbox_name.is_empty() {
            None
        } else {
            config.mailboxes.get(&mailbox_name)
        };
        crate::auth::authorize(self.caller_uid, action, mb).map_err(|e| {
            // Sprint 3 (S3-5): the MCP, CLI, and hooks surfaces all
            // share `auth::format_auth_error` so the four-arm match
            // can never drift between them. The MCP surface skips the
            // `surface` hint (the renderer falls back to the generic
            // "requires root" line) but does pass the resolved mailbox
            // name so `NoSuchMailbox` reads as "mailbox '<name>' not
            // found" — the agent-friendly form.
            crate::auth::format_auth_error(
                &e,
                &crate::auth::AuthErrorContext {
                    mailbox_name: if mailbox_name.is_empty() {
                        None
                    } else {
                        Some(&mailbox_name)
                    },
                    verb,
                    ..Default::default()
                },
            )
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
        // The daemon's `MAILBOX-LIST` is filtered to the caller's uid
        // via SO_PEERCRED, so a missing row collapses "no such mailbox"
        // and "you don't own that mailbox" into the same opaque error
        // (NFR2 opacity contract). This avoids the previous
        // direct config-load path which fails with EACCES on a
        // production-perm `0640 root:root` config from a non-root MCP
        // process.
        let row = lookup_mailbox_row(&params.mailbox)?;

        let folder = resolve_folder(params.folder.as_deref())?;
        let mailbox_dir = folder_path_from_row(&row, folder);

        let limit = clamp_limit(params.limit);
        let offset = params.offset.unwrap_or(0) as usize;

        list_email_page_json(&mailbox_dir, folder, offset, limit).map_err(|e| e.to_string())
    }

    #[tool(name = "email_read", description = "Read the full content of an email")]
    fn email_read(
        &self,
        Parameters(params): Parameters<EmailReadParams>,
    ) -> Result<String, String> {
        validate_email_id(&params.id)?;
        let row = lookup_mailbox_row(&params.mailbox)?;

        let folder = resolve_folder(params.folder.as_deref())?;
        let mailbox_dir = folder_path_from_row(&row, folder);
        // Read paths audit — `email_read` goes through the strict
        // resolver so a planted symlink or escape path cannot
        // exfiltrate another mailbox's mail via `email_read`, not just
        // via the MARK verbs.
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
        // The daemon's MARK handler re-runs SO_PEERCRED-based authz;
        // the MCP-side row lookup is the friendly pre-flight that
        // surfaces "mailbox not found / not yours" as the opaque
        // shared error rather than the daemon's wire reason.
        let _row = lookup_mailbox_row(&params.mailbox)?;
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
        let _row = lookup_mailbox_row(&params.mailbox)?;
        submit_mark_via_daemon(&params.mailbox, &params.id, false)?;
        Ok(format!("Email '{}' marked as unread.", params.id))
    }

    #[tool(
        name = "email_send",
        description = "Submit an email for DKIM signing and delivery via the aimx daemon. \
                       `body` is rendered as Markdown (CommonMark + GFM) into \
                       multipart/alternative HTML+text by default — the recipient sees \
                       styled HTML on Gmail / Outlook / Apple Mail. \
                       DO NOT set `text_only: true` for prose, briefs, summaries, reports, \
                       or any body containing markdown syntax; that ships the raw markdown \
                       source as `text/plain` and breaks rendering. \
                       Only set `text_only: true` for short transactional one-liners with \
                       no formatting (OTPs, verification codes, single-line confirmations). \
                       Set `html_body` to override the auto-rendered HTML with your own template."
    )]
    fn email_send(
        &self,
        Parameters(params): Parameters<EmailSendParams>,
    ) -> Result<String, String> {
        // Mutual-exclusion check runs first so the failure is the
        // cheapest possible: no daemon round-trip, no mailbox lookup,
        // no IO. The wording matches the codec's canonical message so
        // operators see the same string regardless of where validation
        // tripped.
        validate_text_only_html_body_exclusion(
            params.text_only.unwrap_or(false),
            params.html_body.as_deref(),
        )?;

        // The daemon's MAILBOX-LIST row carries the registered address
        // verbatim. We derive the from-address (and only secondarily
        // the domain) from it without reading root-owned
        // `/etc/aimx/config.toml`. The daemon-side SEND handler
        // re-runs SO_PEERCRED authz; the MCP pre-flight is operator
        // UX, not the security boundary.
        let row = lookup_mailbox_row(&params.from_mailbox)?;
        let from_address = row
            .address
            .as_deref()
            .ok_or_else(|| format!("Mailbox '{}' is not registered.", params.from_mailbox))?;
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
        description = "Reply to an email with correct threading headers. \
                       `body` is rendered as Markdown (CommonMark + GFM) into \
                       multipart/alternative HTML+text by default — the recipient sees \
                       styled HTML on Gmail / Outlook / Apple Mail. \
                       DO NOT set `text_only: true` for prose, briefs, summaries, reports, \
                       or any body containing markdown syntax; that ships the raw markdown \
                       source as `text/plain` and breaks rendering. \
                       Only set `text_only: true` for short transactional one-liners with \
                       no formatting (OTPs, verification codes, single-line confirmations). \
                       Set `html_body` to override the auto-rendered HTML with your own template."
    )]
    fn email_reply(
        &self,
        Parameters(params): Parameters<EmailReplyParams>,
    ) -> Result<String, String> {
        // Mutual-exclusion check runs first — same fast-fail principle
        // as `email_send`, and same canonical wording as the codec
        // (`AIMX/1 SEND: --text-only and --html-body are mutually
        // exclusive`) so operators see one consistent string regardless
        // of layer.
        validate_text_only_html_body_exclusion(
            params.text_only.unwrap_or(false),
            params.html_body.as_deref(),
        )?;

        validate_email_id(&params.id)?;
        let row = lookup_mailbox_row(&params.mailbox)?;

        let mailbox_dir = folder_path_from_row(&row, Folder::Inbox);
        // `email_reply` reads the parent message to inherit threading
        // headers; route through the strict resolver so a symlink
        // cannot leak another mailbox's message into a reply
        // composition.
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

        let from_address = row
            .address
            .as_deref()
            .ok_or_else(|| format!("Mailbox '{}' is not registered.", params.mailbox))?;
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
            from: from_address.to_string(),
            to: reply_to_email.to_string(),
            subject,
            body: params.body,
            reply_to: reply_to_id,
            references,
            attachments: vec![],
            text_only: params.text_only.unwrap_or(false),
            html_body: params.html_body,
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
        // Pre-flight: verify the mailbox is visible to the caller via
        // the daemon's MAILBOX-LIST. The listing is SO_PEERCRED-
        // filtered so a missing row collapses "no such mailbox" and
        // "exists but you don't own it" into one opaque error (NFR2).
        // The daemon's HOOK-CREATE handler runs the central
        // `authorize()` predicate again — this pre-flight only exists
        // for a friendly error vs. relying on the daemon's wire shape.
        let _row = lookup_mailbox_row(&params.mailbox)?;

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
        // Route through the daemon's HOOK-LIST so the non-root MCP
        // process never reads root-owned `/etc/aimx/config.toml`. The
        // daemon's listing is SO_PEERCRED-filtered to the caller's uid
        // (root sees every hook on every mailbox; non-root sees only
        // hooks on mailboxes it owns), matching the previous behavior
        // exactly.
        let json = submit_hook_list_via_daemon()?;

        // The optional `mailbox` filter narrows the daemon's response
        // to a single mailbox; the daemon-side filter already removed
        // unowned mailboxes, so this is a pure post-filter for UX.
        // When `mailbox` references a mailbox the caller doesn't own
        // (or that doesn't exist), `lookup_mailbox_row` surfaces the
        // canonical opaque error rather than silently returning `[]`.
        let Some(name) = params.mailbox.as_deref() else {
            return Ok(json);
        };
        let _row = lookup_mailbox_row(name)?;
        let rows: Vec<crate::hook_list_handler::HookListRow> =
            serde_json::from_str(&json).map_err(|e| format!("Failed to parse hook list: {e}"))?;
        let filtered: Vec<crate::hook_list_handler::HookListRow> =
            rows.into_iter().filter(|r| r.mailbox == name).collect();
        serde_json::to_string(&filtered).map_err(|e| format!("Failed to serialize: {e}"))
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
        // Thin pass-through to HOOK-DELETE. The daemon's handler
        // resolves the hook by effective name, runs the central
        // `authorize()` predicate, and returns the canonical opaque
        // not-found error for both unowned and nonexistent hooks
        // (NFR2 opacity contract). The previous MCP-side pre-flight
        // duplicated that work AND introduced the EACCES bug class on
        // production-perm `0640 root:root` configs by reading
        // `/etc/aimx/config.toml` from a non-root MCP process. Trust
        // the daemon.
        match submit_hook_delete_via_daemon(&params.name) {
            Ok(()) => Ok(format!("Hook '{}' deleted.", params.name)),
            Err(HookCrudFallback::SocketMissing) => {
                Err("aimx daemon not running. Start with 'sudo systemctl start aimx'".to_string())
            }
            Err(HookCrudFallback::Daemon(msg)) => Err(msg),
        }
    }

    #[tool(
        name = "mailbox_create",
        description = "Create a new mailbox owned by your uid. Submits the \
                       request through the aimx daemon over UDS; the daemon \
                       resolves the owner from SO_PEERCRED, so the new mailbox \
                       is always owned by you (no `owner` parameter — by \
                       construction agents cannot create mailboxes owned by \
                       another user). Returns the new mailbox's full address \
                       on success."
    )]
    fn mailbox_create(
        &self,
        Parameters(params): Parameters<MailboxCreateParams>,
    ) -> Result<String, String> {
        // No client-side validation: the daemon owns the regex, the
        // reserved-name list, and the idempotent "exists with matching
        // owner → success" semantics. Surfacing daemon errors verbatim
        // matches the pattern used by `email_send` / `hook_create`.
        // The `owner` argument on the wire is `None` so the daemon
        // synthesizes the owner from SO_PEERCRED rather than honoring
        // anything the agent could have planted.
        match submit_mailbox_crud_via_daemon(&params.name, true, None, false) {
            Ok(()) => {
                // Resolve the new mailbox's address through the
                // daemon's `MAILBOX-LIST` rather than reading
                // root-owned `/etc/aimx/config.toml` from the
                // non-root MCP process. The daemon's listing is
                // SO_PEERCRED-filtered to the caller's uid, so the
                // just-created mailbox is always visible to the
                // calling agent.
                let address = lookup_mailbox_address(&params.name).unwrap_or_default();
                if address.is_empty() {
                    Ok(format!("Mailbox '{}' created.", params.name))
                } else {
                    Ok(address)
                }
            }
            Err(MailboxLifecycleFallback::SocketMissing) => {
                Err("aimx daemon not running. Start with 'sudo systemctl start aimx'".to_string())
            }
            Err(MailboxLifecycleFallback::Daemon(msg)) => Err(msg),
        }
    }

    #[tool(
        name = "mailbox_delete",
        description = "Delete a mailbox you own. The daemon enforces ownership \
                       via SO_PEERCRED; an attempt to delete a mailbox owned \
                       by another uid surfaces as a not-authorized error. \
                       When `force` is true, the tool first wipes \
                       `inbox/<name>/` and `sent/<name>/` contents (mirroring \
                       the CLI's `--force` flag) before submitting the delete; \
                       when false (default), the daemon refuses non-empty \
                       mailboxes with a `[NONEMPTY]` error."
    )]
    fn mailbox_delete(
        &self,
        Parameters(params): Parameters<MailboxDeleteParams>,
    ) -> Result<String, String> {
        let force = params.force.unwrap_or(false);

        // Catchall is structurally distinct (owned by
        // `aimx-catchall`) and the daemon refuses to delete it. Mirror
        // that here so the wire surface still produces a friendly
        // operator-facing error rather than the daemon's internal
        // "cannot delete the catchall mailbox" verbatim — also
        // matches the CLI's pre-flight refusal.
        if force && params.name == "catchall" {
            return Err("Cannot delete the catchall mailbox".to_string());
        }

        // The wipe is performed server-side by the daemon under the
        // same per-mailbox lock that guards the stanza removal so the
        // wipe and the rewrite are atomic together. This both
        // eliminates the data-destruction race window the previous
        // client-side wipe left open AND removes the MCP process's
        // dependency on local read access to `/etc/aimx/config.toml`
        // (which is `0640 root:root` in production, so the previous
        // direct config-load path inevitably failed for non-root agents).
        match submit_mailbox_crud_via_daemon(&params.name, false, None, force) {
            Ok(()) => Ok(format!("Mailbox '{}' deleted.", params.name)),
            Err(MailboxLifecycleFallback::SocketMissing) => {
                Err("aimx daemon not running. Start with 'sudo systemctl start aimx'".to_string())
            }
            Err(MailboxLifecycleFallback::Daemon(msg)) => Err(msg),
        }
    }
}

/// Build `SendArgs` from `EmailSendParams` and the resolved sender
/// address. Pure and side-effect-free so it can be unit-tested without
/// the daemon; keeps the `params.reply_to` / `params.references`
/// forwarding explicit (see the deserialization tests below).
///
/// The mutual-exclusion check (`text_only` vs `html_body`) is run by
/// the caller via `validate_text_only_html_body_exclusion` before this
/// function is invoked, so this builder trusts both fields and forwards
/// them unchanged.
fn build_send_args(params: EmailSendParams, from_address: &str) -> SendArgs {
    SendArgs {
        from: from_address.to_string(),
        to: params.to,
        subject: params.subject,
        body: params.body,
        reply_to: params.reply_to,
        references: params.references,
        attachments: params.attachments.unwrap_or_default(),
        text_only: params.text_only.unwrap_or(false),
        html_body: params.html_body,
    }
}

/// Server-side mutual-exclusion check for the MCP `email_send` /
/// `email_reply` tools. Mirrors the clap-level `conflicts_with` rule on
/// the CLI's `SendArgs` and the codec-level rejection in the SEND
/// frame parser. Returns the canonical error string (matching the
/// codec) when both inputs would be carried on the wire — the daemon
/// would refuse such a frame, but firing here keeps the failure cheap
/// and avoids opening the UDS at all.
fn validate_text_only_html_body_exclusion(
    text_only: bool,
    html_body: Option<&str>,
) -> Result<(), String> {
    // Treat `Some("")` as supplied. The wire codec carries an empty
    // `Html-Body-Length: 0` payload through and rejects the conflict;
    // this pre-flight matches that semantic so the failure is identical
    // regardless of layer.
    if text_only && html_body.is_some() {
        // Match the wire-codec phrasing in `src/send_protocol.rs` so
        // the operator sees the same wording regardless of which layer
        // tripped the check.
        return Err("AIMX/1 SEND: --text-only and --html-body are mutually exclusive".to_string());
    }
    Ok(())
}

/// Compose `args` into an `AIMX/1 SEND` request and submit it to
/// `aimx serve` over the UDS. MCP, like `aimx send`, does not sign or
/// deliver mail directly. Everything goes through the daemon. The
/// request frame carries no `From-Mailbox:` header; the daemon parses
/// `From:` out of the composed body itself and resolves the sender
/// mailbox against its in-memory Config.
fn submit_via_daemon(args: &SendArgs) -> Result<String, String> {
    // Build the SEND frame via the shared helper used by `aimx send`
    // so MCP and CLI submit byte-identical requests for the same
    // `SendArgs`. `build_request` honors `args.text_only` /
    // `args.html_body` and applies the same CRLF normalization the CLI
    // uses for the optional second body section.
    let request = send::build_request(args)?;
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
        if is_socket_missing(&e) {
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
        if is_socket_missing(&e) {
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
pub(crate) enum MailboxLifecycleFallback {
    /// Socket not present / not connectable (daemon stopped, socket
    /// cleaned up, first-time setup). Callers fall back to direct edit.
    SocketMissing,
    /// Daemon connected and answered but reported an error (validation,
    /// NONEMPTY, IO, etc.). Caller should surface this verbatim.
    Daemon(String),
}

/// Submit a `MAILBOX-CREATE` / `MAILBOX-DELETE` request over UDS.
/// `owner` is honored only for root callers on CREATE — non-root
/// callers have any wire-supplied owner ignored by the daemon
/// (synthesized from `peer_username(SO_PEERCRED)` instead). `force`
/// is meaningful only on DELETE: when `true` the daemon wipes the
/// inbox and sent directories under the per-mailbox lock that
/// already guards the stanza removal, so the wipe and the rewrite
/// are atomic together (no data-destruction race window between a
/// client-side wipe and the daemon-side delete).
pub(crate) fn submit_mailbox_crud_via_daemon(
    name: &str,
    create: bool,
    owner: Option<&str>,
    force: bool,
) -> Result<(), MailboxLifecycleFallback> {
    let request = MailboxLifecycleRequest {
        name: name.to_string(),
        create,
        owner: owner.map(|s| s.to_string()),
        force,
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
                    MailboxLifecycleFallback::Daemon(format!("Failed to create tokio runtime: {e}"))
                })?;
            rt.block_on(submit_mailbox_crud_request(&socket, &request))
        }
    };

    match io_result {
        Ok(MarkOutcome::Ok) => Ok(()),
        Ok(MarkOutcome::Err { code, reason }) => Err(MailboxLifecycleFallback::Daemon(format!(
            "[{}] {reason}",
            code.as_str()
        ))),
        Ok(MarkOutcome::Malformed(reason)) => Err(MailboxLifecycleFallback::Daemon(format!(
            "Malformed response from aimx daemon: {reason}"
        ))),
        Err(e) => {
            if is_socket_missing(&e) {
                Err(MailboxLifecycleFallback::SocketMissing)
            } else {
                Err(MailboxLifecycleFallback::Daemon(format!(
                    "Failed to connect to aimx daemon at {}: {e}",
                    socket.display()
                )))
            }
        }
    }
}

async fn submit_mailbox_crud_request(
    socket_path: &std::path::Path,
    request: &MailboxLifecycleRequest,
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
/// [`MailboxLifecycleFallback`] so callers can decide whether to fall back to
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

pub async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let server = AimxMcpServer::new();
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
/// Best-effort: ask the daemon for the caller's mailbox listing and
/// return the address recorded for `name`. Returns `None` when the
/// daemon is unreachable, the listing is malformed, the mailbox is
/// absent (race), or the row is unregistered. The MCP
/// `mailbox_create` success message swallows the `None` case
/// gracefully — the create itself already succeeded; the address
/// suffix is operator UX.
fn lookup_mailbox_address(name: &str) -> Option<String> {
    let json = submit_mailbox_list_raw().ok()?;
    let rows: Vec<crate::mailbox_list_handler::MailboxListRow> =
        serde_json::from_str(&json).ok()?;
    rows.into_iter()
        .find(|r| r.name == name)
        .and_then(|r| r.address)
}

/// Fetch the daemon's `MAILBOX-LIST` and return the row matching
/// `name`. Used by every email tool to resolve `inbox_path` /
/// `sent_path` / `address` without the non-root MCP process needing
/// read access to root-owned `/etc/aimx/config.toml`.
///
/// Returns the daemon's verbatim error string on a wire-level failure
/// (socket missing, malformed response, daemon-side ERR), and a
/// canonical "not found" error when the listing comes back clean but
/// no row matches — the listing is already SO_PEERCRED-filtered to
/// the caller's uid, so a missing row is opaque between "doesn't
/// exist" and "exists but you don't own it" (NFR2 opacity).
fn lookup_mailbox_row(name: &str) -> Result<crate::mailbox_list_handler::MailboxListRow, String> {
    let json = submit_mailbox_list_via_daemon()?;
    let rows: Vec<crate::mailbox_list_handler::MailboxListRow> =
        serde_json::from_str(&json).map_err(|e| format!("Failed to parse mailbox list: {e}"))?;
    rows.into_iter()
        .find(|r| r.name == name)
        .ok_or_else(|| format!("Mailbox '{name}' does not exist."))
}

fn submit_mailbox_list_via_daemon() -> Result<String, String> {
    submit_mailbox_list_raw().map_err(|e| match e {
        MailboxLifecycleFallback::SocketMissing => {
            "aimx daemon not running. Start with 'sudo systemctl start aimx'".to_string()
        }
        MailboxLifecycleFallback::Daemon(msg) => msg,
    })
}

/// Submit an `AIMX/1 HOOK-LIST` and return the JSON body verbatim.
/// Mirrors [`submit_mailbox_list_via_daemon`] line-for-line so the
/// non-root MCP process answers `hook_list` without reading
/// root-owned `/etc/aimx/config.toml`.
fn submit_hook_list_via_daemon() -> Result<String, String> {
    submit_hook_list_raw().map_err(|e| match e {
        MailboxLifecycleFallback::SocketMissing => {
            "aimx daemon not running. Start with 'sudo systemctl start aimx'".to_string()
        }
        MailboxLifecycleFallback::Daemon(msg) => msg,
    })
}

fn submit_hook_list_raw() -> Result<String, MailboxLifecycleFallback> {
    let socket = crate::serve::aimx_socket_path();

    let rt = tokio::runtime::Handle::try_current();
    let io_result: Result<Vec<u8>, std::io::Error> = match rt {
        Ok(handle) => {
            tokio::task::block_in_place(|| handle.block_on(submit_hook_list_request(&socket)))
        }
        Err(_) => {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|e| {
                    MailboxLifecycleFallback::Daemon(format!("Failed to create tokio runtime: {e}"))
                })?;
            rt.block_on(submit_hook_list_request(&socket))
        }
    };

    let raw = io_result.map_err(|e| {
        if is_socket_missing(&e) {
            MailboxLifecycleFallback::SocketMissing
        } else {
            MailboxLifecycleFallback::Daemon(format!(
                "Failed to connect to aimx daemon at {}: {e}",
                socket.display()
            ))
        }
    })?;

    decode_mailbox_list_response(&raw).map_err(MailboxLifecycleFallback::Daemon)
}

/// Open the UDS, ship `AIMX/1 HOOK-LIST`, and return the raw daemon
/// response bytes. Decoding shares the `MAILBOX-LIST` decoder because
/// the wire shape (status line + Content-Length + JSON body) is
/// identical — only the schema of the JSON differs.
async fn submit_hook_list_request(
    socket_path: &std::path::Path,
) -> Result<Vec<u8>, std::io::Error> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let stream = tokio::net::UnixStream::connect(socket_path).await?;
    let (mut reader, mut writer) = stream.into_split();

    send_protocol::write_hook_list_request(&mut writer).await?;
    writer.shutdown().await.ok();

    let mut buf = Vec::with_capacity(1024);
    reader.read_to_end(&mut buf).await?;
    Ok(buf)
}

/// CLI-friendly variant of [`submit_mailbox_list_via_daemon`] that
/// distinguishes socket-missing from a daemon-side error so the
/// caller can decide whether to surface the canonical "daemon must
/// be running" hint or render the daemon's reason verbatim. Mirrors
/// the [`MailboxLifecycleFallback`] shape used by the CRUD path.
pub(crate) fn submit_mailbox_list_via_daemon_for_cli() -> Result<String, MailboxLifecycleFallback> {
    submit_mailbox_list_raw()
}

fn submit_mailbox_list_raw() -> Result<String, MailboxLifecycleFallback> {
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
                .map_err(|e| {
                    MailboxLifecycleFallback::Daemon(format!("Failed to create tokio runtime: {e}"))
                })?;
            rt.block_on(submit_mailbox_list_request(&socket))
        }
    };

    let raw = io_result.map_err(|e| {
        if is_socket_missing(&e) {
            MailboxLifecycleFallback::SocketMissing
        } else {
            MailboxLifecycleFallback::Daemon(format!(
                "Failed to connect to aimx daemon at {}: {e}",
                socket.display()
            ))
        }
    })?;

    decode_mailbox_list_response(&raw).map_err(MailboxLifecycleFallback::Daemon)
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
                    id: id.clone(),
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
                    id: id.clone(),
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

#[cfg(test)]
fn folder_dir(config: &Config, mailbox: &str, folder: Folder) -> PathBuf {
    match folder {
        Folder::Inbox => config.inbox_dir(mailbox),
        Folder::Sent => config.sent_dir(mailbox),
    }
}

/// Pick the folder-specific path a `MAILBOX-LIST` row carries. The
/// daemon already populates `inbox_path` / `sent_path` per row, so MCP
/// tools rendering email pages no longer need to read root-owned
/// `/etc/aimx/config.toml` to compute the path themselves.
fn folder_path_from_row(
    row: &crate::mailbox_list_handler::MailboxListRow,
    folder: Folder,
) -> PathBuf {
    match folder {
        Folder::Inbox => PathBuf::from(&row.inbox_path),
        Folder::Sent => PathBuf::from(&row.sent_path),
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
            signature: None,
            upgrade: None,
        }
    }

    #[test]
    fn root_caller_passes_authorize_mailbox_for_any_action() {
        let tmp = TempDir::new().unwrap();
        let config = build_config(tmp.path(), &[("alice", "root")]);
        let server = AimxMcpServer::with_caller_uid_for_test(0);

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
        let server = AimxMcpServer::with_caller_uid_for_test(1000);

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
        let server = AimxMcpServer::with_caller_uid_for_test(1000);

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
        let server = AimxMcpServer::with_caller_uid_for_test(1000);

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

    #[test]
    fn mailbox_create_params_rejects_owner_field() {
        // S2-3 invariant: there is no `owner` parameter on
        // `mailbox_create`. A stale `owner` field on the wire must
        // surface as a parse error rather than be silently dropped —
        // otherwise an agent could believe it created a mailbox owned
        // by some other principal.
        let json = serde_json::json!({
            "name": "task-42",
            "owner": "root",
        });
        let err = match serde_json::from_value::<MailboxCreateParams>(json) {
            Ok(_) => panic!("expected unknown-field error, got Ok"),
            Err(e) => e,
        };
        assert!(
            err.to_string().contains("unknown field"),
            "expected unknown-field error, got {err}"
        );
    }

    #[test]
    fn mailbox_create_params_accepts_name_only() {
        let json = serde_json::json!({ "name": "task-42" });
        let params: MailboxCreateParams =
            serde_json::from_value(json).expect("name-only params must parse");
        assert_eq!(params.name, "task-42");
    }

    #[test]
    fn mailbox_delete_params_force_defaults_to_none() {
        let json = serde_json::json!({ "name": "task-42" });
        let params: MailboxDeleteParams =
            serde_json::from_value(json).expect("name-only delete params must parse");
        assert_eq!(params.name, "task-42");
        assert!(params.force.is_none());
    }

    #[test]
    fn mailbox_delete_params_accepts_force_true() {
        let json = serde_json::json!({ "name": "task-42", "force": true });
        let params: MailboxDeleteParams =
            serde_json::from_value(json).expect("force=true must parse");
        assert_eq!(params.force, Some(true));
    }

    #[test]
    fn mailbox_delete_force_refuses_catchall_without_touching_disk() {
        // The force-wipe path must reject the catchall mailbox
        // client-side before any wipe attempt or daemon submission.
        // Without this guard a force-wipe on `catchall` would clear
        // the catchall storage even when the daemon would refuse the
        // delete itself.
        let server = AimxMcpServer::with_caller_uid_for_test(1000);
        let err = server
            .mailbox_delete(Parameters(MailboxDeleteParams {
                name: "catchall".to_string(),
                force: Some(true),
            }))
            .unwrap_err();
        assert!(err.contains("catchall"), "{err}");
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
                "text_only",
                "html_body",
            ],
        );
    }

    #[test]
    fn email_reply_params_property_order() {
        assert_eq!(
            property_keys::<EmailReplyParams>(),
            vec!["mailbox", "id", "body", "text_only", "html_body"],
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

    #[test]
    fn mailbox_create_params_property_order() {
        // Per S2-3, the only parameter is `name`. There is no `owner`
        // — by construction the daemon synthesizes the owner from
        // SO_PEERCRED. A future field addition reorders the schema and
        // surfaces here as a test failure so agent-facing behaviour is
        // never silently changed.
        assert_eq!(property_keys::<MailboxCreateParams>(), vec!["name"]);
    }

    #[test]
    fn mailbox_delete_params_property_order() {
        // Per S2-4, the parameters are `name` (required) and
        // `force` (optional, default false).
        assert_eq!(
            property_keys::<MailboxDeleteParams>(),
            vec!["name", "force"]
        );
    }
}

#[cfg(test)]
mod socket_missing_tests {
    use super::is_socket_missing;
    use std::io::{Error, ErrorKind};

    #[test]
    fn flags_not_found_connection_refused_and_permission_denied() {
        for kind in [
            ErrorKind::NotFound,
            ErrorKind::ConnectionRefused,
            ErrorKind::PermissionDenied,
        ] {
            assert!(
                is_socket_missing(&Error::from(kind)),
                "expected {kind:?} to count as socket-missing"
            );
        }
    }

    #[test]
    fn rejects_other_io_errors() {
        for kind in [
            ErrorKind::TimedOut,
            ErrorKind::ConnectionReset,
            ErrorKind::Other,
        ] {
            assert!(
                !is_socket_missing(&Error::from(kind)),
                "expected {kind:?} to NOT count as socket-missing"
            );
        }
    }
}

#[cfg(test)]
mod text_only_html_body_tests {
    //! Unit coverage for the MCP-side mutual-exclusion check and the
    //! `build_send_args` plumbing that forwards `text_only` /
    //! `html_body` from `EmailSendParams` into `SendArgs`. The
    //! integration suite covers the full UDS round-trip; these tests
    //! pin the pure plumbing without paying daemon spawn cost.

    use super::*;

    #[test]
    fn validate_allows_neither_set() {
        assert!(validate_text_only_html_body_exclusion(false, None).is_ok());
        assert!(validate_text_only_html_body_exclusion(false, Some("")).is_ok());
    }

    #[test]
    fn validate_allows_text_only_alone() {
        assert!(validate_text_only_html_body_exclusion(true, None).is_ok());
    }

    #[test]
    fn validate_allows_html_body_alone() {
        assert!(validate_text_only_html_body_exclusion(false, Some("<p>hi</p>")).is_ok());
        // Empty html_body alone (without text_only) is allowed — only
        // the conflict between the two flags trips the check.
        assert!(validate_text_only_html_body_exclusion(false, Some("")).is_ok());
    }

    #[test]
    fn validate_rejects_text_only_plus_empty_html_body() {
        // `Some("")` is treated as supplied to match the wire codec,
        // which carries the empty payload through and rejects the
        // conflict at the daemon. Pre-flighting here keeps the failure
        // identical to a non-empty `html_body` so the operator sees the
        // same wording regardless of which layer tripped.
        let err = validate_text_only_html_body_exclusion(true, Some("")).unwrap_err();
        assert_eq!(
            err,
            "AIMX/1 SEND: --text-only and --html-body are mutually exclusive"
        );
    }

    #[test]
    fn validate_rejects_both_set_with_canonical_error() {
        let err = validate_text_only_html_body_exclusion(true, Some("<p>hi</p>")).unwrap_err();
        // Wording must match the codec's canonical error so operators
        // see one consistent string regardless of which layer fired.
        assert_eq!(
            err,
            "AIMX/1 SEND: --text-only and --html-body are mutually exclusive"
        );
    }

    #[test]
    fn build_send_args_defaults_when_new_params_absent() {
        let params = EmailSendParams {
            from_mailbox: "alice".into(),
            to: "rcpt@example.com".into(),
            subject: "S".into(),
            reply_to: None,
            body: "hello".into(),
            attachments: None,
            references: None,
            text_only: None,
            html_body: None,
        };
        let args = build_send_args(params, "alice@example.com");
        assert!(!args.text_only);
        assert!(args.html_body.is_none());
    }

    #[test]
    fn build_send_args_forwards_text_only_true() {
        let params = EmailSendParams {
            from_mailbox: "alice".into(),
            to: "rcpt@example.com".into(),
            subject: "S".into(),
            reply_to: None,
            body: "Your code: 9999".into(),
            attachments: None,
            references: None,
            text_only: Some(true),
            html_body: None,
        };
        let args = build_send_args(params, "alice@example.com");
        assert!(args.text_only);
        assert!(args.html_body.is_none());
    }

    #[test]
    fn build_send_args_forwards_html_body() {
        let params = EmailSendParams {
            from_mailbox: "alice".into(),
            to: "rcpt@example.com".into(),
            subject: "S".into(),
            reply_to: None,
            body: "fallback".into(),
            attachments: None,
            references: None,
            text_only: None,
            html_body: Some("<p>custom</p>".into()),
        };
        let args = build_send_args(params, "alice@example.com");
        assert!(!args.text_only);
        assert_eq!(args.html_body.as_deref(), Some("<p>custom</p>"));
    }

    #[test]
    fn email_send_params_omitting_new_fields_parses() {
        // Backward compat: existing MCP clients that don't know the new
        // parameters keep working — both fields are `Option<...>` so
        // omission deserializes cleanly to `None`.
        let json = serde_json::json!({
            "from_mailbox": "alice",
            "to": "rcpt@example.com",
            "subject": "hi",
            "body": "hello",
        });
        let params: EmailSendParams =
            serde_json::from_value(json).expect("missing-field params must parse");
        assert!(params.text_only.is_none());
        assert!(params.html_body.is_none());
    }

    #[test]
    fn email_send_params_accepts_text_only_true() {
        let json = serde_json::json!({
            "from_mailbox": "alice",
            "to": "rcpt@example.com",
            "subject": "OTP",
            "body": "Your code: 9999",
            "text_only": true,
        });
        let params: EmailSendParams =
            serde_json::from_value(json).expect("text_only=true must parse");
        assert_eq!(params.text_only, Some(true));
    }

    #[test]
    fn email_send_params_accepts_html_body() {
        let json = serde_json::json!({
            "from_mailbox": "alice",
            "to": "rcpt@example.com",
            "subject": "Branded",
            "body": "fallback",
            "html_body": "<p>custom</p>",
        });
        let params: EmailSendParams = serde_json::from_value(json).expect("html_body must parse");
        assert_eq!(params.html_body.as_deref(), Some("<p>custom</p>"));
    }

    #[test]
    fn email_reply_params_accepts_new_fields() {
        let json = serde_json::json!({
            "mailbox": "alice",
            "id": "2025-06-15-120000-hello",
            "body": "thanks",
            "text_only": true,
        });
        let params: EmailReplyParams =
            serde_json::from_value(json).expect("text_only on reply must parse");
        assert_eq!(params.text_only, Some(true));
        assert!(params.html_body.is_none());
    }
}
