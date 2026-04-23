use clap::{Parser, Subcommand};

#[allow(unused_imports)]
pub use crate::version::{build_date, git_hash, release_tag, target_triple, version_string};

#[derive(Parser)]
#[command(
    name = "aimx",
    about = "SMTP for agents. No middleman.",
    long_about = "aimx (AI Mail Exchange). Self-hosted email for AI agents.\n\n\
                   One command to give your AI agents their own email addresses.\n\
                   Incoming mail is parsed to Markdown. Outbound mail is DKIM-signed.\n\
                   MCP is built in. Hooks trigger agent actions on incoming mail.",
    // We render `--version` ourselves so the output is exactly the FR-6.1
    // banner produced by `version_string()`. Clap's built-in version flag
    // would prepend the binary name, yielding `aimx aimx <tag> ...`.
    disable_version_flag = true
)]
pub struct Cli {
    /// Data directory override (default: /var/lib/aimx)
    #[arg(long, env = "AIMX_DATA_DIR", global = true)]
    pub data_dir: Option<std::path::PathBuf>,

    #[command(subcommand)]
    pub command: Command,
}

/// If the user invoked `aimx --version` / `aimx -V` at the top level,
/// print the FR-6.1 banner and return `true` so `main()` can exit before
/// clap's parser refuses a missing subcommand. Handled manually because
/// clap's default `ArgAction::Version` prepends the binary name and would
/// render `aimx aimx <tag> ...`.
pub fn handle_version_flag<I, S>(args: I) -> bool
where
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
    for (idx, arg) in args.into_iter().enumerate() {
        if idx == 0 {
            continue;
        }
        let s = arg.as_ref();
        if s == "--version" || s == "-V" {
            println!("{}", version_string());
            return true;
        }
        // Stop scanning once we cross into a subcommand's own args so
        // subcommand-level `--version` (if any is ever added) isn't
        // swallowed.
        if let Some(str_ref) = s.to_str()
            && !str_ref.starts_with('-')
        {
            return false;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handle_version_flag_matches_long_form() {
        assert!(handle_version_flag(["aimx", "--version"]));
    }

    #[test]
    fn handle_version_flag_matches_short_form() {
        assert!(handle_version_flag(["aimx", "-V"]));
    }

    #[test]
    fn handle_version_flag_ignores_subcommand() {
        assert!(!handle_version_flag(["aimx", "serve", "--version"]));
    }

    #[test]
    fn handle_version_flag_ignores_absent() {
        assert!(!handle_version_flag(["aimx", "doctor"]));
    }
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

    /// Install aimx plugin/skill for an AI agent into the current user's config
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

        /// Install plugin files only; skip probing $PATH and registering the template
        #[arg(long, conflicts_with = "redetect")]
        no_template: bool,

        /// Re-probe $PATH and update an existing invoke-<agent>-<username> template
        #[arg(long)]
        redetect: bool,
    },

    /// Inverse of agent-setup: remove the invoke-<agent>-<username> template, optionally the plugin files too
    AgentCleanup {
        /// Agent short name (e.g. claude-code)
        agent: String,

        /// Also remove plugin files under $HOME laid down by agent-setup
        #[arg(long)]
        full: bool,

        /// Skip the interactive prompt when --full removes plugin files
        #[arg(short = 'y', long)]
        yes: bool,
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

    /// Fetch the latest release and swap the installed binary. Requires root.
    ///
    /// Compares the running tag to the release manifest and, if a newer
    /// version exists, downloads the target-matching tarball, stops the
    /// `aimx.service`, atomically swaps the binary (preserving the old
    /// one at `<install_path>.prev`), and restarts the service. Any
    /// failure between stop and final start rolls back to the previous
    /// binary. Never runs the setup wizard, never prompts.
    Upgrade(UpgradeArgs),
}

#[derive(clap::Args, Clone, Debug)]
pub struct UpgradeArgs {
    /// Print what would happen (current → target version, tarball URL,
    /// install path, action list) without touching the service or
    /// writing to `/usr/local/bin`. Exits 0 on "would proceed" and on
    /// "already up to date"; non-zero only on download failure.
    #[arg(long)]
    pub dry_run: bool,

    /// Target a specific release tag (downgrade path). When omitted,
    /// `aimx upgrade` resolves the latest release from the manifest.
    #[arg(long, value_name = "TAG")]
    pub version: Option<String>,

    /// Re-install the current tag (useful for repair) or the specified
    /// `--version` tag. Bypasses the up-to-date short-circuit.
    #[arg(long)]
    pub force: bool,
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
    #[arg(long)]
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

        /// Linux user that owns the mailbox's storage. When omitted,
        /// the CLI prompts (defaulting to the local part of the
        /// address when a user with that name already exists on the
        /// host). Under `AIMX_NONINTERACTIVE=1` the default is
        /// accepted when available, or the command errors hard. The
        /// owner must resolve via `getpwnam` at daemon load time.
        #[arg(long)]
        owner: Option<String>,
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

    /// Create a new hook on a mailbox
    Create(HookCreateArgs),

    /// Delete a hook by name
    Delete {
        /// Hook name (explicit or derived)
        name: String,

        /// Skip confirmation prompt
        #[arg(short = 'y', long)]
        yes: bool,
    },

    /// List enabled hook templates (`[[hook_template]]` entries in config.toml)
    #[command(alias = "template-list")]
    Templates,

    /// Remove orphan templates / hooks (root-only).
    ///
    /// After `userdel alice`, alice's templates and hooks become orphans.
    /// This command atomically rewrites `config.toml`, removing every
    /// template whose `run_as` does not resolve and every hook whose
    /// effective `run_as` (or referenced template) is orphan. Refuses
    /// when `aimx doctor` surfaces non-orphan failures — fix those first.
    Prune {
        /// Only remove templates/hooks whose `run_as` user no longer
        /// resolves. This is currently the only scope supported (hence
        /// required); added as an explicit flag so future pruning scopes
        /// (e.g. `--broken-cmd`) slot in cleanly.
        #[arg(long)]
        orphans: bool,

        /// Print the proposed diff without writing `config.toml`.
        #[arg(long)]
        dry_run: bool,
    },
}

#[derive(clap::Args, Clone)]
#[command(group(
    clap::ArgGroup::new("hook_body")
        .required(true)
        .args(["cmd", "template"]),
))]
pub struct HookCreateArgs {
    /// Owning mailbox (local part). Must already exist in config
    #[arg(long)]
    pub mailbox: String,

    /// Event that triggers the hook
    #[arg(long, value_parser = ["on_receive", "after_send"])]
    pub event: String,

    /// Shell command executed via `sh -c` when the hook fires.
    /// Raw-cmd hooks require root (writes to /etc/aimx/config.toml
    /// directly and sends SIGHUP to aimx serve; they never traverse
    /// the UDS socket). Mutually exclusive with `--template`.
    #[arg(long, conflicts_with = "template")]
    pub cmd: Option<String>,

    /// Reference a pre-installed `[[hook_template]]` by name. Mutually
    /// exclusive with `--cmd`. Hook creation goes through the daemon
    /// UDS (template-only verb) and the resulting hook inherits the
    /// template's argv shape with parameters supplied via `--param`.
    #[arg(long, conflicts_with = "cmd")]
    pub template: Option<String>,

    /// Bind a template parameter `KEY=VAL`. Repeatable. Only valid
    /// with `--template`. Every parameter the template declares must
    /// be bound exactly once.
    #[arg(
        long = "param",
        value_name = "KEY=VAL",
        requires = "template",
        action = clap::ArgAction::Append
    )]
    pub params: Vec<String>,

    /// Optional hook name. When omitted, a stable 12-char hex name is
    /// derived from the event + (cmd | template + params) shape.
    #[arg(long)]
    pub name: Option<String>,

    /// Opt into firing this hook on non-trusted inbound email. Deliberately
    /// verbose so operators think twice. Only valid on `on_receive`.
    /// Never settable on `--template` hooks (template hooks only fire
    /// on trusted mail).
    #[arg(long, conflicts_with = "template")]
    pub dangerously_support_untrusted: bool,
}
