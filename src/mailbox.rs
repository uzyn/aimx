use crate::cli::MailboxCommand;
use crate::config::{Config, MailboxConfig};
use crate::term;
use std::io::{self, Write};
use std::path::Path;

pub fn run(cmd: MailboxCommand, config: Config) -> Result<(), Box<dyn std::error::Error>> {
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

    // Sprint 36: a mailbox lives in both `inbox/<name>/` and
    // `sent/<name>/`. Create them atomically — if the second one fails,
    // clean up the first so we never leave half a mailbox on disk.
    let inbox = config.inbox_dir(name);
    std::fs::create_dir_all(&inbox)?;

    let sent = config.sent_dir(name);
    if let Err(e) = std::fs::create_dir_all(&sent) {
        let _ = std::fs::remove_dir_all(&inbox);
        return Err(e.into());
    }

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

pub fn list_mailboxes(config: &Config) -> Vec<(String, usize, usize)> {
    let names = discover_mailbox_names(config);
    let mut result: Vec<(String, usize, usize)> = names
        .into_iter()
        .map(|name| {
            let inbox_count = count_messages(&config.inbox_dir(&name));
            let sent_count = count_messages(&config.sent_dir(&name));
            (name, inbox_count, sent_count)
        })
        .collect();
    result.sort_by(|a, b| a.0.cmp(&b.0));
    result
}

/// Sprint 36: union of (a) mailboxes registered in `config.mailboxes` and
/// (b) directories under `<data_dir>/inbox/`. Operators who restore an
/// inbox dir out-of-band, or unregister a mailbox while keeping its
/// messages on disk, still see the directory listed (the CLI/MCP can
/// surface unregistered ones with a marker if needed). The catchall is
/// always kept in config so it is always surfaced.
pub fn discover_mailbox_names(config: &Config) -> Vec<String> {
    use std::collections::BTreeSet;

    let mut set: BTreeSet<String> = config.mailboxes.keys().cloned().collect();

    let inbox_root = config.data_dir.join("inbox");
    if let Ok(entries) = std::fs::read_dir(&inbox_root) {
        for entry in entries.filter_map(|e| e.ok()) {
            if entry.file_type().is_ok_and(|t| t.is_dir())
                && let Some(name) = entry.file_name().to_str()
            {
                set.insert(name.to_string());
            }
        }
    }

    set.into_iter().collect()
}

/// Returns true when a mailbox name appears in the config map.
/// Useful for callers that want to mark filesystem-only mailboxes as
/// `(unregistered)` in display output.
pub fn is_registered(config: &Config, name: &str) -> bool {
    config.mailboxes.contains_key(name)
}

pub fn delete_mailbox(config: &Config, name: &str) -> Result<(), Box<dyn std::error::Error>> {
    if name == "catchall" {
        return Err("Cannot delete the catchall mailbox".into());
    }

    if !config.mailboxes.contains_key(name) {
        return Err(format!("Mailbox '{name}' does not exist").into());
    }

    // Sprint 36: remove both inbox and sent directories.
    let inbox = config.inbox_dir(name);
    if inbox.exists() {
        std::fs::remove_dir_all(&inbox)?;
    }
    let sent = config.sent_dir(name);
    if sent.exists() {
        std::fs::remove_dir_all(&sent)?;
    }

    let mut config = config.clone();
    config.mailboxes.remove(name);

    config.save(&crate::config::config_path())?;

    Ok(())
}

/// Count emails in a mailbox directory. Each flat `<stem>.md` counts as
/// one, and each bundle directory containing `<stem>.md` counts as one.
/// Stray files or non-bundle directories are ignored.
pub fn count_messages(dir: &Path) -> usize {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return 0,
    };

    let mut total = 0usize;
    for entry in entries.filter_map(|e| e.ok()) {
        let path = entry.path();
        if path.is_dir() {
            if let Some(stem) = path.file_name().and_then(|f| f.to_str())
                && path.join(format!("{stem}.md")).exists()
            {
                total += 1;
            }
        } else if path.extension().is_some_and(|ext| ext == "md") {
            total += 1;
        }
    }
    total
}

fn create(config: &Config, name: &str) -> Result<(), Box<dyn std::error::Error>> {
    create_mailbox(config, name)?;
    println!("{}", term::success(&format!("Mailbox '{name}' created.")));
    print_restart_hint();
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
        "{}{:pad$} INBOX    SENT",
        term::header("MAILBOX"),
        "",
        pad = header_pad,
    );
    for (name, inbox_count, sent_count) in mailboxes {
        let name_pad = 20usize.saturating_sub(name.chars().count());
        let suffix = if is_registered(config, &name) {
            String::new()
        } else {
            format!(" {}", term::warn("(unregistered)"))
        };
        println!(
            "{}{:pad$} {:<8} {}{}",
            term::highlight(&name),
            "",
            inbox_count,
            sent_count,
            suffix,
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
    print_restart_hint();
    Ok(())
}

/// Dispatch table: init system -> the canonical restart command. OpenRC is
/// hard-coded here because there's no neutral abstraction across systemd
/// and OpenRC for the *restart* verb beyond `serve::service`'s existing
/// dispatch tables; keeping it inline keeps the hint readable without
/// threading the full init-system check through more modules.
pub(crate) fn restart_hint_command(init: &crate::serve::service::InitSystem) -> &'static str {
    match init {
        crate::serve::service::InitSystem::OpenRC => "sudo rc-service aimx restart",
        // Systemd and Unknown both land here — systemd is far more common,
        // and on an unknown init the systemd wording is a better fallback
        // than saying nothing (operator can translate once).
        _ => "sudo systemctl restart aimx",
    }
}

/// Build the lines of the restart-hint banner without printing them.
/// Exposed for tests so we can assert on content without capturing stdout.
pub(crate) fn restart_hint_lines(init: &crate::serve::service::InitSystem) -> Vec<String> {
    let cmd = restart_hint_command(init);
    vec![
        format!(
            "{} Restart the daemon for the change to take effect:",
            term::warn("Hint:")
        ),
        format!("  {}", term::highlight(cmd)),
    ]
}

/// Print the service-restart hint after a `mailbox create` / `delete`.
///
/// S44-4: `aimx serve` loads `Config` once at startup (`src/serve.rs`),
/// so a fresh `[mailboxes.<name>]` entry in `config.toml` doesn't reach the
/// running daemon until it restarts. Without this hint an operator who
/// creates a new mailbox will see inbound mail silently route to
/// `catchall` until they learn to restart the service. Sprint 46 will
/// remove the need by routing mailbox CRUD through the daemon over UDS;
/// until then the hint is the tier-1 fix for finding #1 from the
/// 2026-04-17 manual test run.
fn print_restart_hint() {
    let init = crate::serve::service::detect_init_system();
    for line in restart_hint_lines(&init) {
        println!("{line}");
    }
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

        // Sprint 36: both `inbox/<name>/` and `sent/<name>/` exist.
        assert!(tmp.path().join("inbox").join("alice").is_dir());
        assert!(tmp.path().join("sent").join("alice").is_dir());
        let reloaded = Config::load_resolved().unwrap();
        assert!(reloaded.mailboxes.contains_key("alice"));
        assert_eq!(reloaded.mailboxes["alice"].address, "alice@test.com");
    }

    #[test]
    fn create_mailbox_is_idempotent_for_dirs_when_config_race_prevented() {
        // If the config-registration side-steps the duplicate check, the
        // create_dir_all calls are idempotent. This is an internal contract
        // test — callers should rely on `create_mailbox` itself to fail
        // duplicate registrations via the HashMap check.
        let tmp = TempDir::new().unwrap();
        let config = test_config(tmp.path());
        let _guard = setup_config_file(tmp.path(), &config);

        create_mailbox(&config, "alice").unwrap();
        // Re-creating via a fresh Config (as if registration rolled back)
        // must not error — dir creation is idempotent.
        let fresh = test_config(tmp.path());
        create_mailbox(&fresh, "alice").unwrap();
        assert!(tmp.path().join("inbox").join("alice").is_dir());
        assert!(tmp.path().join("sent").join("alice").is_dir());
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

        let inbox_catchall = tmp.path().join("inbox").join("catchall");
        std::fs::create_dir_all(&inbox_catchall).unwrap();
        std::fs::write(inbox_catchall.join("2025-01-01-120000-a.md"), "test").unwrap();
        std::fs::write(inbox_catchall.join("2025-01-01-120001-b.md"), "test").unwrap();

        let result = list_mailboxes(&config);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].0, "catchall");
        assert_eq!(result[0].1, 2); // inbox count
        assert_eq!(result[0].2, 0); // sent count
    }

    #[test]
    fn list_surfaces_stray_inbox_dir_without_config_entry() {
        // Sprint 36 review fix: `mailbox_list` must scan `inbox/*/` so an
        // inbox directory left by a backup restore (or an unregistered
        // mailbox) still appears in the listing.
        let tmp = TempDir::new().unwrap();
        let config = test_config(tmp.path());
        let _guard = setup_config_file(tmp.path(), &config);

        // Registered mailbox: catchall + an `alice` we register.
        create_mailbox(&config, "alice").unwrap();
        let config = Config::load_resolved().unwrap();
        let inbox_alice = tmp.path().join("inbox").join("alice");
        std::fs::write(inbox_alice.join("2025-01-01-120000-a.md"), "x").unwrap();

        // Stray dir created out-of-band — no config entry.
        let stray = tmp.path().join("inbox").join("stray");
        std::fs::create_dir_all(&stray).unwrap();
        std::fs::write(stray.join("2025-01-01-120000-z.md"), "x").unwrap();

        let result = list_mailboxes(&config);
        let names: Vec<&str> = result.iter().map(|(n, _, _)| n.as_str()).collect();
        assert!(names.contains(&"alice"), "registered alice listed");
        assert!(names.contains(&"catchall"), "catchall always listed");
        assert!(names.contains(&"stray"), "stray inbox dir surfaced");

        let stray_row = result.iter().find(|(n, _, _)| n == "stray").unwrap();
        assert_eq!(stray_row.1, 1, "stray dir counts its messages");
        assert!(!is_registered(&config, "stray"));
        assert!(is_registered(&config, "alice"));
        assert!(is_registered(&config, "catchall"));
    }

    #[test]
    fn list_counts_bundle_directories() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(tmp.path());
        let _guard = setup_config_file(tmp.path(), &config);

        let inbox_catchall = tmp.path().join("inbox").join("catchall");
        std::fs::create_dir_all(&inbox_catchall).unwrap();
        // A flat email.
        std::fs::write(inbox_catchall.join("2025-01-01-120000-flat.md"), "x").unwrap();
        // A bundle email.
        let bundle = inbox_catchall.join("2025-01-01-120001-bundle");
        std::fs::create_dir_all(&bundle).unwrap();
        std::fs::write(bundle.join("2025-01-01-120001-bundle.md"), "x").unwrap();
        std::fs::write(bundle.join("att.txt"), "att").unwrap();

        let result = list_mailboxes(&config);
        assert_eq!(result[0].1, 2, "bundle and flat each count once (inbox)");
        assert_eq!(result[0].2, 0, "no sent messages");
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

        // Both inbox and sent directories must be gone.
        assert!(!tmp.path().join("inbox").join("alice").exists());
        assert!(!tmp.path().join("sent").join("alice").exists());
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
        let err = result.unwrap_err().to_string();
        assert!(err.contains("Cannot delete"));
        assert!(
            err.contains("catchall"),
            "error should mention catchall: {err}"
        );
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
        assert_eq!(result[0].1, 0); // inbox
        assert_eq!(result[0].2, 0); // sent
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

    // ----- S44-4 restart hint ------------------------------------------

    fn strip_ansi(s: &str) -> String {
        let mut out = String::with_capacity(s.len());
        let mut chars = s.chars().peekable();
        while let Some(c) = chars.next() {
            if c == '\u{001b}' && chars.peek() == Some(&'[') {
                chars.next();
                while let Some(&next) = chars.peek() {
                    chars.next();
                    if ('@'..='~').contains(&next) {
                        break;
                    }
                }
            } else {
                out.push(c);
            }
        }
        out
    }

    #[test]
    fn restart_hint_command_systemd_default() {
        assert_eq!(
            restart_hint_command(&crate::serve::service::InitSystem::Systemd),
            "sudo systemctl restart aimx"
        );
    }

    #[test]
    fn restart_hint_command_openrc() {
        assert_eq!(
            restart_hint_command(&crate::serve::service::InitSystem::OpenRC),
            "sudo rc-service aimx restart"
        );
    }

    #[test]
    fn restart_hint_command_unknown_falls_back_to_systemd() {
        // Better to print systemd wording than to say nothing — operators on
        // niche init systems can translate once.
        assert_eq!(
            restart_hint_command(&crate::serve::service::InitSystem::Unknown),
            "sudo systemctl restart aimx"
        );
    }

    #[test]
    fn restart_hint_lines_include_restart_verb_and_command() {
        let lines = restart_hint_lines(&crate::serve::service::InitSystem::Systemd);
        let joined = strip_ansi(&lines.join("\n"));
        assert!(
            joined.contains("Restart the daemon"),
            "hint must mention restart: {joined}"
        );
        assert!(
            joined.contains("sudo systemctl restart aimx"),
            "hint must include the command: {joined}"
        );
    }

    #[test]
    fn restart_hint_lines_openrc_uses_rc_service() {
        let lines = restart_hint_lines(&crate::serve::service::InitSystem::OpenRC);
        let joined = strip_ansi(&lines.join("\n"));
        assert!(
            joined.contains("sudo rc-service aimx restart"),
            "OpenRC hint must use rc-service: {joined}"
        );
        assert!(
            !joined.contains("systemctl"),
            "OpenRC hint must not reference systemctl: {joined}"
        );
    }
}
