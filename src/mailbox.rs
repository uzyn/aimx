use crate::cli::MailboxCommand;
use crate::config::{Config, MailboxConfig};
use crate::platform::{current_euid, is_root};
use crate::term;
use std::io::{self, Write};
use std::path::Path;

/// Exit code emitted when a non-root caller can't reach the daemon UDS.
/// Mirrors `send::EXIT_CONNECT` so tooling treats both socket-missing
/// failures uniformly.
pub(crate) const EXIT_SOCKET_MISSING: i32 = 2;

/// Stderr message printed before exiting with [`EXIT_SOCKET_MISSING`].
/// Lifted to a constant so the integration test can match it verbatim.
pub(crate) const SOCKET_MISSING_HINT: &str = "daemon must be running for non-root mailbox CRUD; start `aimx serve` \
     or run with sudo to fall back to direct config edit.";

pub fn run(cmd: MailboxCommand, data_dir: Option<&Path>) -> Result<(), Box<dyn std::error::Error>> {
    // S2-1: the entry-point root gate is gone. Both CREATE and DELETE
    // route through the daemon UDS regardless of caller uid; the daemon
    // (post-Sprint-1) does the authz, owner-binding, atomic config
    // write, and `Arc<Config>` hot-swap. The CLI is now a thin client.
    // Root callers retain the direct-on-disk fallback when the socket
    // is absent; non-root callers fail fast with a precise hint.
    //
    // Config loading happens here (not in `dispatch`) so a non-root
    // caller — who cannot read `0640 root:root /etc/aimx/config.toml`
    // in production — can still reach `create` / `delete` and route
    // through the daemon UDS. Each subcommand decides for itself
    // whether the missing-config case is recoverable.
    let loaded = load_config_optional(data_dir);

    match cmd {
        MailboxCommand::Create { name, owner } => create(loaded.as_ref(), &name, owner.as_deref()),
        MailboxCommand::List { all } => list_dispatch(loaded.as_ref(), all),
        MailboxCommand::Show { name } => show(&require_config(loaded)?, &name),
        MailboxCommand::Delete { name, yes, force } => delete(loaded.as_ref(), &name, yes, force),
    }
}

/// Best-effort config load that distinguishes "config genuinely
/// missing or unreadable for this caller" from "loaded fine, here
/// you go". Returns `None` whenever the load failed for any reason
/// (EACCES on the root-owned `/etc/aimx/config.toml`, ENOENT on a
/// fresh install before `aimx setup` has run, parse error, …); the
/// caller decides whether the missing-config case is recoverable.
///
/// The previous shape — `dispatch()` `?`-propagating
/// `Config::load_resolved_with_data_dir(...)` — broke `aimx mailboxes
/// {create,delete,list}` for non-root callers in production because
/// the read EACCES surfaced as a bare `Permission denied (os error
/// 13)` before `mailbox::run` ever saw the request. With config
/// optional here, the create / delete paths run through the daemon
/// UDS (where `SO_PEERCRED` is the authoritative identity) without
/// ever needing to read the root-owned config from a non-root
/// process.
pub(crate) fn load_config_optional(data_dir: Option<&Path>) -> Option<Config> {
    crate::config::Config::load_resolved_with_data_dir(data_dir)
        .map(|(cfg, _warnings)| cfg)
        .ok()
}

/// Subcommands that genuinely need the local config (today: `show`,
/// which renders trust + hook details that aren't surfaced through
/// any UDS verb). Returns a friendly actionable error rather than the
/// raw `Permission denied (os error 13)`.
fn require_config(loaded: Option<Config>) -> Result<Config, Box<dyn std::error::Error>> {
    loaded.ok_or_else(|| -> Box<dyn std::error::Error> {
        format!(
            "this command needs to read {} (root-owned). \
             Re-run with sudo, or use a UDS-backed alternative.",
            crate::config::config_path().display()
        )
        .into()
    })
}

/// `mailboxes list` dispatcher. Root with config readable falls
/// through to the local `list()` implementation (which can show every
/// mailbox + the `--all` switch). Non-root callers — and root callers
/// without a readable config — route through the daemon's
/// `MAILBOX-LIST` verb so the listing reflects the daemon's
/// SO_PEERCRED-based view of what the caller owns.
fn list_dispatch(loaded: Option<&Config>, all: bool) -> Result<(), Box<dyn std::error::Error>> {
    if let Some(cfg) = loaded {
        return list(cfg, all);
    }
    if all {
        return Err("not authorized: --all requires root (run with sudo)".into());
    }
    list_via_daemon()
}

/// Render the daemon's `MAILBOX-LIST` JSON response in the same
/// columnar format as `list()`. Used as the non-root fallback when
/// `/etc/aimx/config.toml` is unreadable. Errors from the UDS layer
/// (socket missing, daemon stopped) bubble up verbatim — same hint
/// the `create` / `delete` paths produce.
fn list_via_daemon() -> Result<(), Box<dyn std::error::Error>> {
    let json = match crate::mcp::submit_mailbox_list_via_daemon_for_cli() {
        Ok(s) => s,
        Err(crate::mcp::MailboxLifecycleFallback::SocketMissing) => {
            exit_socket_missing();
        }
        Err(crate::mcp::MailboxLifecycleFallback::Daemon(msg)) => {
            return Err(msg.into());
        }
    };

    let rows: Vec<crate::mailbox_list_handler::MailboxListRow> =
        serde_json::from_str(&json).map_err(|e| format!("malformed MAILBOX-LIST response: {e}"))?;

    if rows.is_empty() {
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
    for row in rows {
        let name_pad = 20usize.saturating_sub(row.name.chars().count());
        let suffix = if row.registered {
            String::new()
        } else {
            format!(" {}", term::warn("(unregistered)"))
        };
        println!(
            "{}{:pad$} {:<8} {}{}",
            term::highlight(&row.name),
            "",
            row.total,
            row.sent_count,
            suffix,
            pad = name_pad,
        );
    }

    Ok(())
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
    config: Option<&Config>,
    name: &str,
    owner: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    // Resolve the effective owner. Per the PRD's UX contract:
    //
    //   - Non-root callers: the daemon discards any wire-supplied
    //     owner and synthesizes from `peer_username(SO_PEERCRED)`.
    //     Prompting is therefore actively harmful (Non-blocker 5 from
    //     the Cycle 1 review): under non-TTY stdin the prompt loops
    //     5 times before erroring on a value the daemon would have
    //     ignored anyway. Skip the prompt and use the caller's
    //     own username so the local display string is honest.
    //   - Root callers: the daemon honors `Owner:` so the existing
    //     prompt-with-default UX from the setup wizard runs through
    //     the shared `prompt_mailbox_owner` seam.
    let sys = crate::setup::RealSystemOps;
    let owner = if !is_root() {
        match owner {
            Some(o) if !o.is_empty() => o.to_string(),
            Some(_) => return Err("--owner value cannot be empty".into()),
            None => {
                let caller = caller_username();
                if caller.is_empty() {
                    return Err("could not resolve caller's username via getpwuid; \
                         pass --owner <user> explicitly"
                        .into());
                }
                caller
            }
        }
    } else {
        // Root path: needs the domain for the prompt's display
        // address; `prompt_mailbox_owner` errors gracefully when the
        // operator's input doesn't resolve via getpwnam.
        let cfg = config.ok_or_else(|| -> Box<dyn std::error::Error> {
            format!(
                "this command needs to read {} (root-owned). \
                 The config could not be loaded.",
                crate::config::config_path().display()
            )
            .into()
        })?;
        resolve_create_owner(cfg, name, owner, &sys)?
    };

    // Effective owner: what the daemon will actually bind on disk.
    // For non-root callers the daemon discards the wire-supplied value
    // and synthesizes from SO_PEERCRED, so the honest display value is
    // the caller's username (when resolvable). Both the S2-1
    // soft-warning and the success line below read this so they agree.
    let effective_owner = if !is_root() {
        let caller = caller_username();
        if caller.is_empty() {
            owner.clone()
        } else {
            caller
        }
    } else {
        owner.clone()
    };

    // S2-1 soft-warning: a non-root caller who passed `--owner <other>`
    // gets a stderr line clarifying that the daemon will discard the
    // value and bind ownership to the caller's uid via SO_PEERCRED.
    // This is purely UX; the daemon enforces the structural invariant
    // server-side either way.
    if !is_root() && !effective_owner.is_empty() && owner != effective_owner {
        eprintln!(
            "{} --owner ignored for non-root callers; mailbox will be owned by `{effective_owner}`",
            term::warn("Warning:"),
        );
    }

    // Try the UDS path first so the daemon hot-swaps its in-memory
    // Config. On socket-missing (daemon stopped, fresh install), root
    // falls back to direct on-disk edit + the restart-hint banner;
    // non-root cannot rename `/etc/aimx/config.toml` (perm `0640
    // root:root`), so we exit 2 with a precise actionable error.
    match crate::mcp::submit_mailbox_crud_via_daemon(name, true, Some(&owner), false) {
        Ok(()) => {
            println!(
                "{}",
                term::success(&format!(
                    "Mailbox '{name}' created (owner: {effective_owner})."
                ))
            );
            Ok(())
        }
        Err(crate::mcp::MailboxLifecycleFallback::SocketMissing) => {
            if !is_root() {
                exit_socket_missing();
            }
            // Root fallback path: needs the local config to perform
            // the direct on-disk write.
            let cfg = config.ok_or_else(|| -> Box<dyn std::error::Error> {
                format!(
                    "config could not be loaded from {}; \
                     daemon is also unreachable",
                    crate::config::config_path().display()
                )
                .into()
            })?;
            create_mailbox(cfg, name, &owner)?;
            println!(
                "{}",
                term::success(&format!(
                    "Mailbox '{name}' created (owner: {effective_owner})."
                ))
            );
            print_restart_hint();
            Ok(())
        }
        Err(crate::mcp::MailboxLifecycleFallback::Daemon(msg)) => Err(msg.into()),
    }
}

/// Return the current Linux user's name, or an empty string when the
/// uid does not resolve via `getpwuid`. Used by the soft-warning path
/// in `create()` — never panics, never errors; an empty result simply
/// suppresses the warning rather than confusing it.
fn caller_username() -> String {
    crate::uds_authz::lookup_username(current_euid()).unwrap_or_default()
}

/// Print [`SOCKET_MISSING_HINT`] to stderr and exit with
/// [`EXIT_SOCKET_MISSING`]. Centralised so the create / delete paths
/// stay consistent.
fn exit_socket_missing() -> ! {
    eprintln!("{} {SOCKET_MISSING_HINT}", term::error("Error:"));
    std::process::exit(EXIT_SOCKET_MISSING);
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

fn list(config: &Config, all: bool) -> Result<(), Box<dyn std::error::Error>> {
    let euid = current_euid();
    let caller_is_root = is_root();

    if all && !caller_is_root {
        return Err("not authorized: --all requires root (run with sudo)".into());
    }

    // Root sees everything by default; `--all` is a no-op for root.
    // Non-root sees only mailboxes whose `owner_uid()` matches euid.
    // Mailboxes whose owner field doesn't resolve via getpwnam are
    // hidden from non-root callers entirely (they cannot be owned by
    // anyone visible to the caller, so revealing their existence would
    // leak operator-side configuration).
    let show_all = caller_is_root || all;
    let all_mailboxes = list_mailboxes(config);
    let mailboxes: Vec<(String, usize, usize)> = if show_all {
        all_mailboxes
    } else {
        all_mailboxes
            .into_iter()
            .filter(|(name, _, _)| caller_owns(config, name, euid))
            .collect()
    };

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

/// Returns `true` when the named mailbox is configured and its
/// `owner_uid()` resolves to `caller_euid`. Filesystem-only mailboxes
/// (orphans without a config row) and mailboxes whose owner cannot be
/// resolved both return `false` — non-root callers should not see them.
pub(crate) fn caller_owns(config: &Config, name: &str, caller_euid: u32) -> bool {
    let Some(mb) = config.mailboxes.get(name) else {
        return false;
    };
    matches!(mb.owner_uid(), Ok(uid) if uid == caller_euid)
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
    config: Option<&Config>,
    name: &str,
    yes: bool,
    force: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    // `--force` asks the daemon to wipe `inbox/<name>/` and
    // `sent/<name>/` contents under the per-mailbox lock that guards
    // the stanza removal — the wipe and the config rewrite are atomic
    // together (no data-destruction race window if the daemon dies
    // mid-flight). Refuse the catchall up front so the operator gets
    // a friendly error before we even open the UDS.
    if force && name == "catchall" {
        return Err("Cannot delete the catchall mailbox".into());
    }

    if force {
        // Show the pre-wipe counts so the operator knows exactly how
        // much data is about to be destroyed. The counts are
        // best-effort (taken before the daemon acquires its lock, so
        // they may drift if mail lands in the meantime); the daemon
        // is authoritative on what actually gets wiped. When config
        // is unreadable (non-root, root-owned config.toml) the count
        // line just shows `?`.
        let (inbox_label, sent_label) = match config {
            Some(cfg) => {
                let inbox_dir = cfg.inbox_dir(name);
                let sent_dir = cfg.sent_dir(name);
                (
                    pluralize_files(count_messages(&inbox_dir)),
                    pluralize_files(count_messages(&sent_dir)),
                )
            }
            None => ("? files".to_string(), "? files".to_string()),
        };

        if !yes {
            println!(
                "{} About to permanently delete mailbox '{name}':",
                term::warn("DESTRUCTIVE:"),
            );
            println!("  inbox/{name}/: {inbox_label}");
            println!("  sent/{name}/:  {sent_label}");
            print!("Continue? [y/N] ");
            io::stdout().flush()?;
            let mut input = String::new();
            io::stdin().read_line(&mut input)?;
            if !input.trim().eq_ignore_ascii_case("y") {
                println!("Cancelled.");
                return Ok(());
            }
        }
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
    // refuses to delete a non-empty mailbox (ERR NONEMPTY) unless
    // `force=true` is set, in which case it wipes the directories
    // server-side under the per-mailbox lock before unlinking the
    // stanza. Fall back to direct edit only when the socket is absent
    // and the caller is root (non-root cannot rename
    // `/etc/aimx/config.toml`).
    match crate::mcp::submit_mailbox_crud_via_daemon(name, false, None, force) {
        Ok(()) => {
            println!("{}", term::success(&format!("Mailbox '{name}' deleted.")));
            println!(
                "  Empty inbox/{name}/ and sent/{name}/ directories remain on disk; \
                 run `rmdir` to tidy up if desired."
            );
            Ok(())
        }
        Err(crate::mcp::MailboxLifecycleFallback::SocketMissing) => {
            if !is_root() {
                exit_socket_missing();
            }
            // Root fallback: wipe locally then call delete_mailbox
            // directly. This path runs only when the daemon is
            // stopped — no concurrent access to worry about.
            let cfg = config.ok_or_else(|| -> Box<dyn std::error::Error> {
                format!(
                    "config could not be loaded from {}; \
                     daemon is also unreachable",
                    crate::config::config_path().display()
                )
                .into()
            })?;
            if force {
                let inbox_dir = cfg.inbox_dir(name);
                let sent_dir = cfg.sent_dir(name);
                wipe_mailbox_contents(&inbox_dir)?;
                wipe_mailbox_contents(&sent_dir)?;
            }
            delete_mailbox(cfg, name)?;
            println!("{}", term::success(&format!("Mailbox '{name}' deleted.")));
            print_restart_hint();
            Ok(())
        }
        Err(crate::mcp::MailboxLifecycleFallback::Daemon(msg)) => Err(msg.into()),
    }
}

/// Recursively remove every entry inside `dir` while leaving `dir` itself
/// in place. This matches the daemon's NONEMPTY check (which is a top-level
/// `read_dir` count). Once the directory is empty, the daemon-side
/// MAILBOX-DELETE succeeds. Missing directory is treated as already-empty
/// (no error). Each entry is removed via `remove_dir_all` (for bundle
/// directories) or `remove_file` (for flat .md files); errors propagate
/// so the caller can surface the failure verbatim.
///
/// `pub(crate)` so the MCP `mailbox_delete` tool can reuse the same
/// wipe contract — keeping a single implementation prevents the CLI
/// and MCP paths from drifting on edge cases like dotfile preservation
/// or symlink handling.
pub(crate) fn wipe_mailbox_contents(dir: &Path) -> Result<(), Box<dyn std::error::Error>> {
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
    use crate::auth::{AuthError, AuthErrorContext, format_auth_error};

    /// Mailbox-CLI rendering context: mirrors what the production caller
    /// would pass once the per-surface helpers are gone. Sprint 3 (S3-5)
    /// folded the local `format_auth_error` definition into `auth.rs`;
    /// these tests now exercise the canonical renderer with the
    /// mailbox-CLI-shaped context, so any wording drift surfaces as a
    /// test failure here rather than going silently.
    fn mailbox_ctx<'a>(verb: &'a str) -> AuthErrorContext<'a> {
        AuthErrorContext {
            surface: Some("aimx mailboxes"),
            verb: Some(verb),
            resource: Some("mailbox"),
            ..Default::default()
        }
    }

    #[test]
    fn format_auth_error_not_owner_carries_mailbox_name() {
        let msg = format_auth_error(
            &AuthError::NotOwner {
                mailbox: "alice".into(),
            },
            &mailbox_ctx("delete"),
        );
        assert!(msg.contains("not authorized"), "{msg}");
        assert!(msg.contains("alice"), "{msg}");
    }

    #[test]
    fn format_auth_error_owner_mismatch_renders_cleanly() {
        // S2-1: the legacy `<new>` sentinel rendering is gone. The new
        // arm reads as a single user-friendly sentence and uses the
        // verb supplied by the caller (`create` here) so the message
        // matches the command the operator just ran.
        let msg = format_auth_error(
            &AuthError::OwnerMismatch {
                intended_owner_uid: 0,
            },
            &mailbox_ctx("create"),
        );
        assert!(msg.contains("not authorized"), "{msg}");
        assert!(msg.contains("cannot create"), "{msg}");
        assert!(
            !msg.contains("'<new>'"),
            "must not surface the Sprint-1 sentinel: {msg}"
        );
        assert!(!msg.contains('0'), "uid must not leak: {msg}");
    }

    #[test]
    fn format_auth_error_no_such_mailbox_does_not_leak_caller() {
        let msg = format_auth_error(&AuthError::NoSuchMailbox, &mailbox_ctx("create"));
        assert!(msg.contains("not authorized"), "{msg}");
        assert!(msg.contains("no such mailbox"), "{msg}");
    }

    #[test]
    fn format_auth_error_not_root_arm_still_renders() {
        // S2-1 dropped the entry-point root gate, so this arm is no
        // longer reachable from the mailbox CLI dispatch — but the
        // match must stay exhaustive. The render itself stays
        // grep-able for any future caller.
        let msg = format_auth_error(&AuthError::NotRoot, &mailbox_ctx("create"));
        assert!(msg.contains("not authorized"), "{msg}");
        assert!(msg.contains("requires root"), "{msg}");
    }

    fn config_with_owners(owners: &[(&str, &str)]) -> Config {
        let mut mailboxes = std::collections::HashMap::new();
        for (name, owner) in owners {
            mailboxes.insert(
                (*name).into(),
                MailboxConfig {
                    address: format!("{name}@agent.example.com"),
                    owner: (*owner).into(),
                    hooks: vec![],
                    trust: None,
                    trusted_senders: None,
                    allow_root_catchall: false,
                },
            );
        }
        Config {
            domain: "agent.example.com".into(),
            data_dir: std::path::PathBuf::from("/tmp/test"),
            dkim_selector: "aimx".into(),
            trust: "none".into(),
            trusted_senders: vec![],
            mailboxes,
            verify_host: None,
            enable_ipv6: false,
            signature: None,
            upgrade: None,
        }
    }

    #[test]
    fn caller_owns_returns_false_for_unknown_mailbox() {
        let cfg = config_with_owners(&[]);
        assert!(!caller_owns(&cfg, "missing", 1000));
    }

    #[test]
    fn caller_owns_returns_false_for_orphan_owner() {
        let cfg = config_with_owners(&[("alice", "aimx-nonexistent-orphan-user")]);
        // owner_uid() errors → caller_owns must be false (we never
        // surface mailboxes with unresolvable owners to non-root).
        assert!(!caller_owns(&cfg, "alice", 1000));
    }

    #[test]
    fn caller_owns_returns_true_for_root_when_owner_is_root() {
        let cfg = config_with_owners(&[("admin", "root")]);
        assert!(caller_owns(&cfg, "admin", 0));
    }

    #[test]
    fn caller_owns_returns_false_for_uid_mismatch() {
        let cfg = config_with_owners(&[("admin", "root")]);
        // Caller is non-root; root-owned mailbox doesn't match.
        assert!(!caller_owns(&cfg, "admin", 1000));
    }
}
