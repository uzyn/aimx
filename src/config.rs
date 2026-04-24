use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use crate::hook::{Hook, HookEvent, HookOrigin, effective_hook_name, is_valid_hook_name};
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

    /// Installed hook templates. Each entry is a pre-vetted command shape
    /// an agent can reference via MCP `hook_create`. Populated by
    /// `aimx agent-setup <agent>`. Validated at load time.
    ///
    /// Serialized as `[[hook_template]]` blocks (singular) to match the
    /// PRD wording and the usual TOML convention for array-of-tables.
    #[serde(
        default,
        rename = "hook_template",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub hook_templates: Vec<HookTemplate>,

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

/// Stdin delivery mode for a hook template. The daemon pipes the matching
/// payload to the hook's child process on fire.
#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum HookTemplateStdin {
    /// Pipe the raw `.md` file (TOML frontmatter + body) that ingest wrote.
    #[default]
    Email,
    /// Pipe a JSON object `{ "frontmatter": {...}, "body": "..." }`.
    EmailJson,
    /// Close stdin immediately.
    None,
}

/// A pre-vetted command shape an MCP agent can reference via `hook_create`.
///
/// Placeholders in argv entries (`{name}`) are substituted at fire time
/// with either declared `params` values (operator-supplied via MCP) or
/// built-in values (`{event}`, `{mailbox}`, `{message_id}`, `{from}`,
/// `{subject}`). See [`validate_hook_templates`] for the load-time checks.
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct HookTemplate {
    /// Unique across all `[[hook_template]]` entries. Pattern
    /// `[a-z0-9-]+` to keep it CLI-friendly (`aimx hooks template-enable`).
    pub name: String,

    /// Human-readable one-liner for the `aimx setup` checkbox UI and the
    /// `hook_list_templates` MCP response.
    pub description: String,

    /// Argv for the child process. `cmd[0]` is the binary path — placeholders
    /// are forbidden here (validation enforces this). Subsequent entries may
    /// embed `{name}` placeholders inside their string value.
    pub cmd: Vec<String>,

    /// Declared placeholder names that may appear in `cmd`. Must be a 1:1
    /// set with the placeholders referenced in `cmd` (minus the built-ins).
    #[serde(default)]
    pub params: Vec<String>,

    /// Stdin delivery mode. Defaults to `"email"` (pipe the raw `.md`).
    #[serde(default)]
    pub stdin: HookTemplateStdin,

    /// Unix user the daemon drops to before `exec`. Accepts any
    /// `getpwnam`-resolvable username plus the reserved values `"root"`
    /// and `"aimx-catchall"` (PRD §6.1). Usernames that can't resolve at
    /// `Config::load` time do not hard-fail — the template is flagged as
    /// orphan via [`LoadWarning::OrphanTemplateRunAs`] and callers log a
    /// warning under `aimx::config`. The field is required; there is no
    /// default.
    pub run_as: String,

    /// Hard timeout in seconds. SIGTERM at `timeout_secs`, SIGKILL at
    /// `timeout_secs + 5`. Must be in `[1, 600]`.
    #[serde(default = "default_hook_timeout_secs")]
    pub timeout_secs: u32,

    /// Events the template may be wired to. Defaults to both. An MCP
    /// `hook_create` request that selects a disallowed event is rejected.
    #[serde(default = "default_hook_allowed_events")]
    pub allowed_events: Vec<HookEvent>,
}

fn default_hook_timeout_secs() -> u32 {
    60
}

fn default_hook_allowed_events() -> Vec<HookEvent> {
    vec![HookEvent::OnReceive, HookEvent::AfterSend]
}

/// Maximum allowed value for [`HookTemplate::timeout_secs`].
pub const HOOK_TEMPLATE_TIMEOUT_SECS_MAX: u32 = 600;

/// Built-in placeholders the daemon substitutes at fire time without
/// requiring declaration in `params`.
const HOOK_TEMPLATE_BUILTIN_PLACEHOLDERS: &[&str] =
    &["event", "mailbox", "message_id", "from", "subject"];

/// Resolved shape for a `run_as` value returned by [`validate_run_as`].
///
/// `Reserved` short-circuits the `getpwnam` call entirely for `root` and
/// `aimx-catchall`; `User` carries the resolved numeric uid so callers
/// don't have to re-resolve. An orphan `run_as` (regex-valid but absent
/// from `getpwnam`) never produces this type — the caller sees
/// [`ConfigError::OrphanUser`] so it can decide whether to warn (config
/// load) or hard-fail (`aimx agent-setup`).
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
/// (`aimx agent-setup` registration, where the user must exist now).
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
    /// Hook attached to a mailbox names a `run_as` user that does not
    /// resolve. Fire attempts against this hook are soft-skipped.
    OrphanHookRunAs {
        mailbox: String,
        hook_name: String,
        run_as: String,
    },
    /// `[[hook_template]]` names a `run_as` user that does not resolve.
    /// The template is retained but flagged orphan; doctor surfaces it.
    OrphanTemplateRunAs { template: String, run_as: String },
    /// The hook/mailbox owner invariant (PRD §6.3) was skipped at load
    /// because either the mailbox owner or the hook's effective `run_as`
    /// is orphan-flagged. Per PRD §6.1 the daemon stays up — the hook is
    /// unfireable until the user reappears, at which point the next SIGHUP
    /// / restart re-runs the invariant check.
    HookInvariantSkippedDueToOrphan {
        mailbox: String,
        hook_name: String,
        reason: String,
    },
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
            LoadWarning::OrphanHookRunAs {
                mailbox,
                hook_name,
                run_as,
            } => format!(
                "hook '{hook_name}' on mailbox '{mailbox}' has run_as='{run_as}' \
                 which does not resolve; hook will be soft-skipped on fire"
            ),
            LoadWarning::OrphanTemplateRunAs { template, run_as } => format!(
                "hook template '{template}' has run_as='{run_as}' which does \
                 not resolve; template marked orphan"
            ),
            LoadWarning::HookInvariantSkippedDueToOrphan {
                mailbox,
                hook_name,
                reason,
            } => format!(
                "hook '{hook_name}' on mailbox '{mailbox}': owner/run_as \
                 invariant skipped — {reason}; hook will be soft-skipped \
                 on fire until the user reappears (PRD §6.1)"
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
    pub orphan_templates: std::collections::HashSet<String>,
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

    /// True iff the named template has resolved successfully at load
    /// time.
    #[allow(dead_code)]
    pub fn is_template_active(&self, name: &str) -> bool {
        !self.orphan_templates.contains(name)
    }
}

/// Typed variants returned by [`check_hook_owner_invariant`] when the
/// hook/mailbox owner invariant fails. Lets callers (doctor findings,
/// UDS `HOOK-CREATE` response frames) discriminate by variant instead of
/// string-matching on the rendered message. The `Display` impl produces
/// the same user-facing text the old `Result<(), String>` shape did, so
/// existing tests and error-message assertions keep working.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HookOwnerInvariantError {
    /// Hook is attached to a catchall mailbox but `run_as` is neither
    /// `aimx-catchall` nor `root`.
    CatchallRunAsMismatch {
        mailbox: String,
        hook_label: String,
        run_as: String,
    },
    /// Hook is attached to a non-catchall mailbox and its effective
    /// `run_as` does not equal the mailbox `owner` (and is not `root`).
    OwnerMismatch {
        mailbox: String,
        hook_label: String,
        run_as: String,
        owner: String,
    },
}

impl std::fmt::Display for HookOwnerInvariantError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HookOwnerInvariantError::CatchallRunAsMismatch {
                mailbox,
                hook_label,
                run_as,
            } => write!(
                f,
                "hook '{hook_label}' on catchall mailbox '{mailbox}' has \
                 run_as='{run_as}'; catchall hooks must use run_as='{catchall}' or \
                 run_as='{root}' (PRD §6.3)",
                catchall = RESERVED_RUN_AS_CATCHALL,
                root = RESERVED_RUN_AS_ROOT,
            ),
            HookOwnerInvariantError::OwnerMismatch {
                mailbox,
                hook_label,
                run_as,
                owner,
            } => {
                // When the mailbox owner is already `root` the fallback hint
                // (`or run_as='root'`) would duplicate the primary suggestion,
                // so drop the `or` clause for that one case.
                let fix_suggestion = if owner == RESERVED_RUN_AS_ROOT {
                    format!("set run_as='{RESERVED_RUN_AS_ROOT}'")
                } else {
                    format!(
                        "set run_as='{owner}' or run_as='{root}'",
                        root = RESERVED_RUN_AS_ROOT,
                    )
                };
                write!(
                    f,
                    "hook '{hook_label}' on mailbox '{mailbox}' has run_as='{run_as}' \
                     but the mailbox is owned by '{owner}'; fix: {fix_suggestion} so the \
                     hook can read this mailbox's files (PRD §6.3)"
                )
            }
        }
    }
}

impl std::error::Error for HookOwnerInvariantError {}

/// Hook/mailbox owner invariant (PRD §6.3): for every hook on a mailbox
/// the hook's `run_as` must equal the mailbox's `owner` OR be `root`.
/// Catchall relaxes the equality: `run_as` may be `aimx-catchall` or
/// `root` regardless of the catchall's nominal owner.
///
/// This helper is called both at [`Config::load`] (via
/// [`validate_hooks`]) and in the UDS `HOOK-CREATE` path so both gates
/// speak the exact same rule.
pub fn check_hook_owner_invariant(
    config: &Config,
    mailbox_name: &str,
    mailbox: &MailboxConfig,
    hook: &Hook,
) -> Result<(), HookOwnerInvariantError> {
    let effective_run_as = resolve_effective_run_as(config, hook);
    let Some(run_as) = effective_run_as else {
        // No explicit run_as, no template to inherit from — a raw-cmd
        // hook without explicit run_as is already rejected at load; the
        // UDS path forbids raw-cmd entirely, so this branch is a
        // defensive no-op.
        return Ok(());
    };

    if run_as == RESERVED_RUN_AS_ROOT {
        return Ok(());
    }

    let hook_label = hook.name.clone().unwrap_or_else(|| "<anonymous>".into());

    if mailbox.is_catchall(config) {
        if run_as == RESERVED_RUN_AS_CATCHALL {
            return Ok(());
        }
        return Err(HookOwnerInvariantError::CatchallRunAsMismatch {
            mailbox: mailbox_name.to_string(),
            hook_label,
            run_as,
        });
    }

    if run_as == mailbox.owner {
        return Ok(());
    }

    Err(HookOwnerInvariantError::OwnerMismatch {
        mailbox: mailbox_name.to_string(),
        hook_label,
        run_as,
        owner: mailbox.owner.clone(),
    })
}

/// Compute the effective `run_as` for a hook: explicit `hook.run_as`
/// wins; otherwise fall back to the template's `run_as` if the hook is
/// template-bound and the template exists. Returns `None` when neither
/// source is available (raw-cmd hook without explicit `run_as` — in
/// which case callers treat it as "no invariant to enforce").
fn resolve_effective_run_as(config: &Config, hook: &Hook) -> Option<String> {
    if let Some(explicit) = &hook.run_as {
        return Some(explicit.clone());
    }
    let tmpl_name = hook.template.as_ref()?;
    let tmpl = config
        .hook_templates
        .iter()
        .find(|t| &t.name == tmpl_name)?;
    Some(tmpl.run_as.clone())
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

/// Pre-parse check: reject the legacy `[[mailboxes.<name>.on_receive]]`
/// schema with a clear migration error before the TOML parser sees it.
///
/// No compat shim is offered. Users hand-editing old configs see a
/// single actionable error naming the offending mailbox.
fn reject_legacy_on_receive_schema(toml_text: &str) -> Result<(), Box<dyn std::error::Error>> {
    for line in toml_text.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("[[mailboxes.")
            && let Some(inner) = rest.strip_suffix("]]")
            && let Some(name) = inner.strip_suffix(".on_receive")
        {
            return Err(format!(
                "mailbox '{name}' uses the legacy `on_receive` schema; \
                 migrate to `[[mailboxes.{name}.hooks]]` with \
                 `event = \"on_receive\"`"
            )
            .into());
        }
        if let Some(rest) = trimmed.strip_prefix("[mailboxes.")
            && let Some(inner) = rest.strip_suffix("]")
            && let Some(name_dot_on_receive) = inner.strip_suffix(".match")
            && let Some(name) = name_dot_on_receive.strip_suffix(".on_receive")
        {
            return Err(format!(
                "mailbox '{name}' uses the legacy `on_receive.match` schema; \
                 migrate to `[[mailboxes.{name}.hooks]]` with \
                 `event = \"on_receive\"`"
            )
            .into());
        }
    }
    Ok(())
}

/// Context passed to [`validate_hooks`] describing which mailbox
/// owners / template `run_as` names were already flagged orphan by the
/// surrounding load-time validators. `validate_hooks` uses the sets to
/// downgrade the hook/owner invariant check (PRD §6.3) to a warning
/// when either side is unresolvable — PRD §6.1's orphan tolerance
/// requires that a user deletion cannot kill the daemon.
///
/// Callers that create fresh hooks (UDS `HOOK-CREATE`, `aimx hooks
/// create`) pass `OrphanSkipContext::strict()`: at create time we do
/// not want to silently accept a hook whose `run_as` can't be resolved.
#[derive(Debug, Default)]
pub(crate) struct OrphanSkipContext {
    /// Mailboxes whose `owner` did not resolve at load time.
    pub orphan_mailbox_owners: std::collections::HashSet<String>,
    /// Hook templates whose `run_as` did not resolve at load time.
    pub orphan_templates: std::collections::HashSet<String>,
    /// Per-mailbox hook names whose explicit `run_as` did not resolve.
    /// Keyed by `(mailbox, hook_effective_name)` so we only skip the
    /// specific hook that was flagged, not every hook on the mailbox.
    pub orphan_hook_run_as: std::collections::HashSet<(String, String)>,
}

impl OrphanSkipContext {
    /// Strict context: no orphan skipping. Used by UDS / CLI create
    /// paths where the operator is actively introducing a new hook.
    pub(crate) fn strict() -> Self {
        Self::default()
    }

    /// Build a context from an in-progress warning vector. Called by
    /// [`Config::load`] after the owner / template / hook-run_as
    /// validators have run so the invariant pass can see which names
    /// are already known orphan.
    pub(crate) fn from_warnings(warnings: &[LoadWarning]) -> Self {
        let mut ctx = Self::default();
        for w in warnings {
            match w {
                LoadWarning::OrphanMailboxOwner { mailbox, .. } => {
                    ctx.orphan_mailbox_owners.insert(mailbox.clone());
                }
                LoadWarning::OrphanTemplateRunAs { template, .. } => {
                    ctx.orphan_templates.insert(template.clone());
                }
                LoadWarning::OrphanHookRunAs {
                    mailbox, hook_name, ..
                } => {
                    ctx.orphan_hook_run_as
                        .insert((mailbox.clone(), hook_name.clone()));
                }
                _ => {}
            }
        }
        ctx
    }
}

pub(crate) fn validate_hooks(
    config: &Config,
    orphan_ctx: &OrphanSkipContext,
) -> Result<Vec<LoadWarning>, String> {
    // Effective-name map: name -> (mailbox, is_explicit).
    let mut seen: HashMap<String, (String, bool)> = HashMap::new();
    let template_names: std::collections::HashSet<&str> = config
        .hook_templates
        .iter()
        .map(|t| t.name.as_str())
        .collect();
    let mut warnings: Vec<LoadWarning> = Vec::new();

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
            // Template / cmd mutual exclusion:
            //   - template = Some  → cmd must be empty; params may be set
            //   - template = None  → cmd must be non-empty; params must be empty
            match &hook.template {
                Some(tmpl_name) => {
                    if !hook.cmd.trim().is_empty() {
                        return Err(format!(
                            "hook '{label}' on mailbox '{mailbox_name}' sets both \
                             `template = \"{tmpl_name}\"` and a non-empty `cmd`: \
                             they are mutually exclusive"
                        ));
                    }
                    if !template_names.contains(tmpl_name.as_str()) {
                        return Err(format!(
                            "hook '{label}' on mailbox '{mailbox_name}' references \
                             unknown template '{tmpl_name}': add a matching \
                             `[[hook_template]]` block or re-run `aimx setup`"
                        ));
                    }
                }
                None => {
                    if hook.cmd.trim().is_empty() {
                        return Err(format!(
                            "hook '{label}' on mailbox '{mailbox_name}' has empty `cmd`"
                        ));
                    }
                    if !hook.params.is_empty() {
                        return Err(format!(
                            "hook '{label}' on mailbox '{mailbox_name}' has `params` but no \
                             `template`: params are only valid for template-bound hooks"
                        ));
                    }
                }
            }
            if hook.r#type != "cmd" {
                return Err(format!(
                    "hook '{label}' on mailbox '{mailbox_name}' has unsupported type '{}': \
                     only `cmd` is supported",
                    hook.r#type
                ));
            }
            if hook.dangerously_support_untrusted && hook.event != HookEvent::OnReceive {
                return Err(format!(
                    "hook '{label}' on mailbox '{mailbox_name}' sets \
                     `dangerously_support_untrusted = true` on event \
                     '{}': this flag only applies to `on_receive` hooks",
                    hook.event.as_str()
                ));
            }
            if hook.origin == HookOrigin::Mcp && hook.dangerously_support_untrusted {
                // PRD §6.8: MCP-origin hooks always fire only on trusted
                // mail, regardless of whether they are template-bound or
                // raw-cmd. `dangerously_support_untrusted` is an
                // operator-only knob.
                return Err(format!(
                    "hook '{label}' on mailbox '{mailbox_name}' is MCP-origin and sets \
                     `dangerously_support_untrusted = true`: that flag is operator-only"
                ));
            }

            // Hook/mailbox owner invariant (PRD §6.3). Runs after the
            // template-reference check so we know the template exists
            // (when referenced) before resolving the effective run_as.
            //
            // PRD §6.1 orphan tolerance: if either the mailbox owner or
            // the effective run_as is orphan-flagged by the surrounding
            // validators, downgrade the invariant mismatch to a warning
            // — the hook is unfireable anyway and the daemon must stay
            // up on a user-deletion event.
            let hook_effective_name = effective_hook_name(hook);
            let owner_is_orphan = orphan_ctx.orphan_mailbox_owners.contains(mailbox_name);
            let hook_run_as_is_orphan = orphan_ctx
                .orphan_hook_run_as
                .contains(&(mailbox_name.clone(), hook_effective_name.clone()));
            let template_is_orphan = hook
                .template
                .as_ref()
                .is_some_and(|t| orphan_ctx.orphan_templates.contains(t));
            if owner_is_orphan || hook_run_as_is_orphan || template_is_orphan {
                if let Err(_reason) = check_hook_owner_invariant(config, mailbox_name, mb, hook) {
                    let skip_reason = if owner_is_orphan {
                        format!("mailbox owner '{}' is orphan", mb.owner)
                    } else if template_is_orphan {
                        let tmpl = hook.template.as_deref().unwrap_or("<unknown>");
                        format!("template '{tmpl}' run_as is orphan")
                    } else {
                        let run_as = hook.run_as.as_deref().unwrap_or("<unknown>");
                        format!("hook run_as '{run_as}' is orphan")
                    };
                    warnings.push(LoadWarning::HookInvariantSkippedDueToOrphan {
                        mailbox: mailbox_name.clone(),
                        hook_name: hook_effective_name.clone(),
                        reason: skip_reason,
                    });
                }
            } else if let Err(e) = check_hook_owner_invariant(config, mailbox_name, mb, hook) {
                return Err(e.to_string());
            }

            let effective = hook_effective_name;
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
    Ok(warnings)
}

/// Expose for the daemon handler, which needs to pre-validate a single
/// submitted hook stanza before it ever lands in `Config`. Does not check
/// template existence (that's the caller's responsibility — they hold the
/// `Config` snapshot).
pub(crate) fn validate_single_hook(hook: &Hook) -> Result<(), String> {
    if let Some(name) = &hook.name
        && !is_valid_hook_name(name)
    {
        return Err(format!(
            "invalid hook name '{name}': must match \
             [a-zA-Z0-9_][a-zA-Z0-9_.-]{{0,127}}"
        ));
    }
    match &hook.template {
        Some(tmpl) => {
            if !hook.cmd.trim().is_empty() {
                return Err(format!(
                    "hook sets both `template = \"{tmpl}\"` and a non-empty `cmd`: \
                     they are mutually exclusive"
                ));
            }
        }
        None => {
            if hook.cmd.trim().is_empty() {
                return Err("hook has empty `cmd`".into());
            }
            if !hook.params.is_empty() {
                return Err(
                    "hook has `params` but no `template`: params are only valid for template-bound hooks"
                        .into(),
                );
            }
        }
    }
    if hook.r#type != "cmd" {
        return Err(format!(
            "hook has unsupported type '{}': only `cmd` is supported",
            hook.r#type
        ));
    }
    if hook.dangerously_support_untrusted && hook.event != HookEvent::OnReceive {
        return Err(
            "`dangerously_support_untrusted = true` only applies to `on_receive` hooks".into(),
        );
    }
    if hook.origin == HookOrigin::Mcp && hook.dangerously_support_untrusted {
        // PRD §6.8: MCP-origin hooks always fire only on trusted mail,
        // regardless of whether they are template-bound or raw-cmd.
        return Err(
            "MCP-origin hooks may not set `dangerously_support_untrusted = true`: that flag is operator-only".into(),
        );
    }
    Ok(())
}

/// Iterate every `{placeholder}` substring in `s` and yield the name inside
/// the braces. Matches `\{[a-z0-9_]+\}` greedy from left to right. Unclosed
/// braces are silently ignored (they can never form a valid placeholder
/// anyway and are surfaced to the operator as `cmd[0]` rejection or via
/// substitution failure at fire time in a later change).
fn iter_placeholders(s: &str) -> impl Iterator<Item = &str> {
    let bytes = s.as_bytes();
    let mut i = 0;
    std::iter::from_fn(move || {
        while i < bytes.len() {
            if bytes[i] == b'{' {
                let start = i + 1;
                let mut j = start;
                while j < bytes.len() && bytes[j] != b'}' {
                    let c = bytes[j];
                    let ok = c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'_';
                    if !ok {
                        break;
                    }
                    j += 1;
                }
                if j < bytes.len() && bytes[j] == b'}' && j > start {
                    let placeholder = &s[start..j];
                    i = j + 1;
                    return Some(placeholder);
                }
            }
            i += 1;
        }
        None
    })
}

/// Validate every `[[hook_template]]` block in `config.toml`. Runs at
/// [`Config::load`] time so a malformed template fails daemon startup —
/// not the first inbound email that would fire it.
///
/// Rejection rules (per PRD §6.1):
/// - duplicate template `name`
/// - empty `cmd` array
/// - placeholder `{foo}` anywhere inside `cmd[0]` (the binary path)
/// - placeholder not declared in `params` and not in the built-in set
/// - `params` entry never referenced by any placeholder in `cmd`
/// - `timeout_secs == 0` or `> 600`
/// - `run_as` is regex-invalid as a Linux username
/// - `name` not matching `[a-z0-9-]+`
/// - `allowed_events` empty
///
/// Soft (orphan) issues — `run_as` is regex-valid but `getpwnam` does
/// not resolve — are returned as [`LoadWarning::OrphanTemplateRunAs`]
/// via the second tuple slot. Callers log them and mark the template
/// orphan in [`ConfigResolved`].
pub(crate) fn validate_hook_templates(
    templates: &[HookTemplate],
) -> Result<Vec<LoadWarning>, String> {
    let mut warnings = Vec::new();
    let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for tmpl in templates {
        if !is_valid_template_name(&tmpl.name) {
            return Err(format!(
                "invalid hook template name '{}': must match [a-z0-9-]+",
                tmpl.name
            ));
        }
        if !seen.insert(tmpl.name.as_str()) {
            return Err(format!(
                "duplicate hook template name '{}': template names must be unique",
                tmpl.name
            ));
        }
        if tmpl.cmd.is_empty() {
            return Err(format!("hook template '{}' has empty `cmd`", tmpl.name));
        }
        if tmpl.timeout_secs == 0 || tmpl.timeout_secs > HOOK_TEMPLATE_TIMEOUT_SECS_MAX {
            return Err(format!(
                "hook template '{}' has invalid timeout_secs = {}: must be in [1, {}]",
                tmpl.name, tmpl.timeout_secs, HOOK_TEMPLATE_TIMEOUT_SECS_MAX
            ));
        }
        match validate_run_as(&tmpl.run_as) {
            Ok(_) => {}
            Err(ConfigError::InvalidUsername(_)) => {
                return Err(format!(
                    "hook template '{}' has invalid run_as '{}': must be \
                     'root', 'aimx-catchall', or a valid Linux username \
                     matching [a-z_][a-z0-9_-]*[$]?",
                    tmpl.name, tmpl.run_as
                ));
            }
            Err(ConfigError::OrphanUser(name)) => {
                warnings.push(LoadWarning::OrphanTemplateRunAs {
                    template: tmpl.name.clone(),
                    run_as: name,
                });
            }
        }
        if tmpl.allowed_events.is_empty() {
            return Err(format!(
                "hook template '{}' has empty allowed_events: at least one event must be listed",
                tmpl.name
            ));
        }

        // Declared param names must match the placeholder charset
        // (`[a-z0-9_]+`). Otherwise the placeholder extractor silently
        // skips references like `{FOO}` and the operator gets the less
        // helpful "unused param" error downstream.
        for declared in &tmpl.params {
            if !is_valid_placeholder_name(declared) {
                return Err(format!(
                    "hook template '{}' declares invalid param name '{}': param names must match [a-z0-9_]+",
                    tmpl.name, declared
                ));
            }
            if HOOK_TEMPLATE_BUILTIN_PLACEHOLDERS.contains(&declared.as_str()) {
                return Err(format!(
                    "hook template '{}' declares param '{}' that collides with a built-in placeholder ({})",
                    tmpl.name,
                    declared,
                    HOOK_TEMPLATE_BUILTIN_PLACEHOLDERS.join(", ")
                ));
            }
        }

        // `cmd[0]` is the binary path and never admits placeholders.
        let binary = &tmpl.cmd[0];
        if iter_placeholders(binary).next().is_some() {
            return Err(format!(
                "hook template '{}' has a placeholder in cmd[0] ('{}'): the binary path must be a literal",
                tmpl.name, binary,
            ));
        }

        // Build the set of legal placeholder names (declared params + builtins).
        let param_set: std::collections::HashSet<&str> =
            tmpl.params.iter().map(String::as_str).collect();
        let mut used_params: std::collections::HashSet<&str> = std::collections::HashSet::new();

        for slot in &tmpl.cmd[1..] {
            for placeholder in iter_placeholders(slot) {
                if HOOK_TEMPLATE_BUILTIN_PLACEHOLDERS.contains(&placeholder) {
                    continue;
                }
                if !param_set.contains(placeholder) {
                    return Err(format!(
                        "hook template '{}' references undeclared placeholder '{{{}}}': add it to `params` or use a built-in ({})",
                        tmpl.name,
                        placeholder,
                        HOOK_TEMPLATE_BUILTIN_PLACEHOLDERS.join(", ")
                    ));
                }
                used_params.insert(placeholder);
            }
        }

        // Every declared param must be referenced somewhere. Dead entries
        // usually mean the operator forgot to wire up a placeholder.
        for declared in &tmpl.params {
            if !used_params.contains(declared.as_str()) {
                return Err(format!(
                    "hook template '{}' declares unused param '{}': either reference it via '{{{}}}' in `cmd` or remove it",
                    tmpl.name, declared, declared
                ));
            }
        }
    }
    Ok(warnings)
}

fn is_valid_template_name(s: &str) -> bool {
    !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// Public wrapper for [`is_valid_template_name`] exposed for the UDS
/// `TEMPLATE-*` parsers. Mirrors the `[a-z0-9-]+` rule used by
/// [`validate_hook_templates`].
pub fn is_valid_template_name_str(s: &str) -> bool {
    is_valid_template_name(s)
}

/// True if `cmd[0]` would fail the placeholder check in
/// [`validate_hook_templates`]. Exposed for the UDS `TEMPLATE-*`
/// parsers so they can reject obviously-malformed bodies before handler
/// dispatch.
pub fn cmd_zero_contains_placeholder(binary: &str) -> bool {
    iter_placeholders(binary).next().is_some()
}

/// Charset predicate for `[[hook_template]].params` entries and the
/// placeholder names surfaced by [`iter_placeholders`]. Kept in lockstep
/// with the parser so operators get a precise "invalid param name" error
/// at load time rather than a misleading "unused param".
fn is_valid_placeholder_name(s: &str) -> bool {
    !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
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

/// Resolve every hook's explicit `run_as` (when set) — template
/// inheritance is handled at fire time. Regex failures hard-reject;
/// `getpwnam` misses surface as [`LoadWarning::OrphanHookRunAs`].
fn validate_hook_run_as(config: &Config) -> Result<Vec<LoadWarning>, String> {
    let mut warnings = Vec::new();
    for (mailbox_name, mb) in &config.mailboxes {
        for hook in &mb.hooks {
            let Some(run_as) = &hook.run_as else {
                continue;
            };
            match validate_run_as(run_as) {
                Ok(_) => {}
                Err(ConfigError::InvalidUsername(_)) => {
                    let label = hook.name.clone().unwrap_or_else(|| "<anonymous>".into());
                    return Err(format!(
                        "hook '{label}' on mailbox '{mailbox_name}' has \
                         invalid run_as '{run_as}': must be 'root', \
                         'aimx-catchall', or a valid Linux username \
                         matching [a-z_][a-z0-9_-]*[$]?"
                    ));
                }
                Err(ConfigError::OrphanUser(name)) => {
                    let label = hook.name.clone().unwrap_or_else(|| "<anonymous>".into());
                    warnings.push(LoadWarning::OrphanHookRunAs {
                        mailbox: mailbox_name.clone(),
                        hook_name: label,
                        run_as: name,
                    });
                }
            }
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
        reject_legacy_on_receive_schema(&content)?;
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
        let mut warnings = validate_hook_templates(&config.hook_templates)
            .map_err(|e| -> Box<dyn std::error::Error> { e.into() })?;
        let mailbox_warnings = validate_mailbox_owners(&config)
            .map_err(|e| -> Box<dyn std::error::Error> { e.into() })?;
        warnings.extend(mailbox_warnings);
        let hook_warnings = validate_hook_run_as(&config)
            .map_err(|e| -> Box<dyn std::error::Error> { e.into() })?;
        warnings.extend(hook_warnings);
        // Derive orphan-context for the invariant skip (PRD §6.1): a hook
        // whose effective run_as or mailbox owner is orphan-flagged must
        // not hard-fail the load, since the hook is unfireable anyway.
        let orphan_ctx = OrphanSkipContext::from_warnings(&warnings);
        let invariant_warnings = validate_hooks(&config, &orphan_ctx)
            .map_err(|e| -> Box<dyn std::error::Error> { e.into() })?;
        warnings.extend(invariant_warnings);
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
            match w {
                LoadWarning::OrphanMailboxOwner { mailbox, .. } => {
                    resolved.inactive_mailboxes.insert(mailbox.clone());
                }
                LoadWarning::OrphanTemplateRunAs { template, .. } => {
                    resolved.orphan_templates.insert(template.clone());
                }
                _ => {}
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

    fn sample_toml() -> &'static str {
        r#"
domain = "agent.example.com"
data_dir = "/tmp/aimx-test"

[mailboxes.catchall]
address = "*@agent.example.com"
owner = "aimx-catchall"

[mailboxes.support]
address = "support@agent.example.com"
owner = "ops"

[[mailboxes.support.hooks]]
name = "support_inbound"
event = "on_receive"
type = "cmd"
cmd = 'echo "$AIMX_FROM" >> /tmp/log'
"#
    }

    #[test]
    fn load_rejects_invalid_global_trust_value() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("config.toml");
        std::fs::write(
            &path,
            r#"
domain = "test.com"
trust = "verfied"

[mailboxes.catchall]
address = "*@test.com"
owner = "aimx-catchall"
"#,
        )
        .unwrap();
        let err = Config::load(&path).unwrap_err().to_string();
        assert!(
            err.contains("invalid top-level trust value 'verfied'"),
            "error should name the offender: {err}"
        );
        assert!(
            err.contains("\"none\"") && err.contains("\"verified\""),
            "error should list allowed values: {err}"
        );
    }

    #[test]
    fn load_rejects_invalid_mailbox_trust_value() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("config.toml");
        std::fs::write(
            &path,
            r#"
domain = "test.com"

[mailboxes.support]
address = "support@test.com"
owner = "ops"
trust = "strict"
"#,
        )
        .unwrap();
        let err = Config::load(&path).unwrap_err().to_string();
        assert!(
            err.contains("invalid trust value 'strict'"),
            "error should name the offender: {err}"
        );
        assert!(
            err.contains("support"),
            "error should name the mailbox: {err}"
        );
    }

    #[test]
    fn load_accepts_valid_trust_values() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("config.toml");
        std::fs::write(
            &path,
            r#"
domain = "test.com"
trust = "verified"
trusted_senders = ["*@company.com"]

[mailboxes.catchall]
address = "*@test.com"
owner = "aimx-catchall"

[mailboxes.public]
address = "hello@test.com"
owner = "ops"
trust = "none"
"#,
        )
        .unwrap();
        let cfg = Config::load_ignore_warnings(&path).unwrap();
        assert_eq!(cfg.trust, "verified");
        assert_eq!(cfg.mailboxes["public"].trust.as_deref(), Some("none"));
    }

    #[test]
    fn parse_config() {
        let config: Config = toml::from_str(sample_toml()).unwrap();
        assert_eq!(config.domain, "agent.example.com");
        assert_eq!(config.data_dir, PathBuf::from("/tmp/aimx-test"));
        assert_eq!(config.mailboxes.len(), 2);
        assert!(config.mailboxes.contains_key("catchall"));
        assert!(config.mailboxes.contains_key("support"));
    }

    #[test]
    fn parse_hooks() {
        let config: Config = toml::from_str(sample_toml()).unwrap();
        let support = &config.mailboxes["support"];
        assert_eq!(support.hooks.len(), 1);
        let hook = &support.hooks[0];
        assert_eq!(hook.name.as_deref(), Some("support_inbound"));
        assert_eq!(hook.event, HookEvent::OnReceive);
        assert_eq!(hook.r#type, "cmd");
        assert_eq!(hook.cmd, "echo \"$AIMX_FROM\" >> /tmp/log");
    }

    #[test]
    fn load_accepts_env_var_hook_recipe() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("config.toml");
        std::fs::write(
            &path,
            r#"
domain = "test.com"

[mailboxes.support]
address = "support@test.com"
owner = "ops"

[[mailboxes.support.hooks]]
name = "support_env"
event = "on_receive"
cmd = '''
printf 'from=%s subject=%s id=%s\n' "$AIMX_FROM" "$AIMX_SUBJECT" "{id}"
'''
"#,
        )
        .unwrap();
        let cfg = Config::load_ignore_warnings(&path).unwrap();
        assert_eq!(cfg.mailboxes["support"].hooks.len(), 1);
    }

    #[test]
    fn load_accepts_hook_without_name_and_derives_it() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("config.toml");
        std::fs::write(
            &path,
            r#"
domain = "test.com"

[mailboxes.support]
address = "support@test.com"
owner = "ops"

[[mailboxes.support.hooks]]
event = "on_receive"
cmd = "echo derive-me"
"#,
        )
        .unwrap();
        let cfg = Config::load_ignore_warnings(&path).unwrap();
        let hooks = &cfg.mailboxes["support"].hooks;
        assert_eq!(hooks.len(), 1);
        assert!(
            hooks[0].name.is_none(),
            "raw name must stay None when omitted"
        );
        // Derived name must validate.
        let derived = crate::hook::effective_hook_name(&hooks[0]);
        assert!(crate::hook::is_valid_hook_name(&derived));
    }

    #[test]
    fn load_rejects_unknown_hook_fields() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("config.toml");
        std::fs::write(
            &path,
            r#"
domain = "test.com"

[mailboxes.support]
address = "support@test.com"
owner = "ops"

[[mailboxes.support.hooks]]
event = "on_receive"
cmd = "echo hi"
from = "*@gmail.com"
"#,
        )
        .unwrap();
        let err = Config::load(&path).unwrap_err().to_string();
        assert!(
            err.contains("from") || err.contains("unknown field"),
            "error should flag unknown `from` field: {err}"
        );
    }

    #[test]
    fn load_rejects_typo_hook_field() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("config.toml");
        std::fs::write(
            &path,
            r#"
domain = "test.com"

[mailboxes.support]
address = "support@test.com"
owner = "ops"

[[mailboxes.support.hooks]]
event = "on_receive"
cmd = "echo hi"
subjct = "oops"
"#,
        )
        .unwrap();
        let err = Config::load(&path).unwrap_err().to_string();
        assert!(
            err.contains("subjct") || err.contains("unknown field"),
            "error should flag typo'd field: {err}"
        );
    }

    #[test]
    fn load_rejects_two_anonymous_hooks_with_same_identity() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("config.toml");
        std::fs::write(
            &path,
            r#"
domain = "test.com"

[mailboxes.one]
address = "one@test.com"
owner = "ops"

[[mailboxes.one.hooks]]
event = "on_receive"
cmd = "echo same"

[mailboxes.two]
address = "two@test.com"
owner = "ops"

[[mailboxes.two.hooks]]
event = "on_receive"
cmd = "echo same"
"#,
        )
        .unwrap();
        let err = Config::load(&path).unwrap_err().to_string();
        assert!(
            err.contains("anonymous") || err.contains("set an explicit"),
            "expected disambiguation hint: {err}"
        );
        assert!(err.contains("one") && err.contains("two"), "{err}");
    }

    #[test]
    fn load_rejects_explicit_vs_derived_name_collision() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("config.toml");
        // First compute the derived name for the anonymous hook, then
        // assign the same name explicitly on another mailbox.
        let derived =
            crate::hook::derive_hook_name(crate::hook::HookEvent::OnReceive, "echo collide", false);
        let content = format!(
            r#"
domain = "test.com"

[mailboxes.one]
address = "one@test.com"
owner = "ops"

[[mailboxes.one.hooks]]
event = "on_receive"
cmd = "echo collide"

[mailboxes.two]
address = "two@test.com"
owner = "ops"

[[mailboxes.two.hooks]]
name = "{derived}"
event = "on_receive"
cmd = "echo something_else"
"#
        );
        std::fs::write(&path, content).unwrap();
        let err = Config::load(&path).unwrap_err().to_string();
        assert!(
            err.contains("explicit") || err.contains("derived"),
            "expected collision-class hint: {err}"
        );
    }

    #[test]
    fn load_rejects_legacy_on_receive_schema() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("config.toml");
        std::fs::write(
            &path,
            r#"
domain = "test.com"

[mailboxes.support]
address = "support@test.com"
owner = "ops"

[[mailboxes.support.on_receive]]
type = "cmd"
command = "echo hi"
"#,
        )
        .unwrap();
        let err = Config::load(&path).unwrap_err().to_string();
        assert!(
            err.contains("legacy `on_receive` schema"),
            "error should name the schema: {err}"
        );
        assert!(
            err.contains("support"),
            "error should name the mailbox: {err}"
        );
        assert!(
            err.contains("[[mailboxes.support.hooks]]"),
            "error should point at the new schema: {err}"
        );
        assert!(
            err.contains("event = \"on_receive\""),
            "error should show the migration: {err}"
        );
    }

    #[test]
    fn load_rejects_legacy_on_receive_match_block() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("config.toml");
        std::fs::write(
            &path,
            r#"
domain = "test.com"

[mailboxes.support]
address = "support@test.com"
owner = "ops"

[[mailboxes.support.hooks]]
name = "support_legacy"
event = "on_receive"
cmd = "echo hi"

[mailboxes.support.on_receive.match]
from = "*@gmail.com"
"#,
        )
        .unwrap();
        let err = Config::load(&path).unwrap_err().to_string();
        assert!(
            err.contains("legacy"),
            "error should flag legacy match block: {err}"
        );
    }

    #[test]
    fn load_rejects_missing_hook_event() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("config.toml");
        std::fs::write(
            &path,
            r#"
domain = "test.com"

[mailboxes.support]
address = "support@test.com"
owner = "ops"

[[mailboxes.support.hooks]]
name = "h1"
cmd = "echo hi"
"#,
        )
        .unwrap();
        let err = Config::load(&path).unwrap_err().to_string();
        assert!(
            err.to_ascii_lowercase().contains("event"),
            "error should complain about missing event: {err}"
        );
    }

    #[test]
    fn load_rejects_unknown_hook_event() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("config.toml");
        std::fs::write(
            &path,
            r#"
domain = "test.com"

[mailboxes.support]
address = "support@test.com"
owner = "ops"

[[mailboxes.support.hooks]]
name = "h1"
event = "before_send"
cmd = "echo hi"
"#,
        )
        .unwrap();
        let err = Config::load(&path).unwrap_err().to_string();
        assert!(
            err.to_ascii_lowercase().contains("event")
                || err.to_ascii_lowercase().contains("before_send"),
            "error should name the offending event: {err}"
        );
    }

    #[test]
    fn load_rejects_malformed_hook_name() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("config.toml");
        std::fs::write(
            &path,
            r#"
domain = "test.com"

[mailboxes.support]
address = "support@test.com"
owner = "ops"

[[mailboxes.support.hooks]]
name = "bad name!"
event = "on_receive"
cmd = "echo hi"
"#,
        )
        .unwrap();
        let err = Config::load(&path).unwrap_err().to_string();
        assert!(
            err.to_ascii_lowercase().contains("invalid hook name"),
            "error should reject invalid name: {err}"
        );
    }

    #[test]
    fn load_rejects_duplicate_explicit_hook_names_across_mailboxes() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("config.toml");
        std::fs::write(
            &path,
            r#"
domain = "test.com"

[mailboxes.one]
address = "one@test.com"
owner = "ops"

[[mailboxes.one.hooks]]
name = "same_name"
event = "on_receive"
cmd = "echo one"

[mailboxes.two]
address = "two@test.com"
owner = "ops"

[[mailboxes.two.hooks]]
name = "same_name"
event = "on_receive"
cmd = "echo two"
"#,
        )
        .unwrap();
        let err = Config::load(&path).unwrap_err().to_string();
        assert!(
            err.contains("duplicate hook name"),
            "error should flag duplicate: {err}"
        );
        assert!(
            err.contains("one") && err.contains("two"),
            "error should name both mailboxes: {err}"
        );
    }

    #[test]
    fn load_rejects_dangerously_flag_on_after_send() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("config.toml");
        std::fs::write(
            &path,
            r#"
domain = "test.com"

[mailboxes.support]
address = "support@test.com"
owner = "ops"

[[mailboxes.support.hooks]]
name = "h1"
event = "after_send"
cmd = "echo hi"
dangerously_support_untrusted = true
"#,
        )
        .unwrap();
        let err = Config::load(&path).unwrap_err().to_string();
        assert!(
            err.contains("dangerously_support_untrusted"),
            "error should name the flag: {err}"
        );
        assert!(
            err.contains("on_receive"),
            "error should mention on_receive: {err}"
        );
    }

    #[test]
    fn default_data_dir() {
        let toml_str = "domain = \"test.com\"\n[mailboxes]\n";
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(config.data_dir, PathBuf::from("/var/lib/aimx"));
    }

    #[test]
    fn default_dkim_selector_is_aimx() {
        assert_eq!(super::default_dkim_selector(), "aimx");
        let toml_str = "domain = \"test.com\"\n[mailboxes]\n";
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(config.dkim_selector, "aimx");
    }

    #[test]
    fn save_and_load_roundtrip() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("config.toml");

        let mut mailboxes = HashMap::new();
        mailboxes.insert(
            "catchall".to_string(),
            MailboxConfig {
                address: "*@test.com".to_string(),
                owner: "aimx-catchall".to_string(),
                hooks: vec![],
                trust: None,
                trusted_senders: None,
                allow_root_catchall: false,
            },
        );

        let config = Config {
            domain: "test.com".to_string(),
            data_dir: tmp.path().to_path_buf(),
            dkim_selector: "aimx".to_string(),
            trust: "none".to_string(),
            trusted_senders: vec![],
            hook_templates: Vec::new(),
            mailboxes,
            verify_host: None,
            enable_ipv6: false,
            upgrade: None,
        };

        config.save(&path).unwrap();
        let loaded = Config::load_ignore_warnings(&path).unwrap();
        assert_eq!(config, loaded);
    }

    /// `Config::save` must be crash-safe.
    /// When the underlying write fails (here: parent dir missing, so
    /// `File::create` on the temp file returns ENOENT), the on-disk
    /// target file must remain byte-for-byte unchanged.
    #[test]
    fn save_is_atomic_and_preserves_original_on_failure() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("config.toml");

        let mut mailboxes = HashMap::new();
        mailboxes.insert(
            "catchall".to_string(),
            MailboxConfig {
                address: "*@test.com".to_string(),
                owner: "aimx-catchall".to_string(),
                hooks: vec![],
                trust: None,
                trusted_senders: None,
                allow_root_catchall: false,
            },
        );
        let original = Config {
            domain: "test.com".to_string(),
            data_dir: tmp.path().to_path_buf(),
            dkim_selector: "aimx".to_string(),
            trust: "none".to_string(),
            trusted_senders: vec![],
            hook_templates: Vec::new(),
            mailboxes,
            verify_host: None,
            enable_ipv6: false,
            upgrade: None,
        };
        // Seed the file via the happy path first.
        original.save(&path).unwrap();
        let original_bytes = std::fs::read(&path).unwrap();

        // Mutate a copy, then force `save` to fail by pointing at a path
        // whose parent directory does not exist. `write_atomic` creates
        // the temp file in that (missing) parent, so `File::create`
        // returns ENOENT before any rename can run.
        let mut mutated = original.clone();
        mutated.domain = "changed.example".to_string();
        let bad_parent = tmp.path().join("does").join("not").join("exist");
        let bad_target = bad_parent.join("config.toml");
        let err = mutated.save(&bad_target).unwrap_err();
        assert!(
            err.to_string().contains("config.toml") || err.to_string().contains("No such"),
            "save error should mention the target or the missing-path cause: {err}"
        );

        // Original file on disk is untouched.
        let after = std::fs::read(&path).unwrap();
        assert_eq!(
            after, original_bytes,
            "failed save must not disturb the original config.toml",
        );

        // No stray temp file in the original parent dir.
        let leftover: Vec<_> = std::fs::read_dir(tmp.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.file_name()
                    .to_string_lossy()
                    .starts_with(".config.toml.tmp.")
            })
            .collect();
        assert!(
            leftover.is_empty(),
            "temp file must not leak into the happy-path parent dir",
        );
    }

    /// `write_atomic` drops the temp file in the *target's* parent so
    /// `rename(2)` stays on one filesystem. A successful write over an
    /// existing target replaces the inode contents; a failed write
    /// leaves the target bytes alone.
    #[test]
    fn write_atomic_uses_target_parent_for_temp_file() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("config.toml");

        let mut mailboxes = HashMap::new();
        mailboxes.insert(
            "catchall".to_string(),
            MailboxConfig {
                address: "*@test.com".to_string(),
                owner: "aimx-catchall".to_string(),
                hooks: vec![],
                trust: None,
                trusted_senders: None,
                allow_root_catchall: false,
            },
        );
        let cfg = Config {
            domain: "test.com".to_string(),
            data_dir: tmp.path().to_path_buf(),
            dkim_selector: "aimx".to_string(),
            trust: "none".to_string(),
            trusted_senders: vec![],
            hook_templates: Vec::new(),
            mailboxes,
            verify_host: None,
            enable_ipv6: false,
            upgrade: None,
        };
        super::write_atomic(&path, &cfg).unwrap();
        assert!(path.exists());
        // On success the temp file is renamed; no `.tmp.` stragglers.
        let leftover: Vec<_> = std::fs::read_dir(tmp.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp."))
            .collect();
        assert!(
            leftover.is_empty(),
            "temp file should be renamed, not left behind"
        );
    }

    #[test]
    fn parse_trust_settings() {
        let toml_str = r#"
domain = "test.com"

[mailboxes.secure]
address = "secure@test.com"
owner = "ops"
trust = "verified"
trusted_senders = ["*@company.com", "boss@gmail.com"]
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        let secure = &config.mailboxes["secure"];
        assert_eq!(secure.trust.as_deref(), Some("verified"));
        let senders = secure.trusted_senders.as_ref().unwrap();
        assert_eq!(senders.len(), 2);
        assert_eq!(senders[0], "*@company.com");
        assert_eq!(senders[1], "boss@gmail.com");
    }

    #[test]
    fn default_trust_is_none() {
        let toml_str = r#"
domain = "test.com"

[mailboxes.catchall]
address = "*@test.com"
owner = "aimx-catchall"
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(config.trust, "none");
        assert!(config.trusted_senders.is_empty());
        let catchall = &config.mailboxes["catchall"];
        assert!(catchall.trust.is_none());
        assert!(catchall.trusted_senders.is_none());
        assert_eq!(catchall.effective_trust(&config), "none");
        assert!(catchall.effective_trusted_senders(&config).is_empty());
    }

    #[test]
    fn parse_global_trust_defaults() {
        let toml_str = r#"
domain = "test.com"
trust = "verified"
trusted_senders = ["*@company.com"]

[mailboxes.catchall]
address = "*@test.com"
owner = "aimx-catchall"
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(config.trust, "verified");
        assert_eq!(config.trusted_senders, vec!["*@company.com".to_string()]);
        let catchall = &config.mailboxes["catchall"];
        assert!(catchall.trust.is_none());
        assert!(catchall.trusted_senders.is_none());
        assert_eq!(catchall.effective_trust(&config), "verified");
        assert_eq!(
            catchall.effective_trusted_senders(&config),
            ["*@company.com".to_string()].as_slice()
        );
    }

    #[test]
    fn mailbox_trust_overrides_global() {
        let toml_str = r#"
domain = "test.com"
trust = "verified"
trusted_senders = ["*@company.com"]

[mailboxes.public]
address = "hello@test.com"
owner = "ops"
trust = "none"
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        let public = &config.mailboxes["public"];
        assert_eq!(public.effective_trust(&config), "none");
        // trusted_senders is still inherited (only `trust` was overridden).
        assert_eq!(
            public.effective_trusted_senders(&config),
            ["*@company.com".to_string()].as_slice()
        );
    }

    #[test]
    fn mailbox_trusted_senders_replaces_global() {
        let toml_str = r#"
domain = "test.com"
trust = "verified"
trusted_senders = ["*@company.com"]

[mailboxes.strict]
address = "strict@test.com"
owner = "ops"
trusted_senders = ["boss@gmail.com"]
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        let strict = &config.mailboxes["strict"];
        // trust inherited, senders fully replaced.
        assert_eq!(strict.effective_trust(&config), "verified");
        assert_eq!(
            strict.effective_trusted_senders(&config),
            ["boss@gmail.com".to_string()].as_slice()
        );
    }

    #[test]
    fn mailbox_empty_trusted_senders_kills_global_list() {
        let toml_str = r#"
domain = "test.com"
trust = "verified"
trusted_senders = ["*@company.com"]

[mailboxes.sealed]
address = "sealed@test.com"
owner = "ops"
trusted_senders = []
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        let sealed = &config.mailboxes["sealed"];
        assert!(sealed.effective_trusted_senders(&config).is_empty());
    }

    #[test]
    fn save_omits_default_top_level_trust_fields() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("config.toml");

        let config = Config {
            domain: "test.com".to_string(),
            data_dir: PathBuf::from("/tmp/x"),
            dkim_selector: "aimx".to_string(),
            trust: "none".to_string(),
            trusted_senders: vec![],
            hook_templates: Vec::new(),
            mailboxes: HashMap::new(),
            verify_host: None,
            enable_ipv6: false,
            upgrade: None,
        };
        config.save(&path).unwrap();
        let on_disk = std::fs::read_to_string(&path).unwrap();
        assert!(
            !on_disk.contains("trust = "),
            "default trust should not serialize: {on_disk}"
        );
        assert!(
            !on_disk.contains("trusted_senders"),
            "default trusted_senders should not serialize: {on_disk}"
        );
    }

    #[test]
    fn save_omits_unset_mailbox_trust_fields() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("config.toml");

        let mut mailboxes = HashMap::new();
        mailboxes.insert(
            "catchall".to_string(),
            MailboxConfig {
                address: "*@test.com".to_string(),
                owner: "aimx-catchall".to_string(),
                hooks: vec![],
                trust: None,
                trusted_senders: None,
                allow_root_catchall: false,
            },
        );
        let config = Config {
            domain: "test.com".to_string(),
            data_dir: PathBuf::from("/tmp/x"),
            dkim_selector: "aimx".to_string(),
            trust: "verified".to_string(),
            trusted_senders: vec!["*@company.com".to_string()],
            hook_templates: Vec::new(),
            mailboxes,
            verify_host: None,
            enable_ipv6: false,
            upgrade: None,
        };
        config.save(&path).unwrap();
        let on_disk = std::fs::read_to_string(&path).unwrap();
        // Top-level defaults are written; per-mailbox None fields are absent.
        assert!(on_disk.contains("trust = \"verified\""));
        assert!(on_disk.contains("trusted_senders = [\"*@company.com\"]"));
        // The mailbox section must not re-emit the inherited values.
        let mailbox_section = on_disk
            .split("[mailboxes.catchall]")
            .nth(1)
            .expect("mailbox section present");
        assert!(
            !mailbox_section.contains("trust ="),
            "unset mailbox trust should not serialize: {mailbox_section}"
        );
        assert!(
            !mailbox_section.contains("trusted_senders"),
            "unset mailbox trusted_senders should not serialize: {mailbox_section}"
        );
    }

    #[test]
    fn mailbox_dir() {
        let config: Config = toml::from_str(sample_toml()).unwrap();
        assert_eq!(
            config.mailbox_dir("support"),
            PathBuf::from("/tmp/aimx-test/inbox/support")
        );
    }

    #[test]
    fn inbox_and_sent_dirs() {
        let config: Config = toml::from_str(sample_toml()).unwrap();
        assert_eq!(
            config.inbox_dir("support"),
            PathBuf::from("/tmp/aimx-test/inbox/support")
        );
        assert_eq!(
            config.sent_dir("support"),
            PathBuf::from("/tmp/aimx-test/sent/support")
        );
    }

    #[test]
    fn parse_verify_host() {
        let toml_str = r#"
domain = "test.com"
verify_host = "https://verify.example.com"

[mailboxes]
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(
            config.verify_host.as_deref(),
            Some("https://verify.example.com")
        );
    }

    #[test]
    fn verify_host_defaults_to_none() {
        let toml_str = "domain = \"test.com\"\n[mailboxes]\n";
        let config: Config = toml::from_str(toml_str).unwrap();
        assert!(config.verify_host.is_none());
    }

    #[test]
    fn enable_ipv6_defaults_to_false() {
        let toml_str = "domain = \"test.com\"\n[mailboxes]\n";
        let config: Config = toml::from_str(toml_str).unwrap();
        assert!(!config.enable_ipv6);
    }

    #[test]
    fn parse_enable_ipv6_true() {
        let toml_str = r#"
domain = "test.com"
enable_ipv6 = true

[mailboxes]
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert!(config.enable_ipv6);
    }

    #[test]
    fn upgrade_defaults_to_none() {
        let toml_str = "domain = \"test.com\"\n[mailboxes]\n";
        let config: Config = toml::from_str(toml_str).unwrap();
        assert!(config.upgrade.is_none());
    }

    #[test]
    fn parse_upgrade_release_manifest_url() {
        let toml_str = r#"
domain = "test.com"

[upgrade]
release_manifest_url = "file:///tmp/fixture/latest.json"

[mailboxes]
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        let upgrade = config.upgrade.expect("upgrade section parsed");
        assert_eq!(
            upgrade.release_manifest_url.as_deref(),
            Some("file:///tmp/fixture/latest.json")
        );
    }

    #[test]
    fn config_dir_defaults_to_etc_aimx_when_env_unset() {
        // Hold the guard by setting an override to a sentinel, then remove
        // the var while still holding it. The `set`/drop dance keeps the
        // serialization invariant without exposing the raw mutex.
        let tmp = TempDir::new().unwrap();
        let override_guard = ConfigDirOverride::set(tmp.path());
        // SAFETY: serialization ensured by `override_guard` holding the
        // CONFIG_DIR_GUARD; drop will restore the prior value.
        unsafe {
            std::env::remove_var(CONFIG_DIR_ENV);
        }
        assert_eq!(config_dir(), PathBuf::from("/etc/aimx"));
        assert_eq!(config_path(), PathBuf::from("/etc/aimx/config.toml"));
        assert_eq!(dkim_dir(), PathBuf::from("/etc/aimx/dkim"));
        drop(override_guard);
    }

    #[test]
    fn config_dir_env_var_overrides_default() {
        let tmp = TempDir::new().unwrap();
        let _override = ConfigDirOverride::set(tmp.path());
        assert_eq!(config_dir(), tmp.path());
        assert_eq!(config_path(), tmp.path().join("config.toml"));
        assert_eq!(dkim_dir(), tmp.path().join("dkim"));
    }

    #[test]
    fn load_resolved_reads_from_config_dir() {
        let tmp = TempDir::new().unwrap();
        let _override = ConfigDirOverride::set(tmp.path());

        let config_file = tmp.path().join("config.toml");
        std::fs::write(
            &config_file,
            "domain = \"resolved.example.com\"\n[mailboxes]\n",
        )
        .unwrap();

        let config = Config::load_resolved_ignore_warnings().unwrap();
        assert_eq!(config.domain, "resolved.example.com");
    }

    #[test]
    fn load_missing_config_gives_helpful_error() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("nonexistent.toml");
        let err = Config::load(&path).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("Config file not found"),
            "Expected helpful message, got: {msg}"
        );
        assert!(
            msg.contains(&path.display().to_string()),
            "Expected path in message, got: {msg}"
        );
        assert!(
            msg.contains("sudo aimx setup"),
            "Expected setup suggestion, got: {msg}"
        );
    }

    #[test]
    fn load_resolved_missing_config_includes_config_dir_path() {
        let tmp = TempDir::new().unwrap();
        let _override = ConfigDirOverride::set(tmp.path());
        let err = Config::load_resolved().unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("Config file not found"),
            "Expected helpful message, got: {msg}"
        );
        assert!(
            msg.contains(&tmp.path().display().to_string()),
            "Expected config dir path in message, got: {msg}"
        );
    }

    // ----- Hook template schema & validation -------------------------------

    fn write_template_config(body: &str) -> (TempDir, PathBuf) {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("config.toml");
        let mut toml = String::from("domain = \"test.com\"\n\n");
        toml.push_str(body);
        toml.push_str(
            "\n[mailboxes.catchall]\naddress = \"*@test.com\"\nowner = \"aimx-catchall\"\n",
        );
        std::fs::write(&path, toml).unwrap();
        (tmp, path)
    }

    #[test]
    fn iter_placeholders_extracts_simple_names() {
        let names: Vec<&str> = iter_placeholders("curl {url} --header 'X: {token}'").collect();
        assert_eq!(names, vec!["url", "token"]);
    }

    #[test]
    fn iter_placeholders_ignores_unclosed_braces() {
        let names: Vec<&str> = iter_placeholders("echo {incomplete and done").collect();
        assert!(names.is_empty(), "unclosed brace yields no placeholder");
    }

    #[test]
    fn iter_placeholders_ignores_non_alphanum() {
        let names: Vec<&str> = iter_placeholders("echo {BAD-NAME} {ok_name}").collect();
        assert_eq!(names, vec!["ok_name"]);
    }

    #[test]
    fn load_accepts_valid_hook_template() {
        let (_tmp, path) = write_template_config(
            r#"
[[hook_template]]
name = "invoke-claude"
description = "Pipe email into Claude Code with a prompt."
cmd = ["/usr/local/bin/claude", "-p", "{prompt}"]
params = ["prompt"]
stdin = "email"
run_as = "root"
"#,
        );
        let cfg = Config::load_ignore_warnings(&path).unwrap();
        assert_eq!(cfg.hook_templates.len(), 1);
        let tmpl = &cfg.hook_templates[0];
        assert_eq!(tmpl.name, "invoke-claude");
        assert_eq!(tmpl.params, vec!["prompt".to_string()]);
        // `run_as` is required (no default). The fixture
        // above sets it explicitly to `root` so it resolves on every
        // host.
        assert_eq!(tmpl.run_as, "root");
        assert_eq!(tmpl.timeout_secs, 60); // default
        assert!(matches!(tmpl.stdin, HookTemplateStdin::Email));
        // allowed_events defaults to both
        assert_eq!(tmpl.allowed_events.len(), 2);
    }

    #[test]
    fn load_accepts_builtin_placeholders_without_declaring_params() {
        let (_tmp, path) = write_template_config(
            r#"
[[hook_template]]
name = "log-event"
description = "Append event metadata to a log."
cmd = ["/usr/bin/logger", "event={event} mailbox={mailbox} from={from}"]
params = []
run_as = "root"
"#,
        );
        let cfg = Config::load_ignore_warnings(&path).unwrap();
        assert_eq!(cfg.hook_templates.len(), 1);
        assert!(cfg.hook_templates[0].params.is_empty());
    }

    #[test]
    fn load_rejects_duplicate_template_names() {
        let (_tmp, path) = write_template_config(
            r#"
[[hook_template]]
name = "dupe"
description = "first"
cmd = ["/bin/true"]
run_as = "root"

[[hook_template]]
name = "dupe"
description = "second"
cmd = ["/bin/true"]
run_as = "root"
"#,
        );
        let err = Config::load(&path).unwrap_err().to_string();
        assert!(
            err.contains("duplicate hook template name"),
            "expected duplicate-name rejection: {err}"
        );
    }

    #[test]
    fn load_rejects_empty_cmd() {
        let (_tmp, path) = write_template_config(
            r#"
[[hook_template]]
name = "empty"
description = "no cmd"
cmd = []
run_as = "root"
"#,
        );
        let err = Config::load(&path).unwrap_err().to_string();
        assert!(
            err.contains("empty `cmd`"),
            "expected empty-cmd rejection: {err}"
        );
    }

    #[test]
    fn load_rejects_placeholder_in_binary_path() {
        let (_tmp, path) = write_template_config(
            r#"
[[hook_template]]
name = "bad-binary"
description = "placeholder in cmd[0]"
cmd = ["/usr/local/bin/{binary}", "-p", "hi"]
params = ["binary"]
run_as = "root"
"#,
        );
        let err = Config::load(&path).unwrap_err().to_string();
        assert!(
            err.contains("placeholder in cmd[0]"),
            "expected cmd[0] rejection: {err}"
        );
    }

    #[test]
    fn load_rejects_undeclared_placeholder() {
        let (_tmp, path) = write_template_config(
            r#"
[[hook_template]]
name = "ghost-param"
description = "uses undeclared placeholder"
cmd = ["/bin/echo", "{ghost}"]
params = []
run_as = "root"
"#,
        );
        let err = Config::load(&path).unwrap_err().to_string();
        assert!(
            err.contains("undeclared placeholder") && err.contains("ghost"),
            "expected undeclared-placeholder rejection: {err}"
        );
    }

    #[test]
    fn load_rejects_declared_but_unused_param() {
        let (_tmp, path) = write_template_config(
            r#"
[[hook_template]]
name = "dead-param"
description = "param never referenced"
cmd = ["/bin/echo", "hello"]
params = ["dead"]
run_as = "root"
"#,
        );
        let err = Config::load(&path).unwrap_err().to_string();
        assert!(
            err.contains("unused param") && err.contains("dead"),
            "expected dead-param rejection: {err}"
        );
    }

    #[test]
    fn load_rejects_timeout_zero() {
        let (_tmp, path) = write_template_config(
            r#"
[[hook_template]]
name = "zero-timeout"
description = "timeout=0"
cmd = ["/bin/true"]
timeout_secs = 0
run_as = "root"
"#,
        );
        let err = Config::load(&path).unwrap_err().to_string();
        assert!(
            err.contains("invalid timeout_secs"),
            "expected timeout rejection: {err}"
        );
    }

    #[test]
    fn load_rejects_timeout_too_large() {
        let (_tmp, path) = write_template_config(
            r#"
[[hook_template]]
name = "too-long"
description = "timeout too large"
cmd = ["/bin/true"]
timeout_secs = 601
run_as = "root"
"#,
        );
        let err = Config::load(&path).unwrap_err().to_string();
        assert!(
            err.contains("invalid timeout_secs"),
            "expected timeout rejection: {err}"
        );
    }

    #[test]
    fn load_rejects_bad_run_as() {
        // The static allowlist has been retired. A `run_as` that
        // fails the `useradd`-style regex (uppercase, leading digit,
        // spaces) still hard-rejects at load time.
        let (_tmp, path) = write_template_config(
            r#"
[[hook_template]]
name = "bad-runas"
description = "weird run_as"
cmd = ["/bin/true"]
run_as = "Bad Name"
"#,
        );
        let err = Config::load(&path).unwrap_err().to_string();
        assert!(
            err.contains("invalid run_as"),
            "expected run_as rejection: {err}"
        );
    }

    #[test]
    fn load_accepts_run_as_root_when_explicit() {
        // `root` is a legal but rarely-used value — operator must opt in
        // via `config.toml`; it is NOT settable over the UDS.
        let (_tmp, path) = write_template_config(
            r#"
[[hook_template]]
name = "legacy-root"
description = "Operator-opted root exec"
cmd = ["/usr/local/bin/legacy"]
run_as = "root"
"#,
        );
        let cfg = Config::load_ignore_warnings(&path).unwrap();
        assert_eq!(cfg.hook_templates[0].run_as, "root");
    }

    #[test]
    fn load_rejects_invalid_template_name() {
        let (_tmp, path) = write_template_config(
            r#"
[[hook_template]]
name = "Invoke Claude"
description = "bad name"
cmd = ["/bin/true"]
run_as = "root"
"#,
        );
        let err = Config::load(&path).unwrap_err().to_string();
        assert!(
            err.contains("invalid hook template name"),
            "expected name rejection: {err}"
        );
    }

    #[test]
    fn load_rejects_empty_allowed_events() {
        let (_tmp, path) = write_template_config(
            r#"
[[hook_template]]
name = "nope"
description = "no events"
cmd = ["/bin/true"]
allowed_events = []
run_as = "root"
"#,
        );
        let err = Config::load(&path).unwrap_err().to_string();
        assert!(
            err.contains("empty allowed_events"),
            "expected allowed_events rejection: {err}"
        );
    }

    #[test]
    fn load_rejects_param_name_with_uppercase_letters() {
        // Params must match the placeholder charset `[a-z0-9_]+`. An
        // uppercase param name like `FOO` can never be referenced by the
        // placeholder extractor, so we reject it at load time with a
        // precise error rather than letting it surface as "unused param".
        let (_tmp, path) = write_template_config(
            r#"
[[hook_template]]
name = "bad-param"
description = "uppercase param"
cmd = ["/bin/echo", "{FOO}"]
params = ["FOO"]
run_as = "root"
"#,
        );
        let err = Config::load(&path).unwrap_err().to_string();
        assert!(
            err.contains("invalid param name"),
            "expected invalid-param-name rejection: {err}"
        );
        assert!(
            err.contains("FOO"),
            "error should name the offending param: {err}"
        );
    }

    #[test]
    fn load_rejects_param_name_with_dash() {
        let (_tmp, path) = write_template_config(
            r#"
[[hook_template]]
name = "bad-param"
description = "dashed param"
cmd = ["/bin/echo", "placeholder"]
params = ["foo-bar"]
run_as = "root"
"#,
        );
        let err = Config::load(&path).unwrap_err().to_string();
        assert!(
            err.contains("invalid param name"),
            "expected invalid-param-name rejection: {err}"
        );
    }

    #[test]
    fn load_rejects_param_name_colliding_with_builtin() {
        let (_tmp, path) = write_template_config(
            r#"
[[hook_template]]
name = "clash"
description = "param collides with built-in"
cmd = ["/bin/echo", "{event}"]
params = ["event"]
run_as = "root"
"#,
        );
        let err = Config::load(&path).unwrap_err().to_string();
        assert!(
            err.contains("built-in placeholder"),
            "expected built-in collision rejection: {err}"
        );
    }

    #[test]
    fn hook_templates_fixture_round_trips() {
        let fixture = std::fs::read_to_string("tests/fixtures/hook_templates_valid.toml")
            .expect("fixture present");
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("config.toml");
        std::fs::write(&path, &fixture).unwrap();
        let cfg = Config::load_ignore_warnings(&path).unwrap();
        assert_eq!(cfg.hook_templates.len(), 2);

        // Save and re-load — serialization must survive a round trip.
        let round_trip = tmp.path().join("round_trip.toml");
        cfg.save(&round_trip).unwrap();
        let reloaded = Config::load_ignore_warnings(&round_trip).unwrap();
        assert_eq!(cfg, reloaded);
    }

    // ----- Hook template/cmd mutual exclusion ------------------------------

    #[test]
    fn load_rejects_hook_with_both_cmd_and_template() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("config.toml");
        std::fs::write(
            &path,
            r#"
domain = "test.com"

[[hook_template]]
name = "echo"
description = "Echo template"
cmd = ["/bin/echo", "hi"]
run_as = "root"

[mailboxes.catchall]
address = "*@test.com"
owner = "aimx-catchall"

[[mailboxes.catchall.hooks]]
name = "bad"
event = "on_receive"
template = "echo"
cmd = "echo also-raw"
"#,
        )
        .unwrap();
        let err = Config::load(&path).unwrap_err().to_string();
        assert!(
            err.contains("mutually exclusive"),
            "expected mutual-exclusion rejection: {err}"
        );
    }

    #[test]
    fn load_rejects_hook_with_unknown_template_reference() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("config.toml");
        std::fs::write(
            &path,
            r#"
domain = "test.com"

[mailboxes.catchall]
address = "*@test.com"
owner = "aimx-catchall"

[[mailboxes.catchall.hooks]]
name = "ghost"
event = "on_receive"
template = "does-not-exist"
"#,
        )
        .unwrap();
        let err = Config::load(&path).unwrap_err().to_string();
        assert!(
            err.contains("unknown template"),
            "expected unknown-template rejection: {err}"
        );
    }

    #[test]
    fn load_rejects_hook_with_params_but_no_template() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("config.toml");
        std::fs::write(
            &path,
            r#"
domain = "test.com"

[mailboxes.catchall]
address = "*@test.com"
owner = "aimx-catchall"

[[mailboxes.catchall.hooks]]
name = "raw-with-params"
event = "on_receive"
cmd = "echo hi"

[mailboxes.catchall.hooks.params]
prompt = "oops"
"#,
        )
        .unwrap();
        let err = Config::load(&path).unwrap_err().to_string();
        assert!(
            err.contains("params") && err.contains("template"),
            "expected params-without-template rejection: {err}"
        );
    }

    #[test]
    fn load_accepts_template_hook_with_known_template() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("config.toml");
        std::fs::write(
            &path,
            r#"
domain = "test.com"

[[hook_template]]
name = "invoke-claude"
description = "Claude"
cmd = ["/usr/local/bin/claude", "-p", "{prompt}"]
params = ["prompt"]
run_as = "root"

[mailboxes.accounts]
address = "accounts@test.com"
owner = "ops"

[[mailboxes.accounts.hooks]]
name = "ar"
event = "on_receive"
origin = "mcp"
template = "invoke-claude"

[mailboxes.accounts.hooks.params]
prompt = "Draft a reply"
"#,
        )
        .unwrap();
        let cfg = Config::load_ignore_warnings(&path).unwrap();
        let hooks = &cfg.mailboxes["accounts"].hooks;
        assert_eq!(hooks.len(), 1);
        assert_eq!(hooks[0].origin, crate::hook::HookOrigin::Mcp);
        assert_eq!(hooks[0].template.as_deref(), Some("invoke-claude"));
        assert_eq!(
            hooks[0].params.get("prompt").map(|s| s.as_str()),
            Some("Draft a reply")
        );
        assert!(hooks[0].cmd.is_empty());
    }

    #[test]
    fn load_rejects_mcp_origin_raw_cmd_with_dangerously_flag() {
        // PRD §6.8: MCP-origin hooks may not set
        // `dangerously_support_untrusted`, regardless of whether they are
        // template-bound or raw-cmd. The guard in `validate_hooks` drops
        // the `is_template_bound()` qualifier so it catches this shape
        // even on shapes the UDS body-schema tightening also rejects.
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("config.toml");
        std::fs::write(
            &path,
            r#"
domain = "test.com"

[mailboxes.accounts]
address = "accounts@test.com"
owner = "ops"

[[mailboxes.accounts.hooks]]
name = "mcp_raw_danger"
event = "on_receive"
origin = "mcp"
cmd = "echo hi"
dangerously_support_untrusted = true
"#,
        )
        .unwrap();
        let err = Config::load(&path).unwrap_err().to_string();
        assert!(
            err.contains("MCP-origin") && err.contains("operator-only"),
            "expected mcp-origin dangerously rejection: {err}"
        );
    }

    #[test]
    fn validate_single_hook_rejects_mcp_origin_raw_cmd_with_dangerously_flag() {
        // The UDS `HOOK-CREATE` path calls `validate_single_hook` on the
        // submitted stanza before it ever reaches `Config`. The MCP-origin
        // guard must fire there too, for the same reason as above.
        let hook = Hook {
            name: Some("raw_danger".into()),
            event: HookEvent::OnReceive,
            r#type: "cmd".into(),
            cmd: "echo hi".into(),
            dangerously_support_untrusted: true,
            origin: HookOrigin::Mcp,
            template: None,
            params: std::collections::BTreeMap::new(),
            run_as: None,
        };
        let err = validate_single_hook(&hook).unwrap_err();
        assert!(
            err.contains("MCP-origin") && err.contains("operator-only"),
            "expected mcp-origin dangerously rejection: {err}"
        );
    }

    // ----- MailboxConfig.owner ------------------------------------------

    #[test]
    fn load_rejects_non_catchall_owner_root() {
        // `owner = "root"` on a non-catchall mailbox is rejected with
        // an actionable suggestion pointing operators at either a
        // regular user or the `allow_root_catchall` escape hatch on a
        // catchall mailbox.
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("config.toml");
        std::fs::write(
            &path,
            r#"
domain = "test.com"

[mailboxes.support]
address = "support@test.com"
owner = "root"
"#,
        )
        .unwrap();
        let err = Config::load(&path).unwrap_err().to_string();
        assert!(
            err.contains("cannot be owned by root"),
            "non-catchall root owner must reject: {err}"
        );
        assert!(
            err.contains("allow_root_catchall"),
            "error must mention the escape-hatch flag: {err}"
        );
    }

    #[test]
    fn load_rejects_mailbox_missing_owner_field() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("config.toml");
        std::fs::write(
            &path,
            r#"
domain = "test.com"

[mailboxes.support]
address = "support@test.com"
"#,
        )
        .unwrap();
        let err = Config::load(&path).unwrap_err().to_string();
        assert!(
            err.contains("owner"),
            "missing owner must produce an actionable error: {err}"
        );
    }

    #[test]
    fn load_rejects_mailbox_empty_owner_string() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("config.toml");
        std::fs::write(
            &path,
            r#"
domain = "test.com"

[mailboxes.support]
address = "support@test.com"
owner = "   "
"#,
        )
        .unwrap();
        let err = Config::load(&path).unwrap_err().to_string();
        assert!(err.contains("owner"), "empty owner must reject: {err}");
    }

    #[test]
    fn load_mailbox_orphan_owner_warns_not_errors() {
        use crate::user_resolver::{ResolvedUser, set_test_resolver};
        fn only_catchall(name: &str) -> Option<ResolvedUser> {
            // aimx-catchall resolves; everyone else is an orphan.
            if name == "aimx-catchall" {
                Some(ResolvedUser {
                    name: name.into(),
                    uid: 999,
                    gid: 999,
                })
            } else {
                None
            }
        }
        let _guard = set_test_resolver(only_catchall);
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("config.toml");
        std::fs::write(
            &path,
            r#"
domain = "test.com"

[mailboxes.catchall]
address = "*@test.com"
owner = "aimx-catchall"

[mailboxes.orphanbox]
address = "orphan@test.com"
owner = "deleted-user"
"#,
        )
        .unwrap();
        let (cfg, warnings) = Config::load(&path).unwrap();
        assert_eq!(cfg.mailboxes.len(), 2);
        let orphan_warnings: Vec<_> = warnings
            .iter()
            .filter(|w| matches!(w, LoadWarning::OrphanMailboxOwner { .. }))
            .collect();
        assert_eq!(orphan_warnings.len(), 1, "{warnings:?}");
        let resolved = Config::resolved_from_warnings(&warnings);
        assert!(!resolved.is_mailbox_active("orphanbox"));
        assert!(resolved.is_mailbox_active("catchall"));
    }

    #[test]
    fn load_mailbox_owner_roundtrip_preserved() {
        use crate::user_resolver::{ResolvedUser, set_test_resolver};
        fn resolver(name: &str) -> Option<ResolvedUser> {
            match name {
                "aimx-catchall" => Some(ResolvedUser {
                    name: name.into(),
                    uid: 999,
                    gid: 999,
                }),
                "alice" => Some(ResolvedUser {
                    name: name.into(),
                    uid: 1001,
                    gid: 1001,
                }),
                _ => None,
            }
        }
        let _guard = set_test_resolver(resolver);
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("config.toml");
        std::fs::write(
            &path,
            r#"
domain = "test.com"

[mailboxes.catchall]
address = "*@test.com"
owner = "aimx-catchall"

[mailboxes.support]
address = "support@test.com"
owner = "alice"
"#,
        )
        .unwrap();
        let (cfg, _warnings) = Config::load(&path).unwrap();
        let path2 = tmp.path().join("round.toml");
        cfg.save(&path2).unwrap();
        let (cfg2, _) = Config::load(&path2).unwrap();
        assert_eq!(cfg, cfg2);
        assert_eq!(cfg2.mailboxes["support"].owner, "alice");
        assert_eq!(cfg2.mailboxes["catchall"].owner, "aimx-catchall");
    }

    // ----- owner="root" rules + allow_root_catchall ---------------------

    #[test]
    fn load_accepts_catchall_with_default_aimx_catchall_owner() {
        // Catchall with the default `owner = "aimx-catchall"`
        // still loads clean, without the allow_root_catchall flag.
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("config.toml");
        std::fs::write(
            &path,
            r#"
domain = "test.com"

[mailboxes.catchall]
address = "*@test.com"
owner = "aimx-catchall"
"#,
        )
        .unwrap();
        let (cfg, warnings) = Config::load(&path).unwrap();
        assert_eq!(cfg.mailboxes["catchall"].owner, "aimx-catchall");
        assert!(
            warnings.is_empty(),
            "default catchall must not warn: {warnings:?}"
        );
    }

    #[test]
    fn load_rejects_root_catchall_without_allow_flag() {
        // Catchall with owner="root" but allow_root_catchall=false
        // (or omitted) rejects at load with a pointer to the opt-in flag.
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("config.toml");
        std::fs::write(
            &path,
            r#"
domain = "test.com"

[mailboxes.catchall]
address = "*@test.com"
owner = "root"
"#,
        )
        .unwrap();
        let err = Config::load(&path).unwrap_err().to_string();
        assert!(
            err.contains("catchall") && err.contains("allow_root_catchall"),
            "catchall root without flag must hint at the escape hatch: {err}"
        );
    }

    #[test]
    fn load_accepts_root_catchall_with_allow_flag_and_warns() {
        // Catchall with owner="root" + allow_root_catchall=true
        // loads successfully and surfaces a RootCatchallAccepted warning
        // so the elevation is audit-logged.
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("config.toml");
        std::fs::write(
            &path,
            r#"
domain = "test.com"

[mailboxes.catchall]
address = "*@test.com"
owner = "root"
allow_root_catchall = true
"#,
        )
        .unwrap();
        let (cfg, warnings) = Config::load(&path).unwrap();
        assert_eq!(cfg.mailboxes["catchall"].owner, "root");
        assert!(cfg.mailboxes["catchall"].allow_root_catchall);
        let has_warning = warnings.iter().any(
            |w| matches!(w, LoadWarning::RootCatchallAccepted { mailbox } if mailbox == "catchall"),
        );
        assert!(
            has_warning,
            "root-catchall escape hatch must warn: {warnings:?}"
        );
    }

    #[test]
    fn load_rejects_allow_root_catchall_on_non_catchall_mailbox() {
        // allow_root_catchall on any non-catchall mailbox is a
        // config-structure error regardless of owner value — the flag
        // only has meaning for the wildcard mailbox.
        use crate::user_resolver::{ResolvedUser, set_test_resolver};
        fn resolver(name: &str) -> Option<ResolvedUser> {
            if name == "alice" {
                Some(ResolvedUser {
                    name: name.into(),
                    uid: 1001,
                    gid: 1001,
                })
            } else {
                None
            }
        }
        let _guard = set_test_resolver(resolver);
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("config.toml");
        std::fs::write(
            &path,
            r#"
domain = "test.com"

[mailboxes.alice]
address = "alice@test.com"
owner = "alice"
allow_root_catchall = true
"#,
        )
        .unwrap();
        let err = Config::load(&path).unwrap_err().to_string();
        assert!(
            err.contains("allow_root_catchall") && err.contains("not a catchall"),
            "allow_root_catchall on non-catchall must reject: {err}"
        );
    }

    #[test]
    fn allow_root_catchall_default_false_omitted_from_serialized_toml() {
        // Regression guard: `#[serde(skip_serializing_if)]` on the bool
        // keeps standard catchall-owned-by-aimx-catchall configs minimal
        // in the serialized form, so upgrades don't rewrite live configs
        // with a surprising `allow_root_catchall = false` line.
        let cfg = Config {
            domain: "test.com".to_string(),
            data_dir: PathBuf::from("/var/lib/aimx"),
            dkim_selector: "aimx".to_string(),
            trust: "none".to_string(),
            trusted_senders: vec![],
            hook_templates: Vec::new(),
            mailboxes: {
                let mut m = HashMap::new();
                m.insert(
                    "catchall".to_string(),
                    MailboxConfig {
                        address: "*@test.com".to_string(),
                        owner: "aimx-catchall".to_string(),
                        hooks: vec![],
                        trust: None,
                        trusted_senders: None,
                        allow_root_catchall: false,
                    },
                );
                m
            },
            verify_host: None,
            enable_ipv6: false,
            upgrade: None,
        };
        let toml_str = toml::to_string_pretty(&cfg).unwrap();
        assert!(
            !toml_str.contains("allow_root_catchall"),
            "default false must not serialize: {toml_str}"
        );
    }

    // ----- validate_run_as ----------------------------------------------

    #[test]
    fn validate_run_as_reserved_root() {
        assert!(matches!(
            validate_run_as("root").unwrap(),
            RunAsKind::Reserved("root")
        ));
    }

    #[test]
    fn validate_run_as_reserved_catchall() {
        assert!(matches!(
            validate_run_as("aimx-catchall").unwrap(),
            RunAsKind::Reserved("aimx-catchall")
        ));
    }

    #[test]
    fn validate_run_as_rejects_uppercase_username() {
        assert_eq!(
            validate_run_as("Alice"),
            Err(ConfigError::InvalidUsername("Alice".into()))
        );
    }

    #[test]
    fn validate_run_as_rejects_unicode_username() {
        assert_eq!(
            validate_run_as("alicé"),
            Err(ConfigError::InvalidUsername("alicé".into()))
        );
    }

    #[test]
    fn validate_run_as_rejects_leading_digit() {
        assert_eq!(
            validate_run_as("9bad"),
            Err(ConfigError::InvalidUsername("9bad".into()))
        );
    }

    #[test]
    fn validate_run_as_resolves_via_getpwnam_or_returns_orphan() {
        // Happy path — root is the only universal username on a real
        // Linux host. Under test override, we can exercise both the
        // resolve-success and resolve-miss branches deterministically.
        use crate::user_resolver::{ResolvedUser, set_test_resolver};
        fn resolver(name: &str) -> Option<ResolvedUser> {
            if name == "alice" {
                Some(ResolvedUser {
                    name: "alice".into(),
                    uid: 1001,
                    gid: 1001,
                })
            } else if name == "root" {
                Some(ResolvedUser {
                    name: "root".into(),
                    uid: 0,
                    gid: 0,
                })
            } else {
                None
            }
        }
        let _guard = set_test_resolver(resolver);
        match validate_run_as("alice").unwrap() {
            RunAsKind::User(u) => assert_eq!(u.uid, 1001),
            other => panic!("expected User, got {other:?}"),
        }
        assert_eq!(
            validate_run_as("deleted"),
            Err(ConfigError::OrphanUser("deleted".into()))
        );
    }

    // ----- check_hook_owner_invariant -----------------------------------

    fn mailbox_for_invariant(address: &str, owner: &str) -> MailboxConfig {
        MailboxConfig {
            address: address.into(),
            owner: owner.into(),
            hooks: vec![],
            trust: None,
            trusted_senders: None,
            allow_root_catchall: false,
        }
    }

    fn hook_for_invariant(run_as: Option<&str>) -> Hook {
        Hook {
            name: Some("h1".into()),
            event: HookEvent::OnReceive,
            r#type: "cmd".into(),
            cmd: "echo hi".into(),
            dangerously_support_untrusted: false,
            origin: HookOrigin::Operator,
            template: None,
            params: std::collections::BTreeMap::new(),
            run_as: run_as.map(|s| s.to_string()),
        }
    }

    fn invariant_config(domain: &str) -> Config {
        Config {
            domain: domain.into(),
            data_dir: PathBuf::from("/tmp/x"),
            dkim_selector: "aimx".into(),
            trust: "none".into(),
            trusted_senders: vec![],
            hook_templates: Vec::new(),
            mailboxes: HashMap::new(),
            verify_host: None,
            enable_ipv6: false,
            upgrade: None,
        }
    }

    #[test]
    fn invariant_matching_owner_passes() {
        let cfg = invariant_config("test.com");
        let mb = mailbox_for_invariant("alice@test.com", "alice");
        let h = hook_for_invariant(Some("alice"));
        assert!(check_hook_owner_invariant(&cfg, "alice", &mb, &h).is_ok());
    }

    #[test]
    fn invariant_mismatched_owner_fails_with_clear_message() {
        let cfg = invariant_config("test.com");
        let mb = mailbox_for_invariant("alice@test.com", "alice");
        let h = hook_for_invariant(Some("bob"));
        let err = check_hook_owner_invariant(&cfg, "alice", &mb, &h).unwrap_err();
        assert!(
            matches!(
                err,
                HookOwnerInvariantError::OwnerMismatch { ref run_as, ref owner, .. }
                    if run_as == "bob" && owner == "alice"
            ),
            "expected OwnerMismatch with run_as=bob owner=alice, got {err:?}"
        );
        let rendered = err.to_string();
        assert!(
            rendered.contains("run_as='bob'") && rendered.contains("owned by 'alice'"),
            "message must name both run_as and owner: {rendered}"
        );
        assert!(rendered.contains("set run_as="), "{rendered}");
    }

    #[test]
    fn invariant_root_always_passes_even_on_non_catchall() {
        let cfg = invariant_config("test.com");
        let mb = mailbox_for_invariant("alice@test.com", "alice");
        let h = hook_for_invariant(Some("root"));
        assert!(check_hook_owner_invariant(&cfg, "alice", &mb, &h).is_ok());
    }

    #[test]
    fn invariant_root_always_passes_on_catchall() {
        let cfg = invariant_config("test.com");
        let mb = mailbox_for_invariant("*@test.com", "aimx-catchall");
        let h = hook_for_invariant(Some("root"));
        assert!(check_hook_owner_invariant(&cfg, "catchall", &mb, &h).is_ok());
    }

    #[test]
    fn invariant_catchall_accepts_aimx_catchall_run_as() {
        let cfg = invariant_config("test.com");
        let mb = mailbox_for_invariant("*@test.com", "aimx-catchall");
        let h = hook_for_invariant(Some("aimx-catchall"));
        assert!(check_hook_owner_invariant(&cfg, "catchall", &mb, &h).is_ok());
    }

    #[test]
    fn invariant_catchall_rejects_non_catchall_run_as() {
        let cfg = invariant_config("test.com");
        let mb = mailbox_for_invariant("*@test.com", "aimx-catchall");
        let h = hook_for_invariant(Some("alice"));
        let err = check_hook_owner_invariant(&cfg, "catchall", &mb, &h).unwrap_err();
        assert!(
            matches!(
                err,
                HookOwnerInvariantError::CatchallRunAsMismatch { ref run_as, .. }
                    if run_as == "alice"
            ),
            "expected CatchallRunAsMismatch, got {err:?}"
        );
        let rendered = err.to_string();
        assert!(
            rendered.contains("aimx-catchall") && rendered.contains("catchall"),
            "catchall error must name the expected run_as: {rendered}"
        );
    }

    #[test]
    fn invariant_non_catchall_rejects_aimx_catchall_run_as() {
        let cfg = invariant_config("test.com");
        let mb = mailbox_for_invariant("alice@test.com", "alice");
        let h = hook_for_invariant(Some("aimx-catchall"));
        let err = check_hook_owner_invariant(&cfg, "alice", &mb, &h).unwrap_err();
        assert!(
            matches!(
                err,
                HookOwnerInvariantError::OwnerMismatch { ref run_as, .. }
                    if run_as == "aimx-catchall"
            ),
            "expected OwnerMismatch with run_as=aimx-catchall, got {err:?}"
        );
        let rendered = err.to_string();
        assert!(rendered.contains("run_as='aimx-catchall'"), "{rendered}");
    }

    #[test]
    fn load_enforces_invariant_on_mailbox_hook() {
        // Hook run_as resolves via getpwnam but does not match the owner
        // and is not root → hard fail. We inject a resolver so the
        // mismatched user exists; this isolates the invariant check
        // from the orphan-tolerance path (see
        // `load_orphan_mailbox_owner_with_invariant_mismatch_becomes_warning`
        // for the orphan-skip coverage).
        use crate::user_resolver::{ResolvedUser, set_test_resolver};
        fn resolver(name: &str) -> Option<ResolvedUser> {
            match name {
                "root" => Some(ResolvedUser {
                    name: "root".into(),
                    uid: 0,
                    gid: 0,
                }),
                "aimx-catchall" => Some(ResolvedUser {
                    name: "aimx-catchall".into(),
                    uid: 999,
                    gid: 999,
                }),
                "ops" => Some(ResolvedUser {
                    name: "ops".into(),
                    uid: 1000,
                    gid: 1000,
                }),
                "someoneelse" => Some(ResolvedUser {
                    name: "someoneelse".into(),
                    uid: 1234,
                    gid: 1234,
                }),
                _ => None,
            }
        }
        let _guard = set_test_resolver(resolver);
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("config.toml");
        std::fs::write(
            &path,
            r#"
domain = "test.com"

[mailboxes.alice]
address = "alice@test.com"
owner = "ops"

[[mailboxes.alice.hooks]]
name = "mismatch"
event = "on_receive"
cmd = "echo hi"
run_as = "someoneelse"
"#,
        )
        .unwrap();
        let err = Config::load(&path).unwrap_err().to_string();
        assert!(
            err.contains("run_as='someoneelse'"),
            "load must bubble the invariant error: {err}"
        );
    }

    // ----- Orphan tolerance ----------------------------------------------

    #[test]
    fn load_valid_config_produces_empty_warning_vec() {
        use crate::user_resolver::{ResolvedUser, set_test_resolver};
        fn resolver(name: &str) -> Option<ResolvedUser> {
            match name {
                "aimx-catchall" => Some(ResolvedUser {
                    name: name.into(),
                    uid: 999,
                    gid: 999,
                }),
                "ops" => Some(ResolvedUser {
                    name: name.into(),
                    uid: 1000,
                    gid: 1000,
                }),
                _ => None,
            }
        }
        let _guard = set_test_resolver(resolver);
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("config.toml");
        std::fs::write(
            &path,
            r#"
domain = "test.com"

[mailboxes.catchall]
address = "*@test.com"
owner = "aimx-catchall"

[mailboxes.support]
address = "support@test.com"
owner = "ops"
"#,
        )
        .unwrap();
        let (_cfg, warnings) = Config::load(&path).unwrap();
        assert!(
            warnings.is_empty(),
            "valid config should warn nothing: {warnings:?}"
        );
    }

    #[test]
    fn load_template_orphan_run_as_surfaces_as_warning() {
        use crate::user_resolver::{ResolvedUser, set_test_resolver};
        fn only_root(name: &str) -> Option<ResolvedUser> {
            if name == "root" {
                Some(ResolvedUser {
                    name: "root".into(),
                    uid: 0,
                    gid: 0,
                })
            } else if name == "aimx-catchall" {
                Some(ResolvedUser {
                    name: name.into(),
                    uid: 999,
                    gid: 999,
                })
            } else {
                None
            }
        }
        let _guard = set_test_resolver(only_root);
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("config.toml");
        std::fs::write(
            &path,
            r#"
domain = "test.com"

[[hook_template]]
name = "orphan-tmpl"
description = "points at a non-existent user"
cmd = ["/bin/true"]
run_as = "ghost"

[mailboxes.catchall]
address = "*@test.com"
owner = "aimx-catchall"
"#,
        )
        .unwrap();
        let (cfg, warnings) = Config::load(&path).unwrap();
        assert_eq!(cfg.hook_templates.len(), 1);
        let orphan_warnings: Vec<_> = warnings
            .iter()
            .filter_map(|w| match w {
                LoadWarning::OrphanTemplateRunAs { template, run_as } => {
                    Some((template.clone(), run_as.clone()))
                }
                _ => None,
            })
            .collect();
        assert_eq!(
            orphan_warnings,
            vec![("orphan-tmpl".to_string(), "ghost".to_string())]
        );
        let resolved = Config::resolved_from_warnings(&warnings);
        assert!(
            !resolved.is_template_active("orphan-tmpl"),
            "orphan templates must flag as inactive"
        );
    }

    #[test]
    fn load_mixed_valid_and_orphan_only_orphans_surface() {
        use crate::user_resolver::{ResolvedUser, set_test_resolver};
        fn only_alice_and_reserved(name: &str) -> Option<ResolvedUser> {
            match name {
                "alice" => Some(ResolvedUser {
                    name: "alice".into(),
                    uid: 1001,
                    gid: 1001,
                }),
                "root" => Some(ResolvedUser {
                    name: "root".into(),
                    uid: 0,
                    gid: 0,
                }),
                "aimx-catchall" => Some(ResolvedUser {
                    name: "aimx-catchall".into(),
                    uid: 999,
                    gid: 999,
                }),
                _ => None,
            }
        }
        let _guard = set_test_resolver(only_alice_and_reserved);
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("config.toml");
        std::fs::write(
            &path,
            r#"
domain = "test.com"

[mailboxes.catchall]
address = "*@test.com"
owner = "aimx-catchall"

[mailboxes.alice]
address = "alice@test.com"
owner = "alice"

[mailboxes.ghostbox]
address = "ghost@test.com"
owner = "ghost-user"
"#,
        )
        .unwrap();
        let (cfg, warnings) = Config::load(&path).unwrap();
        assert_eq!(cfg.mailboxes.len(), 3);
        assert_eq!(warnings.len(), 1);
        assert!(matches!(
            &warnings[0],
            LoadWarning::OrphanMailboxOwner { mailbox, owner }
                if mailbox == "ghostbox" && owner == "ghost-user"
        ));
    }

    // ----- Review-cycle follow-ups --------------------------------------

    #[test]
    fn invariant_error_drops_redundant_root_when_owner_is_root() {
        // When the mailbox owner is `root`, the fallback hint
        // (`or run_as='root'`) collides with the primary suggestion.
        // The message should render just `set run_as='root'` — no
        // `or run_as='root'` duplication.
        let cfg = invariant_config("test.com");
        let mb = mailbox_for_invariant("alice@test.com", "root");
        let h = hook_for_invariant(Some("nobody"));
        let err = check_hook_owner_invariant(&cfg, "alice", &mb, &h)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("set run_as='root'"),
            "message must suggest run_as=root: {err}"
        );
        assert!(
            !err.contains("or run_as='root'"),
            "message must not duplicate run_as='root' in the or-clause: {err}"
        );
    }

    #[test]
    fn invariant_error_retains_or_clause_for_non_root_owner() {
        // Sanity: when the owner is not `root`, the `or run_as='root'`
        // clause must still be present.
        let cfg = invariant_config("test.com");
        let mb = mailbox_for_invariant("alice@test.com", "alice");
        let h = hook_for_invariant(Some("nobody"));
        let err = check_hook_owner_invariant(&cfg, "alice", &mb, &h)
            .unwrap_err()
            .to_string();
        assert!(err.contains("set run_as='alice'"), "{err}");
        assert!(err.contains("or run_as='root'"), "{err}");
    }

    #[test]
    fn load_regex_invalid_mailbox_owner_hard_fails() {
        // PRD §6.1 — regex-invalid usernames are hard-rejected so the
        // behavior matches `validate_hook_run_as`. A stray "Bad Name"
        // typed into the owner field must not silently degrade to a
        // warning (the earlier behavior was asymmetric with the hook
        // run_as side and confused operators).
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("config.toml");
        std::fs::write(
            &path,
            r#"
domain = "test.com"

[mailboxes.weird]
address = "weird@test.com"
owner = "Bad Name"
"#,
        )
        .unwrap();
        let err = Config::load(&path).unwrap_err().to_string();
        assert!(
            err.contains("invalid owner") && err.contains("Bad Name"),
            "load must hard-fail on regex-invalid owner: {err}"
        );
    }

    #[test]
    fn load_orphan_template_with_invariant_mismatch_becomes_warning() {
        // PRD §6.1 orphan tolerance interplay with PRD §6.3 invariant:
        // a template whose `run_as` is orphan-flagged must not hard-
        // fail the load even when attached to a mailbox with a valid,
        // non-matching owner. The hook is unfireable anyway; the
        // daemon must stay up. Regression coverage for the reviewer-
        // reproduced failure mode.
        use crate::user_resolver::{ResolvedUser, set_test_resolver};
        fn only_reserved(name: &str) -> Option<ResolvedUser> {
            match name {
                "root" => Some(ResolvedUser {
                    name: "root".into(),
                    uid: 0,
                    gid: 0,
                }),
                "aimx-catchall" => Some(ResolvedUser {
                    name: "aimx-catchall".into(),
                    uid: 999,
                    gid: 999,
                }),
                _ => None,
            }
        }
        let _guard = set_test_resolver(only_reserved);
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("config.toml");
        std::fs::write(
            &path,
            r#"
domain = "test.com"

[[hook_template]]
name = "t1"
description = "bound to a deleted user"
cmd = ["/bin/true"]
run_as = "totally-missing-user"

[mailboxes.alice]
address = "alice@test.com"
owner = "ops"

[[mailboxes.alice.hooks]]
name = "uses-t1"
event = "on_receive"
template = "t1"
"#,
        )
        .unwrap();
        let (_cfg, warnings) = Config::load(&path).expect(
            "orphan template + invariant-mismatch must load with warnings, not error (PRD §6.1)",
        );

        let orphan_template: Vec<_> = warnings
            .iter()
            .filter(|w| matches!(w, LoadWarning::OrphanTemplateRunAs { template, .. } if template == "t1"))
            .collect();
        assert_eq!(orphan_template.len(), 1, "{warnings:?}");

        let skipped: Vec<_> = warnings
            .iter()
            .filter(|w| {
                matches!(
                    w,
                    LoadWarning::HookInvariantSkippedDueToOrphan { mailbox, hook_name, .. }
                        if mailbox == "alice" && hook_name == "uses-t1"
                )
            })
            .collect();
        assert_eq!(
            skipped.len(),
            1,
            "invariant-skip warning must fire for the orphan-template hook: {warnings:?}"
        );
    }

    #[test]
    fn load_orphan_mailbox_owner_with_invariant_mismatch_becomes_warning() {
        // Mirror of the orphan-template case: a mailbox whose `owner`
        // is orphan-flagged plus a hook that would otherwise violate
        // the invariant must soft-pass. The daemon stays up and the
        // mailbox is inactive; the hook never fires.
        use crate::user_resolver::{ResolvedUser, set_test_resolver};
        fn only_reserved(name: &str) -> Option<ResolvedUser> {
            match name {
                "root" => Some(ResolvedUser {
                    name: "root".into(),
                    uid: 0,
                    gid: 0,
                }),
                "aimx-catchall" => Some(ResolvedUser {
                    name: "aimx-catchall".into(),
                    uid: 999,
                    gid: 999,
                }),
                _ => None,
            }
        }
        let _guard = set_test_resolver(only_reserved);
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("config.toml");
        std::fs::write(
            &path,
            r#"
domain = "test.com"

[mailboxes.ghostbox]
address = "ghost@test.com"
owner = "deleted-user"

[[mailboxes.ghostbox.hooks]]
name = "stale"
event = "on_receive"
cmd = "echo hi"
run_as = "root"
"#,
        )
        .unwrap();
        // Note: here the hook run_as='root' IS valid against any owner
        // (even an orphan one), so no invariant skip fires — the
        // invariant would pass normally. Add a second fixture where
        // the mismatch is real to exercise the skip branch.
        let (_cfg, warnings) = Config::load(&path).expect("must load");
        assert!(
            warnings.iter().any(|w| matches!(
                w,
                LoadWarning::OrphanMailboxOwner { mailbox, .. } if mailbox == "ghostbox"
            )),
            "orphan mailbox owner must surface: {warnings:?}"
        );

        // Second fixture: orphan owner + non-root mismatched run_as.
        let path2 = tmp.path().join("config2.toml");
        std::fs::write(
            &path2,
            r#"
domain = "test.com"

[mailboxes.ghostbox]
address = "ghost@test.com"
owner = "deleted-user"

[[mailboxes.ghostbox.hooks]]
name = "stale"
event = "on_receive"
cmd = "echo hi"
run_as = "aimx-catchall"
"#,
        )
        .unwrap();
        let (_cfg2, warnings2) = Config::load(&path2).expect(
            "orphan owner + invariant-mismatch must load with warnings, not error (PRD §6.1)",
        );
        assert!(
            warnings2.iter().any(|w| matches!(
                w,
                LoadWarning::HookInvariantSkippedDueToOrphan { mailbox, hook_name, .. }
                    if mailbox == "ghostbox" && hook_name == "stale"
            )),
            "invariant-skip warning must fire when mailbox owner is orphan: {warnings2:?}"
        );
    }

    #[test]
    fn load_fresh_hook_create_still_hard_rejects_invariant_mismatch() {
        // Regression guard: the orphan-skip only triggers on the load
        // path. The strict `OrphanSkipContext` used by UDS `HOOK-CREATE`
        // and `aimx hooks create` must still hard-reject an invariant
        // mismatch even when an orphan template is present in the same
        // config. This test invokes `validate_hooks` directly with the
        // strict context to pin the semantics.
        use crate::user_resolver::{ResolvedUser, set_test_resolver};
        fn only_reserved(name: &str) -> Option<ResolvedUser> {
            match name {
                "root" => Some(ResolvedUser {
                    name: "root".into(),
                    uid: 0,
                    gid: 0,
                }),
                "aimx-catchall" => Some(ResolvedUser {
                    name: "aimx-catchall".into(),
                    uid: 999,
                    gid: 999,
                }),
                _ => None,
            }
        }
        let _guard = set_test_resolver(only_reserved);

        let mut cfg = invariant_config("test.com");
        cfg.mailboxes.insert(
            "alice".into(),
            mailbox_for_invariant("alice@test.com", "root"),
        );
        cfg.mailboxes
            .get_mut("alice")
            .unwrap()
            .hooks
            .push(hook_for_invariant(Some("totally-missing")));

        let err = validate_hooks(&cfg, &OrphanSkipContext::strict()).unwrap_err();
        assert!(
            err.contains("run_as='totally-missing'"),
            "strict context must surface the invariant error: {err}"
        );
    }
}
