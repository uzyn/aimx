//! Daemon-side outbound MIME assembly.
//!
//! The thin UDS client (`aimx send`) ships only headers + an unprocessed
//! Markdown body (or, when attachments are present, a `multipart/mixed`
//! whose first part is `text/plain` Markdown and whose remaining parts are
//! the attachments). The renderer dependency (`comrak`, `lol_html`) lives
//! daemon-side so the client startup path stays lean.
//!
//! `assemble_wire_message` does the wire shape decision in three branches,
//! selected by the `text_only` / `html_body` arguments (the SEND frame's
//! `Text-Only:` / `Html-Body-Length:` headers):
//!
//! - **default Markdown** (neither escape hatch set):
//!   - No attachments → `multipart/alternative` (text/plain + rendered HTML)
//!   - With attachments → `multipart/mixed` wrapping the alternative
//! - **`--text-only`** (`text_only=true`):
//!   - No attachments → single-part `text/plain`, body verbatim, no rendering
//!   - With attachments → `multipart/mixed` with the body as `text/plain`
//! - **`--html-body`** (`html_body=Some(..)`):
//!   - No attachments → `multipart/alternative` (body as text/plain, supplied
//!     HTML verbatim)
//!   - With attachments → `multipart/mixed` wrapping the alternative
//!
//! The per-mailbox signature is appended to the Markdown source **before**
//! rendering so it participates in the HTML output (a `[link](url)`
//! signature renders as a clickable `<a>` in HTML and stays Markdown in the
//! plain-text part). The signature is **only** appended on the default
//! Markdown path — `--text-only` and `--html-body` use the body verbatim
//! because the operator already chose the final rendering.
//!
//! `text/plain` and `text/html` parts use 7bit transfer-encoding when their
//! bytes are pure ASCII and quoted-printable when they contain non-ASCII —
//! the same rule SMTP servers expect.

use std::fmt;

use uuid::Uuid;

use crate::markdown_render::{MarkdownRenderError, render_markdown_to_email_html};

/// Maximum decoded length of the existing header block (everything up to
/// the first blank line) the assembler will accept. The Markdown body cap
/// is enforced by the renderer; this bounds only the header parsing pass.
const MAX_HEADER_BLOCK_BYTES: usize = 64 * 1024;

/// One attachment part recovered from a client-built `multipart/mixed`
/// request body. Stored verbatim and re-emitted as a base64-encoded
/// sibling of the inner `multipart/alternative` block.
#[derive(Debug, Clone)]
pub struct AttachmentPart {
    pub content_type: String,
    pub filename: String,
    pub data: Vec<u8>,
}

/// Errors returned by [`assemble_wire_message`].
#[derive(Debug)]
pub enum AssembleError {
    /// The request body's header block was malformed or absent.
    MissingHeaderSeparator,
    /// The header block exceeded [`MAX_HEADER_BLOCK_BYTES`].
    HeaderBlockTooLarge,
    /// The Markdown source exceeded the renderer cap.
    Render(MarkdownRenderError),
}

impl fmt::Display for AssembleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AssembleError::MissingHeaderSeparator => f.write_str(
                "outbound request body has no header/body separator (expected blank line)",
            ),
            AssembleError::HeaderBlockTooLarge => {
                f.write_str("outbound request header block is too large")
            }
            AssembleError::Render(e) => fmt::Display::fmt(e, f),
        }
    }
}

impl std::error::Error for AssembleError {}

impl From<MarkdownRenderError> for AssembleError {
    fn from(e: MarkdownRenderError) -> Self {
        AssembleError::Render(e)
    }
}

/// Render the request body into the final RFC 5322 message bytes ready
/// for DKIM signing. The original header block is preserved verbatim
/// **except** the `Content-Type:` and `Content-Transfer-Encoding:` headers
/// (and `MIME-Version`, normalized to `1.0`), which are rebuilt to match
/// the new MIME structure.
///
/// `request_body` is the raw bytes the daemon received over UDS — header
/// block + blank line + body section. The body section is either:
/// - bare body text (no `Content-Type: multipart/...` in the headers), or
/// - `multipart/mixed` with the first part `text/plain` (the body text)
///   followed by attachment parts.
///
/// `signature` is appended to the body text before rendering on the
/// default Markdown path. Pass an empty string to disable. On the
/// `text_only` and `html_body` paths the signature is **never** appended:
/// the operator picked the final shape and AIMX must not mutate it.
///
/// Branches:
/// - `text_only=false, html_body=None` → default Markdown render path.
/// - `text_only=true, html_body=None` → ship body verbatim as text/plain.
/// - `text_only=false, html_body=Some(html)` → ship body as text/plain
///   and the supplied HTML verbatim as text/html (multipart/alternative).
/// - `text_only=true, html_body=Some(_)` is rejected upstream by the
///   codec (`AIMX/1 SEND: --text-only and --html-body are mutually
///   exclusive`); this function treats the combination as the
///   `--html-body` shape but the codec ensures it never reaches us.
pub fn assemble_wire_message(
    request_body: &[u8],
    signature: &str,
    text_only: bool,
    html_body: Option<&[u8]>,
) -> Result<Vec<u8>, AssembleError> {
    let split = split_headers_body(request_body)?;
    let original_headers = split.headers;
    let body_section = split.body;

    let (body_text, attachments) =
        extract_markdown_and_attachments(&original_headers, body_section);

    let preserved_headers = strip_outgoing_content_headers(&original_headers);
    let outer = make_boundary();
    let inner = make_boundary();

    let body_bytes = if text_only {
        // Plain-text path: ship the body verbatim. With attachments,
        // wrap in multipart/mixed so the text part and attachments are
        // siblings (no inner alternative — there's no rich part).
        build_text_only_body(&body_text, &attachments, &outer)
    } else if let Some(html) = html_body {
        // Custom-HTML path: text/plain + supplied HTML verbatim. With
        // attachments, the alternative becomes the first child of an
        // outer multipart/mixed.
        let html_str = String::from_utf8_lossy(html).into_owned();
        build_multipart_body(&body_text, &html_str, &attachments, &outer, &inner)
    } else {
        // Default Markdown path: append signature, render to HTML,
        // then assemble multipart/alternative.
        let markdown_with_sig = append_signature_to_markdown(&body_text, signature);
        let html = render_markdown_to_email_html(&markdown_with_sig)?;
        build_multipart_body(&markdown_with_sig, &html, &attachments, &outer, &inner)
    };

    let content_headers = build_outgoing_content_headers_for(text_only, &attachments, &outer);

    let mut out =
        Vec::with_capacity(preserved_headers.len() + content_headers.len() + body_bytes.len() + 4);
    out.extend_from_slice(preserved_headers.as_bytes());
    out.extend_from_slice(content_headers.as_bytes());
    out.extend_from_slice(b"\r\n");
    out.extend_from_slice(&body_bytes);
    Ok(out)
}

struct HeaderBodySplit<'a> {
    headers: String,
    body: &'a [u8],
}

fn split_headers_body(input: &[u8]) -> Result<HeaderBodySplit<'_>, AssembleError> {
    let (sep_idx, sep_len) =
        find_header_separator(input).ok_or(AssembleError::MissingHeaderSeparator)?;
    if sep_idx > MAX_HEADER_BLOCK_BYTES {
        return Err(AssembleError::HeaderBlockTooLarge);
    }
    let headers_bytes = &input[..sep_idx];
    let headers = std::str::from_utf8(headers_bytes)
        .map_err(|_| AssembleError::MissingHeaderSeparator)?
        .to_string();
    let body = &input[sep_idx + sep_len..];
    Ok(HeaderBodySplit { headers, body })
}

fn find_header_separator(input: &[u8]) -> Option<(usize, usize)> {
    if let Some(idx) = find_subslice(input, b"\r\n\r\n") {
        return Some((idx, 4));
    }
    if let Some(idx) = find_subslice(input, b"\n\n") {
        return Some((idx, 2));
    }
    None
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() {
        return Some(0);
    }
    if needle.len() > haystack.len() {
        return None;
    }
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// If the headers carry `Content-Type: multipart/mixed`, parse the body
/// into (text/plain part bytes, attachment parts). Otherwise the entire
/// body is the Markdown source and no attachments exist.
fn extract_markdown_and_attachments(headers: &str, body: &[u8]) -> (String, Vec<AttachmentPart>) {
    let boundary = match extract_multipart_boundary(headers) {
        Some(b) => b,
        None => {
            return (decode_markdown_body(body), vec![]);
        }
    };

    let parts = split_multipart_body(body, &boundary);
    if parts.is_empty() {
        return (decode_markdown_body(body), vec![]);
    }

    let mut iter = parts.into_iter();
    let first = iter.next();
    let markdown_source = first.map(|p| decode_text_part(&p)).unwrap_or_default();

    let attachments: Vec<AttachmentPart> = iter.filter_map(|p| parse_attachment_part(&p)).collect();

    (markdown_source, attachments)
}

fn decode_markdown_body(body: &[u8]) -> String {
    String::from_utf8_lossy(body).into_owned()
}

#[derive(Debug)]
struct RawPart {
    headers: String,
    body: Vec<u8>,
}

fn split_multipart_body(body: &[u8], boundary: &str) -> Vec<RawPart> {
    let dash_boundary = format!("--{boundary}");
    let close_boundary = format!("--{boundary}--");
    let bytes = body;

    // Find each occurrence of `--<boundary>` at the start of a line.
    let occurrences: Vec<usize> = find_line_starts(bytes, dash_boundary.as_bytes());
    let mut parts = Vec::new();
    for window in occurrences.windows(2) {
        let start = window[0];
        let end = window[1];
        // Skip past the boundary line itself.
        let after_boundary_line = match find_subslice(&bytes[start..end], b"\n") {
            Some(nl) => start + nl + 1,
            None => continue,
        };
        if after_boundary_line >= end {
            continue;
        }
        let part_bytes = &bytes[after_boundary_line..end];
        // Trim trailing CRLF that precedes the next boundary line.
        let part_bytes = trim_trailing_crlf(part_bytes);
        // Stop at the closing boundary `--boundary--`.
        let line_at_end = &bytes[end..];
        let is_close = line_at_end.starts_with(close_boundary.as_bytes());
        if let Some(p) = parse_raw_part(part_bytes) {
            parts.push(p);
        }
        if is_close {
            break;
        }
    }
    // If the body has only one occurrence (no closing boundary), still try to parse it.
    if occurrences.len() == 1 {
        let start = occurrences[0];
        if let Some(nl_rel) = find_subslice(&bytes[start..], b"\n") {
            let after_boundary_line = start + nl_rel + 1;
            let part_bytes = trim_trailing_crlf(&bytes[after_boundary_line..]);
            if let Some(p) = parse_raw_part(part_bytes) {
                parts.push(p);
            }
        }
    }
    parts
}

fn find_line_starts(bytes: &[u8], needle: &[u8]) -> Vec<usize> {
    let mut out = Vec::new();
    let mut search_from = 0;
    while search_from < bytes.len() {
        let rel = match bytes[search_from..]
            .windows(needle.len())
            .position(|w| w == needle)
        {
            Some(r) => r,
            None => break,
        };
        let abs = search_from + rel;
        let at_start = abs == 0 || bytes[abs - 1] == b'\n';
        if at_start {
            out.push(abs);
        }
        search_from = abs + needle.len();
    }
    out
}

fn trim_trailing_crlf(bytes: &[u8]) -> &[u8] {
    let mut end = bytes.len();
    while end > 0 && (bytes[end - 1] == b'\n' || bytes[end - 1] == b'\r') {
        end -= 1;
    }
    &bytes[..end]
}

fn parse_raw_part(bytes: &[u8]) -> Option<RawPart> {
    let (sep, sep_len) = find_header_separator(bytes)?;
    let headers = std::str::from_utf8(&bytes[..sep]).ok()?.to_string();
    let body = bytes[sep + sep_len..].to_vec();
    Some(RawPart { headers, body })
}

fn decode_text_part(part: &RawPart) -> String {
    let encoding = header_value(&part.headers, "Content-Transfer-Encoding")
        .map(|s| s.trim().to_ascii_lowercase())
        .unwrap_or_default();
    let decoded: Vec<u8> = if encoding == "base64" {
        decode_base64(&part.body).unwrap_or_else(|| part.body.clone())
    } else if encoding == "quoted-printable" {
        decode_quoted_printable(&part.body)
    } else {
        part.body.clone()
    };
    let text = String::from_utf8_lossy(&decoded).into_owned();
    // Restore CRLF-normalized text to LF for renderer input — comrak is
    // line-ending agnostic but normalising here keeps test fixtures stable.
    text.replace("\r\n", "\n")
}

fn parse_attachment_part(part: &RawPart) -> Option<AttachmentPart> {
    let content_type = header_value(&part.headers, "Content-Type")
        .map(|v| split_param(&v).0)
        .unwrap_or_else(|| "application/octet-stream".to_string());
    let disposition = header_value(&part.headers, "Content-Disposition").unwrap_or_default();
    let filename = extract_filename(&disposition)
        .or_else(|| {
            header_value(&part.headers, "Content-Type").and_then(|v| extract_name_param(&v))
        })
        .unwrap_or_else(|| "attachment".to_string());

    let encoding = header_value(&part.headers, "Content-Transfer-Encoding")
        .map(|s| s.trim().to_ascii_lowercase())
        .unwrap_or_default();
    let data: Vec<u8> = if encoding == "base64" {
        decode_base64(&part.body).unwrap_or_else(|| part.body.clone())
    } else if encoding == "quoted-printable" {
        decode_quoted_printable(&part.body)
    } else {
        part.body.clone()
    };
    Some(AttachmentPart {
        content_type,
        filename,
        data,
    })
}

fn split_param(value: &str) -> (String, String) {
    let trimmed = value.trim();
    match trimmed.find(';') {
        Some(idx) => (
            trimmed[..idx].trim().to_string(),
            trimmed[idx + 1..].trim().to_string(),
        ),
        None => (trimmed.to_string(), String::new()),
    }
}

fn extract_filename(disposition: &str) -> Option<String> {
    extract_quoted_param(disposition, "filename")
}

fn extract_name_param(content_type: &str) -> Option<String> {
    extract_quoted_param(content_type, "name")
}

/// Extract a parameter value from a structured header value (e.g.
/// `Content-Type` / `Content-Disposition`). Parameter name lookup is
/// delimiter-aware: matches happen only against tokens that follow the
/// header value's primary value or another `;`-separated segment, so
/// asking for `name` will NOT spuriously match the `name=` substring
/// inside `filename=`. Parameter name comparison is case-insensitive.
/// Quoted values strip the surrounding `"` and stop at the closing `"`;
/// unquoted values run until the next `;` or whitespace.
fn extract_quoted_param(s: &str, name: &str) -> Option<String> {
    let lower_name = name.to_ascii_lowercase();
    // Skip the primary value (everything before the first `;`); parameters
    // start after that. If there is no `;`, there are no parameters.
    let after_primary = s.split_once(';')?.1;
    for segment in after_primary.split(';') {
        let segment = segment.trim();
        let (raw_name, raw_value) = match segment.split_once('=') {
            Some(pair) => pair,
            None => continue,
        };
        if !raw_name.trim().eq_ignore_ascii_case(&lower_name) {
            continue;
        }
        let value = raw_value.trim_start();
        if let Some(stripped) = value.strip_prefix('"') {
            let end = stripped.find('"')?;
            return Some(stripped[..end].to_string());
        }
        let end = value
            .find(|c: char| c == ';' || c.is_whitespace())
            .unwrap_or(value.len());
        if end == 0 {
            return None;
        }
        return Some(value[..end].to_string());
    }
    None
}

fn decode_base64(bytes: &[u8]) -> Option<Vec<u8>> {
    use base64::Engine;
    let s: String = std::str::from_utf8(bytes)
        .ok()?
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();
    base64::engine::general_purpose::STANDARD.decode(s).ok()
}

fn decode_quoted_printable(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b == b'=' && i + 2 < bytes.len() {
            // Soft line break: `=\r\n` or `=\n`.
            if bytes[i + 1] == b'\r' && bytes[i + 2] == b'\n' {
                i += 3;
                continue;
            }
            if bytes[i + 1] == b'\n' {
                i += 2;
                continue;
            }
            let h1 = (bytes[i + 1] as char).to_digit(16);
            let h2 = (bytes[i + 2] as char).to_digit(16);
            if let (Some(h1), Some(h2)) = (h1, h2) {
                out.push(((h1 << 4) | h2) as u8);
                i += 3;
                continue;
            }
        }
        out.push(b);
        i += 1;
    }
    out
}

/// Header value for `name` (case-insensitive), with continuation lines folded.
fn header_value(headers: &str, name: &str) -> Option<String> {
    let target = name.to_ascii_lowercase();
    let mut current: Option<String> = None;
    let mut found: Option<String> = None;
    for line in headers.lines() {
        if line.starts_with(' ') || line.starts_with('\t') {
            if let Some(cur) = current.as_mut() {
                cur.push(' ');
                cur.push_str(line.trim_start());
            }
            continue;
        }
        if let Some(cur) = current.take()
            && let Some((n, v)) = cur.split_once(':')
            && n.trim().eq_ignore_ascii_case(&target)
            && found.is_none()
        {
            found = Some(v.trim().to_string());
        }
        current = Some(line.to_string());
    }
    if found.is_none()
        && let Some(cur) = current
        && let Some((n, v)) = cur.split_once(':')
        && n.trim().eq_ignore_ascii_case(&target)
    {
        found = Some(v.trim().to_string());
    }
    found
}

fn extract_multipart_boundary(headers: &str) -> Option<String> {
    let value = header_value(headers, "Content-Type")?;
    let lower = value.to_ascii_lowercase();
    if !lower.contains("multipart/") {
        return None;
    }
    extract_quoted_param(&value, "boundary")
}

/// Strip `Content-Type`, `Content-Transfer-Encoding`, and `MIME-Version`
/// from the header block and return the remaining headers + a single
/// trailing CRLF. Header capitalization is preserved verbatim for everything
/// kept; only the recognized header names are dropped (case-insensitive).
fn strip_outgoing_content_headers(headers: &str) -> String {
    let mut out = String::with_capacity(headers.len() + 2);
    let mut keep_current = true;
    for line in headers.lines() {
        let is_continuation = line.starts_with(' ') || line.starts_with('\t');
        if !is_continuation {
            keep_current = match line.split_once(':') {
                Some((n, _)) => {
                    let lower = n.trim().to_ascii_lowercase();
                    !matches!(
                        lower.as_str(),
                        "content-type" | "content-transfer-encoding" | "mime-version"
                    )
                }
                None => true,
            };
        }
        if keep_current {
            out.push_str(line);
            out.push_str("\r\n");
        }
    }
    out
}

/// Build the outbound `Content-Type` (and `MIME-Version`) header lines
/// for the wire shape that matches `text_only` / `attachments`. Includes
/// the trailing CRLF on each line — caller appends a final blank CRLF
/// to terminate the header block.
///
/// The default Markdown path and the `--html-body` path share the
/// `multipart/alternative` (or `multipart/mixed` with attachments) shape,
/// so they collapse into the same branch here; only `text_only` shifts
/// the outer Content-Type away from `alternative`.
///
/// Shape table:
///
/// | text_only | attachments | Content-Type                |
/// |-----------|-------------|-----------------------------|
/// | false     | none        | multipart/alternative       |
/// | false     | some        | multipart/mixed             |
/// | true      | none        | text/plain (single-part)    |
/// | true      | some        | multipart/mixed             |
fn build_outgoing_content_headers_for(
    text_only: bool,
    attachments: &[AttachmentPart],
    outer_boundary: &str,
) -> String {
    let mut out = String::new();
    out.push_str("MIME-Version: 1.0\r\n");
    if text_only && attachments.is_empty() {
        // Single-part: pick the encoding the body actually needs so
        // SMTP servers don't re-wrap the part in transit.
        out.push_str("Content-Type: text/plain; charset=utf-8\r\n");
        // The transfer-encoding header for the single-part path is
        // emitted alongside the body inside `build_text_only_body` so
        // we don't have to know the body bytes here. Skip the header
        // line so we don't double-emit.
        return out;
    }
    if attachments.is_empty() {
        // text_only=false here (text_only with no attachments is the
        // single-part branch above). Both the default Markdown path
        // and the html_body path share the alternative shape.
        out.push_str(&format!(
            "Content-Type: multipart/alternative; boundary=\"{outer_boundary}\"\r\n"
        ));
    } else {
        out.push_str(&format!(
            "Content-Type: multipart/mixed; boundary=\"{outer_boundary}\"\r\n"
        ));
    }
    out
}

/// Build the wire body for the `--text-only` path. Two shapes:
/// - No attachments: single-part `text/plain` with body verbatim. The
///   transfer-encoding header is emitted as part of the body bytes so
///   the parent header-block builder doesn't have to inspect the body.
/// - With attachments: `multipart/mixed` whose first child is a
///   `text/plain` part carrying the body verbatim, and remaining children
///   are the attachments. There is no inner alternative — text-only
///   means there is no rich part.
fn build_text_only_body(body_text: &str, attachments: &[AttachmentPart], outer: &str) -> Vec<u8> {
    let normalized = normalize_text_to_crlf(body_text);
    let encoding = pick_transfer_encoding(normalized.as_bytes());
    let encoded = encode_text_body(&normalized, encoding);

    if attachments.is_empty() {
        // Single-part: the parent header builder already emitted
        // `Content-Type: text/plain; charset=utf-8`. We add the
        // transfer-encoding header here, then a blank line, then the
        // body. The blank line that separates the message header
        // block from the body comes from the parent.
        let mut out = Vec::new();
        out.extend_from_slice(
            format!("Content-Transfer-Encoding: {}\r\n\r\n", encoding.label()).as_bytes(),
        );
        out.extend_from_slice(encoded.as_bytes());
        if !encoded.ends_with('\n') {
            out.extend_from_slice(b"\r\n");
        }
        return out;
    }

    let mut out = Vec::new();
    out.extend_from_slice(format!("--{outer}\r\n").as_bytes());
    out.extend_from_slice(b"Content-Type: text/plain; charset=utf-8\r\n");
    out.extend_from_slice(
        format!("Content-Transfer-Encoding: {}\r\n\r\n", encoding.label()).as_bytes(),
    );
    out.extend_from_slice(encoded.as_bytes());
    out.extend_from_slice(b"\r\n");

    for att in attachments {
        out.extend_from_slice(format!("--{outer}\r\n").as_bytes());
        let safe_name = escape_filename(&att.filename);
        out.extend_from_slice(
            format!(
                "Content-Type: {}; name=\"{safe_name}\"\r\n",
                att.content_type
            )
            .as_bytes(),
        );
        out.extend_from_slice(
            format!("Content-Disposition: attachment; filename=\"{safe_name}\"\r\n").as_bytes(),
        );
        out.extend_from_slice(b"Content-Transfer-Encoding: base64\r\n\r\n");
        let encoded_att = base64_encode_wrapped(&att.data, 76);
        out.extend_from_slice(encoded_att.as_bytes());
        out.extend_from_slice(b"\r\n");
    }
    out.extend_from_slice(format!("--{outer}--\r\n").as_bytes());
    out
}

/// Build the wire body. Two shapes:
/// - No attachments: `multipart/alternative` (text + html). The `outer`
///   parameter doubles as the alternative boundary; `inner` is unused.
/// - With attachments: `multipart/mixed` whose first child is the
///   `multipart/alternative` and whose remaining children are the
///   attachments. `outer` is the mixed boundary; `inner` is the alternative
///   boundary. The two are independent UUIDs.
fn build_multipart_body(
    markdown_source: &str,
    html: &str,
    attachments: &[AttachmentPart],
    outer: &str,
    inner: &str,
) -> Vec<u8> {
    if attachments.is_empty() {
        return build_alternative_body(outer, markdown_source, html);
    }

    let mut out = Vec::new();
    out.extend_from_slice(format!("--{outer}\r\n").as_bytes());
    out.extend_from_slice(
        format!("Content-Type: multipart/alternative; boundary=\"{inner}\"\r\n\r\n").as_bytes(),
    );
    out.extend_from_slice(&build_alternative_body(inner, markdown_source, html));
    out.extend_from_slice(b"\r\n");

    for att in attachments {
        out.extend_from_slice(format!("--{outer}\r\n").as_bytes());
        let safe_name = escape_filename(&att.filename);
        out.extend_from_slice(
            format!(
                "Content-Type: {}; name=\"{safe_name}\"\r\n",
                att.content_type
            )
            .as_bytes(),
        );
        out.extend_from_slice(
            format!("Content-Disposition: attachment; filename=\"{safe_name}\"\r\n").as_bytes(),
        );
        out.extend_from_slice(b"Content-Transfer-Encoding: base64\r\n\r\n");
        let encoded = base64_encode_wrapped(&att.data, 76);
        out.extend_from_slice(encoded.as_bytes());
        out.extend_from_slice(b"\r\n");
    }
    out.extend_from_slice(format!("--{outer}--\r\n").as_bytes());
    out
}

fn build_alternative_body(boundary: &str, markdown_source: &str, html: &str) -> Vec<u8> {
    let text_normalized = normalize_text_to_crlf(markdown_source);
    let html_normalized = normalize_text_to_crlf(html);
    let text_encoding = pick_transfer_encoding(text_normalized.as_bytes());
    let html_encoding = pick_transfer_encoding(html_normalized.as_bytes());

    let text_body = encode_text_body(&text_normalized, text_encoding);
    let html_body = encode_text_body(&html_normalized, html_encoding);

    let mut out = Vec::new();
    out.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
    out.extend_from_slice(b"Content-Type: text/plain; charset=utf-8\r\n");
    out.extend_from_slice(
        format!(
            "Content-Transfer-Encoding: {}\r\n\r\n",
            text_encoding.label()
        )
        .as_bytes(),
    );
    out.extend_from_slice(text_body.as_bytes());
    out.extend_from_slice(b"\r\n");

    out.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
    out.extend_from_slice(b"Content-Type: text/html; charset=utf-8\r\n");
    out.extend_from_slice(
        format!(
            "Content-Transfer-Encoding: {}\r\n\r\n",
            html_encoding.label()
        )
        .as_bytes(),
    );
    out.extend_from_slice(html_body.as_bytes());
    out.extend_from_slice(b"\r\n");

    out.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
    out
}

#[derive(Copy, Clone)]
enum TextTransferEncoding {
    SevenBit,
    QuotedPrintable,
}

impl TextTransferEncoding {
    fn label(self) -> &'static str {
        match self {
            TextTransferEncoding::SevenBit => "7bit",
            TextTransferEncoding::QuotedPrintable => "quoted-printable",
        }
    }
}

fn pick_transfer_encoding(bytes: &[u8]) -> TextTransferEncoding {
    if bytes.iter().all(|b| *b < 128) && !has_long_line(bytes) {
        TextTransferEncoding::SevenBit
    } else {
        TextTransferEncoding::QuotedPrintable
    }
}

fn has_long_line(bytes: &[u8]) -> bool {
    let mut col = 0usize;
    for &b in bytes {
        if b == b'\n' {
            col = 0;
        } else if b != b'\r' {
            col += 1;
            if col > 998 {
                return true;
            }
        }
    }
    false
}

fn encode_text_body(text: &str, enc: TextTransferEncoding) -> String {
    match enc {
        TextTransferEncoding::SevenBit => text.to_string(),
        TextTransferEncoding::QuotedPrintable => quoted_printable_encode(text),
    }
}

fn quoted_printable_encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut col = 0usize;
    for &b in text.as_bytes() {
        if b == b'\n' {
            out.push('\n');
            col = 0;
            continue;
        }
        if b == b'\r' {
            // Carry the CR through; the LF will reset the column.
            out.push('\r');
            continue;
        }
        let needs_encoding = b == b'=' || b == b'\t' || !(0x20..=0x7e).contains(&b);
        if needs_encoding {
            if col + 3 > 75 {
                out.push_str("=\r\n");
                col = 0;
            }
            out.push_str(&format!("={b:02X}"));
            col += 3;
        } else {
            if col + 1 > 75 {
                out.push_str("=\r\n");
                col = 0;
            }
            out.push(b as char);
            col += 1;
        }
    }
    out
}

fn normalize_text_to_crlf(text: &str) -> String {
    text.replace("\r\n", "\n")
        .replace('\r', "\n")
        .replace('\n', "\r\n")
}

fn base64_encode_wrapped(data: &[u8], line_width: usize) -> String {
    use base64::Engine;
    let raw = base64::engine::general_purpose::STANDARD.encode(data);
    let mut out = String::with_capacity(raw.len() + raw.len() / line_width + 1);
    for (i, ch) in raw.chars().enumerate() {
        if i > 0 && i.is_multiple_of(line_width) {
            out.push_str("\r\n");
        }
        out.push(ch);
    }
    out
}

fn escape_filename(name: &str) -> String {
    name.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\r', "")
        .replace('\n', " ")
}

fn make_boundary() -> String {
    format!("aimx-{}", Uuid::new_v4().simple())
}

/// Append `signature` to `markdown` separated by a blank line. Empty
/// signature returns the markdown verbatim. Any trailing whitespace on
/// the signature is preserved as-is.
fn append_signature_to_markdown(markdown: &str, signature: &str) -> String {
    if signature.is_empty() {
        return markdown.to_string();
    }
    let normalized_sig = signature.replace("\r\n", "\n").replace('\r', "\n");
    let trimmed_md = markdown.trim_end_matches(['\r', '\n']);
    let mut out = String::with_capacity(trimmed_md.len() + normalized_sig.len() + 4);
    out.push_str(trimmed_md);
    out.push_str("\n\n");
    out.push_str(&normalized_sig);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header_only(headers: &str) -> Vec<u8> {
        let mut v: Vec<u8> = headers.replace('\n', "\r\n").into_bytes();
        v.extend_from_slice(b"\r\n");
        v
    }

    fn build_request(headers: &str, body: &str) -> Vec<u8> {
        let mut v = header_only(headers);
        v.extend_from_slice(body.as_bytes());
        v
    }

    fn parse_content_type(out: &[u8]) -> String {
        let text = String::from_utf8_lossy(out);
        for line in text.lines() {
            if line.is_empty() {
                break;
            }
            if let Some(v) = line.strip_prefix("Content-Type:") {
                return v.trim().to_string();
            }
        }
        String::new()
    }

    fn extract_boundary_from_ct(ct: &str) -> String {
        extract_quoted_param(ct, "boundary").expect("Content-Type missing boundary")
    }

    #[test]
    fn header_separator_required() {
        let err =
            assemble_wire_message(b"From: a@b.com\r\nNo separator", "", false, None).unwrap_err();
        assert!(matches!(err, AssembleError::MissingHeaderSeparator));
    }

    #[test]
    fn default_path_emits_multipart_alternative() {
        let req = build_request(
            "From: alice@example.com\nTo: bob@x.com\nSubject: Hi\n",
            "# Hello\n\nWorld\n",
        );
        let out = assemble_wire_message(&req, "", false, None).unwrap();
        let text = String::from_utf8_lossy(&out);
        assert!(text.contains("MIME-Version: 1.0\r\n"));
        assert!(text.contains("Content-Type: multipart/alternative; boundary=\"aimx-"));
        assert!(text.contains("Content-Type: text/plain; charset=utf-8"));
        assert!(text.contains("Content-Type: text/html; charset=utf-8"));
        // text part precedes html part per RFC 1341
        let text_pos = text.find("text/plain").unwrap();
        let html_pos = text.find("text/html").unwrap();
        assert!(text_pos < html_pos);
        // Markdown source verbatim in text part.
        assert!(text.contains("# Hello\r\n\r\nWorld"));
        // Rendered HTML in html part.
        assert!(text.contains("<h1"));
        assert!(text.contains(">Hello</h1>"));
    }

    #[test]
    fn boundary_is_aimx_uuid() {
        let req = build_request("From: a@b.com\nTo: c@d.com\n", "hi");
        let out = assemble_wire_message(&req, "", false, None).unwrap();
        let ct = parse_content_type(&out);
        assert!(ct.starts_with("multipart/alternative"), "{ct}");
        let boundary = extract_boundary_from_ct(&ct);
        assert!(boundary.starts_with("aimx-"));
    }

    #[test]
    fn signature_is_appended_to_markdown_before_render() {
        // Use ASCII-only signature so the text part stays 7bit and the
        // assertion can match the literal bytes. The non-ASCII case is
        // covered by `quoted_printable_encodes_non_ascii_text`.
        let req = build_request("From: a@b.com\nTo: c@d.com\n", "Body line.");
        let out =
            assemble_wire_message(&req, "-- [aimx](https://aimx.email)", false, None).unwrap();
        let text = String::from_utf8_lossy(&out);
        // text part contains the literal Markdown signature
        assert!(
            text.contains("-- [aimx](https://aimx.email)"),
            "text part missing signature: {text}"
        );
        // html part contains the rendered link
        assert!(
            text.contains("href=\"https://aimx.email\""),
            "html part missing rendered link: {text}"
        );
    }

    #[test]
    fn empty_signature_appends_nothing() {
        let body = "User body.\n";
        let req = build_request("From: a@b.com\nTo: c@d.com\n", body);
        let out = assemble_wire_message(&req, "", false, None).unwrap();
        let text = String::from_utf8_lossy(&out);
        // The text part should be exactly the user body (modulo CRLF).
        // No "— " or extra trailing lines.
        assert!(!text.contains("— "));
        assert!(text.contains("User body."));
    }

    #[test]
    fn multipart_request_extracts_text_and_attachments() {
        let body = concat!(
            "--BND\r\n",
            "Content-Type: text/plain; charset=utf-8\r\n",
            "\r\n",
            "# Heading\r\n\r\n",
            "Body text.\r\n",
            "--BND\r\n",
            "Content-Type: application/pdf; name=\"doc.pdf\"\r\n",
            "Content-Disposition: attachment; filename=\"doc.pdf\"\r\n",
            "Content-Transfer-Encoding: base64\r\n",
            "\r\n",
            "UERG\r\n",
            "--BND--\r\n",
        );
        let req = build_request(
            "From: a@b.com\nTo: c@d.com\nSubject: T\nMIME-Version: 1.0\nContent-Type: multipart/mixed; boundary=\"BND\"\n",
            body,
        );
        let out = assemble_wire_message(&req, "", false, None).unwrap();
        let text = String::from_utf8_lossy(&out);
        // Outer is multipart/mixed
        assert!(text.contains("Content-Type: multipart/mixed; boundary=\"aimx-"));
        // Inner is multipart/alternative
        assert!(text.contains("Content-Type: multipart/alternative; boundary=\"aimx-"));
        // Markdown source preserved in text part
        assert!(text.contains("# Heading"));
        // HTML render present
        assert!(text.contains("<h1"));
        // Attachment is preserved as a sibling
        assert!(text.contains("filename=\"doc.pdf\""));
        assert!(text.contains("UERG"));
    }

    #[test]
    fn outer_and_inner_boundaries_differ() {
        let body = concat!(
            "--BND\r\n",
            "Content-Type: text/plain; charset=utf-8\r\n",
            "\r\n",
            "x\r\n",
            "--BND\r\n",
            "Content-Type: application/octet-stream; name=\"a.bin\"\r\n",
            "Content-Disposition: attachment; filename=\"a.bin\"\r\n",
            "Content-Transfer-Encoding: base64\r\n",
            "\r\n",
            "QUFB\r\n",
            "--BND--\r\n",
        );
        let req = build_request(
            "From: a@b.com\nTo: c@d.com\nContent-Type: multipart/mixed; boundary=\"BND\"\n",
            body,
        );
        let out = assemble_wire_message(&req, "", false, None).unwrap();
        let text = String::from_utf8_lossy(&out);
        let outer_ct = text
            .lines()
            .find(|l| l.starts_with("Content-Type: multipart/mixed"))
            .unwrap();
        let outer_boundary = extract_quoted_param(outer_ct, "boundary").unwrap();
        let inner_ct = text
            .lines()
            .find(|l| l.contains("multipart/alternative"))
            .unwrap();
        let inner_boundary = extract_quoted_param(inner_ct, "boundary").unwrap();
        assert_ne!(outer_boundary, inner_boundary);
    }

    #[test]
    fn three_attachments_appear_as_siblings() {
        let body = concat!(
            "--BND\r\n",
            "Content-Type: text/plain; charset=utf-8\r\n",
            "\r\n",
            "Body.\r\n",
            "--BND\r\n",
            "Content-Type: application/pdf; name=\"a.pdf\"\r\n",
            "Content-Disposition: attachment; filename=\"a.pdf\"\r\n",
            "Content-Transfer-Encoding: base64\r\n",
            "\r\n",
            "QQ==\r\n",
            "--BND\r\n",
            "Content-Type: image/png; name=\"b.png\"\r\n",
            "Content-Disposition: attachment; filename=\"b.png\"\r\n",
            "Content-Transfer-Encoding: base64\r\n",
            "\r\n",
            "Qg==\r\n",
            "--BND\r\n",
            "Content-Type: text/csv; name=\"c.csv\"\r\n",
            "Content-Disposition: attachment; filename=\"c.csv\"\r\n",
            "Content-Transfer-Encoding: base64\r\n",
            "\r\n",
            "Qw==\r\n",
            "--BND--\r\n",
        );
        let req = build_request(
            "From: a@b.com\nTo: c@d.com\nContent-Type: multipart/mixed; boundary=\"BND\"\n",
            body,
        );
        let out = assemble_wire_message(&req, "", false, None).unwrap();
        let text = String::from_utf8_lossy(&out);
        assert!(text.contains("filename=\"a.pdf\""));
        assert!(text.contains("filename=\"b.png\""));
        assert!(text.contains("filename=\"c.csv\""));
        // Outer boundary count: 1 alt + 3 attachments + 1 close = 5 occurrences
        let outer_ct = text
            .lines()
            .find(|l| l.starts_with("Content-Type: multipart/mixed"))
            .unwrap();
        let outer_boundary = extract_quoted_param(outer_ct, "boundary").unwrap();
        let outer_marker = format!("--{outer_boundary}");
        let count = text.matches(&outer_marker).count();
        assert_eq!(count, 5, "expected 5 outer-boundary markers, got {count}");
    }

    #[test]
    fn mail_parser_roundtrip_default_path() {
        let req = build_request(
            "From: alice@example.com\nTo: bob@x.com\nSubject: Hi\n",
            "# Heading\n\nbody\n",
        );
        let out = assemble_wire_message(&req, "", false, None).unwrap();
        let parsed = mail_parser::MessageParser::default()
            .parse(&out[..])
            .expect("must parse");
        let text = parsed.body_text(0).expect("text part");
        let html = parsed.body_html(0).expect("html part");
        assert!(text.contains("# Heading"));
        assert!(html.contains("<h1"));
    }

    #[test]
    fn mail_parser_roundtrip_with_attachment() {
        let body = concat!(
            "--BND\r\n",
            "Content-Type: text/plain; charset=utf-8\r\n",
            "\r\n",
            "Body.\r\n",
            "--BND\r\n",
            "Content-Type: application/pdf; name=\"doc.pdf\"\r\n",
            "Content-Disposition: attachment; filename=\"doc.pdf\"\r\n",
            "Content-Transfer-Encoding: base64\r\n",
            "\r\n",
            "UERG\r\n",
            "--BND--\r\n",
        );
        let req = build_request(
            "From: a@b.com\nTo: c@d.com\nContent-Type: multipart/mixed; boundary=\"BND\"\n",
            body,
        );
        let out = assemble_wire_message(&req, "", false, None).unwrap();
        let parsed = mail_parser::MessageParser::default()
            .parse(&out[..])
            .expect("must parse");
        assert!(parsed.body_text(0).is_some());
        assert!(parsed.body_html(0).is_some());
        let attachments: Vec<_> = parsed.attachments().collect();
        assert_eq!(attachments.len(), 1);
    }

    #[test]
    fn original_headers_are_preserved() {
        let req = build_request(
            "From: alice@example.com\nTo: bob@x.com\nSubject: Hi\nMessage-ID: <abc@x>\nDate: Thu, 1 Jan 2026 00:00:00 +0000\n",
            "Hi.",
        );
        let out = assemble_wire_message(&req, "", false, None).unwrap();
        let text = String::from_utf8_lossy(&out);
        assert!(text.contains("From: alice@example.com\r\n"));
        assert!(text.contains("To: bob@x.com\r\n"));
        assert!(text.contains("Subject: Hi\r\n"));
        assert!(text.contains("Message-ID: <abc@x>\r\n"));
        assert!(text.contains("Date: Thu, 1 Jan 2026 00:00:00 +0000\r\n"));
    }

    #[test]
    fn signature_with_link_renders_clickable_in_html_part() {
        let req = build_request("From: a@b.com\nTo: c@d.com\n", "Body.");
        let out =
            assemble_wire_message(&req, "-- [aimx](https://aimx.email)", false, None).unwrap();
        let text = String::from_utf8_lossy(&out);
        assert!(text.contains("href=\"https://aimx.email\""));
        // The literal Markdown signature must be visible in the text part.
        assert!(text.contains("[aimx](https://aimx.email)"));
    }

    #[test]
    fn quoted_printable_encodes_non_ascii_text() {
        let req = build_request("From: a@b.com\nTo: c@d.com\n", "Héllo");
        let out = assemble_wire_message(&req, "", false, None).unwrap();
        let text = String::from_utf8_lossy(&out);
        assert!(text.contains("Content-Transfer-Encoding: quoted-printable"));
        // 0xC3 0xA9 = é
        assert!(text.contains("H=C3=A9llo"));
    }

    #[test]
    fn seven_bit_encoding_for_ascii_only() {
        let req = build_request("From: a@b.com\nTo: c@d.com\n", "Plain.");
        let out = assemble_wire_message(&req, "", false, None).unwrap();
        let text = String::from_utf8_lossy(&out);
        assert!(text.contains("Content-Transfer-Encoding: 7bit"));
    }

    #[test]
    fn extract_quoted_param_does_not_match_name_inside_filename() {
        // Regression: `name=` is a substring of `filename=`. A plain
        // `find()` lookup would incorrectly return `"doc.pdf"` here.
        let header = "application/pdf; filename=\"doc.pdf\"";
        assert_eq!(extract_quoted_param(header, "name"), None);
        assert_eq!(
            extract_quoted_param(header, "filename"),
            Some("doc.pdf".to_string())
        );
    }

    #[test]
    fn extract_quoted_param_finds_explicit_name_alongside_filename() {
        // When both params are present each lookup returns its own value.
        let header = "application/pdf; name=\"display.pdf\"; filename=\"stored.pdf\"";
        assert_eq!(
            extract_quoted_param(header, "name"),
            Some("display.pdf".to_string())
        );
        assert_eq!(
            extract_quoted_param(header, "filename"),
            Some("stored.pdf".to_string())
        );
    }

    #[test]
    fn extract_quoted_param_handles_unquoted_values() {
        // The `else` arm of the parser: unquoted values (RFC 2045 token).
        let header = "text/plain; charset=utf-8";
        assert_eq!(
            extract_quoted_param(header, "charset"),
            Some("utf-8".to_string())
        );
        // Unquoted with a trailing parameter — value stops at `;`.
        let header = "text/plain; charset=utf-8; format=flowed";
        assert_eq!(
            extract_quoted_param(header, "charset"),
            Some("utf-8".to_string())
        );
        assert_eq!(
            extract_quoted_param(header, "format"),
            Some("flowed".to_string())
        );
    }

    #[test]
    fn extract_quoted_param_is_case_insensitive_on_param_name() {
        let header = "application/pdf; FileName=\"doc.pdf\"";
        assert_eq!(
            extract_quoted_param(header, "filename"),
            Some("doc.pdf".to_string())
        );
    }

    #[test]
    fn extract_name_from_content_type_with_only_filename_returns_none() {
        // End-to-end sanity for the bug the reviewer flagged: if a future
        // caller asks `extract_name_param` on a Content-Type that carries
        // only a filename-like param, it must return None — not the
        // filename value.
        let ct = "application/pdf; filename=\"doc.pdf\"";
        assert_eq!(extract_name_param(ct), None);
    }

    #[test]
    fn body_too_large_propagates_canonical_message_through_assembler() {
        // The renderer caps the Markdown source at MAX_MARKDOWN_BODY_BYTES
        // (5MB). When `assemble_wire_message` hands the body to the
        // renderer the `BodyTooLarge` error must propagate as
        // `AssembleError::Render(...)` and its `Display` output must
        // carry the canonical actionable message verbatim, so the wire
        // `ERR BODY_TOO_LARGE <reason>` reaches the operator with the
        // actionable hint intact (the dedicated code is mapped from this
        // variant in `send_handler::handle_send_with_signer`).
        use crate::markdown_render::{MAX_MARKDOWN_BODY_BYTES, MarkdownRenderError};
        let oversize = "a".repeat(MAX_MARKDOWN_BODY_BYTES + 1);
        let req = build_request("From: a@b.com\nTo: c@d.com\n", &oversize);
        let err = assemble_wire_message(&req, "", false, None).expect_err("expected BodyTooLarge");
        assert!(
            matches!(
                err,
                AssembleError::Render(MarkdownRenderError::BodyTooLarge)
            ),
            "expected AssembleError::Render(BodyTooLarge), got {err:?}"
        );
        let rendered = err.to_string();
        assert_eq!(
            rendered,
            "markdown body exceeds 5MB; use --html-body for pre-rendered large documents or --attachment for sending the document as a file",
            "canonical BodyTooLarge message must reach the wire reason field verbatim"
        );
    }

    // ------------------------------------------------------------------
    // Escape-hatch branches (--text-only and --html-body)
    // ------------------------------------------------------------------

    /// `--text-only` with no attachments: single-part `text/plain`,
    /// body verbatim, no boundary markers, no rendered HTML.
    #[test]
    fn text_only_no_attachments_emits_single_part_text_plain() {
        let req = build_request("From: a@b.com\nTo: c@d.com\n", "Your code: 9999");
        let out = assemble_wire_message(&req, "ignored-sig", true, None).unwrap();
        let text = String::from_utf8_lossy(&out);
        assert!(text.contains("Content-Type: text/plain; charset=utf-8"));
        assert!(text.contains("Content-Transfer-Encoding: 7bit"));
        // No multipart wrapping of any kind.
        assert!(!text.contains("multipart/"), "{text}");
        // No boundary markers (`--aimx-...`).
        assert!(!text.contains("--aimx-"), "{text}");
        // No rendered HTML.
        assert!(!text.contains("<h1"), "{text}");
        assert!(text.contains("Your code: 9999"));
        // The signature is NOT appended on the text-only path even when
        // the caller passed one.
        assert!(!text.contains("ignored-sig"), "{text}");
    }

    /// `--text-only` with one attachment: `multipart/mixed` whose first
    /// child is the text part, second is the attachment. No inner
    /// alternative — text-only has no rich part.
    #[test]
    fn text_only_with_attachment_wraps_in_multipart_mixed() {
        let body = concat!(
            "--BND\r\n",
            "Content-Type: text/plain; charset=utf-8\r\n",
            "\r\n",
            "Body verbatim.\r\n",
            "--BND\r\n",
            "Content-Type: application/pdf; name=\"doc.pdf\"\r\n",
            "Content-Disposition: attachment; filename=\"doc.pdf\"\r\n",
            "Content-Transfer-Encoding: base64\r\n",
            "\r\n",
            "UERG\r\n",
            "--BND--\r\n",
        );
        let req = build_request(
            "From: a@b.com\nTo: c@d.com\nContent-Type: multipart/mixed; boundary=\"BND\"\n",
            body,
        );
        let out = assemble_wire_message(&req, "", true, None).unwrap();
        let text = String::from_utf8_lossy(&out);
        // Outer mixed
        assert!(text.contains("Content-Type: multipart/mixed; boundary=\"aimx-"));
        // No multipart/alternative — text-only never wraps an alternative
        assert!(!text.contains("multipart/alternative"), "{text}");
        // Text part present
        assert!(text.contains("Content-Type: text/plain"));
        assert!(text.contains("Body verbatim."));
        // Attachment preserved
        assert!(text.contains("filename=\"doc.pdf\""));
        // No rendered HTML
        assert!(!text.contains("<h1"), "{text}");
    }

    /// `--text-only` with three attachments: outer `multipart/mixed`
    /// whose first child is the single text part, then each attachment
    /// as a sibling. No inner `multipart/alternative` and no rendered
    /// HTML — text-only never produces a rich part regardless of how
    /// many attachments ride alongside it. Mirrors the default-path
    /// `three_attachments_appear_as_siblings` coverage so the
    /// attachments-loop on the text-only branch is exercised at >1.
    #[test]
    fn text_only_with_multiple_attachments_wraps_in_multipart_mixed() {
        let body = concat!(
            "--BND\r\n",
            "Content-Type: text/plain; charset=utf-8\r\n",
            "\r\n",
            "Body verbatim.\r\n",
            "--BND\r\n",
            "Content-Type: application/pdf; name=\"a.pdf\"\r\n",
            "Content-Disposition: attachment; filename=\"a.pdf\"\r\n",
            "Content-Transfer-Encoding: base64\r\n",
            "\r\n",
            "QQ==\r\n",
            "--BND\r\n",
            "Content-Type: image/png; name=\"b.png\"\r\n",
            "Content-Disposition: attachment; filename=\"b.png\"\r\n",
            "Content-Transfer-Encoding: base64\r\n",
            "\r\n",
            "Qg==\r\n",
            "--BND\r\n",
            "Content-Type: text/csv; name=\"c.csv\"\r\n",
            "Content-Disposition: attachment; filename=\"c.csv\"\r\n",
            "Content-Transfer-Encoding: base64\r\n",
            "\r\n",
            "Qw==\r\n",
            "--BND--\r\n",
        );
        let req = build_request(
            "From: a@b.com\nTo: c@d.com\nContent-Type: multipart/mixed; boundary=\"BND\"\n",
            body,
        );
        let out = assemble_wire_message(&req, "ignored-sig", true, None).unwrap();
        let text = String::from_utf8_lossy(&out);

        // Outer wrap is multipart/mixed; no inner alternative; no HTML
        // rendering; signature suppressed.
        assert!(text.contains("Content-Type: multipart/mixed; boundary=\"aimx-"));
        assert!(!text.contains("multipart/alternative"), "{text}");
        assert!(!text.contains("text/html"), "{text}");
        assert!(!text.contains("<h1"), "{text}");
        assert!(!text.contains("ignored-sig"), "{text}");

        // Single text part carries the body verbatim.
        assert!(text.contains("Content-Type: text/plain; charset=utf-8"));
        assert!(text.contains("Body verbatim."));

        // All three attachments preserved as siblings.
        assert!(text.contains("filename=\"a.pdf\""));
        assert!(text.contains("filename=\"b.png\""));
        assert!(text.contains("filename=\"c.csv\""));

        // Boundary marker count: 1 text part + 3 attachments + 1 close
        // = 5 occurrences of the outer boundary marker. Same invariant
        // as `three_attachments_appear_as_siblings` for the default
        // path; here we additionally know there is no inner boundary
        // since the alternative is suppressed.
        let outer_ct = text
            .lines()
            .find(|l| l.starts_with("Content-Type: multipart/mixed"))
            .unwrap();
        let outer_boundary = extract_quoted_param(outer_ct, "boundary").unwrap();
        let outer_marker = format!("--{outer_boundary}");
        let count = text.matches(&outer_marker).count();
        assert_eq!(count, 5, "expected 5 outer-boundary markers, got {count}");
    }

    /// `--html-body` with no attachments: `multipart/alternative` with
    /// the body as text/plain and the supplied HTML verbatim. The
    /// renderer must NOT touch the HTML — assert no inlined `style="`
    /// attributes appear that the renderer would have added.
    #[test]
    fn html_body_no_attachments_emits_multipart_alternative_with_verbatim_html() {
        let custom_html = b"<h1>x</h1>";
        let req = build_request("From: a@b.com\nTo: c@d.com\n", "fallback text.");
        let out = assemble_wire_message(&req, "ignored-sig", false, Some(custom_html)).unwrap();
        let text = String::from_utf8_lossy(&out);
        assert!(text.contains("Content-Type: multipart/alternative; boundary=\"aimx-"));
        assert!(text.contains("Content-Type: text/plain; charset=utf-8"));
        assert!(text.contains("Content-Type: text/html; charset=utf-8"));
        // text part = body verbatim
        assert!(text.contains("fallback text."));
        // HTML part = supplied bytes verbatim
        assert!(text.contains("<h1>x</h1>"));
        // No render styling injected (the renderer would add `style="...`)
        let html_after = text
            .find("text/html")
            .and_then(|i| text.get(i..))
            .unwrap_or("");
        assert!(
            !html_after.contains("style=\""),
            "renderer must not be invoked on --html-body path: {html_after}"
        );
        // Signature is not appended on the html-body path either.
        assert!(!text.contains("ignored-sig"), "{text}");
    }

    /// `--html-body` with attachments: outer `multipart/mixed` wrapping
    /// the alternative + each attachment as a sibling. Same shape as
    /// the default path with attachments.
    #[test]
    fn html_body_with_attachment_wraps_alternative_in_multipart_mixed() {
        let body = concat!(
            "--BND\r\n",
            "Content-Type: text/plain; charset=utf-8\r\n",
            "\r\n",
            "fallback text.\r\n",
            "--BND\r\n",
            "Content-Type: application/pdf; name=\"a.pdf\"\r\n",
            "Content-Disposition: attachment; filename=\"a.pdf\"\r\n",
            "Content-Transfer-Encoding: base64\r\n",
            "\r\n",
            "QQ==\r\n",
            "--BND--\r\n",
        );
        let req = build_request(
            "From: a@b.com\nTo: c@d.com\nContent-Type: multipart/mixed; boundary=\"BND\"\n",
            body,
        );
        let out = assemble_wire_message(&req, "", false, Some(b"<p>custom</p>")).unwrap();
        let text = String::from_utf8_lossy(&out);
        assert!(text.contains("Content-Type: multipart/mixed; boundary=\"aimx-"));
        assert!(text.contains("Content-Type: multipart/alternative; boundary=\"aimx-"));
        assert!(text.contains("<p>custom</p>"));
        assert!(text.contains("filename=\"a.pdf\""));
        // Outer and inner boundaries differ.
        let outer_ct = text
            .lines()
            .find(|l| l.starts_with("Content-Type: multipart/mixed"))
            .unwrap();
        let outer_boundary = extract_quoted_param(outer_ct, "boundary").unwrap();
        let inner_ct = text
            .lines()
            .find(|l| l.contains("multipart/alternative"))
            .unwrap();
        let inner_boundary = extract_quoted_param(inner_ct, "boundary").unwrap();
        assert_ne!(outer_boundary, inner_boundary);
    }

    /// The default Markdown path still appends the signature; this
    /// regression-tests that the new branching didn't break the
    /// existing signature-into-Markdown-before-render contract.
    #[test]
    fn default_path_still_appends_signature() {
        let req = build_request("From: a@b.com\nTo: c@d.com\n", "Body.");
        let out = assemble_wire_message(&req, "MARKER-SIG", false, None).unwrap();
        let text = String::from_utf8_lossy(&out);
        assert!(text.contains("MARKER-SIG"));
    }

    /// mail-parser round-trip on the `--text-only` single-part path:
    /// the message parses, the body text is preserved verbatim, and
    /// the wire shape carries no `Content-Type: text/html` header (no
    /// HTML alternative was emitted). mail-parser's `body_html(0)`
    /// helper falls back to the text part on a single-part text/plain
    /// message, so we assert against the wire bytes rather than the
    /// parser's html-fallback view.
    #[test]
    fn mail_parser_roundtrip_text_only_single_part() {
        let req = build_request("From: a@b.com\nTo: c@d.com\n", "Plain body.");
        let out = assemble_wire_message(&req, "", true, None).unwrap();
        let parsed = mail_parser::MessageParser::default()
            .parse(&out[..])
            .expect("must parse");
        let body = parsed.body_text(0).expect("text part");
        assert!(body.contains("Plain body."));
        let wire = String::from_utf8_lossy(&out);
        assert!(
            !wire.contains("Content-Type: text/html"),
            "text-only wire must not contain a text/html part: {wire}"
        );
    }

    /// mail-parser round-trip on the `--html-body` path: both parts
    /// present, HTML is the operator-supplied bytes.
    #[test]
    fn mail_parser_roundtrip_html_body_path() {
        let req = build_request("From: a@b.com\nTo: c@d.com\n", "fallback.");
        let out = assemble_wire_message(&req, "", false, Some(b"<p>raw</p>")).unwrap();
        let parsed = mail_parser::MessageParser::default()
            .parse(&out[..])
            .expect("must parse");
        let text = parsed.body_text(0).expect("text part");
        let html = parsed.body_html(0).expect("html part");
        assert!(text.contains("fallback."));
        assert!(html.contains("<p>raw</p>"));
    }
}
