use crate::cli::SendArgs;
use chrono::Utc;
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

pub fn compose_message(args: &SendArgs) -> Vec<u8> {
    let message_id = format!(
        "<{uuid}@{domain}>",
        uuid = Uuid::new_v4(),
        domain = args.from.split('@').nth(1).unwrap_or("localhost"),
    );
    let date = Utc::now().to_rfc2822();

    let mut msg = String::new();
    msg.push_str(&format!("From: {}\r\n", args.from));
    msg.push_str(&format!("To: {}\r\n", args.to));
    msg.push_str(&format!("Subject: {}\r\n", args.subject));
    msg.push_str(&format!("Date: {}\r\n", date));
    msg.push_str(&format!("Message-ID: {message_id}\r\n"));
    msg.push_str("MIME-Version: 1.0\r\n");
    msg.push_str("Content-Type: text/plain; charset=utf-8\r\n");
    msg.push_str("\r\n");
    msg.push_str(&args.body);
    msg.push_str("\r\n");

    msg.into_bytes()
}

pub fn send_with_transport(
    args: &SendArgs,
    transport: &dyn MailTransport,
) -> Result<(), Box<dyn std::error::Error>> {
    let message = compose_message(args);
    transport.send(&message)
}

pub fn run(args: SendArgs) -> Result<(), Box<dyn std::error::Error>> {
    let transport = SendmailTransport;
    send_with_transport(&args, &transport)?;
    println!("Email sent successfully.");
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
        }
    }

    #[test]
    fn compose_has_required_headers() {
        let args = test_args();
        let message = compose_message(&args);
        let text = String::from_utf8(message).unwrap();

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
        let message = compose_message(&args);
        let text = String::from_utf8(message).unwrap();

        assert!(text.contains("\r\n\r\nHello, world!\r\n"));
    }

    #[test]
    fn message_id_format() {
        let args = test_args();
        let message = compose_message(&args);
        let text = String::from_utf8(message).unwrap();

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
        send_with_transport(&args, &transport).unwrap();

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
        let result = send_with_transport(&args, &transport);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("Mock transport failure")
        );
    }
}
