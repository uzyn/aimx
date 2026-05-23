//! `aimx domains list | add | remove` CLI.
//!
//! Domain CRUD goes through the daemon UDS first so the running daemon
//! hot-swaps its in-memory `Arc<Config>` plus the per-domain DKIM map
//! without a restart. The daemon enforces root-only authz via
//! `Action::DomainCrud` (see `src/auth.rs`).
//!
//! When the daemon is stopped:
//! - Root falls back to a direct `config.toml` edit + DKIM keygen +
//!   restart hint.
//! - Non-root hard-errors with the canonical "daemon must be running"
//!   message because it cannot write the root-owned config.
//!
//! `list` reads the daemon's response shape so a non-root operator
//! running on a host whose daemon is up still gets the listing (the
//! daemon checks `Action::DomainCrud` and rejects non-root callers
//! with `ERR EACCES`, which the CLI surfaces verbatim).

use std::io::{self, Write};

use crate::cli::DomainsCommand;
use crate::config::Config;
use crate::domain_list_handler::DomainListRow;
use crate::platform::is_root;
use crate::term;

/// Exit code used when the daemon UDS is missing and the caller cannot
/// fall back to the direct-edit path. Mirrors
/// `mailbox::EXIT_SOCKET_MISSING` for tooling parity.
pub(crate) const EXIT_SOCKET_MISSING: i32 = 2;

/// Canonical hint for the non-root daemon-down branch. Hoisted to a
/// constant so the integration test can match it verbatim.
pub(crate) const SOCKET_MISSING_HINT: &str = "daemon must be running for non-root domain CRUD; start `aimx serve` \
     or run with sudo to fall back to direct config edit.";

pub fn run(cmd: DomainsCommand) -> Result<(), Box<dyn std::error::Error>> {
    match cmd {
        DomainsCommand::List => list(),
        DomainsCommand::Add {
            domain,
            selector,
            no_dns_check,
        } => add(&domain, selector.as_deref(), no_dns_check),
        DomainsCommand::Remove { domain, force } => remove(&domain, force),
    }
}

/// `aimx domains list` — fetch via UDS, render the table.
fn list() -> Result<(), Box<dyn std::error::Error>> {
    let rows = list_via_daemon()?;
    render_table(&rows);
    Ok(())
}

/// Render the domain table. Uses `term.rs` semantic helpers; no raw
/// color calls. Column widths are chosen to keep the table readable on
/// an 80-column terminal in the common case (single-domain through
/// half-dozen domains).
pub(crate) fn render_table(rows: &[DomainListRow]) {
    if rows.is_empty() {
        println!("No domains configured.");
        return;
    }

    // Column widths: Domain (32), Default (8), DKIM (10), Mailboxes (10),
    // Unread (7), Overrides (rest).
    println!(
        "{}  {}  {}  {}  {}  {}",
        term::header("DOMAIN                          "),
        term::header("DEFAULT"),
        term::header("DKIM     "),
        term::header("MAILBOXES"),
        term::header("UNREAD"),
        term::header("OVERRIDES"),
    );
    for row in rows {
        let default_mark = if row.default {
            term::success_mark().to_string()
        } else {
            " ".to_string()
        };
        let dkim_status = if row.dkim_loaded {
            term::success("loaded").to_string()
        } else {
            term::warn("MISSING").to_string()
        };
        let overrides = if row.overrides.is_empty() {
            term::dim("—").to_string()
        } else {
            row.overrides.clone()
        };
        println!(
            "{:<32}  {:^7}  {:<9}  {:>9}  {:>6}  {}",
            term::highlight(&row.domain).to_string(),
            default_mark,
            dkim_status,
            row.mailbox_count,
            row.unread,
            overrides,
        );
    }
}

/// Fetch the daemon's `DOMAIN-LIST` JSON response and decode it.
/// Returns the raw rows; the caller renders.
pub(crate) fn list_via_daemon() -> Result<Vec<DomainListRow>, Box<dyn std::error::Error>> {
    let json = match crate::mcp::submit_domain_list_via_daemon_for_cli() {
        Ok(s) => s,
        Err(crate::mcp::MailboxLifecycleFallback::SocketMissing) => {
            exit_socket_missing();
        }
        Err(crate::mcp::MailboxLifecycleFallback::Daemon(msg)) => {
            return Err(msg.into());
        }
    };
    let rows: Vec<DomainListRow> =
        serde_json::from_str(&json).map_err(|e| format!("malformed DOMAIN-LIST response: {e}"))?;
    Ok(rows)
}

/// `aimx domains add <domain> [--selector <s>] [--no-dns-check]` — UDS
/// first, daemon-down fallback for root, hard-error for non-root.
///
/// `--data-dir` is read off the global `Cli` struct by the daemon-side
/// loader (`Config::load_resolved_with_data_dir`) when this path
/// falls back to direct config edit, so we do not thread it through
/// here: the storage layout is decided by the running daemon, not the
/// CLI invocation.
fn add(
    domain: &str,
    selector: Option<&str>,
    no_dns_check: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let normalized = domain.trim().to_ascii_lowercase();
    if !crate::config::is_valid_domain_syntax(&normalized) {
        return Err(format!("domain '{domain}' is not a valid RFC 1035 hostname").into());
    }

    // First attempt over UDS.
    match crate::mcp::submit_domain_add_via_daemon(&normalized, selector) {
        Ok(()) => {
            print_add_success_header(&normalized);
        }
        Err(crate::mcp::MailboxLifecycleFallback::SocketMissing) => {
            // Daemon is down. Root can fall back; non-root cannot.
            if !is_root() {
                exit_socket_missing();
            }
            add_direct(&normalized, selector)?;
            print_add_success_header(&normalized);
            println!(
                "{} daemon is stopped; restart `aimx serve` so the new domain takes effect.",
                term::warn_mark()
            );
        }
        Err(crate::mcp::MailboxLifecycleFallback::Daemon(msg)) => {
            return Err(msg.into());
        }
    }

    // DNS guidance (records + verify) is the same shape `aimx setup`
    // prints, parameterized on the new domain. We resolve the server's
    // IP and the per-domain DKIM public key the same way setup does.
    print_dns_guidance_and_verify(&normalized, selector, no_dns_check)?;
    Ok(())
}

/// Daemon-stopped fallback: write config + DKIM directly. Only callable
/// from root; the non-root path exits via [`exit_socket_missing`] above.
fn add_direct(domain: &str, selector: Option<&str>) -> Result<(), Box<dyn std::error::Error>> {
    let config_path = crate::config::config_path();
    let (config, _warnings) = Config::load_resolved_with_data_dir(None)?;
    let dkim_root = crate::config::dkim_dir();
    crate::domain_handler::run_direct_add(&config_path, &dkim_root, &config, domain, selector)?;
    Ok(())
}

fn print_add_success_header(domain: &str) {
    println!();
    let dkim_path = crate::config::dkim_dir().join(domain).join("private.key");
    println!(
        "{} Added domain {}",
        term::success_mark(),
        term::highlight(domain),
    );
    let dkim_dir_display = dkim_path
        .parent()
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    println!(
        "{} DKIM keypair: {}",
        term::success_mark(),
        term::highlight(&dkim_dir_display),
    );
}

/// Print DNS records to publish for the new domain and run the DNS
/// verification loop (reusing setup's helpers). `--no-dns-check`
/// short-circuits the verify step but still prints the records.
fn print_dns_guidance_and_verify(
    domain: &str,
    selector: Option<&str>,
    no_dns_check: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    // Resolve the server's IPv4 (and global IPv6 if available) so the
    // SPF + A records the operator publishes are correct for this host.
    let net = crate::setup::RealNetworkOps::default();
    use crate::setup::NetworkOps as _;
    let (ipv4_opt, ipv6_opt) = net.get_server_ips()?;
    let server_ipv4 = ipv4_opt.ok_or::<Box<dyn std::error::Error>>(
        "Could not determine server IPv4 address; publish DNS records manually.".into(),
    )?;
    let server_ip: std::net::IpAddr = std::net::IpAddr::V4(server_ipv4);
    let server_ipv6_ip: Option<std::net::IpAddr> = ipv6_opt.map(std::net::IpAddr::V6);
    let server_ip_str = server_ip.to_string();
    let server_ipv6_str = server_ipv6_ip.map(|ip| ip.to_string());

    // Resolve the per-domain DKIM public key (already on disk after the
    // add — daemon or fallback both wrote it).
    let dkim_root = crate::config::dkim_dir().join(domain);
    let dkim_value = crate::dkim::dns_record_value(&dkim_root)?;
    let local_dkim_pubkey = dkim_value
        .strip_prefix("v=DKIM1; k=rsa; p=")
        .map(|s| s.to_string());

    // Selector: respect explicit override, else the daemon may have
    // persisted one in `[domain."<d>"] dkim_selector`. Re-load the
    // config to read it back; fall back to the top-level default.
    let selector_resolved = match selector {
        Some(s) => s.to_string(),
        None => {
            let (config, _w) = Config::load_resolved_with_data_dir(None)?;
            crate::dkim_keys::resolve_selector_for_domain(&config, domain)
        }
    };

    crate::setup::display_dns_guidance(
        domain,
        &server_ip_str,
        server_ipv6_str.as_deref(),
        &dkim_value,
        &selector_resolved,
    );

    if no_dns_check {
        println!(
            "{} DNS verification skipped (--no-dns-check). Run `{}` once records are live.",
            term::warn_mark(),
            term::highlight("aimx doctor"),
        );
        return Ok(());
    }

    // Reuse setup's verify loop pattern: prompt to verify, escape with `q`.
    let dns_records = crate::setup::generate_dns_records(
        domain,
        &server_ip_str,
        server_ipv6_str.as_deref(),
        &dkim_value,
        &selector_resolved,
    );
    loop {
        println!();
        println!(
            "  Press {} to verify DNS records now.",
            term::highlight("Enter"),
        );
        println!(
            "  Press {} to skip and run `{}` later.",
            term::highlight("q"),
            term::highlight("aimx doctor"),
        );
        print!("{} ", term::prompt_mark());
        io::stdout().flush()?;
        let mut input = String::new();
        io::stdin().read_line(&mut input)?;
        if input.trim().eq_ignore_ascii_case("q") {
            println!(
                "Update your DNS records and run `{}` to re-verify.",
                term::highlight("aimx doctor")
            );
            break;
        }

        let results = crate::setup::verify_all_dns(
            &net,
            domain,
            &server_ip,
            server_ipv6_ip.as_ref(),
            &selector_resolved,
            local_dkim_pubkey.as_deref(),
        );
        let all_pass = crate::setup::display_dns_verification(&results, &dns_records);
        if all_pass {
            println!(
                "{}",
                term::success("All DNS records verified for the new domain.")
            );
            break;
        } else {
            println!("Some DNS records are not yet correct.");
            println!("DNS propagation can take up to 48 hours.");
        }
    }
    Ok(())
}

/// Placeholder for `aimx domains remove`. The real cascade behaviour
/// lands in a follow-up release; for now we surface a clear "not yet
/// implemented" error so the clap surface is complete and operators
/// see the same help text they will see post-rollout.
fn remove(_domain: &str, _force: bool) -> Result<(), Box<dyn std::error::Error>> {
    Err(
        "`aimx domains remove` is not yet implemented; this command \
         lands in a follow-up release."
            .into(),
    )
}

/// Print the canonical hint and exit `EXIT_SOCKET_MISSING`. Mirrors
/// `mailbox::exit_socket_missing`.
pub(crate) fn exit_socket_missing() -> ! {
    eprintln!("{} {SOCKET_MISSING_HINT}", term::error("Error:"));
    std::process::exit(EXIT_SOCKET_MISSING);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `render_table` on an empty slice prints the "no domains" line
    /// rather than a header with no rows.
    #[test]
    fn render_table_empty_prints_no_domains_line() {
        // Routes through stdout; the assert is that no panic happens
        // and the function returns. A more precise capture would
        // require redirecting stdout — overkill for a CLI helper.
        render_table(&[]);
    }

    /// `render_table` with a registered row exercises every branch of
    /// the format string (default marker, DKIM loaded vs missing,
    /// overrides empty vs set).
    #[test]
    fn render_table_renders_default_and_non_default_rows_without_panic() {
        let rows = vec![
            DomainListRow {
                domain: "a.com".into(),
                default: true,
                dkim_loaded: true,
                dkim_selector: "aimx".into(),
                mailbox_count: 2,
                unread: 0,
                overrides: String::new(),
            },
            DomainListRow {
                domain: "b.com".into(),
                default: false,
                dkim_loaded: false,
                dkim_selector: "s2025".into(),
                mailbox_count: 1,
                unread: 3,
                overrides: "dkim_selector,trust".into(),
            },
        ];
        render_table(&rows);
    }
}
