mod channel;
mod cli;
mod config;
mod dkim;
mod ingest;
mod mailbox;
mod mcp;
mod send;
mod setup;
mod status;
mod verify;

use clap::Parser;
use cli::{Cli, Command};
use std::path::Path;

fn main() {
    let cli = Cli::parse();
    if let Err(e) = dispatch(cli) {
        eprintln!("Error: {e}");
        std::process::exit(1);
    }
}

fn dispatch(cli: Cli) -> Result<(), Box<dyn std::error::Error>> {
    match cli.command {
        Command::Ingest { rcpt } => ingest::run(&rcpt, cli.data_dir.as_deref()),
        Command::Send(args) => send::run(args, cli.data_dir.as_deref()),
        Command::Mailbox(cmd) => mailbox::run(cmd, cli.data_dir.as_deref()),
        Command::DkimKeygen { selector, force } => {
            let config = match load_config(cli.data_dir.as_deref()) {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("Error loading config: {e}");
                    std::process::exit(1);
                }
            };
            dkim::run_keygen(&config.data_dir, &config.domain, &selector, force)
        }
        Command::Mcp => {
            let rt = tokio::runtime::Runtime::new()
                .map_err(|e| format!("Failed to create runtime: {e}"))?;
            rt.block_on(mcp::run(cli.data_dir.as_deref()))
        }
        Command::Setup {
            domain,
            verify_host,
        } => {
            let sys = setup::RealSystemOps;
            let net = build_network_ops(verify_host.as_deref(), cli.data_dir.as_deref())?;
            setup::run_setup(domain.as_deref(), cli.data_dir.as_deref(), &sys, &net)
        }
        Command::Status => status::run(cli.data_dir.as_deref()),
        Command::Verify { verify_host } => {
            verify::run(cli.data_dir.as_deref(), verify_host.as_deref())
        }
    }
}

fn load_config(data_dir: Option<&Path>) -> Result<config::Config, Box<dyn std::error::Error>> {
    match data_dir {
        Some(dir) => config::Config::load_from_data_dir(dir),
        None => config::Config::load_default(),
    }
}

fn build_network_ops(
    cli_override: Option<&str>,
    data_dir: Option<&Path>,
) -> Result<setup::RealNetworkOps, Box<dyn std::error::Error>> {
    let config = load_config(data_dir).ok();
    let host =
        verify::resolve_verify_host(cli_override, config.as_ref(), setup::DEFAULT_VERIFY_HOST);
    setup::RealNetworkOps::from_verify_host(host)
}
