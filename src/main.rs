mod agent_setup;
mod cli;
mod config;
mod datadir_readme;
mod dkim;
mod doctor;
mod frontmatter;
mod hook;
mod hook_client;
mod hook_handler;
mod hook_substitute;
mod hook_templates_defaults;
mod hooks;
mod ingest;
mod logging;
mod logs;
mod mailbox;
mod mailbox_handler;
mod mailbox_locks;
mod mcp;
mod mx;
mod ownership;
mod platform;
mod portcheck;
mod send;
mod send_handler;
mod send_protocol;
mod serve;
mod setup;
mod slug;
mod smtp;
mod state_handler;
mod term;
mod transport;
mod trust;
mod uds_authz;
mod uninstall;
mod user_resolver;

use clap::Parser;
use cli::{Cli, Command};

fn main() {
    let cli = Cli::parse();
    if let Err(e) = dispatch(cli) {
        eprintln!("{} {e}", term::error("Error:"));
        std::process::exit(1);
    }
}

fn dispatch(cli: Cli) -> Result<(), Box<dyn std::error::Error>> {
    match cli.command {
        // Setup runs pre-install: config may not exist yet.
        Command::Setup {
            domain,
            verify_host,
            non_interactive,
        } => {
            let sys = setup::RealSystemOps;
            let net = build_network_ops(verify_host.as_deref())?;
            setup::run_setup(
                domain.as_deref(),
                cli.data_dir.as_deref(),
                non_interactive,
                &sys,
                &net,
            )
        }
        // Uninstall also runs pre-config: config may be missing or unreadable.
        Command::Uninstall { yes } => {
            let sys = setup::RealSystemOps;
            uninstall::run(yes, &sys)
        }
        // Portcheck does not read config for storage, only `verify_host`.
        Command::Portcheck { verify_host } => portcheck::run(verify_host.as_deref()),
        // MCP server reloads config on each tool call; pass the override through.
        Command::Mcp => {
            let rt = tokio::runtime::Runtime::new()
                .map_err(|e| format!("Failed to create runtime: {e}"))?;
            rt.block_on(mcp::run(cli.data_dir.as_deref()))
        }
        // agent-setup uses data_dir as an install-path override for emitted
        // MCP configs, not a config-loading override.
        Command::AgentSetup {
            agent,
            list,
            force,
            print,
            no_template,
            redetect,
        } => agent_setup::run(agent_setup::RunOpts {
            agent,
            list,
            force,
            print,
            no_template,
            redetect,
            data_dir: cli.data_dir.as_deref(),
        }),
        // `aimx send` is a pure UDS client. It never reads config.toml.
        // The daemon parses the `From:` header itself and resolves the
        // sender mailbox against its in-memory Config.
        Command::Send(args) => send::run(args),
        // `aimx logs` is a thin wrapper around journalctl; it does not
        // read config.toml.
        Command::Logs { lines, follow } => logs::run(lines, follow),
        // Everything else loads Config once here and takes it by value.
        // For long-lived processes (`aimx serve`, `aimx doctor`) we log
        // startup warnings via `tracing` so orphan mailboxes / templates
        // surface in journalctl. Short-lived CLI commands inherit the
        // same logging but only a tracing subscriber is installed by
        // `aimx serve` / `aimx doctor` themselves.
        other => {
            let (config, warnings) =
                config::Config::load_resolved_with_data_dir(cli.data_dir.as_deref())?;
            emit_load_warnings(&warnings);
            dispatch_with_config(other, config)
        }
    }
}

fn emit_load_warnings(warnings: &[config::LoadWarning]) {
    for w in warnings {
        tracing::warn!(
            target: "aimx::config",
            "{}",
            w.message(),
        );
    }
}

fn dispatch_with_config(
    cmd: Command,
    config: config::Config,
) -> Result<(), Box<dyn std::error::Error>> {
    match cmd {
        Command::Ingest { rcpt } => ingest::run(&rcpt, config),
        Command::Mailboxes(cmd) => mailbox::run(cmd, config),
        Command::Hooks(cmd) => hooks::run(cmd, config),
        Command::DkimKeygen { selector, force } => {
            dkim::run_keygen(&config::dkim_dir(), &config.domain, &selector, force)
        }
        Command::Serve {
            bind,
            tls_cert,
            tls_key,
        } => serve::run(
            Some(bind.as_str()),
            tls_cert.as_deref(),
            tls_key.as_deref(),
            config,
        ),
        Command::Doctor => doctor::run(config),
        Command::Setup { .. }
        | Command::Uninstall { .. }
        | Command::Portcheck { .. }
        | Command::Mcp
        | Command::Send(_)
        | Command::Logs { .. }
        | Command::AgentSetup { .. } => unreachable!("handled by dispatch"),
    }
}

fn build_network_ops(
    cli_override: Option<&str>,
) -> Result<setup::RealNetworkOps, Box<dyn std::error::Error>> {
    let config = config::Config::load_resolved_ignore_warnings().ok();
    let host =
        portcheck::resolve_verify_host(cli_override, config.as_ref(), setup::DEFAULT_VERIFY_HOST);
    setup::RealNetworkOps::from_verify_host(host)
}
