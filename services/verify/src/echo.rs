use mail_parser::MessageParser;
use std::io::Read;

#[derive(Debug)]
pub struct VerifyResult {
    pub from: String,
    pub message_id: String,
    pub subject: String,
    pub dkim_status: String,
    pub spf_status: String,
}

pub fn parse_incoming(raw: &[u8]) -> Option<VerifyResult> {
    let parser = MessageParser::default();
    let message = parser.parse(raw)?;

    let from = message
        .from()
        .and_then(|addrs| addrs.first())
        .and_then(|addr| addr.address())
        .unwrap_or("")
        .to_string();

    let message_id = message.message_id().unwrap_or("").to_string();

    let subject = message.subject().unwrap_or("").to_string();

    let dkim_status = extract_auth_result(raw, "dkim");
    let spf_status = extract_auth_result(raw, "spf");

    Some(VerifyResult {
        from,
        message_id,
        subject,
        dkim_status,
        spf_status,
    })
}

fn extract_auth_result(raw: &[u8], method: &str) -> String {
    let text = String::from_utf8_lossy(raw);
    for line in text.lines() {
        let lower = line.to_lowercase();
        if lower.contains("authentication-results") && lower.contains(method) {
            if lower.contains(&format!("{method}=pass")) {
                return "pass".to_string();
            } else if lower.contains(&format!("{method}=fail")) {
                return "fail".to_string();
            }
        }
    }
    "none".to_string()
}

pub fn compose_reply(result: &VerifyResult) -> String {
    let reply_to = if result.message_id.is_empty() {
        String::new()
    } else if result.message_id.starts_with('<') {
        format!("In-Reply-To: {}\r\n", result.message_id)
    } else {
        format!("In-Reply-To: <{}>\r\n", result.message_id)
    };

    let subject = if result.subject.starts_with("Re:") {
        result.subject.clone()
    } else {
        format!("Re: {}", result.subject)
    };

    format!(
        "From: verify@aimx.email\r\n\
         To: {to}\r\n\
         Subject: {subject}\r\n\
         {reply_to}\
         Content-Type: text/plain; charset=utf-8\r\n\
         \r\n\
         aimx verification result\r\n\
         ========================\r\n\
         \r\n\
         DKIM: {dkim}\r\n\
         SPF:  {spf}\r\n\
         \r\n\
         Your email was received and processed by the aimx verify service.\r\n\
         \r\n\
         {status_message}\r\n",
        to = result.from,
        subject = subject,
        reply_to = reply_to,
        dkim = result.dkim_status,
        spf = result.spf_status,
        status_message = if result.dkim_status == "pass" {
            "DKIM signature verified successfully. Your outbound email is correctly signed."
        } else {
            "DKIM verification did not pass. Check your DKIM DNS record and signing configuration."
        },
    )
}

pub fn run_echo() -> Result<(), Box<dyn std::error::Error>> {
    let mut raw = Vec::new();
    std::io::stdin().read_to_end(&mut raw)?;

    let result = parse_incoming(&raw).ok_or("Failed to parse incoming email")?;

    if result.from.is_empty() {
        return Err("No sender address found in email".into());
    }

    let reply = compose_reply(&result);

    let mut child = std::process::Command::new("sendmail")
        .arg("-t")
        .stdin(std::process::Stdio::piped())
        .spawn()?;

    if let Some(mut stdin) = child.stdin.take() {
        use std::io::Write;
        stdin.write_all(reply.as_bytes())?;
    }

    let status = child.wait()?;
    if !status.success() {
        return Err("sendmail failed to send reply".into());
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_email() -> Vec<u8> {
        b"From: user@example.com\r\n\
          To: verify@aimx.email\r\n\
          Subject: Test verification\r\n\
          Message-ID: <test123@example.com>\r\n\
          Authentication-Results: mx.aimx.email; dkim=pass; spf=pass\r\n\
          \r\n\
          Testing aimx setup.\r\n"
            .to_vec()
    }

    fn sample_email_no_auth() -> Vec<u8> {
        b"From: user@example.com\r\n\
          To: verify@aimx.email\r\n\
          Subject: Test\r\n\
          Message-ID: <test456@example.com>\r\n\
          \r\n\
          No auth headers.\r\n"
            .to_vec()
    }

    #[test]
    fn parse_incoming_valid() {
        let result = parse_incoming(&sample_email()).unwrap();
        assert_eq!(result.from, "user@example.com");
        assert_eq!(result.message_id, "test123@example.com");
        assert_eq!(result.subject, "Test verification");
        assert_eq!(result.dkim_status, "pass");
        assert_eq!(result.spf_status, "pass");
    }

    #[test]
    fn parse_incoming_no_auth_headers() {
        let result = parse_incoming(&sample_email_no_auth()).unwrap();
        assert_eq!(result.dkim_status, "none");
        assert_eq!(result.spf_status, "none");
    }

    #[test]
    fn compose_reply_includes_verification_results() {
        let result = VerifyResult {
            from: "user@example.com".to_string(),
            message_id: "<test@example.com>".to_string(),
            subject: "Test".to_string(),
            dkim_status: "pass".to_string(),
            spf_status: "pass".to_string(),
        };
        let reply = compose_reply(&result);
        assert!(reply.contains("To: user@example.com"));
        assert!(reply.contains("From: verify@aimx.email"));
        assert!(reply.contains("DKIM: pass"));
        assert!(reply.contains("SPF:  pass"));
        assert!(reply.contains("In-Reply-To: <test@example.com>"));
        assert!(reply.contains("Re: Test"));
        assert!(reply.contains("correctly signed"));
    }

    #[test]
    fn compose_reply_dkim_fail_message() {
        let result = VerifyResult {
            from: "user@example.com".to_string(),
            message_id: String::new(),
            subject: "Test".to_string(),
            dkim_status: "fail".to_string(),
            spf_status: "none".to_string(),
        };
        let reply = compose_reply(&result);
        assert!(reply.contains("DKIM: fail"));
        assert!(reply.contains("did not pass"));
        assert!(!reply.contains("In-Reply-To"));
    }

    #[test]
    fn compose_reply_preserves_re_prefix() {
        let result = VerifyResult {
            from: "user@example.com".to_string(),
            message_id: String::new(),
            subject: "Re: Already a reply".to_string(),
            dkim_status: "pass".to_string(),
            spf_status: "pass".to_string(),
        };
        let reply = compose_reply(&result);
        assert!(reply.contains("Subject: Re: Already a reply"));
    }

    #[test]
    fn extract_auth_dkim_pass() {
        let raw = b"Authentication-Results: mx.test.com; dkim=pass header.d=example.com";
        assert_eq!(extract_auth_result(raw, "dkim"), "pass");
    }

    #[test]
    fn extract_auth_dkim_fail() {
        let raw = b"Authentication-Results: mx.test.com; dkim=fail reason=bad";
        assert_eq!(extract_auth_result(raw, "dkim"), "fail");
    }

    #[test]
    fn extract_auth_missing() {
        let raw = b"Subject: Hello\r\nFrom: test@example.com\r\n";
        assert_eq!(extract_auth_result(raw, "dkim"), "none");
    }

    #[test]
    fn parse_empty_returns_none() {
        assert!(parse_incoming(b"").is_none());
    }

    #[test]
    fn concurrent_parse_safety() {
        let emails: Vec<Vec<u8>> = (0..10)
            .map(|i| {
                format!(
                    "From: user{}@example.com\r\n\
                     To: verify@aimx.email\r\n\
                     Subject: Test {}\r\n\
                     Message-ID: <msg{}@example.com>\r\n\
                     \r\n\
                     Body {}\r\n",
                    i, i, i, i
                )
                .into_bytes()
            })
            .collect();

        let results: Vec<_> = emails.iter().map(|e| parse_incoming(e).unwrap()).collect();

        for (i, r) in results.iter().enumerate() {
            assert_eq!(r.from, format!("user{i}@example.com"));
        }
    }
}
