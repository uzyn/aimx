use crate::config::{Config, MailboxConfig};
use crate::frontmatter::AuthResults;
use serde::{Deserialize, Serialize};
use std::fmt;

/// Result of per-mailbox trust evaluation, surfaced as the `trusted`
/// frontmatter field on every inbound email.
///
/// Three-valued:
///
/// - `None` -- mailbox `trust` is `none` (default). No evaluation performed.
/// - `True` -- mailbox `trust` is `verified`, sender matches
///   `trusted_senders`, AND DKIM passed.
/// - `False` -- mailbox `trust` is `verified`, any other outcome.
///
/// Serializes to lowercase `"none"`, `"true"`, `"false"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrustedValue {
    None,
    True,
    False,
}

impl TrustedValue {
    pub fn as_str(&self) -> &'static str {
        match self {
            TrustedValue::None => "none",
            TrustedValue::True => "true",
            TrustedValue::False => "false",
        }
    }
}

impl fmt::Display for TrustedValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl Serialize for TrustedValue {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for TrustedValue {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        match s.as_str() {
            "none" => Ok(TrustedValue::None),
            "true" => Ok(TrustedValue::True),
            "false" => Ok(TrustedValue::False),
            other => Err(serde::de::Error::custom(format!(
                "invalid trusted value: {other}"
            ))),
        }
    }
}

/// Evaluate the trust outcome for an inbound email against the effective
/// trust policy for `mailbox` (its own override if set, otherwise the
/// global default on `config`).
///
/// Logic:
/// - effective `trust == "none"` -> `TrustedValue::None` (no evaluation)
/// - effective `trust == "verified"` AND sender matches effective
///   `trusted_senders` AND DKIM pass -> `TrustedValue::True`
/// - effective `trust == "verified"`, any other outcome -> `TrustedValue::False`
///
/// Note: hook gating is decoupled from this value. An `on_receive`
/// hook fires iff `trusted == "true"` OR the hook opts in via
/// `fire_on_untrusted`. See `hook::should_fire_on_receive`.
pub fn evaluate_trust(
    config: &Config,
    mailbox: &MailboxConfig,
    auth: &AuthResults,
    from: &str,
) -> TrustedValue {
    let trust = mailbox.effective_trust(config);
    let sender_bare = extract_email_for_match(from);
    let mailbox_name = mailbox_key_for_log(config, mailbox);

    let (result, sender_allowlisted) = if trust == "none" {
        (TrustedValue::None, false)
    } else if trust == "verified" {
        let senders = mailbox.effective_trusted_senders(config);
        let sender_allowlisted = is_sender_in_trusted_senders(senders, from);
        let dkim_passed = auth.dkim == "pass";

        if sender_allowlisted && dkim_passed {
            (TrustedValue::True, sender_allowlisted)
        } else {
            (TrustedValue::False, sender_allowlisted)
        }
    } else {
        // Unknown trust value: fail closed.
        (TrustedValue::False, false)
    };

    tracing::info!(
        target: "aimx::trust",
        "trust eval mailbox={mailbox} policy={policy} sender={sender} dkim={dkim} sender_allowlisted={sender_allowlisted} result={result}",
        mailbox = mailbox_name,
        policy = trust,
        sender = sender_bare,
        dkim = auth.dkim,
        sender_allowlisted = sender_allowlisted,
        result = result.as_str(),
    );

    result
}

/// Best-effort resolution of the mailbox's display name for logs.
/// `MailboxConfig` doesn't carry its own key, so we search the `Config`
/// map for a reverse-lookup. Falls back to the mailbox `address` when
/// the caller constructed an ad-hoc `MailboxConfig` not in the map
/// (e.g. unit tests).
fn mailbox_key_for_log(config: &Config, mailbox: &MailboxConfig) -> String {
    for (name, mb) in &config.mailboxes {
        if mb.address == mailbox.address {
            return name.clone();
        }
    }
    mailbox.address.clone()
}

fn extract_email_for_match(from: &str) -> String {
    // Match RFC 5322 display-name form `"Name" <addr>` by taking the LAST
    // `<` and the first `>` after it. Mirrors `send_handler::extract_bare_address`
    // and avoids slice-panics on pathological input like `"foo>bar<baz>"`
    // where a stray `>` precedes the opening `<`.
    if let Some(start) = from.rfind('<') {
        let tail = &from[start + 1..];
        if let Some(end) = tail.find('>') {
            return tail[..end].to_lowercase();
        }
    }
    from.to_lowercase()
}

fn is_sender_in_trusted_senders(senders: &[String], from: &str) -> bool {
    let from_lower = extract_email_for_match(from);
    for pattern in senders {
        if glob_match::glob_match(pattern, &from_lower) {
            return true;
        }
    }
    false
}
