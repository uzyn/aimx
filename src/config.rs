use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use crate::hook::{Hook, HookEvent, effective_hook_name, is_valid_hook_name};

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
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub struct MailboxConfig {
    pub address: String,

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
}

impl MailboxConfig {
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
/// Sprint 50 is pre-launch, so there is no compat shim. Users hand-editing
/// old configs see a single actionable error naming the offending mailbox.
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
            if hook.cmd.trim().is_empty() {
                return Err(format!(
                    "hook '{label}' on mailbox '{mailbox_name}' has empty `cmd`"
                ));
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
pub(crate) fn validate_single_hook(hook: &Hook) -> Result<(), String> {
    if let Some(name) = &hook.name
        && !is_valid_hook_name(name)
    {
        return Err(format!(
            "invalid hook name '{name}': must match \
             [a-zA-Z0-9_][a-zA-Z0-9_.-]{{0,127}}"
        ));
    }
    if hook.cmd.trim().is_empty() {
        return Err("hook has empty `cmd`".into());
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
    Ok(())
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
    pub fn load(path: &Path) -> Result<Self, Box<dyn std::error::Error>> {
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
        validate_trust_values(&config).map_err(|e| -> Box<dyn std::error::Error> { e.into() })?;
        validate_hooks(&config).map_err(|e| -> Box<dyn std::error::Error> { e.into() })?;
        Ok(config)
    }

    pub fn save(&self, path: &Path) -> Result<(), Box<dyn std::error::Error>> {
        let content = toml::to_string_pretty(self)?;
        std::fs::write(path, content)?;
        Ok(())
    }

    /// Load the config from the canonical path returned by [`config_path`].
    ///
    /// Replaces the old `load_from_data_dir`. Config no longer lives inside
    /// the storage directory. Override via the `AIMX_CONFIG_DIR` env var
    /// (tests, non-standard installs).
    pub fn load_resolved() -> Result<Self, Box<dyn std::error::Error>> {
        Self::load(&config_path())
    }

    /// Load the config and apply an optional `--data-dir` / `AIMX_DATA_DIR`
    /// override for the storage path. `config.data_dir` is the source of
    /// truth on disk; this helper lets the CLI flag still redirect storage
    /// (its documented purpose) without touching the config file on disk.
    pub fn load_resolved_with_data_dir(
        data_dir_override: Option<&Path>,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let mut cfg = Self::load_resolved()?;
        if let Some(dir) = data_dir_override {
            cfg.data_dir = dir.to_path_buf();
        }
        Ok(cfg)
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

    pub fn resolve_mailbox(&self, local_part: &str) -> String {
        if self.mailboxes.contains_key(local_part) {
            local_part.to_string()
        } else {
            "catchall".to_string()
        }
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

[mailboxes.support]
address = "support@agent.example.com"

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

[mailboxes.public]
address = "hello@test.com"
trust = "none"
"#,
        )
        .unwrap();
        let cfg = Config::load(&path).unwrap();
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

[[mailboxes.support.hooks]]
name = "support_env"
event = "on_receive"
cmd = '''
printf 'from=%s subject=%s id=%s\n' "$AIMX_FROM" "$AIMX_SUBJECT" "{id}"
'''
"#,
        )
        .unwrap();
        let cfg = Config::load(&path).unwrap();
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

[[mailboxes.support.hooks]]
event = "on_receive"
cmd = "echo derive-me"
"#,
        )
        .unwrap();
        let cfg = Config::load(&path).unwrap();
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

[[mailboxes.one.hooks]]
event = "on_receive"
cmd = "echo same"

[mailboxes.two]
address = "two@test.com"

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

[[mailboxes.one.hooks]]
event = "on_receive"
cmd = "echo collide"

[mailboxes.two]
address = "two@test.com"

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

[[mailboxes.one.hooks]]
name = "same_name"
event = "on_receive"
cmd = "echo one"

[mailboxes.two]
address = "two@test.com"

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
                hooks: vec![],
                trust: None,
                trusted_senders: None,
            },
        );

        let config = Config {
            domain: "test.com".to_string(),
            data_dir: tmp.path().to_path_buf(),
            dkim_selector: "aimx".to_string(),
            trust: "none".to_string(),
            trusted_senders: vec![],
            mailboxes,
            verify_host: None,
            enable_ipv6: false,
        };

        config.save(&path).unwrap();
        let loaded = Config::load(&path).unwrap();
        assert_eq!(config, loaded);
    }

    #[test]
    fn resolve_mailbox_known() {
        let config: Config = toml::from_str(sample_toml()).unwrap();
        assert_eq!(config.resolve_mailbox("support"), "support");
    }

    #[test]
    fn resolve_mailbox_unknown_falls_to_catchall() {
        let config: Config = toml::from_str(sample_toml()).unwrap();
        assert_eq!(config.resolve_mailbox("unknown"), "catchall");
    }

    #[test]
    fn parse_trust_settings() {
        let toml_str = r#"
domain = "test.com"

[mailboxes.secure]
address = "secure@test.com"
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
            mailboxes: HashMap::new(),
            verify_host: None,
            enable_ipv6: false,
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
                hooks: vec![],
                trust: None,
                trusted_senders: None,
            },
        );
        let config = Config {
            domain: "test.com".to_string(),
            data_dir: PathBuf::from("/tmp/x"),
            dkim_selector: "aimx".to_string(),
            trust: "verified".to_string(),
            trusted_senders: vec!["*@company.com".to_string()],
            mailboxes,
            verify_host: None,
            enable_ipv6: false,
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

        let config = Config::load_resolved().unwrap();
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
}
