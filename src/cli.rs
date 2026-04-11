use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "aimx",
    about = "SMTP for agents. No middleman.",
    long_about = "aimx - Self-hosted email for AI agents.\n\n\
                   One command to give your AI agents their own email addresses.\n\
                   Incoming mail is parsed to Markdown. Outbound mail is DKIM-signed.\n\
                   MCP is built in. Channel rules trigger agent actions on incoming mail.",
    version
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
    /// Ingest an email from stdin (called by OpenSMTPD MDA)
    Ingest {
        /// Recipient address (e.g. user@domain.com)
        rcpt: String,
    },

    /// Compose and send an email
    Send(SendArgs),

    /// Manage mailboxes
    #[command(subcommand)]
    Mailbox(MailboxCommand),

    /// Start MCP server in stdio mode
    Mcp,

    /// Run interactive setup wizard
    Setup {
        /// Domain to configure (e.g. agent.example.com)
        domain: String,

        /// Override the verify service host (e.g. https://verify.example.com)
        #[arg(long)]
        verify_host: Option<String>,
    },

    /// Show server status, mailbox counts, and configuration
    Status,

    /// Run preflight checks (port 25, PTR) without installing anything
    Preflight {
        /// Override the verify service host (e.g. https://verify.example.com)
        #[arg(long)]
        verify_host: Option<String>,
    },

    /// Check port 25 connectivity (outbound, inbound EHLO, PTR)
    Verify {
        /// Override the verify service host (e.g. https://verify.example.com)
        #[arg(long)]
        verify_host: Option<String>,
    },

    /// Generate DKIM keypair for email signing
    DkimKeygen {
        /// DKIM selector name
        #[arg(long, default_value = "dkim")]
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
        #[arg(long)]
        yes: bool,
    },
}
