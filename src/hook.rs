//! Hook manager (formerly "channels"): event dispatch, trust gating, and
//! synchronous shell execution for `on_receive` and `after_send` events.
//!
//! One `Hook` entry in `config.toml` carries an `event`
//! (`on_receive` | `after_send`), a `cmd`, an opt-in
//! `dangerously_support_untrusted` flag that lets `on_receive` hooks fire on
//! non-trusted email, and an optional `name`. Hooks fire on every event of
//! their configured type; the only gate is the `on_receive` trust check.
//!
//! `name` is optional. When omitted, the effective name is derived
//! deterministically from `sha256(event || cmd ||
//! dangerously_support_untrusted)` — stable across restarts without writing
//! anything back to `config.toml`.
//!
//! The trust gate:
//! `on_receive` hooks fire iff `email.trusted == "true"` OR
//! `hook.dangerously_support_untrusted == true`. Mailbox `trust` + the
//! `trusted_senders` allowlist are the knobs that determine the email's
//! `trusted` frontmatter value (see `trust.rs`); the hook gate reads the
//! resolved value, not the policy.

use std::path::Path;
use std::process::Command;
use std::time::Instant;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::config::MailboxConfig;
use crate::frontmatter::InboundFrontmatter;
use crate::trust::TrustedValue;

/// Max length for a hook `name`. Names match
/// `^[a-zA-Z0-9_][a-zA-Z0-9_.-]{0,127}$` — Docker-tag-like.
pub const HOOK_NAME_MAX_LEN: usize = 128;

/// Length of a derived name: first 12 hex chars of the sha256 digest.
pub const DERIVED_HOOK_NAME_LEN: usize = 12;

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

/// One configured hook. `deny_unknown_fields` makes stale filter fields or
/// typos fail loudly at config load.
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Hook {
    /// Optional hook name. When `None`, the effective name is derived
    /// from `sha256(event || cmd || dangerously_support_untrusted)`.
    /// Kept as `Option<String>` so the raw round-trip distinguishes
    /// "omitted" from "present".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,

    pub event: HookEvent,

    /// Subprocess kind. Only `"cmd"` is supported today; kept as a string
    /// field so future hook kinds (webhook, ...) can be added without a
    /// schema break.
    #[serde(default = "default_hook_type")]
    pub r#type: String,

    /// Shell command executed under `sh -c` with `AIMX_*` env vars set.
    pub cmd: String,

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

/// Return true iff `s` matches `^[a-zA-Z0-9_][a-zA-Z0-9_.-]{0,127}$`.
pub fn is_valid_hook_name(s: &str) -> bool {
    let bytes = s.as_bytes();
    if bytes.is_empty() || bytes.len() > HOOK_NAME_MAX_LEN {
        return false;
    }
    let first = bytes[0];
    let first_ok = first.is_ascii_alphanumeric() || first == b'_';
    if !first_ok {
        return false;
    }
    bytes[1..]
        .iter()
        .all(|&b| b.is_ascii_alphanumeric() || b == b'_' || b == b'.' || b == b'-')
}

/// Derive a stable 12-hex-char name from `(event, cmd, dangerous)`.
///
/// Uses sha256 over the three inputs joined by the 0x1F unit-separator
/// byte, which can never appear in the TOML payload. The first 12 hex
/// chars (48 bits) are returned — wide enough that collisions across a
/// realistic config set are vanishingly improbable, and the output
/// satisfies `is_valid_hook_name`.
///
/// The mailbox name is deliberately excluded from the hash. Two mailboxes
/// with the same `(event, cmd, dangerously_support_untrusted)` will
/// produce the same derived name and collide under `validate_hooks`,
/// forcing the operator to set an explicit `name` to disambiguate. The
/// collision error string in `validate_hooks` points this out.
pub fn derive_hook_name(event: HookEvent, cmd: &str, dangerous: bool) -> String {
    let mut hasher = Sha256::new();
    hasher.update(event.as_str().as_bytes());
    hasher.update([0x1F]);
    hasher.update(cmd.as_bytes());
    hasher.update([0x1F]);
    hasher.update([dangerous as u8]);
    let digest = hasher.finalize();
    let mut out = String::with_capacity(DERIVED_HOOK_NAME_LEN);
    // DERIVED_HOOK_NAME_LEN is even, so taking ceil(len/2) bytes and
    // hex-encoding them produces exactly DERIVED_HOOK_NAME_LEN chars.
    for b in digest.iter().take(DERIVED_HOOK_NAME_LEN.div_ceil(2)) {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

/// Resolve the effective name: explicit `name` if present, else derived.
pub fn effective_hook_name(hook: &Hook) -> String {
    match &hook.name {
        Some(n) => n.clone(),
        None => derive_hook_name(hook.event, &hook.cmd, hook.dangerously_support_untrusted),
    }
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
    /// Path to the persisted sent-copy `.md` (empty string when the send
    /// wasn't persisted, e.g. TEMP failures).
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

/// Trust gate for `on_receive` hooks.
///
/// Fires iff `email_trusted == TrustedValue::True` OR
/// `hook.dangerously_support_untrusted == true`.
pub fn should_fire_on_receive(hook: &Hook, email_trusted: TrustedValue) -> bool {
    if hook.dangerously_support_untrusted {
        return true;
    }
    email_trusted == TrustedValue::True
}

/// Substitute aimx-controlled placeholders. Only `{id}` and `{date}` expand.
/// User-controlled values ride on `AIMX_*` env vars.
pub fn substitute_template(command: &str, id: &str, date: &str) -> String {
    command.replace("{id}", id).replace("{date}", date)
}

/// Fire every `on_receive` hook for `mailbox_config` under the resolved
/// trust gate. Failures are logged at `warn` via `tracing` but never
/// propagate.
pub fn execute_on_receive(mailbox_config: &MailboxConfig, ctx: &OnReceiveContext) {
    let email_trusted = parse_trusted(&ctx.metadata.trusted);

    for hook in mailbox_config.on_receive_hooks() {
        if !should_fire_on_receive(hook, email_trusted) {
            continue;
        }

        let hook_name = effective_hook_name(hook);
        let filepath = ctx.filepath.to_string_lossy().into_owned();
        let env: Vec<(&str, &str)> = vec![
            ("AIMX_HOOK_NAME", hook_name.as_str()),
            ("AIMX_FROM", ctx.metadata.from.as_str()),
            ("AIMX_SUBJECT", ctx.metadata.subject.as_str()),
            ("AIMX_TO", ctx.metadata.to.as_str()),
            ("AIMX_MAILBOX", ctx.metadata.mailbox.as_str()),
            ("AIMX_FILEPATH", filepath.as_str()),
        ];

        let expanded = substitute_template(&hook.cmd, &ctx.metadata.id, &ctx.metadata.date);
        run_and_log(
            hook,
            &hook_name,
            &expanded,
            &env,
            &ctx.metadata.mailbox,
            LogSubject::Email(&ctx.metadata.id, &ctx.metadata.message_id),
        );
    }
}

/// Fire every `after_send` hook for `mailbox_config`. Runs synchronously.
/// The daemon awaits subprocess completion for predictable timing, but exit
/// codes are discarded (hooks cannot affect delivery).
pub fn execute_after_send(mailbox_config: &MailboxConfig, ctx: &AfterSendContext) {
    for hook in mailbox_config.after_send_hooks() {
        let hook_name = effective_hook_name(hook);
        let send_status = ctx.send_status.as_str();
        let env: Vec<(&str, &str)> = vec![
            ("AIMX_HOOK_NAME", hook_name.as_str()),
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
            &hook_name,
            &expanded,
            &env,
            ctx.mailbox,
            LogSubject::Email(&id_for_template, ctx.message_id),
        );
    }
}

enum LogSubject<'a> {
    /// `(email_id, message_id)`. Either may be empty; the logger picks the
    /// first non-empty one per the agreed log format.
    Email(&'a str, &'a str),
}

fn run_and_log(
    hook: &Hook,
    hook_name: &str,
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

    tracing::info!(
        target: "aimx::hook",
        "hook_name={hook_name} event={event} mailbox={mailbox} {id_tag} exit_code={exit_code} duration_ms={duration_ms}",
        hook_name = hook_name,
        event = hook.event.as_str(),
        mailbox = mailbox,
    );

    if let Some(msg) = exec_err {
        tracing::warn!(
            target: "aimx::hook",
            "hook_name={hook_name} exec error: {msg}",
            hook_name = hook_name,
        );
    } else if exit_code != 0 {
        tracing::warn!(
            target: "aimx::hook",
            "hook_name={hook_name} exited with code {exit_code}: {stderr}",
            hook_name = hook_name,
            stderr = stderr_msg.trim(),
        );
    }

    if duration_ms > 5_000 {
        tracing::warn!(
            target: "aimx::hook",
            "hook_name={hook_name} slow: duration_ms={duration_ms} (>5s)",
            hook_name = hook_name,
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
    use crate::frontmatter::InboundFrontmatter;
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

    fn basic_hook(name: &str) -> Hook {
        Hook {
            name: Some(name.to_string()),
            event: HookEvent::OnReceive,
            r#type: "cmd".to_string(),
            cmd: "true".to_string(),
            dangerously_support_untrusted: false,
        }
    }

    #[test]
    fn is_valid_hook_name_boundaries() {
        assert!(is_valid_hook_name("a"));
        assert!(is_valid_hook_name("_"));
        assert!(is_valid_hook_name("9"));
        assert!(is_valid_hook_name("abc-123_def.ghi"));
        assert!(is_valid_hook_name(&"a".repeat(HOOK_NAME_MAX_LEN)));
        assert!(!is_valid_hook_name(&"a".repeat(HOOK_NAME_MAX_LEN + 1)));
        assert!(!is_valid_hook_name(""));
        assert!(!is_valid_hook_name(".leading-dot"));
        assert!(!is_valid_hook_name("-leading-dash"));
        assert!(!is_valid_hook_name("has space"));
        assert!(!is_valid_hook_name("bang!"));
        assert!(!is_valid_hook_name("über"));
    }

    #[test]
    fn derive_hook_name_deterministic() {
        let a = derive_hook_name(HookEvent::OnReceive, "echo hi", false);
        let b = derive_hook_name(HookEvent::OnReceive, "echo hi", false);
        assert_eq!(a, b);
        assert_eq!(a.len(), DERIVED_HOOK_NAME_LEN);
        assert!(is_valid_hook_name(&a));
    }

    #[test]
    fn derive_hook_name_differs_by_event() {
        let r = derive_hook_name(HookEvent::OnReceive, "echo hi", false);
        let s = derive_hook_name(HookEvent::AfterSend, "echo hi", false);
        assert_ne!(r, s);
    }

    #[test]
    fn derive_hook_name_differs_by_cmd() {
        let a = derive_hook_name(HookEvent::OnReceive, "echo hi", false);
        let b = derive_hook_name(HookEvent::OnReceive, "echo hj", false);
        assert_ne!(a, b);
    }

    #[test]
    fn derive_hook_name_differs_by_dangerous_flag() {
        let a = derive_hook_name(HookEvent::OnReceive, "echo hi", false);
        let b = derive_hook_name(HookEvent::OnReceive, "echo hi", true);
        assert_ne!(a, b);
    }

    #[test]
    fn effective_hook_name_prefers_explicit() {
        let mut hook = basic_hook("explicit_name");
        assert_eq!(effective_hook_name(&hook), "explicit_name");
        hook.name = None;
        let derived = effective_hook_name(&hook);
        assert_eq!(
            derived,
            derive_hook_name(HookEvent::OnReceive, "true", false)
        );
    }

    #[test]
    fn should_fire_on_receive_trusted_true_fires() {
        let hook = basic_hook("h1");
        assert!(should_fire_on_receive(&hook, TrustedValue::True));
    }

    #[test]
    fn should_fire_on_receive_trusted_false_does_not_fire() {
        let hook = basic_hook("h1");
        assert!(!should_fire_on_receive(&hook, TrustedValue::False));
    }

    #[test]
    fn should_fire_on_receive_trusted_none_does_not_fire_by_default() {
        let hook = basic_hook("h1");
        assert!(!should_fire_on_receive(&hook, TrustedValue::None));
    }

    #[test]
    fn should_fire_on_receive_dangerously_opt_in_fires_for_none() {
        let mut hook = basic_hook("h1");
        hook.dangerously_support_untrusted = true;
        assert!(should_fire_on_receive(&hook, TrustedValue::None));
    }

    #[test]
    fn should_fire_on_receive_dangerously_opt_in_fires_for_false() {
        let mut hook = basic_hook("h1");
        hook.dangerously_support_untrusted = true;
        assert!(should_fire_on_receive(&hook, TrustedValue::False));
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
        let mut hook = basic_hook("h1");
        hook.cmd = format!("touch {}", marker.display());
        let (_m, _p) = execute_single(hook, TrustedValue::True);
        assert!(marker.exists(), "hook should fire when trusted=true");
    }

    #[test]
    fn execute_on_receive_does_not_fire_when_trusted_none() {
        let tmp = tempfile::TempDir::new().unwrap();
        let marker = tmp.path().join("fired");
        let mut hook = basic_hook("h1");
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
        let mut hook = basic_hook("h1");
        hook.dangerously_support_untrusted = true;
        hook.cmd = format!("touch {}", marker.display());
        let (_m, _p) = execute_single(hook, TrustedValue::None);
        assert!(
            marker.exists(),
            "dangerously_support_untrusted hook should fire for trusted=none"
        );
    }

    #[test]
    fn execute_on_receive_sets_all_env_vars_including_hook_name() {
        let tmp = tempfile::TempDir::new().unwrap();
        let out = tmp.path().join("env.out");
        let mut hook = basic_hook("hook_explicit");
        hook.dangerously_support_untrusted = true;
        hook.cmd = format!(
            "printf 'HOOK=%s FROM=%s TO=%s SUBJECT=%s MAILBOX=%s FILEPATH=%s\\n' \
             \"$AIMX_HOOK_NAME\" \"$AIMX_FROM\" \"$AIMX_TO\" \"$AIMX_SUBJECT\" \
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
        assert!(content.contains("HOOK=hook_explicit"), "got: {content}");
        assert!(content.contains("FROM=alice@gmail.com"), "got: {content}");
        assert!(content.contains("SUBJECT=Hello World"), "got: {content}");
        assert!(content.contains("MAILBOX=catchall"), "got: {content}");
    }

    #[test]
    fn execute_on_receive_uses_derived_name_when_name_omitted() {
        let tmp = tempfile::TempDir::new().unwrap();
        let out = tmp.path().join("env.out");
        let hook = Hook {
            name: None,
            event: HookEvent::OnReceive,
            r#type: "cmd".to_string(),
            cmd: format!(
                "printf 'HOOK=%s\\n' \"$AIMX_HOOK_NAME\" > {}",
                out.display()
            ),
            dangerously_support_untrusted: true,
        };
        let derived = derive_hook_name(HookEvent::OnReceive, &hook.cmd, true);
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
            content.contains(&format!("HOOK={derived}")),
            "got: {content}, expected derived: {derived}"
        );
    }

    #[test]
    fn execute_on_receive_env_clear_prevents_parent_leak() {
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

        let mut hook = basic_hook("h1");
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
            name: Some("after_explicit".to_string()),
            event: HookEvent::AfterSend,
            r#type: "cmd".to_string(),
            cmd: format!(
                "printf 'STATUS=%s HOOK=%s TO=%s FROM=%s FILEPATH=%s\\n' \
                 \"$AIMX_SEND_STATUS\" \"$AIMX_HOOK_NAME\" \"$AIMX_TO\" \"$AIMX_FROM\" \
                 \"$AIMX_FILEPATH\" > {}",
                out.display()
            ),
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
            filepath: "/tmp/sent/alice/2025.md",
            message_id: "<outbound-test@test.com>",
            send_status: SendStatus::Delivered,
        };
        execute_after_send(&mailbox, &ctx);

        let content = std::fs::read_to_string(&out).unwrap();
        assert!(content.contains("STATUS=delivered"), "got: {content}");
        assert!(content.contains("HOOK=after_explicit"), "got: {content}");
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
    fn execute_after_send_nonzero_exit_does_not_panic() {
        let hook = Hook {
            name: Some("failhook".to_string()),
            event: HookEvent::AfterSend,
            r#type: "cmd".to_string(),
            cmd: "false".to_string(),
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
        let mut hook = basic_hook("log_explicit");
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
            logs_contain("hook_name=log_explicit"),
            "log line should carry hook_name=..."
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
        let mut hook = basic_hook("log_failed");
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

        assert!(logs_contain("hook_name=log_failed"));
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
            name: Some("tempfail".to_string()),
            event: HookEvent::AfterSend,
            r#type: "cmd".to_string(),
            cmd: "true".to_string(),
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
            filepath: "",
            message_id: "<deferred-uuid@test.com>",
            send_status: SendStatus::Deferred,
        };
        execute_after_send(&mailbox, &ctx);

        assert!(
            logs_contain("hook_name=tempfail"),
            "log line should carry hook_name=..."
        );
        assert!(
            logs_contain("message_id=<deferred-uuid@test.com>"),
            "log line should fall back to message_id when email_id is empty"
        );
    }

    #[test]
    fn env_var_preserves_backtick_injection() {
        let tmp = tempfile::TempDir::new().unwrap();
        let out = tmp.path().join("out.log");
        let mut hook = basic_hook("h1");
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
        let mut hook = basic_hook("h1");
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
        let cfg = sample_config();
        assert_eq!(cfg.trust, "none");
    }

    #[test]
    fn placeholder_path_import() {
        let _p: &Path = Path::new("/tmp");
    }
}
