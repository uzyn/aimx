use crate::cli::MailboxCommand;
use crate::config::{Config, MailboxConfig};
use crate::term;
use std::io::{self, Write};
use std::path::Path;

pub fn run(cmd: MailboxCommand, config: Config) -> Result<(), Box<dyn std::error::Error>> {
    match cmd {
        MailboxCommand::Create { name, owner } => create(&config, &name, owner.as_deref()),
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
    // Reserved names: `catchall` is the runtime-special wildcard mailbox
    // identifier and `aimx-catchall` is the reserved system user that
    // owns it on a default install. Either as a user-defined mailbox
    // name would shadow the wildcard slot or collide with the system
    // user — reject regardless of caller (CLI or UDS).
    if name == "catchall" || name == "aimx-catchall" {
        return Err(format!(
            "Mailbox name '{name}' is reserved for the wildcard catchall slot"
        ));
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

pub fn create_mailbox(
    config: &Config,
    name: &str,
    owner: &str,
) -> Result<(), Box<dyn std::error::Error>> {
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

    let new_mb = MailboxConfig {
        address: format!("{name}@{}", config.domain),
        owner: owner.to_string(),
        hooks: vec![],
        trust: None,
        trusted_senders: None,
        allow_root_catchall: false,
    };

    // Chown to `<owner>:<owner> 0700` so the dir layout matches
    // the daemon-created path (mailbox_handler.rs). Only attempt the
    // chown when running as root — the CLI fallback path is invoked
    // with the daemon stopped, which on a real install means the
    // operator ran `sudo aimx mailboxes create`. Non-root callers fall
    // through to chmod-only; the daemon will fix up perms on next boot
    // via `ensure_mailbox_dirs` in `finalize_setup`.
    if crate::platform::is_root() {
        for dir in [&inbox, &sent] {
            if let Err(e) = crate::ownership::chown_as_owner(dir, &new_mb, 0o700) {
                let _ = std::fs::remove_dir_all(&inbox);
                let _ = std::fs::remove_dir_all(&sent);
                return Err(format!("failed to chown {}: {e}", dir.display()).into());
            }
        }
    } else {
        use std::os::unix::fs::PermissionsExt;
        for dir in [&inbox, &sent] {
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
        }
    }

    let mut config = config.clone();
    config.mailboxes.insert(name.to_string(), new_mb);

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

    // Save-then-delete ordering.
    //
    // 1. Clone the config and drop the mailbox entry in memory.
    // 2. Persist the new config atomically (temp-then-rename). If this
    //    fails, the data dirs on disk are untouched, so the operator can
    //    retry without first resurrecting the mailbox files.
    // 3. Only after the save succeeds do we `remove_dir_all` the inbox
    //    and sent dirs. If step 3 fails the config is already authoritative;
    //    warn the operator by name+error so they can clean up the
    //    leftover directories, and propagate the error so the CLI exits
    //    non-zero.
    let mut new_config = config.clone();
    new_config.mailboxes.remove(name);
    new_config.save(&crate::config::config_path())?;

    let inbox = config.inbox_dir(name);
    let sent = config.sent_dir(name);
    let mut leftovers: Vec<String> = Vec::new();
    if inbox.exists()
        && let Err(e) = std::fs::remove_dir_all(&inbox)
    {
        tracing::warn!(
            path = %inbox.display(),
            error = %e,
            "mailbox '{name}' config removed but inbox dir cleanup failed; \
             remove manually to reclaim space",
        );
        leftovers.push(format!("  - {}: {e}", inbox.display()));
    }
    if sent.exists()
        && let Err(e) = std::fs::remove_dir_all(&sent)
    {
        tracing::warn!(
            path = %sent.display(),
            error = %e,
            "mailbox '{name}' config removed but sent dir cleanup failed; \
             remove manually to reclaim space",
        );
        leftovers.push(format!("  - {}: {e}", sent.display()));
    }

    if !leftovers.is_empty() {
        return Err(format!(
            "mailbox '{name}' removed from config.toml but filesystem cleanup failed:\n{}",
            leftovers.join("\n"),
        )
        .into());
    }

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

fn create(
    config: &Config,
    name: &str,
    owner: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    // Resolve the effective owner: explicit `--owner`, else fall back
    // to the shared `prompt_mailbox_owner` seam. Under a TTY
    // the operator sees a prompt defaulted to the local part; under
    // `AIMX_NONINTERACTIVE=1` the helper accepts that default when it
    // resolves or errors hard so scripted runs fail fast instead of
    // blocking on stdin.
    let sys = crate::setup::RealSystemOps;
    let owner = resolve_create_owner(config, name, owner, &sys)?;
    // Try the UDS path first so the daemon hot-swaps its in-memory
    // Config. On socket-missing (daemon stopped, fresh install), fall
    // back to direct on-disk edit + the restart-hint banner. When UDS
    // succeeds we suppress the hint; the daemon already picked up the
    // change.
    match crate::mcp::submit_mailbox_crud_via_daemon(name, true, Some(&owner)) {
        Ok(()) => {
            println!(
                "{}",
                term::success(&format!("Mailbox '{name}' created (owner: {owner})."))
            );
            Ok(())
        }
        Err(crate::mcp::MailboxCrudFallback::SocketMissing) => {
            create_mailbox(config, name, &owner)?;
            println!(
                "{}",
                term::success(&format!("Mailbox '{name}' created (owner: {owner})."))
            );
            print_restart_hint();
            Ok(())
        }
        Err(crate::mcp::MailboxCrudFallback::Daemon(msg)) => Err(msg.into()),
    }
}

/// Resolve the owner value for `mailbox create`. Explicit `--owner`
/// wins; otherwise the shared `setup::prompt_mailbox_owner` seam is
/// invoked so operators get the same default-and-prompt UX here as in
/// the setup wizard (PRD §6.8). Under `AIMX_NONINTERACTIVE=1` the
/// helper accepts the local-part default when that user resolves via
/// `getpwnam`, or errors hard with an actionable `useradd` hint so
/// scripted callers fail fast rather than blocking on stdin.
fn resolve_create_owner(
    config: &Config,
    name: &str,
    explicit: Option<&str>,
    sys: &dyn crate::setup::SystemOps,
) -> Result<String, Box<dyn std::error::Error>> {
    if let Some(o) = explicit {
        if o.is_empty() {
            return Err("--owner value cannot be empty".into());
        }
        return Ok(o.to_string());
    }
    let address = format!("{name}@{domain}", domain = config.domain);
    crate::setup::prompt_mailbox_owner(&address, sys)
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
        let cmd_display = serde_json::to_string(&h.cmd).unwrap_or_else(|_| h.cmd.join(" "));
        let cmd = truncate_show_cmd(&cmd_display, 60);
        let name = crate::hook::effective_hook_name(h);
        let suffix = if h.fire_on_untrusted {
            "   [fire_on_untrusted=true]"
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

fn truncate_show_cmd(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
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
    // `--force` wipes `inbox/<name>/` and `sent/<name>/` contents
    // before invoking the normal delete path. The wipe is CLI-only
    // (the MCP `mailbox_delete` tool does not gain a force variant)
    // and refuses on the catchall.
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
    match crate::mcp::submit_mailbox_crud_via_daemon(name, false, None) {
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
