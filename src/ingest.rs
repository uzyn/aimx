use crate::config::Config;
use crate::frontmatter::{
    AttachmentMeta, AuthResults, InboundFrontmatter, compute_thread_id, format_frontmatter,
};
use crate::hook::{self, OnReceiveContext};
use crate::mailbox_locks::MailboxLocks;
use crate::slug::{allocate_filename, slugify};
use crate::trust;
use mail_parser::{MessageParser, MimeHeaders};
use std::io::Read;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub fn run(rcpt: &str, config: Config) -> Result<(), Box<dyn std::error::Error>> {
    let mut raw = Vec::new();
    std::io::stdin().read_to_end(&mut raw)?;
    // Manual stdin path: no SMTP session, so received_from_ip is the
    // unspecified sentinel (0.0.0.0) and there is no envelope MAIL FROM.
    let sentinel_ip: IpAddr = "0.0.0.0".parse().unwrap();
    // The manual stdin caller owns a fresh lock map. It's a short-lived
    // process with a single ingest so contention isn't possible. The
    // daemon path shares its map across all writers.
    let locks = Arc::new(MailboxLocks::new());
    ingest_email(&config, &locks, rcpt, &raw, sentinel_ip, None)
}

/// Inbound ingest entry point.
///
/// The inbound write path is unified with the MARK-* / MAILBOX-* write
/// paths under a single per-mailbox `tokio::sync::Mutex<()>` from
/// [`MailboxLocks`]. The critical section below is taken via
/// `blocking_lock()` because `ingest_email` runs from a synchronous
/// context (manual stdin path, or the SMTP session's `spawn_blocking`
/// worker). `blocking_lock()` is sound on a blocking thread, but never
/// from an async runtime thread (which would panic).
pub fn ingest_email(
    config: &Config,
    locks: &MailboxLocks,
    rcpt: &str,
    raw: &[u8],
    received_from_ip: IpAddr,
    envelope_mail_from: Option<&str>,
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

    let cc = message.cc().and_then(|addrs| {
        let parts: Vec<String> = addrs
            .iter()
            .filter_map(|a| {
                a.address().map(|addr| match a.name() {
                    Some(name) => format!("{name} <{addr}>"),
                    None => addr.to_string(),
                })
            })
            .collect();
        if parts.is_empty() {
            None
        } else {
            Some(parts.join(", "))
        }
    });

    let reply_to = message.reply_to().and_then(|addrs| {
        let parts: Vec<String> = addrs
            .iter()
            .filter_map(|a| {
                a.address().map(|addr| match a.name() {
                    Some(name) => format!("{name} <{addr}>"),
                    None => addr.to_string(),
                })
            })
            .collect();
        if parts.is_empty() {
            None
        } else {
            Some(parts.join(", "))
        }
    });

    let subject = message.subject().unwrap_or("(no subject)").to_string();

    let date = message
        .date()
        .map(|d| d.to_rfc3339())
        .unwrap_or_else(|| chrono::Utc::now().to_rfc3339());

    let message_id = message.message_id().unwrap_or("").to_string();

    let in_reply_to_raw = message
        .in_reply_to()
        .as_text()
        .unwrap_or_default()
        .to_string();

    let references_raw = message
        .references()
        .as_text()
        .unwrap_or_default()
        .to_string();

    let thread_id = compute_thread_id(
        &message_id,
        if in_reply_to_raw.is_empty() {
            None
        } else {
            Some(in_reply_to_raw.as_str())
        },
        if references_raw.is_empty() {
            None
        } else {
            Some(references_raw.as_str())
        },
    );

    let list_id = extract_header_value(&message, "List-ID");
    let auto_submitted = extract_header_value(&message, "Auto-Submitted");

    let body = extract_body(&message);

    let prepared_attachments = prepare_attachments(&message);
    let has_attachments = !prepared_attachments.is_empty();

    let auth_results = verify_auth(raw, received_from_ip, envelope_mail_from);

    let trusted_value = config
        .mailboxes
        .get(&mailbox)
        .map(|mb| trust::evaluate_trust(config, mb, &auth_results, &from))
        .unwrap_or(trust::TrustedValue::None);

    let received_at = chrono::Utc::now().to_rfc3339();
    let size_bytes = raw.len();

    let ip_str = if received_from_ip.is_unspecified() {
        None
    } else {
        Some(received_from_ip.to_string())
    };

    let slug = slugify(&subject);
    let timestamp = chrono::Utc::now();

    // S47-4: the per-mailbox lock shared with MARK-* and MAILBOX-* is
    // the outer lock of the two-tier hierarchy (see
    // `crate::mailbox_locks`). Acquired via `blocking_lock()` because
    // `ingest_email` runs from a synchronous context (stdin caller, or
    // the SMTP session's `spawn_blocking` worker). The lock covers the
    // full filename-allocation + bundle-creation + attachment-write +
    // final `.md` write sequence; readers of the mailbox directory
    // observe either the pre-state or the post-state, never a half-
    // written bundle.
    let lock = locks.lock_for(&mailbox);
    let _guard = lock.blocking_lock();

    let md_path = allocate_filename(&inbox_dir, timestamp, &slug, has_attachments);
    let parent_dir = md_path
        .parent()
        .ok_or("allocate_filename returned a rootless path")?
        .to_path_buf();

    std::fs::create_dir_all(&parent_dir)?;

    let id = md_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or_default()
        .to_string();

    let cleanup_bundle = || {
        if has_attachments && parent_dir != inbox_dir {
            let _ = std::fs::remove_dir_all(&parent_dir);
        }
    };

    let attachments = write_attachments(&parent_dir, prepared_attachments).inspect_err(|_| {
        cleanup_bundle();
    })?;

    let meta = InboundFrontmatter {
        id: id.clone(),
        message_id,
        thread_id,
        from,
        to,
        cc,
        reply_to,
        delivered_to: rcpt.to_string(),
        subject,
        date,
        received_at,
        received_from_ip: ip_str,
        size_bytes,
        attachments,
        in_reply_to: if in_reply_to_raw.is_empty() {
            None
        } else {
            Some(in_reply_to_raw)
        },
        references: if references_raw.is_empty() {
            None
        } else {
            Some(references_raw)
        },
        list_id,
        auto_submitted,
        dkim: auth_results.dkim,
        spf: auth_results.spf,
        dmarc: auth_results.dmarc,
        trusted: trusted_value.as_str().to_string(),
        mailbox: mailbox.clone(),
        read: false,
        read_at: None,
        labels: vec![],
    };

    write_markdown(&md_path, &meta, &body).inspect_err(|_| {
        cleanup_bundle();
    })?;

    drop(_guard);

    if let Some(mailbox_config) = config.mailboxes.get(&mailbox) {
        let ctx = OnReceiveContext {
            filepath: &md_path,
            metadata: &meta,
        };
        hook::execute_on_receive(mailbox_config, &ctx);
    }

    Ok(())
}

fn extract_header_value(message: &mail_parser::Message, name: &str) -> Option<String> {
    let val = message.header_raw(name)?;
    let s = val.trim().to_string();
    if s.is_empty() { None } else { Some(s) }
}

fn create_resolver() -> Option<mail_auth::MessageAuthenticator> {
    mail_auth::MessageAuthenticator::new_system_conf()
        .or_else(|_| mail_auth::MessageAuthenticator::new_cloudflare())
        .ok()
}

fn verify_auth(raw: &[u8], peer_ip: IpAddr, envelope_mail_from: Option<&str>) -> AuthResults {
    let rt = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(_) => return AuthResults::default(),
    };

    let resolver = match create_resolver() {
        Some(r) => r,
        None => return AuthResults::default(),
    };

    rt.block_on(async {
        let auth_msg = mail_auth::AuthenticatedMessage::parse(raw);

        let dkim_output = match &auth_msg {
            Some(msg) => resolver.verify_dkim(msg).await,
            None => vec![],
        };

        let dkim_result = dkim_output_to_string(&dkim_output, auth_msg.is_some());

        let (spf_ip, spf_mail_from) = select_spf_inputs(raw, peer_ip, envelope_mail_from);
        let spf_output = build_spf_output(spf_ip, spf_mail_from.as_deref(), &resolver).await;
        let spf_result = spf_output_to_string(&spf_output);

        let dmarc_result = match &auth_msg {
            Some(msg) => {
                verify_dmarc_async(
                    msg,
                    &dkim_output,
                    &spf_output,
                    spf_mail_from.as_deref(),
                    &resolver,
                )
                .await
            }
            None => "none".to_string(),
        };

        AuthResults {
            dkim: dkim_result,
            spf: spf_result,
            dmarc: dmarc_result,
        }
    })
}

/// Decide which IP and MAIL FROM to feed into the SPF check.
///
/// When the SMTP session gives us an authoritative peer IP and envelope
/// MAIL FROM (reverse_path), we use those; they're what RFC 7208 actually
/// requires for SPF. Header parsing is a best-effort fallback triggered
/// when either value is missing: the manual `aimx ingest` stdin path (no
/// envelope at all) and RFC 5321 null-sender bounces (`MAIL FROM:<>`),
/// where `reverse_path` is empty. The null-sender case is strictly wrong
/// per RFC 7208 §2.4 (SPF should be checked against the HELO/EHLO domain
/// for a null sender); threading `ehlo_hostname` through to fix that is
/// tracked as a follow-up.
fn select_spf_inputs(
    raw: &[u8],
    peer_ip: IpAddr,
    envelope_mail_from: Option<&str>,
) -> (Option<IpAddr>, Option<String>) {
    let ip = if peer_ip.is_unspecified() {
        extract_received_ip(raw)
    } else {
        Some(peer_ip)
    };

    let mail_from = match envelope_mail_from {
        Some(s) if !s.is_empty() => Some(s.to_string()),
        _ => extract_mail_from(raw),
    };

    (ip, mail_from)
}

fn dkim_output_to_string(results: &[mail_auth::DkimOutput<'_>], parsed: bool) -> String {
    if !parsed {
        return "none".to_string();
    }

    if results.is_empty() {
        return "none".to_string();
    }

    for output in results {
        if matches!(output.result(), mail_auth::DkimResult::Pass) {
            return "pass".to_string();
        }
    }

    "fail".to_string()
}

async fn build_spf_output(
    ip: Option<IpAddr>,
    mail_from: Option<&str>,
    resolver: &mail_auth::MessageAuthenticator,
) -> mail_auth::SpfOutput {
    let ip = match ip {
        Some(ip) => ip,
        None => return mail_auth::SpfOutput::new(String::new()),
    };

    let mail_from = mail_from.unwrap_or_default();

    let helo_domain = match spf_domain(mail_from) {
        Some(d) => d.to_string(),
        None => return mail_auth::SpfOutput::new(String::new()),
    };

    resolver
        .verify_spf(mail_auth::spf::verify::SpfParameters::verify_mail_from(
            ip,
            &helo_domain,
            &helo_domain,
            mail_from,
        ))
        .await
}

fn spf_output_to_string(output: &mail_auth::SpfOutput) -> String {
    match output.result() {
        mail_auth::SpfResult::Pass => "pass".to_string(),
        mail_auth::SpfResult::Fail => "fail".to_string(),
        mail_auth::SpfResult::SoftFail => "softfail".to_string(),
        mail_auth::SpfResult::Neutral => "neutral".to_string(),
        mail_auth::SpfResult::None => "none".to_string(),
        _ => "none".to_string(),
    }
}

async fn verify_dmarc_async(
    auth_msg: &mail_auth::AuthenticatedMessage<'_>,
    dkim_output: &[mail_auth::DkimOutput<'_>],
    spf_output: &mail_auth::SpfOutput,
    mail_from: Option<&str>,
    resolver: &mail_auth::MessageAuthenticator,
) -> String {
    let mail_from = mail_from.unwrap_or_default();
    let mail_from_domain = spf_domain(mail_from).unwrap_or("");

    let params = mail_auth::dmarc::verify::DmarcParameters {
        message: auth_msg,
        dkim_output,
        rfc5321_mail_from_domain: mail_from_domain,
        spf_output,
        domain_suffix_fn: |d| psl::domain_str(d).unwrap_or(d),
    };

    let output = resolver.verify_dmarc(params).await;

    if output.dkim_result() == &mail_auth::DmarcResult::Pass {
        return "pass".to_string();
    }
    if output.spf_result() == &mail_auth::DmarcResult::Pass {
        return "pass".to_string();
    }

    match (output.dkim_result(), output.spf_result()) {
        (mail_auth::DmarcResult::None, mail_auth::DmarcResult::None) => "none".to_string(),
        _ => "fail".to_string(),
    }
}

pub fn spf_domain(mail_from: &str) -> Option<&str> {
    let domain = mail_from.split('@').nth(1).unwrap_or("");
    if domain.is_empty() {
        None
    } else {
        Some(domain)
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

/// Parse a connecting-client IP from a `Received:` header.
///
/// Trusts **only** the bracketed-form address (`[1.2.3.4]` / `[2001:db8::1]`),
/// the RFC 5321 canonical marker for the client IP recorded by the receiving
/// MTA. Any IP embedded in free-text comments, HELO strings, or
/// `received from hostname` prose is ignored, because those fields are
/// attacker-controlled: the sender can write anything into the HELO/EHLO
/// command or a hostname that happens to contain a dotted-quad. See S43-4.
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

/// Maximum bytes of HTML input passed to `html2text::from_read`.
///
/// SMTP `max_message_size` already caps raw DATA at 25 MB, but pathological
/// HTML (deeply nested tables, runaway CSS) within that envelope can still
/// burn significant CPU in the conversion layer. 2 MB is far above any
/// realistic marketing HTML (< 500 KB typical) so legitimate messages are
/// unaffected; anything larger is truncated and a visible marker is appended
/// to the rendered body.
pub(crate) const HTML_CONVERSION_CAP: usize = 2 * 1024 * 1024;
const HTML_TRUNCATION_MARKER: &str = "\n\n[...HTML body truncated at 2 MB for rendering...]";

fn extract_body(message: &mail_parser::Message) -> String {
    if let Some(text) = message.body_text(0) {
        return text.to_string();
    }

    if let Some(html) = message.body_html(0) {
        return render_html_body(html.as_bytes());
    }

    String::new()
}

/// Convert an HTML body to plain text via `html2text`, capping input size to
/// [`HTML_CONVERSION_CAP`] to bound CPU. When the input is truncated, the
/// rendered output ends with a visible marker so agents (and humans) see
/// that the body was cut.
pub(crate) fn render_html_body(html: &[u8]) -> String {
    if html.is_empty() {
        return String::new();
    }

    let (input, truncated) = if html.len() > HTML_CONVERSION_CAP {
        (&html[..HTML_CONVERSION_CAP], true)
    } else {
        (html, false)
    };

    let mut rendered =
        html2text::from_read(input, 80).unwrap_or_else(|_| String::from_utf8_lossy(input).into());

    if truncated {
        rendered.push_str(HTML_TRUNCATION_MARKER);
    }
    rendered
}

struct PreparedAttachment {
    filename: String,
    content_type: String,
    body: Vec<u8>,
}

fn prepare_attachments(message: &mail_parser::Message) -> Vec<PreparedAttachment> {
    let mut result = Vec::new();

    for (index, attachment) in message.attachments().enumerate() {
        let raw_name = attachment.attachment_name().unwrap_or("").to_string();

        // `sanitize_attachment_filename` always returns a non-empty, filesystem-
        // safe name (falling back to `attachment-<index>` for empty / unsafe
        // input). The sanitized value is the single source of truth for both
        // the on-disk bundle file and the `attachments = [...]` frontmatter.
        let filename = sanitize_attachment_filename(&raw_name, index);

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

/// Cap for sanitized attachment filenames, in bytes. Leaves comfortable
/// headroom under the typical Linux/macOS `NAME_MAX = 255` after any
/// future prefix/suffix. Truncation happens on a UTF-8 char boundary.
const ATTACHMENT_FILENAME_MAX_BYTES: usize = 200;

/// Sanitize an attachment filename from an untrusted inbound email.
///
/// The raw filename coming out of `mail-parser` is attacker-controlled. On
/// top of `Path::file_name()` (which strips directory components), this
/// helper additionally:
/// - NFC-normalizes so visually-identical names don't split into NFC/NFD
///   variants (filesystem collision + agent confusion).
/// - Strips control characters (C0, DEL) and bidi / invisible Unicode
///   controls (RLO, LRO, PDF, ZWJ, ZWNJ, BOM, LRM, RLM, LRE, RLE, etc.)
///   so a filename can't hide its extension or rewrite itself in agent
///   logs.
/// - Replaces `/` and `\` with `_` so a malformed name can never escape
///   the bundle directory even if the Path::file_name() guard is lost in
///   a refactor (defense in depth).
/// - Collapses runs of unsafe characters to a single `_`.
/// - Trims leading/trailing whitespace, `.`, and `-`. Leading `-` matters
///   because downstream CLI tools (`rm`, `find`, `curl`) may interpret a
///   filename beginning with `-` as a flag.
/// - Caps the byte length at [`ATTACHMENT_FILENAME_MAX_BYTES`] on a UTF-8
///   char boundary so the filesystem's `NAME_MAX` is not hit.
/// - Falls back to `attachment-<index>` when the result would be empty
///   (e.g. the whole name was control chars).
pub(crate) fn sanitize_attachment_filename(raw: &str, index: usize) -> String {
    use unicode_normalization::UnicodeNormalization;

    // First line of defense: pick the filename component only. `mail-parser`
    // already unescapes headers, so `../../etc/passwd` arrives as a literal
    // string; `Path::file_name` on `"../../etc/passwd"` returns `"passwd"`,
    // which neutralizes naive path-traversal. `\\` on Unix is NOT a path
    // separator, so Windows-style paths need explicit handling. Do it
    // below after normalization.
    let base = Path::new(raw)
        .file_name()
        .and_then(|f| f.to_str())
        .unwrap_or("");

    // NFC-normalize so NFD-form names collapse into their NFC siblings.
    let normalized: String = base.nfc().collect();

    let mut out = String::with_capacity(normalized.len());
    let mut last_was_underscore = false;
    for ch in normalized.chars() {
        let unsafe_char = is_filename_unsafe(ch);
        if unsafe_char {
            if !last_was_underscore {
                out.push('_');
                last_was_underscore = true;
            }
        } else {
            out.push(ch);
            last_was_underscore = false;
        }
    }

    // Trim leading/trailing whitespace, dots, dashes, and collapse-underscores.
    // A leading `-` is flag-like to many CLI tools; a leading `.` yields a
    // hidden file; a leading `_` would often just be a residue of the
    // unsafe-char collapse step above and adds no information.
    let trim_chars = |c: char| c.is_whitespace() || c == '.' || c == '-' || c == '_';
    let trimmed = out.trim_matches(trim_chars);
    let mut trimmed = trimmed.to_string();

    // Byte-cap on a UTF-8 char boundary.
    if trimmed.len() > ATTACHMENT_FILENAME_MAX_BYTES {
        let mut end = ATTACHMENT_FILENAME_MAX_BYTES;
        while end > 0 && !trimmed.is_char_boundary(end) {
            end -= 1;
        }
        trimmed.truncate(end);
        // Post-truncation: re-trim trailing separators exposed by the cut.
        let retrimmed = trimmed.trim_end_matches(trim_chars);
        trimmed = retrimmed.to_string();
    }

    if trimmed.is_empty() {
        return format!("attachment-{index}");
    }
    trimmed
}

/// Characters never allowed in a sanitized attachment filename.
fn is_filename_unsafe(ch: char) -> bool {
    // Path separators (defense in depth; `Path::file_name` already strips
    // these, but on Unix `\\` is NOT a path separator so we must reject it
    // explicitly to keep a consistent shape across platforms and protect
    // downstream Windows consumers).
    if ch == '/' || ch == '\\' {
        return true;
    }
    // NUL and all C0 control characters + DEL.
    if ch == '\0' || ch.is_control() {
        return true;
    }
    // Bidirectional and other invisible-control Unicode code points.
    // These can hide an extension, re-order visible glyphs, or be used to
    // spoof extensions in agent / operator logs.
    matches!(
        ch,
        '\u{200B}'..='\u{200F}'   // ZWSP / ZWJ / ZWNJ / LRM / RLM
            | '\u{202A}'..='\u{202E}' // LRE / RLE / PDF / LRO / RLO
            | '\u{2066}'..='\u{2069}' // LRI / RLI / FSI / PDI
            | '\u{FEFF}'              // BOM / ZWNBSP
    )
}

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
    meta: &InboundFrontmatter,
    body: &str,
) -> Result<PathBuf, Box<dyn std::error::Error>> {
    use std::fs::OpenOptions;
    use std::io::Write;

    let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
    let content = format_frontmatter(meta, body);
    file.write_all(content.as_bytes())?;
    Ok(path.to_path_buf())
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
                hooks: vec![],
                trust: None,
                trusted_senders: None,
            },
        );
        mailboxes.insert(
            "alice".to_string(),
            MailboxConfig {
                address: "alice@test.com".to_string(),
                hooks: vec![],
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

    fn sentinel_ip() -> IpAddr {
        "0.0.0.0".parse().unwrap()
    }

    /// Most tests exercise a single ingest in isolation, so they own a
    /// fresh `MailboxLocks`. Tests that exercise concurrent ingest + MARK
    /// on the same mailbox (see the integration suite) pass in a shared
    /// `MailboxLocks` instead.
    fn test_locks() -> MailboxLocks {
        MailboxLocks::new()
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

    fn collect_md_files(mailbox_dir: &Path) -> Vec<std::path::PathBuf> {
        let mut result: Vec<std::path::PathBuf> = Vec::new();
        let entries = match std::fs::read_dir(mailbox_dir) {
            Ok(e) => e,
            Err(_) => return result,
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
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

    fn parse_toml_frontmatter(content: &str) -> toml::value::Table {
        let parts: Vec<&str> = content.splitn(3, "+++").collect();
        assert!(parts.len() >= 3);
        let toml_str = parts[1].trim();
        let parsed: toml::Value = toml::from_str(toml_str).unwrap();
        parsed.as_table().unwrap().clone()
    }

    #[test]
    fn ingest_plain_text() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(tmp.path());
        ingest_email(
            &config,
            &test_locks(),
            "alice@test.com",
            plain_text_eml(),
            sentinel_ip(),
            None,
        )
        .unwrap();

        let entries = collect_md_files(&inbox(tmp.path(), "alice"));
        assert_eq!(entries.len(), 1);

        let content = std::fs::read_to_string(&entries[0]).unwrap();
        let table = parse_toml_frontmatter(&content);

        assert_eq!(
            table.get("from").unwrap().as_str().unwrap(),
            "sender@example.com"
        );
        assert_eq!(table.get("subject").unwrap().as_str().unwrap(), "Hello");
        assert_eq!(table.get("read").unwrap(), &toml::Value::Boolean(false));

        assert!(content.contains("This is a plain text email."));
    }

    #[test]
    fn ingest_writes_to_inbox_subdir() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(tmp.path());
        ingest_email(
            &config,
            &test_locks(),
            "alice@test.com",
            plain_text_eml(),
            sentinel_ip(),
            None,
        )
        .unwrap();

        assert!(tmp.path().join("inbox").join("alice").exists());
        assert!(!tmp.path().join("alice").exists());
    }

    #[test]
    fn ingest_filename_uses_utc_slug_format() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(tmp.path());
        ingest_email(
            &config,
            &test_locks(),
            "alice@test.com",
            plain_text_eml(),
            sentinel_ip(),
            None,
        )
        .unwrap();

        let entries = collect_md_files(&inbox(tmp.path(), "alice"));
        let name = entries[0]
            .file_name()
            .unwrap()
            .to_string_lossy()
            .to_string();
        assert!(
            name.ends_with("-hello.md"),
            "expected slug-suffixed filename, got {name}"
        );
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
        ingest_email(
            &config,
            &test_locks(),
            "alice@test.com",
            html_only_eml(),
            sentinel_ip(),
            None,
        )
        .unwrap();

        let entries = collect_md_files(&inbox(tmp.path(), "alice"));
        let content = std::fs::read_to_string(&entries[0]).unwrap();
        assert!(content.contains("Hello"));
        assert!(content.contains("World"));
        assert!(!content.contains("<html>"));
    }

    #[test]
    fn render_html_body_under_cap_is_full_conversion() {
        let html = b"<html><body><h1>Hello</h1><p>World</p></body></html>";
        let rendered = render_html_body(html);
        assert!(rendered.contains("Hello"));
        assert!(rendered.contains("World"));
        assert!(
            !rendered.contains("HTML body truncated"),
            "under-cap input must not produce a truncation marker: {rendered}"
        );
    }

    #[test]
    fn render_html_body_empty_is_empty() {
        assert_eq!(render_html_body(b""), "");
    }

    #[test]
    fn render_html_body_over_cap_appends_marker() {
        // Build 2 MB + 1 KB of harmless HTML. Anything over the cap triggers
        // truncation regardless of content.
        let filler = "<p>x</p>".repeat((HTML_CONVERSION_CAP / 8) + 128);
        let html = format!("<html><body>{filler}</body></html>");
        assert!(html.len() > HTML_CONVERSION_CAP);
        let rendered = render_html_body(html.as_bytes());
        assert!(
            rendered.contains("HTML body truncated at 2 MB for rendering"),
            "over-cap input must end with a truncation marker: (len={})",
            rendered.len()
        );
    }

    #[test]
    fn ingest_multipart_prefers_text() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(tmp.path());
        ingest_email(
            &config,
            &test_locks(),
            "alice@test.com",
            multipart_eml(),
            sentinel_ip(),
            None,
        )
        .unwrap();

        let entries = collect_md_files(&inbox(tmp.path(), "alice"));
        let content = std::fs::read_to_string(&entries[0]).unwrap();
        assert!(content.contains("Plain text part."));
        assert!(!content.contains("<html>"));
    }

    #[test]
    fn ingest_routes_unknown_to_catchall() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(tmp.path());
        ingest_email(
            &config,
            &test_locks(),
            "unknown@test.com",
            plain_text_eml(),
            sentinel_ip(),
            None,
        )
        .unwrap();

        assert!(inbox(tmp.path(), "catchall").exists());
        let entries = collect_md_files(&inbox(tmp.path(), "catchall"));
        assert_eq!(entries.len(), 1);
    }

    #[test]
    fn frontmatter_valid_toml() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(tmp.path());
        ingest_email(
            &config,
            &test_locks(),
            "alice@test.com",
            plain_text_eml(),
            sentinel_ip(),
            None,
        )
        .unwrap();

        let entries = collect_md_files(&inbox(tmp.path(), "alice"));
        let content = std::fs::read_to_string(&entries[0]).unwrap();
        let table = parse_toml_frontmatter(&content);

        assert!(table.contains_key("id"));
        assert!(table.contains_key("message_id"));
        assert!(table.contains_key("thread_id"));
        assert!(table.contains_key("from"));
        assert!(table.contains_key("to"));
        assert!(table.contains_key("delivered_to"));
        assert!(table.contains_key("subject"));
        assert!(table.contains_key("date"));
        assert!(table.contains_key("received_at"));
        assert!(table.contains_key("size_bytes"));
        assert!(table.contains_key("dkim"));
        assert!(table.contains_key("spf"));
        assert!(table.contains_key("dmarc"));
        assert!(table.contains_key("trusted"));
        assert!(table.contains_key("mailbox"));
        assert!(table.contains_key("read"));

        assert_eq!(table.get("read").unwrap(), &toml::Value::Boolean(false));
    }

    #[test]
    fn file_naming_increments() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(tmp.path());

        ingest_email(
            &config,
            &test_locks(),
            "alice@test.com",
            plain_text_eml(),
            sentinel_ip(),
            None,
        )
        .unwrap();
        ingest_email(
            &config,
            &test_locks(),
            "alice@test.com",
            plain_text_eml(),
            sentinel_ip(),
            None,
        )
        .unwrap();

        let entries = collect_md_files(&inbox(tmp.path(), "alice"));
        assert_eq!(entries.len(), 2, "got entries: {entries:?}");
        assert_ne!(entries[0], entries[1]);
    }

    #[test]
    fn attachment_creates_bundle_directory() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(tmp.path());
        ingest_email(
            &config,
            &test_locks(),
            "alice@test.com",
            attachment_eml(),
            sentinel_ip(),
            None,
        )
        .unwrap();

        let alice = inbox(tmp.path(), "alice");
        assert!(!alice.join("attachments").exists());

        let bundles: Vec<_> = std::fs::read_dir(&alice)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().is_dir())
            .collect();
        assert_eq!(bundles.len(), 1);
        let bundle = bundles[0].path();
        let stem = bundle.file_name().unwrap().to_string_lossy().to_string();

        let md = bundle.join(format!("{stem}.md"));
        assert!(md.exists(), "bundle md {md:?} should exist");
        let att_path = bundle.join("notes.txt");
        assert!(att_path.exists(), "attachment at {att_path:?} should exist");

        let content = std::fs::read_to_string(&att_path).unwrap();
        assert!(content.contains("These are my notes."));

        let md_content = std::fs::read_to_string(&md).unwrap();
        let table = parse_toml_frontmatter(&md_content);
        let attachments = table.get("attachments").unwrap().as_array().unwrap();
        assert_eq!(attachments.len(), 1);
        let att = attachments[0].as_table().unwrap();
        assert_eq!(att.get("filename").unwrap().as_str().unwrap(), "notes.txt");
        assert_eq!(att.get("path").unwrap().as_str().unwrap(), "notes.txt");
    }

    #[test]
    fn multiple_attachments_in_single_bundle() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(tmp.path());
        ingest_email(
            &config,
            &test_locks(),
            "alice@test.com",
            multi_attachment_eml(),
            sentinel_ip(),
            None,
        )
        .unwrap();

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
        let tmp = TempDir::new().unwrap();
        let config = test_config(tmp.path());

        ingest_email(
            &config,
            &test_locks(),
            "alice@test.com",
            attachment_eml(),
            sentinel_ip(),
            None,
        )
        .unwrap();
        ingest_email(
            &config,
            &test_locks(),
            "alice@test.com",
            attachment_eml(),
            sentinel_ip(),
            None,
        )
        .unwrap();

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
        ingest_email(
            &config,
            &test_locks(),
            "alice@test.com",
            plain_text_eml(),
            sentinel_ip(),
            None,
        )
        .unwrap();

        let alice = inbox(tmp.path(), "alice");
        let dirs: Vec<_> = std::fs::read_dir(&alice)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().is_dir())
            .collect();
        assert!(dirs.is_empty(), "flat layout must not create directories");

        let entries = collect_md_files(&alice);
        let md_content = std::fs::read_to_string(&entries[0]).unwrap();
        let table = parse_toml_frontmatter(&md_content);
        // Empty attachments vec is omitted from TOML output
        assert!(!table.contains_key("attachments"));
    }

    #[test]
    fn extract_local_part_works() {
        assert_eq!(extract_local_part("alice@test.com"), "alice");
        assert_eq!(extract_local_part("bob"), "bob");
    }

    #[test]
    fn sanitize_filename_embedded_nul() {
        // NUL must never appear in a sanitized filename; its removal must
        // not leave a bare `_` as the whole name.
        let out = sanitize_attachment_filename("bad\0name.txt", 0);
        assert!(!out.contains('\0'));
        assert!(out.contains("name.txt"));
    }

    #[test]
    fn sanitize_filename_cr_lf_stripped() {
        let out = sanitize_attachment_filename("report\r\nline2.pdf", 0);
        assert!(!out.contains('\r') && !out.contains('\n'));
        assert!(out.ends_with(".pdf"));
    }

    #[test]
    fn sanitize_filename_path_traversal_stripped() {
        // `Path::file_name` already collapses `../../etc/passwd` to `passwd`,
        // but this is defense in depth: even if the input arrived whole, the
        // separators would collapse to `_`s.
        let out = sanitize_attachment_filename("../../etc/passwd", 0);
        assert!(!out.contains('/'));
        assert!(out == "passwd" || out.contains("passwd"));
    }

    #[test]
    fn sanitize_filename_leading_dash_stripped() {
        // `-rf` as a filename is dangerous to naive downstream `rm` wrappers.
        // The leading `-` must be stripped.
        let out = sanitize_attachment_filename("-rf", 0);
        assert!(!out.starts_with('-'));
        // May be "rf" or may fall back to attachment-0 if the whole name
        // was trimmed away; both are safe.
        assert!(out == "rf" || out == "attachment-0");
    }

    #[test]
    fn sanitize_filename_500_char_truncated_under_cap_on_char_boundary() {
        let raw: String = "a".repeat(500);
        let out = sanitize_attachment_filename(&raw, 0);
        assert!(
            out.len() <= ATTACHMENT_FILENAME_MAX_BYTES,
            "sanitized name must be ≤{ATTACHMENT_FILENAME_MAX_BYTES} bytes: {}",
            out.len()
        );
        // UTF-8 boundary. ASCII is trivially aligned; just sanity check.
        assert!(out.is_char_boundary(out.len()));
    }

    #[test]
    fn sanitize_filename_empty_after_sanitize_falls_back() {
        // A name made entirely of control characters sanitizes to empty,
        // which must fall back to `attachment-<index>`.
        let all_controls = "\0\0\r\n\x01\x02";
        assert_eq!(
            sanitize_attachment_filename(all_controls, 3),
            "attachment-3"
        );
        assert_eq!(sanitize_attachment_filename("", 0), "attachment-0");
    }

    #[test]
    fn sanitize_filename_windows_style_backslash() {
        // On Unix, `\\` is NOT a path separator, so `Path::file_name` keeps
        // `a\\b\\c.pdf` as-is. The sanitizer must still strip the backslashes
        // so the filename doesn't confuse Windows consumers or shell escapes.
        let out = sanitize_attachment_filename("a\\b\\c.pdf", 0);
        assert!(!out.contains('\\'));
        assert!(out.ends_with(".pdf"));
    }

    #[test]
    fn sanitize_filename_nfd_normalized_to_nfc() {
        // NFD "é" (e + combining acute) normalizes to precomposed NFC "é"
        // so filesystem collisions between NFC/NFD siblings can't happen
        // downstream.
        let nfd = "caf\u{0065}\u{0301}.pdf";
        let out = sanitize_attachment_filename(nfd, 0);
        assert!(
            out.contains("caf\u{00E9}"),
            "NFD must collapse to precomposed é: {out}"
        );
    }

    #[test]
    fn sanitize_filename_bidi_override_stripped() {
        // An RLO (U+202E) embedded in the filename can reorder glyphs in
        // logs/agent output so `file.txt` displays as `file.gpj`. The
        // sanitizer must strip all bidi overrides.
        let bidi = "safe\u{202E}fdp.pdf";
        let out = sanitize_attachment_filename(bidi, 0);
        assert!(!out.chars().any(|c| c == '\u{202E}'));
    }

    #[test]
    fn sanitize_filename_zero_width_joiner_stripped() {
        let zwj = "inv\u{200D}oice.pdf";
        let out = sanitize_attachment_filename(zwj, 0);
        assert!(!out.chars().any(|c| c == '\u{200D}'));
    }

    #[test]
    fn sanitize_filename_unsafe_runs_collapse_to_single_underscore() {
        // Multiple unsafe chars in a row must collapse to one `_`, not spray
        // `____` across the filename.
        let out = sanitize_attachment_filename("a////b\\\\c.pdf", 0);
        // Count runs of `_`: each should be length 1.
        let has_long_run = out.split(|c: char| c != '_').any(|s| s.len() > 1);
        assert!(
            !has_long_run,
            "unsafe-char runs must collapse to a single `_`: {out}"
        );
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

        ingest_email(
            &config,
            &test_locks(),
            "alice@test.com",
            eml,
            sentinel_ip(),
            None,
        )
        .unwrap();

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
    fn attachment_malicious_names_sanitized_integration() {
        // S43-7 integration: two attachments with hostile names.
        // - `../../etc/passwd`: path-traversal attempt.
        // - `\x00rce.sh`: embedded NUL + potentially flag-looking name.
        // Both must land INSIDE the bundle directory with names that
        // contain no path separators and no NUL bytes. `tmp/etc/` must
        // not exist; nothing escaped the bundle.
        let tmp = TempDir::new().unwrap();
        let config = test_config(tmp.path());

        let eml = b"From: sender@example.com\r\n\
            To: alice@test.com\r\n\
            Subject: Malicious\r\n\
            Date: Thu, 01 Jan 2025 12:00:00 +0000\r\n\
            Message-ID: <evil2@example.com>\r\n\
            MIME-Version: 1.0\r\n\
            Content-Type: multipart/mixed; boundary=\"b1\"\r\n\
            \r\n\
            --b1\r\n\
            Content-Type: text/plain; charset=utf-8\r\n\
            \r\n\
            Body.\r\n\
            --b1\r\n\
            Content-Type: text/plain; name=\"../../etc/passwd\"\r\n\
            Content-Disposition: attachment; filename=\"../../etc/passwd\"\r\n\
            \r\n\
            passwd-contents\r\n\
            --b1\r\n\
            Content-Type: application/x-sh; name=\"\x00rce.sh\"\r\n\
            Content-Disposition: attachment; filename=\"\x00rce.sh\"\r\n\
            \r\n\
            #!/bin/sh\r\n\
            --b1--\r\n";

        ingest_email(
            &config,
            &test_locks(),
            "alice@test.com",
            eml,
            sentinel_ip(),
            None,
        )
        .unwrap();

        let alice = inbox(tmp.path(), "alice");
        let bundles: Vec<_> = std::fs::read_dir(&alice)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().is_dir())
            .collect();
        assert_eq!(bundles.len(), 1);
        let bundle = bundles[0].path();

        // Nothing escaped.
        assert!(
            !tmp.path().join("etc").exists(),
            "attachment sanitization must prevent `etc/` escape"
        );

        // Walk the bundle and assert every file is safe.
        let entries: Vec<_> = std::fs::read_dir(&bundle)
            .unwrap()
            .filter_map(|e| e.ok())
            .collect();
        let mut saw_attachment_file = false;
        for e in entries {
            let name = e.file_name().to_string_lossy().into_owned();
            // `<stem>.md` is the email body itself.
            if name.ends_with(".md") {
                continue;
            }
            saw_attachment_file = true;
            assert!(!name.contains('/'), "leaked path separator in {name}");
            assert!(!name.contains('\\'), "leaked backslash in {name}");
            assert!(!name.contains('\0'), "leaked NUL in {name}");
            assert!(!name.starts_with('-'), "leading dash in {name}");
        }
        assert!(
            saw_attachment_file,
            "expected at least one sanitized attachment file in the bundle"
        );
    }

    #[test]
    fn toml_handles_special_characters() {
        let meta = InboundFrontmatter {
            id: "2025-01-01-001".to_string(),
            message_id: "<test@example.com>".to_string(),
            thread_id: "0123456789abcdef".to_string(),
            from: "test\n+++\ninjected: true".to_string(),
            to: "to@test.com".to_string(),
            cc: None,
            reply_to: None,
            delivered_to: "to@test.com".to_string(),
            subject: "colons: and #hashes".to_string(),
            date: "2025-01-01T00:00:00Z".to_string(),
            received_at: "2025-01-01T00:00:01Z".to_string(),
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
            mailbox: "catchall".to_string(),
            read: false,
            read_at: None,
            labels: vec![],
        };

        let toml_str = toml::to_string(&meta).unwrap();
        let parsed: toml::Value = toml::from_str(&toml_str).unwrap();
        let table = parsed.as_table().unwrap();
        let from = table.get("from").unwrap();
        assert_eq!(from.as_str().unwrap(), "test\n+++\ninjected: true");
    }

    #[test]
    fn unsigned_email_has_dkim_none_spf_none_dmarc_none() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(tmp.path());
        ingest_email(
            &config,
            &test_locks(),
            "alice@test.com",
            plain_text_eml(),
            sentinel_ip(),
            None,
        )
        .unwrap();

        let entries = collect_md_files(&inbox(tmp.path(), "alice"));
        let content = std::fs::read_to_string(&entries[0]).unwrap();
        let table = parse_toml_frontmatter(&content);

        assert_eq!(table.get("dkim").unwrap().as_str().unwrap(), "none");
        assert_eq!(table.get("spf").unwrap().as_str().unwrap(), "none");
        assert_eq!(table.get("dmarc").unwrap().as_str().unwrap(), "none");
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
    fn parse_ip_from_received_rejects_bare_ip_in_comment() {
        // S43-4: previously the whitespace-split fallback would happily pick
        // up an IP embedded in a HELO or a free-text comment. That fallback
        // is gone; only the bracketed RFC 5321 form is trusted.
        let line = "Received: from evil.example.com (HELO mail.legit 1.2.3.4 via clever.host)";
        assert_eq!(parse_ip_from_received(line), None);
    }

    #[test]
    fn parse_ip_from_received_rejects_hostname_with_dotted_quad() {
        // A hostname like `host-1.2.3.4.example.com` must NOT be parsed as
        // an IP; the old fallback was susceptible to this.
        let line = "Received: from host-1.2.3.4.example.com by mx.local";
        assert_eq!(parse_ip_from_received(line), None);
    }

    #[test]
    fn parse_ip_from_received_rejects_ipv6_in_free_text() {
        // Same principle for IPv6: no brackets → no trust.
        let line = "Received: from mail.example.com (claims 2001:db8::abcd)";
        assert_eq!(parse_ip_from_received(line), None);
    }

    #[test]
    fn parse_ip_from_received_real_fixture_shapes() {
        // Three canonical `Received:` header shapes pulled from real
        // inbound mail. All three use the bracketed form.
        let gmail = "Received: from mail-sor-f41.google.com (mail-sor-f41.google.com. [209.85.220.41]) by mx.local";
        assert_eq!(
            parse_ip_from_received(gmail),
            Some("209.85.220.41".parse().unwrap())
        );

        let microsoft = "Received: from NAM10-BN7-obe.outbound.protection.outlook.com ([40.107.93.79]) by mx.local";
        assert_eq!(
            parse_ip_from_received(microsoft),
            Some("40.107.93.79".parse().unwrap())
        );

        let postfix =
            "Received: from mta.example.com (unknown [198.51.100.7]) by mx.local with ESMTPS";
        assert_eq!(
            parse_ip_from_received(postfix),
            Some("198.51.100.7".parse().unwrap())
        );
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
    fn frontmatter_includes_dkim_spf_dmarc_fields() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(tmp.path());
        ingest_email(
            &config,
            &test_locks(),
            "alice@test.com",
            plain_text_eml(),
            sentinel_ip(),
            None,
        )
        .unwrap();

        let entries = collect_md_files(&inbox(tmp.path(), "alice"));
        let content = std::fs::read_to_string(&entries[0]).unwrap();
        let table = parse_toml_frontmatter(&content);

        assert!(table.contains_key("dkim"));
        assert!(table.contains_key("spf"));
        assert!(table.contains_key("dmarc"));
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

        ingest_email(
            &config,
            &test_locks(),
            "agent@test.com",
            raw,
            sentinel_ip(),
            None,
        )
        .unwrap();

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
    fn concurrent_same_subject_attachment_ingests_do_not_corrupt_each_other() {
        use std::sync::Arc;
        use std::thread;

        let tmp = TempDir::new().unwrap();
        let config = Arc::new(test_config(tmp.path()));
        let locks = Arc::new(MailboxLocks::new());

        let mut handles = Vec::new();
        for _ in 0..8 {
            let cfg = Arc::clone(&config);
            let locks = Arc::clone(&locks);
            handles.push(thread::spawn(move || {
                ingest_email(
                    &cfg,
                    &locks,
                    "alice@test.com",
                    attachment_eml(),
                    sentinel_ip(),
                    None,
                )
                .unwrap();
            }));
        }
        for h in handles {
            h.join().unwrap();
        }

        let alice = inbox(tmp.path(), "alice");
        let bundles: Vec<_> = std::fs::read_dir(&alice)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().is_dir())
            .collect();
        assert_eq!(bundles.len(), 8, "each ingest must own its bundle dir");

        for b in bundles {
            let path = b.path();
            let stem = path.file_name().unwrap().to_str().unwrap().to_string();
            let md = path.join(format!("{stem}.md"));
            let attachment = path.join("notes.txt");
            assert!(md.exists(), "md file present in {stem}");
            assert!(attachment.exists(), "attachment present in {stem}");
            let body = std::fs::read_to_string(&attachment).unwrap();
            assert!(body.contains("These are my notes."));
        }
    }

    #[test]
    fn rapid_same_subject_ingests_get_unique_paths() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(tmp.path());
        ingest_email(
            &config,
            &test_locks(),
            "alice@test.com",
            plain_text_eml(),
            sentinel_ip(),
            None,
        )
        .unwrap();
        ingest_email(
            &config,
            &test_locks(),
            "alice@test.com",
            plain_text_eml(),
            sentinel_ip(),
            None,
        )
        .unwrap();

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

    #[test]
    fn select_spf_inputs_prefers_envelope_over_headers() {
        // When the SMTP session hands us a real peer IP and envelope MAIL FROM,
        // those take precedence over anything we could scrape from Received:
        // or From: headers. This is the fix for the reported SPF=none bug:
        // messages delivered from Gmail (peer IP within _spf.google.com) with
        // envelope sender `chua@uzyn.com` must verify against the authoritative
        // envelope values, not whatever shows up in the message body.
        let raw = b"Received: from some.internal.relay.example.com ([10.0.0.1])\r\n\
            From: Spoofed <spoof@evil.example>\r\n\
            To: agent@test.com\r\n\
            \r\n\
            body\r\n";

        let peer_ip: IpAddr = "209.85.219.48".parse().unwrap();
        let (ip, mail_from) = select_spf_inputs(raw, peer_ip, Some("chua@uzyn.com"));
        assert_eq!(ip, Some(peer_ip));
        assert_eq!(mail_from.as_deref(), Some("chua@uzyn.com"));
    }

    #[test]
    fn select_spf_inputs_falls_back_to_headers_for_manual_stdin() {
        // `aimx ingest` stdin path has no SMTP session, so peer_ip is the
        // 0.0.0.0 sentinel and envelope_mail_from is None. In that case we
        // fall back to parsing Received: and From: headers (best-effort,
        // matching pre-fix behavior).
        let raw = b"Received: from mail.example.com (mail.example.com [203.0.113.5])\r\n\
            From: Alice <alice@example.com>\r\n\
            To: agent@test.com\r\n\
            \r\n\
            body\r\n";

        let sentinel: IpAddr = "0.0.0.0".parse().unwrap();
        let (ip, mail_from) = select_spf_inputs(raw, sentinel, None);
        assert_eq!(ip, Some("203.0.113.5".parse().unwrap()));
        assert_eq!(mail_from.as_deref(), Some("alice@example.com"));
    }

    #[test]
    fn select_spf_inputs_empty_envelope_string_falls_back() {
        // Defensive: an empty envelope string (should not happen; session
        // resets reverse_path to "" after RSET, but be explicit) falls back
        // to header parsing rather than producing an empty mail_from.
        let raw = b"From: Alice <alice@example.com>\r\nTo: t@test.com\r\n\r\n";
        let peer_ip: IpAddr = "203.0.113.5".parse().unwrap();
        let (ip, mail_from) = select_spf_inputs(raw, peer_ip, Some(""));
        assert_eq!(ip, Some(peer_ip));
        assert_eq!(mail_from.as_deref(), Some("alice@example.com"));
    }

    #[test]
    fn frontmatter_new_fields_populated() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(tmp.path());
        let ip: IpAddr = "203.0.113.50".parse().unwrap();
        ingest_email(
            &config,
            &test_locks(),
            "alice@test.com",
            plain_text_eml(),
            ip,
            None,
        )
        .unwrap();

        let entries = collect_md_files(&inbox(tmp.path(), "alice"));
        let content = std::fs::read_to_string(&entries[0]).unwrap();
        let table = parse_toml_frontmatter(&content);

        assert!(table.contains_key("thread_id"));
        let thread_id = table.get("thread_id").unwrap().as_str().unwrap();
        assert_eq!(thread_id.len(), 16);

        assert!(table.contains_key("received_at"));

        assert_eq!(
            table.get("received_from_ip").unwrap().as_str().unwrap(),
            "203.0.113.50"
        );

        assert_eq!(
            table.get("delivered_to").unwrap().as_str().unwrap(),
            "alice@test.com"
        );

        assert!(table.get("size_bytes").unwrap().as_integer().unwrap() > 0);

        assert_eq!(table.get("trusted").unwrap().as_str().unwrap(), "none");
        assert_eq!(table.get("dmarc").unwrap().as_str().unwrap(), "none");
    }

    #[test]
    fn frontmatter_received_from_ip_omitted_for_sentinel() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(tmp.path());
        ingest_email(
            &config,
            &test_locks(),
            "alice@test.com",
            plain_text_eml(),
            sentinel_ip(),
            None,
        )
        .unwrap();

        let entries = collect_md_files(&inbox(tmp.path(), "alice"));
        let content = std::fs::read_to_string(&entries[0]).unwrap();
        let table = parse_toml_frontmatter(&content);

        // 0.0.0.0 sentinel is omitted
        assert!(!table.contains_key("received_from_ip"));
    }

    #[test]
    fn frontmatter_list_id_populated() {
        let eml = b"From: sender@example.com\r\n\
            To: alice@test.com\r\n\
            Subject: List email\r\n\
            Date: Thu, 01 Jan 2025 12:00:00 +0000\r\n\
            Message-ID: <list1@example.com>\r\n\
            List-ID: <mylist.example.com>\r\n\
            \r\n\
            List message body.\r\n";

        let tmp = TempDir::new().unwrap();
        let config = test_config(tmp.path());
        ingest_email(
            &config,
            &test_locks(),
            "alice@test.com",
            eml,
            sentinel_ip(),
            None,
        )
        .unwrap();

        let entries = collect_md_files(&inbox(tmp.path(), "alice"));
        let content = std::fs::read_to_string(&entries[0]).unwrap();
        let table = parse_toml_frontmatter(&content);
        assert_eq!(
            table.get("list_id").unwrap().as_str().unwrap(),
            "<mylist.example.com>"
        );
    }

    #[test]
    fn frontmatter_auto_submitted_populated() {
        let eml = b"From: sender@example.com\r\n\
            To: alice@test.com\r\n\
            Subject: Auto reply\r\n\
            Date: Thu, 01 Jan 2025 12:00:00 +0000\r\n\
            Message-ID: <auto1@example.com>\r\n\
            Auto-Submitted: auto-replied\r\n\
            \r\n\
            Automatic reply.\r\n";

        let tmp = TempDir::new().unwrap();
        let config = test_config(tmp.path());
        ingest_email(
            &config,
            &test_locks(),
            "alice@test.com",
            eml,
            sentinel_ip(),
            None,
        )
        .unwrap();

        let entries = collect_md_files(&inbox(tmp.path(), "alice"));
        let content = std::fs::read_to_string(&entries[0]).unwrap();
        let table = parse_toml_frontmatter(&content);
        assert_eq!(
            table.get("auto_submitted").unwrap().as_str().unwrap(),
            "auto-replied"
        );
    }

    #[test]
    fn frontmatter_optional_headers_omitted_when_absent() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(tmp.path());
        ingest_email(
            &config,
            &test_locks(),
            "alice@test.com",
            plain_text_eml(),
            sentinel_ip(),
            None,
        )
        .unwrap();

        let entries = collect_md_files(&inbox(tmp.path(), "alice"));
        let content = std::fs::read_to_string(&entries[0]).unwrap();
        let table = parse_toml_frontmatter(&content);

        assert!(!table.contains_key("cc"));
        assert!(!table.contains_key("reply_to"));
        assert!(!table.contains_key("list_id"));
        assert!(!table.contains_key("auto_submitted"));
        assert!(!table.contains_key("in_reply_to"));
        assert!(!table.contains_key("references"));
        assert!(!table.contains_key("labels"));
    }

    #[test]
    fn thread_id_deterministic_for_same_message() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(tmp.path());

        ingest_email(
            &config,
            &test_locks(),
            "alice@test.com",
            plain_text_eml(),
            sentinel_ip(),
            None,
        )
        .unwrap();
        ingest_email(
            &config,
            &test_locks(),
            "alice@test.com",
            plain_text_eml(),
            sentinel_ip(),
            None,
        )
        .unwrap();

        let entries = collect_md_files(&inbox(tmp.path(), "alice"));
        assert_eq!(entries.len(), 2);

        let content1 = std::fs::read_to_string(&entries[0]).unwrap();
        let content2 = std::fs::read_to_string(&entries[1]).unwrap();

        let table1 = parse_toml_frontmatter(&content1);
        let table2 = parse_toml_frontmatter(&content2);

        assert_eq!(
            table1.get("thread_id").unwrap().as_str().unwrap(),
            table2.get("thread_id").unwrap().as_str().unwrap(),
        );
    }
}
