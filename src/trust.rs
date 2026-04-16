use crate::config::MailboxConfig;
use crate::frontmatter::AuthResults;
use serde::{Deserialize, Serialize};
use std::fmt;

/// Result of per-mailbox trust evaluation, surfaced as the `trusted`
/// frontmatter field on every inbound email (FR-37b).
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

/// Evaluate the trust outcome for an inbound email against a mailbox's
/// trust policy.
///
/// Logic:
/// - `trust: none` -> `TrustedValue::None` (no evaluation)
/// - `trust: verified` AND sender matches `trusted_senders` AND DKIM pass
///   -> `TrustedValue::True`
/// - `trust: verified`, any other outcome -> `TrustedValue::False`
///
/// Note: this is stricter than the channel-trigger gate in `channel.rs`,
/// which fires when the sender is allowlisted OR DKIM passes. The
/// `trusted` field requires BOTH. See `channel::should_execute_triggers`
/// for the v1 gate semantics and the rationale comment there.
pub fn evaluate_trust(mailbox: &MailboxConfig, auth: &AuthResults, from: &str) -> TrustedValue {
    if mailbox.trust == "none" {
        return TrustedValue::None;
    }

    if mailbox.trust == "verified" {
        let sender_allowlisted = is_sender_in_trusted_senders(mailbox, from);
        let dkim_passed = auth.dkim == "pass";

        if sender_allowlisted && dkim_passed {
            return TrustedValue::True;
        }
        return TrustedValue::False;
    }

    // Unknown trust value: fail closed (same as channel.rs).
    TrustedValue::False
}

fn extract_email_for_match(from: &str) -> String {
    if let Some(start) = from.find('<')
        && let Some(end) = from.find('>')
    {
        return from[start + 1..end].to_lowercase();
    }
    from.to_lowercase()
}

fn is_sender_in_trusted_senders(mailbox: &MailboxConfig, from: &str) -> bool {
    let from_lower = extract_email_for_match(from);
    for pattern in &mailbox.trusted_senders {
        if glob_match::glob_match(pattern, &from_lower) {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::MailboxConfig;
    use crate::frontmatter::AuthResults;

    fn mailbox_none() -> MailboxConfig {
        MailboxConfig {
            address: "*@test.com".to_string(),
            trust: "none".to_string(),
            trusted_senders: vec![],
            on_receive: vec![],
        }
    }

    fn mailbox_verified(trusted_senders: Vec<String>) -> MailboxConfig {
        MailboxConfig {
            address: "secure@test.com".to_string(),
            trust: "verified".to_string(),
            trusted_senders,
            on_receive: vec![],
        }
    }

    fn auth(dkim: &str) -> AuthResults {
        AuthResults {
            dkim: dkim.to_string(),
            spf: "none".to_string(),
            dmarc: "none".to_string(),
        }
    }

    #[test]
    fn trust_none_returns_none() {
        let mb = mailbox_none();
        let result = evaluate_trust(&mb, &auth("pass"), "anyone@example.com");
        assert_eq!(result, TrustedValue::None);
        assert_eq!(result.as_str(), "none");
    }

    #[test]
    fn trust_none_returns_none_even_with_dkim_fail() {
        let mb = mailbox_none();
        let result = evaluate_trust(&mb, &auth("fail"), "anyone@example.com");
        assert_eq!(result, TrustedValue::None);
    }

    #[test]
    fn verified_allowlisted_dkim_pass_returns_true() {
        let mb = mailbox_verified(vec!["*@gmail.com".to_string()]);
        let result = evaluate_trust(&mb, &auth("pass"), "alice@gmail.com");
        assert_eq!(result, TrustedValue::True);
        assert_eq!(result.as_str(), "true");
    }

    #[test]
    fn verified_allowlisted_dkim_fail_returns_false() {
        let mb = mailbox_verified(vec!["*@gmail.com".to_string()]);
        let result = evaluate_trust(&mb, &auth("fail"), "alice@gmail.com");
        assert_eq!(result, TrustedValue::False);
        assert_eq!(result.as_str(), "false");
    }

    #[test]
    fn verified_not_allowlisted_dkim_pass_returns_false() {
        let mb = mailbox_verified(vec!["*@company.com".to_string()]);
        let result = evaluate_trust(&mb, &auth("pass"), "alice@gmail.com");
        assert_eq!(result, TrustedValue::False);
    }

    #[test]
    fn verified_not_allowlisted_dkim_fail_returns_false() {
        let mb = mailbox_verified(vec!["*@company.com".to_string()]);
        let result = evaluate_trust(&mb, &auth("fail"), "alice@gmail.com");
        assert_eq!(result, TrustedValue::False);
    }

    #[test]
    fn verified_empty_trusted_senders_dkim_pass_returns_false() {
        let mb = mailbox_verified(vec![]);
        let result = evaluate_trust(&mb, &auth("pass"), "alice@gmail.com");
        assert_eq!(result, TrustedValue::False);
    }

    #[test]
    fn verified_dkim_none_returns_false() {
        let mb = mailbox_verified(vec!["*@gmail.com".to_string()]);
        let result = evaluate_trust(&mb, &auth("none"), "alice@gmail.com");
        assert_eq!(result, TrustedValue::False);
    }

    #[test]
    fn verified_exact_sender_match() {
        let mb = mailbox_verified(vec!["alice@gmail.com".to_string()]);
        let result = evaluate_trust(&mb, &auth("pass"), "alice@gmail.com");
        assert_eq!(result, TrustedValue::True);
    }

    #[test]
    fn verified_display_name_in_from() {
        let mb = mailbox_verified(vec!["*@gmail.com".to_string()]);
        let result = evaluate_trust(&mb, &auth("pass"), "Alice Smith <alice@gmail.com>");
        assert_eq!(result, TrustedValue::True);
    }

    #[test]
    fn unknown_trust_value_returns_false() {
        let mb = MailboxConfig {
            address: "test@test.com".to_string(),
            trust: "typo".to_string(),
            trusted_senders: vec![],
            on_receive: vec![],
        };
        let result = evaluate_trust(&mb, &auth("pass"), "alice@gmail.com");
        assert_eq!(result, TrustedValue::False);
    }

    #[test]
    fn serialization_roundtrip() {
        for variant in [TrustedValue::None, TrustedValue::True, TrustedValue::False] {
            let json = serde_json::to_string(&variant).unwrap();
            let back: TrustedValue = serde_json::from_str(&json).unwrap();
            assert_eq!(variant, back);
        }
    }

    /// Parity test: for a `trust: verified` mailbox, `trusted == "true"` IFF
    /// the channel-trigger gate would fire.
    ///
    /// The channel gate (v1 semantics) fires when:
    /// - sender is allowlisted, OR
    /// - DKIM passes
    ///
    /// The `trusted` field (FR-37b) is `"true"` when:
    /// - sender is allowlisted AND DKIM passes
    ///
    /// So `trusted == "true"` is strictly stronger than the trigger gate.
    /// When `trusted == "true"`, the trigger gate also fires (both conditions
    /// met). This test confirms that agreement.
    #[test]
    fn parity_trusted_true_implies_trigger_fires() {
        use crate::channel;
        use crate::frontmatter::InboundFrontmatter;

        let mb = mailbox_verified(vec!["*@gmail.com".to_string()]);

        let test_cases = vec![
            ("alice@gmail.com", "pass"),
            ("alice@gmail.com", "fail"),
            ("alice@gmail.com", "none"),
            ("alice@yahoo.com", "pass"),
            ("alice@yahoo.com", "fail"),
            ("alice@yahoo.com", "none"),
        ];

        for (from, dkim_result) in test_cases {
            let auth_results = auth(dkim_result);
            let trusted = evaluate_trust(&mb, &auth_results, from);

            let meta = InboundFrontmatter {
                id: "test".to_string(),
                message_id: "<test@test.com>".to_string(),
                thread_id: "0123456789abcdef".to_string(),
                from: from.to_string(),
                to: "agent@test.com".to_string(),
                cc: None,
                reply_to: None,
                delivered_to: "agent@test.com".to_string(),
                subject: "Test".to_string(),
                date: "2025-01-01T00:00:00Z".to_string(),
                received_at: "2025-01-01T00:00:01Z".to_string(),
                received_from_ip: None,
                size_bytes: 100,
                in_reply_to: None,
                references: None,
                attachments: vec![],
                list_id: None,
                auto_submitted: None,
                dkim: dkim_result.to_string(),
                spf: "none".to_string(),
                dmarc: "none".to_string(),
                trusted: trusted.as_str().to_string(),
                mailbox: "secure".to_string(),
                read: false,
                labels: vec![],
            };

            let trigger_would_fire = channel::should_execute_triggers(&mb, &meta);

            if trusted == TrustedValue::True {
                assert!(
                    trigger_would_fire,
                    "trusted=true but trigger would NOT fire for from={from}, dkim={dkim_result}"
                );
            }
        }
    }
}
