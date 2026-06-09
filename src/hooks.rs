//! `aimx hooks list | create | delete` CLI.
//!
//! With template hooks gone, every hook in `config.toml` is a raw argv
//! stored under `[[mailboxes.<name>.hooks]]`. All three subcommands go
//! through the daemon UDS first (`HOOK-LIST` / `HOOK-CREATE` /
//! `HOOK-DELETE`) so the running daemon hot-swaps its in-memory
//! `Config` without a restart. The daemon enforces caller-uid =
//! mailbox-owner-uid (or root) per the single auth predicate in
//! `src/auth.rs`, keyed on the kernel-validated `SO_PEERCRED` uid.
//!
//! The local `Config` is loaded *optionally* (mirroring
//! `src/mailbox.rs`) because `/etc/aimx/config.toml` is `0640
//! root:root` in production: a non-root mailbox owner cannot read it,
//! so an eager load would surface `Permission denied (os error 13)`
//! before any hooks code runs. When the config loads (root, or a
//! readable install) the local code path is used — `list` applies the
//! same per-caller-euid ownership filter as `aimx mailboxes list`, and
//! `create` / `delete` run a local authz pre-flight. When the config
//! is unreadable, `list` falls back to the `HOOK-LIST` verb (already
//! ownership-filtered server-side) and `create` / `delete` rely on the
//! daemon's `SO_PEERCRED` authz.
//!
//! When the daemon is down: root falls back to a direct `config.toml`
//! edit + restart hint; non-root hard-errors because it cannot write
//! (or read) the root-owned config.

use std::io::{self, Write};
use std::path::Path;

use crate::auth::{Action, AuthErrorContext, authorize, format_auth_error};
use crate::cli::{HookCommand, HookCreateArgs};
use crate::config::{Config, validate_hooks};
use crate::hook::{
    DEFAULT_HOOK_TIMEOUT_SECS, Hook, HookEvent, effective_hook_name, is_valid_hook_name,
};
use crate::mcp::{
    HookCrudFallback, MailboxLifecycleFallback, submit_hook_create_via_daemon,
    submit_hook_delete_via_daemon, submit_hook_list_via_daemon_for_cli,
};
use crate::platform::{current_euid, is_root};
use crate::term;

pub fn run(cmd: HookCommand, data_dir: Option<&Path>) -> Result<(), Box<dyn std::error::Error>> {
    // Load config optionally so a non-root caller — who cannot read
    // `0640 root:root /etc/aimx/config.toml` in production — still
    // reaches the subcommand code and routes through the daemon UDS.
    // Each subcommand decides whether the missing-config case is
    // recoverable.
    let loaded = crate::mailbox::load_config_optional(data_dir);

    match cmd {
        HookCommand::List { mailbox } => list_dispatch(loaded.as_ref(), mailbox.as_deref()),
        HookCommand::Create(args) => create(loaded.as_ref(), args),
        HookCommand::Delete { name, yes } => delete(loaded.as_ref(), &name, yes),
    }
}

/// `hooks list` dispatcher. With a readable config we walk it locally
/// (applying the per-caller-euid ownership filter); without one — the
/// non-root default-install case — we route through the daemon's
/// `HOOK-LIST` verb, which is already ownership-filtered server-side.
fn list_dispatch(
    config: Option<&Config>,
    filter_mailbox: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    match config {
        Some(cfg) => list(cfg, filter_mailbox),
        None => list_via_daemon(filter_mailbox),
    }
}

fn list(config: &Config, filter_mailbox: Option<&str>) -> Result<(), Box<dyn std::error::Error>> {
    if let Some(name) = filter_mailbox
        && !config.mailboxes.contains_key(name)
    {
        return Err(format!("Mailbox '{name}' does not exist").into());
    }

    // Non-root callers see only hooks on mailboxes they own, using the
    // same euid filter as `aimx mailboxes list`.
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

    render_hook_rows(&rows);
    Ok(())
}

/// Non-root fallback for `hooks list` when the local config is
/// unreadable. The daemon's `HOOK-LIST` rows are already filtered to
/// mailboxes the caller's `SO_PEERCRED` uid owns and sorted by
/// `(mailbox, event, name)`, so we only re-apply the optional
/// `--mailbox` filter and render. A socket-missing daemon yields the
/// canonical "daemon must be running" hint; a daemon-side error
/// bubbles verbatim.
fn list_via_daemon(filter_mailbox: Option<&str>) -> Result<(), Box<dyn std::error::Error>> {
    let json = match submit_hook_list_via_daemon_for_cli() {
        Ok(s) => s,
        Err(MailboxLifecycleFallback::SocketMissing) => {
            return Err(append_daemon_down_hint(format_auth_error(
                &crate::auth::AuthError::NotRoot,
                &hooks_ctx("list"),
            ))
            .into());
        }
        Err(MailboxLifecycleFallback::Daemon(msg)) => return Err(msg.into()),
    };

    let rows: Vec<crate::hook_list_handler::HookListRow> =
        serde_json::from_str(&json).map_err(|e| format!("malformed HOOK-LIST response: {e}"))?;

    // The daemon's listing is opaque between "mailbox doesn't exist"
    // and "exists but not owned by the caller", so a `--mailbox` filter
    // that matches nothing simply renders "No hooks configured." rather
    // than the local path's "Mailbox '<name>' does not exist" error.
    let rows: Vec<HookRow> = rows
        .into_iter()
        .filter(|r| filter_mailbox.is_none_or(|f| f == r.mailbox))
        .map(|r| HookRow {
            name: r.name,
            mailbox: r.mailbox,
            event: r.event.as_str(),
            cmd: format_argv_for_display(&r.cmd),
            timeout_secs: r.timeout_secs,
        })
        .collect();

    render_hook_rows(&rows);
    Ok(())
}

/// Render the hook table (or the empty-state line). Shared by the local
/// and daemon-backed list paths so both emit byte-identical output.
fn render_hook_rows(rows: &[HookRow]) {
    if rows.is_empty() {
        println!("No hooks configured.");
        return;
    }

    println!(
        "{} {} {} {} {}",
        term::header("NAME                        "),
        term::header("MAILBOX             "),
        term::header("EVENT       "),
        term::header("TIMEOUT"),
        term::header("CMD"),
    );
    for row in rows {
        println!(
            "{:<28.28} {:<20.20} {:<11} {:>7} {}",
            term::highlight(&row.name).to_string(),
            row.mailbox,
            row.event,
            row.timeout_secs,
            truncate_with_ellipsis(&row.cmd, 60),
        );
    }
}

/// Build the `AuthErrorContext` the hooks CLI uses for every authz
/// error so the rendered wording stays consistent across `list` /
/// `create` / `delete`.
fn hooks_ctx(verb: &'static str) -> AuthErrorContext<'static> {
    AuthErrorContext {
        surface: Some("aimx hooks"),
        verb: Some(verb),
        resource: Some("resource"),
        ..Default::default()
    }
}

struct HookRow {
    name: String,
    mailbox: String,
    event: &'static str,
    cmd: String,
    timeout_secs: u32,
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
                timeout_secs: hook.timeout_secs,
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

fn create(config: Option<&Config>, args: HookCreateArgs) -> Result<(), Box<dyn std::error::Error>> {
    if let Some(name) = &args.name
        && !is_valid_hook_name(name)
    {
        return Err(format!(
            "invalid hook name '{name}': must match [a-zA-Z0-9_][a-zA-Z0-9_.-]{{0,127}}"
        )
        .into());
    }

    // `AIMX_TEST_SKIP_AUTHZ_CHECK=1` is the test-harness opt-in documented
    // in `CLAUDE.md`'s "Test environment escape hatches" section. It
    // bypasses the entire `authorize()` call (gating `Action::HookCrud`)
    // so the post-gate code path stays exercised under non-root
    // `cargo test`. Never set in production.
    let skip_authz_check = std::env::var_os("AIMX_TEST_SKIP_AUTHZ_CHECK").is_some();
    let euid = current_euid();

    // Local pre-flight authz only when the config is readable (root, or
    // a readable install) so non-owners get a precise error before any
    // socket I/O. With an unreadable config (non-root default install)
    // we skip the local check and rely on the daemon's `SO_PEERCRED`
    // authz; the mailbox-existence and authz errors then come back over
    // UDS verbatim.
    if let Some(cfg) = config {
        let mb_cfg = cfg
            .mailboxes
            .get(&args.mailbox)
            .ok_or_else(|| format!("Mailbox '{}' does not exist", args.mailbox))?;
        if !skip_authz_check
            && let Err(e) = authorize(euid, Action::HookCrud(args.mailbox.clone()), Some(mb_cfg))
        {
            return Err(format_auth_error(&e, &hooks_ctx("create")).into());
        }
    }

    let event = parse_event(&args.event)?;
    if matches!(event, HookEvent::AfterSend) && args.fire_on_untrusted {
        return Err("--fire-on-untrusted is only valid on --event on_receive".into());
    }
    if args.cmd.trim().is_empty() {
        return Err("--cmd must not be empty".into());
    }
    let cmd = parse_cmd_argv(&args.cmd)?;

    let timeout_secs = args.timeout_secs.unwrap_or(DEFAULT_HOOK_TIMEOUT_SECS);

    let hook = Hook {
        name: args.name.clone(),
        event,
        r#type: "cmd".to_string(),
        cmd: cmd.clone(),
        fire_on_untrusted: args.fire_on_untrusted,
        timeout_secs,
    };
    let effective = effective_hook_name(&hook);

    // Try the UDS path first. The daemon authorizes via SO_PEERCRED +
    // auth::authorize so the same gate runs whether the caller used CLI
    // or MCP, and the running Config hot-swaps without a restart.
    let body = build_hook_create_body(&cmd, args.fire_on_untrusted, timeout_secs)?;
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
            // Daemon down. The direct on-disk fallback needs a readable
            // config (to clone + re-serialize) and write access to the
            // root-owned config.toml, so it is gated on `Some(cfg)` and
            // root (or the test escape hatch). Otherwise we hard-error
            // so we don't pretend the change went through. Route the
            // error through the canonical `format_auth_error` so wording
            // stays consistent with every other authz-error surface, then
            // append a daemon-down hint so operators see why sudo is
            // required (the canonical wording mentions sudo but not the
            // underlying "daemon is down" cause).
            match config {
                Some(cfg) if is_root() || skip_authz_check => {
                    apply_create_direct(cfg, &args.mailbox, hook)?;
                    println!(
                        "{} {}",
                        term::success("Hook created:"),
                        term::highlight(&effective)
                    );
                    print_restart_hint();
                    Ok(())
                }
                _ => Err(append_daemon_down_hint(format_auth_error(
                    &crate::auth::AuthError::NotRoot,
                    &hooks_ctx("create"),
                ))
                .into()),
            }
        }
        Err(HookCrudFallback::Daemon(msg)) => Err(msg.into()),
    }
}

/// Build the JSON body the daemon's `HookCreateBody` deserializer
/// expects: `{"cmd": [...], "fire_on_untrusted": <bool>, "type": "cmd"}`.
/// `timeout_secs` is emitted only when it differs from the schema
/// default so existing callers (and round-tripped configs) round
/// through unchanged.
fn build_hook_create_body(
    cmd: &[String],
    fire_on_untrusted: bool,
    timeout_secs: u32,
) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let mut body = serde_json::json!({
        "cmd": cmd,
        "fire_on_untrusted": fire_on_untrusted,
        "type": "cmd",
    });
    if timeout_secs != DEFAULT_HOOK_TIMEOUT_SECS {
        body["timeout_secs"] = serde_json::Value::Number(timeout_secs.into());
    }
    Ok(serde_json::to_vec(&body)?)
}

fn delete(
    config: Option<&Config>,
    name: &str,
    yes: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    match config {
        Some(cfg) => delete_with_config(cfg, name, yes),
        None => delete_via_daemon(name, yes),
    }
}

/// `hooks delete` with a readable local config: resolve the hook for
/// the confirmation prompt + local authz pre-flight, then dispatch over
/// UDS (with the root-only direct-edit fallback when the daemon is down).
fn delete_with_config(
    config: &Config,
    name: &str,
    yes: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let (mailbox, hook) = match find_hook_by_effective_name(config, name) {
        Some(pair) => pair,
        None => return Err(format!("Hook '{name}' not found").into()),
    };

    if !yes
        && !confirm_delete(
            name,
            &mailbox,
            hook.event.as_str(),
            hook.timeout_secs,
            &format_argv_for_display(&hook.cmd),
        )?
    {
        return Ok(());
    }

    let euid = current_euid();
    let skip_authz_check = std::env::var_os("AIMX_TEST_SKIP_AUTHZ_CHECK").is_some();

    // Pre-flight authz: same predicate the daemon enforces, run here so
    // a non-owner hits a precise error before the daemon-or-fallback
    // dispatch.
    if !skip_authz_check {
        let mb_cfg = config.mailboxes.get(&mailbox);
        if let Err(e) = authorize(euid, Action::HookCrud(mailbox.clone()), mb_cfg) {
            return Err(format_auth_error(&e, &hooks_ctx("delete")).into());
        }
    }

    match submit_hook_delete_via_daemon(name) {
        Ok(()) => {
            print_hook_deleted_live(name);
            Ok(())
        }
        Err(HookCrudFallback::SocketMissing) => {
            if is_root() || skip_authz_check {
                apply_delete_direct(config, name)?;
                println!(
                    "{} {}",
                    term::success("Hook deleted:"),
                    term::highlight(name)
                );
                print_restart_hint();
                Ok(())
            } else {
                Err(append_daemon_down_hint(format_auth_error(
                    &crate::auth::AuthError::NotRoot,
                    &hooks_ctx("delete"),
                ))
                .into())
            }
        }
        Err(HookCrudFallback::Daemon(msg)) => Err(msg.into()),
    }
}

/// `hooks delete` without a readable local config (non-root default
/// install). Resolve the hook from the daemon's `HOOK-LIST` view for the
/// confirmation prompt, then submit `HOOK-DELETE`. The daemon
/// re-authorizes via `SO_PEERCRED`, so no local authz pre-flight runs.
fn delete_via_daemon(name: &str, yes: bool) -> Result<(), Box<dyn std::error::Error>> {
    if !yes {
        let json = match submit_hook_list_via_daemon_for_cli() {
            Ok(s) => s,
            Err(MailboxLifecycleFallback::SocketMissing) => {
                return Err(append_daemon_down_hint(format_auth_error(
                    &crate::auth::AuthError::NotRoot,
                    &hooks_ctx("delete"),
                ))
                .into());
            }
            Err(MailboxLifecycleFallback::Daemon(msg)) => return Err(msg.into()),
        };
        let rows: Vec<crate::hook_list_handler::HookListRow> = serde_json::from_str(&json)
            .map_err(|e| format!("malformed HOOK-LIST response: {e}"))?;
        // The daemon's listing is already filtered to hooks the caller
        // owns, so a name absent here is opaque between "no such hook"
        // and "not owned" — surface the same not-found error either way.
        let row = rows
            .iter()
            .find(|r| r.name == name)
            .ok_or_else(|| format!("Hook '{name}' not found"))?;
        if !confirm_delete(
            name,
            &row.mailbox,
            row.event.as_str(),
            row.timeout_secs,
            &format_argv_for_display(&row.cmd),
        )? {
            return Ok(());
        }
    }

    match submit_hook_delete_via_daemon(name) {
        Ok(()) => {
            print_hook_deleted_live(name);
            Ok(())
        }
        // The daemon is the only writer for a non-root caller; with no
        // readable config there is no direct-edit fallback.
        Err(HookCrudFallback::SocketMissing) => Err(append_daemon_down_hint(format_auth_error(
            &crate::auth::AuthError::NotRoot,
            &hooks_ctx("delete"),
        ))
        .into()),
        Err(HookCrudFallback::Daemon(msg)) => Err(msg.into()),
    }
}

/// Print the "About to delete hook" summary and read a `[y/N]`
/// confirmation. Returns `Ok(true)` to proceed, `Ok(false)` when the
/// operator declines (and prints "Cancelled." so both delete paths emit
/// identical output).
fn confirm_delete(
    name: &str,
    mailbox: &str,
    event: &str,
    timeout_secs: u32,
    cmd_display: &str,
) -> Result<bool, Box<dyn std::error::Error>> {
    println!("{}", term::warn("About to delete hook:"));
    println!("  {}   {}", term::header("name:   "), term::highlight(name));
    println!("  {}   {}", term::header("mailbox:"), mailbox);
    println!("  {}   {}", term::header("event:  "), event);
    println!("  {}   {}", term::header("timeout:"), timeout_secs);
    println!(
        "  {}   {}",
        term::header("cmd:    "),
        truncate_with_ellipsis(cmd_display, 60)
    );
    print!("Continue? [y/N] ");
    io::stdout().flush()?;
    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    if input.trim().eq_ignore_ascii_case("y") {
        Ok(true)
    } else {
        println!("Cancelled.");
        Ok(false)
    }
}

fn print_hook_deleted_live(name: &str) {
    println!(
        "{} {} (live via daemon)",
        term::success("Hook deleted:"),
        term::highlight(name)
    );
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

/// Appends a daemon-down hint to a canonical authz error string. Used
/// only on the socket-missing + non-root hook CRUD path, where the
/// canonical `NotRoot` rendering tells the operator to use `sudo` but
/// no longer mentions the underlying cause (the daemon is not running,
/// which is why we fell through to the root-only direct-config-edit
/// path in the first place). The hint is local to that call site so
/// the canonical renderer's surface stays narrow.
fn append_daemon_down_hint(msg: String) -> String {
    format!(
        "{msg}\nhint: if the daemon is running, hook CRUD over UDS would handle this without sudo."
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::AuthError;

    #[test]
    fn build_hook_create_body_emits_canonical_json() {
        let body = build_hook_create_body(
            &["/bin/echo".to_string(), "hi".to_string()],
            true,
            DEFAULT_HOOK_TIMEOUT_SECS,
        )
        .unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(parsed["cmd"][0], "/bin/echo");
        assert_eq!(parsed["cmd"][1], "hi");
        assert_eq!(parsed["fire_on_untrusted"], true);
        assert_eq!(parsed["type"], "cmd");
        // Defaults are not serialized so the wire stays stable across
        // operators that don't touch timeout_secs. `stdin` is no
        // longer a recognized field.
        assert!(parsed.get("stdin").is_none(), "{parsed}");
        assert!(parsed.get("timeout_secs").is_none(), "{parsed}");
    }

    #[test]
    fn build_hook_create_body_default_fire_on_untrusted_is_false() {
        let body =
            build_hook_create_body(&["/bin/true".to_string()], false, DEFAULT_HOOK_TIMEOUT_SECS)
                .unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(parsed["fire_on_untrusted"], false);
    }

    #[test]
    fn build_hook_create_body_emits_timeout_when_non_default() {
        let body = build_hook_create_body(&["/bin/true".to_string()], false, 5).unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(parsed["timeout_secs"], 5);
        assert!(parsed.get("stdin").is_none(), "{parsed}");
    }

    /// Helper mirroring the per-call rendering hooks paths use after
    /// the canonical-renderer consolidation. The two paths live inline
    /// in `create` / `delete`; this tiny shim exists only so the test
    /// assertions stay focused on the rendered wording rather than on
    /// repeating the `AuthErrorContext` construction.
    fn render_for_hooks(err: &AuthError, verb: &str) -> String {
        format_auth_error(
            err,
            &AuthErrorContext {
                surface: Some("aimx hooks"),
                verb: Some(verb),
                resource: Some("resource"),
                ..Default::default()
            },
        )
    }

    #[test]
    fn hook_auth_error_not_owner_names_mailbox() {
        let msg = render_for_hooks(
            &AuthError::NotOwner {
                mailbox: "alice".into(),
            },
            "create",
        );
        assert!(msg.contains("alice"), "{msg}");
        assert!(msg.contains("not authorized"), "{msg}");
    }

    #[test]
    fn hook_auth_error_not_root_mentions_verb() {
        let msg = render_for_hooks(&AuthError::NotRoot, "delete");
        assert!(msg.contains("delete"), "{msg}");
        assert!(msg.contains("sudo"), "{msg}");
    }

    #[test]
    fn hook_auth_error_no_such_mailbox_is_opaque() {
        let msg = render_for_hooks(&AuthError::NoSuchMailbox, "create");
        assert!(msg.contains("not authorized"), "{msg}");
        assert!(msg.contains("no such mailbox"), "{msg}");
    }

    #[test]
    fn append_daemon_down_hint_preserves_canonical_message_and_appends_hint() {
        let canonical = render_for_hooks(&AuthError::NotRoot, "create");
        let with_hint = append_daemon_down_hint(canonical.clone());
        // Canonical wording is preserved verbatim as the first line so
        // the regression guard for the canonical renderer stays valid.
        assert!(with_hint.starts_with(&canonical), "{with_hint}");
        // Operator-helpful daemon-down context is restored on its own
        // line so the suffix can be grepped for in CI / docs.
        assert!(with_hint.contains("\nhint:"), "{with_hint}");
        assert!(with_hint.contains("daemon"), "{with_hint}");
        assert!(with_hint.contains("UDS"), "{with_hint}");
    }
}
