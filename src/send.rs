use crate::cli::SendArgs;
use crate::config::Config;
use crate::dkim;
use base64::Engine;
use chrono::Utc;
use std::path::Path;
use uuid::Uuid;

pub trait MailTransport {
    fn send(&self, message: &[u8]) -> Result<(), Box<dyn std::error::Error>>;
}

pub struct SendmailTransport;

impl MailTransport for SendmailTransport {
    fn send(&self, message: &[u8]) -> Result<(), Box<dyn std::error::Error>> {
        use std::io::Write;
        use std::process::{Command, Stdio};

        let mut child = Command::new("/usr/sbin/sendmail")
            .arg("-t")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("Failed to launch sendmail: {e}"))?;

        child
            .stdin
            .as_mut()
            .ok_or("Failed to open sendmail stdin")?
            .write_all(message)?;

        let output = child.wait_with_output()?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(format!("sendmail failed: {stderr}").into());
        }

        Ok(())
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

fn write_common_headers(msg: &mut String, args: &SendArgs, date: &str, message_id: &str) {
    msg.push_str(&format!("From: {}\r\n", args.from));
    msg.push_str(&format!("To: {}\r\n", args.to));
    msg.push_str(&format!("Subject: {}\r\n", args.subject));
    msg.push_str(&format!("Date: {date}\r\n"));
    msg.push_str(&format!("Message-ID: {message_id}\r\n"));

    if let Some(ref reply_to) = args.reply_to {
        let reply_id = normalize_message_id(reply_to);
        msg.push_str(&format!("In-Reply-To: {reply_id}\r\n"));
        let refs = match &args.references {
            Some(r) if !r.trim().is_empty() => r.clone(),
            _ => reply_id.clone(),
        };
        msg.push_str(&format!("References: {refs}\r\n"));
    }

    msg.push_str("MIME-Version: 1.0\r\n");
}

pub fn compose_message(args: &SendArgs) -> Result<ComposeResult, Box<dyn std::error::Error>> {
    validate_attachments(&args.attachments)?;

    let domain = args.from.split('@').nth(1).unwrap_or("localhost");
    let message_id = format!("<{}@{domain}>", Uuid::new_v4());
    let date = Utc::now().to_rfc2822();

    if args.attachments.is_empty() {
        let mut msg = String::new();
        write_common_headers(&mut msg, args, &date, &message_id);
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
    write_common_headers(&mut msg, args, &date, &message_id);
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
) -> Result<String, Box<dyn std::error::Error>> {
    let composed = compose_message(args)?;

    let final_message = if let Some((key, domain, selector)) = dkim_key {
        dkim::sign_message(&composed.message, key, domain, selector)?
    } else {
        composed.message
    };

    transport.send(&final_message)?;
    Ok(composed.message_id)
}

pub fn run(
    args: SendArgs,
    data_dir: Option<&std::path::Path>,
) -> Result<(), Box<dyn std::error::Error>> {
    let transport = SendmailTransport;

    let config = match data_dir {
        Some(dir) => Config::load_from_data_dir(dir).ok(),
        None => Config::load_default().ok(),
    };
    let private_key = config
        .as_ref()
        .and_then(|c| dkim::load_private_key(&c.data_dir).ok());

    let dkim_info = match (&config, &private_key) {
        (Some(c), Some(k)) => Some((k, c.domain.as_str(), c.dkim_selector.as_str())),
        _ => {
            eprintln!("Warning: DKIM signing disabled (no key found)");
            None
        }
    };

    let message_id = send_with_transport(&args, &transport, dkim_info)?;
    println!("Email sent successfully. Message-ID: {message_id}");
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
        fn send(&self, message: &[u8]) -> Result<(), Box<dyn std::error::Error>> {
            if self.should_fail {
                return Err("Mock transport failure".into());
            }
            self.sent.lock().unwrap().push(message.to_vec());
            Ok(())
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
        send_with_transport(&args, &transport, None).unwrap();

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
}
