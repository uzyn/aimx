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

    /// Manage hooks
    #[command(subcommand, alias = "hook")]
    Hooks(HookCommand),

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
        /// Agent short name (e.g. claude-code). Omit to print the supported-agent registry, or pass --list for the same view.
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

    /// Show trust, hooks, and message counts for a single mailbox
    Show {
        /// Mailbox name to inspect
        name: String,
    },

    /// Delete a mailbox
    Delete {
        /// Mailbox name to delete
        name: String,

        /// Skip confirmation prompt
        #[arg(short = 'y', long)]
        yes: bool,

        /// Wipe `inbox/<name>/` and `sent/<name>/` contents before
        /// deleting. Without this flag, a non-empty mailbox is refused
        /// with the daemon's `ERR NONEMPTY` error. Refuses to wipe the
        /// catchall mailbox; prompts interactively unless `--yes` is
        /// also passed.
        #[arg(long)]
        force: bool,
    },
}

#[derive(Subcommand, Clone)]
pub enum HookCommand {
    /// List hooks (optionally filtered by mailbox)
    List {
        /// Filter hooks by owning mailbox
        #[arg(long)]
        mailbox: Option<String>,
    },

    /// Create a new hook on a mailbox (auto-generates the hook id)
    Create(HookCreateArgs),

    /// Delete a hook by id
    Delete {
        /// 12-char alphanumeric hook id
        id: String,

        /// Skip confirmation prompt
        #[arg(short = 'y', long)]
        yes: bool,
    },
}

#[derive(clap::Args, Clone)]
pub struct HookCreateArgs {
    /// Owning mailbox (local part) — must already exist in config
    #[arg(long)]
    pub mailbox: String,

    /// Event that triggers the hook
    #[arg(long, value_parser = ["on_receive", "after_send"])]
    pub event: String,

    /// Shell command executed via `sh -c` when the hook fires
    #[arg(long)]
    pub cmd: String,

    /// Sender-address glob filter (only valid on `on_receive`)
    #[arg(long)]
    pub from: Option<String>,

    /// Recipient-address glob filter (only valid on `after_send`)
    #[arg(long)]
    pub to: Option<String>,

    /// Subject substring filter (case-insensitive, both events)
    #[arg(long)]
    pub subject: Option<String>,

    /// Require the email to have at least one attachment (only valid on
    /// `on_receive`). Outbound submissions via UDS are text-only in v0.2.
    #[arg(long)]
    pub has_attachment: bool,

    /// Opt into firing this hook on non-trusted inbound email. Deliberately
    /// verbose so operators think twice. Only valid on `on_receive`.
    #[arg(long)]
    pub dangerously_support_untrusted: bool,
}
