//! `aimx hooks list | create | delete` CLI.
//!
//! With template hooks gone, every hook in `config.toml` is a raw shell
//! command stored under `[[mailboxes.<name>.hooks]]`. Hook CRUD is
//! root-only and writes `config.toml` directly (no UDS), then SIGHUPs
//! the running daemon so the change is picked up without a restart.

use std::io::{self, Write};

use crate::cli::{HookCommand, HookCreateArgs};
use crate::config::{Config, validate_hooks};
use crate::hook::{Hook, HookEvent, effective_hook_name, is_valid_hook_name};
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

struct HookRow {
    name: String,
    mailbox: String,
    event: &'static str,
    cmd: String,
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
                cmd: hook.cmd.clone(),
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
    let skip_root_check = std::env::var_os("AIMX_TEST_SKIP_ROOT_CHECK").is_some();
    if !skip_root_check && !crate::platform::is_root() {
        return Err(
            "hook creation requires root: run again with `sudo aimx hooks create --cmd ...`."
                .into(),
        );
    }

    if let Some(name) = &args.name
        && !is_valid_hook_name(name)
    {
        return Err(format!(
            "invalid hook name '{name}': must match [a-zA-Z0-9_][a-zA-Z0-9_.-]{{0,127}}"
        )
        .into());
    }
    if !config.mailboxes.contains_key(&args.mailbox) {
        return Err(format!("Mailbox '{}' does not exist", args.mailbox).into());
    }
    let event = parse_event(&args.event)?;
    if matches!(event, HookEvent::AfterSend) && args.fire_on_untrusted {
        return Err("--fire-on-untrusted is only valid on --event on_receive".into());
    }
    if args.cmd.trim().is_empty() {
        return Err("--cmd must not be empty".into());
    }

    let hook = Hook {
        name: args.name.clone(),
        event,
        r#type: "cmd".to_string(),
        cmd: args.cmd.clone(),
        fire_on_untrusted: args.fire_on_untrusted,
    };
    let effective = effective_hook_name(&hook);

    apply_create_direct(config, &args.mailbox, hook)?;
    println!(
        "{} {}",
        term::success("Hook created:"),
        term::highlight(&effective)
    );

    match crate::serve::sighup_running_daemon() {
        crate::serve::SighupOutcome::Sent(pid) => {
            println!(
                "{} SIGHUP sent to aimx serve (pid {pid}); hook is live.",
                term::info("Reload:")
            );
        }
        crate::serve::SighupOutcome::DaemonNotRunning => {
            print_restart_hint();
        }
        crate::serve::SighupOutcome::SignalFailed(pid, err) => {
            eprintln!(
                "{} failed to SIGHUP aimx serve (pid {pid}): {err}",
                term::warn("Warning:")
            );
            print_restart_hint();
        }
    }
    Ok(())
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

    let skip_root_check = std::env::var_os("AIMX_TEST_SKIP_ROOT_CHECK").is_some();
    if !skip_root_check && !crate::platform::is_root() {
        return Err("hook deletion requires root: run again with `sudo aimx hooks delete`".into());
    }
    apply_delete_direct(config, name)?;
    println!(
        "{} {}",
        term::success("Hook deleted:"),
        term::highlight(name)
    );
    match crate::serve::sighup_running_daemon() {
        crate::serve::SighupOutcome::Sent(pid) => {
            println!(
                "{} SIGHUP sent to aimx serve (pid {pid}); change is live.",
                term::info("Reload:")
            );
        }
        _ => print_restart_hint(),
    }
    Ok(())
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
