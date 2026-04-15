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
    if !ctx.registered_mailboxes.contains(&req.from_mailbox) {
        return SendResponse::Err {
            code: ErrCode::Mailbox,
            reason: format!("mailbox `{}` not registered", req.from_mailbox),
        };
    }

    let (message_id, from_header) = match parse_from_and_message_id(&req.body) {
        Ok(v) => v,
        Err(reason) => {
            return SendResponse::Err {
                code: ErrCode::Malformed,
                reason,
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

    let to_header = match header_value(&req.body, "To") {
        Some(v) => v,
        None => {
            return SendResponse::Err {
                code: ErrCode::Malformed,
                reason: "missing required header: To".to_string(),
            };
        }
    };

    let signed = match dkim::sign_message(
        &req.body,
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

    match ctx.transport.send(&from_header, &to_header, &signed) {
        Ok(_server) => SendResponse::Ok { message_id },
        Err(e) => {
            let msg = e.to_string();
            let code = classify_transport_error(&msg);
            SendResponse::Err { code, reason: msg }
        }
    }
}

/// Extract the `From:` header value and the `Message-ID:` header value from
/// an RFC 5322 message. Returns `Err(reason)` if either is missing.
fn parse_from_and_message_id(body: &[u8]) -> Result<(String, String), String> {
    let message_id = header_value(body, "Message-ID")
        .ok_or_else(|| "missing required header: Message-ID".to_string())?;
    let from =
        header_value(body, "From").ok_or_else(|| "missing required header: From".to_string())?;
    Ok((message_id, from))
}

/// Look up a header by case-insensitive name in an RFC 5322 message. Header
/// lines are assumed to be CRLF-separated (SMTP-ready), but this helper
/// tolerates lone LFs so it also works on locally-composed buffers. Stops
/// at the first blank line (headers/body separator). Joins continuation
/// lines (leading WSP).
fn header_value(body: &[u8], name: &str) -> Option<String> {
    let text = std::str::from_utf8(body).ok()?;
    let mut lines = text.lines();
    let mut current: Option<String> = None;

    let commit = |current: &Option<String>| -> Option<(String, String)> {
        let line = current.as_ref()?;
        let (n, v) = line.split_once(':')?;
        Some((n.trim().to_string(), v.trim().to_string()))
    };

    let target = name.to_ascii_lowercase();

    for line in lines.by_ref() {
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
        if let Some((n, v)) = commit(&current)
            && n.eq_ignore_ascii_case(&target)
        {
            return Some(v);
        }
        current = Some(line.to_string());
    }

    if let Some((n, v)) = commit(&current)
        && n.eq_ignore_ascii_case(&target)
    {
        return Some(v);
    }
    None
}

/// Extract the bare-addr domain from an RFC 5322 `From:` header, handling
/// both `"Name <user@host>"` and `"user@host"` forms. Returns `None` if no
/// `@` is present.
fn extract_domain(from: &str) -> Option<String> {
    let addr = if let Some(start) = from.rfind('<') {
        let tail = &from[start + 1..];
        let end = tail.find('>').unwrap_or(tail.len());
        &tail[..end]
    } else {
        from
    };
    let at = addr.rfind('@')?;
    Some(addr[at + 1..].trim().to_string())
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
    fn header_value_simple() {
        let body = b"From: alice@example.com\r\nTo: bob@x.com\r\n\r\nbody";
        assert_eq!(
            header_value(body, "From"),
            Some("alice@example.com".to_string())
        );
    }

    #[test]
    fn header_value_case_insensitive() {
        let body = b"fRoM: alice@example.com\r\n\r\n";
        assert_eq!(
            header_value(body, "FROM"),
            Some("alice@example.com".to_string())
        );
    }

    #[test]
    fn header_value_continuation_line_joined() {
        let body = b"Subject: one\r\n two\r\n\r\n";
        assert_eq!(header_value(body, "Subject"), Some("one two".to_string()));
    }

    #[test]
    fn header_value_missing_returns_none() {
        let body = b"From: a@b.com\r\n\r\n";
        assert!(header_value(body, "X-Nope").is_none());
    }
}
