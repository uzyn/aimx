mod agent_setup;
mod channel;
mod cli;
mod config;
mod dkim;
mod frontmatter;
mod ingest;
mod mailbox;
mod mcp;
mod mx;
mod platform;
mod send;
mod send_handler;
mod send_protocol;
mod serve;
mod setup;
mod slug;
mod smtp;
mod status;
mod term;
mod transport;
mod verify;

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
        } => {
            let sys = setup::RealSystemOps;
            let net = build_network_ops(verify_host.as_deref())?;
            setup::run_setup(domain.as_deref(), cli.data_dir.as_deref(), &sys, &net)
        }
        // Verify does not read config for storage — only `verify_host`.
        Command::Verify { verify_host } => verify::run(verify_host.as_deref()),
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
        } => agent_setup::run(agent, list, force, print, cli.data_dir.as_deref()),
        // Everything else loads Config once here and takes it by value.
        other => {
            let config = config::Config::load_resolved_with_data_dir(cli.data_dir.as_deref())?;
            dispatch_with_config(other, config)
        }
    }
}

fn dispatch_with_config(
    cmd: Command,
    config: config::Config,
) -> Result<(), Box<dyn std::error::Error>> {
    match cmd {
        Command::Ingest { rcpt } => ingest::run(&rcpt, config),
        Command::Send(args) => send::run(args, config),
        Command::Mailbox(cmd) => mailbox::run(cmd, config),
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
        Command::Status => status::run(config),
        Command::Setup { .. }
        | Command::Verify { .. }
        | Command::Mcp
        | Command::AgentSetup { .. } => unreachable!("handled by dispatch"),
    }
}

fn build_network_ops(
    cli_override: Option<&str>,
) -> Result<setup::RealNetworkOps, Box<dyn std::error::Error>> {
    let config = config::Config::load_resolved().ok();
    let host =
        verify::resolve_verify_host(cli_override, config.as_ref(), setup::DEFAULT_VERIFY_HOST);
    setup::RealNetworkOps::from_verify_host(host)
}
