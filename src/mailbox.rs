use crate::cli::MailboxCommand;
use crate::config::{Config, MailboxConfig};
use crate::term;
use std::io::{self, Write};
use std::path::Path;

pub fn run(
    cmd: MailboxCommand,
    data_dir: Option<&std::path::Path>,
) -> Result<(), Box<dyn std::error::Error>> {
    let config = Config::load_resolved_with_data_dir(data_dir)?;
    match cmd {
        MailboxCommand::Create { name } => create(&config, &name),
        MailboxCommand::List => list(&config),
        MailboxCommand::Delete { name, yes } => delete(&config, &name, yes),
    }
}

fn validate_mailbox_name(name: &str) -> Result<(), Box<dyn std::error::Error>> {
    if name.is_empty() {
        return Err("Mailbox name cannot be empty".into());
    }
    if name.contains("..") {
        return Err("Mailbox name cannot contain '..'".into());
    }
    if name.contains('/') || name.contains('\\') {
        return Err("Mailbox name cannot contain path separators".into());
    }
    if name.contains('\0') {
        return Err("Mailbox name cannot contain null bytes".into());
    }
    Ok(())
}

pub fn create_mailbox(config: &Config, name: &str) -> Result<(), Box<dyn std::error::Error>> {
    validate_mailbox_name(name)?;

    if config.mailboxes.contains_key(name) {
        return Err(format!("Mailbox '{name}' already exists").into());
    }

    let mailbox_dir = config.mailbox_dir(name);
    std::fs::create_dir_all(&mailbox_dir)?;

    let mut config = config.clone();
    config.mailboxes.insert(
        name.to_string(),
        MailboxConfig {
            address: format!("{name}@{}", config.domain),
            on_receive: vec![],
            trust: "none".to_string(),
            trusted_senders: vec![],
        },
    );

    config.save(&crate::config::config_path())?;

    Ok(())
}

pub fn list_mailboxes(config: &Config) -> Vec<(String, usize)> {
    let mut result: Vec<(String, usize)> = config
        .mailboxes
        .keys()
        .map(|name| {
            let count = count_messages(&config.mailbox_dir(name));
            (name.clone(), count)
        })
        .collect();
    result.sort_by(|a, b| a.0.cmp(&b.0));
    result
}

pub fn delete_mailbox(config: &Config, name: &str) -> Result<(), Box<dyn std::error::Error>> {
    if name == "catchall" {
        return Err("Cannot delete the catchall mailbox".into());
    }

    if !config.mailboxes.contains_key(name) {
        return Err(format!("Mailbox '{name}' does not exist").into());
    }

    let mailbox_dir = config.mailbox_dir(name);
    if mailbox_dir.exists() {
        std::fs::remove_dir_all(&mailbox_dir)?;
    }

    let mut config = config.clone();
    config.mailboxes.remove(name);

    config.save(&crate::config::config_path())?;

    Ok(())
}

fn count_messages(dir: &Path) -> usize {
    std::fs::read_dir(dir)
        .map(|entries| {
            entries
                .filter_map(|e| e.ok())
                .filter(|e| e.path().extension().is_some_and(|ext| ext == "md"))
                .count()
        })
        .unwrap_or(0)
}

fn create(config: &Config, name: &str) -> Result<(), Box<dyn std::error::Error>> {
    create_mailbox(config, name)?;
    println!("{}", term::success(&format!("Mailbox '{name}' created.")));
    Ok(())
}

fn list(config: &Config) -> Result<(), Box<dyn std::error::Error>> {
    let mailboxes = list_mailboxes(config);

    if mailboxes.is_empty() {
        println!("No mailboxes configured.");
        return Ok(());
    }

    let header_pad = 20usize.saturating_sub("MAILBOX".len());
    println!(
        "{}{:pad$} MESSAGES",
        term::header("MAILBOX"),
        "",
        pad = header_pad,
    );
    for (name, count) in mailboxes {
        let name_pad = 20usize.saturating_sub(name.chars().count());
        println!(
            "{}{:pad$} {}",
            term::highlight(&name),
            "",
            count,
            pad = name_pad,
        );
    }

    Ok(())
}

fn delete(config: &Config, name: &str, yes: bool) -> Result<(), Box<dyn std::error::Error>> {
    if !yes {
        print!("Delete mailbox '{name}' and all its emails? [y/N] ");
        io::stdout().flush()?;
        let mut input = String::new();
        io::stdin().read_line(&mut input)?;
        if !input.trim().eq_ignore_ascii_case("y") {
            println!("Cancelled.");
            return Ok(());
        }
    }

    delete_mailbox(config, name)?;
    println!("{}", term::success(&format!("Mailbox '{name}' deleted.")));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::test_env::ConfigDirOverride;
    use crate::config::{Config, MailboxConfig};
    use std::collections::HashMap;
    use tempfile::TempDir;

    fn test_config(tmp: &Path) -> Config {
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
        Config {
            domain: "test.com".to_string(),
            data_dir: tmp.to_path_buf(),
            dkim_selector: "dkim".to_string(),
            mailboxes,
            verify_host: None,
            enable_ipv6: false,
        }
    }

    /// Point `AIMX_CONFIG_DIR` at `tmp`, create the storage dir, and write
    /// `config.toml` to the resolved location. Returns the override guard
    /// which must be kept alive for the duration of the test.
    fn setup_config_file(tmp: &Path, config: &Config) -> ConfigDirOverride {
        std::fs::create_dir_all(&config.data_dir).unwrap();
        let guard = ConfigDirOverride::set(tmp);
        config.save(&crate::config::config_path()).unwrap();
        guard
    }

    #[test]
    fn create_new_mailbox() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(tmp.path());
        let _guard = setup_config_file(tmp.path(), &config);

        create_mailbox(&config, "alice").unwrap();

        assert!(tmp.path().join("alice").is_dir());
        let reloaded = Config::load_resolved().unwrap();
        assert!(reloaded.mailboxes.contains_key("alice"));
        assert_eq!(reloaded.mailboxes["alice"].address, "alice@test.com");
    }

    #[test]
    fn create_duplicate_fails() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(tmp.path());
        let _guard = setup_config_file(tmp.path(), &config);

        let result = create_mailbox(&config, "catchall");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("already exists"));
    }

    #[test]
    fn list_shows_mailboxes() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(tmp.path());
        let _guard = setup_config_file(tmp.path(), &config);

        std::fs::create_dir_all(tmp.path().join("catchall")).unwrap();
        std::fs::write(tmp.path().join("catchall/2025-01-01-001.md"), "test").unwrap();
        std::fs::write(tmp.path().join("catchall/2025-01-01-002.md"), "test").unwrap();

        let result = list_mailboxes(&config);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].0, "catchall");
        assert_eq!(result[0].1, 2);
    }

    #[test]
    fn delete_mailbox_works() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(tmp.path());
        let _guard = setup_config_file(tmp.path(), &config);

        create_mailbox(&config, "alice").unwrap();
        let config = Config::load_resolved().unwrap();
        assert!(config.mailboxes.contains_key("alice"));

        delete_mailbox(&config, "alice").unwrap();

        assert!(!tmp.path().join("alice").exists());
        let reloaded = Config::load_resolved().unwrap();
        assert!(!reloaded.mailboxes.contains_key("alice"));
    }

    #[test]
    fn delete_catchall_fails() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(tmp.path());
        let _guard = setup_config_file(tmp.path(), &config);

        let result = delete_mailbox(&config, "catchall");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Cannot delete"));
    }

    #[test]
    fn delete_nonexistent_fails() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(tmp.path());
        let _guard = setup_config_file(tmp.path(), &config);

        let result = delete_mailbox(&config, "nonexistent");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("does not exist"));
    }

    #[test]
    fn list_empty_mailbox() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(tmp.path());
        let result = list_mailboxes(&config);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].1, 0);
    }

    #[test]
    fn create_empty_name_fails() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(tmp.path());
        let _guard = setup_config_file(tmp.path(), &config);

        let result = create_mailbox(&config, "");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("empty"));
    }

    #[test]
    fn create_path_traversal_fails() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(tmp.path());
        let _guard = setup_config_file(tmp.path(), &config);

        let result = create_mailbox(&config, "../etc");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains(".."));
    }

    #[test]
    fn create_with_slash_fails() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(tmp.path());
        let _guard = setup_config_file(tmp.path(), &config);

        let result = create_mailbox(&config, "foo/bar");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("path separator"));
    }

    #[test]
    fn create_with_backslash_fails() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(tmp.path());
        let _guard = setup_config_file(tmp.path(), &config);

        let result = create_mailbox(&config, "foo\\bar");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("path separator"));
    }
}
