use crate::cli::MailboxCommand;
use crate::config::{Config, MailboxConfig};
use crate::term;
use std::io::{self, Write};
use std::path::Path;

pub fn run(cmd: MailboxCommand, config: Config) -> Result<(), Box<dyn std::error::Error>> {
    match cmd {
        MailboxCommand::Create { name } => create(&config, &name),
        MailboxCommand::List => list(&config),
        MailboxCommand::Show { name } => show(&config, &name),
        MailboxCommand::Delete { name, yes, force } => delete(&config, &name, yes, force),
    }
}

/// Canonical mailbox-name validator. Rejects anything that would be
/// unsafe as a file-system path component *or* as the local-part of the
/// resulting email address (`<name>@<domain>`).
///
/// A valid mailbox name is non-empty, matches `[a-z0-9._-]+` (case-folded,
/// no uppercase), and contains no leading/trailing `.` or consecutive `..`.
/// This is stricter than RFC 5322 allows but matches what modern MTAs
/// actually accept without quoting, which is what we care about in
/// practice.
///
/// Used both by the CLI path (`aimx mailboxes create`) and by the UDS
/// handler (`MAILBOX-CREATE`/`MAILBOX-DELETE`). Keeping a single source of
/// truth prevents drift between the two.
pub(crate) fn validate_mailbox_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("Mailbox name cannot be empty".into());
    }
    if name.contains("..") {
        return Err("Mailbox name cannot contain '..'".into());
    }
    if name.starts_with('.') || name.ends_with('.') {
        return Err("Mailbox name cannot start or end with '.'".into());
    }
    for c in name.chars() {
        let ok = c.is_ascii_lowercase() || c.is_ascii_digit() || c == '.' || c == '_' || c == '-';
        if !ok {
            return Err(format!(
                "Mailbox name contains invalid character {c:?}; allowed: [a-z0-9._-]"
            ));
        }
    }
    Ok(())
}

pub fn create_mailbox(config: &Config, name: &str) -> Result<(), Box<dyn std::error::Error>> {
    validate_mailbox_name(name).map_err(|e| -> Box<dyn std::error::Error> { e.into() })?;

    if config.mailboxes.contains_key(name) {
        return Err(format!("Mailbox '{name}' already exists").into());
    }

    // A mailbox lives in both `inbox/<name>/` and `sent/<name>/`. Create
    // them atomically: if the second one fails, clean up the first so we
    // never leave half a mailbox on disk.
    let inbox = config.inbox_dir(name);
    std::fs::create_dir_all(&inbox)?;

    let sent = config.sent_dir(name);
    if let Err(e) = std::fs::create_dir_all(&sent) {
        let _ = std::fs::remove_dir_all(&inbox);
        return Err(e.into());
    }

    let mut config = config.clone();
    // Per PRD §6.2 the mailbox `owner` field is required. Sprint 3's
    // setup refactor introduces the interactive owner prompt; for now,
    // default to the local part of the address so existing CLI flows
    // (`aimx mailboxes create <name>`) keep working. Operators can edit
    // `config.toml` or re-run with Sprint 3's flow to pick a different
    // owner.
    config.mailboxes.insert(
        name.to_string(),
        MailboxConfig {
            address: format!("{name}@{}", config.domain),
            owner: name.to_string(),
            hooks: vec![],
            trust: None,
            trusted_senders: None,
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

/// Union of (a) mailboxes registered in `config.mailboxes` and (b)
/// directories under `<data_dir>/inbox/`. Operators who restore an inbox
/// dir out-of-band, or unregister a mailbox while keeping its messages
/// on disk, still see the directory listed (the CLI/MCP can surface
/// unregistered ones with a marker if needed). The catchall is always
/// kept in config so it is always surfaced.
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

    // Remove both inbox and sent directories.
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

/// Grammatical plural of "file"/"files" for counts used in operator-facing
/// prompts. Keeps the `N file` / `N files` distinction out of inline
/// `format!` calls so every caller stays consistent.
fn pluralize_files(count: usize) -> String {
    if count == 1 {
        format!("{count} file")
    } else {
        format!("{count} files")
    }
}

/// Count emails in a mailbox directory. Each flat `<stem>.md` counts as
/// one, and each bundle directory containing `<stem>.md` counts as one.
/// Stray files or non-bundle directories are ignored.
///
/// NOTE: this is the CLI-side count used only for the `--force` confirmation
/// prompt. The daemon's NONEMPTY check in `mailbox_handler.rs` uses a raw
/// `read_dir().count()` via `count_files_if_exists`, so a mailbox with
/// stray files (editor backups, dotfiles, a bundle missing its `<stem>.md`)
/// can show `0 files` here while the daemon still refuses to delete it
/// without `--force`. After a `--force` wipe both counts land at zero, so
/// the display divergence is cosmetic.
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
    // Try the UDS path first so the daemon hot-swaps its in-memory
    // Config. On socket-missing (daemon stopped, fresh install), fall
    // back to direct on-disk edit + the restart-hint banner. When UDS
    // succeeds we suppress the hint; the daemon already picked up the
    // change.
    match crate::mcp::submit_mailbox_crud_via_daemon(name, true) {
        Ok(()) => {
            println!("{}", term::success(&format!("Mailbox '{name}' created.")));
            Ok(())
        }
        Err(crate::mcp::MailboxCrudFallback::SocketMissing) => {
            create_mailbox(config, name)?;
            println!("{}", term::success(&format!("Mailbox '{name}' created.")));
            print_restart_hint();
            Ok(())
        }
        Err(crate::mcp::MailboxCrudFallback::Daemon(msg)) => Err(msg.into()),
    }
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

/// Build the formatted lines emitted by `aimx mailboxes show <name>`.
/// Pure function with no stdout access, so tests can assert on exact
/// content without capturing process output. The terminal-color helpers
/// already strip ANSI when `NO_COLOR` is set or stdout is not a TTY.
pub(crate) fn build_show_lines(
    config: &Config,
    name: &str,
) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    let mb = config
        .mailboxes
        .get(name)
        .ok_or_else(|| -> Box<dyn std::error::Error> {
            format!("Mailbox '{name}' does not exist").into()
        })?;

    let inbox_dir = config.inbox_dir(name);
    let sent_dir = config.sent_dir(name);
    let (inbox_total, inbox_unread) = count_with_unread(&inbox_dir);
    let (sent_total, _sent_unread) = count_with_unread(&sent_dir);

    let effective_trust = mb.effective_trust(config);
    let effective_senders = mb.effective_trusted_senders(config);

    let mut lines: Vec<String> = Vec::new();
    lines.push(format!(
        "{} {}",
        term::header("Mailbox:"),
        term::highlight(name)
    ));
    lines.push(format!("  {} {}", term::header("Address:"), mb.address));
    lines.push(format!(
        "  {} {}",
        term::header("Trust:  "),
        term::info(effective_trust)
    ));

    if effective_senders.is_empty() {
        lines.push(format!(
            "  {} {}",
            term::header("Trusted senders:"),
            term::dim("(none)")
        ));
    } else {
        lines.push(format!("  {}", term::header("Trusted senders:")));
        for s in effective_senders {
            lines.push(format!("    - {s}"));
        }
    }

    lines.push(String::new());
    lines.push(term::header("Hooks").to_string());
    let on_receive: Vec<_> = mb.on_receive_hooks().collect();
    let after_send: Vec<_> = mb.after_send_hooks().collect();
    if on_receive.is_empty() && after_send.is_empty() {
        lines.push(format!("  {}", term::dim("(none)")));
    } else {
        push_event_group(&mut lines, "on_receive", &on_receive);
        push_event_group(&mut lines, "after_send", &after_send);
    }

    lines.push(String::new());
    lines.push(term::header("Messages").to_string());
    lines.push(format!(
        "  {} {} ({} unread)",
        term::header("inbox:"),
        inbox_total,
        inbox_unread
    ));
    lines.push(format!("  {} {}", term::header("sent: "), sent_total));

    Ok(lines)
}

fn push_event_group(lines: &mut Vec<String>, event: &str, hooks: &[&crate::hook::Hook]) {
    if hooks.is_empty() {
        return;
    }
    lines.push(format!("  {}", term::header(event)));
    for h in hooks {
        let cmd = crate::hooks::truncate_with_ellipsis(&h.cmd, 60);
        let name = crate::hook::effective_hook_name(h);
        let suffix = if h.dangerously_support_untrusted {
            "   [dangerously_support_untrusted=true]"
        } else {
            ""
        };
        lines.push(format!(
            "    - {}  cmd: {}{}",
            term::highlight(&name),
            cmd,
            suffix
        ));
    }
}

fn show(config: &Config, name: &str) -> Result<(), Box<dyn std::error::Error>> {
    let lines = build_show_lines(config, name)?;
    for line in lines {
        println!("{line}");
    }
    Ok(())
}

/// Count messages in a mailbox folder and also return the number of
/// unread inbox entries. Mirrors `doctor::count_messages`; duplicated
/// here so the show command does not depend on the doctor module's
/// internal layout (and because the doctor's helper is private).
fn count_with_unread(dir: &Path) -> (usize, usize) {
    use crate::frontmatter::InboundFrontmatter;

    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return (0, 0),
    };

    let mut total = 0usize;
    let mut unread = 0usize;
    for entry in entries.filter_map(|e| e.ok()) {
        let path = entry.path();
        let md_path = if path.is_dir() {
            let stem = match path.file_name().and_then(|f| f.to_str()) {
                Some(s) => s.to_string(),
                None => continue,
            };
            let candidate = path.join(format!("{stem}.md"));
            if !candidate.exists() {
                continue;
            }
            candidate
        } else if path.extension().is_some_and(|ext| ext == "md") {
            path
        } else {
            continue;
        };

        total += 1;
        let content = match std::fs::read_to_string(&md_path) {
            Ok(c) => c,
            Err(_) => continue,
        };
        let parts: Vec<&str> = content.splitn(3, "+++").collect();
        if parts.len() < 3 {
            continue;
        }
        let toml_str = parts[1].trim();
        if let Ok(meta) = toml::from_str::<InboundFrontmatter>(toml_str)
            && !meta.read
        {
            unread += 1;
        }
    }
    (total, unread)
}

fn delete(
    config: &Config,
    name: &str,
    yes: bool,
    force: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    // S48-5: `--force` wipes `inbox/<name>/` and `sent/<name>/`
    // contents before invoking the normal delete path. The wipe is
    // CLI-only (the MCP `mailbox_delete` tool does not gain a force
    // variant per S48-6) and refuses on the catchall.
    if force {
        if name == "catchall" {
            return Err("Cannot delete the catchall mailbox".into());
        }
        let inbox_dir = config.inbox_dir(name);
        let sent_dir = config.sent_dir(name);
        let inbox_count = count_messages(&inbox_dir);
        let sent_count = count_messages(&sent_dir);

        if !yes {
            println!(
                "{} About to permanently delete mailbox '{name}':",
                term::warn("DESTRUCTIVE:"),
            );
            println!("  inbox/{name}/: {}", pluralize_files(inbox_count));
            println!("  sent/{name}/:  {}", pluralize_files(sent_count));
            print!("Continue? [y/N] ");
            io::stdout().flush()?;
            let mut input = String::new();
            io::stdin().read_line(&mut input)?;
            if !input.trim().eq_ignore_ascii_case("y") {
                println!("Cancelled.");
                return Ok(());
            }
        }

        wipe_mailbox_contents(&inbox_dir)?;
        wipe_mailbox_contents(&sent_dir)?;
    } else if !yes {
        print!("Delete mailbox '{name}' and all its emails? [y/N] ");
        io::stdout().flush()?;
        let mut input = String::new();
        io::stdin().read_line(&mut input)?;
        if !input.trim().eq_ignore_ascii_case("y") {
            println!("Cancelled.");
            return Ok(());
        }
    }

    // Prefer the UDS path so the daemon hot-swaps Config. The daemon
    // refuses to delete a non-empty mailbox (ERR NONEMPTY); we surface
    // that error verbatim rather than falling back to the direct-edit
    // path, because "daemon says no" is not a socket-missing condition.
    // Fall back to direct edit only when the socket is absent. After a
    // successful `--force` wipe the daemon sees empty directories and
    // succeeds normally.
    match crate::mcp::submit_mailbox_crud_via_daemon(name, false) {
        Ok(()) => {
            println!("{}", term::success(&format!("Mailbox '{name}' deleted.")));
            println!(
                "  Empty inbox/{name}/ and sent/{name}/ directories remain on disk; \
                 run `rmdir` to tidy up if desired."
            );
            Ok(())
        }
        Err(crate::mcp::MailboxCrudFallback::SocketMissing) => {
            delete_mailbox(config, name)?;
            println!("{}", term::success(&format!("Mailbox '{name}' deleted.")));
            print_restart_hint();
            Ok(())
        }
        Err(crate::mcp::MailboxCrudFallback::Daemon(msg)) => Err(msg.into()),
    }
}

/// Recursively remove every entry inside `dir` while leaving `dir` itself
/// in place. This matches the daemon's NONEMPTY check (which is a top-level
/// `read_dir` count). Once the directory is empty, the daemon-side
/// MAILBOX-DELETE succeeds. Missing directory is treated as already-empty
/// (no error). Each entry is removed via `remove_dir_all` (for bundle
/// directories) or `remove_file` (for flat .md files); errors propagate
/// so the caller can surface the failure verbatim.
fn wipe_mailbox_contents(dir: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e.into()),
    };
    for entry in entries.filter_map(|e| e.ok()) {
        let path = entry.path();
        if path.is_dir() {
            std::fs::remove_dir_all(&path)?;
        } else {
            std::fs::remove_file(&path)?;
        }
    }
    Ok(())
}

/// Dispatch table: init system -> the canonical restart command. OpenRC is
/// hard-coded here because there's no neutral abstraction across systemd
/// and OpenRC for the *restart* verb beyond `serve::service`'s existing
/// dispatch tables; keeping it inline keeps the hint readable without
/// threading the full init-system check through more modules.
///
/// Every `InitSystem` variant is matched explicitly so adding a new one
/// (e.g. `Runit`, `S6`) fails to compile until the new arm is supplied.
/// No silent fall-through via `_`.
pub(crate) fn restart_hint_command(init: &crate::serve::service::InitSystem) -> &'static str {
    use crate::serve::service::InitSystem;
    match init {
        InitSystem::Systemd => "sudo systemctl restart aimx",
        InitSystem::OpenRC => "sudo rc-service aimx restart",
        // On an unknown init the systemd wording is a better fallback than
        // saying nothing (systemd is far more common; operator can translate).
        InitSystem::Unknown => "sudo systemctl restart aimx",
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
/// `aimx serve` keeps a swappable `Config` handle: the daemon UDS path
/// (the preferred path) hot-swaps `Config` so the running daemon picks
/// up the new mailbox without a restart. This direct-edit path runs only
/// when the daemon is unreachable (stopped, fresh install). In that
/// case the on-disk `[mailboxes.<name>]` entry is in place but the
/// daemon will not see it until it starts (or restarts), so we print
/// this hint to prevent inbound mail from silently routing to
/// `catchall`.
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
                owner: "aimx-catchall".to_string(),
                hooks: vec![],
                trust: None,
                trusted_senders: None,
            },
        );
        Config {
            domain: "test.com".to_string(),
            data_dir: tmp.to_path_buf(),
            dkim_selector: "aimx".to_string(),
            trust: "none".to_string(),
            trusted_senders: vec![],
            hook_templates: Vec::new(),
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
    fn validate_mailbox_name_rejects_whitespace_and_bad_chars() {
        // S47-3: tighter validation closes a hole where names like
        // "hello world" made it past the old validator and produced an
        // invalid email address when interpolated into `<name>@<domain>`.
        assert!(validate_mailbox_name("hello world").is_err());
        assert!(validate_mailbox_name("a b").is_err());
        assert!(validate_mailbox_name("\ttab").is_err());
        assert!(validate_mailbox_name("..foo").is_err());
        assert!(validate_mailbox_name("").is_err());
        assert!(validate_mailbox_name(".leading").is_err());
        assert!(validate_mailbox_name("trailing.").is_err());
        assert!(validate_mailbox_name("foo/bar").is_err());
        assert!(validate_mailbox_name("foo\\bar").is_err());
        assert!(validate_mailbox_name("foo\0bar").is_err());
        // Uppercase rejected; the class is case-folded.
        assert!(validate_mailbox_name("Alice").is_err());
        // RFC-5322 would allow `+` in the local part (Gmail plus-addressing
        // etc.) but we keep the class tight to prevent surprises further
        // downstream.
        assert!(validate_mailbox_name("alice+bob").is_err());
    }

    #[test]
    fn validate_mailbox_name_accepts_safe_names() {
        assert!(validate_mailbox_name("good-mailbox.1").is_ok());
        assert!(validate_mailbox_name("catchall").is_ok());
        assert!(validate_mailbox_name("alice").is_ok());
        assert!(validate_mailbox_name("a").is_ok());
        assert!(validate_mailbox_name("a.b_c-1").is_ok());
    }

    #[test]
    fn create_new_mailbox() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(tmp.path());
        let _guard = setup_config_file(tmp.path(), &config);

        create_mailbox(&config, "alice").unwrap();

        // Both `inbox/<name>/` and `sent/<name>/` exist.
        assert!(tmp.path().join("inbox").join("alice").is_dir());
        assert!(tmp.path().join("sent").join("alice").is_dir());
        let reloaded = Config::load_resolved_ignore_warnings().unwrap();
        assert!(reloaded.mailboxes.contains_key("alice"));
        assert_eq!(reloaded.mailboxes["alice"].address, "alice@test.com");
    }

    #[test]
    fn create_mailbox_is_idempotent_for_dirs_when_config_race_prevented() {
        // If the config-registration side-steps the duplicate check, the
        // create_dir_all calls are idempotent. This is an internal contract
        // test; callers should rely on `create_mailbox` itself to fail
        // duplicate registrations via the HashMap check.
        let tmp = TempDir::new().unwrap();
        let config = test_config(tmp.path());
        let _guard = setup_config_file(tmp.path(), &config);

        create_mailbox(&config, "alice").unwrap();
        // Re-creating via a fresh Config (as if registration rolled back)
        // must not error; dir creation is idempotent.
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
        // `mailbox_list` must scan `inbox/*/` so an inbox directory left
        // by a backup restore (or an unregistered mailbox) still appears
        // in the listing.
        let tmp = TempDir::new().unwrap();
        let config = test_config(tmp.path());
        let _guard = setup_config_file(tmp.path(), &config);

        // Registered mailbox: catchall + an `alice` we register.
        create_mailbox(&config, "alice").unwrap();
        let config = Config::load_resolved_ignore_warnings().unwrap();
        let inbox_alice = tmp.path().join("inbox").join("alice");
        std::fs::write(inbox_alice.join("2025-01-01-120000-a.md"), "x").unwrap();

        // Stray dir created out-of-band; no config entry.
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
        let config = Config::load_resolved_ignore_warnings().unwrap();
        assert!(config.mailboxes.contains_key("alice"));

        delete_mailbox(&config, "alice").unwrap();

        // Both inbox and sent directories must be gone.
        assert!(!tmp.path().join("inbox").join("alice").exists());
        assert!(!tmp.path().join("sent").join("alice").exists());
        let reloaded = Config::load_resolved_ignore_warnings().unwrap();
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
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("invalid character")
        );
    }

    #[test]
    fn create_with_backslash_fails() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(tmp.path());
        let _guard = setup_config_file(tmp.path(), &config);

        let result = create_mailbox(&config, "foo\\bar");
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("invalid character")
        );
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
        // Better to print systemd wording than to say nothing; operators on
        // niche init systems can translate once.
        assert_eq!(
            restart_hint_command(&crate::serve::service::InitSystem::Unknown),
            "sudo systemctl restart aimx"
        );
    }

    #[test]
    fn restart_hint_command_is_exhaustive_over_init_system() {
        // The `match` inside `restart_hint_command` is exhaustive: every
        // `InitSystem` variant has an explicit arm (no `_` fall-through). This
        // test destructures every current variant so adding a new variant
        // without touching this function would fail the exhaustive pattern
        // check below at compile time. If you're here because this stopped
        // compiling, you probably just added a new `InitSystem` variant; add
        // its arm to `restart_hint_command` (and extend the assertions below).
        use crate::serve::service::InitSystem;
        let all = [InitSystem::Systemd, InitSystem::OpenRC, InitSystem::Unknown];
        for variant in &all {
            // Force an explicit destructure; the `_` catch-all is forbidden.
            let expected: &'static str = match variant {
                InitSystem::Systemd => "sudo systemctl restart aimx",
                InitSystem::OpenRC => "sudo rc-service aimx restart",
                InitSystem::Unknown => "sudo systemctl restart aimx",
            };
            assert_eq!(restart_hint_command(variant), expected, "{variant:?}");
        }
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

    // ----- S48-5 wipe_mailbox_contents helper --------------------------

    #[test]
    fn wipe_mailbox_contents_empties_flat_and_bundle_entries() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("inbox").join("eve");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("2025-04-01-120000-flat.md"), "hi").unwrap();
        let bundle = dir.join("2025-04-01-130000-bundle");
        std::fs::create_dir_all(&bundle).unwrap();
        std::fs::write(bundle.join("2025-04-01-130000-bundle.md"), "hi").unwrap();
        std::fs::write(bundle.join("att.txt"), "data").unwrap();

        super::wipe_mailbox_contents(&dir).unwrap();

        // The mailbox directory itself remains, but it is now empty.
        assert!(
            dir.is_dir(),
            "wipe must leave the parent directory in place"
        );
        let remaining: Vec<_> = std::fs::read_dir(&dir).unwrap().collect();
        assert!(remaining.is_empty(), "all entries must be removed");
    }

    #[test]
    fn wipe_mailbox_contents_missing_dir_is_noop() {
        // Force-deleting a mailbox that never had a sent/ subdirectory must
        // not error; treat missing directory as already-empty.
        let tmp = TempDir::new().unwrap();
        super::wipe_mailbox_contents(&tmp.path().join("does-not-exist")).unwrap();
    }

    // ----- S51-1 mailboxes show ----------------------------------------

    fn show_test_config(tmp: &Path) -> Config {
        let mut mailboxes = HashMap::new();
        mailboxes.insert(
            "catchall".to_string(),
            MailboxConfig {
                address: "*@test.com".to_string(),
                owner: "aimx-catchall".to_string(),
                hooks: vec![],
                trust: None,
                trusted_senders: None,
            },
        );
        mailboxes.insert(
            "support".to_string(),
            MailboxConfig {
                address: "support@test.com".to_string(),
                owner: "root".to_string(),
                hooks: vec![
                    crate::hook::Hook {
                        name: Some("inbound_urgent".into()),
                        event: crate::hook::HookEvent::OnReceive,
                        r#type: "cmd".into(),
                        cmd: "echo $AIMX_FROM >> /tmp/log".into(),
                        dangerously_support_untrusted: true,
                        origin: crate::hook::HookOrigin::Operator,
                        template: None,
                        params: std::collections::BTreeMap::new(),
                        run_as: None,
                    },
                    crate::hook::Hook {
                        name: Some("outbound_notify".into()),
                        event: crate::hook::HookEvent::AfterSend,
                        r#type: "cmd".into(),
                        cmd: "curl https://webhook.example.com/notify".into(),
                        dangerously_support_untrusted: false,
                        origin: crate::hook::HookOrigin::Operator,
                        template: None,
                        params: std::collections::BTreeMap::new(),
                        run_as: None,
                    },
                ],
                trust: Some("verified".into()),
                trusted_senders: Some(vec!["*@company.com".into(), "boss@example.com".into()]),
            },
        );
        Config {
            domain: "test.com".to_string(),
            data_dir: tmp.to_path_buf(),
            dkim_selector: "aimx".to_string(),
            trust: "none".to_string(),
            trusted_senders: vec![],
            hook_templates: Vec::new(),
            mailboxes,
            verify_host: None,
            enable_ipv6: false,
        }
    }

    #[test]
    fn show_unknown_mailbox_errors() {
        let tmp = TempDir::new().unwrap();
        let config = show_test_config(tmp.path());
        let err = super::build_show_lines(&config, "ghost").unwrap_err();
        assert!(err.to_string().contains("does not exist"));
    }

    #[test]
    fn show_renders_trust_senders_hooks_and_counts() {
        let tmp = TempDir::new().unwrap();
        let config = show_test_config(tmp.path());

        // Drop a couple of inbox entries (one unread, one read) so the
        // counts assertion has content.
        let inbox = tmp.path().join("inbox").join("support");
        std::fs::create_dir_all(&inbox).unwrap();
        let frontmatter = |read: bool| -> String {
            format!(
                "+++\nid = \"2025-06-01-001\"\n\
                 message_id = \"<m@x>\"\n\
                 thread_id = \"0123456789abcdef\"\n\
                 from = \"s@x\"\n\
                 to = \"support@test.com\"\n\
                 delivered_to = \"support@test.com\"\n\
                 subject = \"hi\"\n\
                 date = \"2025-06-01T00:00:00Z\"\n\
                 received_at = \"2025-06-01T00:00:01Z\"\n\
                 size_bytes = 10\n\
                 dkim = \"none\"\n\
                 spf = \"none\"\n\
                 dmarc = \"none\"\n\
                 trusted = \"none\"\n\
                 mailbox = \"support\"\n\
                 read = {read}\n\
                 +++\n\nbody\n"
            )
        };
        std::fs::write(inbox.join("2025-06-01-001.md"), frontmatter(false)).unwrap();
        std::fs::write(inbox.join("2025-06-02-001.md"), frontmatter(true)).unwrap();

        let lines = super::build_show_lines(&config, "support").unwrap();
        let joined = strip_ansi(&lines.join("\n"));

        assert!(joined.contains("support@test.com"), "{joined}");
        // Effective trust is the per-mailbox override.
        assert!(joined.contains("verified"), "{joined}");
        // Both trusted-sender entries appear verbatim.
        assert!(joined.contains("*@company.com"), "{joined}");
        assert!(joined.contains("boss@example.com"), "{joined}");
        // Both hook names appear.
        assert!(joined.contains("inbound_urgent"), "{joined}");
        assert!(joined.contains("outbound_notify"), "{joined}");
        // Each event has its section header.
        assert!(joined.contains("on_receive"), "{joined}");
        assert!(joined.contains("after_send"), "{joined}");
        // The dangerous opt-in is surfaced.
        assert!(
            joined.contains("dangerously_support_untrusted=true"),
            "{joined}"
        );
        // Inbox count + unread + sent count all present.
        assert!(
            joined.contains("inbox:") && joined.contains("2"),
            "{joined}"
        );
        assert!(joined.contains("1 unread"), "{joined}");
        assert!(joined.contains("sent:"), "{joined}");
    }

    #[test]
    fn show_renders_empty_hooks_and_senders_as_none_placeholder() {
        let tmp = TempDir::new().unwrap();
        let config = show_test_config(tmp.path());
        let lines = super::build_show_lines(&config, "catchall").unwrap();
        let joined = strip_ansi(&lines.join("\n"));
        assert!(joined.contains("*@test.com"));
        assert!(
            joined.contains("(none)"),
            "expected (none) placeholder: {joined}"
        );
    }

    #[test]
    fn show_truncates_long_cmd_with_ellipsis() {
        let tmp = TempDir::new().unwrap();
        let mut config = show_test_config(tmp.path());
        let long_cmd = "x".repeat(120);
        config
            .mailboxes
            .get_mut("support")
            .unwrap()
            .hooks
            .push(crate::hook::Hook {
                name: Some("long_cmd".into()),
                event: crate::hook::HookEvent::OnReceive,
                r#type: "cmd".into(),
                cmd: long_cmd.clone(),
                dangerously_support_untrusted: false,
                origin: crate::hook::HookOrigin::Operator,
                template: None,
                params: std::collections::BTreeMap::new(),
                run_as: None,
            });
        let lines = super::build_show_lines(&config, "support").unwrap();
        let joined = strip_ansi(&lines.join("\n"));
        assert!(
            joined.contains("…"),
            "expected ellipsis truncation marker: {joined}"
        );
        // The full 120-char cmd must NOT appear verbatim.
        assert!(
            !joined.contains(long_cmd.as_str()),
            "overlong cmd should be truncated"
        );
    }
}
