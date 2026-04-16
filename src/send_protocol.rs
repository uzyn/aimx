//! `AIMX/1 SEND` wire protocol codec.
//!
//! Length-prefixed, binary-safe framing used by `aimx send` to submit mail to
//! `aimx serve` over `/run/aimx/send.sock`. The codec is pure: it speaks only
//! `AsyncRead`/`AsyncWrite`, so it can be exercised with in-memory async
//! streams (e.g. `tokio_test::io::Builder`) without touching the filesystem
//! or a real socket.
//!
//! Framing:
//!
//! ```text
//! Client → Server:
//!   AIMX/1 SEND\n
//!   From-Mailbox: <name>\n
//!   Content-Length: <n>\n
//!   \n
//!   <n bytes of RFC 5322 message, unsigned>
//!
//! Server → Client:
//!   AIMX/1 OK <message-id>\n
//! or
//!   AIMX/1 ERR <code> <reason>\n
//! ```
//!
//! The blank separator line is a literal `\n` (or `\r\n`). Header names are
//! case-insensitive. Unknown headers are silently ignored so the protocol
//! can be extended without breaking older clients.

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Maximum body size accepted by [`parse_request`]. Mirrors the SMTP DATA
/// cap (`smtp::DEFAULT_MAX_MESSAGE_SIZE`, 25 MB) so local clients cannot
/// submit mail the MTA would refuse anyway.
pub const DEFAULT_MAX_BODY_SIZE: usize = 25 * 1024 * 1024;

/// Maximum length of the request-line or a single header line, in bytes.
/// Keeps the parser's memory footprint bounded even on pathological input.
const MAX_HEADER_LINE: usize = 8 * 1024;

/// Decoded `AIMX/1 SEND` request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SendRequest {
    pub from_mailbox: String,
    pub body: Vec<u8>,
}

/// Error codes reported on the wire in `AIMX/1 ERR <code> <reason>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrCode {
    Mailbox,
    Domain,
    Sign,
    Delivery,
    Temp,
    Malformed,
}

impl ErrCode {
    pub fn as_str(self) -> &'static str {
        match self {
            ErrCode::Mailbox => "MAILBOX",
            ErrCode::Domain => "DOMAIN",
            ErrCode::Sign => "SIGN",
            ErrCode::Delivery => "DELIVERY",
            ErrCode::Temp => "TEMP",
            ErrCode::Malformed => "MALFORMED",
        }
    }
}

/// Response emitted by the daemon after processing a request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SendResponse {
    Ok { message_id: String },
    Err { code: ErrCode, reason: String },
}

/// Codec error. Kept separate from `std::error::Error` so the daemon's
/// handler can map `ParseError` values into wire-level `ErrCode::Malformed`
/// responses without rebuilding the information.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseError {
    /// Peer closed the connection before any request-line bytes arrived.
    ClosedBeforeRequest,
    /// Malformed framing or missing required headers. Reason is operator-
    /// facing and safe to render into `ERR MALFORMED <reason>`.
    Malformed(String),
    /// An underlying I/O error. Surfaced so the daemon can distinguish
    /// "client sent garbage" from "socket EBADF".
    Io(String),
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ParseError::ClosedBeforeRequest => write!(f, "connection closed before request"),
            ParseError::Malformed(s) => write!(f, "malformed request: {s}"),
            ParseError::Io(s) => write!(f, "i/o error: {s}"),
        }
    }
}

impl std::error::Error for ParseError {}

impl From<std::io::Error> for ParseError {
    fn from(e: std::io::Error) -> Self {
        ParseError::Io(e.to_string())
    }
}

/// Read and decode one `AIMX/1 SEND` request from `reader`.
///
/// Uses the codec-level [`DEFAULT_MAX_BODY_SIZE`] cap. Callers that need a
/// different cap should use [`parse_request_with_limit`].
pub async fn parse_request<R>(reader: &mut R) -> Result<SendRequest, ParseError>
where
    R: AsyncRead + Unpin,
{
    parse_request_with_limit(reader, DEFAULT_MAX_BODY_SIZE).await
}

/// Like [`parse_request`] but with a caller-supplied maximum body size.
pub async fn parse_request_with_limit<R>(
    reader: &mut R,
    max_body: usize,
) -> Result<SendRequest, ParseError>
where
    R: AsyncRead + Unpin,
{
    let request_line = read_line(reader).await?;
    let request_line = match request_line {
        Some(l) => l,
        None => return Err(ParseError::ClosedBeforeRequest),
    };

    if request_line.trim_end_matches('\r') != "AIMX/1 SEND" {
        return Err(ParseError::Malformed(format!(
            "expected request-line 'AIMX/1 SEND', got {request_line:?}"
        )));
    }

    let mut from_mailbox: Option<String> = None;
    let mut content_length: Option<usize> = None;

    loop {
        let line = read_line(reader)
            .await?
            .ok_or_else(|| ParseError::Malformed("unexpected EOF in headers".into()))?;
        let line = line.trim_end_matches('\r');
        if line.is_empty() {
            break;
        }

        let (name, value) = line
            .split_once(':')
            .ok_or_else(|| ParseError::Malformed(format!("invalid header line: {line:?}")))?;

        if !name.is_ascii() {
            return Err(ParseError::Malformed(format!(
                "non-ascii header name: {name:?}"
            )));
        }
        let name_norm = name.trim().to_ascii_lowercase();
        let value = value.trim().to_string();

        match name_norm.as_str() {
            "from-mailbox" => {
                if from_mailbox.is_some() {
                    return Err(ParseError::Malformed(
                        "duplicate From-Mailbox header".into(),
                    ));
                }
                if value.is_empty() {
                    return Err(ParseError::Malformed("empty From-Mailbox value".into()));
                }
                from_mailbox = Some(value);
            }
            "content-length" => {
                if content_length.is_some() {
                    return Err(ParseError::Malformed(
                        "duplicate Content-Length header".into(),
                    ));
                }
                let n: usize = value.parse().map_err(|_| {
                    ParseError::Malformed(format!("non-integer Content-Length: {value:?}"))
                })?;
                if n > max_body {
                    return Err(ParseError::Malformed(format!(
                        "Content-Length {n} exceeds cap {max_body}"
                    )));
                }
                content_length = Some(n);
            }
            _ => {
                // Unknown headers are ignored for forward-compatibility.
            }
        }
    }

    let from_mailbox = from_mailbox
        .ok_or_else(|| ParseError::Malformed("missing required header: From-Mailbox".into()))?;
    let content_length = content_length
        .ok_or_else(|| ParseError::Malformed("missing required header: Content-Length".into()))?;

    let mut body = vec![0u8; content_length];
    reader.read_exact(&mut body).await.map_err(|e| {
        if e.kind() == std::io::ErrorKind::UnexpectedEof {
            ParseError::Malformed(format!("body truncated: expected {content_length} bytes"))
        } else {
            ParseError::Io(e.to_string())
        }
    })?;

    Ok(SendRequest { from_mailbox, body })
}

/// Read a single `\n`-terminated line from `reader`, returning it without the
/// trailing `\n`. Returns `Ok(None)` when the stream ends cleanly before any
/// byte arrives. Enforces [`MAX_HEADER_LINE`] to bound memory on garbage
/// input.
async fn read_line<R>(reader: &mut R) -> Result<Option<String>, ParseError>
where
    R: AsyncRead + Unpin,
{
    let mut buf = Vec::with_capacity(64);
    let mut byte = [0u8; 1];

    loop {
        match reader.read(&mut byte).await {
            Ok(0) => {
                if buf.is_empty() {
                    return Ok(None);
                }
                return Err(ParseError::Malformed(
                    "unexpected EOF mid-line (no terminator)".into(),
                ));
            }
            Ok(_) => {
                if byte[0] == b'\n' {
                    break;
                }
                buf.push(byte[0]);
                if buf.len() > MAX_HEADER_LINE {
                    return Err(ParseError::Malformed(format!(
                        "header line exceeds {MAX_HEADER_LINE} bytes"
                    )));
                }
            }
            Err(e) => return Err(ParseError::Io(e.to_string())),
        }
    }

    let s = String::from_utf8(buf)
        .map_err(|_| ParseError::Malformed("header line contains invalid UTF-8".into()))?;
    Ok(Some(s))
}

/// Write a `SendResponse` frame to `writer` and flush. The frame ends with a
/// single `\n` — no body, no content-length.
pub async fn write_response<W>(
    writer: &mut W,
    response: &SendResponse,
) -> Result<(), std::io::Error>
where
    W: AsyncWrite + Unpin,
{
    let line = match response {
        SendResponse::Ok { message_id } => {
            let id = sanitize_inline(message_id);
            format!("AIMX/1 OK {id}\n")
        }
        SendResponse::Err { code, reason } => {
            let r = sanitize_inline(reason);
            format!("AIMX/1 ERR {} {r}\n", code.as_str())
        }
    };
    writer.write_all(line.as_bytes()).await?;
    writer.flush().await?;
    Ok(())
}

/// Write an `AIMX/1 SEND` request frame to `writer`. Used by the client
/// (`aimx send`) and by tests that exercise `parse_request` over a paired
/// AsyncRead/AsyncWrite harness.
pub async fn write_request<W>(writer: &mut W, request: &SendRequest) -> Result<(), std::io::Error>
where
    W: AsyncWrite + Unpin,
{
    let header = format!(
        "AIMX/1 SEND\nFrom-Mailbox: {}\nContent-Length: {}\n\n",
        sanitize_inline(&request.from_mailbox),
        request.body.len(),
    );
    writer.write_all(header.as_bytes()).await?;
    writer.write_all(&request.body).await?;
    writer.flush().await?;
    Ok(())
}

/// Strip CR/LF from a single-line wire field. Callers must never emit bare
/// LF inside a reason or message-ID or the framer ambiguates the next line.
fn sanitize_inline(s: &str) -> String {
    s.chars().filter(|c| *c != '\n' && *c != '\r').collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::duplex;

    async fn parse_from_bytes(input: &[u8]) -> Result<SendRequest, ParseError> {
        let (mut client, mut server) = duplex(input.len().max(64));
        client.write_all(input).await.unwrap();
        drop(client);
        parse_request(&mut server).await
    }

    #[tokio::test]
    async fn parses_minimal_request() {
        let input = b"AIMX/1 SEND\nFrom-Mailbox: alice\nContent-Length: 5\n\nhello";
        let req = parse_from_bytes(input).await.unwrap();
        assert_eq!(req.from_mailbox, "alice");
        assert_eq!(req.body, b"hello".to_vec());
    }

    #[tokio::test]
    async fn parses_request_with_crlf_line_endings() {
        let input = b"AIMX/1 SEND\r\nFrom-Mailbox: alice\r\nContent-Length: 5\r\n\r\nhello";
        let req = parse_from_bytes(input).await.unwrap();
        assert_eq!(req.from_mailbox, "alice");
        assert_eq!(req.body, b"hello".to_vec());
    }

    #[tokio::test]
    async fn parses_empty_body() {
        let input = b"AIMX/1 SEND\nFrom-Mailbox: alice\nContent-Length: 0\n\n";
        let req = parse_from_bytes(input).await.unwrap();
        assert_eq!(req.from_mailbox, "alice");
        assert!(req.body.is_empty());
    }

    #[tokio::test]
    async fn header_names_are_case_insensitive() {
        let input = b"AIMX/1 SEND\nFROM-MAILBOX: alice\ncontent-length: 3\n\nabc";
        let req = parse_from_bytes(input).await.unwrap();
        assert_eq!(req.from_mailbox, "alice");
        assert_eq!(req.body, b"abc".to_vec());
    }

    #[tokio::test]
    async fn unknown_headers_are_ignored() {
        let input = b"AIMX/1 SEND\nFrom-Mailbox: alice\nX-Future: foo\nContent-Length: 3\n\nabc";
        let req = parse_from_bytes(input).await.unwrap();
        assert_eq!(req.from_mailbox, "alice");
        assert_eq!(req.body, b"abc".to_vec());
    }

    #[tokio::test]
    async fn wrong_request_line_is_malformed() {
        let input = b"GET / HTTP/1.1\nHost: foo\n\n";
        let err = parse_from_bytes(input).await.unwrap_err();
        assert!(matches!(err, ParseError::Malformed(_)), "got {err:?}");
    }

    #[tokio::test]
    async fn missing_from_mailbox_is_malformed() {
        let input = b"AIMX/1 SEND\nContent-Length: 0\n\n";
        let err = parse_from_bytes(input).await.unwrap_err();
        match err {
            ParseError::Malformed(m) => assert!(m.contains("From-Mailbox"), "{m}"),
            other => panic!("expected Malformed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn missing_content_length_is_malformed() {
        let input = b"AIMX/1 SEND\nFrom-Mailbox: alice\n\n";
        let err = parse_from_bytes(input).await.unwrap_err();
        match err {
            ParseError::Malformed(m) => assert!(m.contains("Content-Length"), "{m}"),
            other => panic!("expected Malformed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn non_integer_content_length_is_malformed() {
        let input = b"AIMX/1 SEND\nFrom-Mailbox: alice\nContent-Length: abc\n\n";
        let err = parse_from_bytes(input).await.unwrap_err();
        match err {
            ParseError::Malformed(m) => assert!(m.contains("non-integer"), "{m}"),
            other => panic!("expected Malformed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn oversized_content_length_is_malformed() {
        let input = b"AIMX/1 SEND\nFrom-Mailbox: alice\nContent-Length: 999999999999\n\n";
        let err = parse_from_bytes(input).await.unwrap_err();
        match err {
            ParseError::Malformed(m) => assert!(m.contains("exceeds cap"), "{m}"),
            other => panic!("expected Malformed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn truncated_body_is_malformed() {
        let input = b"AIMX/1 SEND\nFrom-Mailbox: alice\nContent-Length: 10\n\nabc";
        let err = parse_from_bytes(input).await.unwrap_err();
        match err {
            ParseError::Malformed(m) => assert!(m.contains("truncated"), "{m}"),
            other => panic!("expected Malformed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn duplicate_content_length_is_malformed() {
        let input =
            b"AIMX/1 SEND\nFrom-Mailbox: alice\nContent-Length: 3\nContent-Length: 4\n\nabcd";
        let err = parse_from_bytes(input).await.unwrap_err();
        match err {
            ParseError::Malformed(m) => assert!(m.contains("duplicate"), "{m}"),
            other => panic!("expected Malformed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn duplicate_from_mailbox_is_malformed() {
        let input = b"AIMX/1 SEND\nFrom-Mailbox: alice\nFrom-Mailbox: bob\nContent-Length: 0\n\n";
        let err = parse_from_bytes(input).await.unwrap_err();
        match err {
            ParseError::Malformed(m) => assert!(m.contains("duplicate"), "{m}"),
            other => panic!("expected Malformed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn empty_stream_is_closed_before_request() {
        let input = b"";
        let err = parse_from_bytes(input).await.unwrap_err();
        assert!(
            matches!(err, ParseError::ClosedBeforeRequest),
            "got {err:?}"
        );
    }

    #[tokio::test]
    async fn body_containing_request_line_literal_is_not_reparsed() {
        // The body contains the bytes `AIMX/1 SEND\n` — if the parser ever
        // re-scans for a new request mid-body it will misframe. Content-
        // Length must win.
        let body = b"AIMX/1 SEND\nFrom-Mailbox: evil\nContent-Length: 0\n\n";
        let mut input = Vec::new();
        input.extend_from_slice(b"AIMX/1 SEND\nFrom-Mailbox: alice\nContent-Length: ");
        input.extend_from_slice(body.len().to_string().as_bytes());
        input.extend_from_slice(b"\n\n");
        input.extend_from_slice(body);
        let req = parse_from_bytes(&input).await.unwrap();
        assert_eq!(req.from_mailbox, "alice");
        assert_eq!(req.body, body.to_vec());
    }

    #[tokio::test]
    async fn binary_safe_body_roundtrip() {
        let body: Vec<u8> = (0u8..=255).collect();
        let req = SendRequest {
            from_mailbox: "alice".to_string(),
            body: body.clone(),
        };
        let (mut client, mut server) = duplex(4096);
        let w = tokio::spawn(async move {
            write_request(&mut client, &req).await.unwrap();
        });
        let parsed = parse_request(&mut server).await.unwrap();
        w.await.unwrap();
        assert_eq!(parsed.from_mailbox, "alice");
        assert_eq!(parsed.body, body);
    }

    #[tokio::test]
    async fn write_ok_response() {
        let (mut client, mut server) = duplex(256);
        write_response(
            &mut client,
            &SendResponse::Ok {
                message_id: "<abc@example.com>".to_string(),
            },
        )
        .await
        .unwrap();
        drop(client);
        let mut buf = Vec::new();
        tokio::io::AsyncReadExt::read_to_end(&mut server, &mut buf)
            .await
            .unwrap();
        assert_eq!(buf, b"AIMX/1 OK <abc@example.com>\n");
    }

    #[tokio::test]
    async fn write_all_err_codes() {
        for (code, label) in [
            (ErrCode::Mailbox, "MAILBOX"),
            (ErrCode::Domain, "DOMAIN"),
            (ErrCode::Sign, "SIGN"),
            (ErrCode::Delivery, "DELIVERY"),
            (ErrCode::Temp, "TEMP"),
            (ErrCode::Malformed, "MALFORMED"),
        ] {
            let (mut client, mut server) = duplex(256);
            write_response(
                &mut client,
                &SendResponse::Err {
                    code,
                    reason: "nope".to_string(),
                },
            )
            .await
            .unwrap();
            drop(client);
            let mut buf = Vec::new();
            tokio::io::AsyncReadExt::read_to_end(&mut server, &mut buf)
                .await
                .unwrap();
            let expected = format!("AIMX/1 ERR {label} nope\n");
            assert_eq!(buf, expected.as_bytes());
        }
    }

    #[tokio::test]
    async fn response_reason_crlf_stripped() {
        let (mut client, mut server) = duplex(256);
        write_response(
            &mut client,
            &SendResponse::Err {
                code: ErrCode::Delivery,
                reason: "bad\r\nreason\n".to_string(),
            },
        )
        .await
        .unwrap();
        drop(client);
        let mut buf = Vec::new();
        tokio::io::AsyncReadExt::read_to_end(&mut server, &mut buf)
            .await
            .unwrap();
        assert_eq!(buf, b"AIMX/1 ERR DELIVERY badreason\n");
    }

    #[tokio::test]
    async fn missing_blank_line_is_malformed() {
        // `AIMX/1 SEND\nFrom-Mailbox: alice\nContent-Length: 0` followed by
        // EOF — the parser is mid-header, so it hits EOF with non-empty
        // buffer.
        let input = b"AIMX/1 SEND\nFrom-Mailbox: alice\nContent-Length: 0";
        let err = parse_from_bytes(input).await.unwrap_err();
        assert!(matches!(err, ParseError::Malformed(_)), "got {err:?}");
    }

    #[tokio::test]
    async fn custom_max_body_enforced() {
        let input = b"AIMX/1 SEND\nFrom-Mailbox: alice\nContent-Length: 100\n\n";
        let (mut client, mut server) = duplex(4096);
        client.write_all(input).await.unwrap();
        drop(client);
        let err = parse_request_with_limit(&mut server, 50).await.unwrap_err();
        match err {
            ParseError::Malformed(m) => assert!(m.contains("exceeds cap 50"), "{m}"),
            other => panic!("expected Malformed, got {other:?}"),
        }
    }
}
