use crate::config::{MailboxConfig, MatchFilter, OnReceiveRule};
use crate::frontmatter::InboundFrontmatter;
use std::path::Path;
use std::process::Command;

pub struct TriggerContext<'a> {
    pub filepath: &'a Path,
    pub metadata: &'a InboundFrontmatter,
}

/// Expand the aimx-controlled `{id}` and `{date}` placeholders. User-controlled
/// fields (`from`, `subject`, `to`, `mailbox`, `filepath`) are deliberately
/// **not** substituted — those are passed to the trigger shell via `AIMX_*`
/// env vars by [`execute_triggers`], which the shell expands safely even for
/// hostile payloads. `{id}` and `{date}` are opaque aimx-generated strings
/// (hex and ISO-8601), safe to splice.
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
        //
        // Defense in depth: `.env_clear()` wipes the parent-process env
        // before we selectively re-add `PATH`/`HOME` and the `AIMX_*`
        // variables. A stray `LD_PRELOAD` / `SSH_AUTH_SOCK` / credential
        // env var set on the daemon process cannot leak into a trigger
        // subshell. `PATH` is preserved so common commands (`curl`,
        // `python`, ...) still resolve; `HOME` is preserved so tools that
        // read per-user config files still work for the service account.
        let filepath = ctx.filepath.to_string_lossy().into_owned();
        let mut command = Command::new("sh");
        command.env_clear().arg("-c").arg(&expanded);
        if let Some(path) = std::env::var_os("PATH") {
            command.env("PATH", path);
        }
        if let Some(home) = std::env::var_os("HOME") {
            command.env("HOME", home);
        }
        command
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
        // String substitution is restricted to aimx-controlled fields. Every
        // user-controlled placeholder passes through untouched so the shell
        // never sees attacker-controlled bytes as code.
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
        // User-controlled values reach the trigger shell through env vars:
        // `$AIMX_FROM` / `$AIMX_SUBJECT` expand inside double quotes, so
        // whitespace and metacharacters are preserved but never interpreted
        // as shell code.
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

    /// RAII guard that sets an env var on construction and removes it on
    /// drop. Used by `execute_triggers_clears_parent_env` so a panicking
    /// assertion still cleans up the sentinel — `std::env::set_var` is
    /// process-global, and leaking a sentinel across tests is nasty.
    struct EnvVarGuard {
        name: &'static str,
    }
    impl EnvVarGuard {
        fn set(name: &'static str, value: &str) -> Self {
            unsafe {
                std::env::set_var(name, value);
            }
            Self { name }
        }
    }
    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            unsafe {
                std::env::remove_var(self.name);
            }
        }
    }

    #[test]
    fn execute_triggers_clears_parent_env() {
        // `execute_triggers` calls `.env_clear()` before selectively re-adding
        // `PATH`/`HOME`/`AIMX_*`. An unrelated env var set on the parent
        // process must NOT be visible inside the trigger subshell.
        //
        // This test intentionally uses a unique variable name and is not
        // parallelized against other env-var-touching tests because
        // `std::env::set_var` mutates process-global state. The RAII
        // `EnvVarGuard` ensures the sentinel is removed even if an
        // assertion panics.
        let tmp = tempfile::TempDir::new().unwrap();
        let log = tmp.path().join("env.log");
        let meta = sample_metadata();
        let filepath = tmp.path().join("test.md");
        let ctx = sample_ctx(&filepath, &meta);

        // Set a sentinel on the parent process. The guard removes it on
        // drop — panic-safe.
        let _sentinel = EnvVarGuard::set("AIMX_LEAK_TEST", "sentinel-should-not-leak");

        let config = MailboxConfig {
            address: "*@test.com".to_string(),
            trust: "none".to_string(),
            trusted_senders: vec![],
            on_receive: vec![OnReceiveRule {
                rule_type: "cmd".to_string(),
                command: format!(
                    "printf 'leak=[%s] path_set=%s\\n' \"$AIMX_LEAK_TEST\" \
                     \"${{PATH:+yes}}\" > {}",
                    log.to_string_lossy()
                ),
                r#match: None,
            }],
        };

        execute_triggers(&config, &ctx);

        let content = std::fs::read_to_string(&log).unwrap();
        assert!(
            content.contains("leak=[]"),
            "AIMX_LEAK_TEST must not leak into the trigger env: {content:?}"
        );
        // PATH was re-added selectively so everyday shell tools still work.
        assert!(
            content.contains("path_set=yes"),
            "PATH must be preserved for the trigger: {content:?}"
        );
        // `_sentinel` dropped here → `AIMX_LEAK_TEST` removed.
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
        // `U-Zyn Chua <chua@uzyn.com>` lands in the env var verbatim — no
        // redirection, no execution.
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
