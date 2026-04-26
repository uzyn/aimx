//! `AIMX/1` wire protocol codec.
//!
//! Length-prefixed, binary-safe framing used by `aimx send` and the MCP
//! server to submit mail + state-mutation requests to `aimx serve` over
//! `/run/aimx/aimx.sock`. The codec is pure: it speaks only
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
//! Client → Server (HOOK-CREATE):
//!   AIMX/1 HOOK-CREATE\n
//!   Mailbox: <mailbox-name>\n
//!   Event: on_receive|after_send\n
//!   Name: <optional-hook-name>\n
//!   Content-Length: <n>\n
//!   \n
//!   <n bytes of body — handler shape is reworked in a later sprint>
//!
//! Client → Server (HOOK-DELETE):
//!   AIMX/1 HOOK-DELETE\n
//!   Hook-Name: <name>\n
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
//!   which is why `aimx mailboxes create/delete` does not require a daemon
//!   restart for inbound routing to pick up the change. Error codes
//!   reuse the existing set plus `VALIDATION` (name validation failures)
//!   and `NONEMPTY` (delete refused because the mailbox still holds files).
//! - `HOOK-CREATE` / `HOOK-DELETE` are placeholder verbs at the codec
//!   layer; the schema for hooks-over-UDS is reworked in a later sprint.
//!   Raw-cmd hooks remain operator-only via `sudo aimx hooks create
//!   --cmd`, which writes `config.toml` directly.

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Maximum body size accepted by [`parse_request`]. Mirrors the SMTP DATA
/// cap (`smtp::DEFAULT_MAX_MESSAGE_SIZE`, 25 MB) so local clients cannot
/// submit mail the MTA would refuse anyway.
pub const DEFAULT_MAX_BODY_SIZE: usize = 25 * 1024 * 1024;

/// Maximum length of the request-line or a single header line, in bytes.
/// Keeps the parser's memory footprint bounded even on pathological input.
const MAX_HEADER_LINE: usize = 8 * 1024;

/// Decoded `AIMX/1 SEND` request. There is no `From-Mailbox:` header;
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
/// Both verbs share the same shape; the enum selection is encoded in
/// `create` so the codec stays a single flat struct rather than two
/// near-identical types. `MAILBOX-CREATE` requires an `Owner:` header
/// so the daemon knows which Linux user to chown the newly-created
/// mailbox directories to. `MAILBOX-DELETE` ignores
/// `owner` — the daemon only needs the name to remove the stanza.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MailboxCrudRequest {
    pub name: String,
    /// `true` for `MAILBOX-CREATE`, `false` for `MAILBOX-DELETE`.
    pub create: bool,
    /// Linux user that owns the mailbox storage. Required on CREATE,
    /// ignored on DELETE. The daemon validates the owner resolves via
    /// `getpwnam` and chowns `inbox/<name>/` + `sent/<name>/` to
    /// `<owner>:<owner>` mode `0700`.
    pub owner: Option<String>,
}

/// Decoded `AIMX/1 HOOK-CREATE` request. The verb wiring is reworked
/// in a later sprint; for now the daemon-side handler returns
/// `ERR PROTOCOL hook-create over UDS is not implemented` so callers
/// fall back to the root-only CLI path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookCreateRequest {
    pub mailbox: String,
    pub event: String,
    pub name: Option<String>,
    pub body: Vec<u8>,
}

/// Decoded `AIMX/1 HOOK-DELETE` request. Locates the hook by effective
/// name (explicit or derived) across every configured mailbox; there is
/// no `Mailbox:` header on delete.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookDeleteRequest {
    pub name: String,
}

/// One decoded `AIMX/1` request, tagged by verb.
#[derive(Debug, Clone, PartialEq)]
pub enum Request {
    Send(SendRequest),
    Mark(MarkRequest),
    MailboxCrud(MailboxCrudRequest),
    HookCreate(HookCreateRequest),
    HookDelete(HookDeleteRequest),
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
    /// contains files. Operator must archive / remove files first; the
    /// daemon never silently removes mail on delete.
    NonEmpty,
    /// `MAILBOX-CREATE` found `inbox/<name>/` / `sent/<name>/` already
    /// present but owned by a different uid/gid than the requested
    /// owner. Ambiguous state — operator must fix it with `chown`
    /// before the daemon will claim the directories.
    Conflict,
    /// The caller's uid (from `SO_PEERCRED`) is not authorized for the
    /// requested verb on the target mailbox, and is not root. Mirrors
    /// POSIX `EACCES` for operator ergonomics.
    Eaccess,
    /// The target resource referenced by the verb (mailbox, hook,
    /// template) does not exist. Distinct from [`ErrCode::NotFound`]
    /// so authz helpers can signal "unknown-target" vs
    /// "unknown-email-id" without overloading the legacy code. Emitted
    /// instead of `EACCES` whenever the caller's authz outcome is
    /// leaked by the existence-check itself (PRD
    /// §6.5 leak-free shape).
    Enoent,
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
            ErrCode::Conflict => "ECONFLICT",
            ErrCode::Eaccess => "EACCES",
            ErrCode::Enoent => "ENOENT",
        }
    }

    /// Parse a wire-level code string back into the tagged enum.
    /// Returns `None` for unknown strings so clients distinguish
    /// "daemon drifted past our vocabulary" from "daemon reported X".
    pub fn from_str(s: &str) -> Option<Self> {
        let v = match s {
            "MAILBOX" => ErrCode::Mailbox,
            "DOMAIN" => ErrCode::Domain,
            "SIGN" => ErrCode::Sign,
            "DELIVERY" => ErrCode::Delivery,
            "TEMP" => ErrCode::Temp,
            "MALFORMED" => ErrCode::Malformed,
            "PROTOCOL" => ErrCode::Protocol,
            "NOTFOUND" => ErrCode::NotFound,
            "IO" => ErrCode::Io,
            "VALIDATION" => ErrCode::Validation,
            "NONEMPTY" => ErrCode::NonEmpty,
            "ECONFLICT" => ErrCode::Conflict,
            "EACCES" => ErrCode::Eaccess,
            "ENOENT" => ErrCode::Enoent,
            _ => return None,
        };
        Some(v)
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
        "HOOK-CREATE" => parse_hook_create_headers_and_body(reader, max_body)
            .await
            .map(Request::HookCreate),
        "HOOK-DELETE" => parse_hook_delete_headers(reader)
            .await
            .map(Request::HookDelete),
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
        Request::HookCreate(_) | Request::HookDelete(_) => Err(ParseError::Malformed(
            "expected SEND verb, got HOOK-*".to_string(),
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
                // Unknown headers are ignored. The daemon re-derives the
                // mailbox from the message body's `From:` header; no
                // header-provided sender is ever trusted.
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
                // Unknown headers are ignored.
            }
        }
    }

    let mailbox =
        mailbox.ok_or_else(|| ParseError::Malformed("missing required header: Mailbox".into()))?;
    let id = id.ok_or_else(|| ParseError::Malformed("missing required header: Id".into()))?;
    let folder =
        folder.ok_or_else(|| ParseError::Malformed("missing required header: Folder".into()))?;
    // Content-Length is optional for MARK verbs but if present must be 0.
    // The `!= 0` branch above already rejects otherwise. Accept the missing
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
    let mut owner: Option<String> = None;
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
            "owner" => {
                if owner.is_some() {
                    return Err(ParseError::Malformed("duplicate Owner header".into()));
                }
                if value.is_empty() {
                    return Err(ParseError::Malformed("empty Owner value".into()));
                }
                // Shape-check the owner at parse time so malformed
                // header values (embedded whitespace, colons, tabs,
                // etc.) reject with a precise `Malformed` here rather
                // than at the `getpwnam`/resolver layer further in.
                // Mirrors the `validate_run_as` regex gate.
                if !crate::config::is_valid_system_username(&value) {
                    return Err(ParseError::Malformed(format!(
                        "invalid Owner value: {value:?} (must match [a-z_][a-z0-9_-]*[$]?)"
                    )));
                }
                owner = Some(value);
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
                // Unknown headers are ignored.
            }
        }
    }

    let name = name.ok_or_else(|| ParseError::Malformed("missing required header: Name".into()))?;
    // Content-Length is optional for MAILBOX-CRUD verbs; 0 is implicit.
    let _ = content_length;

    // Owner is REQUIRED on CREATE. On DELETE the daemon
    // only needs the name; Owner is ignored even if supplied.
    if create && owner.is_none() {
        return Err(ParseError::Malformed(
            "missing required header: Owner".into(),
        ));
    }

    Ok(MailboxCrudRequest {
        name,
        create,
        owner,
    })
}

async fn parse_hook_create_headers_and_body<R>(
    reader: &mut R,
    max_body: usize,
) -> Result<HookCreateRequest, ParseError>
where
    R: AsyncRead + Unpin,
{
    let mut mailbox: Option<String> = None;
    let mut event: Option<String> = None;
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
            "mailbox" => {
                if mailbox.is_some() {
                    return Err(ParseError::Malformed("duplicate Mailbox header".into()));
                }
                if value.is_empty() {
                    return Err(ParseError::Malformed("empty Mailbox value".into()));
                }
                mailbox = Some(value);
            }
            "event" => {
                if event.is_some() {
                    return Err(ParseError::Malformed("duplicate Event header".into()));
                }
                if value.is_empty() {
                    return Err(ParseError::Malformed("empty Event value".into()));
                }
                event = Some(value);
            }
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
                if n > max_body {
                    return Err(ParseError::Malformed(format!(
                        "Content-Length {n} exceeds cap {max_body}"
                    )));
                }
                content_length = Some(n);
            }
            _ => {}
        }
    }

    let mailbox =
        mailbox.ok_or_else(|| ParseError::Malformed("missing required header: Mailbox".into()))?;
    let event =
        event.ok_or_else(|| ParseError::Malformed("missing required header: Event".into()))?;
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

    Ok(HookCreateRequest {
        mailbox,
        event,
        name,
        body,
    })
}

async fn parse_hook_delete_headers<R>(reader: &mut R) -> Result<HookDeleteRequest, ParseError>
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
        let header_name = n.trim().to_ascii_lowercase();
        let value = v.trim().to_string();

        match header_name.as_str() {
            "hook-name" => {
                if name.is_some() {
                    return Err(ParseError::Malformed("duplicate Hook-Name header".into()));
                }
                if value.is_empty() {
                    return Err(ParseError::Malformed("empty Hook-Name value".into()));
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
                        "HOOK-DELETE must have Content-Length: 0, got {n}"
                    )));
                }
                content_length = Some(n);
            }
            _ => {
                // Unknown headers are ignored.
            }
        }
    }

    let name =
        name.ok_or_else(|| ParseError::Malformed("missing required header: Hook-Name".into()))?;
    let _ = content_length;

    Ok(HookDeleteRequest { name })
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

/// Write a `SendResponse` frame to `writer` and flush.
///
/// Wire format:
/// ```text
/// AIMX/1 OK <message-id>\n           (success)
/// AIMX/1 ERR <CODE> <reason>\n       (error, status line)
/// Code: <CODE>\n                     (structured header)
/// \n                                 (terminator)
/// ```
///
/// The legacy inline `<CODE>` on the status line is preserved so older
/// clients that only parse the first line keep working. New clients
/// read the `Code:` header off the second line for stable branching.
pub async fn write_response<W>(
    writer: &mut W,
    response: &SendResponse,
) -> Result<(), std::io::Error>
where
    W: AsyncWrite + Unpin,
{
    let payload = match response {
        SendResponse::Ok { message_id } => {
            let id = sanitize_inline(message_id);
            format!("AIMX/1 OK {id}\n")
        }
        SendResponse::Err { code, reason } => {
            let r = sanitize_inline(reason);
            format!(
                "AIMX/1 ERR {code} {r}\nCode: {code}\n\n",
                code = code.as_str()
            )
        }
    };
    writer.write_all(payload.as_bytes()).await?;
    writer.flush().await?;
    Ok(())
}

/// Write an `AckResponse` frame (bodyless OK / ERR) to `writer` and flush.
/// Used for MARK-READ / MARK-UNREAD and future bodyless verbs.
///
/// Wire format matches [`write_response`]: the error form carries both
/// the legacy inline `<CODE>` token on the status line AND an explicit
/// `Code:` header on the next line.
pub async fn write_ack_response<W>(
    writer: &mut W,
    response: &AckResponse,
) -> Result<(), std::io::Error>
where
    W: AsyncWrite + Unpin,
{
    let payload = match response {
        AckResponse::Ok => "AIMX/1 OK\n".to_string(),
        AckResponse::Err { code, reason } => {
            let r = sanitize_inline(reason);
            format!(
                "AIMX/1 ERR {code} {r}\nCode: {code}\n\n",
                code = code.as_str()
            )
        }
    };
    writer.write_all(payload.as_bytes()).await?;
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
/// `false` → MAILBOX-DELETE). `Owner:` is emitted when `request.owner`
/// is `Some`; the parser requires it on CREATE.
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
    let mut header = format!("AIMX/1 {verb}\nName: {}\n", sanitize_inline(&request.name),);
    if let Some(owner) = &request.owner {
        header.push_str(&format!("Owner: {}\n", sanitize_inline(owner)));
    }
    header.push_str("Content-Length: 0\n\n");
    writer.write_all(header.as_bytes()).await?;
    writer.flush().await?;
    Ok(())
}

/// Write an `AIMX/1 HOOK-CREATE` request frame. Template-only: the body
/// is a JSON-encoded [`HookTemplateCreateBody`]; the codec does not
/// parse the body, it simply ships the bytes the caller supplies.
#[allow(dead_code)]
pub async fn write_hook_create_request<W>(
    writer: &mut W,
    request: &HookCreateRequest,
) -> Result<(), std::io::Error>
where
    W: AsyncWrite + Unpin,
{
    let mut header = format!(
        "AIMX/1 HOOK-CREATE\nMailbox: {}\nEvent: {}\n",
        sanitize_inline(&request.mailbox),
        sanitize_inline(&request.event),
    );
    if let Some(name) = &request.name {
        header.push_str(&format!("Name: {}\n", sanitize_inline(name)));
    }
    header.push_str(&format!("Content-Length: {}\n\n", request.body.len()));
    writer.write_all(header.as_bytes()).await?;
    writer.write_all(&request.body).await?;
    writer.flush().await?;
    Ok(())
}

/// Write an `AIMX/1 HOOK-DELETE` request frame. Empty body.
#[allow(dead_code)]
pub async fn write_hook_delete_request<W>(
    writer: &mut W,
    request: &HookDeleteRequest,
) -> Result<(), std::io::Error>
where
    W: AsyncWrite + Unpin,
{
    let header = format!(
        "AIMX/1 HOOK-DELETE\nHook-Name: {}\nContent-Length: 0\n\n",
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
