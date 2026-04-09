use crate::cli::MailboxCommand;
use crate::config::{Config, MailboxConfig};
use std::io::{self, Write};
use std::path::Path;

pub fn run(cmd: MailboxCommand) -> Result<(), Box<dyn std::error::Error>> {
    let config = Config::load_default()?;
    match cmd {
        MailboxCommand::Create { name } => create(&config, &name),
        MailboxCommand::List => list(&config),
        MailboxCommand::Delete { name, yes } => delete(&config, &name, yes),
    }
}

pub fn create_mailbox(config: &Config, name: &str) -> Result<(), Box<dyn std::error::Error>> {
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
        },
    );

    let config_path = Config::config_path(&config.data_dir);
    config.save(&config_path)?;

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

    let config_path = Config::config_path(&config.data_dir);
    config.save(&config_path)?;

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
    println!("Mailbox '{name}' created.");
    Ok(())
}

fn list(config: &Config) -> Result<(), Box<dyn std::error::Error>> {
    let mailboxes = list_mailboxes(config);

    if mailboxes.is_empty() {
        println!("No mailboxes configured.");
        return Ok(());
    }

    println!("{:<20} MESSAGES", "MAILBOX");
    for (name, count) in mailboxes {
        println!("{:<20} {}", name, count);
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
    println!("Mailbox '{name}' deleted.");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
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
            },
        );
        Config {
            domain: "test.com".to_string(),
            data_dir: tmp.to_path_buf(),
            mailboxes,
        }
    }

    fn setup_config_file(config: &Config) {
        std::fs::create_dir_all(&config.data_dir).unwrap();
        let path = Config::config_path(&config.data_dir);
        config.save(&path).unwrap();
    }

    #[test]
    fn create_new_mailbox() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(tmp.path());
        setup_config_file(&config);

        create_mailbox(&config, "alice").unwrap();

        assert!(tmp.path().join("alice").is_dir());
        let reloaded = Config::load_from_data_dir(tmp.path()).unwrap();
        assert!(reloaded.mailboxes.contains_key("alice"));
        assert_eq!(reloaded.mailboxes["alice"].address, "alice@test.com");
    }

    #[test]
    fn create_duplicate_fails() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(tmp.path());
        setup_config_file(&config);

        let result = create_mailbox(&config, "catchall");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("already exists"));
    }

    #[test]
    fn list_shows_mailboxes() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(tmp.path());
        setup_config_file(&config);

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
        setup_config_file(&config);

        create_mailbox(&config, "alice").unwrap();
        let config = Config::load_from_data_dir(tmp.path()).unwrap();
        assert!(config.mailboxes.contains_key("alice"));

        delete_mailbox(&config, "alice").unwrap();

        assert!(!tmp.path().join("alice").exists());
        let reloaded = Config::load_from_data_dir(tmp.path()).unwrap();
        assert!(!reloaded.mailboxes.contains_key("alice"));
    }

    #[test]
    fn delete_catchall_fails() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(tmp.path());
        setup_config_file(&config);

        let result = delete_mailbox(&config, "catchall");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Cannot delete"));
    }

    #[test]
    fn delete_nonexistent_fails() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(tmp.path());
        setup_config_file(&config);

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
}
