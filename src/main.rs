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

    let result = match cli.command {
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
        Command::Mcp => match tokio::runtime::Runtime::new() {
            Ok(rt) => rt.block_on(mcp::run(cli.data_dir.as_deref())),
            Err(e) => Err(format!("Failed to create runtime: {e}").into()),
        },
        Command::Setup { domain } => {
            let sys = setup::RealSystemOps;
            let probe_url = load_config(cli.data_dir.as_deref())
                .ok()
                .and_then(|c| c.probe_url);
            let net = setup::RealNetworkOps {
                probe_url: probe_url.unwrap_or_else(|| setup::DEFAULT_PROBE_URL.to_string()),
            };
            setup::run_setup(&domain, cli.data_dir.as_deref(), &sys, &net)
        }
        Command::Status => status::run(cli.data_dir.as_deref()),
        Command::Preflight => {
            let probe_url = load_config(cli.data_dir.as_deref())
                .ok()
                .and_then(|c| c.probe_url);
            let net = setup::RealNetworkOps {
                probe_url: probe_url.unwrap_or_else(|| setup::DEFAULT_PROBE_URL.to_string()),
            };
            setup::run_preflight_command(&net)
        }
        Command::Verify => verify::run(cli.data_dir.as_deref()),
    };

    if let Err(e) = result {
        eprintln!("Error: {e}");
        std::process::exit(1);
    }
}

fn load_config(data_dir: Option<&Path>) -> Result<config::Config, Box<dyn std::error::Error>> {
    match data_dir {
        Some(dir) => config::Config::load_from_data_dir(dir),
        None => config::Config::load_default(),
    }
}
