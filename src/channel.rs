use crate::config::{MailboxConfig, MatchFilter, OnReceiveRule};
use crate::ingest::EmailMetadata;
use std::path::Path;
use std::process::Command;

pub struct TriggerContext<'a> {
    pub filepath: &'a Path,
    pub metadata: &'a EmailMetadata,
}

pub fn substitute_template(command: &str, ctx: &TriggerContext) -> String {
    command
        .replace("{filepath}", &ctx.filepath.to_string_lossy())
        .replace("{from}", &ctx.metadata.from)
        .replace("{to}", &ctx.metadata.to)
        .replace("{subject}", &ctx.metadata.subject)
        .replace("{mailbox}", &ctx.metadata.mailbox)
        .replace("{id}", &ctx.metadata.id)
        .replace("{date}", &ctx.metadata.date)
}

pub fn matches_filter(filter: &MatchFilter, metadata: &EmailMetadata) -> bool {
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

pub fn should_fire(rule: &OnReceiveRule, metadata: &EmailMetadata) -> bool {
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

pub fn should_execute_triggers(mailbox_config: &MailboxConfig, metadata: &EmailMetadata) -> bool {
    if mailbox_config.trust == "none" {
        return true;
    }

    if is_sender_trusted(mailbox_config, &metadata.from) {
        return true;
    }

    if mailbox_config.trust == "verified" {
        return metadata.dkim == "pass";
    }

    true
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

        match Command::new("sh").arg("-c").arg(&expanded).output() {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{MatchFilter, OnReceiveRule};
    use crate::ingest::{AttachmentMeta, EmailMetadata};
    use std::path::PathBuf;

    fn sample_metadata() -> EmailMetadata {
        EmailMetadata {
            id: "2025-06-01-001".to_string(),
            message_id: "<test@example.com>".to_string(),
            from: "alice@gmail.com".to_string(),
            to: "agent@test.com".to_string(),
            subject: "Hello World".to_string(),
            date: "2025-06-01T12:00:00Z".to_string(),
            in_reply_to: "".to_string(),
            references: "".to_string(),
            attachments: vec![],
            mailbox: "catchall".to_string(),
            read: false,
            dkim: "none".to_string(),
            spf: "none".to_string(),
        }
    }

    fn sample_ctx<'a>(filepath: &'a Path, metadata: &'a EmailMetadata) -> TriggerContext<'a> {
        TriggerContext { filepath, metadata }
    }

    #[test]
    fn substitute_all_variables() {
        let meta = sample_metadata();
        let filepath = PathBuf::from("/var/lib/aimx/catchall/2025-06-01-001.md");
        let ctx = sample_ctx(&filepath, &meta);

        let result = substitute_template(
            "echo {filepath} {from} {to} {subject} {mailbox} {id} {date}",
            &ctx,
        );
        assert!(result.contains("/var/lib/aimx/catchall/2025-06-01-001.md"));
        assert!(result.contains("alice@gmail.com"));
        assert!(result.contains("agent@test.com"));
        assert!(result.contains("Hello World"));
        assert!(result.contains("catchall"));
        assert!(result.contains("2025-06-01-001"));
        assert!(result.contains("2025-06-01T12:00:00Z"));
    }

    #[test]
    fn substitute_special_characters_in_values() {
        let mut meta = sample_metadata();
        meta.from = "O'Brien <obrien@test.com>".to_string();
        meta.subject = "Re: \"urgent\" & important".to_string();
        let filepath = PathBuf::from("/tmp/test.md");
        let ctx = sample_ctx(&filepath, &meta);

        let result = substitute_template("echo '{from}' '{subject}'", &ctx);
        assert!(result.contains("O'Brien <obrien@test.com>"));
        assert!(result.contains("Re: \"urgent\" & important"));
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
    fn execute_triggers_with_template_variables() {
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
                    "echo '{{from}} {{subject}}' > {}",
                    output_file.to_string_lossy()
                ),
                r#match: None,
            }],
        };

        execute_triggers(&config, &ctx);
        let content = std::fs::read_to_string(&output_file).unwrap();
        assert!(content.contains("alice@gmail.com"));
        assert!(content.contains("Hello World"));
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
}
