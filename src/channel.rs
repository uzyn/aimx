use crate::config::{MailboxConfig, MatchFilter, OnReceiveRule};
use crate::frontmatter::InboundFrontmatter;
use std::path::Path;
use std::process::Command;

pub struct TriggerContext<'a> {
    pub filepath: &'a Path,
    pub metadata: &'a InboundFrontmatter,
}

/// Placeholders rejected in `on_receive.cmd` at config-load time. Pre-launch,
/// the shell-injection fix in S44-1 drops string substitution for
/// user-controlled fields (`{from}`, `{subject}`, `{to}`, `{mailbox}`,
/// `{filepath}`) and surfaces them as `AIMX_*` env vars instead. Keeping
/// legacy placeholders working would reintroduce the injection; refusing to
/// load is the safer break.
pub const LEGACY_PLACEHOLDERS: &[&str] =
    &["{from}", "{subject}", "{to}", "{mailbox}", "{filepath}"];

/// Expand the aimx-controlled `{id}` and `{date}` placeholders. Every
/// user-controlled field (`{from}`, `{subject}`, `{to}`, `{mailbox}`,
/// `{filepath}`) is deliberately **not** substituted — those are passed to
/// the trigger shell via `AIMX_*` env vars by [`execute_triggers`], which
/// the shell expands safely even for hostile payloads. `{id}` and `{date}`
/// are opaque aimx-generated strings (hex and ISO-8601), safe to splice.
pub fn substitute_template(command: &str, ctx: &TriggerContext) -> String {
    command
        .replace("{id}", &ctx.metadata.id)
        .replace("{date}", &ctx.metadata.date)
}

pub fn matches_filter(filter: &MatchFilter, metadata: &InboundFrontmatter) -> bool {
    if let Some(ref from_pattern) = filter.from {
        let from_addr = extract_email_for_match(&metadata.from);
        if !glob_match::glob_match(from_pattern, &from_addr) {
            return false;
        }
    }

    if let Some(ref subject_pattern) = filter.subject
        && !metadata
            .subject
            .to_lowercase()
            .contains(&subject_pattern.to_lowercase())
    {
        return false;
    }

    if let Some(has_attachment) = filter.has_attachment {
        let email_has_attachment = !metadata.attachments.is_empty();
        if has_attachment != email_has_attachment {
            return false;
        }
    }

    true
}

fn extract_email_for_match(from: &str) -> String {
    if let Some(start) = from.find('<')
        && let Some(end) = from.find('>')
    {
        return from[start + 1..end].to_lowercase();
    }
    from.to_lowercase()
}

pub fn should_fire(rule: &OnReceiveRule, metadata: &InboundFrontmatter) -> bool {
    if rule.rule_type != "cmd" {
        return false;
    }
    match &rule.r#match {
        Some(filter) => matches_filter(filter, metadata),
        None => true,
    }
}

pub fn is_sender_trusted(mailbox_config: &MailboxConfig, from: &str) -> bool {
    let from_lower = extract_email_for_match(from);
    for pattern in &mailbox_config.trusted_senders {
        if glob_match::glob_match(pattern, &from_lower) {
            return true;
        }
    }
    false
}

/// Determine whether channel triggers should fire for this email.
///
/// v1 semantics (preserved intentionally): for `trust: verified`,
/// triggers fire when the sender is allowlisted OR DKIM passes. This
/// is deliberately looser than `trust::evaluate_trust()`, which
/// requires BOTH allowlisted AND DKIM pass for `trusted = "true"`.
/// The trigger gate keeps the "allowlisted senders skip verification"
/// affordance intact; the `trusted` frontmatter field is the strict
/// evaluation surfaced to agents and operators. See S38-1 rationale.
pub fn should_execute_triggers(
    mailbox_config: &MailboxConfig,
    metadata: &InboundFrontmatter,
) -> bool {
    if mailbox_config.trust == "none" {
        return true;
    }

    if is_sender_trusted(mailbox_config, &metadata.from) {
        return true;
    }

    if mailbox_config.trust == "verified" {
        return metadata.dkim == "pass";
    }

    eprintln!(
        "aimx: unknown trust value '{}', denying triggers (fail-closed)",
        mailbox_config.trust
    );
    false
}

pub fn execute_triggers(mailbox_config: &MailboxConfig, ctx: &TriggerContext) {
    if !should_execute_triggers(mailbox_config, ctx.metadata) {
        return;
    }

    for rule in &mailbox_config.on_receive {
        if !should_fire(rule, ctx.metadata) {
            continue;
        }

        let expanded = substitute_template(&rule.command, ctx);

        // User-controlled fields ride on env vars, NOT string substitution.
        // The shell expands `$AIMX_FROM` literally — attacker-controlled
        // payload in `From:` can no longer break out of quotes, add extra
        // commands, or exploit `$()`/backticks.
        let filepath = ctx.filepath.to_string_lossy().into_owned();
        let mut command = Command::new("sh");
        command
            .arg("-c")
            .arg(&expanded)
            .env("AIMX_FROM", &ctx.metadata.from)
            .env("AIMX_SUBJECT", &ctx.metadata.subject)
            .env("AIMX_TO", &ctx.metadata.to)
            .env("AIMX_MAILBOX", &ctx.metadata.mailbox)
            .env("AIMX_FILEPATH", &filepath);

        match command.output() {
            Ok(output) => {
                if !output.status.success() {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    eprintln!(
                        "aimx: trigger failed (exit {}): {expanded}\n  {stderr}",
                        output.status.code().unwrap_or(-1)
                    );
                }
            }
            Err(e) => {
                eprintln!("aimx: trigger exec error: {expanded}\n  {e}");
            }
        }
    }
}

/// Error type returned by [`validate_on_receive_commands`] when a config
/// contains a legacy placeholder that was dropped by S44-1.
#[derive(Debug)]
pub struct LegacyPlaceholderError {
    pub mailbox: String,
    pub placeholder: String,
}

impl std::fmt::Display for LegacyPlaceholderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "mailbox '{}': legacy placeholder '{}' in on_receive.command is no longer supported. \
             Migrate to AIMX_* env vars: replace '{{from}}' with \"$AIMX_FROM\", \
             '{{subject}}' with \"$AIMX_SUBJECT\", '{{to}}' with \"$AIMX_TO\", \
             '{{mailbox}}' with \"$AIMX_MAILBOX\", '{{filepath}}' with \"$AIMX_FILEPATH\". \
             The aimx-controlled placeholders {{id}} and {{date}} still work. \
             See book/channel-recipes.md for updated examples.",
            self.mailbox, self.placeholder
        )
    }
}

impl std::error::Error for LegacyPlaceholderError {}

/// Scan every `on_receive.command` in the config for legacy user-controlled
/// placeholders. Returns the first offender so the operator can fix
/// mailboxes one at a time.
pub fn validate_on_receive_commands(
    mailboxes: &std::collections::HashMap<String, MailboxConfig>,
) -> Result<(), LegacyPlaceholderError> {
    // Sort keys so the error is deterministic when multiple mailboxes are
    // broken — stable ordering keeps CI logs and test expectations sane.
    let mut names: Vec<&String> = mailboxes.keys().collect();
    names.sort();
    for name in names {
        let mailbox = &mailboxes[name];
        for rule in &mailbox.on_receive {
            for placeholder in LEGACY_PLACEHOLDERS {
                if rule.command.contains(placeholder) {
                    return Err(LegacyPlaceholderError {
                        mailbox: name.clone(),
                        placeholder: (*placeholder).to_string(),
                    });
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{MatchFilter, OnReceiveRule};
    use crate::frontmatter::{AttachmentMeta, InboundFrontmatter};
    use std::path::PathBuf;

    fn sample_metadata() -> InboundFrontmatter {
        InboundFrontmatter {
            id: "2025-06-01-001".to_string(),
            message_id: "<test@example.com>".to_string(),
            thread_id: "0123456789abcdef".to_string(),
            from: "alice@gmail.com".to_string(),
            to: "agent@test.com".to_string(),
            cc: None,
            reply_to: None,
            delivered_to: "agent@test.com".to_string(),
            subject: "Hello World".to_string(),
            date: "2025-06-01T12:00:00Z".to_string(),
            received_at: "2025-06-01T12:00:01Z".to_string(),
            received_from_ip: None,
            size_bytes: 100,
            in_reply_to: None,
            references: None,
            attachments: vec![],
            list_id: None,
            auto_submitted: None,
            dkim: "none".to_string(),
            spf: "none".to_string(),
            dmarc: "none".to_string(),
            trusted: "none".to_string(),
            mailbox: "catchall".to_string(),
            read: false,
            labels: vec![],
        }
    }

    fn sample_ctx<'a>(filepath: &'a Path, metadata: &'a InboundFrontmatter) -> TriggerContext<'a> {
        TriggerContext { filepath, metadata }
    }

    #[test]
    fn substitute_only_id_and_date() {
        // S44-1: string substitution is restricted to aimx-controlled fields.
        // Every user-controlled placeholder must pass through untouched so
        // the shell never sees attacker-controlled bytes as code.
        let meta = sample_metadata();
        let filepath = PathBuf::from("/var/lib/aimx/catchall/2025-06-01-001.md");
        let ctx = sample_ctx(&filepath, &meta);

        let result = substitute_template(
            "echo {filepath} {from} {to} {subject} {mailbox} {id} {date}",
            &ctx,
        );
        assert!(
            result.contains("2025-06-01-001"),
            "{{id}} must expand: {result}"
        );
        assert!(
            result.contains("2025-06-01T12:00:00Z"),
            "{{date}} must expand: {result}"
        );
        // Legacy placeholders MUST remain literal in the expanded script —
        // the config loader refuses configs that carry them, but the
        // substitute_template function itself simply leaves them alone.
        assert!(
            result.contains("{filepath}"),
            "{{filepath}} must NOT be substituted: {result}"
        );
        assert!(
            result.contains("{from}"),
            "{{from}} must NOT be substituted: {result}"
        );
        assert!(
            result.contains("{to}"),
            "{{to}} must NOT be substituted: {result}"
        );
        assert!(
            result.contains("{subject}"),
            "{{subject}} must NOT be substituted: {result}"
        );
        assert!(
            result.contains("{mailbox}"),
            "{{mailbox}} must NOT be substituted: {result}"
        );
    }

    #[test]
    fn substitute_leaves_legacy_placeholders_literal() {
        // Paranoid repro of the T8 bug from the 2026-04-17 manual test run:
        // a `From:` with a display name + angle brackets would previously
        // leak unescaped into the shell.
        let mut meta = sample_metadata();
        meta.from = "U-Zyn Chua <chua@uzyn.com>".to_string();
        meta.subject = "Re: \"urgent\" & important".to_string();
        let filepath = PathBuf::from("/tmp/test.md");
        let ctx = sample_ctx(&filepath, &meta);

        let result = substitute_template("echo {from} {subject}", &ctx);
        // The substitute step does not touch these — the env-var path is
        // what safely delivers them to the shell.
        assert_eq!(result, "echo {from} {subject}");
    }

    #[test]
    fn substitute_no_variables() {
        let meta = sample_metadata();
        let filepath = PathBuf::from("/tmp/test.md");
        let ctx = sample_ctx(&filepath, &meta);

        let result = substitute_template("echo hello", &ctx);
        assert_eq!(result, "echo hello");
    }

    #[test]
    fn match_from_glob_match() {
        let meta = sample_metadata();
        let filter = MatchFilter {
            from: Some("*@gmail.com".to_string()),
            subject: None,
            has_attachment: None,
        };
        assert!(matches_filter(&filter, &meta));
    }

    #[test]
    fn match_from_glob_mismatch() {
        let meta = sample_metadata();
        let filter = MatchFilter {
            from: Some("*@yahoo.com".to_string()),
            subject: None,
            has_attachment: None,
        };
        assert!(!matches_filter(&filter, &meta));
    }

    #[test]
    fn match_from_glob_with_display_name() {
        let mut meta = sample_metadata();
        meta.from = "Alice Smith <alice@gmail.com>".to_string();
        let filter = MatchFilter {
            from: Some("*@gmail.com".to_string()),
            subject: None,
            has_attachment: None,
        };
        assert!(matches_filter(&filter, &meta));
    }

    #[test]
    fn match_subject_substring() {
        let meta = sample_metadata();
        let filter = MatchFilter {
            from: None,
            subject: Some("hello".to_string()),
            has_attachment: None,
        };
        assert!(matches_filter(&filter, &meta));
    }

    #[test]
    fn match_subject_case_insensitive() {
        let meta = sample_metadata();
        let filter = MatchFilter {
            from: None,
            subject: Some("HELLO".to_string()),
            has_attachment: None,
        };
        assert!(matches_filter(&filter, &meta));
    }

    #[test]
    fn match_subject_mismatch() {
        let meta = sample_metadata();
        let filter = MatchFilter {
            from: None,
            subject: Some("goodbye".to_string()),
            has_attachment: None,
        };
        assert!(!matches_filter(&filter, &meta));
    }

    #[test]
    fn match_has_attachment_true_with_no_attachments() {
        let meta = sample_metadata();
        let filter = MatchFilter {
            from: None,
            subject: None,
            has_attachment: Some(true),
        };
        assert!(!matches_filter(&filter, &meta));
    }

    #[test]
    fn match_has_attachment_true_with_attachments() {
        let mut meta = sample_metadata();
        meta.attachments = vec![AttachmentMeta {
            filename: "file.txt".to_string(),
            content_type: "text/plain".to_string(),
            size: 100,
            path: "attachments/file.txt".to_string(),
        }];
        let filter = MatchFilter {
            from: None,
            subject: None,
            has_attachment: Some(true),
        };
        assert!(matches_filter(&filter, &meta));
    }

    #[test]
    fn match_has_attachment_false_with_no_attachments() {
        let meta = sample_metadata();
        let filter = MatchFilter {
            from: None,
            subject: None,
            has_attachment: Some(false),
        };
        assert!(matches_filter(&filter, &meta));
    }

    #[test]
    fn match_has_attachment_false_with_attachments() {
        let mut meta = sample_metadata();
        meta.attachments = vec![AttachmentMeta {
            filename: "file.txt".to_string(),
            content_type: "text/plain".to_string(),
            size: 100,
            path: "attachments/file.txt".to_string(),
        }];
        let filter = MatchFilter {
            from: None,
            subject: None,
            has_attachment: Some(false),
        };
        assert!(!matches_filter(&filter, &meta));
    }

    #[test]
    fn and_logic_all_match() {
        let mut meta = sample_metadata();
        meta.attachments = vec![AttachmentMeta {
            filename: "file.txt".to_string(),
            content_type: "text/plain".to_string(),
            size: 100,
            path: "attachments/file.txt".to_string(),
        }];
        let filter = MatchFilter {
            from: Some("*@gmail.com".to_string()),
            subject: Some("hello".to_string()),
            has_attachment: Some(true),
        };
        assert!(matches_filter(&filter, &meta));
    }

    #[test]
    fn and_logic_partial_match_from_fails() {
        let meta = sample_metadata();
        let filter = MatchFilter {
            from: Some("*@yahoo.com".to_string()),
            subject: Some("hello".to_string()),
            has_attachment: None,
        };
        assert!(!matches_filter(&filter, &meta));
    }

    #[test]
    fn and_logic_partial_match_subject_fails() {
        let meta = sample_metadata();
        let filter = MatchFilter {
            from: Some("*@gmail.com".to_string()),
            subject: Some("nonexistent".to_string()),
            has_attachment: None,
        };
        assert!(!matches_filter(&filter, &meta));
    }

    #[test]
    fn and_logic_partial_match_attachment_fails() {
        let meta = sample_metadata();
        let filter = MatchFilter {
            from: Some("*@gmail.com".to_string()),
            subject: Some("hello".to_string()),
            has_attachment: Some(true),
        };
        assert!(!matches_filter(&filter, &meta));
    }

    #[test]
    fn should_fire_cmd_no_match_always_fires() {
        let meta = sample_metadata();
        let rule = OnReceiveRule {
            rule_type: "cmd".to_string(),
            command: "echo test".to_string(),
            r#match: None,
        };
        assert!(should_fire(&rule, &meta));
    }

    #[test]
    fn should_fire_non_cmd_type_does_not_fire() {
        let meta = sample_metadata();
        let rule = OnReceiveRule {
            rule_type: "webhook".to_string(),
            command: "echo test".to_string(),
            r#match: None,
        };
        assert!(!should_fire(&rule, &meta));
    }

    #[test]
    fn should_fire_with_matching_filter() {
        let meta = sample_metadata();
        let rule = OnReceiveRule {
            rule_type: "cmd".to_string(),
            command: "echo test".to_string(),
            r#match: Some(MatchFilter {
                from: Some("*@gmail.com".to_string()),
                subject: None,
                has_attachment: None,
            }),
        };
        assert!(should_fire(&rule, &meta));
    }

    #[test]
    fn should_fire_with_non_matching_filter() {
        let meta = sample_metadata();
        let rule = OnReceiveRule {
            rule_type: "cmd".to_string(),
            command: "echo test".to_string(),
            r#match: Some(MatchFilter {
                from: Some("*@yahoo.com".to_string()),
                subject: None,
                has_attachment: None,
            }),
        };
        assert!(!should_fire(&rule, &meta));
    }

    #[test]
    fn execute_triggers_success() {
        let tmp = tempfile::TempDir::new().unwrap();
        let marker = tmp.path().join("triggered");
        let meta = sample_metadata();
        let filepath = tmp.path().join("test.md");
        let ctx = sample_ctx(&filepath, &meta);

        let config = MailboxConfig {
            address: "*@test.com".to_string(),
            trust: "none".to_string(),
            trusted_senders: vec![],
            on_receive: vec![OnReceiveRule {
                rule_type: "cmd".to_string(),
                command: format!("touch {}", marker.to_string_lossy()),
                r#match: None,
            }],
        };

        execute_triggers(&config, &ctx);
        assert!(marker.exists());
    }

    #[test]
    fn execute_triggers_multiple_in_order() {
        let tmp = tempfile::TempDir::new().unwrap();
        let log_file = tmp.path().join("order.log");
        let meta = sample_metadata();
        let filepath = tmp.path().join("test.md");
        let ctx = sample_ctx(&filepath, &meta);

        let config = MailboxConfig {
            address: "*@test.com".to_string(),
            trust: "none".to_string(),
            trusted_senders: vec![],
            on_receive: vec![
                OnReceiveRule {
                    rule_type: "cmd".to_string(),
                    command: format!("echo first >> {}", log_file.to_string_lossy()),
                    r#match: None,
                },
                OnReceiveRule {
                    rule_type: "cmd".to_string(),
                    command: format!("echo second >> {}", log_file.to_string_lossy()),
                    r#match: None,
                },
            ],
        };

        execute_triggers(&config, &ctx);
        let content = std::fs::read_to_string(&log_file).unwrap();
        let lines: Vec<&str> = content.trim().lines().collect();
        assert_eq!(lines, vec!["first", "second"]);
    }

    #[test]
    fn execute_triggers_failure_does_not_panic() {
        let meta = sample_metadata();
        let filepath = PathBuf::from("/tmp/test.md");
        let ctx = sample_ctx(&filepath, &meta);

        let config = MailboxConfig {
            address: "*@test.com".to_string(),
            trust: "none".to_string(),
            trusted_senders: vec![],
            on_receive: vec![OnReceiveRule {
                rule_type: "cmd".to_string(),
                command: "false".to_string(),
                r#match: None,
            }],
        };

        execute_triggers(&config, &ctx);
    }

    #[test]
    fn execute_triggers_no_rules() {
        let meta = sample_metadata();
        let filepath = PathBuf::from("/tmp/test.md");
        let ctx = sample_ctx(&filepath, &meta);

        let config = MailboxConfig {
            address: "*@test.com".to_string(),
            trust: "none".to_string(),
            trusted_senders: vec![],
            on_receive: vec![],
        };

        execute_triggers(&config, &ctx);
    }

    #[test]
    fn execute_triggers_skips_non_matching() {
        let tmp = tempfile::TempDir::new().unwrap();
        let marker = tmp.path().join("should_not_exist");
        let meta = sample_metadata();
        let filepath = tmp.path().join("test.md");
        let ctx = sample_ctx(&filepath, &meta);

        let config = MailboxConfig {
            address: "*@test.com".to_string(),
            trust: "none".to_string(),
            trusted_senders: vec![],
            on_receive: vec![OnReceiveRule {
                rule_type: "cmd".to_string(),
                command: format!("touch {}", marker.to_string_lossy()),
                r#match: Some(MatchFilter {
                    from: Some("*@yahoo.com".to_string()),
                    subject: None,
                    has_attachment: None,
                }),
            }],
        };

        execute_triggers(&config, &ctx);
        assert!(!marker.exists());
    }

    #[test]
    fn execute_triggers_with_env_vars() {
        // S44-1: the env-var path is now how user-controlled values reach
        // the trigger shell. `$AIMX_FROM` / `$AIMX_SUBJECT` expand inside
        // double quotes, so whitespace and metacharacters are preserved but
        // never interpreted as shell code.
        let tmp = tempfile::TempDir::new().unwrap();
        let output_file = tmp.path().join("output.txt");
        let meta = sample_metadata();
        let filepath = tmp.path().join("catchall/2025-06-01-001.md");
        let ctx = sample_ctx(&filepath, &meta);

        let config = MailboxConfig {
            address: "*@test.com".to_string(),
            trust: "none".to_string(),
            trusted_senders: vec![],
            on_receive: vec![OnReceiveRule {
                rule_type: "cmd".to_string(),
                command: format!(
                    "printf '%s %s\\n' \"$AIMX_FROM\" \"$AIMX_SUBJECT\" > {}",
                    output_file.to_string_lossy()
                ),
                r#match: None,
            }],
        };

        execute_triggers(&config, &ctx);
        let content = std::fs::read_to_string(&output_file).unwrap();
        assert!(content.contains("alice@gmail.com"), "got: {content:?}");
        assert!(content.contains("Hello World"), "got: {content:?}");
    }

    /// Helper: fire a single-shot trigger that writes `$AIMX_*` env vars to
    /// a log file. Used by the injection-attempt tests below to verify the
    /// hostile payload is preserved verbatim (i.e. no command was executed
    /// and no escape sequence was interpreted).
    fn run_env_capture_trigger(
        mut meta_mutator: impl FnMut(&mut InboundFrontmatter),
    ) -> (tempfile::TempDir, String) {
        let tmp = tempfile::TempDir::new().unwrap();
        let log = tmp.path().join("env.log");
        let mut meta = sample_metadata();
        meta_mutator(&mut meta);
        let filepath = tmp.path().join("test.md");
        let ctx = sample_ctx(&filepath, &meta);

        let config = MailboxConfig {
            address: "*@test.com".to_string(),
            trust: "none".to_string(),
            trusted_senders: vec![],
            on_receive: vec![OnReceiveRule {
                rule_type: "cmd".to_string(),
                command: format!(
                    "printf 'FROM=%s\\nSUBJECT=%s\\nTO=%s\\nMAILBOX=%s\\nFILEPATH=%s\\n' \
                     \"$AIMX_FROM\" \"$AIMX_SUBJECT\" \"$AIMX_TO\" \"$AIMX_MAILBOX\" \"$AIMX_FILEPATH\" > {}",
                    log.to_string_lossy()
                ),
                r#match: None,
            }],
        };
        execute_triggers(&config, &ctx);
        let content = std::fs::read_to_string(&log).unwrap();
        (tmp, content)
    }

    #[test]
    fn env_var_preserves_angle_bracket_from() {
        // T8 repro: `U-Zyn Chua <chua@uzyn.com>`. Previously this broke
        // shell-escape quoting on the substitution path. With env-vars, the
        // payload lands verbatim — no redirection, no execution.
        let (_tmp, content) = run_env_capture_trigger(|m| {
            m.from = "U-Zyn Chua <chua@uzyn.com>".to_string();
        });
        assert!(
            content.contains("FROM=U-Zyn Chua <chua@uzyn.com>"),
            "got: {content:?}"
        );
    }

    #[test]
    fn env_var_preserves_backtick_injection() {
        let (_tmp, content) = run_env_capture_trigger(|m| {
            m.from = "`whoami`@attacker.com".to_string();
        });
        assert!(
            content.contains("FROM=`whoami`@attacker.com"),
            "backticks must land verbatim, not execute: {content:?}"
        );
    }

    #[test]
    fn env_var_preserves_dollar_paren_injection() {
        let (_tmp, content) = run_env_capture_trigger(|m| {
            m.subject = "$(rm -rf /)".to_string();
        });
        assert!(
            content.contains("SUBJECT=$(rm -rf /)"),
            "$(..) must land verbatim: {content:?}"
        );
    }

    #[test]
    fn env_var_preserves_semicolon_command() {
        let (_tmp, content) = run_env_capture_trigger(|m| {
            m.subject = "foo; ls".to_string();
        });
        assert!(
            content.contains("SUBJECT=foo; ls"),
            "semicolons must land verbatim: {content:?}"
        );
    }

    #[test]
    fn env_var_preserves_newline_subject() {
        let (_tmp, content) = run_env_capture_trigger(|m| {
            m.subject = "foo\nbar".to_string();
        });
        // Newlines embed via env var (execve preserves them). The shell's
        // `printf '%s'` writes them verbatim — so we assert on both halves.
        assert!(content.contains("SUBJECT=foo"), "got: {content:?}");
        assert!(content.contains("bar"), "got: {content:?}");
    }

    #[test]
    fn env_var_preserves_mixed_quotes() {
        let (_tmp, content) = run_env_capture_trigger(|m| {
            m.subject = "O'Brien says \"hi\"".to_string();
        });
        assert!(
            content.contains("SUBJECT=O'Brien says \"hi\""),
            "got: {content:?}"
        );
    }

    #[test]
    fn env_var_injection_does_not_execute_marker_command() {
        // Integration-level repro: if the attacker could sneak `$(...)`
        // into the command via `{subject}` substitution, a marker file
        // would appear. Env-var expansion under the shell preserves the
        // literal string, so nothing executes.
        let tmp = tempfile::TempDir::new().unwrap();
        let marker = tmp.path().join("pwned");
        let mut meta = sample_metadata();
        meta.subject = format!("$(touch {})", marker.to_string_lossy());
        let filepath = tmp.path().join("test.md");
        let ctx = sample_ctx(&filepath, &meta);

        let config = MailboxConfig {
            address: "*@test.com".to_string(),
            trust: "none".to_string(),
            trusted_senders: vec![],
            on_receive: vec![OnReceiveRule {
                rule_type: "cmd".to_string(),
                // Deliberately echo the env var so the payload reaches the
                // shell via the safe path. The `$(...)` inside $AIMX_SUBJECT
                // must NOT execute.
                command: "echo \"$AIMX_SUBJECT\" > /dev/null".to_string(),
                r#match: None,
            }],
        };

        execute_triggers(&config, &ctx);
        assert!(
            !marker.exists(),
            "env-var payload MUST NOT be interpreted as shell code"
        );
    }

    #[test]
    fn validate_on_receive_rejects_legacy_from() {
        use std::collections::HashMap;
        let mut mailboxes = HashMap::new();
        mailboxes.insert(
            "support".to_string(),
            MailboxConfig {
                address: "support@test.com".to_string(),
                trust: "none".to_string(),
                trusted_senders: vec![],
                on_receive: vec![OnReceiveRule {
                    rule_type: "cmd".to_string(),
                    command: "echo {from}".to_string(),
                    r#match: None,
                }],
            },
        );
        let err = validate_on_receive_commands(&mailboxes).unwrap_err();
        assert_eq!(err.mailbox, "support");
        assert_eq!(err.placeholder, "{from}");
        let msg = err.to_string();
        assert!(msg.contains("support"), "mailbox named: {msg}");
        assert!(msg.contains("{from}"), "placeholder named: {msg}");
        assert!(msg.contains("AIMX_FROM"), "migration hinted: {msg}");
    }

    #[test]
    fn validate_on_receive_rejects_legacy_filepath() {
        use std::collections::HashMap;
        let mut mailboxes = HashMap::new();
        mailboxes.insert(
            "ops".to_string(),
            MailboxConfig {
                address: "ops@test.com".to_string(),
                trust: "none".to_string(),
                trusted_senders: vec![],
                on_receive: vec![OnReceiveRule {
                    rule_type: "cmd".to_string(),
                    command: "cat {filepath}".to_string(),
                    r#match: None,
                }],
            },
        );
        let err = validate_on_receive_commands(&mailboxes).unwrap_err();
        assert_eq!(err.placeholder, "{filepath}");
    }

    #[test]
    fn validate_on_receive_accepts_id_and_date() {
        use std::collections::HashMap;
        let mut mailboxes = HashMap::new();
        mailboxes.insert(
            "log".to_string(),
            MailboxConfig {
                address: "log@test.com".to_string(),
                trust: "none".to_string(),
                trusted_senders: vec![],
                on_receive: vec![OnReceiveRule {
                    rule_type: "cmd".to_string(),
                    command: "echo id={id} date={date}".to_string(),
                    r#match: None,
                }],
            },
        );
        assert!(validate_on_receive_commands(&mailboxes).is_ok());
    }

    #[test]
    fn validate_on_receive_accepts_env_var_recipe() {
        use std::collections::HashMap;
        let mut mailboxes = HashMap::new();
        mailboxes.insert(
            "inbox".to_string(),
            MailboxConfig {
                address: "inbox@test.com".to_string(),
                trust: "none".to_string(),
                trusted_senders: vec![],
                on_receive: vec![OnReceiveRule {
                    rule_type: "cmd".to_string(),
                    command: "echo \"$AIMX_FROM\" \"$AIMX_SUBJECT\"".to_string(),
                    r#match: None,
                }],
            },
        );
        assert!(validate_on_receive_commands(&mailboxes).is_ok());
    }

    #[test]
    fn trust_none_fires_always() {
        let mut meta = sample_metadata();
        meta.dkim = "fail".to_string();
        let config = MailboxConfig {
            address: "*@test.com".to_string(),
            trust: "none".to_string(),
            trusted_senders: vec![],
            on_receive: vec![],
        };
        assert!(should_execute_triggers(&config, &meta));
    }

    #[test]
    fn trust_verified_blocks_on_dkim_fail() {
        let mut meta = sample_metadata();
        meta.dkim = "fail".to_string();
        let config = MailboxConfig {
            address: "*@test.com".to_string(),
            trust: "verified".to_string(),
            trusted_senders: vec![],
            on_receive: vec![],
        };
        assert!(!should_execute_triggers(&config, &meta));
    }

    #[test]
    fn trust_verified_blocks_on_dkim_none() {
        let meta = sample_metadata();
        let config = MailboxConfig {
            address: "*@test.com".to_string(),
            trust: "verified".to_string(),
            trusted_senders: vec![],
            on_receive: vec![],
        };
        assert!(!should_execute_triggers(&config, &meta));
    }

    #[test]
    fn trust_verified_allows_on_dkim_pass() {
        let mut meta = sample_metadata();
        meta.dkim = "pass".to_string();
        let config = MailboxConfig {
            address: "*@test.com".to_string(),
            trust: "verified".to_string(),
            trusted_senders: vec![],
            on_receive: vec![],
        };
        assert!(should_execute_triggers(&config, &meta));
    }

    #[test]
    fn trusted_senders_bypasses_dkim_check() {
        let mut meta = sample_metadata();
        meta.dkim = "fail".to_string();
        let config = MailboxConfig {
            address: "*@test.com".to_string(),
            trust: "verified".to_string(),
            trusted_senders: vec!["*@gmail.com".to_string()],
            on_receive: vec![],
        };
        assert!(should_execute_triggers(&config, &meta));
    }

    #[test]
    fn trusted_senders_exact_match() {
        let mut meta = sample_metadata();
        meta.dkim = "fail".to_string();
        let config = MailboxConfig {
            address: "*@test.com".to_string(),
            trust: "verified".to_string(),
            trusted_senders: vec!["alice@gmail.com".to_string()],
            on_receive: vec![],
        };
        assert!(should_execute_triggers(&config, &meta));
    }

    #[test]
    fn trusted_senders_no_match_falls_through() {
        let mut meta = sample_metadata();
        meta.dkim = "fail".to_string();
        let config = MailboxConfig {
            address: "*@test.com".to_string(),
            trust: "verified".to_string(),
            trusted_senders: vec!["*@yahoo.com".to_string()],
            on_receive: vec![],
        };
        assert!(!should_execute_triggers(&config, &meta));
    }

    #[test]
    fn trusted_senders_with_display_name() {
        let mut meta = sample_metadata();
        meta.from = "Alice Smith <alice@gmail.com>".to_string();
        meta.dkim = "fail".to_string();
        let config = MailboxConfig {
            address: "*@test.com".to_string(),
            trust: "verified".to_string(),
            trusted_senders: vec!["*@gmail.com".to_string()],
            on_receive: vec![],
        };
        assert!(should_execute_triggers(&config, &meta));
    }

    #[test]
    fn trust_verified_trigger_actually_blocked() {
        let tmp = tempfile::TempDir::new().unwrap();
        let marker = tmp.path().join("should_not_exist");
        let mut meta = sample_metadata();
        meta.dkim = "fail".to_string();
        let filepath = tmp.path().join("test.md");
        let ctx = sample_ctx(&filepath, &meta);

        let config = MailboxConfig {
            address: "*@test.com".to_string(),
            trust: "verified".to_string(),
            trusted_senders: vec![],
            on_receive: vec![OnReceiveRule {
                rule_type: "cmd".to_string(),
                command: format!("touch {}", marker.to_string_lossy()),
                r#match: None,
            }],
        };

        execute_triggers(&config, &ctx);
        assert!(
            !marker.exists(),
            "Trigger should not have fired for DKIM fail with trust=verified"
        );
    }

    #[test]
    fn trust_verified_trigger_fires_on_pass() {
        let tmp = tempfile::TempDir::new().unwrap();
        let marker = tmp.path().join("triggered");
        let mut meta = sample_metadata();
        meta.dkim = "pass".to_string();
        let filepath = tmp.path().join("test.md");
        let ctx = sample_ctx(&filepath, &meta);

        let config = MailboxConfig {
            address: "*@test.com".to_string(),
            trust: "verified".to_string(),
            trusted_senders: vec![],
            on_receive: vec![OnReceiveRule {
                rule_type: "cmd".to_string(),
                command: format!("touch {}", marker.to_string_lossy()),
                r#match: None,
            }],
        };

        execute_triggers(&config, &ctx);
        assert!(
            marker.exists(),
            "Trigger should fire for DKIM pass with trust=verified"
        );
    }

    #[test]
    fn invalid_trust_value_denies_triggers() {
        let mut meta = sample_metadata();
        meta.dkim = "pass".to_string();
        let config = MailboxConfig {
            address: "*@test.com".to_string(),
            trust: "verfied".to_string(),
            trusted_senders: vec![],
            on_receive: vec![],
        };
        assert!(!should_execute_triggers(&config, &meta));
    }

    #[test]
    fn invalid_trust_value_blocks_trigger_execution() {
        let tmp = tempfile::TempDir::new().unwrap();
        let marker = tmp.path().join("should_not_exist");
        let meta = sample_metadata();
        let filepath = tmp.path().join("test.md");
        let ctx = sample_ctx(&filepath, &meta);

        let config = MailboxConfig {
            address: "*@test.com".to_string(),
            trust: "typo".to_string(),
            trusted_senders: vec![],
            on_receive: vec![OnReceiveRule {
                rule_type: "cmd".to_string(),
                command: format!("touch {}", marker.to_string_lossy()),
                r#match: None,
            }],
        };

        execute_triggers(&config, &ctx);
        assert!(
            !marker.exists(),
            "Trigger should not fire for unknown trust value"
        );
    }
}
