use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use crate::hook::{Hook, HookEvent, effective_hook_name, is_valid_hook_name};
use crate::user_resolver::{ResolvedUser, resolve_user};

/// Reserved `run_as` / `owner` values that never require a `getpwnam`
/// resolve. `root` always exists (uid 0, gid 0); `aimx-catchall` is a
/// system user that setup creates on demand when the operator configures
/// a catchall mailbox (PRD §6.4). Hook templates that target either
/// value are accepted even on hosts where `aimx-catchall` hasn't been
/// created yet — the invariant check in [`check_hook_owner_invariant`]
/// is what prevents misconfigured pairings.
pub const RESERVED_RUN_AS_ROOT: &str = "root";
pub const RESERVED_RUN_AS_CATCHALL: &str = "aimx-catchall";

/// True when `name` is one of the reserved `run_as` sentinels
/// (`root` or `aimx-catchall`). Used by orphan-detection paths to
/// short-circuit `getpwnam` lookups against names that the config
/// schema accepts unconditionally.
#[allow(dead_code)]
pub fn is_reserved_run_as(name: &str) -> bool {
    name == RESERVED_RUN_AS_ROOT || name == RESERVED_RUN_AS_CATCHALL
}

/// `useradd`-style valid Linux username regex subset used by
/// [`validate_run_as`]. The full POSIX grammar is `[a-z_][a-z0-9_-]*[$]?`;
/// we hand-roll the predicate here to avoid pulling in a regex crate for
/// one call site. The `$` suffix is allowed for Samba-style trailing
/// machine accounts even though aimx never generates them — rejecting
/// them here would confuse operators who imported such users.
pub const USERNAME_MAX_LEN: usize = 32;

const DEFAULT_DATA_DIR: &str = "/var/lib/aimx";
const DEFAULT_CONFIG_DIR: &str = "/etc/aimx";
const CONFIG_DIR_ENV: &str = "AIMX_CONFIG_DIR";

/// Resolve the configuration directory.
///
/// Precedence:
/// 1. `AIMX_CONFIG_DIR` environment variable (tests, non-standard installs)
/// 2. `/etc/aimx/` default
///
/// Mirrors the `--data-dir` / `AIMX_DATA_DIR` shape used for the storage
/// directory, but is deliberately **independent** of it: `--data-dir`
/// governs `/var/lib/aimx/` (storage), `AIMX_CONFIG_DIR` governs
/// `/etc/aimx/` (config + DKIM secrets).
pub fn config_dir() -> PathBuf {
    std::env::var_os(CONFIG_DIR_ENV)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(DEFAULT_CONFIG_DIR))
}

/// Path to the main `config.toml` file.
pub fn config_path() -> PathBuf {
    config_dir().join("config.toml")
}

/// Path to the DKIM directory containing `private.key` and `public.key`.
pub fn dkim_dir() -> PathBuf {
    config_dir().join("dkim")
}

/// Atomically replace `path` with the TOML serialization of `config`.
///
/// Writes to a sibling `.<filename>.tmp.<pid>` file in the same parent
/// directory as `path`, fsyncs it, then renames over the target. On
/// POSIX `rename(2)` is atomic for same-filesystem targets, so readers
/// see either the old snapshot or the new one; never a truncated file.
/// On failure the temp file is cleaned up best-effort so subsequent
/// retries don't trip over stale state.
///
/// This is the single source of truth for config-file durability used
/// by both the daemon (`mailbox_handler::write_config_atomic` delegates
/// here) and the CLI (`Config::save` delegates here).
///
/// **Unknown-key / comment behaviour (v1):** re-serializes `config`
/// through `toml::to_string_pretty`, so any TOML fields the operator
/// added that are not modeled in the `Config` struct are dropped on
/// rewrite, and human-authored comments are erased. v1 assumes
/// `config.toml` is machine-authored (edits go through `aimx setup` /
/// `aimx mailboxes create|delete` / `aimx hooks ...`).
pub fn write_atomic(path: &Path, config: &Config) -> std::io::Result<()> {
    use std::io::Write;

    let serialized = toml::to_string_pretty(config)
        .map_err(|e| std::io::Error::other(format!("toml serialize: {e}")))?;

    let parent = path.parent().unwrap_or(Path::new("."));
    let file_name = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("config.toml");
    let tmp_name = format!(".{file_name}.tmp.{}", std::process::id());
    let tmp_path = parent.join(tmp_name);

    // Scope the file handle so it closes before rename (paranoia on
    // platforms where an open handle can block a rename).
    {
        let mut f = std::fs::File::create(&tmp_path)?;
        f.write_all(serialized.as_bytes())?;
        f.sync_all()?;
    }

    if let Err(e) = std::fs::rename(&tmp_path, path) {
        let _ = std::fs::remove_file(&tmp_path);
        return Err(e);
    }
    Ok(())
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub struct Config {
    pub domain: String,

    #[serde(default = "default_data_dir")]
    pub data_dir: PathBuf,

    #[serde(default = "default_dkim_selector")]
    pub dkim_selector: String,

    /// Default trust policy applied to every mailbox that does not set
    /// its own `trust`. Allowed values: `"none"` (default) or `"verified"`.
    /// A per-mailbox value fully replaces this default for that mailbox.
    #[serde(default = "default_trust", skip_serializing_if = "is_default_trust")]
    pub trust: String,

    /// Default sender allowlist applied to every mailbox that does not
    /// set its own `trusted_senders`. Glob patterns matched against the
    /// lowercased `From:` address. A per-mailbox value fully replaces this
    /// list (no merging).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub trusted_senders: Vec<String>,

    #[serde(default)]
    pub mailboxes: HashMap<String, MailboxConfig>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verify_host: Option<String>,

    #[serde(default)]
    pub enable_ipv6: bool,

    /// Optional `[upgrade]` section. Overrides the release-manifest URL used
    /// by `aimx upgrade`. The `AIMX_RELEASE_MANIFEST_URL` env
    /// var takes precedence over this value when both are set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upgrade: Option<UpgradeConfig>,
}

/// Operator-overridable knobs for `aimx upgrade`. Today only the manifest URL
/// is configurable; the struct is named for extension (e.g. future proxy,
/// timeout, or channel settings land here without breaking the TOML shape).
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq, Default)]
pub struct UpgradeConfig {
    /// URL the release fetcher hits instead of the GitHub Releases API.
    /// Supports `file://` for offline fixtures.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub release_manifest_url: Option<String>,
}

/// Resolved shape for a `run_as` value returned by [`validate_run_as`].
///
/// `Reserved` short-circuits the `getpwnam` call entirely for `root` and
/// `aimx-catchall`; `User` carries the resolved numeric uid so callers
/// don't have to re-resolve. An orphan `run_as` (regex-valid but absent
/// from `getpwnam`) never produces this type — the caller sees
/// [`ConfigError::OrphanUser`] so it can decide whether to warn (config
/// load) or hard-fail (`aimx agents setup`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunAsKind {
    /// `root` or `aimx-catchall`. The inner `&'static str` echoes the
    /// canonical lowercase name.
    Reserved(&'static str),
    /// A `getpwnam`-resolved Linux user. `uid` is the numeric id;
    /// `name` is the normalized username (identical to the input).
    User(ResolvedUser),
}

/// Validation errors for `run_as` / `owner` names.
///
/// `InvalidUsername` is a hard-failure (regex-invalid names can never
/// resolve). `OrphanUser` is a soft-signal — callers decide whether to
/// warn (config load, per PRD §6.1 orphan tolerance) or reject
/// (`aimx agents setup` registration, where the user must exist now).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigError {
    InvalidUsername(String),
    OrphanUser(String),
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConfigError::InvalidUsername(name) => write!(
                f,
                "invalid username '{name}': must match [a-z_][a-z0-9_-]*[$]?"
            ),
            ConfigError::OrphanUser(name) => write!(
                f,
                "user '{name}' does not exist on this host (getpwnam miss)"
            ),
        }
    }
}

impl std::error::Error for ConfigError {}

/// Predicate: does `s` match the standard `useradd` regex
/// `[a-z_][a-z0-9_-]*[$]?`? Empty strings and over-long strings fail.
/// Keeps parity with Linux `useradd` so aimx accepts every username the
/// system itself accepts.
pub fn is_valid_system_username(s: &str) -> bool {
    if s.is_empty() || s.len() > USERNAME_MAX_LEN {
        return false;
    }
    let mut bytes = s.bytes();
    let first = match bytes.next() {
        Some(b) => b,
        None => return false,
    };
    if !(first.is_ascii_lowercase() || first == b'_') {
        return false;
    }
    // Optional trailing `$` (Samba machine account suffix).
    let rest: Vec<u8> = bytes.collect();
    let (body, _tail) = match rest.last() {
        Some(b'$') => (&rest[..rest.len() - 1], Some(b'$')),
        _ => (&rest[..], None),
    };
    body.iter()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'_' || *b == b'-')
}

/// Validate a `run_as` (or `owner`) name. Reserved names short-circuit;
/// everything else must pass the regex gate and then resolve via
/// `getpwnam`. See [`RunAsKind`] and [`ConfigError`] for the return
/// shape.
pub fn validate_run_as(name: &str) -> Result<RunAsKind, ConfigError> {
    if name == RESERVED_RUN_AS_ROOT {
        return Ok(RunAsKind::Reserved(RESERVED_RUN_AS_ROOT));
    }
    if name == RESERVED_RUN_AS_CATCHALL {
        return Ok(RunAsKind::Reserved(RESERVED_RUN_AS_CATCHALL));
    }
    if !is_valid_system_username(name) {
        return Err(ConfigError::InvalidUsername(name.to_string()));
    }
    match resolve_user(name) {
        Some(u) => Ok(RunAsKind::User(u)),
        None => Err(ConfigError::OrphanUser(name.to_string())),
    }
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub struct MailboxConfig {
    pub address: String,

    /// Linux user that owns this mailbox's storage under
    /// `/var/lib/aimx/{inbox,sent}/<mailbox>/`. Required (no default);
    /// missing `owner` on any mailbox fails `Config::load`.
    ///
    /// Resolved via `getpwnam` at load time. Unknown users produce a
    /// [`LoadWarning::OrphanMailboxOwner`] rather than a hard failure,
    /// and the mailbox is flagged inactive for the session (PRD §6.2).
    pub owner: String,

    /// v2 schema: hooks grouped by event (`on_receive`, `after_send`).
    /// Replaces the v1 `on_receive` array-of-tables. Legacy schema is
    /// rejected at `Config::load` with a migration error.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hooks: Vec<Hook>,

    /// Per-mailbox override for the global [`Config::trust`] default.
    /// `None` means "inherit the global default"; `Some("none" | "verified")`
    /// replaces it. Use [`MailboxConfig::effective_trust`] to resolve.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trust: Option<String>,

    /// Per-mailbox override for the global [`Config::trusted_senders`]
    /// default. `None` means "inherit"; `Some(vec)` replaces the global
    /// list entirely (no merging). Use
    /// [`MailboxConfig::effective_trusted_senders`] to resolve.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trusted_senders: Option<Vec<String>>,

    /// Escape hatch for running a catchall mailbox with `owner = "root"`
    /// (PRD §6.2). Default `false`; only honoured when the mailbox is a
    /// catchall AND `owner = "root"`. `Config::load` rejects the flag on
    /// any non-catchall mailbox and emits a `LoadWarning` when it is set
    /// alongside `owner = "root"` on a catchall so operators see the
    /// escape-hatch acknowledgement in `aimx logs`. Omitted from the
    /// serialized `config.toml` when false so standard configs stay
    /// minimal.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub allow_root_catchall: bool,
}

impl MailboxConfig {
    /// True iff this mailbox's `address` is the wildcard catchall for
    /// the configured `domain` (`*@domain`). Used by
    /// [`check_hook_owner_invariant`] to relax the owner-match rule for
    /// the catchall (hooks run as `aimx-catchall` there, not the
    /// mailbox owner).
    pub fn is_catchall(&self, config: &Config) -> bool {
        self.address
            .eq_ignore_ascii_case(&format!("*@{}", config.domain))
    }

    /// Resolve the mailbox's `owner` via [`validate_run_as`]. Returns
    /// the resolved `uid`, or a [`ConfigError`] the caller decides how
    /// to handle. Loops through [`validate_run_as`] to keep one code
    /// path for regex + `getpwnam` semantics.
    ///
    /// Wired into the chown paths; callers use it only through tests
    /// today.
    #[allow(dead_code)]
    pub fn owner_uid(&self) -> Result<u32, ConfigError> {
        match validate_run_as(&self.owner)? {
            RunAsKind::Reserved(RESERVED_RUN_AS_ROOT) => Ok(0),
            RunAsKind::Reserved(_) => match resolve_user(&self.owner) {
                Some(u) => Ok(u.uid),
                None => Err(ConfigError::OrphanUser(self.owner.clone())),
            },
            RunAsKind::User(u) => Ok(u.uid),
        }
    }

    /// Resolve the mailbox's `owner` primary gid. Mirrors
    /// [`Self::owner_uid`].
    #[allow(dead_code)]
    pub fn owner_gid(&self) -> Result<u32, ConfigError> {
        match validate_run_as(&self.owner)? {
            RunAsKind::Reserved(RESERVED_RUN_AS_ROOT) => Ok(0),
            RunAsKind::Reserved(_) => match resolve_user(&self.owner) {
                Some(u) => Ok(u.gid),
                None => Err(ConfigError::OrphanUser(self.owner.clone())),
            },
            RunAsKind::User(u) => Ok(u.gid),
        }
    }

    /// Iterate only this mailbox's `on_receive` hooks.
    pub fn on_receive_hooks(&self) -> impl Iterator<Item = &Hook> {
        self.hooks
            .iter()
            .filter(|h| h.event == HookEvent::OnReceive)
    }

    /// Iterate only this mailbox's `after_send` hooks.
    pub fn after_send_hooks(&self) -> impl Iterator<Item = &Hook> {
        self.hooks
            .iter()
            .filter(|h| h.event == HookEvent::AfterSend)
    }

    /// Resolve the effective trust policy for this mailbox, falling back
    /// to `config.trust` when the mailbox's own `trust` is `None`.
    pub fn effective_trust<'a>(&'a self, config: &'a Config) -> &'a str {
        self.trust.as_deref().unwrap_or(&config.trust)
    }

    /// Resolve the effective trusted-senders list for this mailbox.
    /// Replace semantics: a `Some(vec)` on the mailbox entirely replaces
    /// the global list, even if empty.
    pub fn effective_trusted_senders<'a>(&'a self, config: &'a Config) -> &'a [String] {
        match &self.trusted_senders {
            Some(list) => list.as_slice(),
            None => config.trusted_senders.as_slice(),
        }
    }
}

/// Non-fatal issues surfaced by [`Config::load`]. The daemon retains the
/// offending mailbox or template in the in-memory config but flags it
/// (via [`ConfigResolved`]) so ingest / MCP / hook-fire paths can treat
/// orphans as inactive. Callers (daemon startup, SIGHUP reload, doctor)
/// log each variant under the `aimx::config` tracing target per PRD §7.4.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoadWarning {
    /// Mailbox `owner` doesn't resolve via `getpwnam`. Mailbox is
    /// flagged inactive for the session.
    OrphanMailboxOwner { mailbox: String, owner: String },
    /// A stale `aimx-hook` system user is present on the host. aimx no
    /// longer creates this user; doctor notes its presence so the
    /// operator can clean up.
    #[allow(dead_code)]
    LegacyAimxHookUser,
    /// Catchall mailbox is configured with `owner = "root"` and the
    /// `allow_root_catchall = true` escape hatch (PRD §6.2). Logged at
    /// WARN so the audit trail records the elevation.
    RootCatchallAccepted { mailbox: String },
}

impl LoadWarning {
    /// Human-readable one-liner for log output. Structured fields live
    /// in the tracing call alongside this rendering.
    pub fn message(&self) -> String {
        match self {
            LoadWarning::OrphanMailboxOwner { mailbox, owner } => format!(
                "mailbox '{mailbox}' owner '{owner}' does not resolve via getpwnam; \
                 mailbox marked inactive — create the user or update config.toml"
            ),
            LoadWarning::LegacyAimxHookUser => {
                "legacy 'aimx-hook' system user present; aimx no longer \
                 manages it — remove via 'userdel aimx-hook' when safe"
                    .to_string()
            }
            LoadWarning::RootCatchallAccepted { mailbox } => format!(
                "catchall mailbox '{mailbox}' is running with owner='root' \
                 and allow_root_catchall=true; this is a documented escape \
                 hatch (PRD §6.2) — mail lands owned by uid 0"
            ),
        }
    }
}

/// Resolved-side view of a loaded `Config`. Keeps orphan flags out of
/// the serializable [`Config`] struct so TOML round-trips stay pure
/// schema. Populated by [`Config::load`] after the orphan-aware
/// validation pass.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConfigResolved {
    pub inactive_mailboxes: std::collections::HashSet<String>,
}

impl ConfigResolved {
    /// True iff the named mailbox is active (owner resolved) in the
    /// current session. Missing names return `false` on the theory that
    /// "does this mailbox exist and can we act on it?" answers the same
    /// way as "no such mailbox."
    ///
    /// Wired into the ingest / MCP / UDS paths.
    #[allow(dead_code)]
    pub fn is_mailbox_active(&self, name: &str) -> bool {
        !self.inactive_mailboxes.contains(name)
    }
}

fn default_trust() -> String {
    "none".to_string()
}

fn is_default_trust(s: &str) -> bool {
    s == "none"
}

fn default_data_dir() -> PathBuf {
    PathBuf::from(DEFAULT_DATA_DIR)
}

fn default_dkim_selector() -> String {
    "aimx".to_string()
}

/// Allowed values for `Config::trust` and per-mailbox `MailboxConfig::trust`.
/// Validated at config load time (`Config::load`) so typos fail fast with a
/// clear error rather than silently fail-closed at runtime via
/// [`crate::trust::evaluate_trust`] / [`crate::hook::should_fire_on_receive`].
pub const VALID_TRUST_VALUES: &[&str] = &["none", "verified"];

/// Pre-parse check: reject the legacy schema (template hooks, `run_as`,
/// `origin`, `dangerously_support_untrusted`, `email_json`, and the
/// even-older `[[mailboxes.<name>.on_receive]]` array form) before the
/// TOML parser sees them.
///
/// No compat shim is offered. Users hand-editing old configs see a
/// single actionable error naming the offending construct.
fn reject_legacy_schema(toml_text: &str) -> Result<(), Box<dyn std::error::Error>> {
    // Track whether we're currently inside a `[[mailboxes.<name>.hooks]]`
    // block so legacy hook-only fields (run_as / origin / dangerously_*)
    // are rejected only where they would have appeared on a hook stanza.
    let mut in_hook_aoa = false;

    for line in toml_text.lines() {
        let trimmed = line.trim();

        // Section / array-of-tables header: refresh the in-hook flag.
        if let Some(rest) = trimmed.strip_prefix("[[")
            && let Some(inner) = rest.strip_suffix("]]")
        {
            if inner == "hook_template" || inner.starts_with("hook_template.") {
                return Err(
                    "[[hook_template]] blocks are not supported; template hooks \
                     were removed. See book/hooks.md for the supported raw-cmd schema"
                        .to_string()
                        .into(),
                );
            }
            if let Some(rest) = inner.strip_prefix("mailboxes.") {
                if let Some(name) = rest.strip_suffix(".on_receive") {
                    return Err(format!(
                        "mailbox '{name}' uses the legacy `on_receive` schema; \
                         migrate to `[[mailboxes.{name}.hooks]]` with \
                         `event = \"on_receive\"`"
                    )
                    .into());
                }
                if rest.ends_with(".hooks") {
                    in_hook_aoa = true;
                    continue;
                }
            }
            in_hook_aoa = false;
            continue;
        }

        if let Some(rest) = trimmed.strip_prefix("[")
            && let Some(inner) = rest.strip_suffix("]")
        {
            if let Some(rest) = inner.strip_prefix("mailboxes.")
                && let Some(name_with_dot) = rest.strip_suffix(".match")
                && let Some(name) = name_with_dot.strip_suffix(".on_receive")
            {
                return Err(format!(
                    "mailbox '{name}' uses the legacy `on_receive.match` schema; \
                     migrate to `[[mailboxes.{name}.hooks]]` with \
                     `event = \"on_receive\"`"
                )
                .into());
            }
            in_hook_aoa = false;
            continue;
        }

        if !in_hook_aoa {
            continue;
        }

        let Some((key, value)) = trimmed.split_once('=') else {
            continue;
        };
        let key = key.trim();
        let value = value.trim();

        match key {
            "run_as" => {
                return Err("hook sets `run_as`; the field was removed — hooks now run \
                     as the mailbox's `owner`"
                    .to_string()
                    .into());
            }
            "origin" => {
                return Err("hook sets `origin`; the field was removed"
                    .to_string()
                    .into());
            }
            "dangerously_support_untrusted" => {
                return Err("hook sets `dangerously_support_untrusted`; the field was \
                     renamed to `fire_on_untrusted`"
                    .to_string()
                    .into());
            }
            "template" | "params" => {
                return Err(format!(
                    "hook sets `{key}`; template hooks were removed. \
                     See book/hooks.md for the supported raw-cmd schema"
                )
                .into());
            }
            "stdin" if value.contains("email_json") => {
                return Err("email_json stdin mode was removed; use stdin = \"email\""
                    .to_string()
                    .into());
            }
            _ => {}
        }
    }
    Ok(())
}

/// Reject `fire_on_untrusted = true` on `after_send` hooks, and any hook
/// attached to the catchall mailbox. Runs after TOML parsing so the
/// rejection can leverage the resolved schema instead of grepping the
/// text.
fn reject_post_parse_legacy(config: &Config) -> Result<(), String> {
    for (name, mb) in &config.mailboxes {
        if mb.is_catchall(config) && !mb.hooks.is_empty() {
            return Err(format!(
                "mailbox '{name}' is the catchall but has hooks attached; \
                 catchall does not support hooks"
            ));
        }
        for hook in &mb.hooks {
            if hook.fire_on_untrusted && hook.event != HookEvent::OnReceive {
                let label = hook.name.clone().unwrap_or_else(|| "<anonymous>".into());
                return Err(format!(
                    "hook '{label}' on mailbox '{name}' sets \
                     `fire_on_untrusted = true` on event '{}': \
                     fire_on_untrusted is on_receive only",
                    hook.event.as_str()
                ));
            }
        }
    }
    Ok(())
}

pub(crate) fn validate_hooks(config: &Config) -> Result<(), String> {
    // Effective-name map: name -> (mailbox, is_explicit).
    let mut seen: HashMap<String, (String, bool)> = HashMap::new();

    for (mailbox_name, mb) in &config.mailboxes {
        for hook in &mb.hooks {
            let label = hook.name.clone().unwrap_or_else(|| "<anonymous>".into());
            if let Some(name) = &hook.name
                && !is_valid_hook_name(name)
            {
                return Err(format!(
                    "invalid hook name '{name}' on mailbox '{mailbox_name}': \
                     must match [a-zA-Z0-9_][a-zA-Z0-9_.-]{{0,127}}"
                ));
            }
            if hook.cmd.is_empty() {
                return Err(format!(
                    "hook '{label}' on mailbox '{mailbox_name}' has empty `cmd`: \
                     `cmd` must be a non-empty argv array, e.g. `cmd = [\"/bin/echo\", \"hi\"]`"
                ));
            }
            if hook.cmd[0].trim().is_empty() {
                return Err(format!(
                    "hook '{label}' on mailbox '{mailbox_name}' has blank `cmd[0]`"
                ));
            }
            if !std::path::Path::new(&hook.cmd[0]).is_absolute() {
                return Err(format!(
                    "hook '{label}' on mailbox '{mailbox_name}' has non-absolute `cmd[0]` \
                     '{prog}': hooks fire from /var/lib/aimx/... so PATH lookup is brittle; \
                     use an absolute path (e.g. `/bin/echo` instead of `echo`)",
                    prog = hook.cmd[0]
                ));
            }
            if hook.r#type != "cmd" {
                return Err(format!(
                    "hook '{label}' on mailbox '{mailbox_name}' has unsupported type '{}': \
                     only `cmd` is supported",
                    hook.r#type
                ));
            }
            if hook.fire_on_untrusted && hook.event != HookEvent::OnReceive {
                return Err(format!(
                    "hook '{label}' on mailbox '{mailbox_name}' sets \
                     `fire_on_untrusted = true` on event '{}': this flag is \
                     `on_receive` only",
                    hook.event.as_str()
                ));
            }

            let effective = effective_hook_name(hook);
            let is_explicit = hook.name.is_some();
            if let Some((prior_mb, prior_explicit)) =
                seen.insert(effective.clone(), (mailbox_name.clone(), is_explicit))
            {
                return Err(match (prior_explicit, is_explicit) {
                    (true, true) => format!(
                        "duplicate hook name '{effective}' on mailboxes \
                         '{prior_mb}' and '{mailbox_name}': hook names must \
                         be globally unique"
                    ),
                    (false, false) => format!(
                        "anonymous hooks on mailboxes '{prior_mb}' and \
                         '{mailbox_name}' have identical event/cmd and \
                         derive the same name '{effective}': set an explicit \
                         `name` on at least one to disambiguate"
                    ),
                    _ => format!(
                        "explicit hook name '{effective}' on one mailbox \
                         collides with the derived name of an anonymous hook \
                         on another (mailboxes '{prior_mb}' and \
                         '{mailbox_name}'): rename the explicit hook or set \
                         an explicit `name` on the anonymous one"
                    ),
                });
            }
        }
    }
    Ok(())
}

/// Expose for the daemon handler, which needs to pre-validate a single
/// submitted hook stanza before it ever lands in `Config`.
#[allow(dead_code)]
pub(crate) fn validate_single_hook(hook: &Hook) -> Result<(), String> {
    if let Some(name) = &hook.name
        && !is_valid_hook_name(name)
    {
        return Err(format!(
            "invalid hook name '{name}': must match \
             [a-zA-Z0-9_][a-zA-Z0-9_.-]{{0,127}}"
        ));
    }
    if hook.cmd.is_empty() {
        return Err(
            "hook has empty `cmd`: must be a non-empty argv array, e.g. `[\"/bin/echo\", \"hi\"]`"
                .into(),
        );
    }
    if hook.cmd[0].trim().is_empty() {
        return Err("hook has blank `cmd[0]`".into());
    }
    if !std::path::Path::new(&hook.cmd[0]).is_absolute() {
        return Err(format!(
            "hook has non-absolute `cmd[0]` '{prog}': hooks fire from /var/lib/aimx/... so PATH \
             lookup is brittle; use an absolute path",
            prog = hook.cmd[0]
        ));
    }
    if hook.r#type != "cmd" {
        return Err(format!(
            "hook has unsupported type '{}': only `cmd` is supported",
            hook.r#type
        ));
    }
    if hook.fire_on_untrusted && hook.event != HookEvent::OnReceive {
        return Err("`fire_on_untrusted = true` is `on_receive` only".into());
    }
    Ok(())
}

/// Resolve every mailbox `owner` via [`validate_run_as`]. Regex
/// failures hard-reject (symmetric with [`validate_hook_run_as`] — per
/// PRD §6.1 a regex-invalid owner can never resolve, so it is treated
/// as a config typo rather than an orphan). `getpwnam` misses surface
/// as [`LoadWarning::OrphanMailboxOwner`] (PRD §6.2).
///
/// `owner = "root"` is rejected on any non-catchall
/// mailbox. On a catchall it is accepted only when
/// `allow_root_catchall = true` — the escape hatch — and that
/// acceptance surfaces as a [`LoadWarning::RootCatchallAccepted`] so
/// the elevation is logged. `allow_root_catchall = true` on any
/// non-catchall mailbox is rejected hard. On a catchall whose owner is
/// not `root` (for example the default `aimx-catchall`), the flag is a
/// no-op — it only gates the root escape hatch — and is silently
/// accepted.
fn validate_mailbox_owners(config: &Config) -> Result<Vec<LoadWarning>, String> {
    let mut warnings = Vec::new();
    for (name, mb) in &config.mailboxes {
        match validate_run_as(&mb.owner) {
            Ok(_) => {}
            Err(ConfigError::InvalidUsername(_)) => {
                return Err(format!(
                    "mailbox '{name}' has invalid owner '{owner}': must be \
                     'root', 'aimx-catchall', or a valid Linux username \
                     matching [a-z_][a-z0-9_-]*[$]?",
                    owner = mb.owner,
                ));
            }
            Err(ConfigError::OrphanUser(_)) => {
                warnings.push(LoadWarning::OrphanMailboxOwner {
                    mailbox: name.clone(),
                    owner: mb.owner.clone(),
                });
            }
        }

        let is_catchall = mb.is_catchall(config);
        let owner_is_root = mb.owner == RESERVED_RUN_AS_ROOT;

        if mb.allow_root_catchall && !is_catchall {
            return Err(format!(
                "mailbox '{name}' sets allow_root_catchall=true but is not a \
                 catchall (*@{domain}); remove the flag or change the address",
                domain = config.domain,
            ));
        }

        if owner_is_root && !is_catchall {
            return Err(format!(
                "mailbox '{name}' cannot be owned by root; use a regular \
                 Linux user or set 'allow_root_catchall = true' on a \
                 catchall mailbox"
            ));
        }

        if is_catchall && owner_is_root {
            if !mb.allow_root_catchall {
                return Err(format!(
                    "catchall mailbox '{name}' has owner='root' but \
                     allow_root_catchall=false; set allow_root_catchall=true \
                     to opt into the root-catchall escape hatch, or change \
                     owner to 'aimx-catchall' (the default)"
                ));
            }
            warnings.push(LoadWarning::RootCatchallAccepted {
                mailbox: name.clone(),
            });
        }
    }
    Ok(warnings)
}

fn validate_trust_values(config: &Config) -> Result<(), String> {
    if !VALID_TRUST_VALUES.contains(&config.trust.as_str()) {
        return Err(format!(
            "invalid top-level trust value '{}': expected one of {:?}",
            config.trust, VALID_TRUST_VALUES
        ));
    }
    for (name, mb) in &config.mailboxes {
        if let Some(t) = mb.trust.as_deref()
            && !VALID_TRUST_VALUES.contains(&t)
        {
            return Err(format!(
                "invalid trust value '{t}' on mailbox '{name}': expected one of {VALID_TRUST_VALUES:?}"
            ));
        }
    }
    Ok(())
}

impl Config {
    /// Load and validate a `config.toml`. Returns both the parsed
    /// `Config` and a vector of non-fatal warnings (orphan mailbox
    /// owners, orphan hook/template `run_as` users). Callers (daemon
    /// startup, SIGHUP reload, `aimx doctor`) log the warnings under
    /// the `aimx::config` tracing target — the operator sees them in
    /// `aimx logs`.
    ///
    /// Hard errors (regex-invalid names, template/cmd mutual-exclusion
    /// violations, hook/owner invariant violations, …) still return
    /// `Err` so the daemon refuses to start on a misconfigured host.
    pub fn load(path: &Path) -> Result<(Self, Vec<LoadWarning>), Box<dyn std::error::Error>> {
        let content = std::fs::read_to_string(path).map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                format!(
                    "Config file not found at {}. Run 'sudo aimx setup' first",
                    path.display()
                )
                .into()
            } else {
                Box::new(e) as Box<dyn std::error::Error>
            }
        })?;
        reject_legacy_schema(&content)?;
        let config: Config = toml::from_str(&content)?;
        for (name, mb) in &config.mailboxes {
            if mb.owner.trim().is_empty() {
                return Err(format!(
                    "mailbox '{name}' is missing required field 'owner'; \
                     re-run 'sudo aimx setup' or hand-edit config.toml"
                )
                .into());
            }
        }
        validate_trust_values(&config).map_err(|e| -> Box<dyn std::error::Error> { e.into() })?;
        reject_post_parse_legacy(&config)
            .map_err(|e| -> Box<dyn std::error::Error> { e.into() })?;
        let warnings = validate_mailbox_owners(&config)
            .map_err(|e| -> Box<dyn std::error::Error> { e.into() })?;
        validate_hooks(&config).map_err(|e| -> Box<dyn std::error::Error> { e.into() })?;
        Ok((config, warnings))
    }

    /// Persist the config to `path` atomically via temp-then-rename.
    ///
    /// Delegates to [`write_atomic`] so CLI fallback callers (`mailbox
    /// create/delete`, `hooks create/delete`) get the same crash-safety
    /// guarantee the daemon already had via
    /// [`crate::mailbox_handler::write_config_atomic`]. Either the new
    /// snapshot is fully durable or the existing file is left byte-for-byte
    /// unchanged; a `config.toml` is never truncated mid-write.
    pub fn save(&self, path: &Path) -> Result<(), Box<dyn std::error::Error>> {
        write_atomic(path, self).map_err(|e| -> Box<dyn std::error::Error> {
            format!("failed to write {}: {e}", path.display()).into()
        })
    }

    /// Load the config from the canonical path returned by [`config_path`].
    ///
    /// Replaces the old `load_from_data_dir`. Config no longer lives inside
    /// the storage directory. Override via the `AIMX_CONFIG_DIR` env var
    /// (tests, non-standard installs).
    pub fn load_resolved() -> Result<(Self, Vec<LoadWarning>), Box<dyn std::error::Error>> {
        Self::load(&config_path())
    }

    /// Load the config and apply an optional `--data-dir` / `AIMX_DATA_DIR`
    /// override for the storage path. `config.data_dir` is the source of
    /// truth on disk; this helper lets the CLI flag still redirect storage
    /// (its documented purpose) without touching the config file on disk.
    pub fn load_resolved_with_data_dir(
        data_dir_override: Option<&Path>,
    ) -> Result<(Self, Vec<LoadWarning>), Box<dyn std::error::Error>> {
        let (mut cfg, warnings) = Self::load_resolved()?;
        if let Some(dir) = data_dir_override {
            cfg.data_dir = dir.to_path_buf();
        }
        Ok((cfg, warnings))
    }

    /// Convenience wrapper around [`Self::load`] for call sites that
    /// only want the parsed [`Config`] and don't care about warnings.
    /// Warnings are silently discarded — production callers that need
    /// to surface them (daemon startup, SIGHUP reload, doctor) must
    /// use [`Self::load`] directly.
    ///
    /// TODO: when ingest / MCP / UDS paths start consulting
    /// [`ConfigResolved`] directly (rather than relying on
    /// `validate_hooks` + load-time rejection), the transitional
    /// `*_ignore_warnings` helpers should either be pruned from the
    /// callers that newly need the warning stream, or reserved for
    /// test-only call sites. Today every audited caller either
    /// re-parses after a write (where warnings are redundant), is a
    /// pre-serve one-shot CLI (mailbox CRUD, portcheck, main's best-
    /// effort peek), or is a test fixture. Keep the helpers until a new
    /// production path arrives that genuinely needs warnings suppressed.
    pub fn load_ignore_warnings(path: &Path) -> Result<Self, Box<dyn std::error::Error>> {
        Self::load(path).map(|(cfg, _)| cfg)
    }

    /// Companion to [`Self::load_ignore_warnings`] for the resolved
    /// config path. See the audit TODO on
    /// [`Self::load_ignore_warnings`] for the remaining-caller story.
    pub fn load_resolved_ignore_warnings() -> Result<Self, Box<dyn std::error::Error>> {
        Self::load_resolved().map(|(cfg, _)| cfg)
    }

    /// Compute a [`ConfigResolved`] side-table from the load warnings.
    /// Used by ingest / MCP / hook-fire paths to skip inactive
    /// mailboxes and orphan templates without re-running validation.
    ///
    /// Wired into the ingest / MCP / UDS paths; exercised through the
    /// orphan-tolerance unit tests.
    #[allow(dead_code)]
    pub fn resolved_from_warnings(warnings: &[LoadWarning]) -> ConfigResolved {
        let mut resolved = ConfigResolved::default();
        for w in warnings {
            if let LoadWarning::OrphanMailboxOwner { mailbox, .. } = w {
                resolved.inactive_mailboxes.insert(mailbox.clone());
            }
        }
        resolved
    }

    /// Path to a mailbox's inbox directory (`<data_dir>/inbox/<name>/`).
    ///
    /// The datadir splits inbound mail into `inbox/` and outbound sent
    /// copies into `sent/`. `mailbox_dir` remains a shorthand for the
    /// inbox path; callers that want the outbound side use
    /// [`Config::sent_dir`].
    pub fn mailbox_dir(&self, name: &str) -> PathBuf {
        self.inbox_dir(name)
    }

    /// Path to a mailbox's inbox directory (`<data_dir>/inbox/<name>/`).
    pub fn inbox_dir(&self, name: &str) -> PathBuf {
        self.data_dir.join("inbox").join(name)
    }

    /// Path to a mailbox's sent directory (`<data_dir>/sent/<name>/`).
    ///
    /// Sent storage is populated by `aimx serve` on outbound delivery;
    /// the directory is still created on `mailbox create` so the layout
    /// is consistent from day one.
    pub fn sent_dir(&self, name: &str) -> PathBuf {
        self.data_dir.join("sent").join(name)
    }
}

/// Shared, swappable handle to the daemon's in-memory `Config`.
///
/// `aimx serve` does not treat `Config` as immutable. The
/// MAILBOX-CREATE / MAILBOX-DELETE UDS verbs rewrite `config.toml` and
/// then replace the daemon's in-memory snapshot so inbound mail routes
/// correctly on the very next SMTP session. No restart required.
///
/// The concurrency model is deliberately boring: a single
/// `RwLock<Arc<Config>>`. Readers (ingest, send handler, state handler) take
/// a read lock just long enough to clone the inner `Arc`, then release it
/// and use their own snapshot for the rest of the request. Writers
/// (MAILBOX-CREATE / MAILBOX-DELETE) take the write lock only to swap the
/// inner `Arc` after `config.toml` has been atomically renamed into place.
///
/// `RwLock<Arc<Config>>` was chosen over `arc_swap::ArcSwap<Config>`
/// intentionally: a fresh dependency for lock-free reads isn't worth it
/// given the critical section is an `Arc::clone` and the write path runs
/// only on mailbox CRUD. If ingest latency ever shows up in a profile the
/// swap is local.
#[derive(Clone)]
pub struct ConfigHandle {
    inner: Arc<RwLock<Arc<Config>>>,
}

impl ConfigHandle {
    /// Create a fresh handle wrapping `config`.
    pub fn new(config: Config) -> Self {
        Self {
            inner: Arc::new(RwLock::new(Arc::new(config))),
        }
    }

    /// Borrow the current `Config` snapshot. The returned `Arc<Config>` is a
    /// stable view: a subsequent `store` by another task will not mutate
    /// the snapshot the caller already holds.
    pub fn load(&self) -> Arc<Config> {
        let guard = self
            .inner
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        Arc::clone(&guard)
    }

    /// Atomically swap the stored `Config` for `new`. Previous snapshots
    /// remain valid. Callers that already `load`ed continue to see the
    /// pre-swap view until they call `load` again.
    pub fn store(&self, new: Config) {
        let mut guard = self
            .inner
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *guard = Arc::new(new);
    }
}

impl std::fmt::Debug for ConfigHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConfigHandle")
            .field("domain", &self.load().domain)
            .finish_non_exhaustive()
    }
}

/// Test-only helpers for overriding `AIMX_CONFIG_DIR` safely from multiple
/// test modules. Process-wide env is not parallel-safe, so every test that
/// mutates this variable must go through [`ConfigDirOverride`], which
/// serializes mutations behind a module-level [`Mutex`] and restores the
/// previous value on drop.
#[cfg(test)]
pub(crate) mod test_env {
    use super::CONFIG_DIR_ENV;
    use std::path::Path;
    use std::sync::Mutex;

    static CONFIG_DIR_GUARD: Mutex<()> = Mutex::new(());

    pub(crate) struct ConfigDirOverride {
        _guard: std::sync::MutexGuard<'static, ()>,
        prev: Option<std::ffi::OsString>,
    }

    impl ConfigDirOverride {
        pub(crate) fn set(path: &Path) -> Self {
            let guard = CONFIG_DIR_GUARD.lock().unwrap_or_else(|e| e.into_inner());
            let prev = std::env::var_os(CONFIG_DIR_ENV);
            // SAFETY: env mutation serialized via CONFIG_DIR_GUARD.
            unsafe {
                std::env::set_var(CONFIG_DIR_ENV, path);
            }
            Self {
                _guard: guard,
                prev,
            }
        }
    }

    impl Drop for ConfigDirOverride {
        fn drop(&mut self) {
            // SAFETY: env mutation serialized via CONFIG_DIR_GUARD.
            unsafe {
                match &self.prev {
                    Some(v) => std::env::set_var(CONFIG_DIR_ENV, v),
                    None => std::env::remove_var(CONFIG_DIR_ENV),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::test_env::ConfigDirOverride;
    use super::*;
    use tempfile::TempDir;

    fn write_cfg(path: &Path, contents: &str) {
        std::fs::write(path, contents).unwrap();
    }

    #[test]
    fn load_rejects_invalid_global_trust_value() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("config.toml");
        write_cfg(
            &path,
            r#"
domain = "test.com"
trust = "verfied"

[mailboxes.catchall]
address = "*@test.com"
owner = "aimx-catchall"
"#,
        );
        let err = Config::load(&path).unwrap_err().to_string();
        assert!(
            err.contains("invalid top-level trust value 'verfied'"),
            "error should name the offender: {err}"
        );
    }

    #[test]
    fn load_rejects_hook_template_block() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("config.toml");
        write_cfg(
            &path,
            r#"
domain = "test.com"

[[hook_template]]
name = "old"
description = "x"
cmd = ["/bin/true"]
run_as = "root"
"#,
        );
        let err = Config::load(&path).unwrap_err().to_string();
        assert!(
            err.contains("template hooks were removed"),
            "error should explain removal: {err}"
        );
        assert!(
            err.contains("book/hooks.md"),
            "error should link guide: {err}"
        );
    }

    #[test]
    fn load_rejects_hook_run_as() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("config.toml");
        write_cfg(
            &path,
            r#"
domain = "test.com"

[mailboxes.support]
address = "support@test.com"
owner = "ops"

[[mailboxes.support.hooks]]
event = "on_receive"
cmd = "true"
run_as = "ops"
"#,
        );
        let err = Config::load(&path).unwrap_err().to_string();
        assert!(
            err.contains("`run_as`") && err.contains("removed"),
            "error should explain removal: {err}"
        );
    }

    #[test]
    fn load_rejects_hook_origin() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("config.toml");
        write_cfg(
            &path,
            r#"
domain = "test.com"

[mailboxes.support]
address = "support@test.com"
owner = "ops"

[[mailboxes.support.hooks]]
event = "on_receive"
cmd = "true"
origin = "operator"
"#,
        );
        let err = Config::load(&path).unwrap_err().to_string();
        assert!(
            err.contains("`origin`") && err.contains("removed"),
            "error should explain removal: {err}"
        );
    }

    #[test]
    fn load_rejects_dangerously_support_untrusted() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("config.toml");
        write_cfg(
            &path,
            r#"
domain = "test.com"

[mailboxes.support]
address = "support@test.com"
owner = "ops"

[[mailboxes.support.hooks]]
event = "on_receive"
cmd = "true"
dangerously_support_untrusted = true
"#,
        );
        let err = Config::load(&path).unwrap_err().to_string();
        assert!(
            err.contains("dangerously_support_untrusted") && err.contains("fire_on_untrusted"),
            "error should reference the rename: {err}"
        );
    }

    #[test]
    fn load_rejects_hook_template_field_on_hook() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("config.toml");
        write_cfg(
            &path,
            r#"
domain = "test.com"

[mailboxes.support]
address = "support@test.com"
owner = "ops"

[[mailboxes.support.hooks]]
event = "on_receive"
template = "invoke-claude"
"#,
        );
        let err = Config::load(&path).unwrap_err().to_string();
        assert!(
            err.contains("template hooks were removed"),
            "error should explain template removal: {err}"
        );
    }

    #[test]
    fn load_rejects_email_json_stdin() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("config.toml");
        write_cfg(
            &path,
            r#"
domain = "test.com"

[mailboxes.support]
address = "support@test.com"
owner = "ops"

[[mailboxes.support.hooks]]
event = "on_receive"
cmd = "true"
stdin = "email_json"
"#,
        );
        let err = Config::load(&path).unwrap_err().to_string();
        assert!(
            err.contains("email_json"),
            "error should mention email_json: {err}"
        );
    }

    #[test]
    fn load_rejects_catchall_with_hooks() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("config.toml");
        write_cfg(
            &path,
            r#"
domain = "test.com"

[mailboxes.catchall]
address = "*@test.com"
owner = "aimx-catchall"

[[mailboxes.catchall.hooks]]
event = "on_receive"
cmd = ["/bin/true"]
"#,
        );
        let err = Config::load(&path).unwrap_err().to_string();
        assert!(
            err.contains("catchall does not support hooks"),
            "error should reject catchall hooks: {err}"
        );
    }

    #[test]
    fn load_rejects_fire_on_untrusted_on_after_send() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("config.toml");
        write_cfg(
            &path,
            r#"
domain = "test.com"

[mailboxes.support]
address = "support@test.com"
owner = "ops"

[[mailboxes.support.hooks]]
event = "after_send"
cmd = ["/bin/true"]
fire_on_untrusted = true
"#,
        );
        let err = Config::load(&path).unwrap_err().to_string();
        assert!(
            err.contains("fire_on_untrusted is on_receive only"),
            "error should reject fire_on_untrusted on after_send: {err}"
        );
    }

    #[test]
    fn load_accepts_minimal_raw_cmd_hook() {
        let _g = ConfigDirOverride::set(Path::new("/tmp"));
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("config.toml");
        write_cfg(
            &path,
            r#"
domain = "test.com"

[mailboxes.support]
address = "support@test.com"
owner = "ops"

[[mailboxes.support.hooks]]
event = "on_receive"
cmd = ["/bin/true"]
"#,
        );
        let cfg = Config::load_ignore_warnings(&path).unwrap();
        assert_eq!(cfg.mailboxes["support"].hooks.len(), 1);
        let hook = &cfg.mailboxes["support"].hooks[0];
        assert_eq!(hook.cmd, vec!["/bin/true"]);
        assert!(!hook.fire_on_untrusted);
    }

    #[test]
    fn load_accepts_fire_on_untrusted_on_on_receive() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("config.toml");
        write_cfg(
            &path,
            r#"
domain = "test.com"

[mailboxes.support]
address = "support@test.com"
owner = "ops"

[[mailboxes.support.hooks]]
event = "on_receive"
cmd = ["/bin/true"]
fire_on_untrusted = true
"#,
        );
        let cfg = Config::load_ignore_warnings(&path).unwrap();
        let hook = &cfg.mailboxes["support"].hooks[0];
        assert!(hook.fire_on_untrusted);
    }

    #[test]
    fn load_rejects_empty_cmd_array() {
        let _g = ConfigDirOverride::set(Path::new("/tmp"));
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("config.toml");
        write_cfg(
            &path,
            r#"
domain = "test.com"

[mailboxes.support]
address = "support@test.com"
owner = "ops"

[[mailboxes.support.hooks]]
event = "on_receive"
cmd = []
"#,
        );
        let err = Config::load(&path).unwrap_err().to_string();
        assert!(
            err.contains("empty `cmd`"),
            "error should call out empty cmd array: {err}"
        );
    }

    #[test]
    fn load_rejects_non_absolute_cmd_program() {
        let _g = ConfigDirOverride::set(Path::new("/tmp"));
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("config.toml");
        write_cfg(
            &path,
            r#"
domain = "test.com"

[mailboxes.support]
address = "support@test.com"
owner = "ops"

[[mailboxes.support.hooks]]
event = "on_receive"
cmd = ["echo", "hi"]
"#,
        );
        let err = Config::load(&path).unwrap_err().to_string();
        assert!(
            err.contains("non-absolute `cmd[0]`"),
            "error should call out non-absolute cmd[0]: {err}"
        );
    }
}
