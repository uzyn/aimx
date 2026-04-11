use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

const DEFAULT_DATA_DIR: &str = "/var/lib/aimx";

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
        let content = std::fs::read_to_string(path)?;
        let config: Config = toml::from_str(&content)?;
        Ok(config)
    }

    pub fn save(&self, path: &Path) -> Result<(), Box<dyn std::error::Error>> {
        let content = toml::to_string_pretty(self)?;
        std::fs::write(path, content)?;
        Ok(())
    }

    pub fn config_path(data_dir: &Path) -> PathBuf {
        data_dir.join("config.toml")
    }

    pub fn load_from_data_dir(data_dir: &Path) -> Result<Self, Box<dyn std::error::Error>> {
        Self::load(&Self::config_path(data_dir))
    }

    pub fn load_default() -> Result<Self, Box<dyn std::error::Error>> {
        Self::load_from_data_dir(Path::new(DEFAULT_DATA_DIR))
    }

    pub fn mailbox_dir(&self, name: &str) -> PathBuf {
        self.data_dir.join(name)
    }

    pub fn resolve_mailbox(&self, local_part: &str) -> String {
        if self.mailboxes.contains_key(local_part) {
            local_part.to_string()
        } else {
            "catchall".to_string()
        }
    }
}

#[cfg(test)]
mod tests {
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
            PathBuf::from("/tmp/aimx-test/support")
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
}
