use crate::config::{Config, MailboxConfig};
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
/// Note: Sprint 50 moves hook gating off this value. An `on_receive`
/// hook fires iff `trusted == "true"` OR the hook opts in via
/// `dangerously_support_untrusted`. See `hook::should_fire_on_receive`.
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, MailboxConfig};
    use crate::frontmatter::AuthResults;
    use std::collections::HashMap;
    use std::path::PathBuf;
    use tracing_test::traced_test;

    fn bare_config() -> Config {
        Config {
            domain: "test.com".to_string(),
            data_dir: PathBuf::from("/tmp/aimx-test"),
            dkim_selector: "aimx".to_string(),
            trust: "none".to_string(),
            trusted_senders: vec![],
            hook_templates: Vec::new(),
            mailboxes: HashMap::new(),
            verify_host: None,
            enable_ipv6: false,
        }
    }

    fn mailbox_none() -> MailboxConfig {
        MailboxConfig {
            address: "*@test.com".to_string(),
            trust: Some("none".to_string()),
            trusted_senders: Some(vec![]),
            hooks: vec![],
        }
    }

    fn mailbox_verified(trusted_senders: Vec<String>) -> MailboxConfig {
        MailboxConfig {
            address: "secure@test.com".to_string(),
            trust: Some("verified".to_string()),
            trusted_senders: Some(trusted_senders),
            hooks: vec![],
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
        let cfg = bare_config();
        let mb = mailbox_none();
        let result = evaluate_trust(&cfg, &mb, &auth("pass"), "anyone@example.com");
        assert_eq!(result, TrustedValue::None);
        assert_eq!(result.as_str(), "none");
    }

    #[test]
    fn trust_none_returns_none_even_with_dkim_fail() {
        let cfg = bare_config();
        let mb = mailbox_none();
        let result = evaluate_trust(&cfg, &mb, &auth("fail"), "anyone@example.com");
        assert_eq!(result, TrustedValue::None);
    }

    #[test]
    fn verified_allowlisted_dkim_pass_returns_true() {
        let cfg = bare_config();
        let mb = mailbox_verified(vec!["*@gmail.com".to_string()]);
        let result = evaluate_trust(&cfg, &mb, &auth("pass"), "alice@gmail.com");
        assert_eq!(result, TrustedValue::True);
        assert_eq!(result.as_str(), "true");
    }

    #[test]
    fn verified_allowlisted_dkim_fail_returns_false() {
        let cfg = bare_config();
        let mb = mailbox_verified(vec!["*@gmail.com".to_string()]);
        let result = evaluate_trust(&cfg, &mb, &auth("fail"), "alice@gmail.com");
        assert_eq!(result, TrustedValue::False);
        assert_eq!(result.as_str(), "false");
    }

    #[test]
    fn verified_not_allowlisted_dkim_pass_returns_false() {
        let cfg = bare_config();
        let mb = mailbox_verified(vec!["*@company.com".to_string()]);
        let result = evaluate_trust(&cfg, &mb, &auth("pass"), "alice@gmail.com");
        assert_eq!(result, TrustedValue::False);
    }

    #[test]
    fn verified_not_allowlisted_dkim_fail_returns_false() {
        let cfg = bare_config();
        let mb = mailbox_verified(vec!["*@company.com".to_string()]);
        let result = evaluate_trust(&cfg, &mb, &auth("fail"), "alice@gmail.com");
        assert_eq!(result, TrustedValue::False);
    }

    #[test]
    fn verified_empty_trusted_senders_dkim_pass_returns_false() {
        let cfg = bare_config();
        let mb = mailbox_verified(vec![]);
        let result = evaluate_trust(&cfg, &mb, &auth("pass"), "alice@gmail.com");
        assert_eq!(result, TrustedValue::False);
    }

    #[test]
    fn verified_dkim_none_returns_false() {
        let cfg = bare_config();
        let mb = mailbox_verified(vec!["*@gmail.com".to_string()]);
        let result = evaluate_trust(&cfg, &mb, &auth("none"), "alice@gmail.com");
        assert_eq!(result, TrustedValue::False);
    }

    #[test]
    fn verified_exact_sender_match() {
        let cfg = bare_config();
        let mb = mailbox_verified(vec!["alice@gmail.com".to_string()]);
        let result = evaluate_trust(&cfg, &mb, &auth("pass"), "alice@gmail.com");
        assert_eq!(result, TrustedValue::True);
    }

    #[test]
    fn verified_display_name_in_from() {
        let cfg = bare_config();
        let mb = mailbox_verified(vec!["*@gmail.com".to_string()]);
        let result = evaluate_trust(&cfg, &mb, &auth("pass"), "Alice Smith <alice@gmail.com>");
        assert_eq!(result, TrustedValue::True);
    }

    #[test]
    fn unknown_trust_value_returns_false() {
        let cfg = bare_config();
        let mb = MailboxConfig {
            address: "test@test.com".to_string(),
            trust: Some("typo".to_string()),
            trusted_senders: Some(vec![]),
            hooks: vec![],
        };
        let result = evaluate_trust(&cfg, &mb, &auth("pass"), "alice@gmail.com");
        assert_eq!(result, TrustedValue::False);
    }

    #[test]
    fn mailbox_inherits_global_trust_when_unset() {
        let mut cfg = bare_config();
        cfg.trust = "verified".to_string();
        cfg.trusted_senders = vec!["*@gmail.com".to_string()];

        let mb = MailboxConfig {
            address: "any@test.com".to_string(),
            trust: None,
            trusted_senders: None,
            hooks: vec![],
        };

        let t = evaluate_trust(&cfg, &mb, &auth("pass"), "alice@gmail.com");
        assert_eq!(t, TrustedValue::True);

        let t = evaluate_trust(&cfg, &mb, &auth("pass"), "bob@yahoo.com");
        assert_eq!(t, TrustedValue::False);
    }

    #[test]
    fn mailbox_trust_none_override_beats_global_verified() {
        let mut cfg = bare_config();
        cfg.trust = "verified".to_string();
        cfg.trusted_senders = vec!["*@gmail.com".to_string()];

        let mb = MailboxConfig {
            address: "public@test.com".to_string(),
            trust: Some("none".to_string()),
            trusted_senders: None,
            hooks: vec![],
        };

        let t = evaluate_trust(&cfg, &mb, &auth("fail"), "alice@gmail.com");
        assert_eq!(t, TrustedValue::None);
    }

    #[test]
    fn mailbox_trusted_senders_override_fully_replaces_global() {
        let mut cfg = bare_config();
        cfg.trust = "verified".to_string();
        cfg.trusted_senders = vec!["*@gmail.com".to_string()];

        let mb = MailboxConfig {
            address: "strict@test.com".to_string(),
            trust: None,
            trusted_senders: Some(vec!["boss@company.com".to_string()]),
            hooks: vec![],
        };

        // Global says gmail is trusted; mailbox replaces that list, so an
        // `@gmail.com` sender is no longer in the effective list.
        let t = evaluate_trust(&cfg, &mb, &auth("pass"), "alice@gmail.com");
        assert_eq!(t, TrustedValue::False);

        let t = evaluate_trust(&cfg, &mb, &auth("pass"), "boss@company.com");
        assert_eq!(t, TrustedValue::True);
    }

    #[test]
    fn serialization_roundtrip() {
        for variant in [TrustedValue::None, TrustedValue::True, TrustedValue::False] {
            let json = serde_json::to_string(&variant).unwrap();
            let back: TrustedValue = serde_json::from_str(&json).unwrap();
            assert_eq!(variant, back);
        }
    }

    /// Parity test (Sprint 50 rewrite): for `trust: verified`, an
    /// `on_receive` hook fires iff `trusted == TrustedValue::True`
    /// OR `dangerously_support_untrusted = true`. `evaluate_trust` is
    /// the sole source of truth for the `trusted` value.
    #[test]
    fn parity_hook_gate_follows_trusted_true() {
        use crate::hook::{Hook, HookEvent, should_fire_on_receive};

        let cfg = bare_config();
        let mb = mailbox_verified(vec!["*@gmail.com".to_string()]);

        let default_hook = Hook {
            name: Some("parity".to_string()),
            event: HookEvent::OnReceive,
            r#type: "cmd".to_string(),
            cmd: "true".to_string(),
            dangerously_support_untrusted: false,
            origin: crate::hook::HookOrigin::Operator,
            template: None,
            params: std::collections::BTreeMap::new(),
            run_as: None,
        };

        for (from, dkim_result) in [
            ("alice@gmail.com", "pass"),
            ("alice@gmail.com", "fail"),
            ("alice@yahoo.com", "pass"),
            ("alice@yahoo.com", "fail"),
        ] {
            let auth_results = auth(dkim_result);
            let trusted = evaluate_trust(&cfg, &mb, &auth_results, from);
            let fires = should_fire_on_receive(&default_hook, trusted);
            assert_eq!(
                fires,
                trusted == TrustedValue::True,
                "default hook gate must track trusted==true; from={from} dkim={dkim_result} \
                 trusted={trusted:?} fires={fires}"
            );
        }

        // dangerously_support_untrusted fires unconditionally.
        let mut yolo = default_hook.clone();
        yolo.dangerously_support_untrusted = true;
        for trusted in [TrustedValue::None, TrustedValue::False, TrustedValue::True] {
            assert!(should_fire_on_receive(&yolo, trusted));
        }
    }

    #[traced_test]
    #[test]
    fn evaluate_trust_emits_aimx_trust_log_none() {
        let cfg = bare_config();
        let mb = mailbox_none();
        let _ = evaluate_trust(&cfg, &mb, &auth("pass"), "anyone@example.com");
        assert!(logs_contain("aimx::trust"));
        assert!(logs_contain("trust eval"));
        assert!(logs_contain("policy=none"));
        assert!(logs_contain("result=none"));
    }

    #[traced_test]
    #[test]
    fn evaluate_trust_emits_aimx_trust_log_verified_true() {
        let cfg = bare_config();
        let mb = mailbox_verified(vec!["*@gmail.com".to_string()]);
        let _ = evaluate_trust(&cfg, &mb, &auth("pass"), "alice@gmail.com");
        assert!(logs_contain("aimx::trust"));
        assert!(logs_contain("policy=verified"));
        assert!(logs_contain("sender=alice@gmail.com"));
        assert!(logs_contain("dkim=pass"));
        assert!(logs_contain("sender_allowlisted=true"));
        assert!(logs_contain("result=true"));
    }

    #[traced_test]
    #[test]
    fn evaluate_trust_emits_aimx_trust_log_verified_false_dkim_fail() {
        let cfg = bare_config();
        let mb = mailbox_verified(vec!["*@gmail.com".to_string()]);
        let _ = evaluate_trust(&cfg, &mb, &auth("fail"), "alice@gmail.com");
        assert!(logs_contain("policy=verified"));
        assert!(logs_contain("dkim=fail"));
        assert!(logs_contain("sender_allowlisted=true"));
        assert!(logs_contain("result=false"));
    }

    #[traced_test]
    #[test]
    fn evaluate_trust_emits_aimx_trust_log_not_allowlisted() {
        let cfg = bare_config();
        let mb = mailbox_verified(vec!["*@company.com".to_string()]);
        let _ = evaluate_trust(&cfg, &mb, &auth("pass"), "alice@gmail.com");
        assert!(logs_contain("sender_allowlisted=false"));
        assert!(logs_contain("result=false"));
    }

    #[test]
    fn extract_email_for_match_no_panic_on_inverted_brackets() {
        // Regression: the pre-hardening `find('<') + find('>')` slice
        // panicked when `>` preceded `<`. After the rfind/tail-find
        // hardening, this must return a well-formed lowercased address
        // and never panic, even on adversarial sender headers.
        let out = extract_email_for_match("foo>bar<baz@example.com>");
        assert_eq!(out, "baz@example.com");

        // No `<` at all. Fall through cleanly.
        let out = extract_email_for_match("weird> input");
        assert_eq!(out, "weird> input");

        // Multiple `<`: pick the last one (matches send_handler semantics).
        let out = extract_email_for_match("<spoof@bad> real <user@example.com>");
        assert_eq!(out, "user@example.com");
    }
}
