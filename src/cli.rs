use clap::{Parser, Subcommand};

pub fn version_string() -> &'static str {
    use std::sync::LazyLock;
    static VERSION: LazyLock<String> = LazyLock::new(|| {
        let hash = env!("GIT_HASH");
        if hash == "unknown" || hash.is_empty() {
            env!("CARGO_PKG_VERSION").to_string()
        } else {
            format!("{} ({hash})", env!("CARGO_PKG_VERSION"))
        }
    });
    &VERSION
}

#[derive(Parser)]
#[command(
    name = "aimx",
    about = "SMTP for agents. No middleman.",
    long_about = "AIMX - Self-hosted email for AI agents.\n\n\
                   One command to give your AI agents their own email addresses.\n\
                   Incoming mail is parsed to Markdown. Outbound mail is DKIM-signed.\n\
                   MCP is built in. Channel rules trigger agent actions on incoming mail.",
    version = version_string()
)]
pub struct Cli {
    /// Data directory override (default: /var/lib/aimx)
    #[arg(long, env = "AIMX_DATA_DIR", global = true)]
    pub data_dir: Option<std::path::PathBuf>,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand)]
pub enum Command {
    /// Ingest an email from stdin (called by aimx serve or via stdin)
    Ingest {
        /// Recipient address (e.g. user@domain.com)
        rcpt: String,
    },

    /// Compose and send an email
    Send(SendArgs),

    /// Manage mailboxes
    #[command(subcommand, alias = "mailbox")]
    Mailboxes(MailboxCommand),

    /// Start MCP server in stdio mode
    Mcp,

    /// Run interactive setup wizard
    Setup {
        /// Domain to configure (e.g. agent.example.com)
        domain: Option<String>,

        /// Override the verify service host (e.g. https://verify.example.com)
        #[arg(long)]
        verify_host: Option<String>,
    },

    /// Uninstall the aimx daemon service (config and data are retained)
    Uninstall {
        /// Skip the confirmation prompt
        #[arg(short = 'y', long)]
        yes: bool,
    },

    /// Show server health, mailbox counts, configuration, DNS verification, and recent logs
    Doctor,

    /// Tail or follow the aimx service log
    Logs {
        /// Number of lines to show (default: 50)
        #[arg(long)]
        lines: Option<usize>,

        /// Stream the log live (like `journalctl -f`)
        #[arg(short = 'f', long)]
        follow: bool,
    },

    /// Start the embedded SMTP listener daemon
    Serve {
        /// Bind address (default: 0.0.0.0:25)
        #[arg(long, default_value = "0.0.0.0:25")]
        bind: String,

        /// Path to TLS certificate PEM file
        #[arg(long)]
        tls_cert: Option<String>,

        /// Path to TLS private key PEM file
        #[arg(long)]
        tls_key: Option<String>,
    },

    /// Check port 25 connectivity (outbound, inbound)
    Portcheck {
        /// Override the verify service host (e.g. https://verify.example.com)
        #[arg(long)]
        verify_host: Option<String>,
    },

    /// Install AIMX plugin/skill for an AI agent into the current user's config
    AgentSetup {
        /// Agent short name (e.g. claude-code). Omit with --list.
        agent: Option<String>,

        /// List supported agents with destinations and activation hints
        #[arg(long)]
        list: bool,

        /// Overwrite existing destination files without prompting
        #[arg(long)]
        force: bool,

        /// Print plugin contents to stdout instead of writing to disk
        #[arg(long)]
        print: bool,
    },

    /// Generate DKIM keypair for email signing
    DkimKeygen {
        /// DKIM selector name
        #[arg(long, default_value = "aimx")]
        selector: String,

        /// Overwrite existing keys
        #[arg(long)]
        force: bool,
    },
}

#[derive(clap::Args, Clone)]
pub struct SendArgs {
    /// Sender address
    #[arg(long)]
    pub from: String,

    /// Recipient address
    #[arg(long)]
    pub to: String,

    /// Email subject
    #[arg(long)]
    pub subject: String,

    /// Email body
    #[arg(long)]
    pub body: String,

    /// Message-ID to reply to (sets In-Reply-To header)
    #[arg(long)]
    pub reply_to: Option<String>,

    /// Full References header chain for threading
    #[arg(long, hide = true)]
    pub references: Option<String>,

    /// File paths to attach
    #[arg(long = "attachment")]
    pub attachments: Vec<String>,
}

#[derive(Subcommand, Clone)]
pub enum MailboxCommand {
    /// Create a new mailbox
    Create {
        /// Mailbox name (local part of email address)
        name: String,
    },

    /// List all mailboxes
    List,

    /// Delete a mailbox
    Delete {
        /// Mailbox name to delete
        name: String,

        /// Skip confirmation prompt
        #[arg(short = 'y', long)]
        yes: bool,
    },
}
