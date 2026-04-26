//! `aimx hooks list | create | delete` CLI.
//!
//! With template hooks gone, every hook in `config.toml` is a raw argv
//! stored under `[[mailboxes.<name>.hooks]]`. CRUD goes through the
//! daemon UDS first (`HOOK-CREATE` / `HOOK-DELETE`) so the running
//! daemon hot-swaps its in-memory `Config` without a restart. The
//! daemon enforces caller-uid = mailbox-owner-uid (or root) per the
//! single auth predicate in `src/auth.rs`. When the daemon is down:
//! root falls back to a direct `config.toml` edit + restart hint;
//! non-root hard-errors because it cannot write the root-owned config.
//!
//! `list` reads the locally-loaded `Config` and filters to caller-owned
//! mailboxes for non-root callers. Reads do not need the daemon —
//! `config.toml` is `0640 root:root`, but the local
//! `dispatch_with_config` path uses `Config::load_resolved`, which
//! requires read access. Non-root operators on a default install
//! cannot read `/etc/aimx/config.toml`; the load failure is surfaced
//! by the dispatcher before the CLI is reached, with a "permission
//! denied" message that is more actionable than this layer would
//! produce.

use std::io::{self, Write};

use crate::auth::{Action, AuthError, authorize};
use crate::cli::{HookCommand, HookCreateArgs};
use crate::config::{Config, validate_hooks};
use crate::hook::{Hook, HookEvent, effective_hook_name, is_valid_hook_name};
use crate::mcp::{HookCrudFallback, submit_hook_create_via_daemon, submit_hook_delete_via_daemon};
use crate::platform::{current_euid, is_root};
use crate::term;

pub fn run(cmd: HookCommand, config: Config) -> Result<(), Box<dyn std::error::Error>> {
    match cmd {
        HookCommand::List { mailbox } => list(&config, mailbox.as_deref()),
        HookCommand::Create(args) => create(&config, args),
        HookCommand::Delete { name, yes } => delete(&config, &name, yes),
    }
}

fn list(config: &Config, filter_mailbox: Option<&str>) -> Result<(), Box<dyn std::error::Error>> {
    if let Some(name) = filter_mailbox
        && !config.mailboxes.contains_key(name)
    {
        return Err(format!("Mailbox '{name}' does not exist").into());
    }

    // Non-root callers see only hooks on mailboxes they own. The
    // daemon does not have a HOOK-LIST verb today; reads come straight
    // from the locally-loaded Config snapshot the dispatcher already
    // produced, with the same euid filter as `aimx mailboxes list`.
    let caller_is_root = is_root();
    let euid = current_euid();
    let mut rows = gather_rows(config, filter_mailbox);
    if !caller_is_root {
        rows.retain(|row| crate::mailbox::caller_owns(config, &row.mailbox, euid));
    }
    rows.sort_by(|a, b| {
        a.mailbox
            .cmp(&b.mailbox)
            .then_with(|| a.event.cmp(b.event))
            .then_with(|| a.name.cmp(&b.name))
    });

    if rows.is_empty() {
        println!("No hooks configured.");
        return Ok(());
    }

    println!(
        "{} {} {} {}",
        term::header("NAME                        "),
        term::header("MAILBOX             "),
        term::header("EVENT       "),
        term::header("CMD"),
    );
    for row in rows {
        println!(
            "{:<28.28} {:<20.20} {:<11} {}",
            term::highlight(&row.name).to_string(),
            row.mailbox,
            row.event,
            truncate_with_ellipsis(&row.cmd, 60),
        );
    }
    Ok(())
}

struct HookRow {
    name: String,
    mailbox: String,
    event: &'static str,
    cmd: String,
}

/// Render a hook argv into a single-line JSON-array string for display
/// in `aimx hooks list` / `aimx mailboxes show`. Falls back to a
/// space-joined argv if `serde_json` somehow refuses (it won't for
/// `Vec<String>` but the fallback keeps the CLI infallible).
fn format_argv_for_display(argv: &[String]) -> String {
    serde_json::to_string(argv).unwrap_or_else(|_| argv.join(" "))
}

fn gather_rows(config: &Config, filter_mailbox: Option<&str>) -> Vec<HookRow> {
    let mut rows = Vec::new();
    for (mailbox_name, mb) in &config.mailboxes {
        if let Some(f) = filter_mailbox
            && f != mailbox_name
        {
            continue;
        }
        for hook in &mb.hooks {
            rows.push(HookRow {
                name: effective_hook_name(hook),
                mailbox: mailbox_name.clone(),
                event: hook.event.as_str(),
                cmd: format_argv_for_display(&hook.cmd),
            });
        }
    }
    rows
}

fn truncate_with_ellipsis(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

fn create(config: &Config, args: HookCreateArgs) -> Result<(), Box<dyn std::error::Error>> {
    if let Some(name) = &args.name
        && !is_valid_hook_name(name)
    {
        return Err(format!(
            "invalid hook name '{name}': must match [a-zA-Z0-9_][a-zA-Z0-9_.-]{{0,127}}"
        )
        .into());
    }
    let mb_cfg = config
        .mailboxes
        .get(&args.mailbox)
        .ok_or_else(|| format!("Mailbox '{}' does not exist", args.mailbox))?;

    // Pre-flight authz so non-owners get a precise error before any
    // socket I/O. The daemon enforces the same predicate; we run it
    // here too so non-root + daemon-down + non-owner errors out
    // consistently rather than producing a misleading "daemon not
    // running" message.
    let euid = current_euid();
    let skip_root_check = std::env::var_os("AIMX_TEST_SKIP_ROOT_CHECK").is_some();
    if !skip_root_check
        && let Err(e) = authorize(euid, Action::HookCrud(args.mailbox.clone()), Some(mb_cfg))
    {
        return Err(format_hook_auth_error(&e, "create").into());
    }

    let event = parse_event(&args.event)?;
    if matches!(event, HookEvent::AfterSend) && args.fire_on_untrusted {
        return Err("--fire-on-untrusted is only valid on --event on_receive".into());
    }
    if args.cmd.trim().is_empty() {
        return Err("--cmd must not be empty".into());
    }
    let cmd = parse_cmd_argv(&args.cmd)?;

    let hook = Hook {
        name: args.name.clone(),
        event,
        r#type: "cmd".to_string(),
        cmd: cmd.clone(),
        fire_on_untrusted: args.fire_on_untrusted,
    };
    let effective = effective_hook_name(&hook);

    // Try the UDS path first. The daemon authorizes via SO_PEERCRED +
    // auth::authorize so the same gate runs whether the caller used CLI
    // or MCP, and the running Config hot-swaps without a restart.
    let body = build_hook_create_body(&cmd, args.fire_on_untrusted)?;
    match submit_hook_create_via_daemon(&args.mailbox, &args.event, args.name.as_deref(), body) {
        Ok(()) => {
            println!(
                "{} {} (live via daemon)",
                term::success("Hook created:"),
                term::highlight(&effective)
            );
            Ok(())
        }
        Err(HookCrudFallback::SocketMissing) => {
            // Daemon down. Only root can rewrite the root-owned
            // config.toml; non-root callers hard-error so we don't
            // pretend the change went through.
            if !skip_root_check && !is_root() {
                return Err("daemon not running, non-root hook CRUD requires daemon".into());
            }
            apply_create_direct(config, &args.mailbox, hook)?;
            println!(
                "{} {}",
                term::success("Hook created:"),
                term::highlight(&effective)
            );
            print_restart_hint();
            Ok(())
        }
        Err(HookCrudFallback::Daemon(msg)) => Err(msg.into()),
    }
}

/// Render an [`AuthError`] for hook CRUD CLI paths.
fn format_hook_auth_error(err: &AuthError, verb: &str) -> String {
    match err {
        AuthError::NotRoot => {
            format!("not authorized: aimx hooks {verb} requires root (run with sudo)")
        }
        AuthError::NotOwner { mailbox } => {
            format!("not authorized: caller does not own mailbox '{mailbox}'")
        }
        AuthError::NoSuchMailbox => "not authorized: no such mailbox".to_string(),
    }
}

/// Build the JSON body the daemon's `HookCreateBody` deserializer
/// expects: `{"cmd": [...], "fire_on_untrusted": <bool>, "type": "cmd"}`.
fn build_hook_create_body(
    cmd: &[String],
    fire_on_untrusted: bool,
) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let body = serde_json::json!({
        "cmd": cmd,
        "fire_on_untrusted": fire_on_untrusted,
        "type": "cmd",
    });
    Ok(serde_json::to_vec(&body)?)
}

fn delete(config: &Config, name: &str, yes: bool) -> Result<(), Box<dyn std::error::Error>> {
    let (mailbox, hook) = match find_hook_by_effective_name(config, name) {
        Some(pair) => pair,
        None => return Err(format!("Hook '{name}' not found").into()),
    };

    if !yes {
        println!("{}", term::warn("About to delete hook:"));
        println!("  {}   {}", term::header("name:   "), term::highlight(name));
        println!("  {}   {}", term::header("mailbox:"), mailbox);
        println!("  {}   {}", term::header("event:  "), hook.event);
        println!(
            "  {}   {}",
            term::header("cmd:    "),
            truncate_with_ellipsis(&format_argv_for_display(&hook.cmd), 60)
        );
        print!("Continue? [y/N] ");
        io::stdout().flush()?;
        let mut input = String::new();
        io::stdin().read_line(&mut input)?;
        if !input.trim().eq_ignore_ascii_case("y") {
            println!("Cancelled.");
            return Ok(());
        }
    }

    let euid = current_euid();
    let skip_root_check = std::env::var_os("AIMX_TEST_SKIP_ROOT_CHECK").is_some();

    // Pre-flight authz: same predicate the daemon enforces, run here so
    // a non-owner hits a precise error before the daemon-or-fallback
    // dispatch.
    if !skip_root_check {
        let mb_cfg = config.mailboxes.get(&mailbox);
        if let Err(e) = authorize(euid, Action::HookCrud(mailbox.clone()), mb_cfg) {
            return Err(format_hook_auth_error(&e, "delete").into());
        }
    }

    match submit_hook_delete_via_daemon(name) {
        Ok(()) => {
            println!(
                "{} {} (live via daemon)",
                term::success("Hook deleted:"),
                term::highlight(name)
            );
            Ok(())
        }
        Err(HookCrudFallback::SocketMissing) => {
            if !skip_root_check && !is_root() {
                return Err("daemon not running, non-root hook CRUD requires daemon".into());
            }
            apply_delete_direct(config, name)?;
            println!(
                "{} {}",
                term::success("Hook deleted:"),
                term::highlight(name)
            );
            print_restart_hint();
            Ok(())
        }
        Err(HookCrudFallback::Daemon(msg)) => Err(msg.into()),
    }
}

fn parse_event(s: &str) -> Result<HookEvent, Box<dyn std::error::Error>> {
    match s {
        "on_receive" => Ok(HookEvent::OnReceive),
        "after_send" => Ok(HookEvent::AfterSend),
        other => {
            Err(format!("Invalid event '{other}': expected 'on_receive' or 'after_send'").into())
        }
    }
}

/// Parse `--cmd` into an argv. Accepts a JSON array of strings
/// (e.g. `["/bin/echo", "hello"]`). Validates that the array is
/// non-empty and that every element is a string; absolute-path checks
/// on `cmd[0]` are deferred to `validate_hooks` so the CLI and the
/// daemon use the same validator.
fn parse_cmd_argv(raw: &str) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    let trimmed = raw.trim_start();
    if !trimmed.starts_with('[') {
        return Err(format!(
            "--cmd must be a JSON array of strings, e.g. \
             --cmd '[\"/bin/echo\", \"hello\"]'; got: {raw}"
        )
        .into());
    }
    let argv: Vec<String> = serde_json::from_str(raw).map_err(|e| {
        format!(
            "--cmd must be a JSON array of strings, e.g. \
             --cmd '[\"/bin/echo\", \"hello\"]'; parse error: {e}"
        )
    })?;
    if argv.is_empty() {
        return Err("--cmd must be a non-empty JSON array of strings".into());
    }
    Ok(argv)
}

fn find_hook_by_effective_name<'a>(config: &'a Config, name: &str) -> Option<(String, &'a Hook)> {
    for (mailbox_name, mb) in &config.mailboxes {
        for hook in &mb.hooks {
            if effective_hook_name(hook) == name {
                return Some((mailbox_name.clone(), hook));
            }
        }
    }
    None
}

fn apply_create_direct(
    config: &Config,
    mailbox: &str,
    hook: Hook,
) -> Result<(), Box<dyn std::error::Error>> {
    let path = crate::config::config_path();
    let mut cfg = config.clone();
    let mb = cfg
        .mailboxes
        .get_mut(mailbox)
        .ok_or_else(|| format!("Mailbox '{mailbox}' does not exist"))?;
    mb.hooks.push(hook);
    validate_hooks(&cfg)?;
    cfg.save(&path)?;
    Ok(())
}

fn apply_delete_direct(config: &Config, name: &str) -> Result<(), Box<dyn std::error::Error>> {
    let path = crate::config::config_path();
    let mut cfg = config.clone();
    let mut found = false;
    for mb in cfg.mailboxes.values_mut() {
        let before = mb.hooks.len();
        mb.hooks.retain(|h| effective_hook_name(h) != name);
        if mb.hooks.len() != before {
            found = true;
        }
    }
    if !found {
        return Err(format!("Hook '{name}' not found").into());
    }
    cfg.save(&path)?;
    Ok(())
}

fn print_restart_hint() {
    println!(
        "{} aimx serve is not running. The change will take effect on the next start \
         (`sudo systemctl start aimx`).",
        term::info("Note:")
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::AuthError;

    #[test]
    fn build_hook_create_body_emits_canonical_json() {
        let body =
            build_hook_create_body(&["/bin/echo".to_string(), "hi".to_string()], true).unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(parsed["cmd"][0], "/bin/echo");
        assert_eq!(parsed["cmd"][1], "hi");
        assert_eq!(parsed["fire_on_untrusted"], true);
        assert_eq!(parsed["type"], "cmd");
    }

    #[test]
    fn build_hook_create_body_default_fire_on_untrusted_is_false() {
        let body = build_hook_create_body(&["/bin/true".to_string()], false).unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(parsed["fire_on_untrusted"], false);
    }

    #[test]
    fn format_hook_auth_error_not_owner_names_mailbox() {
        let msg = format_hook_auth_error(
            &AuthError::NotOwner {
                mailbox: "alice".into(),
            },
            "create",
        );
        assert!(msg.contains("alice"), "{msg}");
        assert!(msg.contains("not authorized"), "{msg}");
    }

    #[test]
    fn format_hook_auth_error_not_root_mentions_verb() {
        let msg = format_hook_auth_error(&AuthError::NotRoot, "delete");
        assert!(msg.contains("delete"), "{msg}");
        assert!(msg.contains("sudo"), "{msg}");
    }

    #[test]
    fn format_hook_auth_error_no_such_mailbox_is_opaque() {
        let msg = format_hook_auth_error(&AuthError::NoSuchMailbox, "create");
        assert!(msg.contains("not authorized"), "{msg}");
        assert!(msg.contains("no such mailbox"), "{msg}");
    }
}
