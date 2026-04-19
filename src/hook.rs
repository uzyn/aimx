//! Hook manager (formerly "channels"): rule evaluation, trust gating, and
//! synchronous shell execution for `on_receive` and `after_send` events.
//!
//! One `Hook` entry in `config.toml` carries a globally-unique 12-char `id`,
//! an `event` (`on_receive` | `after_send`), a `cmd`, optional match filters,
//! and an opt-in `dangerously_support_untrusted` flag that lets `on_receive`
//! hooks fire on non-trusted email.
//!
//! The trust gate (Sprint 50 inversion of FR-35/36/37):
//! `on_receive` hooks fire iff `email.trusted == "true"` OR
//! `hook.dangerously_support_untrusted == true`. Mailbox `trust` + the
//! `trusted_senders` allowlist are still the knobs that determine the
//! email's `trusted` frontmatter value (see `trust.rs`); the hook gate
//! reads the resolved value, not the policy.

use std::path::Path;
use std::process::Command;
use std::time::Instant;

use rand::Rng;
use serde::{Deserialize, Serialize};

use crate::config::MailboxConfig;
use crate::frontmatter::InboundFrontmatter;
use crate::trust::TrustedValue;

pub const HOOK_ID_LEN: usize = 12;

/// Supported hook events. `on_receive` fires during inbound ingest after the
/// email is saved to disk. `after_send` fires on outbound delivery after the
/// MX attempt resolves (success, failure, or deferred).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HookEvent {
    OnReceive,
    AfterSend,
}

impl HookEvent {
    pub fn as_str(&self) -> &'static str {
        match self {
            HookEvent::OnReceive => "on_receive",
            HookEvent::AfterSend => "after_send",
        }
    }
}

impl std::fmt::Display for HookEvent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One configured hook. Filter fields are flat on the hook table (no nested
/// `[match]` sub-block); `on_receive` uses `from`, `after_send` uses `to`,
/// and both accept `subject` + `has_attachment`.
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub struct Hook {
    /// Globally-unique 12-char `[a-z0-9]` identifier. Generated via OsRng by
    /// `aimx hooks create`; operators can hand-edit their own IDs as long as
    /// the format validates.
    pub id: String,

    pub event: HookEvent,

    /// Subprocess kind. Only `"cmd"` is supported today; kept as a string
    /// field so future hook kinds (webhook, ...) can be added without a
    /// schema break.
    #[serde(default = "default_hook_type")]
    pub r#type: String,

    /// Shell command executed under `sh -c` with `AIMX_*` env vars set.
    pub cmd: String,

    /// `on_receive` only: glob over the sender address.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,

    /// `after_send` only: glob over the recipient address.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,

    /// Substring match against the email subject (case-insensitive).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,

    /// Require the email to have (`true`) or not have (`false`) attachments.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub has_attachment: Option<bool>,

    /// `on_receive` only: when `true`, the hook fires even if the email's
    /// `trusted` value is not `"true"`. Deliberately verbose name so
    /// operators think twice.
    #[serde(
        default,
        skip_serializing_if = "is_false",
        rename = "dangerously_support_untrusted"
    )]
    pub dangerously_support_untrusted: bool,
}

fn default_hook_type() -> String {
    "cmd".to_string()
}

fn is_false(b: &bool) -> bool {
    !*b
}

/// Return true iff `s` is exactly 12 characters of `[a-z0-9]`.
pub fn is_valid_hook_id(s: &str) -> bool {
    s.len() == HOOK_ID_LEN
        && s.chars()
            .all(|c| c.is_ascii_digit() || (c.is_ascii_alphabetic() && c.is_ascii_lowercase()))
}

/// Generate a fresh 12-char `[a-z0-9]` hook id backed by `OsRng`.
///
/// Consumed by Sprint 51's `aimx hooks create` command; exercised from tests
/// today so the function is alive even before that CLI lands.
#[allow(dead_code)]
pub fn generate_hook_id() -> String {
    const CHARS: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";
    let mut rng = rand::rng();
    (0..HOOK_ID_LEN)
        .map(|_| {
            let i = rng.random_range(0..CHARS.len());
            CHARS[i] as char
        })
        .collect()
}

/// Context for an `on_receive` dispatch: the written `.md` file and parsed
/// frontmatter.
pub struct OnReceiveContext<'a> {
    pub filepath: &'a Path,
    pub metadata: &'a InboundFrontmatter,
}

/// Context for an `after_send` dispatch. All fields are already validated
/// by the send handler; we just plumb them into env vars.
pub struct AfterSendContext<'a> {
    pub mailbox: &'a str,
    pub from: &'a str,
    pub to: &'a str,
    pub subject: &'a str,
    pub has_attachment: bool,
    /// Path to the persisted sent-copy `.md` (empty string when the send
    /// wasn't persisted — e.g. TEMP failures).
    pub filepath: &'a str,
    /// RFC Message-ID of the outbound message. Always known by the send
    /// handler even when delivery failed before persistence, so the
    /// structured log line can surface a useful identifier on TEMP errors
    /// where `filepath` (and therefore `email_id`) is empty.
    pub message_id: &'a str,
    pub send_status: SendStatus,
}

/// Classification of an outbound delivery attempt, surfaced to `after_send`
/// hooks as the `AIMX_SEND_STATUS` env var.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SendStatus {
    Delivered,
    Failed,
    Deferred,
}

impl SendStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            SendStatus::Delivered => "delivered",
            SendStatus::Failed => "failed",
            SendStatus::Deferred => "deferred",
        }
    }
}

/// Trust gate for `on_receive` hooks (Sprint 50 inversion).
///
/// Fires iff `email_trusted == TrustedValue::True` OR
/// `hook.dangerously_support_untrusted == true`.
///
/// This is a deliberate behavioral change vs FR-35: mailboxes with
/// `trust = "none"` no longer fire hooks by default. Operators must either
/// set `trust = "verified"` + an allowlist or flag each hook explicitly.
pub fn should_fire_on_receive(hook: &Hook, email_trusted: TrustedValue) -> bool {
    if hook.dangerously_support_untrusted {
        return true;
    }
    email_trusted == TrustedValue::True
}

fn extract_email_for_match(addr: &str) -> String {
    // Match RFC 5322 display-name form `"Name" <addr>` by taking the LAST
    // `<` and the first `>` after it. Mirrors `send_handler::extract_bare_address`
    // and avoids slice-panics on pathological input like `"foo>bar<baz>"`
    // where a stray `>` precedes the opening `<`.
    if let Some(start) = addr.rfind('<') {
        let tail = &addr[start + 1..];
        if let Some(end) = tail.find('>') {
            return tail[..end].to_lowercase();
        }
    }
    addr.to_lowercase()
}

fn subject_matches(pattern: &str, subject: &str) -> bool {
    subject.to_lowercase().contains(&pattern.to_lowercase())
}

/// Evaluate filter predicates for an `on_receive` hook.
pub fn on_receive_filter_matches(hook: &Hook, metadata: &InboundFrontmatter) -> bool {
    if let Some(pattern) = hook.from.as_deref() {
        let from_addr = extract_email_for_match(&metadata.from);
        if !glob_match::glob_match(pattern, &from_addr) {
            return false;
        }
    }
    if let Some(pattern) = hook.subject.as_deref()
        && !subject_matches(pattern, &metadata.subject)
    {
        return false;
    }
    if let Some(expect) = hook.has_attachment {
        let has = !metadata.attachments.is_empty();
        if expect != has {
            return false;
        }
    }
    true
}

/// Evaluate filter predicates for an `after_send` hook.
pub fn after_send_filter_matches(hook: &Hook, ctx: &AfterSendContext) -> bool {
    if let Some(pattern) = hook.to.as_deref() {
        let to_addr = extract_email_for_match(ctx.to);
        if !glob_match::glob_match(pattern, &to_addr) {
            return false;
        }
    }
    if let Some(pattern) = hook.subject.as_deref()
        && !subject_matches(pattern, ctx.subject)
    {
        return false;
    }
    if let Some(expect) = hook.has_attachment
        && expect != ctx.has_attachment
    {
        return false;
    }
    true
}

/// Substitute aimx-controlled placeholders. Only `{id}` and `{date}` expand —
/// user-controlled values ride on `AIMX_*` env vars.
pub fn substitute_template(command: &str, id: &str, date: &str) -> String {
    command.replace("{id}", id).replace("{date}", date)
}

/// Fire every matching `on_receive` hook for `mailbox_config` under the
/// resolved trust gate. Failures are logged at `warn` via `tracing` but
/// never propagate.
pub fn execute_on_receive(mailbox_config: &MailboxConfig, ctx: &OnReceiveContext) {
    let email_trusted = parse_trusted(&ctx.metadata.trusted);

    for hook in mailbox_config.on_receive_hooks() {
        if !should_fire_on_receive(hook, email_trusted) {
            continue;
        }
        if !on_receive_filter_matches(hook, ctx.metadata) {
            continue;
        }

        let filepath = ctx.filepath.to_string_lossy().into_owned();
        let env: Vec<(&str, &str)> = vec![
            ("AIMX_HOOK_ID", hook.id.as_str()),
            ("AIMX_FROM", ctx.metadata.from.as_str()),
            ("AIMX_SUBJECT", ctx.metadata.subject.as_str()),
            ("AIMX_TO", ctx.metadata.to.as_str()),
            ("AIMX_MAILBOX", ctx.metadata.mailbox.as_str()),
            ("AIMX_FILEPATH", filepath.as_str()),
        ];

        let expanded = substitute_template(&hook.cmd, &ctx.metadata.id, &ctx.metadata.date);
        run_and_log(
            hook,
            &expanded,
            &env,
            &ctx.metadata.mailbox,
            LogSubject::Email(&ctx.metadata.id, &ctx.metadata.message_id),
        );
    }
}

/// Fire every matching `after_send` hook for `mailbox_config`. Runs
/// synchronously — the daemon awaits subprocess completion for predictable
/// timing, but exit codes are discarded (hooks cannot affect delivery).
pub fn execute_after_send(mailbox_config: &MailboxConfig, ctx: &AfterSendContext) {
    for hook in mailbox_config.after_send_hooks() {
        if !after_send_filter_matches(hook, ctx) {
            continue;
        }

        let send_status = ctx.send_status.as_str();
        let env: Vec<(&str, &str)> = vec![
            ("AIMX_HOOK_ID", hook.id.as_str()),
            ("AIMX_FROM", ctx.from),
            ("AIMX_TO", ctx.to),
            ("AIMX_SUBJECT", ctx.subject),
            ("AIMX_MAILBOX", ctx.mailbox),
            ("AIMX_FILEPATH", ctx.filepath),
            ("AIMX_SEND_STATUS", send_status),
        ];

        // For outbound mail, template `{id}` is the sent-file stem (last
        // path segment); `{date}` is the current timestamp.
        let id_for_template = std::path::Path::new(ctx.filepath)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_string();
        let now = chrono::Utc::now().to_rfc3339();

        let expanded = substitute_template(&hook.cmd, &id_for_template, &now);
        run_and_log(
            hook,
            &expanded,
            &env,
            ctx.mailbox,
            LogSubject::Email(&id_for_template, ctx.message_id),
        );
    }
}

enum LogSubject<'a> {
    /// `(email_id, message_id)` — either may be empty; the logger picks the
    /// first non-empty one per the agreed log format.
    Email(&'a str, &'a str),
}

fn run_and_log(
    hook: &Hook,
    expanded: &str,
    env: &[(&str, &str)],
    mailbox: &str,
    subject: LogSubject<'_>,
) {
    let start = Instant::now();
    let mut command = Command::new("sh");
    command.env_clear().arg("-c").arg(expanded);
    if let Some(path) = std::env::var_os("PATH") {
        command.env("PATH", path);
    }
    if let Some(home) = std::env::var_os("HOME") {
        command.env("HOME", home);
    }
    for (k, v) in env {
        command.env(k, v);
    }

    let (exit_code, stderr_msg, exec_err) = match command.output() {
        Ok(output) => {
            let code = output.status.code().unwrap_or(-1);
            let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
            (code, stderr, None)
        }
        Err(e) => (-1, String::new(), Some(e.to_string())),
    };

    let duration_ms = start.elapsed().as_millis();

    let (email_id, message_id) = match subject {
        LogSubject::Email(eid, mid) => (eid, mid),
    };
    let id_tag = if !email_id.is_empty() {
        format!("email_id={email_id}")
    } else if !message_id.is_empty() {
        format!("message_id={message_id}")
    } else {
        "email_id=".to_string()
    };

    // Stable single-line format (FR-32b / S50-5).
    tracing::info!(
        target: "aimx::hook",
        "hook_id={hook_id} event={event} mailbox={mailbox} {id_tag} exit_code={exit_code} duration_ms={duration_ms}",
        hook_id = hook.id,
        event = hook.event.as_str(),
        mailbox = mailbox,
    );

    if let Some(msg) = exec_err {
        tracing::warn!(
            target: "aimx::hook",
            "hook_id={hook_id} exec error: {msg}",
            hook_id = hook.id,
        );
    } else if exit_code != 0 {
        tracing::warn!(
            target: "aimx::hook",
            "hook_id={hook_id} exited with code {exit_code}: {stderr}",
            hook_id = hook.id,
            stderr = stderr_msg.trim(),
        );
    }

    if duration_ms > 5_000 {
        tracing::warn!(
            target: "aimx::hook",
            "hook_id={hook_id} slow: duration_ms={duration_ms} (>5s)",
            hook_id = hook.id,
        );
    }
}

fn parse_trusted(s: &str) -> TrustedValue {
    match s {
        "true" => TrustedValue::True,
        "false" => TrustedValue::False,
        _ => TrustedValue::None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, MailboxConfig};
    use crate::frontmatter::{AttachmentMeta, InboundFrontmatter};
    use std::collections::HashMap;
    use std::path::{Path, PathBuf};

    fn sample_config() -> Config {
        Config {
            domain: "test.com".to_string(),
            data_dir: PathBuf::from("/tmp/aimx-test"),
            dkim_selector: "aimx".to_string(),
            trust: "none".to_string(),
            trusted_senders: vec![],
            mailboxes: HashMap::new(),
            verify_host: None,
            enable_ipv6: false,
        }
    }

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
            read_at: None,
            labels: vec![],
        }
    }

    fn basic_hook(id: &str) -> Hook {
        Hook {
            id: id.to_string(),
            event: HookEvent::OnReceive,
            r#type: "cmd".to_string(),
            cmd: "true".to_string(),
            from: None,
            to: None,
            subject: None,
            has_attachment: None,
            dangerously_support_untrusted: false,
        }
    }

    #[test]
    fn generate_hook_id_produces_12_chars_alphanumeric() {
        for _ in 0..50 {
            let id = generate_hook_id();
            assert_eq!(id.len(), HOOK_ID_LEN, "id should be 12 chars: {id}");
            assert!(
                is_valid_hook_id(&id),
                "id should validate as [a-z0-9]{{12}}: {id}"
            );
        }
    }

    #[test]
    fn generate_hook_id_is_not_trivially_deterministic() {
        let a = generate_hook_id();
        let b = generate_hook_id();
        let c = generate_hook_id();
        // OsRng collisions on three 12-char alphanumeric draws are
        // astronomically unlikely; asserting "not all equal" is enough.
        assert!(!(a == b && b == c), "three consecutive ids collided: {a}");
    }

    #[test]
    fn is_valid_hook_id_boundaries() {
        assert!(is_valid_hook_id("abcdefghijkl"));
        assert!(is_valid_hook_id("0123456789ab"));
        assert!(!is_valid_hook_id("short"));
        assert!(!is_valid_hook_id("abcdefghijklm")); // 13 chars
        assert!(!is_valid_hook_id("abcdefghijk")); // 11 chars
        assert!(!is_valid_hook_id("ABCDEFGHIJKL")); // uppercase
        assert!(!is_valid_hook_id("abc-def-ghij")); // hyphens
        assert!(!is_valid_hook_id("abcdefghijk!")); // punctuation
        assert!(!is_valid_hook_id("")); // empty
    }

    #[test]
    fn should_fire_on_receive_trusted_true_fires() {
        let hook = basic_hook("aaaabbbbcccc");
        assert!(should_fire_on_receive(&hook, TrustedValue::True));
    }

    #[test]
    fn should_fire_on_receive_trusted_false_does_not_fire() {
        let hook = basic_hook("aaaabbbbcccc");
        assert!(!should_fire_on_receive(&hook, TrustedValue::False));
    }

    #[test]
    fn should_fire_on_receive_trusted_none_does_not_fire_by_default() {
        // Behavioral inversion vs FR-35: trust=none no longer fires hooks.
        let hook = basic_hook("aaaabbbbcccc");
        assert!(!should_fire_on_receive(&hook, TrustedValue::None));
    }

    #[test]
    fn should_fire_on_receive_dangerously_opt_in_fires_for_none() {
        let mut hook = basic_hook("aaaabbbbcccc");
        hook.dangerously_support_untrusted = true;
        assert!(should_fire_on_receive(&hook, TrustedValue::None));
    }

    #[test]
    fn should_fire_on_receive_dangerously_opt_in_fires_for_false() {
        let mut hook = basic_hook("aaaabbbbcccc");
        hook.dangerously_support_untrusted = true;
        assert!(should_fire_on_receive(&hook, TrustedValue::False));
    }

    #[test]
    fn filter_from_glob_matches() {
        let mut hook = basic_hook("aaaabbbbcccc");
        hook.from = Some("*@gmail.com".to_string());
        let meta = sample_metadata();
        assert!(on_receive_filter_matches(&hook, &meta));
    }

    #[test]
    fn filter_from_glob_mismatch() {
        let mut hook = basic_hook("aaaabbbbcccc");
        hook.from = Some("*@yahoo.com".to_string());
        let meta = sample_metadata();
        assert!(!on_receive_filter_matches(&hook, &meta));
    }

    #[test]
    fn filter_subject_case_insensitive() {
        let mut hook = basic_hook("aaaabbbbcccc");
        hook.subject = Some("HELLO".to_string());
        let meta = sample_metadata();
        assert!(on_receive_filter_matches(&hook, &meta));
    }

    #[test]
    fn filter_has_attachment_true_with_attachments_matches() {
        let mut hook = basic_hook("aaaabbbbcccc");
        hook.has_attachment = Some(true);
        let mut meta = sample_metadata();
        meta.attachments = vec![AttachmentMeta {
            filename: "file.txt".to_string(),
            content_type: "text/plain".to_string(),
            size: 100,
            path: "attachments/file.txt".to_string(),
        }];
        assert!(on_receive_filter_matches(&hook, &meta));
    }

    #[test]
    fn filter_has_attachment_false_with_no_attachments_matches() {
        let mut hook = basic_hook("aaaabbbbcccc");
        hook.has_attachment = Some(false);
        let meta = sample_metadata();
        assert!(on_receive_filter_matches(&hook, &meta));
    }

    #[test]
    fn extract_email_for_match_handles_inverted_angle_brackets() {
        // Regression: the old implementation used `find('<')` + `find('>')`
        // and panicked on pathological input where `>` preceded `<`
        // (e.g. `"foo>bar<baz>"` — `start+1 > end` when the naive slice
        // `addr[start+1..end]` was formed). Confirm the hardened form
        // returns a non-panicking, useful result.
        let out = extract_email_for_match("foo>bar<baz@example.com>");
        assert_eq!(out, "baz@example.com");
    }

    #[test]
    fn extract_email_for_match_no_panic_on_leading_close_bracket() {
        // Even without a trailing `>`, the function must not panic.
        let out = extract_email_for_match("weird> input");
        // No `<` at all → falls through to lowercase of the whole string.
        assert_eq!(out, "weird> input");
    }

    #[test]
    fn extract_email_for_match_takes_last_open_bracket() {
        // Display name may itself contain `<`; we pick the LAST one.
        let out = extract_email_for_match("<spoofed@attacker.com> real <user@example.com>");
        assert_eq!(out, "user@example.com");
    }

    #[test]
    fn after_send_filter_to_does_not_panic_on_pathological_input() {
        // Ensures the `after_send` filter path itself does not slice-panic.
        let hook = Hook {
            id: "panicguard01".to_string(),
            event: HookEvent::AfterSend,
            r#type: "cmd".to_string(),
            cmd: "true".to_string(),
            from: None,
            to: Some("*@example.com".to_string()),
            subject: None,
            has_attachment: None,
            dangerously_support_untrusted: false,
        };
        let ctx = AfterSendContext {
            mailbox: "alice",
            from: "alice@test.com",
            to: "foo>bar<malformed",
            subject: "x",
            has_attachment: false,
            filepath: "",
            message_id: "<x@test.com>",
            send_status: SendStatus::Failed,
        };
        // Does not panic; returns false because the `to` glob doesn't match
        // the lowercased fallback.
        assert!(!after_send_filter_matches(&hook, &ctx));
    }

    #[test]
    fn substitute_template_id_and_date_only() {
        let out = substitute_template(
            "echo {id} {date} {filepath}",
            "email-id-123",
            "2025-01-01T00:00:00Z",
        );
        // `{filepath}` is a user-controlled field and must NOT expand.
        assert!(out.contains("email-id-123"));
        assert!(out.contains("2025-01-01T00:00:00Z"));
        assert!(out.contains("{filepath}"));
    }

    fn execute_single(hook: Hook, trusted: TrustedValue) -> (MailboxConfig, PathBuf) {
        let mailbox = MailboxConfig {
            address: "*@test.com".to_string(),
            hooks: vec![hook],
            trust: Some("none".to_string()),
            trusted_senders: Some(vec![]),
        };
        let mut meta = sample_metadata();
        meta.trusted = trusted.as_str().to_string();

        let tmp = tempfile::TempDir::new().unwrap();
        let filepath = tmp.path().join("test.md");
        let ctx = OnReceiveContext {
            filepath: &filepath,
            metadata: &meta,
        };
        execute_on_receive(&mailbox, &ctx);
        let path = tmp.keep();
        (mailbox, path)
    }

    #[test]
    fn execute_on_receive_fires_when_trusted_true() {
        let tmp = tempfile::TempDir::new().unwrap();
        let marker = tmp.path().join("fired");
        let mut hook = basic_hook("aaaabbbbcccc");
        hook.cmd = format!("touch {}", marker.display());
        let (_m, _p) = execute_single(hook, TrustedValue::True);
        assert!(marker.exists(), "hook should fire when trusted=true");
    }

    #[test]
    fn execute_on_receive_does_not_fire_when_trusted_none() {
        let tmp = tempfile::TempDir::new().unwrap();
        let marker = tmp.path().join("fired");
        let mut hook = basic_hook("aaaabbbbcccc");
        hook.cmd = format!("touch {}", marker.display());
        let (_m, _p) = execute_single(hook, TrustedValue::None);
        assert!(
            !marker.exists(),
            "default hook should NOT fire for trusted=none"
        );
    }

    #[test]
    fn execute_on_receive_fires_with_dangerously_opt_in() {
        let tmp = tempfile::TempDir::new().unwrap();
        let marker = tmp.path().join("fired");
        let mut hook = basic_hook("aaaabbbbcccc");
        hook.dangerously_support_untrusted = true;
        hook.cmd = format!("touch {}", marker.display());
        let (_m, _p) = execute_single(hook, TrustedValue::None);
        assert!(
            marker.exists(),
            "dangerously_support_untrusted hook should fire for trusted=none"
        );
    }

    #[test]
    fn execute_on_receive_sets_all_env_vars_including_hook_id() {
        let tmp = tempfile::TempDir::new().unwrap();
        let out = tmp.path().join("env.out");
        let mut hook = basic_hook("hook00000001");
        hook.dangerously_support_untrusted = true;
        hook.cmd = format!(
            "printf 'HOOK=%s FROM=%s TO=%s SUBJECT=%s MAILBOX=%s FILEPATH=%s\\n' \
             \"$AIMX_HOOK_ID\" \"$AIMX_FROM\" \"$AIMX_TO\" \"$AIMX_SUBJECT\" \
             \"$AIMX_MAILBOX\" \"$AIMX_FILEPATH\" > {}",
            out.display()
        );
        let mailbox = MailboxConfig {
            address: "*@test.com".to_string(),
            hooks: vec![hook],
            trust: Some("none".to_string()),
            trusted_senders: Some(vec![]),
        };
        let meta = sample_metadata();
        let filepath = tmp.path().join("test.md");
        let ctx = OnReceiveContext {
            filepath: &filepath,
            metadata: &meta,
        };
        execute_on_receive(&mailbox, &ctx);

        let content = std::fs::read_to_string(&out).unwrap();
        assert!(content.contains("HOOK=hook00000001"), "got: {content}");
        assert!(content.contains("FROM=alice@gmail.com"), "got: {content}");
        assert!(content.contains("SUBJECT=Hello World"), "got: {content}");
        assert!(content.contains("MAILBOX=catchall"), "got: {content}");
    }

    #[test]
    fn execute_on_receive_env_clear_prevents_parent_leak() {
        // Set a sentinel in the parent; the subprocess must not see it.
        let tmp = tempfile::TempDir::new().unwrap();
        let out = tmp.path().join("env.out");

        struct EnvGuard(&'static str);
        impl Drop for EnvGuard {
            fn drop(&mut self) {
                unsafe { std::env::remove_var(self.0) };
            }
        }
        unsafe {
            std::env::set_var("AIMX_LEAK_SENTINEL_HOOK", "leaked");
        }
        let _guard = EnvGuard("AIMX_LEAK_SENTINEL_HOOK");

        let mut hook = basic_hook("hook00000002");
        hook.dangerously_support_untrusted = true;
        hook.cmd = format!(
            "printf 'leak=[%s]\\n' \"$AIMX_LEAK_SENTINEL_HOOK\" > {}",
            out.display()
        );
        let mailbox = MailboxConfig {
            address: "*@test.com".to_string(),
            hooks: vec![hook],
            trust: Some("none".to_string()),
            trusted_senders: Some(vec![]),
        };
        let meta = sample_metadata();
        let filepath = tmp.path().join("test.md");
        let ctx = OnReceiveContext {
            filepath: &filepath,
            metadata: &meta,
        };
        execute_on_receive(&mailbox, &ctx);

        let content = std::fs::read_to_string(&out).unwrap();
        assert!(
            content.contains("leak=[]"),
            "parent env must not leak: {content}"
        );
    }

    #[test]
    fn execute_after_send_fires_with_status_env_var() {
        let tmp = tempfile::TempDir::new().unwrap();
        let out = tmp.path().join("after.out");
        let hook = Hook {
            id: "aftersend001".to_string(),
            event: HookEvent::AfterSend,
            r#type: "cmd".to_string(),
            cmd: format!(
                "printf 'STATUS=%s HOOK=%s TO=%s FROM=%s FILEPATH=%s\\n' \
                 \"$AIMX_SEND_STATUS\" \"$AIMX_HOOK_ID\" \"$AIMX_TO\" \"$AIMX_FROM\" \
                 \"$AIMX_FILEPATH\" > {}",
                out.display()
            ),
            from: None,
            to: None,
            subject: None,
            has_attachment: None,
            dangerously_support_untrusted: false,
        };
        let mailbox = MailboxConfig {
            address: "alice@test.com".to_string(),
            hooks: vec![hook],
            trust: None,
            trusted_senders: None,
        };
        let ctx = AfterSendContext {
            mailbox: "alice",
            from: "alice@test.com",
            to: "bob@example.com",
            subject: "Hi",
            has_attachment: false,
            filepath: "/tmp/sent/alice/2025.md",
            message_id: "<outbound-test@test.com>",
            send_status: SendStatus::Delivered,
        };
        execute_after_send(&mailbox, &ctx);

        let content = std::fs::read_to_string(&out).unwrap();
        assert!(content.contains("STATUS=delivered"), "got: {content}");
        assert!(content.contains("HOOK=aftersend001"), "got: {content}");
        assert!(content.contains("TO=bob@example.com"), "got: {content}");
        assert!(content.contains("FROM=alice@test.com"), "got: {content}");
    }

    #[test]
    fn execute_after_send_status_mapping() {
        for (status, expected) in [
            (SendStatus::Delivered, "delivered"),
            (SendStatus::Failed, "failed"),
            (SendStatus::Deferred, "deferred"),
        ] {
            assert_eq!(status.as_str(), expected);
        }
    }

    #[test]
    fn execute_after_send_filter_to_glob_match() {
        let tmp = tempfile::TempDir::new().unwrap();
        let marker = tmp.path().join("fired");
        let hook = Hook {
            id: "tofilter0001".to_string(),
            event: HookEvent::AfterSend,
            r#type: "cmd".to_string(),
            cmd: format!("touch {}", marker.display()),
            from: None,
            to: Some("*@example.com".to_string()),
            subject: None,
            has_attachment: None,
            dangerously_support_untrusted: false,
        };
        let mailbox = MailboxConfig {
            address: "alice@test.com".to_string(),
            hooks: vec![hook],
            trust: None,
            trusted_senders: None,
        };
        let ctx = AfterSendContext {
            mailbox: "alice",
            from: "alice@test.com",
            to: "bob@example.com",
            subject: "Hi",
            has_attachment: false,
            filepath: "/tmp/sent/alice/x.md",
            message_id: "<outbound-test@test.com>",
            send_status: SendStatus::Delivered,
        };
        execute_after_send(&mailbox, &ctx);
        assert!(marker.exists());
    }

    #[test]
    fn execute_after_send_filter_mismatch_silent_skip() {
        let tmp = tempfile::TempDir::new().unwrap();
        let marker = tmp.path().join("should_not_fire");
        let hook = Hook {
            id: "tofilter0002".to_string(),
            event: HookEvent::AfterSend,
            r#type: "cmd".to_string(),
            cmd: format!("touch {}", marker.display()),
            from: None,
            to: Some("*@other.net".to_string()),
            subject: None,
            has_attachment: None,
            dangerously_support_untrusted: false,
        };
        let mailbox = MailboxConfig {
            address: "alice@test.com".to_string(),
            hooks: vec![hook],
            trust: None,
            trusted_senders: None,
        };
        let ctx = AfterSendContext {
            mailbox: "alice",
            from: "alice@test.com",
            to: "bob@example.com",
            subject: "Hi",
            has_attachment: false,
            filepath: "/tmp/sent/alice/x.md",
            message_id: "<outbound-test@test.com>",
            send_status: SendStatus::Delivered,
        };
        execute_after_send(&mailbox, &ctx);
        assert!(!marker.exists(), "filter-mismatched hook should not fire");
    }

    #[test]
    fn execute_after_send_nonzero_exit_does_not_panic() {
        let hook = Hook {
            id: "failhookxxxx".to_string(),
            event: HookEvent::AfterSend,
            r#type: "cmd".to_string(),
            cmd: "false".to_string(),
            from: None,
            to: None,
            subject: None,
            has_attachment: None,
            dangerously_support_untrusted: false,
        };
        let mailbox = MailboxConfig {
            address: "alice@test.com".to_string(),
            hooks: vec![hook],
            trust: None,
            trusted_senders: None,
        };
        let ctx = AfterSendContext {
            mailbox: "alice",
            from: "alice@test.com",
            to: "bob@example.com",
            subject: "Hi",
            has_attachment: false,
            filepath: "/tmp/sent/alice/x.md",
            message_id: "<outbound-test@test.com>",
            send_status: SendStatus::Failed,
        };
        execute_after_send(&mailbox, &ctx);
    }

    #[test]
    #[tracing_test::traced_test]
    fn hook_fire_emits_structured_log_line() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mut hook = basic_hook("logid0000001");
        hook.dangerously_support_untrusted = true;
        hook.cmd = "true".to_string();
        let mailbox = MailboxConfig {
            address: "*@test.com".to_string(),
            hooks: vec![hook],
            trust: Some("none".to_string()),
            trusted_senders: Some(vec![]),
        };
        let mut meta = sample_metadata();
        meta.trusted = "none".to_string();
        let filepath = tmp.path().join("test.md");
        let ctx = OnReceiveContext {
            filepath: &filepath,
            metadata: &meta,
        };
        execute_on_receive(&mailbox, &ctx);

        assert!(
            logs_contain("hook_id=logid0000001"),
            "log line should carry hook_id=..."
        );
        assert!(
            logs_contain("event=on_receive"),
            "log line should carry event=..."
        );
        assert!(
            logs_contain("mailbox=catchall"),
            "log line should carry mailbox=..."
        );
        assert!(
            logs_contain("email_id=2025-06-01-001"),
            "log line should carry email_id=..."
        );
        assert!(
            logs_contain("exit_code=0"),
            "log line should carry exit_code=..."
        );
        assert!(
            logs_contain("duration_ms="),
            "log line should carry duration_ms=..."
        );
    }

    #[test]
    #[tracing_test::traced_test]
    fn hook_fire_emits_log_for_nonzero_exit() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mut hook = basic_hook("logid0000002");
        hook.dangerously_support_untrusted = true;
        hook.cmd = "false".to_string();
        let mailbox = MailboxConfig {
            address: "*@test.com".to_string(),
            hooks: vec![hook],
            trust: Some("none".to_string()),
            trusted_senders: Some(vec![]),
        };
        let mut meta = sample_metadata();
        meta.trusted = "none".to_string();
        let filepath = tmp.path().join("test.md");
        let ctx = OnReceiveContext {
            filepath: &filepath,
            metadata: &meta,
        };
        execute_on_receive(&mailbox, &ctx);

        assert!(logs_contain("hook_id=logid0000002"));
        assert!(logs_contain("exit_code=1"));
    }

    #[test]
    #[tracing_test::traced_test]
    fn after_send_log_line_falls_back_to_message_id_when_filepath_empty() {
        // On TEMP delivery failures the send handler doesn't persist the
        // sent copy, so `filepath` is empty and the `email_id` tag would
        // be empty too. The structured log line must still surface the
        // RFC Message-ID so operators can grep by a stable identifier.
        let hook = Hook {
            id: "tempfailid01".to_string(),
            event: HookEvent::AfterSend,
            r#type: "cmd".to_string(),
            cmd: "true".to_string(),
            from: None,
            to: None,
            subject: None,
            has_attachment: None,
            dangerously_support_untrusted: false,
        };
        let mailbox = MailboxConfig {
            address: "alice@test.com".to_string(),
            hooks: vec![hook],
            trust: None,
            trusted_senders: None,
        };
        let ctx = AfterSendContext {
            mailbox: "alice",
            from: "alice@test.com",
            to: "bob@example.com",
            subject: "Hi",
            has_attachment: false,
            filepath: "", // TEMP failure: nothing persisted
            message_id: "<deferred-uuid@test.com>",
            send_status: SendStatus::Deferred,
        };
        execute_after_send(&mailbox, &ctx);

        assert!(
            logs_contain("hook_id=tempfailid01"),
            "log line should carry hook_id=..."
        );
        assert!(
            logs_contain("message_id=<deferred-uuid@test.com>"),
            "log line should fall back to message_id when email_id is empty"
        );
    }

    // --- Tests preserved from the old channel.rs that remain relevant ---

    #[test]
    fn env_var_preserves_backtick_injection() {
        let tmp = tempfile::TempDir::new().unwrap();
        let out = tmp.path().join("out.log");
        let mut hook = basic_hook("aaaabbbbcccc");
        hook.dangerously_support_untrusted = true;
        hook.cmd = format!("printf 'FROM=%s\\n' \"$AIMX_FROM\" > {}", out.display());
        let mailbox = MailboxConfig {
            address: "*@test.com".to_string(),
            hooks: vec![hook],
            trust: Some("none".to_string()),
            trusted_senders: Some(vec![]),
        };
        let mut meta = sample_metadata();
        meta.from = "`whoami`@attacker.com".to_string();
        let filepath = tmp.path().join("test.md");
        let ctx = OnReceiveContext {
            filepath: &filepath,
            metadata: &meta,
        };
        execute_on_receive(&mailbox, &ctx);
        let content = std::fs::read_to_string(&out).unwrap();
        assert!(
            content.contains("FROM=`whoami`@attacker.com"),
            "backticks must land verbatim: {content}"
        );
    }

    #[test]
    fn env_var_preserves_dollar_paren_injection() {
        let tmp = tempfile::TempDir::new().unwrap();
        let marker = tmp.path().join("pwned");
        let mut hook = basic_hook("aaaabbbbcccc");
        hook.dangerously_support_untrusted = true;
        hook.cmd = "echo \"$AIMX_SUBJECT\" > /dev/null".to_string();
        let mailbox = MailboxConfig {
            address: "*@test.com".to_string(),
            hooks: vec![hook],
            trust: Some("none".to_string()),
            trusted_senders: Some(vec![]),
        };
        let mut meta = sample_metadata();
        meta.subject = format!("$(touch {})", marker.display());
        let filepath = tmp.path().join("test.md");
        let ctx = OnReceiveContext {
            filepath: &filepath,
            metadata: &meta,
        };
        execute_on_receive(&mailbox, &ctx);
        assert!(
            !marker.exists(),
            "env-var payload must not execute as shell code"
        );
    }

    #[test]
    fn config_sample_config_is_valid() {
        // Catch any drift in the shared test helper.
        let cfg = sample_config();
        assert_eq!(cfg.trust, "none");
    }

    // Ensure `Path` import survives if tests shrink.
    #[test]
    fn placeholder_path_import() {
        let _p: &Path = Path::new("/tmp");
    }
}
