//! Hook manager: event dispatch, trust gating, and synchronous shell
//! execution for `on_receive` and `after_send` events.
//!
//! One `Hook` entry in `config.toml` carries an `event`
//! (`on_receive` | `after_send`), a `cmd`, an opt-in `fire_on_untrusted`
//! flag that lets `on_receive` hooks fire on non-trusted email, and an
//! optional `name`. Hooks fire on every event of their configured type;
//! the only gate is the `on_receive` trust check.
//!
//! `name` is optional. When omitted, the effective name is derived
//! deterministically from `sha256(event || cmd || fire_on_untrusted)` —
//! stable across restarts without writing anything back to `config.toml`.
//!
//! The trust gate:
//! `on_receive` hooks fire iff `email.trusted == "true"` OR
//! `hook.fire_on_untrusted == true`. Mailbox `trust` + the
//! `trusted_senders` allowlist are the knobs that determine the email's
//! `trusted` frontmatter value (see `trust.rs`); the hook gate reads the
//! resolved value, not the policy.

use std::collections::HashMap;
use std::path::Path;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::config::{Config, MailboxConfig};
use crate::frontmatter::InboundFrontmatter;
use crate::platform::{SandboxError, SandboxOutcome, SandboxStdin, spawn_sandboxed};
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
///
/// Hooks are raw-cmd only: `cmd` is a non-empty shell string created by
/// the operator via CLI / hand-edit. Hook creation requires root and
/// SIGHUPs the running daemon; there is no UDS verb for it.
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Hook {
    /// Optional hook name. When `None`, the effective name is derived
    /// from `sha256(event || cmd || fire_on_untrusted)`. Kept as
    /// `Option<String>` so the raw round-trip distinguishes "omitted"
    /// from "present".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,

    pub event: HookEvent,

    /// Subprocess kind. Only `"cmd"` is supported today; kept as a string
    /// field so future hook kinds (webhook, ...) can be added without a
    /// schema break.
    #[serde(default = "default_hook_type")]
    pub r#type: String,

    /// Shell command for raw-cmd hooks. Required and non-empty.
    pub cmd: String,

    /// `on_receive` only: when `true`, the hook fires even if the email's
    /// `trusted` value is not `"true"`.
    #[serde(default, skip_serializing_if = "is_false")]
    pub fire_on_untrusted: bool,
}

impl Hook {
    /// Resolve the final argv that the sandboxed executor will `exec`.
    ///
    /// Wraps the operator-provided shell string in
    /// `["/bin/sh", "-c", <cmd>]` so the spawn site has a uniform argv
    /// shape; shell interpretation is intentional for operator-authored
    /// hooks.
    pub fn resolve_argv(&self) -> Result<Vec<String>, ResolveArgvError> {
        if self.cmd.trim().is_empty() {
            return Err(ResolveArgvError::EmptyCmd);
        }
        Ok(vec![
            "/bin/sh".to_string(),
            "-c".to_string(),
            self.cmd.clone(),
        ])
    }
}

/// Reasons [`Hook::resolve_argv`] can fail at fire time.
#[derive(Debug)]
pub enum ResolveArgvError {
    EmptyCmd,
}

impl std::fmt::Display for ResolveArgvError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ResolveArgvError::EmptyCmd => write!(f, "raw-cmd hook has empty cmd"),
        }
    }
}

impl std::error::Error for ResolveArgvError {}

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

/// Derive a stable 12-hex-char name from `(event, cmd, fire_on_untrusted)`.
///
/// Uses sha256 over the three inputs joined by the 0x1F unit-separator
/// byte, which can never appear in the TOML payload. The first 12 hex
/// chars (48 bits) are returned — wide enough that collisions across a
/// realistic config set are vanishingly improbable, and the output
/// satisfies `is_valid_hook_name`.
///
/// The mailbox name is deliberately excluded from the hash. Two mailboxes
/// with the same `(event, cmd, fire_on_untrusted)` will produce the same
/// derived name and collide under the load-time hook-name uniqueness
/// check, forcing the operator to set an explicit `name` to disambiguate.
pub fn derive_hook_name(event: HookEvent, cmd: &str, fire_on_untrusted: bool) -> String {
    let mut hasher = Sha256::new();
    hasher.update(event.as_str().as_bytes());
    hasher.update([0x1F]);
    hasher.update(cmd.as_bytes());
    hasher.update([0x1F]);
    hasher.update([fire_on_untrusted as u8]);
    let digest = hasher.finalize();
    let mut out = String::with_capacity(DERIVED_HOOK_NAME_LEN);
    // DERIVED_HOOK_NAME_LEN is even, so taking ceil(len/2) bytes and
    // hex-encoding them produces exactly DERIVED_HOOK_NAME_LEN chars.
    for b in digest.iter().take(DERIVED_HOOK_NAME_LEN.div_ceil(2)) {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

/// Resolve the effective name: explicit `name` if present, else derived
/// from `(event, cmd, fire_on_untrusted)`.
pub fn effective_hook_name(hook: &Hook) -> String {
    if let Some(n) = &hook.name {
        return n.clone();
    }
    derive_hook_name(hook.event, &hook.cmd, hook.fire_on_untrusted)
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
/// `hook.fire_on_untrusted == true`.
pub fn should_fire_on_receive(hook: &Hook, email_trusted: TrustedValue) -> bool {
    if hook.fire_on_untrusted {
        return true;
    }
    email_trusted == TrustedValue::True
}

/// Fire every `on_receive` hook for `mailbox_config` under the resolved
/// trust gate. Failures are logged at `warn` via `tracing` but never
/// propagate.
pub fn execute_on_receive(config: &Config, mailbox_config: &MailboxConfig, ctx: &OnReceiveContext) {
    let email_trusted = parse_trusted(&ctx.metadata.trusted);
    let mailbox_name = &ctx.metadata.mailbox;

    let hooks: Vec<&Hook> = mailbox_config.on_receive_hooks().collect();
    if hooks.is_empty() {
        tracing::info!(
            target: "aimx::hook",
            "No hooks found for event=on_receive mailbox={mailbox}",
            mailbox = mailbox_name,
        );
        return;
    }

    for hook in hooks {
        if !should_fire_on_receive(hook, email_trusted) {
            let hook_name = effective_hook_name(hook);
            tracing::info!(
                target: "aimx::hook",
                "hook_name={hook_name} event=on_receive mailbox={mailbox} skipped: trusted={trusted} fire_on_untrusted={fire_on_untrusted}",
                hook_name = hook_name,
                mailbox = mailbox_name,
                trusted = email_trusted.as_str(),
                fire_on_untrusted = hook.fire_on_untrusted,
            );
            continue;
        }

        let hook_name = effective_hook_name(hook);
        let filepath = ctx.filepath.to_string_lossy().into_owned();
        let mut env: HashMap<String, String> = HashMap::new();
        env.insert("AIMX_HOOK_NAME".into(), hook_name.clone());
        env.insert("AIMX_FROM".into(), ctx.metadata.from.clone());
        env.insert("AIMX_SUBJECT".into(), ctx.metadata.subject.clone());
        env.insert("AIMX_TO".into(), ctx.metadata.to.clone());
        env.insert("AIMX_MAILBOX".into(), ctx.metadata.mailbox.clone());
        env.insert("AIMX_FILEPATH".into(), filepath);
        env.insert("AIMX_MESSAGE_ID".into(), ctx.metadata.message_id.clone());
        env.insert("AIMX_EVENT".into(), HookEvent::OnReceive.as_str().into());
        env.insert("AIMX_ID".into(), ctx.metadata.id.clone());
        env.insert("AIMX_DATE".into(), ctx.metadata.date.clone());

        run_and_log(
            config,
            mailbox_config,
            hook,
            &hook_name,
            &env,
            &ctx.metadata.mailbox,
            Some(ctx.filepath),
            LogSubject::Email(&ctx.metadata.id, &ctx.metadata.message_id),
        );
    }
}

/// Fire every `after_send` hook for `mailbox_config`. Runs synchronously.
/// The daemon awaits subprocess completion for predictable timing, but exit
/// codes are discarded (hooks cannot affect delivery).
pub fn execute_after_send(config: &Config, mailbox_config: &MailboxConfig, ctx: &AfterSendContext) {
    let hooks: Vec<&Hook> = mailbox_config.after_send_hooks().collect();
    if hooks.is_empty() {
        tracing::info!(
            target: "aimx::hook",
            "No hooks found for event=after_send mailbox={mailbox}",
            mailbox = ctx.mailbox,
        );
        return;
    }

    for hook in hooks {
        let hook_name = effective_hook_name(hook);
        let send_status = ctx.send_status.as_str();

        let id_for_template = std::path::Path::new(ctx.filepath)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_string();
        let now = chrono::Utc::now().to_rfc3339();

        let mut env: HashMap<String, String> = HashMap::new();
        env.insert("AIMX_HOOK_NAME".into(), hook_name.clone());
        env.insert("AIMX_FROM".into(), ctx.from.to_string());
        env.insert("AIMX_TO".into(), ctx.to.to_string());
        env.insert("AIMX_SUBJECT".into(), ctx.subject.to_string());
        env.insert("AIMX_MAILBOX".into(), ctx.mailbox.to_string());
        env.insert("AIMX_FILEPATH".into(), ctx.filepath.to_string());
        env.insert("AIMX_SEND_STATUS".into(), send_status.to_string());
        env.insert("AIMX_MESSAGE_ID".into(), ctx.message_id.to_string());
        env.insert("AIMX_EVENT".into(), HookEvent::AfterSend.as_str().into());
        env.insert("AIMX_ID".into(), id_for_template.clone());
        env.insert("AIMX_DATE".into(), now);

        let filepath_opt = if ctx.filepath.is_empty() {
            None
        } else {
            Some(std::path::Path::new(ctx.filepath))
        };

        run_and_log(
            config,
            mailbox_config,
            hook,
            &hook_name,
            &env,
            ctx.mailbox,
            filepath_opt,
            LogSubject::Email(&id_for_template, ctx.message_id),
        );
    }
}

enum LogSubject<'a> {
    /// `(email_id, message_id)`. Either may be empty; the logger picks the
    /// first non-empty one per the agreed log format.
    Email(&'a str, &'a str),
}

/// Default timeout for hooks. Mirrors the prior raw-cmd default.
const HOOK_DEFAULT_TIMEOUT_SECS: u64 = 60;

#[allow(clippy::too_many_arguments)]
fn run_and_log(
    _config: &Config,
    mailbox_config: &MailboxConfig,
    hook: &Hook,
    hook_name: &str,
    env: &HashMap<String, String>,
    mailbox: &str,
    stdin_source: Option<&Path>,
    subject: LogSubject<'_>,
) {
    let start = Instant::now();

    // --- Resolve argv ------------------------------------------------------
    let argv = match hook.resolve_argv() {
        Ok(argv) => argv,
        Err(e) => {
            tracing::warn!(
                target: "aimx::hook",
                "hook_name={hook_name} resolve_argv error: {e}",
                hook_name = hook_name,
            );
            return;
        }
    };

    // --- Resolve run_as / timeout / stdin mode -----------------------------
    // With the legacy schema gone, hooks always run as the mailbox's
    // owner. The auth predicate will gate this in a later pass; for
    // now this is the simple single-line rule.
    let run_as: String = mailbox_config.owner.clone();

    let timeout = Duration::from_secs(HOOK_DEFAULT_TIMEOUT_SECS);

    let stdin_payload = build_stdin(stdin_source);

    tracing::info!(
        target: "aimx::hook",
        "firing hook_name={hook_name} event={event} mailbox={mailbox} run_as={run_as}",
        hook_name = hook_name,
        event = hook.event.as_str(),
        mailbox = mailbox,
        run_as = run_as,
    );

    // --- Spawn -------------------------------------------------------------
    let outcome_result = spawn_sandboxed(&argv, stdin_payload, &run_as, timeout, env);

    let (exit_code, stderr_tail, timed_out, sandbox, exec_err, duration_ms) = match outcome_result {
        Ok(SandboxOutcome {
            exit_code,
            stderr_tail,
            duration,
            sandbox,
            timed_out,
            ..
        }) => (
            exit_code,
            stderr_tail,
            timed_out,
            Some(sandbox),
            None,
            duration.as_millis(),
        ),
        Err(e) => {
            let msg = format!("{e}");
            let kind = match &e {
                SandboxError::UserNotFound(_) => "user-not-found",
                SandboxError::SpawnFailed(_) => "spawn-failed",
                SandboxError::IoFailed(_) => "io-failed",
            };
            (
                -1,
                Vec::new(),
                false,
                None,
                Some((kind, msg)),
                start.elapsed().as_millis(),
            )
        }
    };

    let sandbox_tag = sandbox.map(|s| s.as_str()).unwrap_or("-");
    let stderr_tail_str = format_stderr_tail(&stderr_tail);

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
        "hook_name={hook_name} event={event} mailbox={mailbox} run_as={run_as} sandbox={sandbox_tag} {id_tag} exit_code={exit_code} duration_ms={duration_ms} timed_out={timed_out} stderr_tail={stderr_tail_str}",
        hook_name = hook_name,
        event = hook.event.as_str(),
        mailbox = mailbox,
    );

    if let Some((kind, msg)) = exec_err {
        tracing::warn!(
            target: "aimx::hook",
            "hook_name={hook_name} exec error ({kind}): {msg}",
            hook_name = hook_name,
        );
    } else if exit_code != 0 {
        let stderr_for_log = String::from_utf8_lossy(&stderr_tail);
        tracing::warn!(
            target: "aimx::hook",
            "hook_name={hook_name} exited with code {exit_code}: {stderr}",
            hook_name = hook_name,
            stderr = stderr_for_log.trim(),
        );
    }

    if timed_out {
        tracing::warn!(
            target: "aimx::hook",
            "hook_name={hook_name} timed out after {timeout_ms}ms",
            hook_name = hook_name,
            timeout_ms = timeout.as_millis(),
        );
    }

    if duration_ms > 5_000 && !timed_out {
        tracing::warn!(
            target: "aimx::hook",
            "hook_name={hook_name} slow: duration_ms={duration_ms} (>5s)",
            hook_name = hook_name,
        );
    }
}

/// Build the stdin payload for a hook fire. The raw `.md` file is piped
/// into the hook's child process. If the file doesn't exist (e.g. TEMP
/// failures on `after_send` where persistence was skipped) we pipe an
/// empty payload rather than failing the hook. A real read error
/// (EACCES etc.) is logged at WARN.
fn build_stdin(source: Option<&Path>) -> SandboxStdin {
    SandboxStdin::Email(read_stdin_source(source))
}

fn read_stdin_source(source: Option<&Path>) -> Vec<u8> {
    let Some(p) = source else {
        return Vec::new();
    };
    match std::fs::read(p) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(e) => {
            tracing::warn!(
                target: "aimx::hook",
                "hook stdin: read {path} failed: {e}; piping empty payload",
                path = p.display(),
            );
            Vec::new()
        }
    }
}

/// Render `stderr_tail` as a compact, log-safe string. Newlines and
/// control characters are JSON-escaped so the single-line structured
/// log record cannot be broken by hook stderr; if the tail would exceed
/// 1 KiB after escaping, it is truncated with an ellipsis.
fn format_stderr_tail(bytes: &[u8]) -> String {
    if bytes.is_empty() {
        return "\"\"".into();
    }
    const LOG_INLINE_LIMIT: usize = 1024;
    let as_str = String::from_utf8_lossy(bytes);
    let escaped = serde_json::to_string(as_str.as_ref()).unwrap_or_else(|_| "\"\"".into());
    if escaped.len() > LOG_INLINE_LIMIT {
        let head_n = LOG_INLINE_LIMIT / 2;
        let tail_n = LOG_INLINE_LIMIT / 2 - 8;
        let inner = &escaped[1..escaped.len() - 1];
        if inner.len() > head_n + tail_n + 8 {
            let head: String = inner.chars().take(head_n).collect();
            let tail_start = inner.len() - tail_n;
            let mut tail = String::new();
            let mut idx = tail_start;
            while !inner.is_char_boundary(idx) && idx < inner.len() {
                idx += 1;
            }
            tail.push_str(&inner[idx..]);
            return format!("\"{head}...{tail}\"");
        }
    }
    escaped
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
    use std::sync::Once;

    /// Force the fallback (non-systemd-run) sandbox path for unit tests.
    /// `systemd-run` on a systemd host requires interactive auth when
    /// the caller is non-root, which makes the hook tests fail on any
    /// developer workstation. The fallback path works regardless and
    /// exercises the same observable surface (exit_code, stderr_tail,
    /// env var propagation, timeout).
    fn force_sandbox_fallback() {
        static ONCE: Once = Once::new();
        ONCE.call_once(|| unsafe {
            std::env::set_var("AIMX_SANDBOX_FORCE_FALLBACK", "1");
        });
    }

    fn sample_config() -> Config {
        force_sandbox_fallback();
        Config {
            domain: "test.com".to_string(),
            data_dir: PathBuf::from("/tmp/aimx-test"),
            dkim_selector: "aimx".to_string(),
            trust: "none".to_string(),
            trusted_senders: vec![],
            mailboxes: HashMap::new(),
            verify_host: None,
            enable_ipv6: false,
            upgrade: None,
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
            fire_on_untrusted: false,
        }
    }

    fn current_user_name() -> String {
        let uid = nix::unistd::Uid::current();
        nix::unistd::User::from_uid(uid)
            .ok()
            .flatten()
            .map(|u| u.name)
            .unwrap_or_else(|| "nobody".to_string())
    }

    fn catchall_mailbox(hooks: Vec<Hook>) -> MailboxConfig {
        MailboxConfig {
            address: "*@test.com".to_string(),
            owner: current_user_name(),
            hooks,
            trust: Some("none".to_string()),
            trusted_senders: Some(vec![]),
            allow_root_catchall: false,
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
    fn derive_hook_name_differs_by_fire_on_untrusted_flag() {
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
    fn should_fire_on_receive_fire_on_untrusted_fires_for_none() {
        let mut hook = basic_hook("h1");
        hook.fire_on_untrusted = true;
        assert!(should_fire_on_receive(&hook, TrustedValue::None));
    }

    #[test]
    fn should_fire_on_receive_fire_on_untrusted_fires_for_false() {
        let mut hook = basic_hook("h1");
        hook.fire_on_untrusted = true;
        assert!(should_fire_on_receive(&hook, TrustedValue::False));
    }

    fn execute_single(hook: Hook, trusted: TrustedValue) -> (MailboxConfig, PathBuf) {
        let mailbox = catchall_mailbox(vec![hook]);
        let mut meta = sample_metadata();
        meta.trusted = trusted.as_str().to_string();

        let tmp = tempfile::TempDir::new().unwrap();
        let filepath = tmp.path().join("test.md");
        std::fs::write(&filepath, b"test\n").ok();
        let ctx = OnReceiveContext {
            filepath: &filepath,
            metadata: &meta,
        };
        let cfg = sample_config();
        execute_on_receive(&cfg, &mailbox, &ctx);
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
    fn execute_on_receive_fires_with_fire_on_untrusted_opt_in() {
        let tmp = tempfile::TempDir::new().unwrap();
        let marker = tmp.path().join("fired");
        let mut hook = basic_hook("h1");
        hook.fire_on_untrusted = true;
        hook.cmd = format!("touch {}", marker.display());
        let (_m, _p) = execute_single(hook, TrustedValue::None);
        assert!(
            marker.exists(),
            "fire_on_untrusted hook should fire for trusted=none"
        );
    }

    #[test]
    fn execute_on_receive_sets_all_env_vars_including_hook_name() {
        let tmp = tempfile::TempDir::new().unwrap();
        let out = tmp.path().join("env.out");
        let mut hook = basic_hook("hook_explicit");
        hook.fire_on_untrusted = true;
        hook.cmd = format!(
            "printf 'HOOK=%s FROM=%s TO=%s SUBJECT=%s MAILBOX=%s FILEPATH=%s\\n' \
             \"$AIMX_HOOK_NAME\" \"$AIMX_FROM\" \"$AIMX_TO\" \"$AIMX_SUBJECT\" \
             \"$AIMX_MAILBOX\" \"$AIMX_FILEPATH\" > {}",
            out.display()
        );
        let mailbox = catchall_mailbox(vec![hook]);
        let meta = sample_metadata();
        let filepath = tmp.path().join("test.md");
        let ctx = OnReceiveContext {
            filepath: &filepath,
            metadata: &meta,
        };
        execute_on_receive(&sample_config(), &mailbox, &ctx);

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
            fire_on_untrusted: true,
        };
        let derived = derive_hook_name(HookEvent::OnReceive, &hook.cmd, true);
        let mailbox = catchall_mailbox(vec![hook]);
        let meta = sample_metadata();
        let filepath = tmp.path().join("test.md");
        let ctx = OnReceiveContext {
            filepath: &filepath,
            metadata: &meta,
        };
        execute_on_receive(&sample_config(), &mailbox, &ctx);
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
        hook.fire_on_untrusted = true;
        hook.cmd = format!(
            "printf 'leak=[%s]\\n' \"$AIMX_LEAK_SENTINEL_HOOK\" > {}",
            out.display()
        );
        let mailbox = catchall_mailbox(vec![hook]);
        let meta = sample_metadata();
        let filepath = tmp.path().join("test.md");
        let ctx = OnReceiveContext {
            filepath: &filepath,
            metadata: &meta,
        };
        execute_on_receive(&sample_config(), &mailbox, &ctx);

        let content = std::fs::read_to_string(&out).unwrap();
        assert!(
            content.contains("leak=[]"),
            "parent env must not leak: {content}"
        );
    }

    fn alice_mailbox(hooks: Vec<Hook>) -> MailboxConfig {
        MailboxConfig {
            address: "alice@test.com".to_string(),
            owner: current_user_name(),
            hooks,
            trust: None,
            trusted_senders: None,
            allow_root_catchall: false,
        }
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
            fire_on_untrusted: false,
        };
        let mailbox = alice_mailbox(vec![hook]);
        let ctx = AfterSendContext {
            mailbox: "alice",
            from: "alice@test.com",
            to: "bob@example.com",
            subject: "Hi",
            filepath: "/tmp/sent/alice/2025.md",
            message_id: "<outbound-test@test.com>",
            send_status: SendStatus::Delivered,
        };
        execute_after_send(&sample_config(), &mailbox, &ctx);

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
            fire_on_untrusted: false,
        };
        let mailbox = alice_mailbox(vec![hook]);
        let ctx = AfterSendContext {
            mailbox: "alice",
            from: "alice@test.com",
            to: "bob@example.com",
            subject: "Hi",
            filepath: "/tmp/sent/alice/x.md",
            message_id: "<outbound-test@test.com>",
            send_status: SendStatus::Failed,
        };
        execute_after_send(&sample_config(), &mailbox, &ctx);
    }

    #[test]
    fn raw_cmd_hook_omits_default_fields_on_serialize() {
        let hook = Hook {
            name: Some("raw".to_string()),
            event: HookEvent::OnReceive,
            r#type: "cmd".to_string(),
            cmd: "echo hi".to_string(),
            fire_on_untrusted: false,
        };
        let s = toml::to_string(&hook).unwrap();
        assert!(
            !s.contains("fire_on_untrusted"),
            "default fire_on_untrusted must be skipped: {s}"
        );
    }

    #[test]
    fn legacy_hook_toml_with_only_required_fields_loads() {
        let src = r#"
event = "on_receive"
cmd = "echo hi"
"#;
        let hook: Hook = toml::from_str(src).unwrap();
        assert_eq!(hook.event, HookEvent::OnReceive);
        assert_eq!(hook.cmd, "echo hi");
        assert!(!hook.fire_on_untrusted);
    }

    #[test]
    fn resolve_argv_raw_cmd_wraps_in_sh_dash_c() {
        let mut hook = basic_hook("raw");
        hook.cmd = "echo hi".into();
        let argv = hook.resolve_argv().unwrap();
        assert_eq!(argv, vec!["/bin/sh", "-c", "echo hi"]);
    }

    #[test]
    fn resolve_argv_raw_cmd_with_empty_cmd_fails() {
        let mut hook = basic_hook("empty");
        hook.cmd = "   ".into();
        assert!(matches!(
            hook.resolve_argv(),
            Err(ResolveArgvError::EmptyCmd)
        ));
    }

    #[test]
    fn format_stderr_tail_empty_is_quoted_empty() {
        assert_eq!(format_stderr_tail(b""), "\"\"");
    }

    #[test]
    fn format_stderr_tail_escapes_newlines_and_quotes() {
        let rendered = format_stderr_tail(b"line1\nline\"2");
        assert!(rendered.starts_with('"'));
        assert!(rendered.ends_with('"'));
        assert!(rendered.contains("\\n"), "got: {rendered}");
        assert!(rendered.contains("\\\""), "got: {rendered}");
        assert!(!rendered.contains('\n'), "raw newline must be escaped");
    }

    #[test]
    fn format_stderr_tail_truncates_past_limit() {
        let big = vec![b'x'; 4096];
        let rendered = format_stderr_tail(&big);
        assert!(rendered.contains("..."), "long stderr must be truncated");
        assert!(rendered.starts_with('"'));
        assert!(rendered.ends_with('"'));
    }

    #[test]
    fn format_stderr_tail_handles_multibyte_boundary() {
        let mut payload: Vec<u8> = vec![b'x'; 4096];
        for _ in 0..256 {
            payload.extend_from_slice("えあ🦀".as_bytes());
        }
        let rendered = format_stderr_tail(&payload);
        assert!(rendered.starts_with('"'), "got: {rendered}");
        assert!(rendered.ends_with('"'), "got: {rendered}");
        assert!(rendered.contains("..."), "long stderr must be truncated");
        let as_str = std::str::from_utf8(rendered.as_bytes())
            .expect("format_stderr_tail must emit valid UTF-8");
        let parsed: serde_json::Value =
            serde_json::from_str(as_str).expect("rendered value must be valid JSON");
        assert!(parsed.is_string());
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
