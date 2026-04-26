//! `aimx hooks list | create | delete` CLI.
//!
//! `hook` (singular) is kept as a clap alias on the subcommand for muscle
//! memory.
//!
//! Create splits two paths by flag:
//!
//! * `--template NAME --param KEY=VAL ...` — template-bound hook. Goes
//!   through the daemon UDS (`HOOK-CREATE` verb) so `Arc<Config>`
//!   hot-swaps and the new hook fires on the next event. On
//!   socket-missing we fall back to a direct `config.toml` edit + a
//!   "restart aimx" hint. CLI-origin template hooks are tagged `origin =
//!   "operator"` — the operator typed the command, so the operator
//!   retains delete control (MCP cannot remove operator-origin hooks via
//!   UDS, see `hook_handler::handle_hook_delete`).
//!
//! * `--cmd "..."` — raw-cmd hook. Requires root (`sudo`). Writes
//!   `config.toml` directly (never UDS) and sends `SIGHUP` to the
//!   running `aimx serve` so the daemon picks up the change without a
//!   restart. If no daemon is running, prints a restart hint. Raw-cmd
//!   hooks are the only way to register an arbitrary shell command —
//!   which is why the UDS verb refuses them entirely.
//!
//! Delete resolves against effective names (explicit or derived) and
//! shows the hook's mailbox / event / cmd with a `[y/N]` prompt unless
//! `--yes`. Operator-origin hooks can only be deleted by a CLI caller;
//! the UDS verb refuses.

use std::collections::BTreeMap;
use std::io::{self, Write};

use crate::cli::{HookCommand, HookCreateArgs};
use crate::config::{Config, OrphanSkipContext, validate_hooks};
use crate::hook::{Hook, HookEvent, effective_hook_name, is_valid_hook_name};
use crate::hook_client::{
    HookCrudFallback, submit_hook_delete_via_daemon, submit_hook_template_create_via_daemon,
};
use crate::term;

pub fn run(cmd: HookCommand, config: Config) -> Result<(), Box<dyn std::error::Error>> {
    match cmd {
        HookCommand::List { mailbox } => list(&config, mailbox.as_deref()),
        HookCommand::Create(args) => create(&config, args),
        HookCommand::Delete { name, yes } => delete(&config, &name, yes),
        HookCommand::Templates => list_templates(&config, &mut io::stdout()),
        HookCommand::Prune { orphans, dry_run } => prune(&config, orphans, dry_run),
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

/// `aimx hooks templates` — print a 4-column table of enabled
/// templates (PRD §9 v1 scope).
///
/// Reads the loaded `Config` directly; no UDS round-trip needed for a
/// read-only query. Empty output prints a one-line hint pointing the
/// operator at `sudo aimx setup`. Output is sent to `out` so unit tests
/// can capture it without touching stdout.
fn list_templates(
    config: &Config,
    out: &mut dyn io::Write,
) -> Result<(), Box<dyn std::error::Error>> {
    if config.hook_templates.is_empty() {
        writeln!(
            out,
            "No hook templates enabled. Run `{}` to install one per agent.",
            term::highlight("aimx agents setup")
        )?;
        return Ok(());
    }

    // Render the header via the 4-column format used by `aimx hooks
    // list` so the two subcommands sit next to each other visually.
    writeln!(
        out,
        "{} {} {} {}",
        term::header("NAME                "),
        term::header("PARAMS                        "),
        term::header("EVENTS                 "),
        term::header("DESCRIPTION"),
    )?;

    let mut rows = config.hook_templates.clone();
    rows.sort_by(|a, b| a.name.cmp(&b.name));

    for tmpl in rows {
        let params = if tmpl.params.is_empty() {
            "-".to_string()
        } else {
            tmpl.params.join(",")
        };
        let events: Vec<&'static str> = tmpl.allowed_events.iter().map(|e| e.as_str()).collect();
        let events = events.join(",");
        writeln!(
            out,
            "{:<20.20} {:<30.30} {:<23.23} {}",
            term::highlight(&tmpl.name).to_string(),
            truncate_with_ellipsis(&params, 30),
            truncate_with_ellipsis(&events, 23),
            truncate_with_ellipsis(&tmpl.description, 60),
        )?;
    }
    Ok(())
}

fn create(config: &Config, args: HookCreateArgs) -> Result<(), Box<dyn std::error::Error>> {
    if !config.mailboxes.contains_key(&args.mailbox) {
        return Err(format!("Mailbox '{}' does not exist", args.mailbox).into());
    }

    // `ArgGroup` on `HookCreateArgs` guarantees exactly one of
    // `--template` / `--cmd` is set; match on that.
    match (&args.template, &args.cmd) {
        (Some(template), None) => create_template(config, &args, template.clone()),
        (None, Some(cmd)) => create_raw_cmd(config, &args, cmd.clone()),
        _ => Err(
            "exactly one of --template NAME or --cmd \"...\" must be supplied (not both, not neither)"
                .into(),
        ),
    }
}

// ---- template path -------------------------------------------------

fn create_template(
    config: &Config,
    args: &HookCreateArgs,
    template: String,
) -> Result<(), Box<dyn std::error::Error>> {
    validate_create_args_common(args)?;

    let event = parse_event(&args.event)?;

    // Parse --param K=V pairs into a BTreeMap and fail fast on malformed
    // values so the operator doesn't get a cryptic daemon error.
    let params = parse_params(&args.params)?;

    // Resolve the template locally for pre-flight friendliness. The
    // daemon re-validates, but local validation gives better error
    // messages before the UDS round-trip (and also catches
    // socket-missing installs that are about to fall back).
    let tmpl = config
        .hook_templates
        .iter()
        .find(|t| t.name == template)
        .ok_or_else(|| -> Box<dyn std::error::Error> {
            format!(
                "unknown template '{template}': run `aimx hooks templates` to \
                 list enabled templates, or enable one via `sudo aimx setup`"
            )
            .into()
        })?;

    // Check allowed_events for this template.
    if !tmpl.allowed_events.contains(&event) {
        return Err(format!(
            "template '{template}' does not allow event '{}' (allowed: {})",
            event.as_str(),
            tmpl.allowed_events
                .iter()
                .map(|e| e.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )
        .into());
    }

    // Check the params set: declared-but-missing and unknown-param.
    for required in &tmpl.params {
        if !params.contains_key(required) {
            return Err(format!(
                "missing --param {required}=... (template '{template}' declares it)"
            )
            .into());
        }
    }
    for k in params.keys() {
        if !tmpl.params.iter().any(|p| p == k) {
            return Err(format!(
                "template '{template}' does not declare parameter '{k}' \
                 (declared: {})",
                tmpl.params.join(", ")
            )
            .into());
        }
    }

    // Submit via UDS. The daemon always stamps `origin = "mcp"` on the
    // resulting hook, even for CLI callers — a deliberate simplification
    // so the audit story stays "anything the daemon writes from UDS is
    // MCP-origin". Operators who want `origin = "operator"` on the hook
    // can drop the `--template` flag and use `--cmd` (which writes
    // `config.toml` directly and preserves operator origin).
    let hook_for_preview = Hook {
        name: args.name.clone(),
        event,
        r#type: "cmd".into(),
        cmd: String::new(),
        dangerously_support_untrusted: false,
        origin: crate::hook::HookOrigin::Mcp,
        template: Some(template.clone()),
        params: params.clone(),
        run_as: None,
    };
    let effective = effective_hook_name(&hook_for_preview);

    match submit_hook_template_create_via_daemon(
        &args.mailbox,
        event,
        &template,
        params.clone(),
        args.name.as_deref(),
    ) {
        Ok(()) => {
            println!(
                "{} {}",
                term::success("Hook created:"),
                term::highlight(&effective)
            );
            Ok(())
        }
        Err(HookCrudFallback::SocketMissing) => {
            // Fallback: write directly to config.toml with the same
            // `origin = "mcp"` the daemon would have stamped (keeps the
            // file identical regardless of whether the daemon was up).
            apply_create_direct(config, &args.mailbox, hook_for_preview)?;
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

// ---- raw-cmd path --------------------------------------------------

fn create_raw_cmd(
    config: &Config,
    args: &HookCreateArgs,
    cmd: String,
) -> Result<(), Box<dyn std::error::Error>> {
    // Test-only escape hatch: CI runs non-root, but integration tests
    // exercise the raw-cmd path to verify the direct-write + SIGHUP
    // flow. Production never sets this env var; the daemon's runtime
    // env is scrubbed by systemd. The same pattern is used by
    // `platform::spawn_sandboxed` (AIMX_SANDBOX_FORCE_FALLBACK).
    let skip_root_check = std::env::var_os("AIMX_TEST_SKIP_ROOT_CHECK").is_some();
    if !skip_root_check && !crate::platform::is_root() {
        return Err(
            "--cmd hooks require root: run again with `sudo aimx hooks create --cmd ...`.\n\
             Raw-cmd hooks are operator-only and never traverse the UDS socket. \
             To let an agent create a hook, install a `[[hook_template]]` via \
             `sudo aimx setup` and use `--template` instead."
                .into(),
        );
    }

    validate_create_args_common(args)?;
    if cmd.trim().is_empty() {
        return Err("--cmd must not be empty".into());
    }
    let event = parse_event(&args.event)?;
    if matches!(event, HookEvent::AfterSend) && args.dangerously_support_untrusted {
        return Err("--dangerously-support-untrusted is only valid on --event on_receive".into());
    }

    let hook = Hook {
        name: args.name.clone(),
        event,
        r#type: "cmd".to_string(),
        cmd,
        dangerously_support_untrusted: args.dangerously_support_untrusted,
        origin: crate::hook::HookOrigin::Operator,
        template: None,
        params: BTreeMap::new(),
        run_as: None,
    };
    let effective = effective_hook_name(&hook);

    apply_create_direct(config, &args.mailbox, hook)?;
    println!(
        "{} {}",
        term::success("Hook created:"),
        term::highlight(&effective)
    );

    // SIGHUP the running daemon (if any) so the new hook fires on the
    // next event without a full restart.
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
    let is_operator_origin = hook.origin == crate::hook::HookOrigin::Operator;

    if !yes {
        println!("{}", term::warn("About to delete hook:"));
        println!("  {}   {}", term::header("name:   "), term::highlight(name));
        println!("  {}   {}", term::header("mailbox:"), mailbox);
        println!("  {}   {}", term::header("event:  "), hook.event);
        if hook.template.is_some() {
            println!(
                "  {}   {}",
                term::header("template:"),
                hook.template.clone().unwrap_or_default()
            );
        } else {
            println!(
                "  {}   {}",
                term::header("cmd:    "),
                truncate_with_ellipsis(&hook.cmd, 60)
            );
        }
        print!("Continue? [y/N] ");
        io::stdout().flush()?;
        let mut input = String::new();
        io::stdin().read_line(&mut input)?;
        if !input.trim().eq_ignore_ascii_case("y") {
            println!("Cancelled.");
            return Ok(());
        }
    }

    // Operator-origin hooks are UDS-protected (see
    // `hook_handler::handle_hook_delete`). Route them through the
    // direct-write path, which requires root anyway. MCP-origin hooks
    // try UDS first (fast, hot-swap) and fall back on socket-missing.
    let skip_root_check = std::env::var_os("AIMX_TEST_SKIP_ROOT_CHECK").is_some();
    if is_operator_origin {
        if !skip_root_check && !crate::platform::is_root() {
            return Err(
                "hook is operator-origin and requires root to delete: run again with \
                 `sudo aimx hooks delete`"
                    .into(),
            );
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
        return Ok(());
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

/// `aimx hooks prune --orphans` — remove templates and hooks whose
/// `run_as` user no longer resolves on this host. Root-only. Refuses
/// when `aimx doctor` reports any non-orphan `Fail` finding so the
/// operator fixes the underlying breakage before pruning.
fn prune(config: &Config, orphans: bool, dry_run: bool) -> Result<(), Box<dyn std::error::Error>> {
    if !orphans {
        return Err(
            "`aimx hooks prune` currently requires `--orphans` to be set explicitly; \
             no other prune scopes are implemented yet"
                .into(),
        );
    }

    let skip_root_check = std::env::var_os("AIMX_TEST_SKIP_ROOT_CHECK").is_some();
    if !skip_root_check && !crate::platform::is_root() {
        return Err("hooks prune --orphans requires root: run again with \
             `sudo aimx hooks prune --orphans`"
            .into());
    }

    prune_preflight_check(config)
        .map_err(|r| -> Box<dyn std::error::Error> { r.message.into() })?;

    let plan = build_prune_plan(config);
    if plan.is_empty() {
        println!(
            "{}",
            term::success("No orphan templates or hooks to prune.")
        );
        return Ok(());
    }

    print_prune_plan(&plan);

    if dry_run {
        println!(
            "{} dry-run only; config.toml not modified.",
            term::info("Note:"),
        );
        return Ok(());
    }

    // Apply. Writes a temp file in the config's parent, fsyncs, then
    // renames over the target. ConfigHandle::store is the daemon's
    // in-memory view; the CLI only owns a snapshot, so we emit a
    // restart/SIGHUP hint so operators know how to activate the new
    // config without bouncing the whole service.
    let mut new_config = config.clone();
    apply_prune_plan(&mut new_config, &plan);

    let path = crate::config::config_path();
    crate::mailbox_handler::write_config_atomic(&path, &new_config).map_err(
        |e| -> Box<dyn std::error::Error> { format!("failed to rewrite config.toml: {e}").into() },
    )?;

    println!(
        "{} {}",
        term::success("Pruned:"),
        format_prune_summary(&plan),
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

/// Refusal returned by [`prune_preflight_check`] when doctor reports a
/// non-orphan `Fail` finding. The operator has to fix the underlying
/// error first because a bad hook invariant or missing dir indicates
/// config or filesystem damage that the prune command has no business
/// silently rewriting around.
#[derive(Debug)]
struct PruneRefusal {
    message: String,
}

/// Pre-flight: run doctor's checks and refuse when any `Fail` finding
/// is outside [`crate::doctor::ORPHAN_CHECK_IDS`]. Extracted so the
/// refusal logic can be unit-tested independently of the root check,
/// atomic write, and SIGHUP plumbing in [`prune`].
fn prune_preflight_check(config: &Config) -> Result<(), PruneRefusal> {
    let load_warnings = match crate::config::Config::load_resolved() {
        Ok((_, w)) => w,
        Err(_) => Vec::new(),
    };
    let findings = crate::doctor::run_checks(config, &load_warnings);
    let non_orphan_fails: Vec<&crate::doctor::DoctorFinding> = findings
        .iter()
        .filter(|f| f.severity == crate::doctor::FindingSeverity::Fail)
        .filter(|f| !crate::doctor::ORPHAN_CHECK_IDS.contains(&f.check))
        .collect();
    if non_orphan_fails.is_empty() {
        return Ok(());
    }
    let mut msg =
        String::from("Config has non-orphan failures. Fix those first, then re-run prune.\n");
    for f in &non_orphan_fails {
        msg.push_str(&format!("  - [{}] {}\n", f.check, f.message));
    }
    Err(PruneRefusal { message: msg })
}

/// Deterministic, sorted description of what `prune --orphans` will
/// remove. The `template_names` vector lists orphan template names;
/// `hooks_by_mailbox` is a sorted list of (mailbox, Vec<hook_name>).
#[derive(Debug, Default)]
struct PrunePlan {
    template_names: Vec<String>,
    hooks_by_mailbox: Vec<(String, Vec<String>)>,
}

impl PrunePlan {
    fn is_empty(&self) -> bool {
        self.template_names.is_empty() && self.hooks_by_mailbox.is_empty()
    }

    fn total_hooks(&self) -> usize {
        self.hooks_by_mailbox
            .iter()
            .map(|(_, hooks)| hooks.len())
            .sum()
    }
}

fn build_prune_plan(config: &Config) -> PrunePlan {
    use crate::config::is_reserved_run_as;
    use crate::user_resolver::resolve_user;

    // Sorted for deterministic output in both diff preview and tests.
    let mut orphan_templates: Vec<String> = config
        .hook_templates
        .iter()
        .filter(|t| !is_reserved_run_as(&t.run_as) && resolve_user(&t.run_as).is_none())
        .map(|t| t.name.clone())
        .collect();
    orphan_templates.sort();
    orphan_templates.dedup();

    let orphan_template_set: std::collections::HashSet<&str> =
        orphan_templates.iter().map(String::as_str).collect();

    let mut hooks_by_mailbox: Vec<(String, Vec<String>)> = Vec::new();
    let mut mailbox_names: Vec<&String> = config.mailboxes.keys().collect();
    mailbox_names.sort();
    for mb_name in mailbox_names {
        let mb = &config.mailboxes[mb_name];
        let mut orphan_hook_names: Vec<String> = Vec::new();
        for hook in &mb.hooks {
            let effective = effective_hook_name(hook);
            let template_orphaned = hook
                .template
                .as_ref()
                .is_some_and(|t| orphan_template_set.contains(t.as_str()));
            let run_as_orphaned = hook_run_as_is_orphan(config, hook);
            if template_orphaned || run_as_orphaned {
                orphan_hook_names.push(effective);
            }
        }
        orphan_hook_names.sort();
        if !orphan_hook_names.is_empty() {
            hooks_by_mailbox.push((mb_name.clone(), orphan_hook_names));
        }
    }

    PrunePlan {
        template_names: orphan_templates,
        hooks_by_mailbox,
    }
}

/// True when the hook's effective `run_as` (explicit or inherited from
/// its template) does not resolve via `getpwnam`. Reserved values
/// (`root`, `aimx-catchall`) always resolve for prune's purposes.
fn hook_run_as_is_orphan(config: &Config, hook: &crate::hook::Hook) -> bool {
    use crate::config::is_reserved_run_as;
    use crate::user_resolver::resolve_user;

    let explicit = hook.run_as.clone();
    let inherited = hook.template.as_ref().and_then(|tmpl_name| {
        config
            .hook_templates
            .iter()
            .find(|t| &t.name == tmpl_name)
            .map(|t| t.run_as.clone())
    });
    let effective = explicit.or(inherited);
    let Some(name) = effective else {
        return false;
    };
    if is_reserved_run_as(&name) {
        return false;
    }
    resolve_user(&name).is_none()
}

fn apply_prune_plan(config: &mut Config, plan: &PrunePlan) {
    let template_set: std::collections::HashSet<&str> =
        plan.template_names.iter().map(String::as_str).collect();
    config
        .hook_templates
        .retain(|t| !template_set.contains(t.name.as_str()));

    for (mb_name, hook_names) in &plan.hooks_by_mailbox {
        let hook_set: std::collections::HashSet<&str> =
            hook_names.iter().map(String::as_str).collect();
        if let Some(mb) = config.mailboxes.get_mut(mb_name) {
            mb.hooks
                .retain(|h| !hook_set.contains(effective_hook_name(h).as_str()));
        }
    }
}

fn print_prune_plan(plan: &PrunePlan) {
    println!(
        "{} proposed prune diff:",
        term::header("aimx hooks prune --orphans"),
    );
    if !plan.template_names.is_empty() {
        println!("  {}", term::warn("Templates to remove:"));
        for name in &plan.template_names {
            println!("    - {}", term::highlight(name));
        }
    }
    if !plan.hooks_by_mailbox.is_empty() {
        println!("  {}", term::warn("Hooks to remove:"));
        for (mb, names) in &plan.hooks_by_mailbox {
            for name in names {
                println!("    - {}.{}", term::dim(mb), term::highlight(name));
            }
        }
    }
}

fn format_prune_summary(plan: &PrunePlan) -> String {
    let templates_phrase = if plan.template_names.is_empty() {
        "0 templates".to_string()
    } else {
        format!(
            "{} templates ({})",
            plan.template_names.len(),
            plan.template_names.join(", "),
        )
    };
    format!(
        "Removed {} and {} hooks from {} mailboxes",
        templates_phrase,
        plan.total_hooks(),
        plan.hooks_by_mailbox.len(),
    )
}

/// Common pre-submission validation shared between the template and
/// raw-cmd create paths.
fn validate_create_args_common(args: &HookCreateArgs) -> Result<(), Box<dyn std::error::Error>> {
    // `--event` value is already restricted by clap's `value_parser`, but
    // re-parse here so the caller can handle the enum.
    let _ = parse_event(&args.event)?;
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

fn parse_params(params: &[String]) -> Result<BTreeMap<String, String>, Box<dyn std::error::Error>> {
    let mut out: BTreeMap<String, String> = BTreeMap::new();
    for raw in params {
        let (k, v) = raw
            .split_once('=')
            .ok_or_else(|| format!("--param '{raw}' must be KEY=VAL (got no '=' separator)"))?;
        if k.is_empty() {
            return Err(format!("--param '{raw}' has empty KEY").into());
        }
        if out.insert(k.to_string(), v.to_string()).is_some() {
            return Err(format!("--param '{k}' specified twice").into());
        }
    }
    Ok(out)
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
    // Fresh create path: orphan-skip does not apply — if the operator
    // typed a missing user, surface the invariant error now, not at
    // next daemon restart.
    validate_hooks(&new_config, &OrphanSkipContext::strict())
        .map_err(|e| -> Box<dyn std::error::Error> { e.into() })?;
    // Use the same temp-then-rename helper the pruner and daemon use so
    // the CLI fallback path is crash-safe.
    let path = crate::config::config_path();
    crate::mailbox_handler::write_config_atomic(&path, &new_config).map_err(
        |e| -> Box<dyn std::error::Error> { format!("failed to rewrite config.toml: {e}").into() },
    )?;
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
    // Atomic write-temp-then-rename; symmetric with apply_create_direct
    // and the pruner at the top of this module (hardening PRD §6.4).
    let path = crate::config::config_path();
    crate::mailbox_handler::write_config_atomic(&path, &new_config).map_err(
        |e| -> Box<dyn std::error::Error> { format!("failed to rewrite config.toml: {e}").into() },
    )?;
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
            // Template-bound hooks render the template name in the CMD
            // column since `cmd` is empty on those.
            let cmd = if let Some(t) = &h.template {
                format!("template: {t}")
            } else {
                h.cmd.clone()
            };
            rows.push(Row {
                name: effective_hook_name(h),
                mailbox: name.clone(),
                event: match h.event {
                    HookEvent::OnReceive => "on_receive",
                    HookEvent::AfterSend => "after_send",
                },
                cmd,
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
    use crate::config::{HookTemplate, HookTemplateStdin, MailboxConfig};
    use std::collections::HashMap;
    use std::path::{Path, PathBuf};

    fn base_config() -> Config {
        let mut mailboxes = HashMap::new();
        mailboxes.insert(
            "catchall".to_string(),
            MailboxConfig {
                address: "*@test.com".to_string(),
                owner: "aimx-catchall".to_string(),
                hooks: vec![],
                trust: None,
                trusted_senders: None,
                allow_root_catchall: false,
            },
        );
        mailboxes.insert(
            "alice".to_string(),
            MailboxConfig {
                address: "alice@test.com".to_string(),
                owner: "root".to_string(),
                hooks: vec![],
                trust: None,
                trusted_senders: None,
                allow_root_catchall: false,
            },
        );
        Config {
            domain: "test.com".to_string(),
            data_dir: PathBuf::from("/tmp"),
            dkim_selector: "aimx".to_string(),
            trust: "none".to_string(),
            trusted_senders: vec![],
            hook_templates: vec![HookTemplate {
                name: "invoke-claude".into(),
                description: "test".into(),
                cmd: vec!["/usr/local/bin/claude".into(), "{prompt}".into()],
                params: vec!["prompt".into()],
                stdin: HookTemplateStdin::Email,
                run_as: "aimx-hook".into(),
                timeout_secs: 60,
                allowed_events: vec![HookEvent::OnReceive, HookEvent::AfterSend],
            }],
            mailboxes,
            verify_host: None,
            enable_ipv6: false,
            upgrade: None,
        }
    }

    fn base_args(event: &str) -> HookCreateArgs {
        HookCreateArgs {
            mailbox: "alice".into(),
            event: event.into(),
            cmd: Some("echo hi".into()),
            template: None,
            params: vec![],
            name: None,
            dangerously_support_untrusted: false,
        }
    }

    /// End-to-end validation of the raw-cmd CLI create path: the
    /// common validator (shared with `--template`) plus the two raw-cmd
    /// specific invariants (`--cmd` non-empty, `--dangerously-…` only
    /// on `on_receive`). Inlined here so tests no longer depend on a
    /// back-compat shim in `src/hooks.rs`.
    fn validate_raw_cmd_args(args: &HookCreateArgs) -> Result<(), Box<dyn std::error::Error>> {
        validate_create_args_common(args)?;
        let event = parse_event(&args.event)?;
        if let Some(cmd) = &args.cmd {
            if matches!(event, HookEvent::AfterSend) && args.dangerously_support_untrusted {
                return Err(
                    "--dangerously-support-untrusted is only valid on --event on_receive".into(),
                );
            }
            if cmd.trim().is_empty() {
                return Err("--cmd must not be empty".into());
            }
        }
        Ok(())
    }

    #[test]
    fn validate_rejects_dangerous_on_after_send() {
        let args = HookCreateArgs {
            dangerously_support_untrusted: true,
            ..base_args("after_send")
        };
        let err = validate_raw_cmd_args(&args).unwrap_err().to_string();
        assert!(err.contains("--dangerously-support-untrusted"), "{err}");
    }

    #[test]
    fn validate_rejects_empty_cmd() {
        let args = HookCreateArgs {
            cmd: Some("   ".into()),
            ..base_args("on_receive")
        };
        let err = validate_raw_cmd_args(&args).unwrap_err().to_string();
        assert!(err.contains("--cmd"), "{err}");
    }

    #[test]
    fn validate_rejects_invalid_name() {
        let args = HookCreateArgs {
            name: Some("bad name!".into()),
            ..base_args("on_receive")
        };
        let err = validate_create_args_common(&args).unwrap_err().to_string();
        assert!(err.contains("--name"), "{err}");
    }

    #[test]
    fn validate_accepts_valid_name() {
        let args = HookCreateArgs {
            name: Some("nightly_summary".into()),
            ..base_args("on_receive")
        };
        validate_create_args_common(&args).unwrap();
    }

    #[test]
    fn validate_accepts_anonymous() {
        validate_create_args_common(&base_args("on_receive")).unwrap();
    }

    #[test]
    fn parse_params_happy_path() {
        let pairs = vec!["prompt=hello world".into(), "foo=bar".into()];
        let parsed = parse_params(&pairs).unwrap();
        assert_eq!(
            parsed.get("prompt").map(String::as_str),
            Some("hello world")
        );
        assert_eq!(parsed.get("foo").map(String::as_str), Some("bar"));
    }

    #[test]
    fn parse_params_allows_equals_in_value() {
        let pairs = vec!["token=a=b=c".into()];
        let parsed = parse_params(&pairs).unwrap();
        assert_eq!(parsed.get("token").map(String::as_str), Some("a=b=c"));
    }

    #[test]
    fn parse_params_rejects_missing_separator() {
        let pairs = vec!["no-equals-here".into()];
        let err = parse_params(&pairs).unwrap_err().to_string();
        assert!(err.contains("KEY=VAL"), "{err}");
    }

    #[test]
    fn parse_params_rejects_empty_key() {
        let pairs = vec!["=value".into()];
        let err = parse_params(&pairs).unwrap_err().to_string();
        assert!(err.contains("empty KEY"), "{err}");
    }

    #[test]
    fn parse_params_rejects_duplicate_key() {
        let pairs = vec!["foo=1".into(), "foo=2".into()];
        let err = parse_params(&pairs).unwrap_err().to_string();
        assert!(err.contains("twice"), "{err}");
    }

    #[test]
    fn create_template_rejects_missing_param() {
        let cfg = base_config();
        let args = HookCreateArgs {
            mailbox: "alice".into(),
            event: "on_receive".into(),
            cmd: None,
            template: Some("invoke-claude".into()),
            params: vec![],
            name: None,
            dangerously_support_untrusted: false,
        };
        let err = create_template(&cfg, &args, "invoke-claude".into())
            .unwrap_err()
            .to_string();
        assert!(err.contains("missing --param"), "{err}");
        assert!(err.contains("prompt"), "{err}");
    }

    #[test]
    fn create_template_rejects_unknown_template() {
        let cfg = base_config();
        let args = HookCreateArgs {
            mailbox: "alice".into(),
            event: "on_receive".into(),
            cmd: None,
            template: Some("nope".into()),
            params: vec!["prompt=hi".into()],
            name: None,
            dangerously_support_untrusted: false,
        };
        let err = create_template(&cfg, &args, "nope".into())
            .unwrap_err()
            .to_string();
        assert!(err.contains("unknown template"), "{err}");
    }

    #[test]
    fn create_template_rejects_unknown_param() {
        let cfg = base_config();
        let args = HookCreateArgs {
            mailbox: "alice".into(),
            event: "on_receive".into(),
            cmd: None,
            template: Some("invoke-claude".into()),
            params: vec!["prompt=hi".into(), "bogus=1".into()],
            name: None,
            dangerously_support_untrusted: false,
        };
        let err = create_template(&cfg, &args, "invoke-claude".into())
            .unwrap_err()
            .to_string();
        assert!(err.contains("does not declare parameter 'bogus'"), "{err}");
    }

    #[test]
    fn create_template_rejects_event_not_allowed() {
        let mut cfg = base_config();
        cfg.hook_templates[0].allowed_events = vec![HookEvent::OnReceive];
        let args = HookCreateArgs {
            mailbox: "alice".into(),
            event: "after_send".into(),
            cmd: None,
            template: Some("invoke-claude".into()),
            params: vec!["prompt=hi".into()],
            name: None,
            dangerously_support_untrusted: false,
        };
        let err = create_template(&cfg, &args, "invoke-claude".into())
            .unwrap_err()
            .to_string();
        assert!(err.contains("does not allow event"), "{err}");
    }

    // ----- `aimx hooks templates` ----------------------------------------

    fn capture_list_templates(config: &Config) -> String {
        let mut buf: Vec<u8> = Vec::new();
        super::list_templates(config, &mut buf).unwrap();
        String::from_utf8(buf).unwrap()
    }

    #[test]
    fn list_templates_empty_prints_setup_hint() {
        let mut cfg = base_config();
        cfg.hook_templates.clear();
        let out = capture_list_templates(&cfg);
        assert!(out.contains("No hook templates enabled"), "{out}");
        assert!(out.contains("aimx agents setup"), "{out}");
    }

    #[test]
    fn list_templates_single_template_renders_table() {
        let cfg = base_config(); // has invoke-claude
        let out = capture_list_templates(&cfg);
        assert!(out.contains("NAME"), "header missing: {out}");
        assert!(out.contains("PARAMS"), "header missing: {out}");
        assert!(out.contains("EVENTS"), "header missing: {out}");
        assert!(out.contains("DESCRIPTION"), "header missing: {out}");
        assert!(out.contains("invoke-claude"), "name missing: {out}");
        assert!(out.contains("prompt"), "param missing: {out}");
        assert!(out.contains("on_receive"), "event missing: {out}");
        assert!(out.contains("after_send"), "event missing: {out}");
    }

    #[test]
    fn list_templates_bundled_defaults_render() {
        // Every `invoke-*` block has been stripped from
        // `hook-templates/defaults.toml`; only `webhook` remains
        // pre-bundled. Per-agent templates are registered on demand
        // by `aimx agents setup`.
        let mut cfg = base_config();
        cfg.hook_templates = crate::hook_templates_defaults::default_templates();
        let out = capture_list_templates(&cfg);
        assert!(out.contains("webhook"), "missing webhook in output: {out}");
        for legacy in [
            "invoke-claude",
            "invoke-codex",
            "invoke-opencode",
            "invoke-gemini",
            "invoke-goose",
            "invoke-openclaw",
            "invoke-hermes",
        ] {
            assert!(
                !out.contains(legacy),
                "bundled defaults should no longer ship {legacy}: {out}"
            );
        }
    }

    #[test]
    fn list_templates_truncates_long_description() {
        let mut cfg = base_config();
        cfg.hook_templates[0].description = "x".repeat(200);
        let out = capture_list_templates(&cfg);
        // The ellipsis character marks truncation.
        assert!(
            out.contains('…'),
            "long description must be truncated: {out}"
        );
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
            origin: crate::hook::HookOrigin::Operator,
            template: None,
            params: BTreeMap::new(),
            run_as: None,
        });
        // Anonymous
        let anon = Hook {
            name: None,
            event: HookEvent::AfterSend,
            r#type: "cmd".into(),
            cmd: "echo anon".into(),
            dangerously_support_untrusted: false,
            origin: crate::hook::HookOrigin::Operator,
            template: None,
            params: BTreeMap::new(),
            run_as: None,
        };
        let anon_derived = effective_hook_name(&anon);
        cfg.mailboxes.get_mut("catchall").unwrap().hooks.push(anon);

        assert!(find_hook_by_effective_name(&cfg, "explicit_one").is_some());
        assert!(find_hook_by_effective_name(&cfg, &anon_derived).is_some());
        assert!(find_hook_by_effective_name(&cfg, "not_there").is_none());
    }

    // -------------------- hooks prune --orphans ------------------------------

    use crate::user_resolver::{ResolvedUser, set_test_resolver};

    fn current_uid_gid() -> (u32, u32) {
        #[cfg(unix)]
        unsafe {
            (libc::geteuid(), libc::getegid())
        }
        #[cfg(not(unix))]
        {
            (0, 0)
        }
    }

    fn prune_test_config() -> Config {
        let mut cfg = base_config();
        // alice is owned by "root" in base_config; switch to "testowner"
        // so mock resolver hits a single canonical name.
        cfg.mailboxes.get_mut("alice").unwrap().owner = "testowner".into();
        // Add a second mailbox owned by the to-be-deleted user "bob".
        cfg.mailboxes.insert(
            "bob".into(),
            MailboxConfig {
                address: "bob@test.com".into(),
                owner: "bob".into(),
                hooks: vec![],
                trust: None,
                trusted_senders: None,
                allow_root_catchall: false,
            },
        );
        // Template whose run_as is bob (orphan after userdel).
        cfg.hook_templates.push(HookTemplate {
            name: "invoke-codex-bob".into(),
            description: "bob's codex".into(),
            cmd: vec!["/usr/local/bin/codex".into(), "{prompt}".into()],
            params: vec!["prompt".into()],
            stdin: HookTemplateStdin::Email,
            run_as: "bob".into(),
            timeout_secs: 60,
            allowed_events: vec![HookEvent::OnReceive, HookEvent::AfterSend],
        });
        // Hook on bob's mailbox bound to the bob template.
        let mut params = BTreeMap::new();
        params.insert("prompt".into(), "hello".into());
        cfg.mailboxes.get_mut("bob").unwrap().hooks.push(Hook {
            name: Some("bob-on-receive".into()),
            event: HookEvent::OnReceive,
            r#type: "cmd".into(),
            cmd: String::new(),
            dangerously_support_untrusted: false,
            origin: crate::hook::HookOrigin::Operator,
            template: Some("invoke-codex-bob".into()),
            params,
            run_as: None,
        });
        // Hook on alice's mailbox with explicit run_as = bob (also orphan).
        cfg.mailboxes.get_mut("alice").unwrap().hooks.push(Hook {
            name: Some("alice-via-bob".into()),
            event: HookEvent::OnReceive,
            r#type: "cmd".into(),
            cmd: "echo hi".into(),
            dangerously_support_untrusted: false,
            origin: crate::hook::HookOrigin::Operator,
            template: None,
            params: BTreeMap::new(),
            run_as: Some("bob".into()),
        });
        cfg
    }

    fn resolver_without_bob(name: &str) -> Option<ResolvedUser> {
        let (uid, gid) = current_uid_gid();
        match name {
            "testowner" | "root" | "aimx-catchall" | "aimx-hook" => Some(ResolvedUser {
                name: name.to_string(),
                uid,
                gid,
            }),
            _ => None,
        }
    }

    #[test]
    fn build_prune_plan_finds_orphan_templates_and_hooks() {
        let _g = set_test_resolver(resolver_without_bob);
        let cfg = prune_test_config();
        let plan = super::build_prune_plan(&cfg);
        assert_eq!(plan.template_names, vec!["invoke-codex-bob".to_string()]);
        assert!(plan.hooks_by_mailbox.iter().any(|(mb, _)| mb == "bob"));
        assert!(plan.hooks_by_mailbox.iter().any(|(mb, _)| mb == "alice"));
        assert_eq!(plan.total_hooks(), 2);
    }

    #[test]
    fn build_prune_plan_empty_when_nothing_is_orphan() {
        fn all_resolve(name: &str) -> Option<ResolvedUser> {
            let (uid, gid) = current_uid_gid();
            Some(ResolvedUser {
                name: name.to_string(),
                uid,
                gid,
            })
        }
        let _g = set_test_resolver(all_resolve);
        let cfg = prune_test_config();
        let plan = super::build_prune_plan(&cfg);
        assert!(plan.is_empty(), "no orphans → empty plan, got: {plan:?}");
    }

    #[test]
    fn apply_prune_plan_removes_templates_and_hooks() {
        let _g = set_test_resolver(resolver_without_bob);
        let mut cfg = prune_test_config();
        let plan = super::build_prune_plan(&cfg);
        super::apply_prune_plan(&mut cfg, &plan);
        assert!(
            cfg.hook_templates
                .iter()
                .all(|t| t.name != "invoke-codex-bob"),
            "bob's template must be removed"
        );
        assert!(
            cfg.mailboxes.get("bob").unwrap().hooks.is_empty(),
            "bob's hook must be removed"
        );
        assert!(
            cfg.mailboxes.get("alice").unwrap().hooks.is_empty(),
            "alice's bob-run_as hook must be removed"
        );
    }

    #[test]
    fn apply_prune_plan_is_idempotent() {
        let _g = set_test_resolver(resolver_without_bob);
        let mut cfg = prune_test_config();
        let plan = super::build_prune_plan(&cfg);
        super::apply_prune_plan(&mut cfg, &plan);
        // Rebuild plan off the pruned config; should be empty.
        let plan2 = super::build_prune_plan(&cfg);
        assert!(plan2.is_empty(), "second prune is a no-op, got: {plan2:?}");
    }

    #[test]
    fn format_prune_summary_reports_counts() {
        let _g = set_test_resolver(resolver_without_bob);
        let cfg = prune_test_config();
        let plan = super::build_prune_plan(&cfg);
        let out = super::format_prune_summary(&plan);
        assert!(out.contains("Removed 1 templates"), "{out}");
        assert!(out.contains("invoke-codex-bob"), "{out}");
        assert!(out.contains("2 hooks"), "{out}");
        assert!(out.contains("2 mailboxes"), "{out}");
    }

    #[test]
    fn hook_run_as_is_orphan_respects_reserved_names() {
        let _g = set_test_resolver(resolver_without_bob);
        let cfg = prune_test_config();
        // A hook running as root is never orphan.
        let h = Hook {
            name: Some("h".into()),
            event: HookEvent::OnReceive,
            r#type: "cmd".into(),
            cmd: "echo".into(),
            dangerously_support_untrusted: false,
            origin: crate::hook::HookOrigin::Operator,
            template: None,
            params: BTreeMap::new(),
            run_as: Some("root".into()),
        };
        assert!(!super::hook_run_as_is_orphan(&cfg, &h));
        // A hook running as bob (missing user) is orphan.
        let h2 = Hook {
            run_as: Some("bob".into()),
            ..h.clone()
        };
        assert!(super::hook_run_as_is_orphan(&cfg, &h2));
        // A hook with no run_as and no template is not orphan.
        let h3 = Hook { run_as: None, ..h };
        assert!(!super::hook_run_as_is_orphan(&cfg, &h3));
    }

    // -------------------- prune_preflight_check unit tests --------------------

    /// Build a `Config` whose mailbox storage dirs are populated on disk
    /// and whose owner resolves via the mock resolver, so
    /// `run_checks` returns no non-orphan Fail findings. Tests mutate
    /// the returned `(Config, TempDir)` to induce specific failures.
    fn preflight_clean_config(data_dir: &Path) -> Config {
        let mut mailboxes = HashMap::new();
        mailboxes.insert(
            "alice".to_string(),
            MailboxConfig {
                address: "alice@test.com".to_string(),
                owner: "testowner".to_string(),
                hooks: vec![],
                trust: None,
                trusted_senders: None,
                allow_root_catchall: false,
            },
        );
        Config {
            domain: "test.com".to_string(),
            data_dir: data_dir.to_path_buf(),
            dkim_selector: "aimx".to_string(),
            trust: "none".to_string(),
            trusted_senders: vec![],
            hook_templates: vec![],
            mailboxes,
            verify_host: None,
            enable_ipv6: false,
            upgrade: None,
        }
    }

    #[cfg(unix)]
    fn chmod(path: &Path, mode: u32) {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(mode);
        std::fs::set_permissions(path, perms).unwrap();
    }

    fn create_mailbox_dirs(data_dir: &Path, mailbox: &str) {
        let inbox = data_dir.join("inbox").join(mailbox);
        let sent = data_dir.join("sent").join(mailbox);
        std::fs::create_dir_all(&inbox).unwrap();
        std::fs::create_dir_all(&sent).unwrap();
        #[cfg(unix)]
        {
            chmod(&inbox, 0o700);
            chmod(&sent, 0o700);
        }
    }

    #[test]
    fn prune_preflight_check_passes_on_clean_config() {
        let _g = set_test_resolver(resolver_without_bob);
        let tmp = tempfile::TempDir::new().unwrap();
        create_mailbox_dirs(tmp.path(), "alice");
        let cfg = preflight_clean_config(tmp.path());
        super::prune_preflight_check(&cfg).expect("clean config should pass preflight");
    }

    #[test]
    fn prune_preflight_check_refuses_on_non_orphan_fail() {
        // Mailbox dirs are deliberately NOT created, so
        // `check_mailbox_ownership` emits a `MAILBOX-DIR-MISSING` Fail
        // finding for 'alice'. That check ID is not in
        // `ORPHAN_CHECK_IDS`, so preflight must refuse.
        let _g = set_test_resolver(resolver_without_bob);
        let tmp = tempfile::TempDir::new().unwrap();
        let cfg = preflight_clean_config(tmp.path());
        let err =
            super::prune_preflight_check(&cfg).expect_err("missing storage dir must block prune");
        assert!(
            err.message.contains("MAILBOX-DIR-MISSING"),
            "refusal must name the offending check ID, got: {}",
            err.message
        );
        assert!(
            err.message.contains("alice"),
            "refusal must name the offending mailbox, got: {}",
            err.message
        );
    }

    #[test]
    fn prune_preflight_check_ignores_orphan_only_findings() {
        // Add an orphan template (`run_as = "bob"`, and the mock
        // resolver does not know "bob") alongside a clean mailbox.
        // `run_checks` emits `ORPHAN-TEMPLATE-RUN_AS` at Warn severity
        // — which is exactly what `hooks prune --orphans` is here to
        // clean up. Preflight must permit the prune rather than block
        // it on the very finding being pruned.
        let _g = set_test_resolver(resolver_without_bob);
        let tmp = tempfile::TempDir::new().unwrap();
        create_mailbox_dirs(tmp.path(), "alice");
        let mut cfg = preflight_clean_config(tmp.path());
        cfg.hook_templates.push(HookTemplate {
            name: "invoke-codex-bob".into(),
            description: "bob's codex".into(),
            cmd: vec!["/usr/local/bin/codex".into(), "{prompt}".into()],
            params: vec!["prompt".into()],
            stdin: HookTemplateStdin::Email,
            run_as: "bob".into(),
            timeout_secs: 60,
            allowed_events: vec![HookEvent::OnReceive, HookEvent::AfterSend],
        });
        // Sanity-check that the config actually produces an orphan
        // finding — otherwise the test would pass vacuously.
        let findings = crate::doctor::run_checks(&cfg, &[]);
        assert!(
            findings.iter().any(|f| f.check == "ORPHAN-TEMPLATE-RUN_AS"),
            "expected ORPHAN-TEMPLATE-RUN_AS in findings, got: {findings:?}"
        );
        super::prune_preflight_check(&cfg).expect("orphan-only findings must not block prune");
    }

    /// Build a minimal raw-cmd `Hook` for the save-failure tests. The
    /// `run_as: Some("root")` is deliberate so the post-apply invariant
    /// check (`validate_hooks` with a strict orphan context) always
    /// resolves — `root` is a reserved run_as name.
    fn raw_cmd_hook(cmd: &str) -> crate::hook::Hook {
        crate::hook::Hook {
            name: None,
            event: HookEvent::OnReceive,
            r#type: "cmd".into(),
            cmd: cmd.into(),
            dangerously_support_untrusted: false,
            origin: crate::hook::HookOrigin::Operator,
            template: None,
            params: std::collections::BTreeMap::new(),
            run_as: Some("root".into()),
        }
    }

    /// `apply_create_direct` must write
    /// `config.toml` via temp-then-rename and must NOT truncate or
    /// otherwise mutate the file when the underlying write fails.
    #[test]
    fn apply_create_direct_preserves_config_on_save_failure() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(tmp.path()).unwrap();
        let cfg = base_config();
        {
            // Seed the file at the real config path.
            let _guard = crate::config::test_env::ConfigDirOverride::set(tmp.path());
            cfg.save(&crate::config::config_path()).unwrap();
        }
        let real_path = tmp.path().join("config.toml");
        let bytes_before = std::fs::read(&real_path).unwrap();

        // Point config_path() at a nonexistent parent so write_atomic
        // fails on `File::create` for the temp file.
        let bad_dir = tmp.path().join("does").join("not").join("exist");
        let _bad_guard = crate::config::test_env::ConfigDirOverride::set(&bad_dir);

        let err = super::apply_create_direct(&cfg, "alice", raw_cmd_hook("echo hi")).unwrap_err();
        assert!(
            !err.to_string().is_empty(),
            "non-empty error message expected on write failure",
        );

        drop(_bad_guard);
        // Original file on disk is byte-for-byte unchanged.
        let bytes_after = std::fs::read(&real_path).unwrap();
        assert_eq!(
            bytes_after, bytes_before,
            "failed apply_create_direct must not rewrite config.toml",
        );
    }

    /// Same invariant as above, for the
    /// delete-hook direct path. Prove the pre-existing hook survives
    /// untouched on save failure.
    #[test]
    fn apply_delete_direct_preserves_config_on_save_failure() {
        let tmp = tempfile::TempDir::new().unwrap();
        // Start from a config that already carries a hook so delete has
        // something to find.
        let mut cfg = base_config();
        let hook = raw_cmd_hook("echo hi");
        let hook_name = effective_hook_name(&hook);
        cfg.mailboxes.get_mut("alice").unwrap().hooks.push(hook);

        {
            let _guard = crate::config::test_env::ConfigDirOverride::set(tmp.path());
            cfg.save(&crate::config::config_path()).unwrap();
        }
        let real_path = tmp.path().join("config.toml");
        let bytes_before = std::fs::read(&real_path).unwrap();

        let bad_dir = tmp.path().join("nope").join("nah");
        let _bad_guard = crate::config::test_env::ConfigDirOverride::set(&bad_dir);

        let err = super::apply_delete_direct(&cfg, &hook_name).unwrap_err();
        assert!(!err.to_string().is_empty());

        drop(_bad_guard);
        let bytes_after = std::fs::read(&real_path).unwrap();
        assert_eq!(
            bytes_after, bytes_before,
            "failed apply_delete_direct must not rewrite config.toml",
        );
    }
}
