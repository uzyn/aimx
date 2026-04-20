//! `aimx hooks list | create | delete` CLI.
//!
//! `hook` (singular) is kept as a clap alias on the subcommand for muscle
//! memory. All three sub-subcommands route UDS-first so the daemon can
//! hot-swap its in-memory `Arc<Config>`; on socket-missing the CLI falls
//! back to a direct `config.toml` edit + restart hint. `create` is
//! flag-based and accepts an optional `--name`; when omitted, the effective
//! name is derived at runtime from `sha256(event + cmd + dangerous)` and is
//! NOT written back to `config.toml`. `delete <name>` resolves against
//! effective names (explicit or derived) and shows the hook's mailbox /
//! event / cmd with a `[y/N]` prompt unless `--yes`.

use std::io::{self, Write};

use crate::cli::{HookCommand, HookCreateArgs};
use crate::config::{Config, validate_hooks};
use crate::hook::{Hook, HookEvent, effective_hook_name, is_valid_hook_name};
use crate::hook_client::{
    HookCrudFallback, submit_hook_create_via_daemon, submit_hook_delete_via_daemon,
};
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

    let mut rows = gather_rows(config, filter_mailbox);
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

fn create(config: &Config, args: HookCreateArgs) -> Result<(), Box<dyn std::error::Error>> {
    if !config.mailboxes.contains_key(&args.mailbox) {
        return Err(format!("Mailbox '{}' does not exist", args.mailbox).into());
    }

    validate_create_args(&args)?;

    let event = parse_event(&args.event)?;
    let hook = Hook {
        name: args.name.clone(),
        event,
        r#type: "cmd".to_string(),
        cmd: args.cmd,
        dangerously_support_untrusted: args.dangerously_support_untrusted,
    };
    let effective = effective_hook_name(&hook);

    match submit_hook_create_via_daemon(&args.mailbox, &hook) {
        Ok(()) => {
            println!(
                "{} {}",
                term::success("Hook created:"),
                term::highlight(&effective)
            );
            Ok(())
        }
        Err(HookCrudFallback::SocketMissing) => {
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
            truncate_with_ellipsis(&hook.cmd, 60)
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

    match submit_hook_delete_via_daemon(name) {
        Ok(()) => {
            println!(
                "{} {}",
                term::success("Hook deleted:"),
                term::highlight(name)
            );
            Ok(())
        }
        Err(HookCrudFallback::SocketMissing) => {
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

/// Public-for-test helper: validate the `HookCreateArgs` pre-submission.
pub(crate) fn validate_create_args(
    args: &HookCreateArgs,
) -> Result<(), Box<dyn std::error::Error>> {
    let event = parse_event(&args.event)?;
    if let HookEvent::AfterSend = event
        && args.dangerously_support_untrusted
    {
        return Err("--dangerously-support-untrusted is only valid on --event on_receive".into());
    }
    if args.cmd.trim().is_empty() {
        return Err("--cmd must not be empty".into());
    }
    if let Some(name) = &args.name
        && !is_valid_hook_name(name)
    {
        return Err(format!(
            "--name '{name}' is invalid: must match \
             [a-zA-Z0-9_][a-zA-Z0-9_.-]{{0,127}}"
        )
        .into());
    }
    Ok(())
}

fn parse_event(s: &str) -> Result<HookEvent, Box<dyn std::error::Error>> {
    match s {
        "on_receive" => Ok(HookEvent::OnReceive),
        "after_send" => Ok(HookEvent::AfterSend),
        other => Err(format!("invalid event '{other}': expected on_receive or after_send").into()),
    }
}

fn apply_create_direct(
    config: &Config,
    mailbox: &str,
    hook: Hook,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut new_config = config.clone();
    if !new_config.mailboxes.contains_key(mailbox) {
        return Err(format!("Mailbox '{mailbox}' does not exist").into());
    }
    if let Some(mb) = new_config.mailboxes.get_mut(mailbox) {
        mb.hooks.push(hook);
    }
    validate_hooks(&new_config).map_err(|e| -> Box<dyn std::error::Error> { e.into() })?;
    new_config.save(&crate::config::config_path())?;
    Ok(())
}

fn apply_delete_direct(config: &Config, name: &str) -> Result<(), Box<dyn std::error::Error>> {
    let mut new_config = config.clone();
    let mut removed = false;
    for mb in new_config.mailboxes.values_mut() {
        let before = mb.hooks.len();
        mb.hooks.retain(|h| effective_hook_name(h) != name);
        if mb.hooks.len() != before {
            removed = true;
            // Safe to break: `validate_hooks` guarantees effective-name
            // uniqueness globally, so at most one hook ever matches.
            break;
        }
    }
    if !removed {
        return Err(format!("Hook '{name}' not found").into());
    }
    new_config.save(&crate::config::config_path())?;
    Ok(())
}

fn find_hook_by_effective_name<'a>(config: &'a Config, name: &str) -> Option<(String, &'a Hook)> {
    for (mb_name, mb) in &config.mailboxes {
        for h in &mb.hooks {
            if effective_hook_name(h) == name {
                return Some((mb_name.clone(), h));
            }
        }
    }
    None
}

fn print_restart_hint() {
    let init = crate::serve::service::detect_init_system();
    for line in crate::mailbox::restart_hint_lines(&init) {
        println!("{line}");
    }
}

#[derive(Debug)]
struct Row {
    name: String,
    mailbox: String,
    event: &'static str,
    cmd: String,
}

fn gather_rows(config: &Config, filter_mailbox: Option<&str>) -> Vec<Row> {
    let mut rows = Vec::new();
    for (name, mb) in &config.mailboxes {
        if let Some(f) = filter_mailbox
            && f != name
        {
            continue;
        }
        for h in &mb.hooks {
            rows.push(Row {
                name: effective_hook_name(h),
                mailbox: name.clone(),
                event: match h.event {
                    HookEvent::OnReceive => "on_receive",
                    HookEvent::AfterSend => "after_send",
                },
                cmd: h.cmd.clone(),
            });
        }
    }
    rows
}

/// Truncate `s` to `max` *chars* (not bytes), appending `…` on overflow.
/// Deliberately uses the single-codepoint ellipsis so columnar layouts
/// stay aligned.
pub(crate) fn truncate_with_ellipsis(s: &str, max: usize) -> String {
    let count = s.chars().count();
    if count <= max {
        return s.to_string();
    }
    let head: String = s.chars().take(max.saturating_sub(1)).collect();
    format!("{head}…")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::MailboxConfig;
    use std::collections::HashMap;
    use std::path::PathBuf;

    fn base_config() -> Config {
        let mut mailboxes = HashMap::new();
        mailboxes.insert(
            "catchall".to_string(),
            MailboxConfig {
                address: "*@test.com".to_string(),
                hooks: vec![],
                trust: None,
                trusted_senders: None,
            },
        );
        mailboxes.insert(
            "alice".to_string(),
            MailboxConfig {
                address: "alice@test.com".to_string(),
                hooks: vec![],
                trust: None,
                trusted_senders: None,
            },
        );
        Config {
            domain: "test.com".to_string(),
            data_dir: PathBuf::from("/tmp"),
            dkim_selector: "aimx".to_string(),
            trust: "none".to_string(),
            trusted_senders: vec![],
            mailboxes,
            verify_host: None,
            enable_ipv6: false,
        }
    }

    fn base_args(event: &str) -> HookCreateArgs {
        HookCreateArgs {
            mailbox: "alice".into(),
            event: event.into(),
            cmd: "echo hi".into(),
            name: None,
            dangerously_support_untrusted: false,
        }
    }

    #[test]
    fn validate_rejects_dangerous_on_after_send() {
        let args = HookCreateArgs {
            dangerously_support_untrusted: true,
            ..base_args("after_send")
        };
        let err = validate_create_args(&args).unwrap_err().to_string();
        assert!(err.contains("--dangerously-support-untrusted"), "{err}");
    }

    #[test]
    fn validate_rejects_empty_cmd() {
        let args = HookCreateArgs {
            cmd: "   ".into(),
            ..base_args("on_receive")
        };
        let err = validate_create_args(&args).unwrap_err().to_string();
        assert!(err.contains("--cmd"), "{err}");
    }

    #[test]
    fn validate_rejects_invalid_name() {
        let args = HookCreateArgs {
            name: Some("bad name!".into()),
            ..base_args("on_receive")
        };
        let err = validate_create_args(&args).unwrap_err().to_string();
        assert!(err.contains("--name"), "{err}");
    }

    #[test]
    fn validate_accepts_valid_name() {
        let args = HookCreateArgs {
            name: Some("nightly_summary".into()),
            ..base_args("on_receive")
        };
        validate_create_args(&args).unwrap();
    }

    #[test]
    fn validate_accepts_anonymous() {
        validate_create_args(&base_args("on_receive")).unwrap();
    }

    #[test]
    fn truncate_short_string_unchanged() {
        assert_eq!(truncate_with_ellipsis("hi", 60), "hi");
    }

    #[test]
    fn truncate_long_string_appends_ellipsis() {
        let long = "x".repeat(100);
        let out = truncate_with_ellipsis(&long, 60);
        assert_eq!(out.chars().count(), 60);
        assert!(out.ends_with('…'));
    }

    #[test]
    fn find_hook_by_effective_name_matches_explicit_and_derived() {
        let mut cfg = base_config();
        // Explicit
        cfg.mailboxes.get_mut("alice").unwrap().hooks.push(Hook {
            name: Some("explicit_one".into()),
            event: HookEvent::OnReceive,
            r#type: "cmd".into(),
            cmd: "echo one".into(),
            dangerously_support_untrusted: false,
        });
        // Anonymous
        let anon = Hook {
            name: None,
            event: HookEvent::AfterSend,
            r#type: "cmd".into(),
            cmd: "echo anon".into(),
            dangerously_support_untrusted: false,
        };
        let anon_derived = effective_hook_name(&anon);
        cfg.mailboxes.get_mut("catchall").unwrap().hooks.push(anon);

        assert!(find_hook_by_effective_name(&cfg, "explicit_one").is_some());
        assert!(find_hook_by_effective_name(&cfg, &anon_derived).is_some());
        assert!(find_hook_by_effective_name(&cfg, "not_there").is_none());
    }
}
