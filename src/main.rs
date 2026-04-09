mod cli;
mod config;
mod dkim;
mod ingest;
mod mailbox;
mod send;

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
        Command::Mcp => {
            eprintln!("MCP server not yet implemented");
            Ok(())
        }
        Command::Setup { domain: _ } => {
            eprintln!("Setup wizard not yet implemented");
            Ok(())
        }
        Command::Status => {
            eprintln!("Status not yet implemented");
            Ok(())
        }
        Command::Preflight => {
            eprintln!("Preflight not yet implemented");
            Ok(())
        }
        Command::Verify => {
            eprintln!("Verify not yet implemented");
            Ok(())
        }
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
