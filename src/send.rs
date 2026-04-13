use crate::cli::SendArgs;
use crate::config::Config;
use crate::dkim;
use base64::Engine;
use chrono::Utc;
use std::path::Path;
use uuid::Uuid;

pub trait MailTransport {
    fn send(&self, recipient: &str, message: &[u8]) -> Result<String, Box<dyn std::error::Error>>;
}

pub struct LettreTransport;

impl LettreTransport {
    fn extract_domain(recipient: &str) -> Result<String, Box<dyn std::error::Error>> {
        let addr = recipient
            .rsplit('<')
            .next()
            .unwrap_or(recipient)
            .trim_end_matches('>');
        addr.split('@')
            .nth(1)
            .map(|d| d.to_string())
            .ok_or_else(|| format!("Invalid recipient address: {recipient}").into())
    }
}

impl MailTransport for LettreTransport {
    fn send(&self, recipient: &str, message: &[u8]) -> Result<String, Box<dyn std::error::Error>> {
        let domain = Self::extract_domain(recipient)?;
        let rt = tokio::runtime::Handle::try_current();

        let mx_hosts = match rt {
            Ok(handle) => {
                // We're inside an async context, use block_in_place
                tokio::task::block_in_place(|| handle.block_on(crate::mx::resolve_mx(&domain)))?
            }
            Err(_) => {
                let rt = tokio::runtime::Runtime::new()
                    .map_err(|e| format!("Failed to create runtime: {e}"))?;
                rt.block_on(crate::mx::resolve_mx(&domain))?
            }
        };

        let envelope = lettre::address::Envelope::new(
            None,
            vec![
                recipient
                    .parse()
                    .map_err(|e| format!("Invalid recipient address '{recipient}': {e}"))?,
            ],
        )
        .map_err(|e| format!("Failed to create envelope: {e}"))?;

        let mut last_error: Option<String> = None;

        for host in &mx_hosts {
            match self.try_deliver(host, &envelope, message) {
                Ok(server) => return Ok(server),
                Err(e) => {
                    last_error = Some(format!("{host}: {e}"));
                }
            }
        }

        Err(format!(
            "All MX servers for {domain} unreachable: {}",
            last_error.unwrap_or_default()
        )
        .into())
    }
}

impl LettreTransport {
    fn try_deliver(
        &self,
        host: &str,
        envelope: &lettre::address::Envelope,
        message: &[u8],
    ) -> Result<String, Box<dyn std::error::Error>> {
        use lettre::Transport;

        let tls_params = lettre::transport::smtp::client::TlsParameters::builder(host.to_string())
            .dangerous_accept_invalid_certs(true)
            .build_rustls()
            .map_err(|e| format!("TLS configuration error: {e}"))?;

        let transport = lettre::SmtpTransport::builder_dangerous(host)
            .port(25)
            .tls(lettre::transport::smtp::client::Tls::Opportunistic(
                tls_params,
            ))
            .timeout(Some(std::time::Duration::from_secs(60)))
            .build();

        transport
            .send_raw(envelope, message)
            .map_err(|e| -> Box<dyn std::error::Error> {
                let msg = e.to_string();
                if msg.contains("Connection refused") {
                    format!("Connection refused by {host}").into()
                } else if msg.contains("timed out") || msg.contains("Timeout") {
                    format!("Connection timed out to {host}").into()
                } else {
                    format!("Rejected by {host}: {msg}").into()
                }
            })?;

        Ok(host.to_string())
    }
}

#[derive(Debug)]
pub struct ComposeResult {
    pub message: Vec<u8>,
    pub message_id: String,
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
        return Err(format!(
            "Header '{name}' contains CRLF characters — possible header injection"
        )
        .into());
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

    if args.attachments.is_empty() {
        let mut msg = String::new();
        write_common_headers(&mut msg, args, &date, &message_id)?;
        msg.push_str("Content-Type: text/plain; charset=utf-8\r\n");
        msg.push_str("\r\n");
        msg.push_str(&args.body);
        msg.push_str("\r\n");

        return Ok(ComposeResult {
            message: msg.into_bytes(),
            message_id,
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
    msg.push_str(&args.body);
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
        message_id,
    })
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

pub fn send_with_transport(
    args: &SendArgs,
    transport: &dyn MailTransport,
    dkim_key: Option<(&rsa::RsaPrivateKey, &str, &str)>,
) -> Result<(String, String), Box<dyn std::error::Error>> {
    let composed = compose_message(args)?;

    let final_message = if let Some((key, domain, selector)) = dkim_key {
        dkim::sign_message(&composed.message, key, domain, selector)?
    } else {
        composed.message
    };

    let server = transport.send(&args.to, &final_message)?;
    Ok((composed.message_id, server))
}

pub fn run(
    args: SendArgs,
    data_dir: Option<&std::path::Path>,
) -> Result<(), Box<dyn std::error::Error>> {
    let transport = LettreTransport;

    let config = match data_dir {
        Some(dir) => Config::load_from_data_dir(dir)
            .map_err(|e| format!("Failed to load config for DKIM signing: {e}"))?,
        None => Config::load_default()
            .map_err(|e| format!("Failed to load config for DKIM signing: {e}"))?,
    };
    let private_key = dkim::load_private_key(&config.data_dir)
        .map_err(|e| format!("DKIM signing required but private key could not be loaded: {e}"))?;

    let dkim_info = Some((
        &private_key,
        config.domain.as_str(),
        config.dkim_selector.as_str(),
    ));

    let (message_id, server) = send_with_transport(&args, &transport, dkim_info)?;
    eprintln!(
        "Delivered to {server} for {}. Message-ID: {message_id}",
        args.to
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    struct MockTransport {
        sent: Arc<Mutex<Vec<Vec<u8>>>>,
        should_fail: bool,
    }

    impl MailTransport for MockTransport {
        fn send(
            &self,
            _recipient: &str,
            message: &[u8],
        ) -> Result<String, Box<dyn std::error::Error>> {
            if self.should_fail {
                return Err("Mock transport failure".into());
            }
            self.sent.lock().unwrap().push(message.to_vec());
            Ok("mock-mx.example.com".to_string())
        }
    }

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
    fn send_via_mock_transport() {
        let sent = Arc::new(Mutex::new(Vec::new()));
        let transport = MockTransport {
            sent: sent.clone(),
            should_fail: false,
        };

        let args = test_args();
        let (message_id, server) = send_with_transport(&args, &transport, None).unwrap();

        assert!(!message_id.is_empty());
        assert_eq!(server, "mock-mx.example.com");
        let messages = sent.lock().unwrap();
        assert_eq!(messages.len(), 1);
        let text = String::from_utf8(messages[0].clone()).unwrap();
        assert!(text.contains("From: agent@example.com"));
    }

    #[test]
    fn send_failure_propagates() {
        let sent = Arc::new(Mutex::new(Vec::new()));
        let transport = MockTransport {
            sent,
            should_fail: true,
        };

        let args = test_args();
        let result = send_with_transport(&args, &transport, None);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("Mock transport failure")
        );
    }

    #[test]
    fn send_with_transport_returns_delivery_info() {
        let sent = Arc::new(Mutex::new(Vec::new()));
        let transport = MockTransport {
            sent,
            should_fail: false,
        };

        let args = test_args();
        let (message_id, server) = send_with_transport(&args, &transport, None).unwrap();

        assert!(message_id.starts_with('<'));
        assert!(message_id.ends_with('>'));
        assert_eq!(server, "mock-mx.example.com");
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
    fn send_with_dkim_signing() {
        let tmp = tempfile::TempDir::new().unwrap();
        crate::dkim::generate_keypair(tmp.path(), false).unwrap();
        let private_key = crate::dkim::load_private_key(tmp.path()).unwrap();

        let sent = Arc::new(Mutex::new(Vec::new()));
        let transport = MockTransport {
            sent: sent.clone(),
            should_fail: false,
        };

        let args = test_args();
        send_with_transport(
            &args,
            &transport,
            Some((&private_key, "example.com", "dkim")),
        )
        .unwrap();

        let messages = sent.lock().unwrap();
        assert_eq!(messages.len(), 1);
        let text = String::from_utf8(messages[0].clone()).unwrap();
        assert!(text.contains("DKIM-Signature:"));
        assert!(text.contains("From: agent@example.com"));
    }

    #[test]
    fn compose_returns_message_id() {
        let args = test_args();
        let result = compose_message(&args).unwrap();
        assert!(result.message_id.starts_with('<'));
        assert!(result.message_id.ends_with('>'));
        assert!(result.message_id.contains('@'));
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
    fn dkim_selector_config_used() {
        let tmp = tempfile::TempDir::new().unwrap();
        crate::dkim::generate_keypair(tmp.path(), false).unwrap();
        let private_key = crate::dkim::load_private_key(tmp.path()).unwrap();

        let sent = Arc::new(Mutex::new(Vec::new()));
        let transport = MockTransport {
            sent: sent.clone(),
            should_fail: false,
        };

        let custom_selector = "myselector";
        let args = test_args();
        send_with_transport(
            &args,
            &transport,
            Some((&private_key, "example.com", custom_selector)),
        )
        .unwrap();

        let messages = sent.lock().unwrap();
        let text = String::from_utf8(messages[0].clone()).unwrap();
        assert!(text.contains("s=myselector"));
    }

    #[test]
    fn run_with_missing_dkim_key_returns_error() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mut mailboxes = std::collections::HashMap::new();
        mailboxes.insert(
            "catchall".to_string(),
            crate::config::MailboxConfig {
                address: "*@test.com".to_string(),
                on_receive: vec![],
                trust: "none".to_string(),
                trusted_senders: vec![],
            },
        );
        let config = crate::config::Config {
            domain: "test.com".to_string(),
            data_dir: tmp.path().to_path_buf(),
            dkim_selector: "dkim".to_string(),
            mailboxes,
            verify_host: None,
        };
        config
            .save(&crate::config::Config::config_path(tmp.path()))
            .unwrap();

        let args = test_args();
        let result = run(args, Some(tmp.path()));
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("DKIM") || err.contains("private key"),
            "Error should mention DKIM key: {err}"
        );
    }

    #[test]
    fn run_with_missing_config_returns_error() {
        let tmp = tempfile::TempDir::new().unwrap();

        let args = test_args();
        let result = run(args, Some(tmp.path()));
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("config") || err.contains("DKIM"),
            "Error should mention config loading: {err}"
        );
    }

    #[test]
    fn lettre_transport_extract_domain() {
        assert_eq!(
            LettreTransport::extract_domain("user@gmail.com").unwrap(),
            "gmail.com"
        );
        assert_eq!(
            LettreTransport::extract_domain("User <user@gmail.com>").unwrap(),
            "gmail.com"
        );
        assert!(LettreTransport::extract_domain("nodomain").is_err());
    }
}
