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

use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::config::{Config, HookTemplate, HookTemplateStdin, MailboxConfig};
use crate::frontmatter::InboundFrontmatter;
use crate::hook_substitute::{BuiltinContext, SubstitutionError, substitute_argv};
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

/// Provenance tag recorded on every hook written to `config.toml`.
///
/// `Operator` = authored by root via CLI or hand-edit (default when the
/// field is absent). `Mcp` = created by an agent over the UDS
/// `HOOK-CREATE` verb. The daemon uses this tag at `HOOK-DELETE` time: MCP
/// may only delete hooks whose `origin = "mcp"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum HookOrigin {
    #[default]
    Operator,
    Mcp,
}

impl HookOrigin {
    /// String form used by `doctor` output and tests. Production log
    /// lines do not currently surface origin (see PRD §7.3); kept as an
    /// API so Sprint 3's `hooks list` / doctor summary can format it.
    #[allow(dead_code)]
    pub fn as_str(&self) -> &'static str {
        match self {
            HookOrigin::Operator => "operator",
            HookOrigin::Mcp => "mcp",
        }
    }
}

/// One configured hook. `deny_unknown_fields` makes stale filter fields or
/// typos fail loudly at config load.
///
/// A hook is one of two flavors:
/// 1. **Raw-cmd** — `template = None`, `cmd` is a non-empty shell string.
///    Created by the operator via CLI / hand-edit.
/// 2. **Template-bound** — `template = Some(...)`, `params` carries the
///    bound values, and `cmd` is empty. Created by an agent via MCP, or
///    by the operator via `aimx hooks create --template`.
///
/// Mutual exclusion is enforced at config load (see
/// [`crate::config::validate_hook_mutual_exclusion`]).
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

    /// Shell command for raw-cmd hooks. Empty string when `template` is
    /// `Some` — the resolved argv comes from the template at fire time.
    #[serde(default, skip_serializing_if = "String::is_empty")]
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

    /// Provenance tag. Defaults to `Operator` when absent so legacy hooks
    /// hand-edited into `config.toml` remain operator-origin. MCP writes
    /// always stamp `Mcp`.
    #[serde(default, skip_serializing_if = "is_default_origin")]
    pub origin: HookOrigin,

    /// Name of a `[[hook_template]]` this hook binds to. `None` on
    /// raw-cmd hooks (mutually exclusive with a non-empty `cmd`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub template: Option<String>,

    /// Bound parameter values for template hooks. Keys must match the
    /// template's declared `params`. Always empty for raw-cmd hooks.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub params: BTreeMap<String, String>,

    /// Unix user the hook's child process runs as. Defaults to
    /// `aimx-hook` (unprivileged). The only other accepted value is
    /// `root`, and it is intentionally only settable by an operator
    /// hand-editing `config.toml` — the UDS `HOOK-CREATE` verb rejects
    /// this field entirely. Template-bound hooks inherit the template's
    /// `run_as` when this field is `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_as: Option<String>,
}

impl Hook {
    /// True iff this hook references a template (rather than carrying its
    /// own raw `cmd`). Consumed by tests today; wired into `doctor` and
    /// `hooks list` summary in Sprint 6.
    #[allow(dead_code)]
    pub fn is_template_bound(&self) -> bool {
        self.template.is_some()
    }

    /// Resolve the final argv that the sandboxed executor will `exec`.
    ///
    /// For template-bound hooks: looks up the matching [`HookTemplate`]
    /// in `templates`, then substitutes declared `params` and the
    /// provided `builtins` into the template's argv via
    /// [`crate::hook_substitute::substitute_argv`].
    ///
    /// For raw-cmd hooks: wraps the operator-provided shell string in
    /// `["/bin/sh", "-c", <cmd>]`. This keeps raw-cmd hooks uniform with
    /// template hooks at the [`crate::platform::spawn_sandboxed`] call
    /// site (both produce a `Vec<String>` argv); shell interpretation is
    /// intentional for operator-authored hooks.
    pub fn resolve_argv(
        &self,
        templates: &[HookTemplate],
        builtins: &BuiltinContext,
    ) -> Result<Vec<String>, ResolveArgvError> {
        match &self.template {
            Some(name) => {
                let tmpl = templates
                    .iter()
                    .find(|t| &t.name == name)
                    .ok_or_else(|| ResolveArgvError::UnknownTemplate(name.clone()))?;
                substitute_argv(&tmpl.cmd, &self.params, builtins)
                    .map_err(ResolveArgvError::Substitution)
            }
            None => {
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
    }
}

/// Reasons [`Hook::resolve_argv`] can fail at fire time.
///
/// All variants indicate a configuration-level bug that validation should
/// normally catch at load time. They are kept as distinct variants so the
/// caller can emit a precise `tracing::warn!` without swallowing context.
#[derive(Debug)]
pub enum ResolveArgvError {
    UnknownTemplate(String),
    EmptyCmd,
    Substitution(SubstitutionError),
}

impl std::fmt::Display for ResolveArgvError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ResolveArgvError::UnknownTemplate(name) => {
                write!(f, "hook references unknown template '{name}'")
            }
            ResolveArgvError::EmptyCmd => write!(f, "raw-cmd hook has empty cmd"),
            ResolveArgvError::Substitution(e) => write!(f, "{e}"),
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

fn is_default_origin(o: &HookOrigin) -> bool {
    matches!(o, HookOrigin::Operator)
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

/// Derive a stable 12-hex-char name for a template-bound hook.
///
/// Uses sha256 over `(event, template, sorted_params)`. Two template-bound
/// hooks with identical bound params collide — operators are nudged to set
/// an explicit `name` via the same `validate_hooks` error path that
/// catches duplicate raw-cmd hooks.
pub fn derive_template_hook_name(
    event: HookEvent,
    template: &str,
    params: &BTreeMap<String, String>,
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(event.as_str().as_bytes());
    hasher.update([0x1F]);
    hasher.update(b"template=");
    hasher.update(template.as_bytes());
    for (k, v) in params {
        hasher.update([0x1F]);
        hasher.update(k.as_bytes());
        hasher.update(b"=");
        hasher.update(v.as_bytes());
    }
    let digest = hasher.finalize();
    let mut out = String::with_capacity(DERIVED_HOOK_NAME_LEN);
    for b in digest.iter().take(DERIVED_HOOK_NAME_LEN.div_ceil(2)) {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

/// Resolve the effective name: explicit `name` if present, else derived.
/// Template-bound hooks derive from `(event, template, sorted params)`;
/// raw-cmd hooks derive from `(event, cmd, dangerous)` (legacy shape).
pub fn effective_hook_name(hook: &Hook) -> String {
    if let Some(n) = &hook.name {
        return n.clone();
    }
    match &hook.template {
        Some(tmpl) => derive_template_hook_name(hook.event, tmpl, &hook.params),
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
                "hook_name={hook_name} event=on_receive mailbox={mailbox} skipped: trusted={trusted} dangerously_support_untrusted={dangerous}",
                hook_name = hook_name,
                mailbox = mailbox_name,
                trusted = email_trusted.as_str(),
                dangerous = hook.dangerously_support_untrusted,
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

        let builtins = BuiltinContext {
            event: HookEvent::OnReceive.as_str().into(),
            mailbox: ctx.metadata.mailbox.clone(),
            message_id: ctx.metadata.message_id.clone(),
            from: ctx.metadata.from.clone(),
            subject: ctx.metadata.subject.clone(),
        };

        run_and_log(
            config,
            hook,
            &hook_name,
            &builtins,
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

        // For outbound mail, the `{id}` placeholder is the sent-file
        // stem (last path segment). `{date}` is the current UTC
        // timestamp — kept for legacy raw-cmd substitution compat.
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

        let builtins = BuiltinContext {
            event: HookEvent::AfterSend.as_str().into(),
            mailbox: ctx.mailbox.to_string(),
            message_id: ctx.message_id.to_string(),
            from: ctx.from.to_string(),
            subject: ctx.subject.to_string(),
        };

        let filepath_opt = if ctx.filepath.is_empty() {
            None
        } else {
            Some(std::path::Path::new(ctx.filepath))
        };

        run_and_log(
            config,
            hook,
            &hook_name,
            &builtins,
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

/// Default timeout for raw-cmd hooks (which have no template to carry a
/// `timeout_secs`). Matches the template default.
const RAW_CMD_DEFAULT_TIMEOUT_SECS: u64 = 60;

/// Default `run_as` for raw-cmd hooks that don't set the field. PRD
/// §6.7: hooks run as `aimx-hook` unless the operator explicitly sets
/// `run_as = "root"` in `config.toml`.
const RAW_CMD_DEFAULT_RUN_AS: &str = "aimx-hook";

#[allow(clippy::too_many_arguments)]
fn run_and_log(
    config: &Config,
    hook: &Hook,
    hook_name: &str,
    builtins: &BuiltinContext,
    env: &HashMap<String, String>,
    mailbox: &str,
    stdin_source: Option<&Path>,
    subject: LogSubject<'_>,
) {
    let start = Instant::now();

    // --- Resolve argv ------------------------------------------------------
    let argv = match hook.resolve_argv(&config.hook_templates, builtins) {
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
    let template = hook
        .template
        .as_ref()
        .and_then(|n| config.hook_templates.iter().find(|t| &t.name == n));

    let run_as: String = match &hook.run_as {
        Some(explicit) => explicit.clone(),
        None => template
            .map(|t| t.run_as.clone())
            .unwrap_or_else(|| RAW_CMD_DEFAULT_RUN_AS.to_string()),
    };

    let timeout = match template {
        Some(t) => Duration::from_secs(t.timeout_secs as u64),
        None => Duration::from_secs(RAW_CMD_DEFAULT_TIMEOUT_SECS),
    };

    let stdin_mode = template
        .map(|t| t.stdin)
        .unwrap_or(HookTemplateStdin::Email);
    let stdin_payload = build_stdin(stdin_mode, stdin_source);

    let pre_fire_template_tag = hook
        .template
        .as_deref()
        .map(|s| s.to_string())
        .unwrap_or_else(|| "-".to_string());
    tracing::info!(
        target: "aimx::hook",
        "firing hook_name={hook_name} event={event} mailbox={mailbox} template={template_tag} run_as={run_as}",
        hook_name = hook_name,
        event = hook.event.as_str(),
        mailbox = mailbox,
        template_tag = pre_fire_template_tag,
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

    let template_tag = hook
        .template
        .as_deref()
        .map(|s| s.to_string())
        .unwrap_or_else(|| "-".to_string());
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
        "hook_name={hook_name} event={event} mailbox={mailbox} template={template_tag} run_as={run_as} sandbox={sandbox_tag} {id_tag} exit_code={exit_code} duration_ms={duration_ms} timed_out={timed_out} stderr_tail={stderr_tail_str}",
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

/// Build the stdin payload for a hook fire. For `Email` / `EmailJson`
/// modes we load the associated `.md` file; if the file doesn't exist
/// (e.g. TEMP failures on `after_send` where persistence was skipped)
/// we pipe an empty payload rather than failing the hook. A real
/// read error (EACCES etc.) is logged at WARN so operators can tell a
/// missing-file (intentional empty payload) apart from a permissions
/// bug (which would otherwise silently present as empty stdin).
fn build_stdin(mode: HookTemplateStdin, source: Option<&Path>) -> SandboxStdin {
    match mode {
        HookTemplateStdin::None => SandboxStdin::None,
        HookTemplateStdin::Email => SandboxStdin::Email(read_stdin_source(source)),
        HookTemplateStdin::EmailJson => {
            // Best-effort per PRD §9 out-of-scope: wrap raw `.md` bytes
            // in a JSON object keyed by `raw`. Stabilization is post-v1.
            let bytes = read_stdin_source(source);
            let escaped = serde_json::to_string(&String::from_utf8_lossy(&bytes).into_owned())
                .unwrap_or_else(|_| "\"\"".into());
            let json = format!("{{\"raw\":{escaped}}}");
            SandboxStdin::EmailJson(json.into_bytes())
        }
    }
}

/// Read the backing `.md` file for a hook's stdin payload. `None` path
/// and `NotFound` both yield empty bytes silently (the TEMP-failure
/// `after_send` case where no file was ever persisted — see PRD §9).
/// Any other `io::Error` is surfaced as a WARN so an EACCES on a real
/// file doesn't hide behind the empty-payload code path.
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
/// 1 KiB after escaping, it is truncated with an ellipsis. Empty tails
/// are rendered as `""`, never `null`.
fn format_stderr_tail(bytes: &[u8]) -> String {
    if bytes.is_empty() {
        return "\"\"".into();
    }
    const LOG_INLINE_LIMIT: usize = 1024;
    let as_str = String::from_utf8_lossy(bytes);
    let escaped = serde_json::to_string(as_str.as_ref()).unwrap_or_else(|_| "\"\"".into());
    if escaped.len() > LOG_INLINE_LIMIT {
        // Keep the head + trailing ellipsis + close quote; the PRD wants
        // the `tail`, but realistically the first KiB of stderr is the
        // most informative prefix after truncation. We include both:
        // half from the start, half from the end.
        let head_n = LOG_INLINE_LIMIT / 2;
        let tail_n = LOG_INLINE_LIMIT / 2 - 8;
        let inner = &escaped[1..escaped.len() - 1]; // strip outer quotes
        if inner.len() > head_n + tail_n + 8 {
            let head: String = inner.chars().take(head_n).collect();
            let tail_start = inner.len() - tail_n;
            let mut tail = String::new();
            // Walk back to a char boundary.
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
            hook_templates: Vec::new(),
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
            origin: crate::hook::HookOrigin::Operator,
            template: None,
            params: std::collections::BTreeMap::new(),
            run_as: None,
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

    fn execute_single(hook: Hook, trusted: TrustedValue) -> (MailboxConfig, PathBuf) {
        let mailbox = MailboxConfig {
            address: "*@test.com".to_string(),
            owner: "aimx-catchall".to_string(),
            hooks: vec![hook],
            trust: Some("none".to_string()),
            trusted_senders: Some(vec![]),
        };
        let mut meta = sample_metadata();
        meta.trusted = trusted.as_str().to_string();

        let tmp = tempfile::TempDir::new().unwrap();
        let filepath = tmp.path().join("test.md");
        // Ensure the file exists so stdin = "email" has something to read
        // (the run_and_log rewrite now pipes `.md` into the child).
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
            owner: "aimx-catchall".to_string(),
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
            dangerously_support_untrusted: true,
            origin: crate::hook::HookOrigin::Operator,
            template: None,
            params: std::collections::BTreeMap::new(),
            run_as: None,
        };
        let derived = derive_hook_name(HookEvent::OnReceive, &hook.cmd, true);
        let mailbox = MailboxConfig {
            address: "*@test.com".to_string(),
            owner: "aimx-catchall".to_string(),
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
        hook.dangerously_support_untrusted = true;
        hook.cmd = format!(
            "printf 'leak=[%s]\\n' \"$AIMX_LEAK_SENTINEL_HOOK\" > {}",
            out.display()
        );
        let mailbox = MailboxConfig {
            address: "*@test.com".to_string(),
            owner: "aimx-catchall".to_string(),
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
        execute_on_receive(&sample_config(), &mailbox, &ctx);

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
            origin: crate::hook::HookOrigin::Operator,
            template: None,
            params: std::collections::BTreeMap::new(),
            run_as: None,
        };
        let mailbox = MailboxConfig {
            address: "alice@test.com".to_string(),
            owner: "root".to_string(),
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
            dangerously_support_untrusted: false,
            origin: crate::hook::HookOrigin::Operator,
            template: None,
            params: std::collections::BTreeMap::new(),
            run_as: None,
        };
        let mailbox = MailboxConfig {
            address: "alice@test.com".to_string(),
            owner: "root".to_string(),
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
        execute_after_send(&sample_config(), &mailbox, &ctx);
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
            owner: "aimx-catchall".to_string(),
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
        execute_on_receive(&sample_config(), &mailbox, &ctx);

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
            owner: "aimx-catchall".to_string(),
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
        execute_on_receive(&sample_config(), &mailbox, &ctx);

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
            origin: crate::hook::HookOrigin::Operator,
            template: None,
            params: std::collections::BTreeMap::new(),
            run_as: None,
        };
        let mailbox = MailboxConfig {
            address: "alice@test.com".to_string(),
            owner: "root".to_string(),
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
        execute_after_send(&sample_config(), &mailbox, &ctx);

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
            owner: "aimx-catchall".to_string(),
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
        execute_on_receive(&sample_config(), &mailbox, &ctx);
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
            owner: "aimx-catchall".to_string(),
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
        execute_on_receive(&sample_config(), &mailbox, &ctx);
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

    // ----- Sprint 1 S1-3: origin + template + params -----------------------

    #[test]
    fn hook_origin_default_is_operator() {
        assert_eq!(HookOrigin::default(), HookOrigin::Operator);
        assert_eq!(HookOrigin::Operator.as_str(), "operator");
        assert_eq!(HookOrigin::Mcp.as_str(), "mcp");
    }

    #[test]
    fn hook_origin_roundtrips_via_toml() {
        #[derive(Serialize, Deserialize, PartialEq, Debug)]
        struct W {
            origin: HookOrigin,
        }
        let w = W {
            origin: HookOrigin::Mcp,
        };
        let s = toml::to_string(&w).unwrap();
        assert!(s.contains("origin = \"mcp\""), "serialized: {s}");
        let back: W = toml::from_str(&s).unwrap();
        assert_eq!(back, w);
    }

    #[test]
    fn hook_is_template_bound() {
        let mut hook = basic_hook("t1");
        assert!(!hook.is_template_bound());
        hook.cmd.clear();
        hook.template = Some("invoke-claude".to_string());
        assert!(hook.is_template_bound());
    }

    #[test]
    fn template_hook_round_trips_through_toml() {
        let mut params = BTreeMap::new();
        params.insert("prompt".to_string(), "draft a reply".to_string());
        let hook = Hook {
            name: Some("auto-reply".to_string()),
            event: HookEvent::OnReceive,
            r#type: "cmd".to_string(),
            cmd: String::new(),
            dangerously_support_untrusted: false,
            origin: HookOrigin::Mcp,
            template: Some("invoke-claude".to_string()),
            params,
            run_as: None,
        };
        let s = toml::to_string(&hook).unwrap();
        assert!(s.contains("origin = \"mcp\""), "serialized: {s}");
        assert!(
            s.contains("template = \"invoke-claude\""),
            "serialized: {s}"
        );
        assert!(!s.contains("cmd ="), "empty cmd must be skipped: {s}");
        let back: Hook = toml::from_str(&s).unwrap();
        assert_eq!(back, hook);
    }

    #[test]
    fn raw_cmd_hook_omits_new_fields_on_serialize() {
        let hook = Hook {
            name: Some("raw".to_string()),
            event: HookEvent::OnReceive,
            r#type: "cmd".to_string(),
            cmd: "echo hi".to_string(),
            dangerously_support_untrusted: false,
            origin: HookOrigin::Operator,
            template: None,
            params: BTreeMap::new(),
            run_as: None,
        };
        let s = toml::to_string(&hook).unwrap();
        // origin = operator is the default → skipped
        assert!(!s.contains("origin"), "default origin must be skipped: {s}");
        assert!(
            !s.contains("template"),
            "None template must be skipped: {s}"
        );
        assert!(!s.contains("params"), "empty params must be skipped: {s}");
    }

    #[test]
    fn legacy_hook_toml_defaults_to_operator_origin() {
        // Hooks hand-edited into `config.toml` before this sprint had no
        // origin field — load must default to Operator, not error.
        let src = r#"
event = "on_receive"
cmd = "echo legacy"
"#;
        let hook: Hook = toml::from_str(src).unwrap();
        assert_eq!(hook.origin, HookOrigin::Operator);
        assert!(hook.template.is_none());
        assert!(hook.params.is_empty());
    }

    #[test]
    fn derive_template_hook_name_is_deterministic() {
        let mut params = BTreeMap::new();
        params.insert("prompt".to_string(), "hi".to_string());
        let a = derive_template_hook_name(HookEvent::OnReceive, "invoke-claude", &params);
        let b = derive_template_hook_name(HookEvent::OnReceive, "invoke-claude", &params);
        assert_eq!(a, b);
        assert_eq!(a.len(), DERIVED_HOOK_NAME_LEN);
        assert!(is_valid_hook_name(&a));
    }

    #[test]
    fn derive_template_hook_name_differs_by_params() {
        let mut p1 = BTreeMap::new();
        p1.insert("prompt".to_string(), "hi".to_string());
        let mut p2 = BTreeMap::new();
        p2.insert("prompt".to_string(), "hello".to_string());
        let a = derive_template_hook_name(HookEvent::OnReceive, "invoke-claude", &p1);
        let b = derive_template_hook_name(HookEvent::OnReceive, "invoke-claude", &p2);
        assert_ne!(a, b);
    }

    #[test]
    fn effective_hook_name_prefers_template_hash_over_cmd_hash() {
        let mut params = BTreeMap::new();
        params.insert("prompt".to_string(), "hi".to_string());
        let hook = Hook {
            name: None,
            event: HookEvent::OnReceive,
            r#type: "cmd".to_string(),
            cmd: String::new(),
            dangerously_support_untrusted: false,
            origin: HookOrigin::Mcp,
            template: Some("invoke-claude".to_string()),
            params: params.clone(),
            run_as: None,
        };
        let effective = effective_hook_name(&hook);
        let expected = derive_template_hook_name(HookEvent::OnReceive, "invoke-claude", &params);
        assert_eq!(effective, expected);
    }

    // ----- Sprint 2 S2-1: Hook::resolve_argv -------------------------------

    fn claude_template() -> HookTemplate {
        HookTemplate {
            name: "invoke-claude".into(),
            description: "Pipe email into Claude Code".into(),
            cmd: vec![
                "/usr/local/bin/claude".into(),
                "-p".into(),
                "{prompt}".into(),
            ],
            params: vec!["prompt".into()],
            stdin: crate::config::HookTemplateStdin::Email,
            run_as: "aimx-hook".into(),
            timeout_secs: 60,
            allowed_events: vec![HookEvent::OnReceive, HookEvent::AfterSend],
        }
    }

    #[test]
    fn resolve_argv_raw_cmd_wraps_in_sh_dash_c() {
        let mut hook = basic_hook("raw");
        hook.cmd = "echo hi".into();
        let b = BuiltinContext::default();
        let argv = hook.resolve_argv(&[], &b).unwrap();
        assert_eq!(argv, vec!["/bin/sh", "-c", "echo hi"]);
    }

    #[test]
    fn resolve_argv_raw_cmd_with_empty_cmd_fails() {
        let mut hook = basic_hook("empty");
        hook.cmd = "   ".into();
        let b = BuiltinContext::default();
        assert!(matches!(
            hook.resolve_argv(&[], &b),
            Err(ResolveArgvError::EmptyCmd)
        ));
    }

    #[test]
    fn resolve_argv_template_substitutes_params_and_builtins() {
        let tmpl = claude_template();
        let mut hook = Hook {
            name: Some("h".into()),
            event: HookEvent::OnReceive,
            r#type: "cmd".into(),
            cmd: String::new(),
            dangerously_support_untrusted: false,
            origin: HookOrigin::Mcp,
            template: Some("invoke-claude".into()),
            params: BTreeMap::new(),
            run_as: None,
        };
        hook.params.insert("prompt".into(), "draft a reply".into());
        let b = BuiltinContext {
            event: "on_receive".into(),
            mailbox: "accounts".into(),
            ..Default::default()
        };
        let argv = hook.resolve_argv(&[tmpl], &b).unwrap();
        assert_eq!(argv, vec!["/usr/local/bin/claude", "-p", "draft a reply"]);
    }

    #[test]
    fn resolve_argv_unknown_template_fails() {
        let hook = Hook {
            name: Some("h".into()),
            event: HookEvent::OnReceive,
            r#type: "cmd".into(),
            cmd: String::new(),
            dangerously_support_untrusted: false,
            origin: HookOrigin::Mcp,
            template: Some("nope".into()),
            params: BTreeMap::new(),
            run_as: None,
        };
        let b = BuiltinContext::default();
        match hook.resolve_argv(&[claude_template()], &b).unwrap_err() {
            ResolveArgvError::UnknownTemplate(n) => assert_eq!(n, "nope"),
            e => panic!("unexpected: {e}"),
        }
    }

    #[test]
    fn resolve_argv_template_with_bad_param_propagates_substitution_error() {
        let tmpl = claude_template();
        let mut hook = Hook {
            name: Some("h".into()),
            event: HookEvent::OnReceive,
            r#type: "cmd".into(),
            cmd: String::new(),
            dangerously_support_untrusted: false,
            origin: HookOrigin::Mcp,
            template: Some("invoke-claude".into()),
            params: BTreeMap::new(),
            run_as: None,
        };
        hook.params.insert("prompt".into(), "bad\0value".into());
        let b = BuiltinContext::default();
        assert!(matches!(
            hook.resolve_argv(&[tmpl], &b).unwrap_err(),
            ResolveArgvError::Substitution(SubstitutionError::ParamContainsNul { .. })
        ));
    }

    // ----- Sprint 2 S2-3: structured hook-fire log new fields --------------

    #[test]
    fn format_stderr_tail_empty_is_quoted_empty() {
        assert_eq!(format_stderr_tail(b""), "\"\"");
    }

    #[test]
    fn format_stderr_tail_escapes_newlines_and_quotes() {
        let rendered = format_stderr_tail(b"line1\nline\"2");
        // JSON-escaped: \n and \" survive on the escape side.
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

    /// Regression: the tail-start index inside `format_stderr_tail` is
    /// a byte offset computed from `tail_n`. With ASCII-only input it
    /// always lands on a char boundary, but multi-byte UTF-8 scalars
    /// can straddle the boundary. The implementation walks forward to
    /// the next `is_char_boundary`; this test locks in that the walk
    /// doesn't panic on either a 3-byte-BMP or a 4-byte-astral char
    /// straddling the truncation point, AND that the final output is
    /// valid UTF-8 (i.e. serde_json can still parse it back).
    #[test]
    fn format_stderr_tail_handles_multibyte_boundary() {
        // Build a payload big enough to force truncation, ending with
        // a stretch of 3-byte (え = U+3048) + 4-byte (🦀 = U+1F980)
        // UTF-8 chars so the truncation point is guaranteed to land
        // inside one of them for at least one of the many byte offsets
        // between the ASCII filler and the multi-byte tail.
        let mut payload: Vec<u8> = vec![b'x'; 4096];
        for _ in 0..256 {
            payload.extend_from_slice("えあ🦀".as_bytes());
        }
        let rendered = format_stderr_tail(&payload);
        assert!(rendered.starts_with('"'), "got: {rendered}");
        assert!(rendered.ends_with('"'), "got: {rendered}");
        assert!(rendered.contains("..."), "long stderr must be truncated");
        // Must still be valid UTF-8 (the inner slice comes from `str`
        // via `is_char_boundary`, so this is a lock-in against a future
        // regression that replaces the walk with raw byte slicing).
        let as_str = std::str::from_utf8(rendered.as_bytes())
            .expect("format_stderr_tail must emit valid UTF-8");
        // And must still round-trip through serde_json so a downstream
        // structured-log parser doesn't choke on a half-escape.
        let parsed: serde_json::Value =
            serde_json::from_str(as_str).expect("rendered value must be valid JSON");
        assert!(parsed.is_string());
    }

    #[test]
    #[tracing_test::traced_test]
    fn log_line_includes_template_run_as_and_stderr_tail_fields_for_raw_cmd() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mut hook = basic_hook("s2_3");
        hook.dangerously_support_untrusted = true;
        // Exit non-zero so stderr capture + warn-path are exercised.
        hook.cmd = "echo hi 1>&2; exit 1".to_string();
        let mailbox = MailboxConfig {
            address: "*@test.com".to_string(),
            owner: "aimx-catchall".to_string(),
            hooks: vec![hook],
            trust: Some("none".to_string()),
            trusted_senders: Some(vec![]),
        };
        let meta = sample_metadata();
        let filepath = tmp.path().join("test.md");
        std::fs::write(&filepath, b"+++\n+++\n").unwrap();
        let ctx = OnReceiveContext {
            filepath: &filepath,
            metadata: &meta,
        };
        execute_on_receive(&sample_config(), &mailbox, &ctx);

        // Raw-cmd → template is the `-` sentinel.
        assert!(
            logs_contain("template=-"),
            "log line should carry template=..."
        );
        // Raw-cmd default run_as is aimx-hook (even though the fallback
        // path runs as the test user since aimx-hook doesn't exist on CI).
        assert!(
            logs_contain("run_as=aimx-hook"),
            "log line should carry run_as=..."
        );
        // stderr was "hi\n" — escaped JSON should surface it.
        assert!(
            logs_contain("stderr_tail=\"hi\\n\""),
            "log line should carry stderr_tail=... (got: no match)"
        );
        // sandbox tag appears.
        assert!(
            logs_contain("sandbox=setuid") || logs_contain("sandbox=systemd-run"),
            "log line should carry sandbox=..."
        );
        // timed_out flag appears.
        assert!(logs_contain("timed_out=false"));
    }

    #[test]
    #[tracing_test::traced_test]
    fn log_line_template_field_is_template_name_for_template_hook() {
        let tmp = tempfile::TempDir::new().unwrap();
        let filepath = tmp.path().join("test.md");
        std::fs::write(&filepath, b"+++\n+++\nbody\n").unwrap();

        let tmpl = HookTemplate {
            name: "echoer".into(),
            description: "echo".into(),
            cmd: vec!["/bin/sh".into(), "-c".into(), "echo hi > {path}".into()],
            params: vec!["path".into()],
            stdin: crate::config::HookTemplateStdin::None,
            run_as: "aimx-hook".into(),
            timeout_secs: 5,
            allowed_events: vec![HookEvent::OnReceive, HookEvent::AfterSend],
        };
        let out_path = tmp.path().join("out.log");
        let mut params = BTreeMap::new();
        params.insert("path".into(), out_path.to_string_lossy().into_owned());

        let hook = Hook {
            name: Some("tplhook".into()),
            event: HookEvent::OnReceive,
            r#type: "cmd".into(),
            cmd: String::new(),
            dangerously_support_untrusted: true,
            origin: HookOrigin::Operator,
            template: Some("echoer".into()),
            params,
            run_as: None,
        };

        let mut cfg = sample_config();
        cfg.hook_templates.push(tmpl);
        let mailbox = MailboxConfig {
            address: "*@test.com".to_string(),
            owner: "aimx-catchall".to_string(),
            hooks: vec![hook],
            trust: Some("none".to_string()),
            trusted_senders: Some(vec![]),
        };
        let meta = sample_metadata();
        let ctx = OnReceiveContext {
            filepath: &filepath,
            metadata: &meta,
        };
        execute_on_receive(&cfg, &mailbox, &ctx);

        assert!(
            logs_contain("template=echoer"),
            "log line should carry template=echoer"
        );
        assert!(std::fs::read_to_string(&out_path).unwrap().contains("hi"));
    }

    #[tracing_test::traced_test]
    #[test]
    fn execute_on_receive_zero_hooks_emits_no_hooks_log() {
        let mailbox = MailboxConfig {
            address: "*@test.com".to_string(),
            owner: "aimx-catchall".to_string(),
            hooks: vec![],
            trust: Some("none".to_string()),
            trusted_senders: Some(vec![]),
        };
        let meta = sample_metadata();
        let tmp = tempfile::TempDir::new().unwrap();
        let filepath = tmp.path().join("test.md");
        let ctx = OnReceiveContext {
            filepath: &filepath,
            metadata: &meta,
        };
        execute_on_receive(&sample_config(), &mailbox, &ctx);

        assert!(logs_contain("aimx::hook"));
        assert!(logs_contain(
            "No hooks found for event=on_receive mailbox=catchall"
        ));
    }

    #[tracing_test::traced_test]
    #[test]
    fn execute_on_receive_skipped_by_gate_emits_skip_log() {
        // A regular hook with trusted=none fails the trust gate, so it
        // must emit the skip log rather than fire.
        let tmp = tempfile::TempDir::new().unwrap();
        let marker = tmp.path().join("fired");
        let mut hook = basic_hook("gated");
        hook.cmd = format!("touch {}", marker.display());
        let mailbox = MailboxConfig {
            address: "*@test.com".to_string(),
            owner: "aimx-catchall".to_string(),
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
        execute_on_receive(&sample_config(), &mailbox, &ctx);

        assert!(!marker.exists(), "gated hook must not fire");
        assert!(logs_contain("hook_name=gated"));
        assert!(logs_contain("event=on_receive"));
        assert!(logs_contain("mailbox=catchall"));
        assert!(logs_contain("skipped"));
        assert!(logs_contain("trusted=none"));
        assert!(logs_contain("dangerously_support_untrusted=false"));
    }

    #[tracing_test::traced_test]
    #[test]
    fn execute_on_receive_emits_pre_fire_log_before_summary() {
        let tmp = tempfile::TempDir::new().unwrap();
        let marker = tmp.path().join("fired");
        let mut hook = basic_hook("firing_hook");
        hook.cmd = format!("touch {}", marker.display());
        let mailbox = MailboxConfig {
            address: "*@test.com".to_string(),
            owner: "aimx-catchall".to_string(),
            hooks: vec![hook],
            trust: Some("none".to_string()),
            trusted_senders: Some(vec![]),
        };
        let mut meta = sample_metadata();
        meta.trusted = "true".to_string();
        let filepath = tmp.path().join("test.md");
        std::fs::write(&filepath, b"test\n").ok();
        let ctx = OnReceiveContext {
            filepath: &filepath,
            metadata: &meta,
        };
        execute_on_receive(&sample_config(), &mailbox, &ctx);

        assert!(marker.exists());
        assert!(logs_contain("firing hook_name=firing_hook"));
        assert!(logs_contain("event=on_receive"));
        assert!(logs_contain("mailbox=catchall"));
        // Post-fire summary is still emitted.
        assert!(logs_contain("exit_code="));
    }

    #[tracing_test::traced_test]
    #[test]
    fn execute_after_send_zero_hooks_emits_no_hooks_log() {
        let mailbox = MailboxConfig {
            address: "alice@test.com".to_string(),
            owner: "root".to_string(),
            hooks: vec![],
            trust: None,
            trusted_senders: None,
        };
        let ctx = AfterSendContext {
            mailbox: "alice",
            from: "alice@test.com",
            to: "bob@example.com",
            subject: "Hi",
            filepath: "",
            message_id: "<m@test.com>",
            send_status: SendStatus::Delivered,
        };
        execute_after_send(&sample_config(), &mailbox, &ctx);

        assert!(logs_contain(
            "No hooks found for event=after_send mailbox=alice"
        ));
    }

    #[tracing_test::traced_test]
    #[test]
    fn execute_after_send_emits_pre_fire_log() {
        let tmp = tempfile::TempDir::new().unwrap();
        let marker = tmp.path().join("fired");
        let hook = Hook {
            name: Some("after_hook".to_string()),
            event: HookEvent::AfterSend,
            r#type: "cmd".to_string(),
            cmd: format!("touch {}", marker.display()),
            dangerously_support_untrusted: false,
            origin: crate::hook::HookOrigin::Operator,
            template: None,
            params: std::collections::BTreeMap::new(),
            run_as: None,
        };
        let mailbox = MailboxConfig {
            address: "alice@test.com".to_string(),
            owner: "root".to_string(),
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
            message_id: "<m@test.com>",
            send_status: SendStatus::Delivered,
        };
        execute_after_send(&sample_config(), &mailbox, &ctx);

        assert!(marker.exists());
        assert!(logs_contain("firing hook_name=after_hook"));
        assert!(logs_contain("event=after_send"));
        assert!(logs_contain("mailbox=alice"));
    }
}
