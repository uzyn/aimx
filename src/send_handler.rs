//! Daemon-side handler for `AIMX/1 SEND` UDS requests.
//!
//! This module contains the per-connection business logic that runs inside
//! `aimx serve` after a request frame has been decoded: domain validation,
//! DKIM signing, delivery, and sent-items persistence. Framing is the
//! [`send_protocol`] module's responsibility; this one deals only in parsed
//! `SendRequest`s.
//!
//! The handler is deliberately testable: real MX delivery is abstracted
//! behind the [`MailTransport`](crate::transport::MailTransport) trait so
//! tests can inject a mock.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use rsa::RsaPrivateKey;
use uuid::Uuid;

use crate::config::{Config, ConfigHandle, MailboxConfig};
use crate::dkim;
use crate::frontmatter::{
    DeliveryStatus, OutboundFrontmatter, compute_thread_id, format_outbound_frontmatter,
};
use crate::hook::{self, AfterSendContext, SendStatus};
use crate::ownership::chown_as_owner;
use crate::send_protocol::{ErrCode, SendRequest, SendResponse};
use crate::slug::{allocate_filename, slugify};
use crate::transport::{MailTransport, TransportError};
use crate::uds_authz::{Caller, enforce_mailbox_owner_or_root};

/// Process-scoped lock guarding the outbound critical section: filename
/// allocation + file/directory creation. The daemon is the single writer
/// to `<data_dir>/sent/`, so a process Mutex is sufficient. The lock is
/// held only for the metadata check + `fs::File::create`. The actual
/// file write happens outside the lock. Symmetric to `INGEST_WRITE_LOCK`
/// in `ingest.rs`.
static SENT_WRITE_LOCK: Mutex<()> = Mutex::new(());

/// A single mailbox entry as seen by the send handler. The daemon only
/// needs the configured address (to check for concrete-mailbox match); it
/// never executes triggers or reads `trusted_senders` on the outbound path.
#[derive(Debug, Clone)]
pub struct RegisteredMailbox {
    pub address: String,
}

/// Context shared across every per-connection send.
///
/// Heap-allocated once at daemon startup and cloned (cheap; `Arc` clones)
/// into each task. Holding the DKIM key in an `Arc` here is what lets us
/// load it exactly once despite accepting concurrent sends.
///
/// The `mailboxes` / `primary_domain` fields are resolved live via
/// `config_handle` so a `MAILBOX-CREATE` over UDS is immediately visible
/// to subsequent `SEND` requests without a restart.
pub struct SendContext {
    /// DKIM private key, loaded once at `aimx serve` startup.
    pub dkim_key: Arc<RsaPrivateKey>,
    /// DKIM selector (`<selector>._domainkey.<domain>`).
    pub dkim_selector: String,
    /// Live handle to the daemon's `Config`. Read briefly at the top of
    /// `handle_send` to capture the snapshot used for this request.
    pub config_handle: ConfigHandle,
    /// Transport used for final MX delivery. In production this is a
    /// `LettreTransport`; tests inject a mock.
    pub transport: Arc<dyn MailTransport + Send + Sync>,
    /// Data directory root (`/var/lib/aimx` by default). Sent files are
    /// written to `<data_dir>/sent/<from_mailbox>/`.
    pub data_dir: PathBuf,
}

/// Execute one submitted send end-to-end and return the wire response.
///
/// The flow: validate `From-Mailbox` is registered → parse the `From:`
/// header out of the body → validate the sender domain matches config →
/// DKIM-sign → deliver via MX. Every error path maps to a stable
/// [`ErrCode`].
pub async fn handle_send(req: SendRequest, ctx: &SendContext, caller: &Caller) -> SendResponse {
    handle_send_with_signer(req, ctx, caller, dkim::sign_message).await
}

/// Generic form of [`handle_send`] parameterized on the DKIM signer so tests
/// can inject a failing signer without constructing a bad key. Production
/// code always routes through [`handle_send`], which wires [`dkim::sign_message`].
pub(crate) async fn handle_send_with_signer<F>(
    req: SendRequest,
    ctx: &SendContext,
    caller: &Caller,
    signer: F,
) -> SendResponse
where
    F: FnOnce(&[u8], &RsaPrivateKey, &str, &str) -> Result<Vec<u8>, Box<dyn std::error::Error>>,
{
    // Snapshot the live config at the start of the request. Any
    // MAILBOX-CREATE/DELETE that lands after this point still runs; the
    // swap just doesn't affect the decision for *this* particular send.
    let config = ctx.config_handle.load();
    let primary_domain = config.domain.as_str();
    let mailboxes = config.mailboxes.iter().map(|(name, mb)| {
        (
            name.clone(),
            RegisteredMailbox {
                address: mb.address.clone(),
            },
        )
    });
    let mailboxes: HashMap<String, RegisteredMailbox> = mailboxes.collect();

    let headers = scan_headers(
        &req.body,
        &[
            "From",
            "To",
            "Message-ID",
            "Subject",
            "In-Reply-To",
            "References",
            "Date",
        ],
    );

    let from_header = match headers.get("From") {
        Some(v) => v.clone(),
        None => {
            return SendResponse::Err {
                code: ErrCode::Malformed,
                reason: "missing required header: From".to_string(),
            };
        }
    };

    // The daemon resolves the sender mailbox from the submitted `From:`
    // itself. The client does not send `From-Mailbox:` and does not read
    // `/etc/aimx/config.toml`.
    //
    // The sender domain must equal the configured primary domain
    // (case-insensitive) AND the local part must resolve to an explicitly
    // configured non-wildcard mailbox. Catchall (`*@domain`) is
    // inbound-routing only and never accepted as an outbound sender.

    let bare_from = match extract_bare_address(&from_header) {
        Some(addr) => addr,
        None => {
            return SendResponse::Err {
                code: ErrCode::Malformed,
                reason: format!("could not extract sender address from From: {from_header}"),
            };
        }
    };

    let sender_domain = match bare_from.rfind('@') {
        Some(at) => bare_from[at + 1..].to_string(),
        None => {
            return SendResponse::Err {
                code: ErrCode::Malformed,
                reason: format!("could not extract domain from From: {from_header}"),
            };
        }
    };

    if !sender_domain.eq_ignore_ascii_case(primary_domain) {
        return SendResponse::Err {
            code: ErrCode::Domain,
            reason: format!(
                "sender domain '{sender_domain}' does not match aimx domain '{primary_domain}'"
            ),
        };
    }

    let from_mailbox = match resolve_concrete_mailbox(&mailboxes, &bare_from) {
        Some(name) => name,
        None => {
            return SendResponse::Err {
                code: ErrCode::Mailbox,
                reason: format!(
                    "no mailbox matches From: {bare_from} \
                     (run `aimx mailboxes create <name>` to register one; \
                     catchall is inbound-only)"
                ),
            };
        }
    };

    // Authorize the caller against the resolved
    // mailbox. Non-owners (other than root) get EACCES so a uid bound
    // to mailbox `bob` cannot spoof `From: alice@domain`.
    //
    // `resolve_concrete_mailbox` above guarantees the mailbox exists
    // in `config.mailboxes`, so the `None` branch is unreachable today.
    // We handle it explicitly with `ENOENT` rather than letting authz
    // be silently skipped if a future refactor splits the resolve /
    // lookup pair.
    let mailbox_cfg = match config.mailboxes.get(&from_mailbox) {
        Some(m) => m,
        None => {
            return SendResponse::Err {
                code: ErrCode::Mailbox,
                reason: format!(
                    "mailbox '{from_mailbox}' resolved but not found in config \
                     (race with concurrent MAILBOX-DELETE)"
                ),
            };
        }
    };
    if let Err(reject) = enforce_mailbox_owner_or_root("SEND", caller, &from_mailbox, mailbox_cfg) {
        return SendResponse::Err {
            code: reject.code,
            reason: reject.reason,
        };
    }

    let to_header = match headers.get("To") {
        Some(v) => v.clone(),
        None => {
            return SendResponse::Err {
                code: ErrCode::Malformed,
                reason: "missing required header: To".to_string(),
            };
        }
    };

    // The submitted To: header may carry a display-name (`"Name"
    // <user@host>`), a bare addr (`user@host`), or even angle-only
    // (`<user@host>`). `lettre::Address::FromStr` only parses the bare form,
    // so normalize to `user@host` before handing it to the transport. Any
    // failure to extract a bare recipient is MALFORMED, not a delivery
    // error, because nothing has been attempted over the wire.
    let recipient_bare = match extract_bare_address(&to_header) {
        Some(addr) => addr,
        None => {
            return SendResponse::Err {
                code: ErrCode::Malformed,
                reason: format!("could not extract recipient address from To: {to_header}"),
            };
        }
    };

    // If Message-ID is absent we synthesize one ourselves rather than
    // erroring out: Message-ID is not a required client header, and
    // `AIMX/1 OK <message-id>` still needs something to echo. Using the
    // configured primary domain matches the DKIM `d=` tag and avoids
    // leaking a recipient-side hostname.
    let (message_id, body_with_id) = match headers.get("Message-ID") {
        Some(v) => (v.clone(), req.body.clone()),
        None => {
            let synthetic = format!("<{}@{}>", Uuid::new_v4(), primary_domain);
            let injected = inject_message_id_header(&req.body, &synthetic);
            (synthetic, injected)
        }
    };

    // Default Markdown send path: render the Markdown body to HTML and
    // assemble the multipart/alternative (or nested multipart/mixed when
    // the client packed attachments) shape daemon-side. The signature
    // is appended to the body on every path so the recipient always
    // sees it in the text/plain region (Markdown source on the default
    // path, body verbatim on `--text-only`, text fallback on
    // `--html-body`). The two escape-hatch fields on the SEND frame
    // select an alternate wire shape: `text_only=true` ships single-part
    // text/plain, and `html_body=Some(..)` ships multipart/alternative
    // whose HTML part is the operator-supplied bytes verbatim. Operators
    // wanting the signature in the HTML view of an `--html-body` send
    // put it inside their HTML template — the daemon never modifies the
    // supplied HTML bytes.
    let body_bytes = match crate::wire_assembly::assemble_wire_message(
        &body_with_id,
        config.effective_signature(),
        req.text_only,
        req.html_body.as_deref(),
    ) {
        Ok(bytes) => bytes,
        Err(e) => {
            // Surface the renderer's size-cap rejection on a dedicated
            // wire code so scripts can branch on the failure without
            // parsing the reason string. The canonical message stays in
            // the reason field for human readers.
            let code = match &e {
                crate::wire_assembly::AssembleError::Render(
                    crate::markdown_render::MarkdownRenderError::BodyTooLarge,
                ) => ErrCode::BodyTooLarge,
                _ => ErrCode::Malformed,
            };
            return SendResponse::Err {
                code,
                reason: e.to_string(),
            };
        }
    };

    let signed = match signer(
        &body_bytes,
        &ctx.dkim_key,
        primary_domain,
        &ctx.dkim_selector,
    ) {
        Ok(bytes) => bytes,
        Err(e) => {
            return SendResponse::Err {
                code: ErrCode::Sign,
                reason: e.to_string(),
            };
        }
    };

    let subject = headers.get("Subject").cloned().unwrap_or_default();
    let in_reply_to = headers.get("In-Reply-To").cloned();
    let references_val = headers.get("References").cloned();
    let date_header = headers.get("Date").cloned();

    // Audit-trail field for the sent record. Mirrors the wire-shape
    // branch picked in `assemble_wire_message` above:
    //   - `text_only=true`            → `"text"`  (single-part text/plain)
    //   - `html_body=Some(...)`       → `"html"`  (custom HTML alternative)
    //   - default                     → `"markdown"` (rendered alternative)
    // The two escape hatches are mutually exclusive at the codec layer,
    // so this match is exhaustive without a tie-breaker.
    let outbound_format = if req.text_only {
        "text"
    } else if req.html_body.is_some() {
        "html"
    } else {
        "markdown"
    };

    let send_result = ctx.transport.send(&from_header, &recipient_bare, &signed);

    let (send_status, persisted_path, response) = match send_result {
        Ok(_server) => {
            let delivered_at = chrono::Utc::now().to_rfc3339();
            let path = persist_sent_file(
                ctx,
                &config,
                &from_mailbox,
                &message_id,
                &from_header,
                &to_header,
                &subject,
                date_header.as_deref(),
                in_reply_to.as_deref(),
                references_val.as_deref(),
                &signed,
                DeliveryStatus::Delivered,
                Some(&delivered_at),
                None,
                outbound_format,
            );
            (
                SendStatus::Delivered,
                path,
                SendResponse::Ok {
                    message_id: message_id.clone(),
                },
            )
        }
        Err(e) => {
            let msg = e.to_string();
            let (code, status) = match &e {
                TransportError::Temp(_) => (ErrCode::Temp, SendStatus::Deferred),
                TransportError::Permanent(_) => (ErrCode::Delivery, SendStatus::Failed),
            };
            // TEMP errors: do NOT persist. The client sees the transient
            // error and may retry, so writing a "failed" record would be
            // premature. Only permanent delivery failures (DELIVERY) get
            // persisted.
            let path = if code == ErrCode::Delivery {
                persist_sent_file(
                    ctx,
                    &config,
                    &from_mailbox,
                    &message_id,
                    &from_header,
                    &to_header,
                    &subject,
                    date_header.as_deref(),
                    in_reply_to.as_deref(),
                    references_val.as_deref(),
                    &signed,
                    DeliveryStatus::Failed,
                    None,
                    Some(&msg),
                    outbound_format,
                )
            } else {
                None
            };
            (status, path, SendResponse::Err { code, reason: msg })
        }
    };

    // Fire `after_send` hooks for the from-mailbox.
    // Synchronous: daemon awaits subprocess completion for predictable
    // timing, but exit code is discarded. Failures cannot affect the
    // outbound result the client already expects.
    fire_after_send_hooks(
        &config,
        &from_mailbox,
        &from_header,
        &to_header,
        &subject,
        &message_id,
        persisted_path.as_deref(),
        send_status,
    );

    response
}

#[allow(clippy::too_many_arguments)]
fn fire_after_send_hooks(
    config: &crate::config::Config,
    from_mailbox: &str,
    from_header: &str,
    to_header: &str,
    subject: &str,
    message_id: &str,
    persisted_path: Option<&std::path::Path>,
    send_status: SendStatus,
) {
    let Some(mailbox_config) = config.mailboxes.get(from_mailbox) else {
        return;
    };
    if !has_any_after_send(mailbox_config) {
        return;
    }
    let filepath = persisted_path
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    let ctx = AfterSendContext {
        mailbox: from_mailbox,
        from: from_header,
        to: to_header,
        subject,
        filepath: &filepath,
        message_id,
        send_status,
    };
    hook::execute_after_send(config, mailbox_config, &ctx);
}

fn has_any_after_send(mailbox: &MailboxConfig) -> bool {
    mailbox.after_send_hooks().next().is_some()
}

/// Resolve the sender local part to a concrete registered mailbox name.
/// Tries (in order): exact `address` match → mailbox name equal to the
/// local part (for the common case where name == local). Returns `None`
/// when nothing concrete matches. There is no catchall (`*@domain`)
/// fallback. Catchall is inbound-only.
fn resolve_concrete_mailbox(
    mailboxes: &HashMap<String, RegisteredMailbox>,
    bare_from: &str,
) -> Option<String> {
    for (name, mb) in mailboxes {
        if mb.address.starts_with('*') {
            continue;
        }
        if mb.address.eq_ignore_ascii_case(bare_from) {
            return Some(name.clone());
        }
    }

    let local = bare_from
        .rfind('@')
        .map(|i| &bare_from[..i])
        .unwrap_or(bare_from);
    if let Some(mb) = mailboxes.get(local)
        && !mb.address.starts_with('*')
    {
        return Some(local.to_string());
    }

    None
}

/// Insert a `Message-ID:` header at the top of an RFC 5322 message body. The
/// body's existing line-endings (CRLF or LF) are preserved by reusing the
/// same terminator the first header uses.
fn inject_message_id_header(body: &[u8], message_id: &str) -> Vec<u8> {
    let terminator: &[u8] = if body.windows(2).take(1024).any(|w| w == b"\r\n") {
        b"\r\n"
    } else {
        b"\n"
    };
    let mut out = Vec::with_capacity(body.len() + 32 + message_id.len());
    out.extend_from_slice(b"Message-ID: ");
    out.extend_from_slice(message_id.as_bytes());
    out.extend_from_slice(terminator);
    out.extend_from_slice(body);
    out
}

/// Single-pass walk over an RFC 5322 header block, returning the values for
/// each of `names` (case-insensitive, continuation-line folded). The returned
/// map keys match the original `names` argument casing so callers can look
/// up using the literal name they asked for.
fn scan_headers(body: &[u8], names: &[&str]) -> std::collections::HashMap<String, String> {
    let mut out: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    let text = match std::str::from_utf8(body) {
        Ok(t) => t,
        Err(_) => return out,
    };

    let lowercased: Vec<(String, &str)> =
        names.iter().map(|n| (n.to_ascii_lowercase(), *n)).collect();

    let mut current: Option<String> = None;

    let commit = |current: &Option<String>,
                  out: &mut std::collections::HashMap<String, String>,
                  lowercased: &[(String, &str)]| {
        let Some(line) = current.as_ref() else {
            return;
        };
        let Some((n, v)) = line.split_once(':') else {
            return;
        };
        let n_lower = n.trim().to_ascii_lowercase();
        for (target_lower, original) in lowercased {
            if n_lower == *target_lower && !out.contains_key(*original) {
                out.insert((*original).to_string(), v.trim().to_string());
            }
        }
    };

    for line in text.lines() {
        if line.is_empty() {
            break;
        }
        if line.starts_with(' ') || line.starts_with('\t') {
            if let Some(cur) = current.as_mut() {
                cur.push(' ');
                cur.push_str(line.trim_start());
            }
            continue;
        }
        commit(&current, &mut out, &lowercased);
        current = Some(line.to_string());
    }
    commit(&current, &mut out, &lowercased);
    out
}

/// Extract the bare-addr domain from an RFC 5322 `From:` header, handling
/// both `"Name <user@host>"` and `"user@host"` forms. Returns `None` if no
/// `@` is present. Retained for test coverage; the main path inlines the
/// rfind('@') lookup on the already-extracted bare address.
#[cfg(test)]
fn extract_domain(from: &str) -> Option<String> {
    let addr = extract_bare_address(from)?;
    let at = addr.rfind('@')?;
    Some(addr[at + 1..].trim().to_string())
}

/// Extract the bare `local@host` form from a header value, accepting
/// `"Name" <local@host>`, `local@host`, and angle-only `<local@host>`. For
/// comma-separated header values only the first recipient is returned;
/// v0.2 submissions are single-recipient and the daemon's envelope already
/// only takes one address.
fn extract_bare_address(value: &str) -> Option<String> {
    let first = value.split(',').next().unwrap_or(value).trim();
    if first.is_empty() {
        return None;
    }
    let addr = if let Some(start) = first.rfind('<') {
        let tail = &first[start + 1..];
        let end = tail.find('>').unwrap_or(tail.len());
        &tail[..end]
    } else {
        first
    };
    let addr = addr.trim();
    if addr.is_empty() || !addr.contains('@') {
        return None;
    }
    Some(addr.to_string())
}

#[allow(clippy::too_many_arguments)]
fn persist_sent_file(
    ctx: &SendContext,
    config: &Config,
    from_mailbox: &str,
    message_id: &str,
    from_header: &str,
    to_header: &str,
    subject: &str,
    date_header: Option<&str>,
    in_reply_to: Option<&str>,
    references: Option<&str>,
    signed_bytes: &[u8],
    delivery_status: DeliveryStatus,
    delivered_at: Option<&str>,
    delivery_details: Option<&str>,
    outbound_format: &str,
) -> Option<PathBuf> {
    let sent_dir = ctx.data_dir.join("sent").join(from_mailbox);
    if let Err(e) = std::fs::create_dir_all(&sent_dir) {
        eprintln!(
            "[send] failed to create sent dir {}: {e}",
            sent_dir.display()
        );
        return None;
    }
    // Chown the sent directory (idempotent when already correct; heals
    // drift otherwise). The mailbox lookup can only miss in exotic
    // cases because `from_mailbox` was resolved from `config.mailboxes`
    // earlier in the request.
    let mailbox_cfg = config.mailboxes.get(from_mailbox);
    if let Some(mb_cfg) = mailbox_cfg
        && let Err(e) = chown_as_owner(&sent_dir, mb_cfg, 0o700)
    {
        tracing::warn!(
            target: "aimx::send",
            "chown sent dir failed mailbox={from_mailbox} path={path} err={err}",
            from_mailbox = from_mailbox,
            path = sent_dir.display(),
            err = e,
        );
    }

    let slug = slugify(subject);
    let timestamp = chrono::Utc::now();
    let has_attachments = false; // Outbound via UDS is text-only for now (v0.2).

    let thread_id = compute_thread_id(message_id, in_reply_to, references);

    let date = date_header
        .map(|d| d.to_string())
        .unwrap_or_else(|| chrono::Utc::now().to_rfc3339());

    let meta = OutboundFrontmatter {
        id: String::new(), // filled after allocation
        message_id: message_id.to_string(),
        thread_id,
        from: from_header.to_string(),
        to: to_header.to_string(),
        cc: None,
        reply_to: None,
        delivered_to: to_header.to_string(),
        subject: subject.to_string(),
        date,
        received_at: String::new(),
        received_from_ip: None,
        size_bytes: signed_bytes.len(),
        attachments: vec![],
        in_reply_to: in_reply_to.map(|s| s.to_string()),
        references: references.map(|s| s.to_string()),
        list_id: None,
        auto_submitted: None,
        dkim: "pass".to_string(),
        spf: "none".to_string(),
        dmarc: "none".to_string(),
        trusted: "none".to_string(),
        mailbox: from_mailbox.to_string(),
        read: false,
        labels: vec![],
        outbound: true,
        outbound_format: outbound_format.to_string(),
        delivery_status,
        bcc: None,
        delivered_at: delivered_at.map(|s| s.to_string()),
        delivery_details: delivery_details.map(|s| s.to_string()),
    };

    // Critical section: allocate filename + create the file atomically.
    let (md_path, id) = {
        let _guard = SENT_WRITE_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        let md_path = allocate_filename(&sent_dir, timestamp, &slug, has_attachments);
        let parent_dir = md_path.parent().unwrap_or(&sent_dir);

        if let Err(e) = std::fs::create_dir_all(parent_dir) {
            eprintln!(
                "[send] failed to create parent dir {}: {e}",
                parent_dir.display()
            );
            return None;
        }

        let id = md_path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or_default()
            .to_string();

        // Touch the file to claim the path before releasing the lock.
        if let Err(e) = std::fs::File::create_new(&md_path) {
            eprintln!(
                "[send] failed to create sent file {}: {e}",
                md_path.display()
            );
            return None;
        }

        (md_path, id)
    };

    // Write the actual content outside the lock.
    let mut meta = meta;
    meta.id = id;

    let body = String::from_utf8_lossy(signed_bytes);
    let content = format_outbound_frontmatter(&meta, &body);

    if let Err(e) = std::fs::write(&md_path, content) {
        eprintln!(
            "[send] failed to write sent file {}: {e}",
            md_path.display()
        );
        return None;
    }

    // Chown the newly-written sent file to the mailbox owner (PRD §6.3).
    // Mode `0o600` — only the owner can read. Failures are logged but
    // not fatal: the file sits inside a `0o700` directory, so non-
    // owners cannot traverse to reach it.
    if let Some(mb_cfg) = mailbox_cfg
        && let Err(e) = chown_as_owner(&md_path, mb_cfg, 0o600)
    {
        tracing::warn!(
            target: "aimx::send",
            "chown sent file failed mailbox={from_mailbox} path={path} err={err}",
            from_mailbox = from_mailbox,
            path = md_path.display(),
            err = e,
        );
    }

    Some(md_path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    struct MockTransport {
        captured: Mutex<Vec<Vec<u8>>>,
        behavior: Behavior,
    }

    enum Behavior {
        Ok,
        TempErr(String),
        PermanentErr(String),
    }

    impl MailTransport for MockTransport {
        fn send(
            &self,
            _sender: &str,
            _recipient: &str,
            message: &[u8],
        ) -> Result<String, crate::transport::TransportError> {
            match &self.behavior {
                Behavior::Ok => {
                    self.captured.lock().unwrap().push(message.to_vec());
                    Ok("mock-mx.example.com".to_string())
                }
                Behavior::TempErr(s) => Err(crate::transport::TransportError::Temp(s.clone())),
                Behavior::PermanentErr(s) => {
                    Err(crate::transport::TransportError::Permanent(s.clone()))
                }
            }
        }
    }

    fn test_ctx(transport: Arc<dyn MailTransport + Send + Sync>) -> SendContext {
        test_ctx_with_data_dir(transport, None)
    }

    fn test_ctx_with_data_dir(
        transport: Arc<dyn MailTransport + Send + Sync>,
        data_dir: Option<std::path::PathBuf>,
    ) -> SendContext {
        use tempfile::TempDir;
        let tmp = TempDir::new().unwrap();
        dkim::generate_keypair(tmp.path(), false).unwrap();
        let key = dkim::load_private_key(tmp.path()).unwrap();
        let dir = data_dir.unwrap_or_else(|| tmp.path().to_path_buf());
        let mut mailboxes = std::collections::HashMap::new();
        mailboxes.insert(
            "catchall".to_string(),
            crate::config::MailboxConfig {
                address: "*@example.com".to_string(),
                owner: "aimx-catchall".to_string(),
                hooks: vec![],
                trust: None,
                trusted_senders: None,
                allow_root_catchall: false,
            },
        );
        mailboxes.insert(
            "alice".to_string(),
            crate::config::MailboxConfig {
                address: "alice@example.com".to_string(),
                owner: "root".to_string(),
                hooks: vec![],
                trust: None,
                trusted_senders: None,
                allow_root_catchall: false,
            },
        );
        let config = crate::config::Config {
            domain: "example.com".to_string(),
            data_dir: dir.clone(),
            dkim_selector: "aimx".to_string(),
            trust: "none".to_string(),
            trusted_senders: vec![],
            mailboxes,
            verify_host: None,
            enable_ipv6: false,
            signature: None,
            upgrade: None,
        };
        let config_handle = ConfigHandle::new(config);
        SendContext {
            dkim_key: Arc::new(key),
            dkim_selector: "aimx".to_string(),
            config_handle,
            transport,
            data_dir: dir,
        }
    }

    fn body(from: &str) -> Vec<u8> {
        format!(
            "From: {from}\r\n\
             To: user@gmail.com\r\n\
             Subject: Hi\r\n\
             Date: Thu, 01 Jan 2025 12:00:00 +0000\r\n\
             Message-ID: <abc@example.com>\r\n\
             \r\n\
             hello\r\n"
        )
        .into_bytes()
    }

    #[tokio::test]
    async fn ok_path_signs_and_delivers() {
        let mock = Arc::new(MockTransport {
            captured: Mutex::new(vec![]),
            behavior: Behavior::Ok,
        });
        let ctx = test_ctx(mock.clone());
        let req = SendRequest {
            body: body("alice@example.com"),
            ..Default::default()
        };
        let resp = handle_send(req, &ctx, &Caller::internal_root()).await;
        match resp {
            SendResponse::Ok { message_id } => {
                assert_eq!(message_id, "<abc@example.com>");
            }
            other => panic!("expected Ok, got {other:?}"),
        }
        let captured = mock.captured.lock().unwrap();
        assert_eq!(captured.len(), 1);
        let delivered = String::from_utf8_lossy(&captured[0]);
        assert!(
            delivered.starts_with("DKIM-Signature:"),
            "delivered message must start with DKIM-Signature: {delivered}"
        );
        assert!(delivered.contains("d=example.com"));
    }

    #[tokio::test]
    async fn bogus_local_part_under_config_domain_returns_mailbox_error() {
        // Wildcard fallback is gone. Sending as a local part that
        // doesn't match a concrete registered mailbox must be rejected
        // with ERR MAILBOX even when the domain matches the configured one.
        let mock = Arc::new(MockTransport {
            captured: Mutex::new(vec![]),
            behavior: Behavior::Ok,
        });
        let ctx = test_ctx(mock);
        let req = SendRequest {
            body: body("bogus@example.com"),
            ..Default::default()
        };
        let resp = handle_send(req, &ctx, &Caller::internal_root()).await;
        match resp {
            SendResponse::Err { code, reason } => {
                assert_eq!(code, ErrCode::Mailbox);
                assert!(reason.contains("bogus@example.com"), "{reason}");
                assert!(
                    reason.contains("aimx mailboxes create"),
                    "error should point operator at aimx mailboxes create: {reason}"
                );
            }
            other => panic!("expected Err, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn wildcard_catchall_is_never_accepted_as_outbound_sender() {
        // Even if `catchall` has address `*@example.com`, sending
        // `catchall@example.com` must not succeed: catchall is inbound-only.
        let mock = Arc::new(MockTransport {
            captured: Mutex::new(vec![]),
            behavior: Behavior::Ok,
        });
        let ctx = test_ctx(mock);
        let req = SendRequest {
            body: body("catchall@example.com"),
            ..Default::default()
        };
        let resp = handle_send(req, &ctx, &Caller::internal_root()).await;
        match resp {
            SendResponse::Err { code, .. } => assert_eq!(code, ErrCode::Mailbox),
            other => panic!("expected Err(MAILBOX), got {other:?}"),
        }
    }

    #[tokio::test]
    async fn domain_mismatch_returns_domain_error() {
        let mock = Arc::new(MockTransport {
            captured: Mutex::new(vec![]),
            behavior: Behavior::Ok,
        });
        let ctx = test_ctx(mock);
        let req = SendRequest {
            body: body("alice@other.org"),
            ..Default::default()
        };
        let resp = handle_send(req, &ctx, &Caller::internal_root()).await;
        match resp {
            SendResponse::Err { code, reason } => {
                assert_eq!(code, ErrCode::Domain);
                assert!(reason.contains("other.org"), "{reason}");
            }
            other => panic!("expected Err, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn domain_match_is_case_insensitive() {
        let mock = Arc::new(MockTransport {
            captured: Mutex::new(vec![]),
            behavior: Behavior::Ok,
        });
        let ctx = test_ctx(mock);
        let req = SendRequest {
            body: body("alice@EXAMPLE.COM"),
            ..Default::default()
        };
        let resp = handle_send(req, &ctx, &Caller::internal_root()).await;
        assert!(matches!(resp, SendResponse::Ok { .. }), "{resp:?}");
    }

    #[tokio::test]
    async fn concrete_mailbox_under_config_domain_is_accepted() {
        // Sending as a registered concrete mailbox (not the
        // wildcard catchall) under the configured domain succeeds end-to-end.
        let mock = Arc::new(MockTransport {
            captured: Mutex::new(vec![]),
            behavior: Behavior::Ok,
        });
        let ctx = test_ctx(mock);
        let req = SendRequest {
            body: body("alice@example.com"),
            ..Default::default()
        };
        let resp = handle_send(req, &ctx, &Caller::internal_root()).await;
        assert!(matches!(resp, SendResponse::Ok { .. }), "{resp:?}");
    }

    #[tokio::test]
    async fn display_name_from_header_resolves_to_concrete_mailbox() {
        // Display-name forms like `Alice <alice@example.com>` must still
        // resolve to the registered mailbox named `alice`.
        let mock = Arc::new(MockTransport {
            captured: Mutex::new(vec![]),
            behavior: Behavior::Ok,
        });
        let ctx = test_ctx(mock);
        let req = SendRequest {
            body: body("Alice <alice@example.com>"),
            ..Default::default()
        };
        let resp = handle_send(req, &ctx, &Caller::internal_root()).await;
        assert!(matches!(resp, SendResponse::Ok { .. }), "{resp:?}");
    }

    #[tokio::test]
    async fn missing_from_returns_malformed() {
        let mock = Arc::new(MockTransport {
            captured: Mutex::new(vec![]),
            behavior: Behavior::Ok,
        });
        let ctx = test_ctx(mock);
        let body = b"To: user@gmail.com\r\n\
                     Subject: Hi\r\n\
                     Message-ID: <abc@example.com>\r\n\
                     \r\n\
                     hello\r\n"
            .to_vec();
        let req = SendRequest {
            body,
            ..Default::default()
        };
        let resp = handle_send(req, &ctx, &Caller::internal_root()).await;
        match resp {
            SendResponse::Err { code, .. } => assert_eq!(code, ErrCode::Malformed),
            other => panic!("expected Err, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn transport_permanent_error_maps_to_delivery() {
        let mock = Arc::new(MockTransport {
            captured: Mutex::new(vec![]),
            behavior: Behavior::PermanentErr(
                "Rejected by mx.example.com: 550 no such user".to_string(),
            ),
        });
        let ctx = test_ctx(mock);
        let req = SendRequest {
            body: body("alice@example.com"),
            ..Default::default()
        };
        let resp = handle_send(req, &ctx, &Caller::internal_root()).await;
        match resp {
            SendResponse::Err { code, reason } => {
                assert_eq!(code, ErrCode::Delivery);
                assert!(reason.contains("550"), "{reason}");
            }
            other => panic!("expected Err, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn transport_unreachable_maps_to_temp() {
        let mock = Arc::new(MockTransport {
            captured: Mutex::new(vec![]),
            behavior: Behavior::TempErr(
                "All MX servers for gmail.com unreachable: ...".to_string(),
            ),
        });
        let ctx = test_ctx(mock);
        let req = SendRequest {
            body: body("alice@example.com"),
            ..Default::default()
        };
        let resp = handle_send(req, &ctx, &Caller::internal_root()).await;
        match resp {
            SendResponse::Err { code, .. } => assert_eq!(code, ErrCode::Temp),
            other => panic!("expected Err, got {other:?}"),
        }
    }

    #[test]
    fn extract_domain_name_form() {
        assert_eq!(
            extract_domain("Alice <alice@example.com>"),
            Some("example.com".to_string())
        );
    }

    #[test]
    fn extract_domain_bare_form() {
        assert_eq!(
            extract_domain("alice@example.com"),
            Some("example.com".to_string())
        );
    }

    #[test]
    fn extract_domain_none() {
        assert_eq!(extract_domain("no-at-sign"), None);
    }

    #[test]
    fn scan_headers_simple_multi() {
        let body = b"From: alice@example.com\r\nTo: bob@x.com\r\nSubject: hi\r\n\r\nbody";
        let h = scan_headers(body, &["From", "To", "Subject"]);
        assert_eq!(h.get("From"), Some(&"alice@example.com".to_string()));
        assert_eq!(h.get("To"), Some(&"bob@x.com".to_string()));
        assert_eq!(h.get("Subject"), Some(&"hi".to_string()));
    }

    #[test]
    fn scan_headers_case_insensitive() {
        let body = b"fRoM: alice@example.com\r\n\r\n";
        let h = scan_headers(body, &["FROM"]);
        assert_eq!(h.get("FROM"), Some(&"alice@example.com".to_string()));
    }

    #[test]
    fn scan_headers_continuation_line_joined() {
        let body = b"Subject: one\r\n two\r\n\r\n";
        let h = scan_headers(body, &["Subject"]);
        assert_eq!(h.get("Subject"), Some(&"one two".to_string()));
    }

    #[test]
    fn scan_headers_missing_is_absent() {
        let body = b"From: a@b.com\r\n\r\n";
        let h = scan_headers(body, &["X-Nope"]);
        assert!(!h.contains_key("X-Nope"));
    }

    #[test]
    fn extract_bare_address_display_name_form() {
        assert_eq!(
            extract_bare_address("Alice <alice@example.com>"),
            Some("alice@example.com".to_string())
        );
    }

    #[test]
    fn extract_bare_address_bare_form() {
        assert_eq!(
            extract_bare_address("alice@example.com"),
            Some("alice@example.com".to_string())
        );
    }

    #[test]
    fn extract_bare_address_angle_only() {
        assert_eq!(
            extract_bare_address("<alice@example.com>"),
            Some("alice@example.com".to_string())
        );
    }

    #[test]
    fn extract_bare_address_takes_first_of_list() {
        assert_eq!(
            extract_bare_address("a@x.com, b@y.com"),
            Some("a@x.com".to_string())
        );
    }

    #[test]
    fn extract_bare_address_none_without_at() {
        assert!(extract_bare_address("no-at-sign").is_none());
        assert!(extract_bare_address("").is_none());
    }

    #[tokio::test]
    async fn to_header_with_display_name_delivers_bare_address() {
        let mock = Arc::new(MockTransport {
            captured: Mutex::new(vec![]),
            behavior: Behavior::Ok,
        });
        let ctx = test_ctx(mock.clone());
        // `To:` carries a display name. The handler must normalize to the
        // bare addr before calling the transport; otherwise the lettre
        // `Address::FromStr` parse at the transport layer would fail and
        // we would have mapped an RFC 5322-valid header into `ERR DELIVERY`.
        let body = b"From: alice@example.com\r\n\
                     To: Bob <bob@gmail.com>\r\n\
                     Subject: Hi\r\n\
                     Date: Thu, 01 Jan 2025 12:00:00 +0000\r\n\
                     Message-ID: <abc@example.com>\r\n\
                     \r\n\
                     hello\r\n"
            .to_vec();
        let req = SendRequest {
            body,
            ..Default::default()
        };
        let resp = handle_send(req, &ctx, &Caller::internal_root()).await;
        assert!(matches!(resp, SendResponse::Ok { .. }), "{resp:?}");
    }

    #[tokio::test]
    async fn missing_message_id_is_synthesized() {
        let mock = Arc::new(MockTransport {
            captured: Mutex::new(vec![]),
            behavior: Behavior::Ok,
        });
        let ctx = test_ctx(mock.clone());
        let body = b"From: alice@example.com\r\n\
                     To: user@gmail.com\r\n\
                     Subject: Hi\r\n\
                     Date: Thu, 01 Jan 2025 12:00:00 +0000\r\n\
                     \r\n\
                     hello\r\n"
            .to_vec();
        let req = SendRequest {
            body,
            ..Default::default()
        };
        let resp = handle_send(req, &ctx, &Caller::internal_root()).await;
        match resp {
            SendResponse::Ok { message_id } => {
                assert!(
                    message_id.starts_with('<') && message_id.ends_with('>'),
                    "message_id should be angle-bracketed: {message_id}"
                );
                assert!(
                    message_id.contains("@example.com>"),
                    "synthesized Message-ID must use primary domain: {message_id}"
                );
            }
            other => panic!("expected Ok, got {other:?}"),
        }
        // The delivered message should contain the synthesized header.
        let captured = mock.captured.lock().unwrap();
        let delivered = String::from_utf8_lossy(&captured[0]);
        assert!(
            delivered.contains("Message-ID: <"),
            "synthesized Message-ID must be part of the signed message: {delivered}"
        );
    }

    #[tokio::test]
    async fn sign_failure_returns_sign_error() {
        let mock = Arc::new(MockTransport {
            captured: Mutex::new(vec![]),
            behavior: Behavior::Ok,
        });
        let ctx = test_ctx(mock);
        let req = SendRequest {
            body: body("alice@example.com"),
            ..Default::default()
        };
        // Inject a signer that always fails, to exercise the `ERR SIGN`
        // branch without needing a malformed RSA key.
        let failing_signer = |_: &[u8],
                              _: &RsaPrivateKey,
                              _: &str,
                              _: &str|
         -> Result<Vec<u8>, Box<dyn std::error::Error>> {
            Err("simulated DKIM signing failure".into())
        };
        let resp =
            handle_send_with_signer(req, &ctx, &Caller::internal_root(), failing_signer).await;
        match resp {
            SendResponse::Err { code, reason } => {
                assert_eq!(code, ErrCode::Sign);
                assert!(
                    reason.contains("simulated DKIM signing failure"),
                    "{reason}"
                );
            }
            other => panic!("expected Err(SIGN), got {other:?}"),
        }
    }

    #[tokio::test]
    async fn successful_send_persists_sent_file() {
        let data_dir = tempfile::TempDir::new().unwrap();
        let mock = Arc::new(MockTransport {
            captured: Mutex::new(vec![]),
            behavior: Behavior::Ok,
        });
        let ctx = test_ctx_with_data_dir(mock, Some(data_dir.path().to_path_buf()));
        let req = SendRequest {
            body: body("alice@example.com"),
            ..Default::default()
        };
        let resp = handle_send(req, &ctx, &Caller::internal_root()).await;
        assert!(matches!(resp, SendResponse::Ok { .. }), "{resp:?}");

        let sent_dir = data_dir.path().join("sent").join("alice");
        assert!(sent_dir.exists(), "sent dir should be created");

        let entries: Vec<_> = std::fs::read_dir(&sent_dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .collect();
        assert_eq!(entries.len(), 1, "one sent file");

        let content = std::fs::read_to_string(entries[0].path()).unwrap();
        assert!(content.contains("delivery_status = \"delivered\""));
        assert!(content.contains("outbound = true"));
        assert!(content.contains("delivered_at ="));
        assert!(content.contains("DKIM-Signature:"));
        assert!(content.contains("dkim = \"pass\""));
    }

    #[tokio::test]
    async fn permanent_failure_persists_sent_file_with_failed_status() {
        let data_dir = tempfile::TempDir::new().unwrap();
        let mock = Arc::new(MockTransport {
            captured: Mutex::new(vec![]),
            behavior: Behavior::PermanentErr(
                "Rejected by mx.example.com: 550 no such user".to_string(),
            ),
        });
        let ctx = test_ctx_with_data_dir(mock, Some(data_dir.path().to_path_buf()));
        let req = SendRequest {
            body: body("alice@example.com"),
            ..Default::default()
        };
        let resp = handle_send(req, &ctx, &Caller::internal_root()).await;
        assert!(matches!(resp, SendResponse::Err { .. }), "{resp:?}");

        let sent_dir = data_dir.path().join("sent").join("alice");
        let entries: Vec<_> = std::fs::read_dir(&sent_dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .collect();
        assert_eq!(entries.len(), 1, "failed send persists a file");

        let content = std::fs::read_to_string(entries[0].path()).unwrap();
        assert!(content.contains("delivery_status = \"failed\""));
        assert!(content.contains("delivery_details ="));
        assert!(content.contains("550 no such user"));
    }

    #[tokio::test]
    async fn temp_error_does_not_persist_sent_file() {
        let data_dir = tempfile::TempDir::new().unwrap();
        let mock = Arc::new(MockTransport {
            captured: Mutex::new(vec![]),
            behavior: Behavior::TempErr(
                "All MX servers for gmail.com unreachable: ...".to_string(),
            ),
        });
        let ctx = test_ctx_with_data_dir(mock, Some(data_dir.path().to_path_buf()));
        let req = SendRequest {
            body: body("alice@example.com"),
            ..Default::default()
        };
        let resp = handle_send(req, &ctx, &Caller::internal_root()).await;
        assert!(matches!(resp, SendResponse::Err { .. }), "{resp:?}");

        let sent_dir = data_dir.path().join("sent").join("alice");
        if sent_dir.exists() {
            let entries: Vec<_> = std::fs::read_dir(&sent_dir)
                .unwrap()
                .filter_map(|e| e.ok())
                .collect();
            assert_eq!(entries.len(), 0, "TEMP errors should not persist");
        }
    }

    #[tokio::test]
    async fn sent_file_frontmatter_roundtrips() {
        let data_dir = tempfile::TempDir::new().unwrap();
        let mock = Arc::new(MockTransport {
            captured: Mutex::new(vec![]),
            behavior: Behavior::Ok,
        });
        let ctx = test_ctx_with_data_dir(mock, Some(data_dir.path().to_path_buf()));
        let req = SendRequest {
            body: body("alice@example.com"),
            ..Default::default()
        };
        let resp = handle_send(req, &ctx, &Caller::internal_root()).await;
        assert!(matches!(resp, SendResponse::Ok { .. }));

        let sent_dir = data_dir.path().join("sent").join("alice");
        let entries: Vec<_> = std::fs::read_dir(&sent_dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .collect();
        let content = std::fs::read_to_string(entries[0].path()).unwrap();

        // Parse frontmatter back
        let parts: Vec<&str> = content.splitn(3, "+++").collect();
        assert!(parts.len() >= 3);
        let toml_str = parts[1].trim();
        let parsed: crate::frontmatter::OutboundFrontmatter = toml::from_str(toml_str).unwrap();
        assert!(parsed.outbound);
        assert_eq!(
            parsed.delivery_status,
            crate::frontmatter::DeliveryStatus::Delivered
        );
        assert_eq!(parsed.mailbox, "alice");
        assert_eq!(parsed.message_id, "<abc@example.com>");
        assert!(parsed.delivered_at.is_some());
    }

    /// Sent files land `0o600` via the post-persist
    /// chown. Uses a test resolver that maps `testowner` (a non-
    /// reserved name so it routes through the resolver) to the current
    /// uid/gid, and a dedicated SendContext whose alice mailbox has
    /// `owner = "testowner"`. The chown-to-self syscall is accepted by
    /// every user; the explicit chmod inside `chown_as_owner` sets
    /// mode 0o600.
    #[tokio::test]
    async fn sent_file_lands_mode_0600() {
        use std::os::unix::fs::MetadataExt;

        fn fake(name: &str) -> Option<crate::user_resolver::ResolvedUser> {
            if name == "testowner" {
                Some(crate::user_resolver::ResolvedUser {
                    name: "testowner".into(),
                    uid: unsafe { libc::geteuid() },
                    gid: unsafe { libc::getegid() },
                })
            } else {
                None
            }
        }
        let _r = crate::user_resolver::set_test_resolver(fake);

        let data_dir = tempfile::TempDir::new().unwrap();
        let mock = Arc::new(MockTransport {
            captured: Mutex::new(vec![]),
            behavior: Behavior::Ok,
        });
        let tmp = tempfile::TempDir::new().unwrap();
        dkim::generate_keypair(tmp.path(), false).unwrap();
        let key = dkim::load_private_key(tmp.path()).unwrap();
        let mut mailboxes = std::collections::HashMap::new();
        mailboxes.insert(
            "catchall".into(),
            crate::config::MailboxConfig {
                address: "*@example.com".into(),
                owner: "aimx-catchall".into(),
                hooks: vec![],
                trust: None,
                trusted_senders: None,
                allow_root_catchall: false,
            },
        );
        mailboxes.insert(
            "alice".into(),
            crate::config::MailboxConfig {
                address: "alice@example.com".into(),
                owner: "testowner".into(),
                hooks: vec![],
                trust: None,
                trusted_senders: None,
                allow_root_catchall: false,
            },
        );
        let config = crate::config::Config {
            domain: "example.com".into(),
            data_dir: data_dir.path().to_path_buf(),
            dkim_selector: "aimx".into(),
            trust: "none".into(),
            trusted_senders: vec![],
            mailboxes,
            verify_host: None,
            enable_ipv6: false,
            signature: None,
            upgrade: None,
        };
        let ctx = SendContext {
            dkim_key: Arc::new(key),
            dkim_selector: "aimx".into(),
            config_handle: ConfigHandle::new(config),
            transport: mock,
            data_dir: data_dir.path().to_path_buf(),
        };

        let req = SendRequest {
            body: body("alice@example.com"),
            ..Default::default()
        };
        let resp = handle_send(req, &ctx, &Caller::internal_root()).await;
        assert!(matches!(resp, SendResponse::Ok { .. }));

        let sent_dir = data_dir.path().join("sent").join("alice");
        let entries: Vec<_> = std::fs::read_dir(&sent_dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .collect();
        let md = std::fs::metadata(entries[0].path()).unwrap();
        assert_eq!(
            md.mode() & 0o777,
            0o600,
            "sent file must land 0o600 via post-persist chown"
        );
    }

    // ------------------------------------------------------------------
    // Daemon-level signature tests (end-to-end through handle_send)
    // ------------------------------------------------------------------

    fn ctx_with_signature(
        transport: Arc<dyn MailTransport + Send + Sync>,
        data_dir: std::path::PathBuf,
        signature: Option<String>,
    ) -> SendContext {
        let tmp = tempfile::TempDir::new().unwrap();
        dkim::generate_keypair(tmp.path(), false).unwrap();
        let key = dkim::load_private_key(tmp.path()).unwrap();
        let mut mailboxes = std::collections::HashMap::new();
        mailboxes.insert(
            "alice".to_string(),
            crate::config::MailboxConfig {
                address: "alice@example.com".to_string(),
                owner: "root".to_string(),
                hooks: vec![],
                trust: None,
                trusted_senders: None,
                allow_root_catchall: false,
            },
        );
        let config = crate::config::Config {
            domain: "example.com".to_string(),
            data_dir: data_dir.clone(),
            dkim_selector: "aimx".to_string(),
            trust: "none".to_string(),
            trusted_senders: vec![],
            mailboxes,
            verify_host: None,
            enable_ipv6: false,
            signature,
            upgrade: None,
        };
        SendContext {
            dkim_key: Arc::new(key),
            dkim_selector: "aimx".to_string(),
            config_handle: ConfigHandle::new(config),
            transport,
            data_dir,
        }
    }

    fn read_sent_body(data_dir: &std::path::Path) -> String {
        let sent_dir = data_dir.join("sent").join("alice");
        let entries: Vec<_> = std::fs::read_dir(&sent_dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .collect();
        assert_eq!(entries.len(), 1);
        std::fs::read_to_string(entries[0].path()).unwrap()
    }

    #[tokio::test]
    async fn default_signature_appended_when_config_omits_it() {
        let data_dir = tempfile::TempDir::new().unwrap();
        let mock = Arc::new(MockTransport {
            captured: Mutex::new(vec![]),
            behavior: Behavior::Ok,
        });
        let ctx = ctx_with_signature(mock.clone(), data_dir.path().to_path_buf(), None);
        let req = SendRequest {
            body: body("alice@example.com"),
            ..Default::default()
        };
        let resp = handle_send(req, &ctx, &Caller::internal_root()).await;
        assert!(matches!(resp, SendResponse::Ok { .. }), "{resp:?}");

        let captured = mock.captured.lock().unwrap();
        let delivered = String::from_utf8_lossy(&captured[0]);
        assert!(
            delivered.contains("Sent from AIMX.") && delivered.contains("https://aimx.email"),
            "default signature must be appended to delivered body: {delivered}"
        );

        let persisted = read_sent_body(data_dir.path());
        assert!(persisted.contains("Sent from AIMX."));
        assert!(persisted.contains("https://aimx.email"));
    }

    #[tokio::test]
    async fn custom_signature_overrides_default() {
        let data_dir = tempfile::TempDir::new().unwrap();
        let mock = Arc::new(MockTransport {
            captured: Mutex::new(vec![]),
            behavior: Behavior::Ok,
        });
        let ctx = ctx_with_signature(
            mock.clone(),
            data_dir.path().to_path_buf(),
            Some("Best,\nBot".to_string()),
        );
        let req = SendRequest {
            body: body("alice@example.com"),
            ..Default::default()
        };
        let resp = handle_send(req, &ctx, &Caller::internal_root()).await;
        assert!(matches!(resp, SendResponse::Ok { .. }), "{resp:?}");

        let persisted = read_sent_body(data_dir.path());
        assert!(persisted.contains("Best,\r\nBot"));
        assert!(
            !persisted.contains("Sent from AIMX."),
            "default signature must not appear when custom is set"
        );
    }

    #[tokio::test]
    async fn empty_signature_disables_appending() {
        let data_dir = tempfile::TempDir::new().unwrap();
        let mock = Arc::new(MockTransport {
            captured: Mutex::new(vec![]),
            behavior: Behavior::Ok,
        });
        let ctx = ctx_with_signature(
            mock.clone(),
            data_dir.path().to_path_buf(),
            Some(String::new()),
        );
        let req = SendRequest {
            body: body("alice@example.com"),
            ..Default::default()
        };
        let resp = handle_send(req, &ctx, &Caller::internal_root()).await;
        assert!(matches!(resp, SendResponse::Ok { .. }), "{resp:?}");

        let captured = mock.captured.lock().unwrap();
        let delivered = String::from_utf8_lossy(&captured[0]);
        assert!(
            !delivered.contains("Sent from AIMX."),
            "empty signature must disable the default: {delivered}"
        );
        assert!(
            !delivered.contains("https://aimx.email"),
            "empty signature must disable the default: {delivered}"
        );
    }

    /// Daemon-level coverage for the text-only path: a custom signature
    /// configured via `Config::signature` must land in the single-part
    /// text/plain body shipped by `text_only=true`. Regression for the
    /// bug where `wire_assembly` dropped the signature on every escape
    /// hatch.
    #[tokio::test]
    async fn custom_signature_appended_on_text_only_path() {
        let data_dir = tempfile::TempDir::new().unwrap();
        let mock = Arc::new(MockTransport {
            captured: Mutex::new(vec![]),
            behavior: Behavior::Ok,
        });
        let ctx = ctx_with_signature(
            mock.clone(),
            data_dir.path().to_path_buf(),
            Some("-- daemon-sig-marker".to_string()),
        );
        let req = SendRequest {
            body: body("alice@example.com"),
            text_only: true,
            html_body: None,
        };
        let resp = handle_send(req, &ctx, &Caller::internal_root()).await;
        assert!(matches!(resp, SendResponse::Ok { .. }), "{resp:?}");

        let captured = mock.captured.lock().unwrap();
        let delivered = String::from_utf8_lossy(&captured[0]);
        // Single-part text/plain: no multipart wrapping.
        assert!(
            !delivered.contains("multipart/"),
            "text-only must not produce a multipart wire: {delivered}"
        );
        // Body and signature both present.
        assert!(delivered.contains("hello"));
        assert!(
            delivered.contains("daemon-sig-marker"),
            "custom signature must be appended on text-only path: {delivered}"
        );
        // Persisted sent record carries the signature too (we read the
        // stored body off the signed wire, so the signature rides along).
        let persisted = read_sent_body(data_dir.path());
        assert!(
            persisted.contains("daemon-sig-marker"),
            "persisted sent body must carry the signature: {persisted}"
        );
    }

    #[tokio::test]
    async fn signature_renders_in_text_and_html_parts_with_attachment() {
        let data_dir = tempfile::TempDir::new().unwrap();
        let mock = Arc::new(MockTransport {
            captured: Mutex::new(vec![]),
            behavior: Behavior::Ok,
        });
        let ctx = ctx_with_signature(
            mock.clone(),
            data_dir.path().to_path_buf(),
            Some("MARK".to_string()),
        );
        let multipart_body = b"From: alice@example.com\r\n\
            To: user@gmail.com\r\n\
            Subject: with attach\r\n\
            Date: Thu, 01 Jan 2025 12:00:00 +0000\r\n\
            Message-ID: <m1@example.com>\r\n\
            Content-Type: multipart/mixed; boundary=\"BND\"\r\n\
            \r\n\
            --BND\r\n\
            Content-Type: text/plain; charset=utf-8\r\n\
            \r\n\
            see attached.\r\n\
            --BND\r\n\
            Content-Type: application/octet-stream; name=\"x.bin\"\r\n\
            Content-Disposition: attachment; filename=\"x.bin\"\r\n\
            Content-Transfer-Encoding: base64\r\n\
            \r\n\
            QUFB\r\n\
            --BND--\r\n";
        let req = SendRequest {
            body: multipart_body.to_vec(),
            ..Default::default()
        };
        let resp = handle_send(req, &ctx, &Caller::internal_root()).await;
        assert!(matches!(resp, SendResponse::Ok { .. }), "{resp:?}");

        let captured = mock.captured.lock().unwrap();
        let delivered = String::from_utf8_lossy(&captured[0]);
        // The signature is appended to the Markdown source before
        // rendering, so it appears verbatim in the text part and (since
        // it is plain ASCII) literally in the rendered HTML part too.
        assert!(
            delivered.contains("MARK"),
            "signature must appear in the delivered message: {delivered}"
        );
        // The wire shape is multipart/mixed wrapping multipart/alternative.
        assert!(
            delivered.contains("Content-Type: multipart/mixed"),
            "outer content-type must be multipart/mixed: {delivered}"
        );
        assert!(
            delivered.contains("Content-Type: multipart/alternative"),
            "inner content-type must be multipart/alternative: {delivered}"
        );
        assert!(
            delivered.contains("filename=\"x.bin\""),
            "attachment must survive the daemon-side reassembly: {delivered}"
        );
    }

    // ------------------------------------------------------------------
    // Escape-hatch branches end-to-end through `handle_send`.
    // ------------------------------------------------------------------

    /// `text_only=true` skips Markdown rendering and ships single-part
    /// text/plain. The signature IS auto-appended on this branch so the
    /// recipient always sees the configured footer.
    #[tokio::test]
    async fn text_only_send_emits_single_part_text_plain() {
        let mock = Arc::new(MockTransport {
            captured: Mutex::new(vec![]),
            behavior: Behavior::Ok,
        });
        let ctx = test_ctx(mock.clone());
        let req = SendRequest {
            body: body("alice@example.com"),
            text_only: true,
            html_body: None,
        };
        let resp = handle_send(req, &ctx, &Caller::internal_root()).await;
        assert!(matches!(resp, SendResponse::Ok { .. }), "{resp:?}");

        let captured = mock.captured.lock().unwrap();
        assert_eq!(captured.len(), 1);
        let delivered = String::from_utf8_lossy(&captured[0]);
        // Single-part: no multipart wrapping at all.
        assert!(
            !delivered.contains("multipart/"),
            "text-only must not produce a multipart wire: {delivered}"
        );
        // No HTML part.
        assert!(
            !delivered.contains("Content-Type: text/html"),
            "text-only must not include a text/html part: {delivered}"
        );
        // text/plain present, body verbatim.
        assert!(delivered.contains("Content-Type: text/plain"));
        assert!(delivered.contains("hello"));
        // The default signature IS appended on the text-only path.
        assert!(
            delivered.contains("Sent from AIMX."),
            "text-only must append the default signature: {delivered}"
        );
        assert!(
            delivered.contains("https://aimx.email"),
            "text-only must carry the default signature URL: {delivered}"
        );
    }

    /// `html_body=Some(html)` skips Markdown rendering and uses the
    /// operator's HTML verbatim. The signature is appended to the
    /// text/plain fallback but the supplied HTML stays untouched —
    /// operators on `--html-body` already own their HTML rendering.
    #[tokio::test]
    async fn html_body_send_uses_supplied_html_verbatim() {
        let mock = Arc::new(MockTransport {
            captured: Mutex::new(vec![]),
            behavior: Behavior::Ok,
        });
        let ctx = test_ctx(mock.clone());
        let custom_html = b"<h1>x</h1>";
        let req = SendRequest {
            body: body("alice@example.com"),
            text_only: false,
            html_body: Some(custom_html.to_vec()),
        };
        let resp = handle_send(req, &ctx, &Caller::internal_root()).await;
        assert!(matches!(resp, SendResponse::Ok { .. }), "{resp:?}");

        let captured = mock.captured.lock().unwrap();
        let delivered = String::from_utf8_lossy(&captured[0]);
        assert!(delivered.contains("Content-Type: multipart/alternative"));
        assert!(delivered.contains("Content-Type: text/plain"));
        assert!(delivered.contains("Content-Type: text/html"));
        // The operator's HTML appears verbatim — not the renderer's
        // styled output. The renderer would have added `style="..."`
        // attributes and inlined CSS; we assert their absence in the
        // html portion of the wire as a "no rendering happened" check.
        assert!(
            delivered.contains("<h1>x</h1>"),
            "verbatim HTML missing: {delivered}"
        );
        let html_idx = delivered
            .find("Content-Type: text/html")
            .expect("text/html part missing");
        let html_section = &delivered[html_idx..];
        assert!(
            !html_section.contains("style=\""),
            "renderer must not be invoked on --html-body path: {html_section}"
        );
        // Default signature IS appended to the text/plain fallback so
        // text-only readers still see it; the operator-supplied HTML
        // remains verbatim (signature must not appear inside the HTML
        // part).
        assert!(
            delivered.contains("Sent from AIMX."),
            "--html-body must append the default signature to text/plain: {delivered}"
        );
        assert!(
            !html_section.contains("Sent from AIMX."),
            "operator-supplied HTML must stay verbatim (signature must not leak into HTML): \
             {html_section}"
        );
    }

    // ------------------------------------------------------------------
    // `outbound_format` audit-trail field on the persisted sent record.
    // Asserts each of the three wire-shape branches stamps the correct
    // value into the `OutboundFrontmatter`. Sent body content + the
    // "no .html sibling" invariant land in the integration suite; here
    // we just pin the frontmatter contract.
    // ------------------------------------------------------------------

    fn read_sent_outbound_format(data_dir: &std::path::Path) -> String {
        let sent_dir = data_dir.join("sent").join("alice");
        let entry = std::fs::read_dir(&sent_dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .next()
            .expect("sent record must exist");
        let content = std::fs::read_to_string(entry.path()).unwrap();
        let parts: Vec<&str> = content.splitn(3, "+++").collect();
        let toml_str = parts[1].trim();
        let parsed: crate::frontmatter::OutboundFrontmatter =
            toml::from_str(toml_str).expect("frontmatter must parse");
        parsed.outbound_format
    }

    #[tokio::test]
    async fn default_markdown_send_persists_outbound_format_markdown() {
        let data_dir = tempfile::TempDir::new().unwrap();
        let mock = Arc::new(MockTransport {
            captured: Mutex::new(vec![]),
            behavior: Behavior::Ok,
        });
        let ctx = test_ctx_with_data_dir(mock, Some(data_dir.path().to_path_buf()));
        let req = SendRequest {
            body: body("alice@example.com"),
            ..Default::default()
        };
        let resp = handle_send(req, &ctx, &Caller::internal_root()).await;
        assert!(matches!(resp, SendResponse::Ok { .. }));
        assert_eq!(read_sent_outbound_format(data_dir.path()), "markdown");
    }

    #[tokio::test]
    async fn text_only_send_persists_outbound_format_text() {
        let data_dir = tempfile::TempDir::new().unwrap();
        let mock = Arc::new(MockTransport {
            captured: Mutex::new(vec![]),
            behavior: Behavior::Ok,
        });
        let ctx = test_ctx_with_data_dir(mock, Some(data_dir.path().to_path_buf()));
        let req = SendRequest {
            body: body("alice@example.com"),
            text_only: true,
            html_body: None,
        };
        let resp = handle_send(req, &ctx, &Caller::internal_root()).await;
        assert!(matches!(resp, SendResponse::Ok { .. }));
        assert_eq!(read_sent_outbound_format(data_dir.path()), "text");
    }

    #[tokio::test]
    async fn html_body_send_persists_outbound_format_html() {
        let data_dir = tempfile::TempDir::new().unwrap();
        let mock = Arc::new(MockTransport {
            captured: Mutex::new(vec![]),
            behavior: Behavior::Ok,
        });
        let ctx = test_ctx_with_data_dir(mock, Some(data_dir.path().to_path_buf()));
        let req = SendRequest {
            body: body("alice@example.com"),
            text_only: false,
            html_body: Some(b"<p>custom</p>".to_vec()),
        };
        let resp = handle_send(req, &ctx, &Caller::internal_root()).await;
        assert!(matches!(resp, SendResponse::Ok { .. }));
        assert_eq!(read_sent_outbound_format(data_dir.path()), "html");
    }

    /// No `.html` sibling file is written next to the sent record on
    /// any of the three send paths. Pin the single-file-`.md`
    /// persistence invariant at the unit-test level so a future change
    /// that introduces an HTML twin file is caught immediately.
    #[tokio::test]
    async fn sent_record_never_writes_html_sibling() {
        for req in [
            SendRequest {
                body: body("alice@example.com"),
                ..Default::default()
            },
            SendRequest {
                body: body("alice@example.com"),
                text_only: true,
                html_body: None,
            },
            SendRequest {
                body: body("alice@example.com"),
                text_only: false,
                html_body: Some(b"<p>custom</p>".to_vec()),
            },
        ] {
            let data_dir = tempfile::TempDir::new().unwrap();
            let mock = Arc::new(MockTransport {
                captured: Mutex::new(vec![]),
                behavior: Behavior::Ok,
            });
            let ctx = test_ctx_with_data_dir(mock, Some(data_dir.path().to_path_buf()));
            let resp = handle_send(req, &ctx, &Caller::internal_root()).await;
            assert!(matches!(resp, SendResponse::Ok { .. }));

            let sent_dir = data_dir.path().join("sent").join("alice");
            let any_html = std::fs::read_dir(&sent_dir)
                .unwrap()
                .filter_map(|e| e.ok())
                .any(|e| {
                    e.path()
                        .extension()
                        .is_some_and(|ext| ext.eq_ignore_ascii_case("html"))
                });
            assert!(
                !any_html,
                "no .html sibling file should appear under {}",
                sent_dir.display()
            );
        }
    }
}
