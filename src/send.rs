//! `aimx send`: thin UDS client for `AIMX/1 SEND`.
//!
//! This module does not sign, load DKIM keys, or talk to MX servers
//! directly. It composes an unsigned RFC 5322 message, opens
//! `/run/aimx/aimx.sock`, writes a single `AIMX/1 SEND` request frame,
//! and maps the daemon's response to a stable CLI exit code + user
//! message.
//!
//! `aimx send` does not read `/etc/aimx/config.toml` at all. The daemon
//! parses `From:` out of the submitted message body and resolves the
//! sender mailbox from its in-memory Config. This lets a non-root
//! operator run `aimx send` on a default install where `config.toml` is
//! `0640 root:root`.
//!
//! Signing and MX delivery live in `aimx serve` (see `send_handler.rs` and
//! `transport.rs`).

use crate::cli::SendArgs;
use crate::send_protocol::{self, ErrCode, SendRequest};
use crate::serve::aimx_socket_path;
use crate::term;
use base64::Engine;
use chrono::Utc;
use std::io;
use std::path::Path;
use uuid::Uuid;

#[derive(Debug)]
pub struct ComposeResult {
    pub message: Vec<u8>,
}

fn escape_filename(name: &str) -> String {
    name.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\r', "")
        .replace('\n', " ")
}

fn sanitize_header_value(value: &str) -> String {
    value.replace(['\r', '\n'], "")
}

fn validate_header_value(name: &str, value: &str) -> Result<(), Box<dyn std::error::Error>> {
    if value.contains('\r') || value.contains('\n') {
        return Err(
            format!("Header '{name}' contains CRLF characters. Possible header injection").into(),
        );
    }
    Ok(())
}

fn write_common_headers(
    msg: &mut String,
    args: &SendArgs,
    date: &str,
    message_id: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    validate_header_value("From", &args.from)?;
    validate_header_value("To", &args.to)?;
    validate_header_value("Subject", &args.subject)?;

    let from = sanitize_header_value(&args.from);
    let to = sanitize_header_value(&args.to);
    let subject = sanitize_header_value(&args.subject);

    msg.push_str(&format!("From: {from}\r\n"));
    msg.push_str(&format!("To: {to}\r\n"));
    msg.push_str(&format!("Subject: {subject}\r\n"));
    msg.push_str(&format!("Date: {date}\r\n"));
    msg.push_str(&format!("Message-ID: {message_id}\r\n"));

    if let Some(ref reply_to) = args.reply_to {
        let reply_id = normalize_message_id(&sanitize_header_value(reply_to));
        msg.push_str(&format!("In-Reply-To: {reply_id}\r\n"));
        let refs = match &args.references {
            Some(r) if !r.trim().is_empty() => sanitize_header_value(r),
            _ => reply_id.clone(),
        };
        msg.push_str(&format!("References: {refs}\r\n"));
    }

    msg.push_str("MIME-Version: 1.0\r\n");
    Ok(())
}

pub fn compose_message(args: &SendArgs) -> Result<ComposeResult, Box<dyn std::error::Error>> {
    validate_attachments(&args.attachments)?;

    let sanitized_from = sanitize_header_value(&args.from);
    let domain = sanitized_from.split('@').nth(1).unwrap_or("localhost");
    let message_id = format!("<{}@{domain}>", Uuid::new_v4());
    let date = Utc::now().to_rfc2822();
    let normalized_body = normalize_crlf(&args.body);

    if args.attachments.is_empty() {
        let mut msg = String::new();
        write_common_headers(&mut msg, args, &date, &message_id)?;
        msg.push_str("Content-Type: text/plain; charset=utf-8\r\n");
        msg.push_str("\r\n");
        msg.push_str(&normalized_body);
        msg.push_str("\r\n");

        return Ok(ComposeResult {
            message: msg.into_bytes(),
        });
    }

    let boundary = format!("aimx-{}", Uuid::new_v4().simple());
    let mut msg = String::new();
    write_common_headers(&mut msg, args, &date, &message_id)?;
    msg.push_str(&format!(
        "Content-Type: multipart/mixed; boundary=\"{boundary}\"\r\n"
    ));
    msg.push_str("\r\n");

    msg.push_str(&format!("--{boundary}\r\n"));
    msg.push_str("Content-Type: text/plain; charset=utf-8\r\n");
    msg.push_str("\r\n");
    msg.push_str(&normalized_body);
    msg.push_str("\r\n");

    for path_str in &args.attachments {
        let path = Path::new(path_str);
        let raw_name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("attachment");
        let safe_name = escape_filename(raw_name);
        let content_type = mime_guess::from_path(path)
            .first_or_octet_stream()
            .to_string();
        let file_data = std::fs::read(path)?;
        let encoded = base64::engine::general_purpose::STANDARD.encode(&file_data);

        msg.push_str(&format!("--{boundary}\r\n"));
        msg.push_str(&format!(
            "Content-Type: {content_type}; name=\"{safe_name}\"\r\n"
        ));
        msg.push_str(&format!(
            "Content-Disposition: attachment; filename=\"{safe_name}\"\r\n"
        ));
        msg.push_str("Content-Transfer-Encoding: base64\r\n");
        msg.push_str("\r\n");

        for chunk in encoded.as_bytes().chunks(76) {
            msg.push_str(std::str::from_utf8(chunk).unwrap_or(""));
            msg.push_str("\r\n");
        }
    }

    msg.push_str(&format!("--{boundary}--\r\n"));

    Ok(ComposeResult {
        message: msg.into_bytes(),
    })
}

fn normalize_crlf(text: &str) -> String {
    text.replace("\r\n", "\n")
        .replace('\r', "\n")
        .replace('\n', "\r\n")
}

fn normalize_message_id(id: &str) -> String {
    let trimmed = id.trim();
    if trimmed.starts_with('<') && trimmed.ends_with('>') {
        trimmed.to_string()
    } else {
        format!("<{trimmed}>")
    }
}

fn validate_attachments(paths: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    for path_str in paths {
        let path = Path::new(path_str);
        if !path.exists() {
            return Err(format!("Attachment file not found: {path_str}").into());
        }
        if !path.is_file() {
            return Err(format!("Attachment path is not a file: {path_str}").into());
        }
    }
    Ok(())
}

pub fn build_references(original_references: Option<&str>, original_message_id: &str) -> String {
    let mid = normalize_message_id(original_message_id);

    match original_references {
        Some(refs) if !refs.trim().is_empty() => format!("{} {mid}", refs.trim()),
        _ => mid,
    }
}

/// CLI exit codes exposed by `aimx send`. Kept as named constants so the
/// unit tests and downstream tooling can reference them symbolically.
pub const EXIT_OK: i32 = 0;
pub const EXIT_DAEMON_ERR: i32 = 1;
pub const EXIT_CONNECT: i32 = 2;
pub const EXIT_MALFORMED: i32 = 3;

/// Outcome of submitting one `AIMX/1 SEND` request.
#[derive(Debug)]
pub enum SubmitOutcome {
    Ok {
        message_id: String,
    },
    Err {
        code: ErrCode,
        reason: String,
    },
    /// The daemon replied with something that isn't a valid `AIMX/1` frame.
    Malformed(String),
}

/// Submit a prepared [`SendRequest`] over the UDS at `socket_path` and return
/// the parsed outcome. Extracted so integration tests (and the real client)
/// share the exact same framing logic.
pub async fn submit_request(
    socket_path: &Path,
    request: &SendRequest,
) -> Result<SubmitOutcome, io::Error> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let stream = tokio::net::UnixStream::connect(socket_path).await?;
    let (mut reader, mut writer) = stream.into_split();

    send_protocol::write_request(&mut writer, request).await?;
    writer.shutdown().await.ok();

    let mut buf = Vec::with_capacity(128);
    reader.read_to_end(&mut buf).await?;

    Ok(parse_response_line(&buf))
}

fn parse_response_line(buf: &[u8]) -> SubmitOutcome {
    let text = match std::str::from_utf8(buf) {
        Ok(s) => s,
        Err(_) => return SubmitOutcome::Malformed("response is not UTF-8".to_string()),
    };

    let line = text.lines().next().unwrap_or("").trim_end_matches('\r');
    if line.is_empty() {
        return SubmitOutcome::Malformed("empty response from daemon".to_string());
    }

    let rest = match line.strip_prefix("AIMX/1 ") {
        Some(r) => r,
        None => {
            return SubmitOutcome::Malformed(format!("unexpected response: {line:?}"));
        }
    };

    if let Some(message_id) = rest.strip_prefix("OK ") {
        return SubmitOutcome::Ok {
            message_id: message_id.trim().to_string(),
        };
    }
    if let Some(err_body) = rest.strip_prefix("ERR ") {
        let (code_str, reason) = err_body.split_once(' ').unwrap_or((err_body, ""));
        // The frame may carry a follow-up `Code:` header on
        // subsequent lines. We scan for it so clients can branch on
        // the structured code even if the status-line token drifts.
        let header_code = header_code(text);
        let code = match (header_code, ErrCode::from_str(code_str)) {
            (Some(h), _) => h,
            (None, Some(inline)) => inline,
            _ => {
                return SubmitOutcome::Malformed(format!(
                    "unknown ERR code {code_str:?} in response"
                ));
            }
        };
        return SubmitOutcome::Err {
            code,
            reason: reason.trim().to_string(),
        };
    }

    SubmitOutcome::Malformed(format!("unexpected response: {line:?}"))
}

/// Extract the first `Code:` header from the response text, if present.
/// The structured code on the wire survives any future change to the
/// status-line token.
fn header_code(text: &str) -> Option<ErrCode> {
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

/// Is the current process running as root? Factored out so tests can override.
#[cfg(unix)]
fn current_is_root() -> bool {
    // SAFETY: libc::geteuid is a simple syscall with no preconditions.
    unsafe { libc::geteuid() == 0 }
}

#[cfg(not(unix))]
fn current_is_root() -> bool {
    false
}

/// Write the root-refusal error message to `stderr` and return the exit
/// code. Pure function so tests can verify the message without observing
/// real stderr or requiring an actual root process.
pub fn render_root_refusal<E: io::Write>(stderr: &mut E) -> i32 {
    let _ = writeln!(
        stderr,
        "{} send is a per-user operation, run without sudo",
        term::error("Error:")
    );
    EXIT_CONNECT
}

/// Map a daemon response to a CLI exit code + stderr/stdout messaging, then
/// write the output through the caller-supplied sinks. Pure function so
/// tests don't have to observe real stderr.
pub fn render_outcome<O: io::Write, E: io::Write>(
    outcome: SubmitOutcome,
    stdout: &mut O,
    stderr: &mut E,
) -> i32 {
    match outcome {
        SubmitOutcome::Ok { message_id } => {
            let _ = writeln!(
                stderr,
                "{}",
                term::success(&format!("Email sent.\nMessage-ID: {message_id}"))
            );
            let _ = stdout.write_all(message_id.as_bytes());
            let _ = stdout.write_all(b"\n");
            EXIT_OK
        }
        SubmitOutcome::Err { code, reason } => {
            let _ = writeln!(
                stderr,
                "{} [{}]: {reason}",
                term::error("Error"),
                code.as_str()
            );
            EXIT_DAEMON_ERR
        }
        SubmitOutcome::Malformed(reason) => {
            let _ = writeln!(
                stderr,
                "{} malformed response from aimx daemon: {reason}",
                term::error("Error:")
            );
            EXIT_MALFORMED
        }
    }
}

/// Build the `AIMX/1 SEND` request frame from composed CLI args. The
/// request carries no `From-Mailbox:` header. The daemon parses the
/// `From:` header out of the body itself and resolves the mailbox
/// against its in-memory Config.
pub fn build_request(args: &SendArgs) -> Result<SendRequest, String> {
    let composed = compose_message(args).map_err(|e| e.to_string())?;
    Ok(SendRequest {
        body: composed.message,
    })
}

fn run_inner(args: SendArgs) -> Result<(), Box<dyn std::error::Error>> {
    if current_is_root() {
        let code = render_root_refusal(&mut io::stderr());
        std::process::exit(code);
    }

    let request = match build_request(&args) {
        Ok(r) => r,
        Err(e) => {
            return Err(e.into());
        }
    };

    let socket = aimx_socket_path();

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("Failed to create tokio runtime: {e}"))?;

    let submit_result = rt.block_on(async { submit_request(&socket, &request).await });

    let outcome = match submit_result {
        Ok(o) => o,
        Err(e) => {
            handle_connect_error(&socket, &e);
            std::process::exit(EXIT_CONNECT);
        }
    };

    let code = render_outcome(outcome, &mut io::stdout(), &mut io::stderr());
    if code != EXIT_OK {
        std::process::exit(code);
    }
    Ok(())
}

fn handle_connect_error(socket: &Path, err: &io::Error) {
    if err.kind() == io::ErrorKind::NotFound {
        eprintln!(
            "{} aimx daemon not running. Check 'systemctl status aimx'",
            term::error("Error:")
        );
    } else {
        eprintln!(
            "{} Failed to connect to aimx daemon at {}: {err}",
            term::error("Error:"),
            socket.display()
        );
    }
}

pub fn run(args: SendArgs) -> Result<(), Box<dyn std::error::Error>> {
    run_inner(args)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::send_protocol::SendResponse;
    use std::sync::{Arc, Mutex};

    fn test_args() -> SendArgs {
        SendArgs {
            from: "agent@example.com".to_string(),
            to: "user@gmail.com".to_string(),
            subject: "Test Subject".to_string(),
            body: "Hello, world!".to_string(),
            reply_to: None,
            references: None,
            attachments: vec![],
        }
    }

    #[test]
    fn compose_has_required_headers() {
        let args = test_args();
        let result = compose_message(&args).unwrap();
        let text = String::from_utf8(result.message).unwrap();

        assert!(text.contains("From: agent@example.com\r\n"));
        assert!(text.contains("To: user@gmail.com\r\n"));
        assert!(text.contains("Subject: Test Subject\r\n"));
        assert!(text.contains("Date: "));
        assert!(text.contains("Message-ID: <"));
        assert!(text.contains("@example.com>\r\n"));
    }

    #[test]
    fn compose_has_body() {
        let args = test_args();
        let result = compose_message(&args).unwrap();
        let text = String::from_utf8(result.message).unwrap();

        assert!(text.contains("\r\n\r\nHello, world!\r\n"));
    }

    #[test]
    fn message_id_format() {
        let args = test_args();
        let result = compose_message(&args).unwrap();
        let text = String::from_utf8(result.message).unwrap();

        let mid_line = text.lines().find(|l| l.starts_with("Message-ID:")).unwrap();
        assert!(mid_line.contains('<'));
        assert!(mid_line.contains('>'));
        assert!(mid_line.contains("@example.com"));
    }

    #[test]
    fn reply_to_sets_in_reply_to_header() {
        let mut args = test_args();
        args.reply_to = Some("<original123@example.com>".to_string());

        let result = compose_message(&args).unwrap();
        let text = String::from_utf8(result.message).unwrap();

        assert!(text.contains("In-Reply-To: <original123@example.com>\r\n"));
    }

    #[test]
    fn reply_to_sets_references_header() {
        let mut args = test_args();
        args.reply_to = Some("<original123@example.com>".to_string());

        let result = compose_message(&args).unwrap();
        let text = String::from_utf8(result.message).unwrap();

        assert!(text.contains("References: <original123@example.com>\r\n"));
    }

    #[test]
    fn references_provided_uses_explicit_value() {
        let mut args = test_args();
        args.reply_to = Some("<reply@example.com>".to_string());
        args.references = Some("<first@example.com> <second@example.com>".to_string());

        let result = compose_message(&args).unwrap();
        let text = String::from_utf8(result.message).unwrap();

        assert!(text.contains("In-Reply-To: <reply@example.com>\r\n"));
        assert!(text.contains("References: <first@example.com> <second@example.com>\r\n"));
    }

    #[test]
    fn reply_to_normalizes_bare_message_id() {
        let mut args = test_args();
        args.reply_to = Some("original123@example.com".to_string());

        let result = compose_message(&args).unwrap();
        let text = String::from_utf8(result.message).unwrap();

        assert!(text.contains("In-Reply-To: <original123@example.com>\r\n"));
        assert!(text.contains("References: <original123@example.com>\r\n"));
    }

    #[test]
    fn no_reply_to_omits_threading_headers() {
        let args = test_args();
        let result = compose_message(&args).unwrap();
        let text = String::from_utf8(result.message).unwrap();

        assert!(!text.contains("In-Reply-To:"));
        assert!(!text.contains("References:"));
    }

    #[test]
    fn build_references_from_message_id_only() {
        let refs = build_references(None, "abc@example.com");
        assert_eq!(refs, "<abc@example.com>");
    }

    #[test]
    fn build_references_appends_to_existing() {
        let refs = build_references(Some("<first@example.com>"), "<second@example.com>");
        assert_eq!(refs, "<first@example.com> <second@example.com>");
    }

    #[test]
    fn build_references_chain() {
        let refs = build_references(
            Some("<first@example.com> <second@example.com>"),
            "<third@example.com>",
        );
        assert_eq!(
            refs,
            "<first@example.com> <second@example.com> <third@example.com>"
        );
    }

    #[test]
    fn single_attachment() {
        let tmp = tempfile::TempDir::new().unwrap();
        let file_path = tmp.path().join("test.txt");
        std::fs::write(&file_path, "file content").unwrap();

        let args = SendArgs {
            from: "agent@example.com".to_string(),
            to: "user@gmail.com".to_string(),
            subject: "With attachment".to_string(),
            body: "See attached.".to_string(),
            reply_to: None,
            references: None,
            attachments: vec![file_path.to_string_lossy().to_string()],
        };

        let result = compose_message(&args).unwrap();
        let text = String::from_utf8(result.message).unwrap();

        assert!(text.contains("multipart/mixed"));
        assert!(text.contains("Content-Disposition: attachment; filename=\"test.txt\""));
        assert!(text.contains("See attached."));
    }

    #[test]
    fn multiple_attachments() {
        let tmp = tempfile::TempDir::new().unwrap();
        let file1 = tmp.path().join("doc.pdf");
        let file2 = tmp.path().join("image.png");
        std::fs::write(&file1, "pdf content").unwrap();
        std::fs::write(&file2, "png content").unwrap();

        let args = SendArgs {
            from: "agent@example.com".to_string(),
            to: "user@gmail.com".to_string(),
            subject: "Multiple attachments".to_string(),
            body: "Two files.".to_string(),
            reply_to: None,
            references: None,
            attachments: vec![
                file1.to_string_lossy().to_string(),
                file2.to_string_lossy().to_string(),
            ],
        };

        let result = compose_message(&args).unwrap();
        let text = String::from_utf8(result.message).unwrap();

        assert!(text.contains("filename=\"doc.pdf\""));
        assert!(text.contains("filename=\"image.png\""));
    }

    #[test]
    fn attachment_mime_type_inference() {
        let tmp = tempfile::TempDir::new().unwrap();

        let pdf = tmp.path().join("doc.pdf");
        let png = tmp.path().join("image.png");
        let txt = tmp.path().join("notes.txt");
        std::fs::write(&pdf, "pdf").unwrap();
        std::fs::write(&png, "png").unwrap();
        std::fs::write(&txt, "txt").unwrap();

        let args = SendArgs {
            from: "a@b.com".to_string(),
            to: "c@d.com".to_string(),
            subject: "MIME test".to_string(),
            body: "body".to_string(),
            reply_to: None,
            references: None,
            attachments: vec![
                pdf.to_string_lossy().to_string(),
                png.to_string_lossy().to_string(),
                txt.to_string_lossy().to_string(),
            ],
        };

        let result = compose_message(&args).unwrap();
        let text = String::from_utf8(result.message).unwrap();

        assert!(text.contains("application/pdf"));
        assert!(text.contains("image/png"));
        assert!(text.contains("text/plain"));
    }

    #[test]
    fn missing_attachment_file_error() {
        let args = SendArgs {
            from: "a@b.com".to_string(),
            to: "c@d.com".to_string(),
            subject: "test".to_string(),
            body: "body".to_string(),
            reply_to: None,
            references: None,
            attachments: vec!["/nonexistent/file.txt".to_string()],
        };

        let result = compose_message(&args);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not found"));
    }

    #[test]
    fn compose_returns_message_id() {
        let args = test_args();
        let result = compose_message(&args).unwrap();
        let text = String::from_utf8(result.message).unwrap();
        let mid_line = text
            .lines()
            .find(|l| l.starts_with("Message-ID:"))
            .expect("composed message must contain a Message-ID header");
        let value = mid_line.trim_start_matches("Message-ID:").trim();
        assert!(value.starts_with('<'));
        assert!(value.ends_with('>'));
        assert!(value.contains('@'));
    }

    #[test]
    fn attachment_with_reply_to() {
        let tmp = tempfile::TempDir::new().unwrap();
        let file_path = tmp.path().join("data.csv");
        std::fs::write(&file_path, "a,b,c").unwrap();

        let args = SendArgs {
            from: "agent@example.com".to_string(),
            to: "user@gmail.com".to_string(),
            subject: "Re: Data".to_string(),
            body: "Here is the data.".to_string(),
            reply_to: Some("<orig@example.com>".to_string()),
            references: None,
            attachments: vec![file_path.to_string_lossy().to_string()],
        };

        let result = compose_message(&args).unwrap();
        let text = String::from_utf8(result.message).unwrap();

        assert!(text.contains("In-Reply-To: <orig@example.com>"));
        assert!(text.contains("References: <orig@example.com>"));
        assert!(text.contains("multipart/mixed"));
        assert!(text.contains("filename=\"data.csv\""));
    }

    #[test]
    fn attachment_filename_with_quotes_escaped() {
        let tmp = tempfile::TempDir::new().unwrap();
        let file_path = tmp.path().join("file\"name.txt");
        std::fs::write(&file_path, "content").unwrap();

        let args = SendArgs {
            from: "a@b.com".to_string(),
            to: "c@d.com".to_string(),
            subject: "test".to_string(),
            body: "body".to_string(),
            reply_to: None,
            references: None,
            attachments: vec![file_path.to_string_lossy().to_string()],
        };

        let result = compose_message(&args).unwrap();
        let text = String::from_utf8(result.message).unwrap();

        assert!(text.contains("filename=\"file\\\"name.txt\""));
        assert!(!text.contains("filename=\"file\"name.txt\""));
    }

    #[test]
    fn attachment_filename_with_newline_escaped() {
        let escaped = super::escape_filename("file\nname.txt");
        assert!(!escaped.contains('\n'));
        assert_eq!(escaped, "file name.txt");
    }

    #[test]
    fn attachment_filename_with_cr_escaped() {
        let escaped = super::escape_filename("file\rname.txt");
        assert!(!escaped.contains('\r'));
    }

    #[test]
    fn subject_crlf_injection_returns_error() {
        let args = SendArgs {
            from: "a@b.com".to_string(),
            to: "c@d.com".to_string(),
            subject: "Hello\r\nBcc: victim@evil.com\r\n\r\nInjected body".to_string(),
            body: "body".to_string(),
            reply_to: None,
            references: None,
            attachments: vec![],
        };

        let result = compose_message(&args);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("Subject") && err.contains("CRLF"),
            "Error should mention Subject and CRLF: {err}"
        );
    }

    #[test]
    fn from_crlf_injection_returns_error() {
        let args = SendArgs {
            from: "a@b.com\r\nBcc: victim@evil.com".to_string(),
            to: "c@d.com".to_string(),
            subject: "Test".to_string(),
            body: "body".to_string(),
            reply_to: None,
            references: None,
            attachments: vec![],
        };

        let result = compose_message(&args);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("From") && err.contains("CRLF"),
            "Error should mention From and CRLF: {err}"
        );
    }

    #[test]
    fn to_crlf_injection_returns_error() {
        let args = SendArgs {
            from: "a@b.com".to_string(),
            to: "c@d.com\r\nBcc: victim@evil.com".to_string(),
            subject: "Test".to_string(),
            body: "body".to_string(),
            reply_to: None,
            references: None,
            attachments: vec![],
        };

        let result = compose_message(&args);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("To") && err.contains("CRLF"),
            "Error should mention To and CRLF: {err}"
        );
    }

    #[test]
    fn bare_newline_injection_returns_error() {
        let args = SendArgs {
            from: "a@b.com".to_string(),
            to: "c@d.com".to_string(),
            subject: "Hello\nBcc: victim@evil.com".to_string(),
            body: "body".to_string(),
            reply_to: None,
            references: None,
            attachments: vec![],
        };

        let result = compose_message(&args);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("Subject") && err.contains("CRLF"),
            "Error should mention Subject and CRLF: {err}"
        );
    }

    #[test]
    fn reply_to_crlf_injection_sanitized() {
        let mut args = test_args();
        args.reply_to = Some("<orig@example.com>\r\nBcc: victim@evil.com".to_string());

        let result = compose_message(&args).unwrap();
        let text = String::from_utf8(result.message).unwrap();
        for line in text.split("\r\n") {
            assert!(
                !line.starts_with("Bcc:"),
                "CRLF injection in In-Reply-To created a Bcc header"
            );
        }
        assert!(text.contains("In-Reply-To:"));
        let in_reply_line = text
            .split("\r\n")
            .find(|l| l.starts_with("In-Reply-To:"))
            .unwrap();
        assert!(!in_reply_line.contains('\n'));
        assert!(!in_reply_line.contains('\r'));
    }

    #[test]
    fn references_crlf_injection_sanitized() {
        let mut args = test_args();
        args.reply_to = Some("<orig@example.com>".to_string());
        args.references = Some("<first@example.com>\r\nBcc: victim@evil.com".to_string());

        let result = compose_message(&args).unwrap();
        let text = String::from_utf8(result.message).unwrap();
        for line in text.split("\r\n") {
            assert!(
                !line.starts_with("Bcc:"),
                "CRLF injection in References created a Bcc header"
            );
        }
    }

    #[test]
    fn normal_headers_pass_unchanged() {
        let args = SendArgs {
            from: "sender@example.com".to_string(),
            to: "recipient@example.com".to_string(),
            subject: "Normal Subject Line".to_string(),
            body: "body".to_string(),
            reply_to: None,
            references: None,
            attachments: vec![],
        };

        let result = compose_message(&args).unwrap();
        let text = String::from_utf8(result.message).unwrap();
        assert!(text.contains("From: sender@example.com\r\n"));
        assert!(text.contains("To: recipient@example.com\r\n"));
        assert!(text.contains("Subject: Normal Subject Line\r\n"));
    }

    #[test]
    fn normalize_crlf_bare_lf() {
        let result = super::normalize_crlf("Hello\nWorld\n");
        assert_eq!(result, "Hello\r\nWorld\r\n");
    }

    #[test]
    fn normalize_crlf_bare_cr() {
        let result = super::normalize_crlf("Hello\rWorld\r");
        assert_eq!(result, "Hello\r\nWorld\r\n");
    }

    #[test]
    fn normalize_crlf_already_normalized() {
        let result = super::normalize_crlf("Hello\r\nWorld\r\n");
        assert_eq!(result, "Hello\r\nWorld\r\n");
    }

    #[test]
    fn normalize_crlf_mixed() {
        let result = super::normalize_crlf("Line1\nLine2\r\nLine3\rLine4");
        assert_eq!(result, "Line1\r\nLine2\r\nLine3\r\nLine4");
    }

    #[test]
    fn normalize_crlf_no_newlines() {
        let result = super::normalize_crlf("Hello world");
        assert_eq!(result, "Hello world");
    }

    #[test]
    fn normalize_crlf_multibyte_utf8() {
        let result = super::normalize_crlf("Héllo\nwörld\r\némail — sent\rfin");
        assert_eq!(result, "Héllo\r\nwörld\r\némail — sent\r\nfin");
    }

    #[test]
    fn compose_normalizes_bare_lf_in_body() {
        let args = SendArgs {
            from: "agent@example.com".to_string(),
            to: "user@gmail.com".to_string(),
            subject: "Test".to_string(),
            body: "Line1\nLine2\nLine3".to_string(),
            reply_to: None,
            references: None,
            attachments: vec![],
        };

        let result = compose_message(&args).unwrap();
        let text = String::from_utf8(result.message).unwrap();

        assert!(
            !text.contains("Line1\nLine2"),
            "Body should not contain bare LF"
        );
        assert!(
            text.contains("Line1\r\nLine2\r\nLine3"),
            "Body should have CRLF line endings"
        );
    }

    #[test]
    fn compose_message_all_crlf() {
        let args = test_args();
        let result = compose_message(&args).unwrap();
        let raw = result.message;

        for (i, byte) in raw.iter().enumerate() {
            if *byte == b'\n' && (i == 0 || raw[i - 1] != b'\r') {
                let context_start = i.saturating_sub(20);
                let context = String::from_utf8_lossy(&raw[context_start..=i]);
                panic!("Found bare LF at byte offset {i}: ...{context}");
            }
        }
    }

    // ------------------------------------------------------------------
    // Client wire-layer tests. These use a tempdir-scoped UDS plus an
    // in-memory handler so we never hit the real `/run/aimx/`. Mailbox
    // resolution lives in `send_handler::tests` where the daemon-side
    // resolver is exercised directly.
    // ------------------------------------------------------------------

    #[test]
    fn parse_response_ok() {
        let buf = b"AIMX/1 OK <abc@example.com>\n";
        match parse_response_line(buf) {
            SubmitOutcome::Ok { message_id } => {
                assert_eq!(message_id, "<abc@example.com>");
            }
            _ => panic!("expected Ok"),
        }
    }

    #[test]
    fn parse_response_err_domain() {
        let buf = b"AIMX/1 ERR DOMAIN sender domain does not match\n";
        match parse_response_line(buf) {
            SubmitOutcome::Err { code, reason } => {
                assert_eq!(code, ErrCode::Domain);
                assert_eq!(reason, "sender domain does not match");
            }
            _ => panic!("expected Err"),
        }
    }

    #[test]
    fn parse_response_err_all_codes() {
        for (label, expected) in [
            ("MAILBOX", ErrCode::Mailbox),
            ("DOMAIN", ErrCode::Domain),
            ("SIGN", ErrCode::Sign),
            ("DELIVERY", ErrCode::Delivery),
            ("TEMP", ErrCode::Temp),
            ("MALFORMED", ErrCode::Malformed),
        ] {
            let buf = format!("AIMX/1 ERR {label} reason here\n");
            match parse_response_line(buf.as_bytes()) {
                SubmitOutcome::Err { code, .. } => assert_eq!(code, expected),
                _ => panic!("expected Err for {label}"),
            }
        }
    }

    #[test]
    fn parse_response_unknown_prefix() {
        let buf = b"HTTP/1.1 200 OK\n";
        assert!(matches!(
            parse_response_line(buf),
            SubmitOutcome::Malformed(_)
        ));
    }

    #[test]
    fn parse_response_unknown_err_code() {
        // Neither the inline token nor a Code: header resolves — the
        // client surfaces Malformed so downstream tooling can flag it.
        let buf = b"AIMX/1 ERR SPLORG something\n";
        assert!(matches!(
            parse_response_line(buf),
            SubmitOutcome::Malformed(_)
        ));
    }

    #[test]
    fn parse_response_empty() {
        assert!(matches!(
            parse_response_line(b""),
            SubmitOutcome::Malformed(_)
        ));
    }

    #[test]
    fn parse_response_eacces_via_inline_and_header() {
        // A response carrying both the legacy inline
        // `EACCES` token AND a structured `Code:` header is parseable
        // back into the same `ErrCode::Eaccess` variant.
        let buf = b"AIMX/1 ERR EACCES not owner\nCode: EACCES\n\n";
        match parse_response_line(buf) {
            SubmitOutcome::Err { code, reason } => {
                assert_eq!(code, ErrCode::Eaccess);
                assert_eq!(reason, "not owner");
            }
            other => panic!("expected Err(EACCES), got {other:?}"),
        }
    }

    #[test]
    fn parse_response_enoent_header_takes_precedence() {
        // If the Code: header disagrees with the inline token, the
        // structured header wins. This future-proofs against the
        // status line drifting past the enum vocabulary.
        let buf = b"AIMX/1 ERR UNKNOWN not found\nCode: ENOENT\n\n";
        match parse_response_line(buf) {
            SubmitOutcome::Err { code, reason } => {
                assert_eq!(code, ErrCode::Enoent);
                assert_eq!(reason, "not found");
            }
            other => panic!("expected Err(ENOENT), got {other:?}"),
        }
    }

    #[test]
    fn render_outcome_ok_returns_exit_0() {
        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = render_outcome(
            SubmitOutcome::Ok {
                message_id: "<x@example.com>".to_string(),
            },
            &mut out,
            &mut err,
        );
        assert_eq!(code, EXIT_OK);
        let err_text = String::from_utf8_lossy(&err);
        assert!(err_text.contains("Email sent."));
        assert!(err_text.contains("<x@example.com>"));
    }

    #[test]
    fn render_outcome_err_returns_exit_1_with_code_prefix() {
        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = render_outcome(
            SubmitOutcome::Err {
                code: ErrCode::Domain,
                reason: "sender domain does not match aimx domain".to_string(),
            },
            &mut out,
            &mut err,
        );
        assert_eq!(code, EXIT_DAEMON_ERR);
        let err_text = String::from_utf8_lossy(&err);
        assert!(err_text.contains("[DOMAIN]"));
        assert!(err_text.contains("sender domain does not match"));
    }

    #[test]
    fn render_outcome_malformed_returns_exit_3() {
        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = render_outcome(
            SubmitOutcome::Malformed("garbage".to_string()),
            &mut out,
            &mut err,
        );
        assert_eq!(code, EXIT_MALFORMED);
        let err_text = String::from_utf8_lossy(&err);
        assert!(err_text.contains("malformed response"));
    }

    #[test]
    fn render_root_refusal_returns_exit_2_with_message() {
        let mut err = Vec::new();
        let code = render_root_refusal(&mut err);
        assert_eq!(code, EXIT_CONNECT);
        let err_text = String::from_utf8_lossy(&err);
        assert!(err_text.contains("send is a per-user operation"));
        assert!(err_text.contains("run without sudo"));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn submit_request_end_to_end_via_fake_listener() {
        let tmp = tempfile::TempDir::new().unwrap();
        let sock = tmp.path().join("test.sock");

        let listener = tokio::net::UnixListener::bind(&sock).unwrap();
        let captured: Arc<Mutex<Option<SendRequest>>> = Arc::new(Mutex::new(None));
        let captured_c = Arc::clone(&captured);
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (mut reader, mut writer) = stream.into_split();
            let req = send_protocol::parse_send_request(&mut reader)
                .await
                .unwrap();
            *captured_c.lock().unwrap() = Some(req);
            send_protocol::write_response(
                &mut writer,
                &SendResponse::Ok {
                    message_id: "<srv@example.com>".to_string(),
                },
            )
            .await
            .unwrap();
            use tokio::io::AsyncWriteExt;
            writer.shutdown().await.ok();
        });

        let request = SendRequest {
            body: b"From: alice@example.com\r\n\r\nhi\r\n".to_vec(),
        };
        let outcome = submit_request(&sock, &request).await.unwrap();
        server.await.unwrap();

        match outcome {
            SubmitOutcome::Ok { message_id } => assert_eq!(message_id, "<srv@example.com>"),
            _ => panic!("expected Ok"),
        }
        let seen = captured.lock().unwrap().clone().unwrap();
        assert_eq!(seen.body, b"From: alice@example.com\r\n\r\nhi\r\n".to_vec());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn submit_request_missing_socket_returns_not_found() {
        let tmp = tempfile::TempDir::new().unwrap();
        let missing = tmp.path().join("does-not-exist.sock");
        let request = SendRequest {
            body: b"From: alice@example.com\r\n\r\nhi\r\n".to_vec(),
        };
        let result = submit_request(&missing, &request).await;
        let err = result.unwrap_err();
        assert!(
            err.kind() == io::ErrorKind::NotFound || err.kind() == io::ErrorKind::ConnectionRefused,
            "expected NotFound or ConnectionRefused, got {:?}",
            err.kind()
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn submit_request_daemon_err_domain_mapped_to_exit_1() {
        let tmp = tempfile::TempDir::new().unwrap();
        let sock = tmp.path().join("test.sock");

        let listener = tokio::net::UnixListener::bind(&sock).unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (mut reader, mut writer) = stream.into_split();
            let _ = send_protocol::parse_send_request(&mut reader)
                .await
                .unwrap();
            send_protocol::write_response(
                &mut writer,
                &SendResponse::Err {
                    code: ErrCode::Domain,
                    reason: "sender domain does not match aimx domain".to_string(),
                },
            )
            .await
            .unwrap();
            use tokio::io::AsyncWriteExt;
            writer.shutdown().await.ok();
        });

        let request = SendRequest {
            body: b"From: alice@other.org\r\n\r\nhi\r\n".to_vec(),
        };
        let outcome = submit_request(&sock, &request).await.unwrap();
        server.await.unwrap();

        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = render_outcome(outcome, &mut out, &mut err);
        assert_eq!(code, EXIT_DAEMON_ERR);
        let err_text = String::from_utf8_lossy(&err);
        assert!(err_text.contains("[DOMAIN]"));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn submit_request_malformed_response_mapped_to_exit_3() {
        let tmp = tempfile::TempDir::new().unwrap();
        let sock = tmp.path().join("test.sock");

        let listener = tokio::net::UnixListener::bind(&sock).unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (mut reader, mut writer) = stream.into_split();
            let _ = send_protocol::parse_send_request(&mut reader)
                .await
                .unwrap();
            use tokio::io::AsyncWriteExt;
            writer.write_all(b"garbage not a frame\n").await.unwrap();
            writer.shutdown().await.ok();
        });

        let request = SendRequest {
            body: b"From: alice@example.com\r\n\r\nhi\r\n".to_vec(),
        };
        let outcome = submit_request(&sock, &request).await.unwrap();
        server.await.unwrap();

        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = render_outcome(outcome, &mut out, &mut err);
        assert_eq!(code, EXIT_MALFORMED);
    }
}
