use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

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

    #[serde(default)]
    pub on_receive: Vec<OnReceiveRule>,

    #[serde(default = "default_trust")]
    pub trust: String,

    #[serde(default)]
    pub trusted_senders: Vec<String>,
}

fn default_trust() -> String {
    "none".to_string()
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub struct OnReceiveRule {
    #[serde(rename = "type")]
    pub rule_type: String,

    pub command: String,

    #[serde(default)]
    pub r#match: Option<MatchFilter>,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub struct MatchFilter {
    pub from: Option<String>,
    pub subject: Option<String>,
    pub has_attachment: Option<bool>,
}

fn default_data_dir() -> PathBuf {
    PathBuf::from(DEFAULT_DATA_DIR)
}

fn default_dkim_selector() -> String {
    "dkim".to_string()
}

impl Config {
    pub fn load(path: &Path) -> Result<Self, Box<dyn std::error::Error>> {
        let content = std::fs::read_to_string(path).map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                format!(
                    "Config file not found at {} — run 'sudo aimx setup' first",
                    path.display()
                )
                .into()
            } else {
                Box::new(e) as Box<dyn std::error::Error>
            }
        })?;
        let config: Config = toml::from_str(&content)?;
        Ok(config)
    }

    pub fn save(&self, path: &Path) -> Result<(), Box<dyn std::error::Error>> {
        let content = toml::to_string_pretty(self)?;
        std::fs::write(path, content)?;
        Ok(())
    }

    /// Load the config from the canonical path returned by [`config_path`].
    ///
    /// Replaces the old `load_from_data_dir` — config no longer lives inside
    /// the storage directory. Override via the `AIMX_CONFIG_DIR` env var
    /// (tests, non-standard installs).
    pub fn load_resolved() -> Result<Self, Box<dyn std::error::Error>> {
        Self::load(&config_path())
    }

    /// Load the config and apply an optional `--data-dir` / `AIMX_DATA_DIR`
    /// override for the storage path. `config.data_dir` is the source of
    /// truth post-Sprint 33; this helper lets the CLI flag still redirect
    /// storage (its documented purpose) without touching the config file on
    /// disk.
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
    /// Since Sprint 36 the datadir splits inbound mail into `inbox/` and
    /// outbound sent copies into `sent/`. `mailbox_dir` remains a shorthand
    /// for the inbox path (which is what every legacy reader cared about);
    /// callers that want the outbound side use [`Config::sent_dir`].
    pub fn mailbox_dir(&self, name: &str) -> PathBuf {
        self.inbox_dir(name)
    }

    /// Path to a mailbox's inbox directory (`<data_dir>/inbox/<name>/`).
    pub fn inbox_dir(&self, name: &str) -> PathBuf {
        self.data_dir.join("inbox").join(name)
    }

    /// Path to a mailbox's sent directory (`<data_dir>/sent/<name>/`).
    ///
    /// Sent storage is populated by `aimx serve` in Sprint 38; the directory
    /// is still created on `mailbox create` so the layout is consistent
    /// from day one.
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

/// Test-only helpers for overriding `AIMX_CONFIG_DIR` safely from multiple
/// test modules. Process-wide env is not parallel-safe, so every test that
/// mutates this variable must go through [`ConfigDirOverride`] — it
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

[[mailboxes.support.on_receive]]
type = "cmd"
command = 'echo "{from}" >> /tmp/log'

[mailboxes.support.on_receive.match]
from = "*@gmail.com"
subject = "urgent"
has_attachment = true
"#
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
    fn parse_on_receive_rules() {
        let config: Config = toml::from_str(sample_toml()).unwrap();
        let support = &config.mailboxes["support"];
        assert_eq!(support.on_receive.len(), 1);
        let rule = &support.on_receive[0];
        assert_eq!(rule.rule_type, "cmd");
        assert_eq!(rule.command, "echo \"{from}\" >> /tmp/log");
        let m = rule.r#match.as_ref().unwrap();
        assert_eq!(m.from.as_deref(), Some("*@gmail.com"));
        assert_eq!(m.subject.as_deref(), Some("urgent"));
        assert_eq!(m.has_attachment, Some(true));
    }

    #[test]
    fn default_data_dir() {
        let toml_str = "domain = \"test.com\"\n[mailboxes]\n";
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(config.data_dir, PathBuf::from("/var/lib/aimx"));
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
                on_receive: vec![],
                trust: "none".to_string(),
                trusted_senders: vec![],
            },
        );

        let config = Config {
            domain: "test.com".to_string(),
            data_dir: tmp.path().to_path_buf(),
            dkim_selector: "dkim".to_string(),
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
        assert_eq!(secure.trust, "verified");
        assert_eq!(secure.trusted_senders.len(), 2);
        assert_eq!(secure.trusted_senders[0], "*@company.com");
        assert_eq!(secure.trusted_senders[1], "boss@gmail.com");
    }

    #[test]
    fn default_trust_is_none() {
        let toml_str = r#"
domain = "test.com"

[mailboxes.catchall]
address = "*@test.com"
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        let catchall = &config.mailboxes["catchall"];
        assert_eq!(catchall.trust, "none");
        assert!(catchall.trusted_senders.is_empty());
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
    fn legacy_verify_address_field_ignored() {
        // Config with removed verify_address should still parse (serde ignores unknown fields)
        let toml_str = "domain = \"test.com\"\nverify_address = \"verify@old.com\"\n[mailboxes]\n";
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(config.domain, "test.com");
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
    fn legacy_probe_url_field_silently_ignored() {
        // Pre-rename configs used `probe_url`; serde drops unknown fields so those
        // configs still load, but `verify_host` is left unset (falls back to default).
        let toml_str = r#"
domain = "test.com"
probe_url = "https://old.example.com/probe"

[mailboxes]
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(config.domain, "test.com");
        assert!(config.verify_host.is_none());
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
