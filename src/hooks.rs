//! `aimx hooks list | create | delete` CLI.
//!
//! `hook` (singular) is kept as a clap alias on the subcommand for muscle
//! memory. All three sub-subcommands route UDS-first so the daemon can
//! hot-swap its in-memory `Arc<Config>` (S51-3); on socket-missing the
//! CLI falls back to a direct `config.toml` edit + the Sprint 44 restart
//! hint. `create` is flag-based and auto-generates the 12-char hook id.
//! `delete <id>` shows the hook's mailbox / event / cmd and prompts
//! `[y/N]` unless `--yes`.

use std::io::{self, Write};

use crate::cli::{HookCommand, HookCreateArgs};
use crate::config::{Config, validate_hooks};
use crate::hook::{Hook, HookEvent, generate_hook_id};
use crate::hook_client::{
    HookCrudFallback, submit_hook_create_via_daemon, submit_hook_delete_via_daemon,
};
use crate::term;

pub fn run(cmd: HookCommand, config: Config) -> Result<(), Box<dyn std::error::Error>> {
    match cmd {
        HookCommand::List { mailbox } => list(&config, mailbox.as_deref()),
        HookCommand::Create(args) => create(&config, args),
        HookCommand::Delete { id, yes } => delete(&config, &id, yes),
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
            .then_with(|| a.id.cmp(&b.id))
    });

    if rows.is_empty() {
        println!("No hooks configured.");
        return Ok(());
    }

    println!(
        "{} {} {} {} {}",
        term::header("ID          "),
        term::header("MAILBOX             "),
        term::header("EVENT       "),
        term::header("CMD                                                          "),
        term::header("FILTERS"),
    );
    for row in rows {
        println!(
            "{}  {:<20.20} {:<11} {:<60} {}",
            term::highlight(&row.id),
            row.mailbox,
            row.event,
            truncate_with_ellipsis(&row.cmd, 60),
            row.filters,
        );
    }
    Ok(())
}

fn create(config: &Config, args: HookCreateArgs) -> Result<(), Box<dyn std::error::Error>> {
    if !config.mailboxes.contains_key(&args.mailbox) {
        return Err(format!("Mailbox '{}' does not exist", args.mailbox).into());
    }

    validate_flag_combinations(&args)?;

    let event = parse_event(&args.event)?;
    let id = generate_unique_hook_id(config);
    let hook = Hook {
        id: id.clone(),
        event,
        r#type: "cmd".to_string(),
        cmd: args.cmd,
        from: args.from,
        to: args.to,
        subject: args.subject,
        has_attachment: if args.has_attachment {
            Some(true)
        } else {
            None
        },
        dangerously_support_untrusted: args.dangerously_support_untrusted,
    };

    match submit_hook_create_via_daemon(&args.mailbox, &hook) {
        Ok(()) => {
            println!(
                "{} {}",
                term::success("Hook created:"),
                term::highlight(&id)
            );
            Ok(())
        }
        Err(HookCrudFallback::SocketMissing) => {
            apply_create_direct(config, &args.mailbox, hook)?;
            println!(
                "{} {}",
                term::success("Hook created:"),
                term::highlight(&id)
            );
            print_restart_hint();
            Ok(())
        }
        Err(HookCrudFallback::Daemon(msg)) => Err(msg.into()),
    }
}

fn delete(config: &Config, id: &str, yes: bool) -> Result<(), Box<dyn std::error::Error>> {
    let (mailbox, hook) = match find_hook(config, id) {
        Some(pair) => pair,
        None => return Err(format!("Hook id '{id}' not found").into()),
    };

    if !yes {
        println!("{}", term::warn("About to delete hook:"));
        println!("  {}   {}", term::header("id:     "), term::highlight(id));
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

    match submit_hook_delete_via_daemon(id) {
        Ok(()) => {
            println!("{} {}", term::success("Hook deleted:"), term::highlight(id));
            Ok(())
        }
        Err(HookCrudFallback::SocketMissing) => {
            apply_delete_direct(config, id)?;
            println!("{} {}", term::success("Hook deleted:"), term::highlight(id));
            print_restart_hint();
            Ok(())
        }
        Err(HookCrudFallback::Daemon(msg)) => Err(msg.into()),
    }
}

/// Public-for-test helper: validate event × filter flag combinations
/// against the same rules [`crate::config::validate_hooks`] enforces on
/// load. Returns `Ok(())` when the combo is legal.
pub(crate) fn validate_flag_combinations(
    args: &HookCreateArgs,
) -> Result<(), Box<dyn std::error::Error>> {
    let event = parse_event(&args.event)?;
    match event {
        HookEvent::OnReceive => {
            if args.to.is_some() {
                return Err("--to is only valid on --event after_send (inbound hooks \
                     filter on --from)"
                    .into());
            }
        }
        HookEvent::AfterSend => {
            if args.from.is_some() {
                return Err(
                    "--from is only valid on --event on_receive (outbound hooks \
                     filter on --to)"
                        .into(),
                );
            }
            if args.has_attachment {
                return Err("--has-attachment is only valid on --event on_receive \
                     (outbound submissions via UDS are text-only in v0.2)"
                    .into());
            }
            if args.dangerously_support_untrusted {
                return Err("--dangerously-support-untrusted is only valid on --event \
                     on_receive"
                    .into());
            }
        }
    }
    if args.cmd.trim().is_empty() {
        return Err("--cmd must not be empty".into());
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

fn generate_unique_hook_id(config: &Config) -> String {
    loop {
        let candidate = generate_hook_id();
        if !config
            .mailboxes
            .values()
            .any(|mb| mb.hooks.iter().any(|h| h.id == candidate))
        {
            return candidate;
        }
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

fn apply_delete_direct(config: &Config, id: &str) -> Result<(), Box<dyn std::error::Error>> {
    let mut new_config = config.clone();
    let mut removed = false;
    for mb in new_config.mailboxes.values_mut() {
        let before = mb.hooks.len();
        mb.hooks.retain(|h| h.id != id);
        if mb.hooks.len() != before {
            removed = true;
            break;
        }
    }
    if !removed {
        return Err(format!("Hook id '{id}' not found").into());
    }
    new_config.save(&crate::config::config_path())?;
    Ok(())
}

fn find_hook<'a>(config: &'a Config, id: &str) -> Option<(String, &'a Hook)> {
    for (name, mb) in &config.mailboxes {
        for h in &mb.hooks {
            if h.id == id {
                return Some((name.clone(), h));
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
    id: String,
    mailbox: String,
    event: &'static str,
    cmd: String,
    filters: String,
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
                id: h.id.clone(),
                mailbox: name.clone(),
                event: match h.event {
                    HookEvent::OnReceive => "on_receive",
                    HookEvent::AfterSend => "after_send",
                },
                cmd: h.cmd.clone(),
                filters: compact_filters(h),
            });
        }
    }
    rows
}

/// Build a compact one-line representation of the filter set for list
/// output and `mailboxes show`. Empty when no filters are set.
pub(crate) fn compact_filters(hook: &Hook) -> String {
    let mut parts = Vec::new();
    if let Some(p) = hook.from.as_deref() {
        parts.push(format!("from={p}"));
    }
    if let Some(p) = hook.to.as_deref() {
        parts.push(format!("to={p}"));
    }
    if let Some(p) = hook.subject.as_deref() {
        parts.push(format!("subject={p}"));
    }
    if let Some(b) = hook.has_attachment {
        parts.push(format!("has_attachment={b}"));
    }
    if hook.dangerously_support_untrusted {
        parts.push("dangerously_support_untrusted=true".into());
    }
    parts.join(" ")
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
            from: None,
            to: None,
            subject: None,
            has_attachment: false,
            dangerously_support_untrusted: false,
        }
    }

    #[test]
    fn validate_rejects_to_on_on_receive() {
        let args = HookCreateArgs {
            to: Some("*@example.com".into()),
            ..base_args("on_receive")
        };
        let err = validate_flag_combinations(&args).unwrap_err().to_string();
        assert!(err.contains("--to"), "{err}");
    }

    #[test]
    fn validate_rejects_from_on_after_send() {
        let args = HookCreateArgs {
            from: Some("*@example.com".into()),
            ..base_args("after_send")
        };
        let err = validate_flag_combinations(&args).unwrap_err().to_string();
        assert!(err.contains("--from"), "{err}");
    }

    #[test]
    fn validate_rejects_has_attachment_on_after_send() {
        let args = HookCreateArgs {
            has_attachment: true,
            ..base_args("after_send")
        };
        let err = validate_flag_combinations(&args).unwrap_err().to_string();
        assert!(err.contains("--has-attachment"), "{err}");
    }

    #[test]
    fn validate_rejects_dangerous_on_after_send() {
        let args = HookCreateArgs {
            dangerously_support_untrusted: true,
            ..base_args("after_send")
        };
        let err = validate_flag_combinations(&args).unwrap_err().to_string();
        assert!(err.contains("--dangerously-support-untrusted"), "{err}");
    }

    #[test]
    fn validate_rejects_empty_cmd() {
        let args = HookCreateArgs {
            cmd: "   ".into(),
            ..base_args("on_receive")
        };
        let err = validate_flag_combinations(&args).unwrap_err().to_string();
        assert!(err.contains("--cmd"), "{err}");
    }

    #[test]
    fn validate_accepts_on_receive_with_from_subject_has_attachment() {
        let args = HookCreateArgs {
            from: Some("*@gmail.com".into()),
            subject: Some("urgent".into()),
            has_attachment: true,
            dangerously_support_untrusted: true,
            ..base_args("on_receive")
        };
        validate_flag_combinations(&args).unwrap();
    }

    #[test]
    fn validate_accepts_after_send_with_to_subject() {
        let args = HookCreateArgs {
            to: Some("*@client.com".into()),
            subject: Some("receipt".into()),
            ..base_args("after_send")
        };
        validate_flag_combinations(&args).unwrap();
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
    fn generate_unique_hook_id_avoids_existing() {
        // Seed config with every-possible-prefix IDs won't help; instead,
        // check that the generator at least returns a well-formed ID
        // distinct from any present in config. The odds of collision
        // across two random 12-char draws are ~1 in 36^12 — the loop will
        // terminate practically every time.
        let mut cfg = base_config();
        cfg.mailboxes.get_mut("alice").unwrap().hooks.push(Hook {
            id: "aaaabbbbcccc".into(),
            event: HookEvent::OnReceive,
            r#type: "cmd".into(),
            cmd: "x".into(),
            from: None,
            to: None,
            subject: None,
            has_attachment: None,
            dangerously_support_untrusted: false,
        });
        let id = generate_unique_hook_id(&cfg);
        assert_ne!(id, "aaaabbbbcccc");
        assert_eq!(id.chars().count(), 12);
    }

    #[test]
    fn compact_filters_emits_stable_order() {
        let hook = Hook {
            id: "aaaabbbbcccc".into(),
            event: HookEvent::OnReceive,
            r#type: "cmd".into(),
            cmd: "x".into(),
            from: Some("*@gmail.com".into()),
            to: None,
            subject: Some("urgent".into()),
            has_attachment: Some(true),
            dangerously_support_untrusted: true,
        };
        let out = compact_filters(&hook);
        assert_eq!(
            out,
            "from=*@gmail.com subject=urgent has_attachment=true \
             dangerously_support_untrusted=true"
        );
    }

    #[test]
    fn compact_filters_empty_when_nothing_set() {
        let hook = Hook {
            id: "aaaabbbbcccc".into(),
            event: HookEvent::OnReceive,
            r#type: "cmd".into(),
            cmd: "x".into(),
            from: None,
            to: None,
            subject: None,
            has_attachment: None,
            dangerously_support_untrusted: false,
        };
        assert_eq!(compact_filters(&hook), "");
    }
}
