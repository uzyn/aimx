//! Daemon-side handler for `AIMX/1 SEND` UDS requests.
//!
//! This module contains the per-connection business logic that runs inside
//! `aimx serve` after a request frame has been decoded: domain validation,
//! DKIM signing, and delivery. Framing is the [`send_protocol`] module's
//! responsibility — this one deals only in parsed `SendRequest`s.
//!
//! The handler is deliberately testable: real MX delivery is abstracted
//! behind the [`MailTransport`](crate::send::MailTransport) trait so tests
//! can inject a mock.

use std::collections::HashSet;
use std::sync::Arc;

use rsa::RsaPrivateKey;
use uuid::Uuid;

use crate::dkim;
use crate::send::MailTransport;
use crate::send_protocol::{ErrCode, SendRequest, SendResponse};

/// Context shared across every per-connection send.
///
/// Heap-allocated once at daemon startup and cloned (cheap — `Arc` clones)
/// into each task. Holding the DKIM key in an `Arc` here is what lets us
/// load it exactly once despite accepting concurrent sends.
pub struct SendContext {
    /// DKIM private key, loaded once at `aimx serve` startup.
    pub dkim_key: Arc<RsaPrivateKey>,
    /// Primary domain from `/etc/aimx/config.toml`. Compared case-
    /// insensitively against the submitted `From:` header.
    pub primary_domain: String,
    /// DKIM selector (`dkim._domainkey.<domain>`).
    pub dkim_selector: String,
    /// Set of mailbox names registered in config. `From-Mailbox` must be
    /// one of these.
    pub registered_mailboxes: HashSet<String>,
    /// Transport used for final MX delivery. In production this is a
    /// `LettreTransport`; tests inject a mock.
    pub transport: Arc<dyn MailTransport + Send + Sync>,
}

/// Execute one submitted send end-to-end and return the wire response.
///
/// The flow: validate `From-Mailbox` is registered → parse the `From:`
/// header out of the body → validate the sender domain matches config →
/// DKIM-sign → deliver via MX. Every error path maps to a stable
/// [`ErrCode`].
pub async fn handle_send(req: SendRequest, ctx: &SendContext) -> SendResponse {
    handle_send_with_signer(req, ctx, dkim::sign_message).await
}

/// Generic form of [`handle_send`] parameterized on the DKIM signer so tests
/// can inject a failing signer without constructing a bad key. Production
/// code always routes through [`handle_send`], which wires [`dkim::sign_message`].
pub(crate) async fn handle_send_with_signer<F>(
    req: SendRequest,
    ctx: &SendContext,
    signer: F,
) -> SendResponse
where
    F: FnOnce(&[u8], &RsaPrivateKey, &str, &str) -> Result<Vec<u8>, Box<dyn std::error::Error>>,
{
    if !ctx.registered_mailboxes.contains(&req.from_mailbox) {
        return SendResponse::Err {
            code: ErrCode::Mailbox,
            reason: format!("mailbox `{}` not registered", req.from_mailbox),
        };
    }

    let headers = scan_headers(&req.body, &["From", "To", "Message-ID"]);

    let from_header = match headers.get("From") {
        Some(v) => v.clone(),
        None => {
            return SendResponse::Err {
                code: ErrCode::Malformed,
                reason: "missing required header: From".to_string(),
            };
        }
    };

    let sender_domain = match extract_domain(&from_header) {
        Some(d) => d,
        None => {
            return SendResponse::Err {
                code: ErrCode::Malformed,
                reason: format!("could not extract domain from From: {from_header}"),
            };
        }
    };

    if !sender_domain.eq_ignore_ascii_case(&ctx.primary_domain) {
        return SendResponse::Err {
            code: ErrCode::Domain,
            reason: format!(
                "sender domain does not match aimx domain (got {sender_domain}, \
                 expected {})",
                ctx.primary_domain
            ),
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
    // failure to extract a bare recipient is MALFORMED — not a delivery
    // error — because nothing has been attempted over the wire.
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
    // erroring out: the sprint's error table never listed Message-ID as a
    // required client header, and `AIMX/1 OK <message-id>` still needs
    // something to echo. Using the configured primary domain matches the
    // DKIM `d=` tag and avoids leaking a recipient-side hostname.
    let (message_id, body_bytes) = match headers.get("Message-ID") {
        Some(v) => (v.clone(), req.body.clone()),
        None => {
            let synthetic = format!("<{}@{}>", Uuid::new_v4(), ctx.primary_domain);
            let injected = inject_message_id_header(&req.body, &synthetic);
            (synthetic, injected)
        }
    };

    let signed = match signer(
        &body_bytes,
        &ctx.dkim_key,
        &ctx.primary_domain,
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

    match ctx.transport.send(&from_header, &recipient_bare, &signed) {
        Ok(_server) => SendResponse::Ok { message_id },
        Err(e) => {
            let msg = e.to_string();
            let code = classify_transport_error(&msg);
            SendResponse::Err { code, reason: msg }
        }
    }
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
/// `@` is present.
fn extract_domain(from: &str) -> Option<String> {
    let addr = extract_bare_address(from)?;
    let at = addr.rfind('@')?;
    Some(addr[at + 1..].trim().to_string())
}

/// Extract the bare `local@host` form from a header value, accepting
/// `"Name" <local@host>`, `local@host`, and angle-only `<local@host>`. For
/// comma-separated header values only the first recipient is returned —
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

/// Map a transport error string to an `ErrCode`.
///
/// Heuristic — the transport layer returns `Box<dyn Error>` strings today,
/// so we pattern-match the message. Connection-level failures (DNS,
/// refused, timeout) are classified as `Temp` because they're usually
/// retriable; explicit MX rejects (5xx SMTP replies) map to `Delivery`.
fn classify_transport_error(msg: &str) -> ErrCode {
    let lower = msg.to_ascii_lowercase();
    if lower.contains("unreachable")
        || lower.contains("timed out")
        || lower.contains("timeout")
        || lower.contains("connection refused")
        || lower.contains("no a record")
        || lower.contains("dns")
    {
        ErrCode::Temp
    } else {
        ErrCode::Delivery
    }
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
        Err(String),
    }

    impl MailTransport for MockTransport {
        fn send(
            &self,
            _sender: &str,
            _recipient: &str,
            message: &[u8],
        ) -> Result<String, Box<dyn std::error::Error>> {
            match &self.behavior {
                Behavior::Ok => {
                    self.captured.lock().unwrap().push(message.to_vec());
                    Ok("mock-mx.example.com".to_string())
                }
                Behavior::Err(s) => Err(s.clone().into()),
            }
        }
    }

    fn test_ctx(transport: Arc<dyn MailTransport + Send + Sync>) -> SendContext {
        use tempfile::TempDir;
        let tmp = TempDir::new().unwrap();
        dkim::generate_keypair(tmp.path(), false).unwrap();
        let key = dkim::load_private_key(tmp.path()).unwrap();
        let mut boxes = HashSet::new();
        boxes.insert("catchall".to_string());
        boxes.insert("alice".to_string());
        SendContext {
            dkim_key: Arc::new(key),
            primary_domain: "example.com".to_string(),
            dkim_selector: "dkim".to_string(),
            registered_mailboxes: boxes,
            transport,
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
            from_mailbox: "alice".to_string(),
            body: body("alice@example.com"),
        };
        let resp = handle_send(req, &ctx).await;
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
    async fn unknown_mailbox_returns_mailbox_error() {
        let mock = Arc::new(MockTransport {
            captured: Mutex::new(vec![]),
            behavior: Behavior::Ok,
        });
        let ctx = test_ctx(mock);
        let req = SendRequest {
            from_mailbox: "ghost".to_string(),
            body: body("alice@example.com"),
        };
        let resp = handle_send(req, &ctx).await;
        match resp {
            SendResponse::Err { code, reason } => {
                assert_eq!(code, ErrCode::Mailbox);
                assert!(reason.contains("ghost"), "{reason}");
            }
            other => panic!("expected Err, got {other:?}"),
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
            from_mailbox: "alice".to_string(),
            body: body("alice@other.org"),
        };
        let resp = handle_send(req, &ctx).await;
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
            from_mailbox: "alice".to_string(),
            body: body("alice@EXAMPLE.COM"),
        };
        let resp = handle_send(req, &ctx).await;
        assert!(matches!(resp, SendResponse::Ok { .. }), "{resp:?}");
    }

    #[tokio::test]
    async fn any_local_part_accepted() {
        let mock = Arc::new(MockTransport {
            captured: Mutex::new(vec![]),
            behavior: Behavior::Ok,
        });
        let ctx = test_ctx(mock);
        let req = SendRequest {
            from_mailbox: "alice".to_string(),
            body: body("anything@example.com"),
        };
        let resp = handle_send(req, &ctx).await;
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
            from_mailbox: "alice".to_string(),
            body,
        };
        let resp = handle_send(req, &ctx).await;
        match resp {
            SendResponse::Err { code, .. } => assert_eq!(code, ErrCode::Malformed),
            other => panic!("expected Err, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn transport_permanent_error_maps_to_delivery() {
        let mock = Arc::new(MockTransport {
            captured: Mutex::new(vec![]),
            behavior: Behavior::Err("Rejected by mx.example.com: 550 no such user".to_string()),
        });
        let ctx = test_ctx(mock);
        let req = SendRequest {
            from_mailbox: "alice".to_string(),
            body: body("alice@example.com"),
        };
        let resp = handle_send(req, &ctx).await;
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
            behavior: Behavior::Err("All MX servers for gmail.com unreachable: ...".to_string()),
        });
        let ctx = test_ctx(mock);
        let req = SendRequest {
            from_mailbox: "alice".to_string(),
            body: body("alice@example.com"),
        };
        let resp = handle_send(req, &ctx).await;
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
        // bare addr before calling the transport — otherwise the lettre
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
            from_mailbox: "alice".to_string(),
            body,
        };
        let resp = handle_send(req, &ctx).await;
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
            from_mailbox: "alice".to_string(),
            body,
        };
        let resp = handle_send(req, &ctx).await;
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
            from_mailbox: "alice".to_string(),
            body: body("alice@example.com"),
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
        let resp = handle_send_with_signer(req, &ctx, failing_signer).await;
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
}
