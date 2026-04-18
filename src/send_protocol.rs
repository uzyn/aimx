//! `AIMX/1` wire protocol codec.
//!
//! Length-prefixed, binary-safe framing used by `aimx send` and the MCP
//! server to submit mail + state-mutation requests to `aimx serve` over
//! `/run/aimx/send.sock`. The codec is pure: it speaks only
//! `AsyncRead`/`AsyncWrite`, so it can be exercised with in-memory async
//! streams (e.g. `tokio_test::io::Builder`) without touching the filesystem
//! or a real socket.
//!
//! Framing:
//!
//! ```text
//! Client → Server (SEND):
//!   AIMX/1 SEND\n
//!   Content-Length: <n>\n
//!   \n
//!   <n bytes of RFC 5322 message, unsigned>
//!
//! Client → Server (MARK-READ / MARK-UNREAD):
//!   AIMX/1 MARK-READ\n
//!   Mailbox: <name>\n
//!   Id: <id>\n
//!   Folder: inbox|sent\n
//!   Content-Length: 0\n
//!   \n
//!
//! Client → Server (MAILBOX-CREATE / MAILBOX-DELETE):
//!   AIMX/1 MAILBOX-CREATE\n
//!   Name: <mailbox-name>\n
//!   Content-Length: 0\n
//!   \n
//!
//! Server → Client:
//!   AIMX/1 OK [<message-id>]\n
//! or
//!   AIMX/1 ERR <code> <reason>\n
//! ```
//!
//! The blank separator line is a literal `\n` (or `\r\n`). Header names are
//! case-insensitive. Unknown headers are silently ignored so the protocol
//! can be extended without breaking older clients.
//!
//! Notes on `SEND`:
//! - There is no `From-Mailbox:` header. The daemon parses the `From:`
//!   header from the submitted message body itself and resolves the
//!   sender mailbox against its in-memory Config. This lets `aimx send`
//!   run as a non-root user without needing read access to `config.toml`.
//!
//! Notes on state-mutation verbs:
//! - `MARK-READ` / `MARK-UNREAD` let the MCP server invoke
//!   `email_mark_read` / `email_mark_unread` without the MCP process
//!   needing write access to the root-owned mailbox files.
//! - `MAILBOX-CREATE` / `MAILBOX-DELETE` join the same codec, carrying a
//!   single `Name:` header. The daemon handles `config.toml`
//!   rewrite-and-rename plus the in-memory `RwLock<Arc<Config>>` swap,
//!   which is why `aimx mailbox create/delete` does not require a daemon
//!   restart for inbound routing to pick up the change. Error codes
//!   reuse the existing set plus `VALIDATION` (name validation failures)
//!   and `NONEMPTY` (delete refused because the mailbox still holds files).

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Maximum body size accepted by [`parse_request`]. Mirrors the SMTP DATA
/// cap (`smtp::DEFAULT_MAX_MESSAGE_SIZE`, 25 MB) so local clients cannot
/// submit mail the MTA would refuse anyway.
pub const DEFAULT_MAX_BODY_SIZE: usize = 25 * 1024 * 1024;

/// Maximum length of the request-line or a single header line, in bytes.
/// Keeps the parser's memory footprint bounded even on pathological input.
const MAX_HEADER_LINE: usize = 8 * 1024;

/// Decoded `AIMX/1 SEND` request. There is no `From-Mailbox:` header —
/// the daemon parses `From:` out of `body` and resolves the sender
/// mailbox itself against its in-memory Config.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SendRequest {
    pub body: Vec<u8>,
}

/// Which on-disk folder a MARK request targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MarkFolder {
    Inbox,
    Sent,
}

impl MarkFolder {
    pub fn as_str(self) -> &'static str {
        match self {
            MarkFolder::Inbox => "inbox",
            MarkFolder::Sent => "sent",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "inbox" => Some(MarkFolder::Inbox),
            "sent" => Some(MarkFolder::Sent),
            _ => None,
        }
    }
}

/// Decoded `AIMX/1 MARK-READ` or `AIMX/1 MARK-UNREAD` request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MarkRequest {
    pub mailbox: String,
    pub id: String,
    pub folder: MarkFolder,
    pub read: bool,
}

/// Decoded `AIMX/1 MAILBOX-CREATE` / `AIMX/1 MAILBOX-DELETE` request.
///
/// Both verbs share the same shape (a single `Name:` header and an empty
/// body); the enum selection is encoded in `create` so the codec stays a
/// single flat struct rather than two near-identical types.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MailboxCrudRequest {
    pub name: String,
    /// `true` for `MAILBOX-CREATE`, `false` for `MAILBOX-DELETE`.
    pub create: bool,
}

/// One decoded `AIMX/1` request, tagged by verb.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    Send(SendRequest),
    Mark(MarkRequest),
    MailboxCrud(MailboxCrudRequest),
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
    /// Codec-level failure: unknown verb, missing required header, or other
    /// framing violation that is neither malformed-message nor a
    /// business-logic error. Kept distinct from `Malformed` so callers can
    /// tell "bad RFC 5322 body" apart from "bad AIMX/1 envelope".
    Protocol,
    /// Generic not-found / invalid-argument at the handler level (e.g. the
    /// referenced email id is absent, the id contains `..`, etc.).
    NotFound,
    /// I/O failure while performing the requested mutation (read error,
    /// write error, parse error on the persisted frontmatter).
    Io,
    /// Input validation failure on a `MAILBOX-CREATE` / `MAILBOX-DELETE`
    /// request: the submitted mailbox name violated one of
    /// `validate_mailbox_name`'s rules (empty, `..`, path separator, NUL,
    /// etc.).
    Validation,
    /// `MAILBOX-DELETE` refused because the target directory still
    /// contains files. Operator must archive / remove files first — the
    /// daemon never silently removes mail on delete.
    NonEmpty,
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
            ErrCode::Protocol => "PROTOCOL",
            ErrCode::NotFound => "NOTFOUND",
            ErrCode::Io => "IO",
            ErrCode::Validation => "VALIDATION",
            ErrCode::NonEmpty => "NONEMPTY",
        }
    }
}

/// Response emitted by the daemon after processing a `SEND` request. The
/// `Ok` variant carries the message-id the daemon echoes back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SendResponse {
    Ok { message_id: String },
    Err { code: ErrCode, reason: String },
}

/// Response emitted by the daemon after processing a bodyless verb
/// (MARK-READ, MARK-UNREAD). `AIMX/1 OK\n` on success, no payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AckResponse {
    Ok,
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
    /// Verb token after `AIMX/1 ` was not one the codec recognises. The
    /// daemon maps this to `ERR PROTOCOL unknown verb '<x>'` to distinguish
    /// it from a body-level malformation.
    UnknownVerb(String),
    /// An underlying I/O error. Surfaced so the daemon can distinguish
    /// "client sent garbage" from "socket EBADF".
    Io(String),
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ParseError::ClosedBeforeRequest => write!(f, "connection closed before request"),
            ParseError::Malformed(s) => write!(f, "malformed request: {s}"),
            ParseError::UnknownVerb(v) => write!(f, "unknown verb '{v}'"),
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

/// Read and decode one `AIMX/1` request from `reader`. Dispatches on the
/// verb token of the request-line so `SEND`, `MARK-READ`, and `MARK-UNREAD`
/// share the same framing.
///
/// Uses the codec-level [`DEFAULT_MAX_BODY_SIZE`] cap. Callers that need a
/// different cap should use [`parse_request_with_limit`].
pub async fn parse_request<R>(reader: &mut R) -> Result<Request, ParseError>
where
    R: AsyncRead + Unpin,
{
    parse_request_with_limit(reader, DEFAULT_MAX_BODY_SIZE).await
}

/// Like [`parse_request`] but with a caller-supplied maximum body size.
pub async fn parse_request_with_limit<R>(
    reader: &mut R,
    max_body: usize,
) -> Result<Request, ParseError>
where
    R: AsyncRead + Unpin,
{
    let request_line = read_line(reader).await?;
    let request_line = match request_line {
        Some(l) => l,
        None => return Err(ParseError::ClosedBeforeRequest),
    };

    let line = request_line.trim_end_matches('\r');
    let verb = line
        .strip_prefix("AIMX/1 ")
        .ok_or_else(|| ParseError::Malformed(format!("expected 'AIMX/1 <verb>', got {line:?}")))?;

    match verb {
        "SEND" => parse_send_headers_and_body(reader, max_body)
            .await
            .map(Request::Send),
        "MARK-READ" => parse_mark_headers(reader, true).await.map(Request::Mark),
        "MARK-UNREAD" => parse_mark_headers(reader, false).await.map(Request::Mark),
        "MAILBOX-CREATE" => parse_mailbox_crud_headers(reader, true)
            .await
            .map(Request::MailboxCrud),
        "MAILBOX-DELETE" => parse_mailbox_crud_headers(reader, false)
            .await
            .map(Request::MailboxCrud),
        other => Err(ParseError::UnknownVerb(other.to_string())),
    }
}

/// Convenience wrapper for callers that only want the `SEND` verb.
/// Retained for test harnesses that exercise the SEND path directly; the
/// production dispatcher uses [`parse_request`] and matches on the
/// resulting [`Request`] enum so it can route MARK verbs to the state
/// handler. Returns `ParseError::Malformed` if a non-SEND verb is
/// received.
#[cfg(test)]
pub async fn parse_send_request<R>(reader: &mut R) -> Result<SendRequest, ParseError>
where
    R: AsyncRead + Unpin,
{
    match parse_request(reader).await? {
        Request::Send(r) => Ok(r),
        Request::Mark(_) => Err(ParseError::Malformed(
            "expected SEND verb, got MARK-*".to_string(),
        )),
        Request::MailboxCrud(_) => Err(ParseError::Malformed(
            "expected SEND verb, got MAILBOX-*".to_string(),
        )),
    }
}

async fn parse_send_headers_and_body<R>(
    reader: &mut R,
    max_body: usize,
) -> Result<SendRequest, ParseError>
where
    R: AsyncRead + Unpin,
{
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
                // The `From-Mailbox:` header is deliberately ignored here so
                // stray older-client submissions still parse (the value is
                // untrusted anyway — the daemon always re-derives the
                // mailbox from the message body's `From:` header).
            }
        }
    }

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

    Ok(SendRequest { body })
}

async fn parse_mark_headers<R>(reader: &mut R, read: bool) -> Result<MarkRequest, ParseError>
where
    R: AsyncRead + Unpin,
{
    let mut mailbox: Option<String> = None;
    let mut id: Option<String> = None;
    let mut folder: Option<MarkFolder> = None;
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
            "mailbox" => {
                if mailbox.is_some() {
                    return Err(ParseError::Malformed("duplicate Mailbox header".into()));
                }
                if value.is_empty() {
                    return Err(ParseError::Malformed("empty Mailbox value".into()));
                }
                mailbox = Some(value);
            }
            "id" => {
                if id.is_some() {
                    return Err(ParseError::Malformed("duplicate Id header".into()));
                }
                if value.is_empty() {
                    return Err(ParseError::Malformed("empty Id value".into()));
                }
                id = Some(value);
            }
            "folder" => {
                if folder.is_some() {
                    return Err(ParseError::Malformed("duplicate Folder header".into()));
                }
                let parsed = MarkFolder::parse(&value).ok_or_else(|| {
                    ParseError::Malformed(format!(
                        "invalid Folder '{value}', expected 'inbox' or 'sent'"
                    ))
                })?;
                folder = Some(parsed);
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
                if n != 0 {
                    return Err(ParseError::Malformed(format!(
                        "MARK verb must have Content-Length: 0, got {n}"
                    )));
                }
                content_length = Some(n);
            }
            _ => {
                // Unknown headers are ignored for forward-compatibility.
            }
        }
    }

    let mailbox =
        mailbox.ok_or_else(|| ParseError::Malformed("missing required header: Mailbox".into()))?;
    let id = id.ok_or_else(|| ParseError::Malformed("missing required header: Id".into()))?;
    let folder =
        folder.ok_or_else(|| ParseError::Malformed("missing required header: Folder".into()))?;
    // Content-Length is optional for MARK verbs but if present must be 0 —
    // the `!= 0` branch above already rejects otherwise. Accept the missing
    // case as "implicitly 0" to keep clients terse.
    let _ = content_length;

    Ok(MarkRequest {
        mailbox,
        id,
        folder,
        read,
    })
}

async fn parse_mailbox_crud_headers<R>(
    reader: &mut R,
    create: bool,
) -> Result<MailboxCrudRequest, ParseError>
where
    R: AsyncRead + Unpin,
{
    let mut name: Option<String> = None;
    let mut content_length: Option<usize> = None;

    loop {
        let line = read_line(reader)
            .await?
            .ok_or_else(|| ParseError::Malformed("unexpected EOF in headers".into()))?;
        let line = line.trim_end_matches('\r');
        if line.is_empty() {
            break;
        }

        let (n, v) = line
            .split_once(':')
            .ok_or_else(|| ParseError::Malformed(format!("invalid header line: {line:?}")))?;

        if !n.is_ascii() {
            return Err(ParseError::Malformed(format!(
                "non-ascii header name: {n:?}"
            )));
        }
        let name_norm = n.trim().to_ascii_lowercase();
        let value = v.trim().to_string();

        match name_norm.as_str() {
            "name" => {
                if name.is_some() {
                    return Err(ParseError::Malformed("duplicate Name header".into()));
                }
                if value.is_empty() {
                    return Err(ParseError::Malformed("empty Name value".into()));
                }
                name = Some(value);
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
                if n != 0 {
                    return Err(ParseError::Malformed(format!(
                        "MAILBOX verb must have Content-Length: 0, got {n}"
                    )));
                }
                content_length = Some(n);
            }
            _ => {
                // Unknown headers are ignored for forward-compatibility.
            }
        }
    }

    let name = name.ok_or_else(|| ParseError::Malformed("missing required header: Name".into()))?;
    // Content-Length is optional for MAILBOX-CRUD verbs — 0 is implicit.
    let _ = content_length;

    Ok(MailboxCrudRequest { name, create })
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

/// Write an `AckResponse` frame (bodyless OK / ERR) to `writer` and flush.
/// Used for MARK-READ / MARK-UNREAD and future bodyless verbs.
pub async fn write_ack_response<W>(
    writer: &mut W,
    response: &AckResponse,
) -> Result<(), std::io::Error>
where
    W: AsyncWrite + Unpin,
{
    let line = match response {
        AckResponse::Ok => "AIMX/1 OK\n".to_string(),
        AckResponse::Err { code, reason } => {
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
    let header = format!("AIMX/1 SEND\nContent-Length: {}\n\n", request.body.len());
    writer.write_all(header.as_bytes()).await?;
    writer.write_all(&request.body).await?;
    writer.flush().await?;
    Ok(())
}

/// Write an `AIMX/1 MARK-READ` or `AIMX/1 MARK-UNREAD` request frame. Verb
/// chosen by `request.read` (`true` → MARK-READ, `false` → MARK-UNREAD).
pub async fn write_mark_request<W>(
    writer: &mut W,
    request: &MarkRequest,
) -> Result<(), std::io::Error>
where
    W: AsyncWrite + Unpin,
{
    let verb = if request.read {
        "MARK-READ"
    } else {
        "MARK-UNREAD"
    };
    let header = format!(
        "AIMX/1 {verb}\nMailbox: {}\nId: {}\nFolder: {}\nContent-Length: 0\n\n",
        sanitize_inline(&request.mailbox),
        sanitize_inline(&request.id),
        request.folder.as_str(),
    );
    writer.write_all(header.as_bytes()).await?;
    writer.flush().await?;
    Ok(())
}

/// Write an `AIMX/1 MAILBOX-CREATE` or `AIMX/1 MAILBOX-DELETE` request
/// frame. Verb chosen by `request.create` (`true` → MAILBOX-CREATE,
/// `false` → MAILBOX-DELETE).
pub async fn write_mailbox_crud_request<W>(
    writer: &mut W,
    request: &MailboxCrudRequest,
) -> Result<(), std::io::Error>
where
    W: AsyncWrite + Unpin,
{
    let verb = if request.create {
        "MAILBOX-CREATE"
    } else {
        "MAILBOX-DELETE"
    };
    let header = format!(
        "AIMX/1 {verb}\nName: {}\nContent-Length: 0\n\n",
        sanitize_inline(&request.name),
    );
    writer.write_all(header.as_bytes()).await?;
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

    async fn parse_send_from_bytes(input: &[u8]) -> Result<SendRequest, ParseError> {
        let (mut client, mut server) = duplex(input.len().max(64));
        client.write_all(input).await.unwrap();
        drop(client);
        parse_send_request(&mut server).await
    }

    async fn parse_any_from_bytes(input: &[u8]) -> Result<Request, ParseError> {
        let (mut client, mut server) = duplex(input.len().max(64));
        client.write_all(input).await.unwrap();
        drop(client);
        parse_request(&mut server).await
    }

    // ----- SEND verb -------------------------------------------------

    #[tokio::test]
    async fn parses_minimal_send_request() {
        let input = b"AIMX/1 SEND\nContent-Length: 5\n\nhello";
        let req = parse_send_from_bytes(input).await.unwrap();
        assert_eq!(req.body, b"hello".to_vec());
    }

    #[tokio::test]
    async fn parses_send_with_crlf_line_endings() {
        let input = b"AIMX/1 SEND\r\nContent-Length: 5\r\n\r\nhello";
        let req = parse_send_from_bytes(input).await.unwrap();
        assert_eq!(req.body, b"hello".to_vec());
    }

    #[tokio::test]
    async fn parses_send_empty_body() {
        let input = b"AIMX/1 SEND\nContent-Length: 0\n\n";
        let req = parse_send_from_bytes(input).await.unwrap();
        assert!(req.body.is_empty());
    }

    #[tokio::test]
    async fn send_header_names_case_insensitive() {
        let input = b"AIMX/1 SEND\ncontent-length: 3\n\nabc";
        let req = parse_send_from_bytes(input).await.unwrap();
        assert_eq!(req.body, b"abc".to_vec());
    }

    #[tokio::test]
    async fn send_unknown_headers_ignored() {
        let input = b"AIMX/1 SEND\nX-Future: foo\nContent-Length: 3\n\nabc";
        let req = parse_send_from_bytes(input).await.unwrap();
        assert_eq!(req.body, b"abc".to_vec());
    }

    #[tokio::test]
    async fn legacy_from_mailbox_header_is_silently_ignored() {
        // Older clients may still emit `From-Mailbox:` — the codec treats
        // it as unknown (the value is never trusted anyway; the daemon
        // re-derives the mailbox from the message body's `From:` header).
        let input = b"AIMX/1 SEND\nFrom-Mailbox: bob\nContent-Length: 3\n\nabc";
        let req = parse_send_from_bytes(input).await.unwrap();
        assert_eq!(req.body, b"abc".to_vec());
    }

    #[tokio::test]
    async fn wrong_request_line_is_malformed() {
        let input = b"GET / HTTP/1.1\nHost: foo\n\n";
        let err = parse_any_from_bytes(input).await.unwrap_err();
        assert!(matches!(err, ParseError::Malformed(_)), "got {err:?}");
    }

    #[tokio::test]
    async fn unknown_verb_is_reported_distinctly() {
        let input = b"AIMX/1 EXPLODE\nContent-Length: 0\n\n";
        let err = parse_any_from_bytes(input).await.unwrap_err();
        match err {
            ParseError::UnknownVerb(v) => assert_eq!(v, "EXPLODE"),
            other => panic!("expected UnknownVerb, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn send_missing_content_length_is_malformed() {
        let input = b"AIMX/1 SEND\n\n";
        let err = parse_send_from_bytes(input).await.unwrap_err();
        match err {
            ParseError::Malformed(m) => assert!(m.contains("Content-Length"), "{m}"),
            other => panic!("expected Malformed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn non_integer_content_length_is_malformed() {
        let input = b"AIMX/1 SEND\nContent-Length: abc\n\n";
        let err = parse_send_from_bytes(input).await.unwrap_err();
        match err {
            ParseError::Malformed(m) => assert!(m.contains("non-integer"), "{m}"),
            other => panic!("expected Malformed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn oversized_content_length_is_malformed() {
        let input = b"AIMX/1 SEND\nContent-Length: 999999999999\n\n";
        let err = parse_send_from_bytes(input).await.unwrap_err();
        match err {
            ParseError::Malformed(m) => assert!(m.contains("exceeds cap"), "{m}"),
            other => panic!("expected Malformed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn truncated_body_is_malformed() {
        let input = b"AIMX/1 SEND\nContent-Length: 10\n\nabc";
        let err = parse_send_from_bytes(input).await.unwrap_err();
        match err {
            ParseError::Malformed(m) => assert!(m.contains("truncated"), "{m}"),
            other => panic!("expected Malformed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn duplicate_content_length_is_malformed() {
        let input = b"AIMX/1 SEND\nContent-Length: 3\nContent-Length: 4\n\nabcd";
        let err = parse_send_from_bytes(input).await.unwrap_err();
        match err {
            ParseError::Malformed(m) => assert!(m.contains("duplicate"), "{m}"),
            other => panic!("expected Malformed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn empty_stream_is_closed_before_request() {
        let input = b"";
        let err = parse_any_from_bytes(input).await.unwrap_err();
        assert!(
            matches!(err, ParseError::ClosedBeforeRequest),
            "got {err:?}"
        );
    }

    #[tokio::test]
    async fn body_containing_request_line_literal_is_not_reparsed() {
        let body = b"AIMX/1 SEND\nContent-Length: 0\n\n";
        let mut input = Vec::new();
        input.extend_from_slice(b"AIMX/1 SEND\nContent-Length: ");
        input.extend_from_slice(body.len().to_string().as_bytes());
        input.extend_from_slice(b"\n\n");
        input.extend_from_slice(body);
        let req = parse_send_from_bytes(&input).await.unwrap();
        assert_eq!(req.body, body.to_vec());
    }

    #[tokio::test]
    async fn binary_safe_send_body_roundtrip() {
        let body: Vec<u8> = (0u8..=255).collect();
        let req = SendRequest { body: body.clone() };
        let (mut client, mut server) = duplex(4096);
        let w = tokio::spawn(async move {
            write_request(&mut client, &req).await.unwrap();
        });
        let parsed = parse_send_request(&mut server).await.unwrap();
        w.await.unwrap();
        assert_eq!(parsed.body, body);
    }

    #[tokio::test]
    async fn send_missing_blank_line_is_malformed() {
        let input = b"AIMX/1 SEND\nContent-Length: 0";
        let err = parse_send_from_bytes(input).await.unwrap_err();
        assert!(matches!(err, ParseError::Malformed(_)), "got {err:?}");
    }

    #[tokio::test]
    async fn send_custom_max_body_enforced() {
        let input = b"AIMX/1 SEND\nContent-Length: 100\n\n";
        let (mut client, mut server) = duplex(4096);
        client.write_all(input).await.unwrap();
        drop(client);
        let err = parse_request_with_limit(&mut server, 50).await.unwrap_err();
        match err {
            ParseError::Malformed(m) => assert!(m.contains("exceeds cap 50"), "{m}"),
            other => panic!("expected Malformed, got {other:?}"),
        }
    }

    // ----- Responses -------------------------------------------------

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
            (ErrCode::Protocol, "PROTOCOL"),
            (ErrCode::NotFound, "NOTFOUND"),
            (ErrCode::Io, "IO"),
            (ErrCode::Validation, "VALIDATION"),
            (ErrCode::NonEmpty, "NONEMPTY"),
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
    async fn write_ack_ok_and_err() {
        let (mut client, mut server) = duplex(256);
        write_ack_response(&mut client, &AckResponse::Ok)
            .await
            .unwrap();
        drop(client);
        let mut buf = Vec::new();
        tokio::io::AsyncReadExt::read_to_end(&mut server, &mut buf)
            .await
            .unwrap();
        assert_eq!(buf, b"AIMX/1 OK\n");

        let (mut client, mut server) = duplex(256);
        write_ack_response(
            &mut client,
            &AckResponse::Err {
                code: ErrCode::NotFound,
                reason: "missing".into(),
            },
        )
        .await
        .unwrap();
        drop(client);
        let mut buf = Vec::new();
        tokio::io::AsyncReadExt::read_to_end(&mut server, &mut buf)
            .await
            .unwrap();
        assert_eq!(buf, b"AIMX/1 ERR NOTFOUND missing\n");
    }

    // ----- MARK-READ / MARK-UNREAD verbs -----------------------------

    #[tokio::test]
    async fn parses_mark_read_request() {
        let input = b"AIMX/1 MARK-READ\nMailbox: alice\nId: 2025-06-01-001\nFolder: inbox\nContent-Length: 0\n\n";
        match parse_any_from_bytes(input).await.unwrap() {
            Request::Mark(r) => {
                assert_eq!(r.mailbox, "alice");
                assert_eq!(r.id, "2025-06-01-001");
                assert_eq!(r.folder, MarkFolder::Inbox);
                assert!(r.read);
            }
            other => panic!("expected Mark, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn parses_mark_unread_request_with_sent_folder() {
        let input = b"AIMX/1 MARK-UNREAD\nMailbox: alice\nId: 2025-06-02-001\nFolder: sent\nContent-Length: 0\n\n";
        match parse_any_from_bytes(input).await.unwrap() {
            Request::Mark(r) => {
                assert_eq!(r.mailbox, "alice");
                assert_eq!(r.folder, MarkFolder::Sent);
                assert!(!r.read);
            }
            other => panic!("expected Mark, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn mark_without_content_length_accepted() {
        // Content-Length is optional for MARK — 0 is implicit.
        let input = b"AIMX/1 MARK-READ\nMailbox: alice\nId: 2025-06-01-001\nFolder: inbox\n\n";
        match parse_any_from_bytes(input).await.unwrap() {
            Request::Mark(r) => assert_eq!(r.id, "2025-06-01-001"),
            other => panic!("expected Mark, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn mark_missing_mailbox_is_malformed() {
        let input = b"AIMX/1 MARK-READ\nId: 2025-06-01-001\nFolder: inbox\n\n";
        let err = parse_any_from_bytes(input).await.unwrap_err();
        match err {
            ParseError::Malformed(m) => assert!(m.contains("Mailbox"), "{m}"),
            other => panic!("expected Malformed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn mark_missing_id_is_malformed() {
        let input = b"AIMX/1 MARK-READ\nMailbox: alice\nFolder: inbox\n\n";
        let err = parse_any_from_bytes(input).await.unwrap_err();
        match err {
            ParseError::Malformed(m) => assert!(m.contains("Id"), "{m}"),
            other => panic!("expected Malformed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn mark_missing_folder_is_malformed() {
        let input = b"AIMX/1 MARK-READ\nMailbox: alice\nId: 2025-06-01-001\n\n";
        let err = parse_any_from_bytes(input).await.unwrap_err();
        match err {
            ParseError::Malformed(m) => assert!(m.contains("Folder"), "{m}"),
            other => panic!("expected Malformed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn mark_invalid_folder_is_malformed() {
        let input = b"AIMX/1 MARK-READ\nMailbox: alice\nId: 2025-06-01-001\nFolder: drafts\n\n";
        let err = parse_any_from_bytes(input).await.unwrap_err();
        match err {
            ParseError::Malformed(m) => assert!(m.contains("Folder"), "{m}"),
            other => panic!("expected Malformed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn mark_nonzero_content_length_is_malformed() {
        let input = b"AIMX/1 MARK-READ\nMailbox: alice\nId: 2025-06-01-001\nFolder: inbox\nContent-Length: 5\n\nhello";
        let err = parse_any_from_bytes(input).await.unwrap_err();
        match err {
            ParseError::Malformed(m) => assert!(m.contains("Content-Length: 0"), "{m}"),
            other => panic!("expected Malformed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn mark_request_roundtrip() {
        let req = MarkRequest {
            mailbox: "alice".to_string(),
            id: "2025-06-01-001".to_string(),
            folder: MarkFolder::Sent,
            read: false,
        };
        let (mut client, mut server) = duplex(1024);
        let w = tokio::spawn(async move {
            write_mark_request(&mut client, &req).await.unwrap();
        });
        let parsed = parse_request(&mut server).await.unwrap();
        w.await.unwrap();
        match parsed {
            Request::Mark(r) => {
                assert_eq!(r.mailbox, "alice");
                assert_eq!(r.id, "2025-06-01-001");
                assert_eq!(r.folder, MarkFolder::Sent);
                assert!(!r.read);
            }
            other => panic!("expected Mark, got {other:?}"),
        }
    }

    // ----- MAILBOX-CREATE / MAILBOX-DELETE verbs ---------------------

    #[tokio::test]
    async fn parses_mailbox_create_request() {
        let input = b"AIMX/1 MAILBOX-CREATE\nName: alice\nContent-Length: 0\n\n";
        match parse_any_from_bytes(input).await.unwrap() {
            Request::MailboxCrud(r) => {
                assert_eq!(r.name, "alice");
                assert!(r.create);
            }
            other => panic!("expected MailboxCrud, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn parses_mailbox_delete_request() {
        let input = b"AIMX/1 MAILBOX-DELETE\nName: alice\nContent-Length: 0\n\n";
        match parse_any_from_bytes(input).await.unwrap() {
            Request::MailboxCrud(r) => {
                assert_eq!(r.name, "alice");
                assert!(!r.create);
            }
            other => panic!("expected MailboxCrud, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn mailbox_crud_without_content_length_accepted() {
        let input = b"AIMX/1 MAILBOX-CREATE\nName: alice\n\n";
        match parse_any_from_bytes(input).await.unwrap() {
            Request::MailboxCrud(r) => assert_eq!(r.name, "alice"),
            other => panic!("expected MailboxCrud, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn mailbox_crud_missing_name_is_malformed() {
        let input = b"AIMX/1 MAILBOX-CREATE\nContent-Length: 0\n\n";
        let err = parse_any_from_bytes(input).await.unwrap_err();
        match err {
            ParseError::Malformed(m) => assert!(m.contains("Name"), "{m}"),
            other => panic!("expected Malformed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn mailbox_crud_empty_name_is_malformed() {
        let input = b"AIMX/1 MAILBOX-CREATE\nName: \nContent-Length: 0\n\n";
        let err = parse_any_from_bytes(input).await.unwrap_err();
        match err {
            ParseError::Malformed(m) => assert!(m.contains("empty Name"), "{m}"),
            other => panic!("expected Malformed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn mailbox_crud_duplicate_name_is_malformed() {
        let input = b"AIMX/1 MAILBOX-CREATE\nName: alice\nName: bob\n\n";
        let err = parse_any_from_bytes(input).await.unwrap_err();
        match err {
            ParseError::Malformed(m) => assert!(m.contains("duplicate Name"), "{m}"),
            other => panic!("expected Malformed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn mailbox_crud_nonzero_content_length_is_malformed() {
        let input = b"AIMX/1 MAILBOX-CREATE\nName: alice\nContent-Length: 5\n\nhello";
        let err = parse_any_from_bytes(input).await.unwrap_err();
        match err {
            ParseError::Malformed(m) => assert!(m.contains("Content-Length: 0"), "{m}"),
            other => panic!("expected Malformed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn mailbox_crud_header_names_case_insensitive() {
        let input = b"AIMX/1 MAILBOX-CREATE\nname: alice\ncontent-length: 0\n\n";
        match parse_any_from_bytes(input).await.unwrap() {
            Request::MailboxCrud(r) => assert_eq!(r.name, "alice"),
            other => panic!("expected MailboxCrud, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn mailbox_crud_request_roundtrip() {
        for create in [true, false] {
            let req = MailboxCrudRequest {
                name: "alice".to_string(),
                create,
            };
            let (mut client, mut server) = duplex(1024);
            let w = {
                let req = req.clone();
                tokio::spawn(async move {
                    write_mailbox_crud_request(&mut client, &req).await.unwrap();
                })
            };
            let parsed = parse_request(&mut server).await.unwrap();
            w.await.unwrap();
            match parsed {
                Request::MailboxCrud(r) => {
                    assert_eq!(r.name, "alice");
                    assert_eq!(r.create, create);
                }
                other => panic!("expected MailboxCrud, got {other:?}"),
            }
        }
    }

    #[tokio::test]
    async fn mailbox_crud_verb_selection_in_wire_format() {
        // Explicit verb-selection: `create=true` serializes to
        // MAILBOX-CREATE, `create=false` serializes to MAILBOX-DELETE.
        for (create, verb) in [
            (true, b"AIMX/1 MAILBOX-CREATE".as_slice()),
            (false, b"AIMX/1 MAILBOX-DELETE".as_slice()),
        ] {
            let req = MailboxCrudRequest {
                name: "alice".to_string(),
                create,
            };
            let (mut client, mut server) = duplex(1024);
            write_mailbox_crud_request(&mut client, &req).await.unwrap();
            drop(client);
            let mut buf = Vec::new();
            tokio::io::AsyncReadExt::read_to_end(&mut server, &mut buf)
                .await
                .unwrap();
            assert!(
                buf.starts_with(verb),
                "expected prefix {:?}, got {:?}",
                String::from_utf8_lossy(verb),
                String::from_utf8_lossy(&buf)
            );
        }
    }

    #[tokio::test]
    async fn mark_read_true_and_false_roundtrip_verb() {
        // Explicit verb-selection assertion: `read=true` serializes to
        // MARK-READ, `read=false` serializes to MARK-UNREAD.
        for (read, verb) in [
            (true, b"AIMX/1 MARK-READ".as_slice()),
            (false, b"AIMX/1 MARK-UNREAD".as_slice()),
        ] {
            let req = MarkRequest {
                mailbox: "x".into(),
                id: "1".into(),
                folder: MarkFolder::Inbox,
                read,
            };
            let (mut client, mut server) = duplex(1024);
            write_mark_request(&mut client, &req).await.unwrap();
            drop(client);
            let mut buf = Vec::new();
            tokio::io::AsyncReadExt::read_to_end(&mut server, &mut buf)
                .await
                .unwrap();
            assert!(
                buf.starts_with(verb),
                "expected prefix {:?}, got {:?}",
                String::from_utf8_lossy(verb),
                String::from_utf8_lossy(&buf)
            );
        }
    }
}
