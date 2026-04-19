use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fmt;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InboundFrontmatter {
    // -- Identity --
    pub id: String,
    pub message_id: String,
    #[serde(default)]
    pub thread_id: String,

    // -- Parties --
    pub from: String,
    pub to: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cc: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply_to: Option<String>,
    #[serde(default)]
    pub delivered_to: String,

    // -- Content --
    pub subject: String,
    pub date: String,
    #[serde(default)]
    pub received_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub received_from_ip: Option<String>,
    #[serde(default)]
    pub size_bytes: usize,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<AttachmentMeta>,

    // -- Threading --
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub in_reply_to: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub references: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub list_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_submitted: Option<String>,

    // -- Auth --
    #[serde(default = "default_auth_result")]
    pub dkim: String,
    #[serde(default = "default_auth_result")]
    pub spf: String,
    #[serde(default = "default_auth_result")]
    pub dmarc: String,
    #[serde(default = "default_auth_result")]
    pub trusted: String,

    // -- Storage --
    pub mailbox: String,
    pub read: bool,
    /// RFC 3339 UTC timestamp written by the `MARK-READ` handler when
    /// the email is marked read. Removed entirely on `MARK-UNREAD`.
    /// Re-marking read overwrites with a new timestamp — "most recent
    /// read", not "first read" (FR-13).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub read_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub labels: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttachmentMeta {
    pub filename: String,
    pub content_type: String,
    pub size: usize,
    pub path: String,
}

fn default_auth_result() -> String {
    "none".to_string()
}

pub struct AuthResults {
    pub dkim: String,
    pub spf: String,
    pub dmarc: String,
}

impl Default for AuthResults {
    fn default() -> Self {
        Self {
            dkim: "none".to_string(),
            spf: "none".to_string(),
            dmarc: "none".to_string(),
        }
    }
}

/// Delivery status for outbound sent files (FR-19c).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeliveryStatus {
    Delivered,
    Deferred,
    Failed,
    Pending,
}

impl DeliveryStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            DeliveryStatus::Delivered => "delivered",
            DeliveryStatus::Deferred => "deferred",
            DeliveryStatus::Failed => "failed",
            DeliveryStatus::Pending => "pending",
        }
    }
}

impl fmt::Display for DeliveryStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl Serialize for DeliveryStatus {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for DeliveryStatus {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        match s.as_str() {
            "delivered" => Ok(DeliveryStatus::Delivered),
            "deferred" => Ok(DeliveryStatus::Deferred),
            "failed" => Ok(DeliveryStatus::Failed),
            "pending" => Ok(DeliveryStatus::Pending),
            other => Err(serde::de::Error::custom(format!(
                "invalid delivery_status: {other}"
            ))),
        }
    }
}

/// Outbound frontmatter for sent-copy `.md` files (FR-19c).
///
/// Composes the same inbound fields (Identity/Parties/Content/Threading/
/// Auth/Storage) at the top, plus an outbound block at the end containing
/// `outbound`, `delivery_status`, and optional `bcc`, `delivered_at`,
/// `delivery_details`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutboundFrontmatter {
    // -- Identity --
    pub id: String,
    pub message_id: String,
    #[serde(default)]
    pub thread_id: String,

    // -- Parties --
    pub from: String,
    pub to: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cc: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply_to: Option<String>,
    #[serde(default)]
    pub delivered_to: String,

    // -- Content --
    pub subject: String,
    pub date: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub received_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub received_from_ip: Option<String>,
    #[serde(default)]
    pub size_bytes: usize,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<AttachmentMeta>,

    // -- Threading --
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub in_reply_to: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub references: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub list_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_submitted: Option<String>,

    // -- Auth --
    #[serde(default = "default_auth_result")]
    pub dkim: String,
    #[serde(default = "default_auth_result")]
    pub spf: String,
    #[serde(default = "default_auth_result")]
    pub dmarc: String,
    #[serde(default = "default_auth_result")]
    pub trusted: String,

    // -- Storage --
    pub mailbox: String,
    pub read: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub labels: Vec<String>,

    // -- Outbound block --
    pub outbound: bool,
    pub delivery_status: DeliveryStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bcc: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivered_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivery_details: Option<String>,
}

pub fn format_outbound_frontmatter(meta: &OutboundFrontmatter, body: &str) -> String {
    let toml_str = toml::to_string(meta).expect("OutboundFrontmatter must serialize to TOML");
    let mut result = String::new();
    result.push_str("+++\n");
    result.push_str(&toml_str);
    result.push_str("+++\n\n");
    result.push_str(body);

    if !body.ends_with('\n') {
        result.push('\n');
    }

    result
}

/// Compute a deterministic thread ID from email threading headers.
///
/// Resolution order:
/// 1. Walk `In-Reply-To` — use the first Message-ID found.
/// 2. Walk `References` — use the earliest (leftmost) Message-ID.
/// 3. Fall back to the message's own `Message-ID`.
///
/// The resolved root Message-ID is SHA-256 hashed and truncated to 16 hex chars.
pub fn compute_thread_id(
    message_id: &str,
    in_reply_to: Option<&str>,
    references: Option<&str>,
) -> String {
    let root = resolve_thread_root(message_id, in_reply_to, references);
    let normalized = strip_angle_brackets(&root);
    let mut hasher = Sha256::new();
    hasher.update(normalized.as_bytes());
    let hash = hasher.finalize();
    hex_encode(&hash[..8])
}

fn strip_angle_brackets(s: &str) -> &str {
    s.strip_prefix('<')
        .and_then(|s| s.strip_suffix('>'))
        .unwrap_or(s)
}

fn resolve_thread_root(
    message_id: &str,
    in_reply_to: Option<&str>,
    references: Option<&str>,
) -> String {
    if let Some(irt) = in_reply_to {
        let extracted = extract_first_message_id(irt);
        if !extracted.is_empty() {
            return extracted;
        }
    }

    if let Some(refs) = references {
        let extracted = extract_first_message_id(refs);
        if !extracted.is_empty() {
            return extracted;
        }
    }

    let own = extract_first_message_id(message_id);
    if own.is_empty() {
        message_id.to_string()
    } else {
        own
    }
}

fn extract_first_message_id(header_value: &str) -> String {
    let trimmed = header_value.trim();
    if trimmed.is_empty() {
        return String::new();
    }

    // Message-IDs are angle-bracket-delimited: <id@domain>
    if let Some(start) = trimmed.find('<')
        && let Some(end) = trimmed[start..].find('>')
    {
        let id = &trimmed[start..start + end + 1];
        if !id.is_empty() {
            return id.to_string();
        }
    }

    // Bare Message-ID without angle brackets
    trimmed.to_string()
}

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn format_frontmatter(meta: &InboundFrontmatter, body: &str) -> String {
    let toml_str = toml::to_string(meta).expect("InboundFrontmatter must serialize to TOML");
    let mut result = String::new();
    result.push_str("+++\n");
    result.push_str(&toml_str);
    result.push_str("+++\n\n");
    result.push_str(body);

    if !body.ends_with('\n') {
        result.push('\n');
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_frontmatter() -> InboundFrontmatter {
        InboundFrontmatter {
            id: "2025-01-01-120000-hello".to_string(),
            message_id: "<abc123@example.com>".to_string(),
            thread_id: "a1b2c3d4e5f6a7b8".to_string(),
            from: "sender@example.com".to_string(),
            to: "alice@test.com".to_string(),
            cc: None,
            reply_to: None,
            delivered_to: "alice@test.com".to_string(),
            subject: "Hello".to_string(),
            date: "2025-01-01T12:00:00Z".to_string(),
            received_at: "2025-01-01T12:00:01Z".to_string(),
            received_from_ip: Some("203.0.113.1".to_string()),
            size_bytes: 256,
            attachments: vec![],
            in_reply_to: None,
            references: None,
            list_id: None,
            auto_submitted: None,
            dkim: "none".to_string(),
            spf: "none".to_string(),
            dmarc: "none".to_string(),
            trusted: "none".to_string(),
            mailbox: "alice".to_string(),
            read: false,
            read_at: None,
            labels: vec![],
        }
    }

    #[test]
    fn field_order_matches_spec() {
        let fm = sample_frontmatter();
        let toml_str = toml::to_string(&fm).unwrap();
        let lines: Vec<&str> = toml_str.lines().collect();

        // Verify field ordering: Identity -> Parties -> Content -> Threading -> Auth -> Storage
        let field_positions: Vec<(&str, usize)> = lines
            .iter()
            .enumerate()
            .filter_map(|(i, line)| {
                let key = line.split('=').next()?.trim();
                if key.starts_with('[') || key.is_empty() {
                    None
                } else {
                    Some((key, i))
                }
            })
            .collect();

        let field_names: Vec<&str> = field_positions.iter().map(|(name, _)| *name).collect();

        // Identity section
        assert!(
            field_names.iter().position(|&f| f == "id").unwrap()
                < field_names.iter().position(|&f| f == "message_id").unwrap()
        );
        assert!(
            field_names.iter().position(|&f| f == "message_id").unwrap()
                < field_names.iter().position(|&f| f == "thread_id").unwrap()
        );

        // Parties section comes after Identity
        assert!(
            field_names.iter().position(|&f| f == "thread_id").unwrap()
                < field_names.iter().position(|&f| f == "from").unwrap()
        );
        assert!(
            field_names.iter().position(|&f| f == "from").unwrap()
                < field_names.iter().position(|&f| f == "to").unwrap()
        );
        assert!(
            field_names.iter().position(|&f| f == "to").unwrap()
                < field_names
                    .iter()
                    .position(|&f| f == "delivered_to")
                    .unwrap()
        );

        // Content section comes after Parties
        assert!(
            field_names
                .iter()
                .position(|&f| f == "delivered_to")
                .unwrap()
                < field_names.iter().position(|&f| f == "subject").unwrap()
        );
        assert!(
            field_names.iter().position(|&f| f == "subject").unwrap()
                < field_names.iter().position(|&f| f == "date").unwrap()
        );

        // Auth section
        assert!(
            field_names.iter().position(|&f| f == "dkim").unwrap()
                < field_names.iter().position(|&f| f == "spf").unwrap()
        );
        assert!(
            field_names.iter().position(|&f| f == "spf").unwrap()
                < field_names.iter().position(|&f| f == "dmarc").unwrap()
        );
        assert!(
            field_names.iter().position(|&f| f == "dmarc").unwrap()
                < field_names.iter().position(|&f| f == "trusted").unwrap()
        );

        // Storage section comes after Auth
        assert!(
            field_names.iter().position(|&f| f == "trusted").unwrap()
                < field_names.iter().position(|&f| f == "mailbox").unwrap()
        );
        assert!(
            field_names.iter().position(|&f| f == "mailbox").unwrap()
                < field_names.iter().position(|&f| f == "read").unwrap()
        );
    }

    #[test]
    fn optional_fields_omitted_when_none() {
        let fm = sample_frontmatter();
        let toml_str = toml::to_string(&fm).unwrap();
        assert!(!toml_str.contains("cc ="));
        assert!(!toml_str.contains("reply_to ="));
        assert!(!toml_str.contains("in_reply_to ="));
        assert!(!toml_str.contains("references ="));
        assert!(!toml_str.contains("list_id ="));
        assert!(!toml_str.contains("auto_submitted ="));
    }

    #[test]
    fn empty_vecs_omitted() {
        let fm = sample_frontmatter();
        let toml_str = toml::to_string(&fm).unwrap();
        assert!(!toml_str.contains("attachments"));
        assert!(!toml_str.contains("labels"));
    }

    #[test]
    fn optional_fields_present_when_set() {
        let mut fm = sample_frontmatter();
        fm.cc = Some("bob@example.com".to_string());
        fm.reply_to = Some("reply@example.com".to_string());
        fm.in_reply_to = Some("<parent@example.com>".to_string());
        fm.references = Some("<root@example.com> <parent@example.com>".to_string());
        fm.list_id = Some("<mylist.example.com>".to_string());
        fm.auto_submitted = Some("auto-generated".to_string());

        let toml_str = toml::to_string(&fm).unwrap();
        assert!(toml_str.contains("cc ="));
        assert!(toml_str.contains("reply_to ="));
        assert!(toml_str.contains("in_reply_to ="));
        assert!(toml_str.contains("references ="));
        assert!(toml_str.contains("list_id ="));
        assert!(toml_str.contains("auto_submitted ="));
    }

    #[test]
    fn always_written_fields_present_at_defaults() {
        let fm = sample_frontmatter();
        let toml_str = toml::to_string(&fm).unwrap();
        assert!(toml_str.contains("dkim = \"none\""));
        assert!(toml_str.contains("spf = \"none\""));
        assert!(toml_str.contains("dmarc = \"none\""));
        assert!(toml_str.contains("trusted = \"none\""));
        assert!(toml_str.contains("read = false"));
    }

    #[test]
    fn trusted_placeholder_always_none() {
        let fm = sample_frontmatter();
        assert_eq!(fm.trusted, "none");
        let toml_str = toml::to_string(&fm).unwrap();
        assert!(toml_str.contains("trusted = \"none\""));
    }

    #[test]
    fn format_frontmatter_wraps_with_delimiters() {
        let fm = sample_frontmatter();
        let output = format_frontmatter(&fm, "Hello world");
        assert!(output.starts_with("+++\n"));
        assert!(output.contains("+++\n\nHello world\n"));
        let parts: Vec<&str> = output.splitn(3, "+++").collect();
        assert_eq!(parts.len(), 3);
    }

    #[test]
    fn serialized_frontmatter_is_valid_toml() {
        let fm = sample_frontmatter();
        let toml_str = toml::to_string(&fm).unwrap();
        let parsed: toml::Value = toml::from_str(&toml_str).unwrap();
        let table = parsed.as_table().unwrap();
        assert!(table.contains_key("id"));
        assert!(table.contains_key("message_id"));
        assert!(table.contains_key("thread_id"));
        assert!(table.contains_key("from"));
        assert!(table.contains_key("to"));
        assert!(table.contains_key("delivered_to"));
        assert!(table.contains_key("subject"));
        assert!(table.contains_key("date"));
        assert!(table.contains_key("received_at"));
        assert!(table.contains_key("size_bytes"));
        assert!(table.contains_key("dkim"));
        assert!(table.contains_key("spf"));
        assert!(table.contains_key("dmarc"));
        assert!(table.contains_key("trusted"));
        assert!(table.contains_key("mailbox"));
        assert!(table.contains_key("read"));
    }

    #[test]
    fn attachments_vec_serialized_when_non_empty() {
        let mut fm = sample_frontmatter();
        fm.attachments = vec![AttachmentMeta {
            filename: "file.txt".to_string(),
            content_type: "text/plain".to_string(),
            size: 42,
            path: "file.txt".to_string(),
        }];
        let toml_str = toml::to_string(&fm).unwrap();
        assert!(toml_str.contains("[[attachments]]"));
        assert!(toml_str.contains("filename = \"file.txt\""));
    }

    #[test]
    fn compute_thread_id_orphan_message() {
        let tid = compute_thread_id("<abc@example.com>", None, None);
        assert_eq!(tid.len(), 16);
        // Deterministic: same input always yields same output
        let tid2 = compute_thread_id("<abc@example.com>", None, None);
        assert_eq!(tid, tid2);
    }

    #[test]
    fn compute_thread_id_direct_reply() {
        let tid = compute_thread_id("<reply@example.com>", Some("<parent@example.com>"), None);
        // Should hash the In-Reply-To parent, not the current message
        let parent_tid = compute_thread_id("<parent@example.com>", None, None);
        assert_eq!(tid, parent_tid);
    }

    #[test]
    fn compute_thread_id_uses_references_earliest() {
        let tid = compute_thread_id(
            "<msg3@example.com>",
            None,
            Some("<root@example.com> <msg2@example.com>"),
        );
        // Should hash the earliest reference (root)
        let root_tid = compute_thread_id("<root@example.com>", None, None);
        assert_eq!(tid, root_tid);
    }

    #[test]
    fn compute_thread_id_in_reply_to_takes_precedence() {
        let tid = compute_thread_id(
            "<msg3@example.com>",
            Some("<irt@example.com>"),
            Some("<ref-root@example.com> <ref2@example.com>"),
        );
        // In-Reply-To takes precedence over References
        let irt_tid = compute_thread_id("<irt@example.com>", None, None);
        assert_eq!(tid, irt_tid);
    }

    #[test]
    fn compute_thread_id_missing_headers() {
        let tid = compute_thread_id("bare-id", None, None);
        assert_eq!(tid.len(), 16);
    }

    #[test]
    fn compute_thread_id_empty_in_reply_to_falls_through() {
        let tid = compute_thread_id("<own@example.com>", Some(""), Some("<ref@example.com>"));
        let ref_tid = compute_thread_id("<ref@example.com>", None, None);
        assert_eq!(tid, ref_tid);
    }

    #[test]
    fn compute_thread_id_multiple_message_ids_in_references() {
        let tid1 = compute_thread_id(
            "<msg@example.com>",
            None,
            Some("<first@example.com> <second@example.com> <third@example.com>"),
        );
        let tid2 = compute_thread_id("<first@example.com>", None, None);
        assert_eq!(tid1, tid2);
    }

    #[test]
    fn extract_first_message_id_angle_brackets() {
        assert_eq!(
            extract_first_message_id("<abc@example.com>"),
            "<abc@example.com>"
        );
    }

    #[test]
    fn extract_first_message_id_multiple() {
        assert_eq!(
            extract_first_message_id("<first@ex.com> <second@ex.com>"),
            "<first@ex.com>"
        );
    }

    #[test]
    fn extract_first_message_id_empty() {
        assert_eq!(extract_first_message_id(""), "");
    }

    #[test]
    fn extract_first_message_id_bare() {
        assert_eq!(
            extract_first_message_id("bare-id@example.com"),
            "bare-id@example.com"
        );
    }

    #[test]
    fn golden_frontmatter_output() {
        let fm = sample_frontmatter();
        let output = format_frontmatter(&fm, "Test body content.");
        // Verify exact structure
        assert!(output.starts_with("+++\n"));
        assert!(output.contains("\n+++\n\n"));
        assert!(output.ends_with("Test body content.\n"));

        // Parse back to validate roundtrip
        let parts: Vec<&str> = output.splitn(3, "+++").collect();
        let toml_str = parts[1].trim();
        let parsed: InboundFrontmatter = toml::from_str(toml_str).unwrap();
        assert_eq!(parsed.id, fm.id);
        assert_eq!(parsed.thread_id, fm.thread_id);
        assert_eq!(parsed.dmarc, "none");
        assert_eq!(parsed.trusted, "none");
    }

    #[test]
    fn field_order_regression_golden() {
        let fm = sample_frontmatter();
        let toml_str = toml::to_string(&fm).unwrap();

        // Capture the expected field order by parsing keys in order
        let expected_keys = vec![
            "id",
            "message_id",
            "thread_id",
            "from",
            "to",
            "delivered_to",
            "subject",
            "date",
            "received_at",
            "received_from_ip",
            "size_bytes",
            "dkim",
            "spf",
            "dmarc",
            "trusted",
            "mailbox",
            "read",
        ];

        let actual_keys: Vec<&str> = toml_str
            .lines()
            .filter_map(|line| {
                let key = line.split('=').next()?.trim();
                if key.starts_with('[') || key.is_empty() {
                    None
                } else {
                    Some(key)
                }
            })
            .collect();

        assert_eq!(actual_keys, expected_keys);
    }

    #[test]
    fn received_from_ip_omitted_when_none() {
        let mut fm = sample_frontmatter();
        fm.received_from_ip = None;
        let toml_str = toml::to_string(&fm).unwrap();
        assert!(!toml_str.contains("received_from_ip"));
    }

    #[test]
    fn read_at_omitted_when_none() {
        let fm = sample_frontmatter();
        assert!(fm.read_at.is_none());
        let toml_str = toml::to_string(&fm).unwrap();
        assert!(
            !toml_str.contains("read_at"),
            "read_at must be omitted when None (FR-19d); got:\n{toml_str}"
        );
    }

    #[test]
    fn read_at_present_when_set() {
        use chrono::TimeZone;
        let mut fm = sample_frontmatter();
        fm.read = true;
        fm.read_at = Some(Utc.with_ymd_and_hms(2025, 6, 15, 12, 30, 45).unwrap());
        let toml_str = toml::to_string(&fm).unwrap();
        assert!(toml_str.contains("read_at ="), "got:\n{toml_str}");
    }

    #[test]
    fn read_at_appears_after_read_in_storage_section() {
        use chrono::TimeZone;
        let mut fm = sample_frontmatter();
        fm.read = true;
        fm.read_at = Some(Utc.with_ymd_and_hms(2025, 6, 15, 12, 30, 45).unwrap());
        let toml_str = toml::to_string(&fm).unwrap();
        let read_pos = toml_str.find("\nread = ").expect("read field present");
        let read_at_pos = toml_str
            .find("\nread_at = ")
            .expect("read_at field present");
        assert!(
            read_pos < read_at_pos,
            "read_at must follow read; got:\n{toml_str}"
        );
    }

    #[test]
    fn read_at_roundtrip_preserves_value() {
        use chrono::TimeZone;
        let mut fm = sample_frontmatter();
        fm.read = true;
        let ts = Utc.with_ymd_and_hms(2025, 6, 15, 12, 30, 45).unwrap();
        fm.read_at = Some(ts);

        let toml_str = toml::to_string(&fm).unwrap();
        let parsed: InboundFrontmatter = toml::from_str(&toml_str).unwrap();
        assert_eq!(parsed.read_at, Some(ts));
    }

    fn sample_outbound_frontmatter() -> OutboundFrontmatter {
        OutboundFrontmatter {
            id: "2025-01-01-120000-hello".to_string(),
            message_id: "<abc123@example.com>".to_string(),
            thread_id: "a1b2c3d4e5f6a7b8".to_string(),
            from: "alice@example.com".to_string(),
            to: "bob@gmail.com".to_string(),
            cc: None,
            reply_to: None,
            delivered_to: "bob@gmail.com".to_string(),
            subject: "Hello".to_string(),
            date: "2025-01-01T12:00:00Z".to_string(),
            received_at: String::new(),
            received_from_ip: None,
            size_bytes: 256,
            attachments: vec![],
            in_reply_to: None,
            references: None,
            list_id: None,
            auto_submitted: None,
            dkim: "pass".to_string(),
            spf: "none".to_string(),
            dmarc: "none".to_string(),
            trusted: "none".to_string(),
            mailbox: "alice".to_string(),
            read: false,
            labels: vec![],
            outbound: true,
            delivery_status: DeliveryStatus::Delivered,
            bcc: None,
            delivered_at: Some("2025-01-01T12:00:05Z".to_string()),
            delivery_details: Some("250 OK".to_string()),
        }
    }

    #[test]
    fn delivery_status_serialization() {
        for (variant, expected) in [
            (DeliveryStatus::Delivered, "\"delivered\""),
            (DeliveryStatus::Deferred, "\"deferred\""),
            (DeliveryStatus::Failed, "\"failed\""),
            (DeliveryStatus::Pending, "\"pending\""),
        ] {
            let json = serde_json::to_string(&variant).unwrap();
            assert_eq!(json, expected);
            let back: DeliveryStatus = serde_json::from_str(&json).unwrap();
            assert_eq!(variant, back);
        }
    }

    #[test]
    fn outbound_frontmatter_field_order() {
        let fm = sample_outbound_frontmatter();
        let toml_str = toml::to_string(&fm).unwrap();

        let actual_keys: Vec<&str> = toml_str
            .lines()
            .filter_map(|line| {
                let key = line.split('=').next()?.trim();
                if key.starts_with('[') || key.is_empty() {
                    None
                } else {
                    Some(key)
                }
            })
            .collect();

        // Inbound block first, then outbound block at the end.
        // `received_at` is omitted because it is empty on outbound messages
        // (S41-1: skip_serializing_if = "String::is_empty").
        let expected_keys = vec![
            "id",
            "message_id",
            "thread_id",
            "from",
            "to",
            "delivered_to",
            "subject",
            "date",
            "size_bytes",
            "dkim",
            "spf",
            "dmarc",
            "trusted",
            "mailbox",
            "read",
            "outbound",
            "delivery_status",
            "delivered_at",
            "delivery_details",
        ];

        assert_eq!(actual_keys, expected_keys);
    }

    #[test]
    fn outbound_delivery_status_always_written() {
        let mut fm = sample_outbound_frontmatter();
        fm.delivered_at = None;
        fm.delivery_details = None;
        fm.bcc = None;
        let toml_str = toml::to_string(&fm).unwrap();
        assert!(toml_str.contains("delivery_status = \"delivered\""));
        assert!(!toml_str.contains("delivered_at"));
        assert!(!toml_str.contains("delivery_details"));
        assert!(!toml_str.contains("bcc"));
    }

    #[test]
    fn outbound_golden_test() {
        let fm = sample_outbound_frontmatter();
        let output = format_outbound_frontmatter(&fm, "DKIM-Signature: ...\r\nFrom: ...");

        assert!(output.starts_with("+++\n"));
        assert!(output.contains("\n+++\n\n"));

        // Parse back
        let parts: Vec<&str> = output.splitn(3, "+++").collect();
        let toml_str = parts[1].trim();
        let parsed: OutboundFrontmatter = toml::from_str(toml_str).unwrap();
        assert_eq!(parsed.id, fm.id);
        assert!(parsed.outbound);
        assert_eq!(parsed.delivery_status, DeliveryStatus::Delivered);
        assert_eq!(parsed.delivered_at.as_deref(), Some("2025-01-01T12:00:05Z"));
        assert_eq!(parsed.delivery_details.as_deref(), Some("250 OK"));
    }

    #[test]
    fn outbound_failed_delivery() {
        let mut fm = sample_outbound_frontmatter();
        fm.delivery_status = DeliveryStatus::Failed;
        fm.delivered_at = None;
        fm.delivery_details = Some("550 no such user".to_string());

        let toml_str = toml::to_string(&fm).unwrap();
        assert!(toml_str.contains("delivery_status = \"failed\""));
        assert!(toml_str.contains("delivery_details = \"550 no such user\""));
        assert!(!toml_str.contains("delivered_at"));
    }

    #[test]
    fn outbound_with_bcc() {
        let mut fm = sample_outbound_frontmatter();
        fm.bcc = Some(vec![
            "secret@example.com".to_string(),
            "hidden@example.com".to_string(),
        ]);

        let toml_str = toml::to_string(&fm).unwrap();
        assert!(toml_str.contains("bcc"));
        assert!(toml_str.contains("secret@example.com"));
        assert!(toml_str.contains("hidden@example.com"));
    }

    #[test]
    fn outbound_outbound_always_true() {
        let fm = sample_outbound_frontmatter();
        assert!(fm.outbound);
        let toml_str = toml::to_string(&fm).unwrap();
        assert!(toml_str.contains("outbound = true"));
    }

    #[test]
    fn outbound_received_at_omitted_when_empty() {
        let fm = sample_outbound_frontmatter();
        assert!(fm.received_at.is_empty());
        let toml_str = toml::to_string(&fm).unwrap();
        assert!(
            !toml_str.contains("received_at"),
            "empty received_at must be omitted on outbound files; got:\n{toml_str}"
        );
    }
}
