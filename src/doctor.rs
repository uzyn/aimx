use crate::config::Config;
use crate::frontmatter::InboundFrontmatter;
use crate::setup::{
    self, DnsRecord, DnsVerifyResult, NetworkOps, RealNetworkOps, RealSystemOps, SystemOps,
};
use crate::term;
use std::net::IpAddr;
use std::path::Path;

pub struct StatusInfo {
    pub domain: String,
    pub data_dir: String,
    /// Resolved `/etc/aimx/config.toml` path, honouring `AIMX_CONFIG_DIR`.
    /// Surfaced in the Configuration section so operators troubleshooting
    /// "is doctor reading the file I think it is" don't have to grep.
    pub config_path: String,
    pub dkim_selector: String,
    pub dkim_key_present: bool,
    pub smtp_running: bool,
    /// True when the old `send.sock` path still exists in the runtime dir
    /// and the new `aimx.sock` is absent. Surfaced as a warn line in the
    /// Service section so operators upgrading from pre-launch builds see
    /// the rename without having to read release notes.
    pub stale_send_sock_present: bool,
    /// Top-level default trust policy from `Config::trust`. Per-mailbox
    /// overrides are surfaced on each `MailboxStatus` row.
    pub default_trust: String,
    /// Entries from the top-level `Config::trusted_senders` list. Rendered
    /// verbatim on the `Trusted senders:` line below `Global trust:`.
    pub default_trusted_senders: Vec<String>,
    pub mailboxes: Vec<MailboxStatus>,
    pub dns: Option<DnsSection>,
}

pub struct DnsSection {
    pub results: Vec<(String, DnsVerifyResult)>,
    pub records: Vec<DnsRecord>,
}

pub struct MailboxStatus {
    pub name: String,
    pub address: String,
    pub total: usize,
    pub unread: usize,
    /// Effective trust policy for this mailbox after resolving any
    /// per-mailbox override against the top-level default.
    pub trust: String,
    /// Number of entries in the effective `trusted_senders` list (per-mailbox
    /// override if present, otherwise the top-level default).
    pub trusted_senders_count: usize,
    /// Number of hooks on this mailbox (aggregated across all events).
    pub hook_count: usize,
}

pub fn gather_status(config: &Config) -> StatusInfo {
    gather_status_with_ops(config, &RealSystemOps, &RealNetworkOps::default())
}

/// Injectable seam for testing: takes `SystemOps` + `NetworkOps` implementations
/// so tests can mock service-state probes and DNS resolution without touching
/// the real system or network.
pub fn gather_status_with_ops<S: SystemOps>(
    config: &Config,
    sys: &S,
    net: &dyn NetworkOps,
) -> StatusInfo {
    let dkim_key_present = crate::config::dkim_dir().join("private.key").exists();
    let smtp_running = sys.is_service_running("aimx");
    let runtime_dir = crate::serve::runtime_dir();
    let stale_send_sock_present =
        runtime_dir.join("send.sock").exists() && !runtime_dir.join("aimx.sock").exists();

    let mut mailboxes: Vec<MailboxStatus> = config
        .mailboxes
        .iter()
        .map(|(name, mb_config)| {
            let dir = config.mailbox_dir(name);
            let (total, unread) = count_messages(&dir);
            MailboxStatus {
                name: name.clone(),
                address: mb_config.address.clone(),
                total,
                unread,
                trust: mb_config.effective_trust(config).to_string(),
                trusted_senders_count: mb_config.effective_trusted_senders(config).len(),
                hook_count: mb_config.hooks.len(),
            }
        })
        .collect();

    mailboxes.sort_by(|a, b| a.name.cmp(&b.name));

    let dns = gather_dns_section(config, net);

    StatusInfo {
        domain: config.domain.clone(),
        data_dir: config.data_dir.to_string_lossy().to_string(),
        config_path: crate::config::config_path().to_string_lossy().to_string(),
        dkim_selector: config.dkim_selector.clone(),
        dkim_key_present,
        smtp_running,
        stale_send_sock_present,
        default_trust: config.trust.clone(),
        default_trusted_senders: config.trusted_senders.clone(),
        mailboxes,
        dns,
    }
}

fn gather_dns_section(config: &Config, net: &dyn NetworkOps) -> Option<DnsSection> {
    let (ipv4, ipv6) = net.get_server_ips().ok()?;
    let server_ipv4 = ipv4?;
    let server_ip: IpAddr = IpAddr::V4(server_ipv4);
    let server_ipv6: Option<IpAddr> = if config.enable_ipv6 {
        ipv6.map(IpAddr::V6)
    } else {
        None
    };

    let dkim_dir = crate::config::dkim_dir();
    let local_dkim_pubkey = if dkim_dir.join("public.key").exists() {
        crate::dkim::dns_record_value(&dkim_dir)
            .ok()
            .and_then(|v| v.strip_prefix("v=DKIM1; k=rsa; p=").map(|s| s.to_string()))
    } else {
        None
    };

    let results = setup::verify_all_dns(
        net,
        &config.domain,
        &server_ip,
        server_ipv6.as_ref(),
        &config.dkim_selector,
        local_dkim_pubkey.as_deref(),
    );

    let server_ipv6_str = server_ipv6.map(|ip| ip.to_string());
    let mut records = setup::generate_dns_records(
        &config.domain,
        &server_ip.to_string(),
        server_ipv6_str.as_deref(),
        local_dkim_pubkey
            .as_deref()
            .map(|p| format!("v=DKIM1; k=rsa; p={p}"))
            .unwrap_or_default()
            .as_str(),
        &config.dkim_selector,
    );

    // Without a local DKIM public key on disk we have no authoritative value
    // to suggest in the "→ Add:" hint, so drop the DKIM record and the hint is
    // suppressed. The DKIM DNS check itself still runs (it just verifies the
    // record exists) and its FAIL/MISSING badge is still rendered.
    if local_dkim_pubkey.is_none() {
        let dkim_name = format!("{}._domainkey.{}", config.dkim_selector, config.domain);
        records.retain(|r| !(r.record_type == "TXT" && r.name == dkim_name));
    }

    Some(DnsSection { results, records })
}

fn count_messages(dir: &Path) -> (usize, usize) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return (0, 0),
    };

    let mut total = 0;
    let mut unread = 0;

    for entry in entries.flatten() {
        let path = entry.path();
        let md_path = if path.is_dir() {
            // Bundle directory: look for the `<stem>.md` inside.
            let stem = match path.file_name().and_then(|f| f.to_str()) {
                Some(s) => s.to_string(),
                None => continue,
            };
            let candidate = path.join(format!("{stem}.md"));
            if !candidate.exists() {
                continue;
            }
            candidate
        } else if path.extension().is_some_and(|ext| ext == "md") {
            path
        } else {
            continue;
        };

        total += 1;
        if is_unread(&md_path) {
            unread += 1;
        }
    }

    (total, unread)
}

fn is_unread(path: &Path) -> bool {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return false,
    };

    let frontmatter = match extract_frontmatter(&content) {
        Some(fm) => fm,
        None => return false,
    };

    match toml::from_str::<InboundFrontmatter>(&frontmatter) {
        Ok(meta) => !meta.read,
        Err(_) => false,
    }
}

fn extract_frontmatter(content: &str) -> Option<String> {
    let trimmed = content.trim_start();
    if !trimmed.starts_with("+++") {
        return None;
    }

    let after_first = &trimmed[3..];
    let end = after_first.find("+++")?;
    Some(after_first[..end].to_string())
}

/// Render the per-mailbox ASCII table for the `Mailboxes` section of
/// `aimx doctor`. Columns: Mailbox, Address, Total, Unread, Trust, Senders,
/// Hooks. Numeric columns right-align; text columns left-align. Column
/// widths auto-size to the widest visible cell (header or row value).
///
/// The mailbox name cell is rendered with `term::highlight`, but padding
/// is computed against the plain (ANSI-stripped) cell length so alignment
/// is preserved when color is enabled.
fn render_mailbox_table(mailboxes: &[MailboxStatus]) -> String {
    const HEADERS: [&str; 7] = [
        "Mailbox", "Address", "Total", "Unread", "Trust", "Senders", "Hooks",
    ];
    // right-align mask: Total, Unread, Senders, Hooks
    const RIGHT: [bool; 7] = [false, false, true, true, false, true, true];

    // Plain-text cells (no ANSI) used for width computation and for every
    // column except the mailbox name. The name is rendered with
    // `term::highlight` when emitted below, but we pad against the plain
    // length so alignment survives ANSI escapes.
    let rows: Vec<[String; 7]> = mailboxes
        .iter()
        .map(|mb| {
            [
                mb.name.clone(),
                mb.address.clone(),
                mb.total.to_string(),
                mb.unread.to_string(),
                mb.trust.clone(),
                mb.trusted_senders_count.to_string(),
                mb.hook_count.to_string(),
            ]
        })
        .collect();

    let mut widths = [0usize; 7];
    for (i, h) in HEADERS.iter().enumerate() {
        widths[i] = h.chars().count();
    }
    for row in &rows {
        for (i, cell) in row.iter().enumerate() {
            widths[i] = widths[i].max(cell.chars().count());
        }
    }

    let mut out = String::new();
    let frame = {
        let mut s = String::from("  +");
        for w in &widths {
            s.push('-');
            for _ in 0..*w {
                s.push('-');
            }
            s.push('-');
            s.push('+');
        }
        s.push('\n');
        s
    };

    // Header row.
    out.push_str(&frame);
    out.push_str("  |");
    for (i, h) in HEADERS.iter().enumerate() {
        let pad = widths[i] - h.chars().count();
        if RIGHT[i] {
            out.push(' ');
            for _ in 0..pad {
                out.push(' ');
            }
            out.push_str(h);
            out.push(' ');
        } else {
            out.push(' ');
            out.push_str(h);
            for _ in 0..pad {
                out.push(' ');
            }
            out.push(' ');
        }
        out.push('|');
    }
    out.push('\n');
    out.push_str(&frame);

    // Data rows. The Mailbox cell is colored via `term::highlight`; all
    // other cells render plain.
    for row in &rows {
        out.push_str("  |");
        for (i, cell) in row.iter().enumerate() {
            let visible_len = cell.chars().count();
            let pad = widths[i] - visible_len;
            if RIGHT[i] {
                out.push(' ');
                for _ in 0..pad {
                    out.push(' ');
                }
                out.push_str(cell);
                out.push(' ');
            } else {
                out.push(' ');
                if i == 0 {
                    // Mailbox name: emit with highlight, pad against plain length.
                    out.push_str(&term::highlight(cell).to_string());
                } else {
                    out.push_str(cell);
                }
                for _ in 0..pad {
                    out.push(' ');
                }
                out.push(' ');
            }
            out.push('|');
        }
        out.push('\n');
    }
    out.push_str(&frame);

    out
}

pub fn format_status(info: &StatusInfo) -> String {
    let mut out = String::new();

    out.push_str(&format!("{}\n", term::header("Configuration")));
    out.push_str(&format!("Domain:           {}\n", info.domain));
    out.push_str(&format!("Config file:      {}\n", info.config_path));
    out.push_str(&format!("Data directory:   {}\n", info.data_dir));
    out.push_str(&format!("DKIM selector:    {}\n", info.dkim_selector));
    out.push_str(&format!(
        "DKIM key:         {}\n",
        if info.dkim_key_present {
            term::success("present")
        } else {
            term::warn("MISSING - run `aimx dkim-keygen`")
        }
    ));
    out.push_str(&format!(
        "Global trust:     {}\n",
        term::info(&info.default_trust),
    ));
    let senders_line = if info.default_trusted_senders.is_empty() {
        "(none)".to_string()
    } else {
        info.default_trusted_senders.join(", ")
    };
    out.push_str(&format!("Trusted senders:  {senders_line}\n"));

    out.push_str(&format!("\n{}\n", term::header("Service")));
    out.push_str(&format!(
        "SMTP server:      {}\n",
        if info.smtp_running {
            term::success("running")
        } else {
            term::warn("not running")
        }
    ));
    if info.stale_send_sock_present {
        out.push_str(&format!(
            "UDS socket:       {} - the runtime socket was renamed to `aimx.sock`; restart `aimx serve` to replace the stale `send.sock`\n",
            term::warn("stale send.sock detected"),
        ));
    }

    let total_msgs: usize = info.mailboxes.iter().map(|m| m.total).sum();
    let total_unread: usize = info.mailboxes.iter().map(|m| m.unread).sum();
    out.push_str(&format!("\n{}\n", term::header("Mailboxes")));
    out.push_str(&format!(
        "Total:            {} ({} messages, {} unread)\n",
        info.mailboxes.len(),
        total_msgs,
        total_unread,
    ));

    if !info.mailboxes.is_empty() {
        out.push('\n');
        out.push_str(&render_mailbox_table(&info.mailboxes));
    }

    out.push_str(&format!("\n{}\n", term::header("DNS")));
    match &info.dns {
        Some(dns) => {
            let (lines, _) = setup::dns_verification_record_lines(&dns.results, &dns.records);
            for line in lines {
                out.push_str(&format!("{line}\n"));
            }
        }
        None => {
            out.push_str(&format!(
                "  {} - could not determine server IP\n",
                term::warn("skipped"),
            ));
        }
    }

    out.push_str(&format!("\n{}\n", term::header("Logs")));
    out.push_str(&format!(
        "  {}\n",
        term::dim("Run `aimx logs` to view recent logs, or `aimx logs --follow` to tail."),
    ));

    out
}

pub fn run(config: Config) -> Result<(), Box<dyn std::error::Error>> {
    let info = gather_status(&config);
    print!("{}", format_status(&info));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::setup::Port25Status;
    use std::cell::Cell;
    use std::collections::HashMap;
    use std::net::{Ipv4Addr, Ipv6Addr};
    use std::path::PathBuf;

    /// Test double for DNS resolution + server-IP discovery. Defaults to a
    /// server with IPv4 `1.2.3.4`, no IPv6, and empty DNS tables. Tests
    /// override fields to simulate matching or drifted DNS records.
    struct MockNetworkOps {
        server_ipv4: Option<Ipv4Addr>,
        server_ipv6: Option<Ipv6Addr>,
        get_server_ips_fails: bool,
        mx_records: HashMap<String, Vec<String>>,
        a_records: HashMap<String, Vec<IpAddr>>,
        aaaa_records: HashMap<String, Vec<IpAddr>>,
        txt_records: HashMap<String, Vec<String>>,
        resolve_calls: Cell<u32>,
    }

    impl Default for MockNetworkOps {
        fn default() -> Self {
            Self {
                server_ipv4: Some("1.2.3.4".parse().unwrap()),
                server_ipv6: None,
                get_server_ips_fails: false,
                mx_records: HashMap::new(),
                a_records: HashMap::new(),
                aaaa_records: HashMap::new(),
                txt_records: HashMap::new(),
                resolve_calls: Cell::new(0),
            }
        }
    }

    impl NetworkOps for MockNetworkOps {
        fn check_outbound_port25(&self) -> Result<bool, Box<dyn std::error::Error>> {
            unreachable!("gather_status must not touch check_outbound_port25")
        }
        fn check_inbound_port25(&self) -> Result<bool, Box<dyn std::error::Error>> {
            unreachable!("gather_status must not touch check_inbound_port25")
        }
        fn get_server_ips(
            &self,
        ) -> Result<(Option<Ipv4Addr>, Option<Ipv6Addr>), Box<dyn std::error::Error>> {
            if self.get_server_ips_fails {
                return Err("mock get_server_ips failure".into());
            }
            Ok((self.server_ipv4, self.server_ipv6))
        }
        fn resolve_mx(&self, domain: &str) -> Result<Vec<String>, Box<dyn std::error::Error>> {
            self.resolve_calls.set(self.resolve_calls.get() + 1);
            Ok(self.mx_records.get(domain).cloned().unwrap_or_default())
        }
        fn resolve_a(&self, domain: &str) -> Result<Vec<IpAddr>, Box<dyn std::error::Error>> {
            self.resolve_calls.set(self.resolve_calls.get() + 1);
            Ok(self.a_records.get(domain).cloned().unwrap_or_default())
        }
        fn resolve_aaaa(&self, domain: &str) -> Result<Vec<IpAddr>, Box<dyn std::error::Error>> {
            self.resolve_calls.set(self.resolve_calls.get() + 1);
            Ok(self.aaaa_records.get(domain).cloned().unwrap_or_default())
        }
        fn resolve_txt(&self, domain: &str) -> Result<Vec<String>, Box<dyn std::error::Error>> {
            self.resolve_calls.set(self.resolve_calls.get() + 1);
            Ok(self.txt_records.get(domain).cloned().unwrap_or_default())
        }
    }

    /// Minimal mock that exercises `is_service_running`. The log-tail
    /// hook is retained only to assert doctor does NOT call it. All
    /// other `SystemOps` methods panic; they must not be reached by
    /// `gather_status`.
    struct FakeServiceOps {
        running: bool,
        log_tail_calls: Cell<u32>,
    }

    impl FakeServiceOps {
        fn new(running: bool) -> Self {
            Self {
                running,
                log_tail_calls: Cell::new(0),
            }
        }
    }

    impl SystemOps for FakeServiceOps {
        fn write_file(
            &self,
            _path: &Path,
            _content: &str,
        ) -> Result<(), Box<dyn std::error::Error>> {
            unreachable!("gather_status must not touch write_file")
        }
        fn file_exists(&self, _path: &Path) -> bool {
            unreachable!("gather_status must not touch file_exists")
        }
        fn restart_service(&self, _service: &str) -> Result<(), Box<dyn std::error::Error>> {
            unreachable!("gather_status must not touch restart_service")
        }
        fn is_service_running(&self, service: &str) -> bool {
            assert_eq!(service, "aimx", "status must query the aimx service");
            self.running
        }
        fn generate_tls_cert(
            &self,
            _cert_dir: &Path,
            _domain: &str,
        ) -> Result<(), Box<dyn std::error::Error>> {
            unreachable!("gather_status must not touch generate_tls_cert")
        }
        fn get_aimx_binary_path(&self) -> Result<PathBuf, Box<dyn std::error::Error>> {
            unreachable!("gather_status must not touch get_aimx_binary_path")
        }
        fn check_root(&self) -> bool {
            unreachable!("gather_status must not touch check_root")
        }
        fn check_port25_occupancy(&self) -> Result<Port25Status, Box<dyn std::error::Error>> {
            unreachable!("gather_status must not touch check_port25_occupancy")
        }
        fn install_service_file(&self, _data_dir: &Path) -> Result<(), Box<dyn std::error::Error>> {
            unreachable!("gather_status must not touch install_service_file")
        }
        fn uninstall_service_file(&self) -> Result<(), Box<dyn std::error::Error>> {
            unreachable!("gather_status must not touch uninstall_service_file")
        }
        fn wait_for_service_ready(&self) -> bool {
            unreachable!("gather_status must not touch wait_for_service_ready")
        }
        fn tail_service_logs(
            &self,
            _unit: &str,
            _n: usize,
        ) -> Result<String, Box<dyn std::error::Error>> {
            self.log_tail_calls.set(self.log_tail_calls.get() + 1);
            Err("doctor must not tail service logs".into())
        }
        fn follow_service_logs(&self, _unit: &str) -> Result<(), Box<dyn std::error::Error>> {
            unreachable!("gather_status must not touch follow_service_logs")
        }
    }

    fn empty_config(data_dir: &Path) -> Config {
        Config {
            domain: "test.com".to_string(),
            data_dir: data_dir.to_path_buf(),
            dkim_selector: "aimx".to_string(),
            trust: "none".to_string(),
            trusted_senders: vec![],
            hook_templates: Vec::new(),
            mailboxes: std::collections::HashMap::new(),
            verify_host: None,
            enable_ipv6: false,
        }
    }

    #[test]
    fn gather_status_reports_running_when_systemops_returns_true() {
        let tmp = tempfile::TempDir::new().unwrap();
        let _cfg_guard = crate::config::test_env::ConfigDirOverride::set(tmp.path());

        let config = empty_config(tmp.path());
        let net = MockNetworkOps::default();
        let info = gather_status_with_ops(&config, &FakeServiceOps::new(true), &net);
        assert!(info.smtp_running);
    }

    #[test]
    fn gather_status_reports_not_running_when_systemops_returns_false() {
        let tmp = tempfile::TempDir::new().unwrap();
        let _cfg_guard = crate::config::test_env::ConfigDirOverride::set(tmp.path());

        let config = empty_config(tmp.path());
        let net = MockNetworkOps::default();
        let info = gather_status_with_ops(&config, &FakeServiceOps::new(false), &net);
        assert!(!info.smtp_running);
    }

    // Manual verification note (S43-2): on OpenRC hosts (Alpine) the real
    // `RealSystemOps::is_service_running` dispatches to `rc-service aimx status`
    // via `crate::serve::service::is_service_running_command`. The previous
    // hardcoded `systemctl is-active` call always returned false on OpenRC.
    // With this refactor, `aimx status` now reports the correct state on
    // both systemd and OpenRC hosts.

    #[test]
    fn format_status_no_mailboxes() {
        let info = StatusInfo {
            domain: "test.example.com".to_string(),
            data_dir: "/var/lib/aimx".to_string(),
            dkim_selector: "aimx".to_string(),
            config_path: "/etc/aimx/config.toml".to_string(),
            dkim_key_present: true,
            smtp_running: true,
            stale_send_sock_present: false,
            default_trust: "none".to_string(),
            default_trusted_senders: vec![],
            mailboxes: vec![],
            dns: None,
        };
        let output = format_status(&info);
        assert!(output.contains("test.example.com"));
        assert!(output.contains("present"));
        assert!(output.contains("running"));
        assert!(output.contains("0 (0 messages, 0 unread)"));
        assert!(
            !output.contains("stale send.sock"),
            "no stale-socket warning when flag is false"
        );
    }

    #[test]
    fn format_status_flags_stale_send_sock() {
        let info = StatusInfo {
            domain: "test.example.com".to_string(),
            data_dir: "/var/lib/aimx".to_string(),
            dkim_selector: "aimx".to_string(),
            config_path: "/etc/aimx/config.toml".to_string(),
            dkim_key_present: true,
            smtp_running: true,
            stale_send_sock_present: true,
            default_trust: "none".to_string(),
            default_trusted_senders: vec![],
            mailboxes: vec![],
            dns: None,
        };
        let output = format_status(&info);
        assert!(
            output.contains("stale send.sock"),
            "expected stale-socket warning in doctor output: {output}"
        );
        assert!(
            output.contains("aimx.sock"),
            "warning should mention the new socket name: {output}"
        );
    }

    #[test]
    fn format_status_with_mailboxes() {
        let info = StatusInfo {
            domain: "agent.example.com".to_string(),
            data_dir: "/var/lib/aimx".to_string(),
            dkim_selector: "aimx".to_string(),
            config_path: "/etc/aimx/config.toml".to_string(),
            dkim_key_present: true,
            smtp_running: false,
            stale_send_sock_present: false,
            default_trust: "none".to_string(),
            default_trusted_senders: vec![],
            mailboxes: vec![
                MailboxStatus {
                    name: "catchall".to_string(),
                    address: "*@agent.example.com".to_string(),
                    total: 10,
                    unread: 3,
                    trust: "none".to_string(),
                    trusted_senders_count: 0,
                    hook_count: 0,
                },
                MailboxStatus {
                    name: "support".to_string(),
                    address: "support@agent.example.com".to_string(),
                    total: 5,
                    unread: 1,
                    trust: "none".to_string(),
                    trusted_senders_count: 0,
                    hook_count: 0,
                },
            ],
            dns: None,
        };
        let output = format_status(&info);
        assert!(output.contains("agent.example.com"));
        assert!(output.contains("not running"));
        assert!(output.contains("2 (15 messages, 4 unread)"));
        assert!(output.contains("catchall"));
        assert!(output.contains("support"));
        assert!(output.contains("Mailbox"));
        assert!(output.contains("Total"));
        assert!(output.contains("Unread"));
    }

    #[test]
    fn format_status_mailbox_table_has_title_case_headers() {
        let info = StatusInfo {
            domain: "ex.com".to_string(),
            data_dir: "/var/lib/aimx".to_string(),
            dkim_selector: "aimx".to_string(),
            config_path: "/etc/aimx/config.toml".to_string(),
            dkim_key_present: true,
            smtp_running: true,
            stale_send_sock_present: false,
            default_trust: "none".to_string(),
            default_trusted_senders: vec![],
            mailboxes: vec![MailboxStatus {
                name: "ops".to_string(),
                address: "ops@ex.com".to_string(),
                total: 0,
                unread: 0,
                trust: "none".to_string(),
                trusted_senders_count: 0,
                hook_count: 0,
            }],
            dns: None,
        };
        let output = format_status(&info);
        for header in &[
            "Mailbox", "Address", "Total", "Unread", "Trust", "Senders", "Hooks",
        ] {
            assert!(
                output.contains(header),
                "Mailboxes table must contain title-case header {header:?}, got:\n{output}"
            );
        }
        // The old all-caps header row must be gone.
        for token in &["MAILBOX", "ADDRESS", "TOTAL", "UNREAD"] {
            assert!(
                !output.contains(token),
                "Mailboxes table must NOT contain old all-caps header {token:?}, got:\n{output}"
            );
        }
    }

    #[test]
    fn format_status_mailbox_table_drops_trailing_trust_line() {
        let info = StatusInfo {
            domain: "ex.com".to_string(),
            data_dir: "/var/lib/aimx".to_string(),
            dkim_selector: "aimx".to_string(),
            config_path: "/etc/aimx/config.toml".to_string(),
            dkim_key_present: true,
            smtp_running: true,
            stale_send_sock_present: false,
            default_trust: "none".to_string(),
            default_trusted_senders: vec![],
            mailboxes: vec![MailboxStatus {
                name: "ops".to_string(),
                address: "ops@ex.com".to_string(),
                total: 0,
                unread: 0,
                trust: "verified".to_string(),
                trusted_senders_count: 1,
                hook_count: 2,
            }],
            dns: None,
        };
        let output = format_status(&info);
        // The old `→ trust = …` indented summary line must be gone — its data
        // is now in the Trust / Senders / Hooks columns of the table.
        assert!(
            !output.contains("→ trust ="),
            "old trailing trust summary line must be dropped, got:\n{output}"
        );
        assert!(
            !output.contains("trusted_senders: 1 entries"),
            "old trusted_senders summary phrasing must be dropped, got:\n{output}"
        );
    }

    #[test]
    fn format_status_mailbox_table_renders_ascii_frame() {
        let info = StatusInfo {
            domain: "ex.com".to_string(),
            data_dir: "/var/lib/aimx".to_string(),
            dkim_selector: "aimx".to_string(),
            config_path: "/etc/aimx/config.toml".to_string(),
            dkim_key_present: true,
            smtp_running: true,
            stale_send_sock_present: false,
            default_trust: "none".to_string(),
            default_trusted_senders: vec![],
            mailboxes: vec![MailboxStatus {
                name: "ops".to_string(),
                address: "ops@ex.com".to_string(),
                total: 0,
                unread: 0,
                trust: "none".to_string(),
                trusted_senders_count: 0,
                hook_count: 0,
            }],
            dns: None,
        };
        let output = format_status(&info);
        // Frame line starts with `+--` (after a two-space indent) and row
        // separators use `|`. These are load-bearing for the visible grid.
        assert!(
            output.lines().any(|l| l.trim_start().starts_with("+--")),
            "table must include an ASCII frame line starting with '+--', got:\n{output}"
        );
        assert!(
            output.contains('|'),
            "table rows must include '|' column separators, got:\n{output}"
        );
    }

    #[test]
    fn mailbox_table_columns_align_regardless_of_color() {
        let info = StatusInfo {
            domain: "ex.com".to_string(),
            data_dir: "/var/lib/aimx".to_string(),
            dkim_selector: "aimx".to_string(),
            config_path: "/etc/aimx/config.toml".to_string(),
            dkim_key_present: true,
            smtp_running: true,
            stale_send_sock_present: false,
            default_trust: "none".to_string(),
            default_trusted_senders: vec![],
            mailboxes: vec![
                MailboxStatus {
                    name: "ops".to_string(),
                    address: "ops@ex.com".to_string(),
                    total: 1,
                    unread: 0,
                    trust: "none".to_string(),
                    trusted_senders_count: 0,
                    hook_count: 0,
                },
                MailboxStatus {
                    name: "catchall".to_string(),
                    address: "*@ex.com".to_string(),
                    total: 2,
                    unread: 1,
                    trust: "none".to_string(),
                    trusted_senders_count: 0,
                    hook_count: 0,
                },
            ],
            dns: None,
        };

        // Returns the visible column where the Address cell starts on each
        // mailbox data row. In the ASCII table format each row is
        // `  | <name> | <address> | …`, so the Address column begins right
        // after the second `| ` separator (the one that follows the mailbox
        // name cell). The bug this guards: ANSI escapes in the name cell
        // must not push the second separator out of alignment.
        fn address_column(output: &str) -> Vec<usize> {
            let ansi = regex_like_strip(output);
            ansi.lines()
                .filter(|l| l.contains("@ex.com"))
                .filter_map(|l| {
                    // Find the `|` starting the Mailbox cell, then the `|`
                    // starting the Address cell. The Address content starts
                    // one character after the second `|` (the `| ` padding).
                    let first_pipe = l.find('|')?;
                    let second_pipe = l[first_pipe + 1..].find('|')? + first_pipe + 1;
                    // Skip the single space padding after the `|`.
                    Some(second_pipe + 2)
                })
                .collect()
        }

        // Force color on so ANSI escapes land in the formatted output; then
        // strip them and check that the visible address column still aligns.
        // The bug this guards: Rust's width formatter counts escape bytes as
        // visible chars, so colored `{:<20}` padding misaligns.
        colored::control::set_override(true);
        let colored_out = format_status(&info);
        colored::control::unset_override();

        let cols = address_column(&colored_out);
        assert_eq!(cols.len(), 2, "expected two mailbox rows, got {cols:?}");
        assert_eq!(
            cols[0], cols[1],
            "mailbox rows must share a common visible address column after ANSI strip"
        );
    }

    // Minimal ANSI-strip helper for test assertions (avoid a new dep).
    fn regex_like_strip(s: &str) -> String {
        let mut out = String::with_capacity(s.len());
        let bytes = s.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == 0x1b && i + 1 < bytes.len() && bytes[i + 1] == b'[' {
                i += 2;
                while i < bytes.len() && !bytes[i].is_ascii_alphabetic() {
                    i += 1;
                }
                if i < bytes.len() {
                    i += 1;
                }
            } else {
                out.push(bytes[i] as char);
                i += 1;
            }
        }
        out
    }

    #[test]
    fn format_status_missing_dkim() {
        let info = StatusInfo {
            domain: "test.com".to_string(),
            data_dir: "/var/lib/aimx".to_string(),
            dkim_selector: "aimx".to_string(),
            config_path: "/etc/aimx/config.toml".to_string(),
            dkim_key_present: false,
            smtp_running: false,
            stale_send_sock_present: false,
            default_trust: "none".to_string(),
            default_trusted_senders: vec![],
            mailboxes: vec![],
            dns: None,
        };
        let output = format_status(&info);
        assert!(output.contains("MISSING"));
        assert!(output.contains("dkim-keygen"));
    }

    #[test]
    fn extract_frontmatter_valid() {
        let content = "+++\nid = \"test\"\nread = false\n+++\nBody here";
        let fm = extract_frontmatter(content).unwrap();
        assert!(fm.contains("id = \"test\""));
        assert!(fm.contains("read = false"));
    }

    #[test]
    fn extract_frontmatter_no_marker() {
        assert!(extract_frontmatter("No frontmatter here").is_none());
    }

    #[test]
    fn extract_frontmatter_no_end_marker() {
        assert!(extract_frontmatter("+++\nid = \"test\"\nno end").is_none());
    }

    #[test]
    fn count_messages_empty_dir() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (total, unread) = count_messages(tmp.path());
        assert_eq!(total, 0);
        assert_eq!(unread, 0);
    }

    #[test]
    fn count_messages_nonexistent_dir() {
        let (total, unread) = count_messages(Path::new("/nonexistent/path"));
        assert_eq!(total, 0);
        assert_eq!(unread, 0);
    }

    #[test]
    fn count_messages_with_emails() {
        let tmp = tempfile::TempDir::new().unwrap();

        let unread_content = "+++\nid = \"001\"\nmessage_id = \"<a@b>\"\nfrom = \"a@b\"\nto = \"c@d\"\nsubject = \"Test\"\ndate = \"2025-01-01\"\nin_reply_to = \"\"\nreferences = \"\"\nattachments = []\nmailbox = \"catchall\"\nread = false\ndkim = \"none\"\nspf = \"none\"\n+++\nBody";
        let read_content = "+++\nid = \"002\"\nmessage_id = \"<a@b>\"\nfrom = \"a@b\"\nto = \"c@d\"\nsubject = \"Test\"\ndate = \"2025-01-01\"\nin_reply_to = \"\"\nreferences = \"\"\nattachments = []\nmailbox = \"catchall\"\nread = true\ndkim = \"none\"\nspf = \"none\"\n+++\nBody";

        std::fs::write(tmp.path().join("2025-01-01-001.md"), unread_content).unwrap();
        std::fs::write(tmp.path().join("2025-01-01-002.md"), read_content).unwrap();
        std::fs::write(tmp.path().join("2025-01-01-003.md"), unread_content).unwrap();
        std::fs::write(tmp.path().join("notes.txt"), "not an email").unwrap();

        let (total, unread) = count_messages(tmp.path());
        assert_eq!(total, 3);
        assert_eq!(unread, 2);
    }

    #[test]
    fn is_unread_true() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("test.md");
        let content = "+++\nid = \"001\"\nmessage_id = \"<a@b>\"\nfrom = \"a@b\"\nto = \"c@d\"\nsubject = \"Test\"\ndate = \"2025-01-01\"\nin_reply_to = \"\"\nreferences = \"\"\nattachments = []\nmailbox = \"catchall\"\nread = false\ndkim = \"none\"\nspf = \"none\"\n+++\nBody";
        std::fs::write(&path, content).unwrap();
        assert!(is_unread(&path));
    }

    #[test]
    fn is_unread_false() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("test.md");
        let content = "+++\nid = \"001\"\nmessage_id = \"<a@b>\"\nfrom = \"a@b\"\nto = \"c@d\"\nsubject = \"Test\"\ndate = \"2025-01-01\"\nin_reply_to = \"\"\nreferences = \"\"\nattachments = []\nmailbox = \"catchall\"\nread = true\ndkim = \"none\"\nspf = \"none\"\n+++\nBody";
        std::fs::write(&path, content).unwrap();
        assert!(!is_unread(&path));
    }

    #[test]
    fn gather_status_with_temp_dir() {
        let tmp = tempfile::TempDir::new().unwrap();
        let data_dir = tmp.path();

        // Point AIMX_CONFIG_DIR at `tmp` so `dkim_dir()` resolves inside it.
        let _cfg_guard = crate::config::test_env::ConfigDirOverride::set(data_dir);

        std::fs::create_dir_all(data_dir.join("dkim")).unwrap();
        std::fs::write(data_dir.join("dkim/private.key"), "test").unwrap();

        std::fs::create_dir_all(data_dir.join("catchall")).unwrap();

        let mut mailboxes = std::collections::HashMap::new();
        mailboxes.insert(
            "catchall".to_string(),
            crate::config::MailboxConfig {
                address: "*@test.com".to_string(),
                hooks: vec![],
                trust: None,
                trusted_senders: None,
            },
        );

        let config = Config {
            domain: "test.com".to_string(),
            data_dir: data_dir.to_path_buf(),
            dkim_selector: "aimx".to_string(),
            trust: "none".to_string(),
            trusted_senders: vec![],
            hook_templates: Vec::new(),
            mailboxes,
            verify_host: None,
            enable_ipv6: false,
        };

        let info = gather_status(&config);
        assert_eq!(info.domain, "test.com");
        assert!(info.dkim_key_present);
        assert_eq!(info.mailboxes.len(), 1);
        assert_eq!(info.mailboxes[0].name, "catchall");
    }

    #[test]
    fn gather_status_includes_dns_when_server_ip_available() {
        let tmp = tempfile::TempDir::new().unwrap();
        let _cfg_guard = crate::config::test_env::ConfigDirOverride::set(tmp.path());

        let config = empty_config(tmp.path());
        let mut net = MockNetworkOps::default();
        // Make the live DNS records line up with the server so we get PASS badges.
        let ip: IpAddr = "1.2.3.4".parse().unwrap();
        net.a_records.insert(config.domain.clone(), vec![ip]);
        net.mx_records
            .insert(config.domain.clone(), vec!["10 test.com.".into()]);
        net.txt_records.insert(
            config.domain.clone(),
            vec!["v=spf1 ip4:1.2.3.4 -all".into()],
        );
        net.txt_records.insert(
            format!("_dmarc.{}", config.domain),
            vec!["v=DMARC1; p=reject".into()],
        );
        net.txt_records.insert(
            format!("aimx._domainkey.{}", config.domain),
            vec!["v=DKIM1; k=rsa; p=AAAA".into()],
        );

        let info = gather_status_with_ops(&config, &FakeServiceOps::new(false), &net);
        let dns = info
            .dns
            .expect("DNS section must be present when server IP is known");
        let names: Vec<&str> = dns.results.iter().map(|(n, _)| n.as_str()).collect();
        assert!(names.contains(&"MX"));
        assert!(names.contains(&"A"));
        assert!(names.contains(&"SPF"));
        assert!(names.contains(&"DKIM"));
        assert!(names.contains(&"DMARC"));
        assert!(
            !names.contains(&"AAAA"),
            "AAAA must NOT appear when enable_ipv6 = false"
        );
        assert!(
            net.resolve_calls.get() > 0,
            "gather_status must perform DNS lookups when building the DNS section"
        );

        // The A-record verification was primed with a matching IP; it must
        // produce Pass. This proves the IP→verify_all_dns→DnsSection wiring
        // end-to-end, not just struct shape.
        let a_result = dns
            .results
            .iter()
            .find(|(n, _)| n == "A")
            .map(|(_, r)| r)
            .expect("A check must be present");
        assert!(
            matches!(a_result, DnsVerifyResult::Pass),
            "A check must Pass when the mock A record matches the server IP, got {a_result:?}"
        );
    }

    #[test]
    fn gather_status_dns_none_when_server_ip_unavailable() {
        let tmp = tempfile::TempDir::new().unwrap();
        let _cfg_guard = crate::config::test_env::ConfigDirOverride::set(tmp.path());

        let config = empty_config(tmp.path());
        let net = MockNetworkOps {
            server_ipv4: None,
            ..Default::default()
        };

        let info = gather_status_with_ops(&config, &FakeServiceOps::new(false), &net);
        assert!(
            info.dns.is_none(),
            "DNS section must be None when the server IPv4 cannot be determined"
        );
    }

    #[test]
    fn gather_status_dns_none_when_get_server_ips_errors() {
        let tmp = tempfile::TempDir::new().unwrap();
        let _cfg_guard = crate::config::test_env::ConfigDirOverride::set(tmp.path());

        let config = empty_config(tmp.path());
        let net = MockNetworkOps {
            get_server_ips_fails: true,
            ..Default::default()
        };

        let info = gather_status_with_ops(&config, &FakeServiceOps::new(false), &net);
        assert!(
            info.dns.is_none(),
            "DNS section must be None when get_server_ips errors out"
        );
    }

    #[test]
    fn gather_dns_drops_dkim_record_when_local_pubkey_missing() {
        // When no DKIM public key is on disk (e.g. setup not yet run, or key
        // removed), `aimx status` must NOT emit a "→ Add:" DNS hint with an
        // empty `p=` value. Guarding this at the `records` level is the
        // cheapest way: dns_record_for_check falls back to None for DKIM,
        // and dns_verification_lines omits the hint line.
        let tmp = tempfile::TempDir::new().unwrap();
        let _cfg_guard = crate::config::test_env::ConfigDirOverride::set(tmp.path());

        // Do NOT write a DKIM public.key. This is the scenario under test.
        assert!(
            !crate::config::dkim_dir().join("public.key").exists(),
            "precondition: no DKIM public key on disk"
        );

        let config = empty_config(tmp.path());
        let net = MockNetworkOps::default();

        let info = gather_status_with_ops(&config, &FakeServiceOps::new(false), &net);
        let dns = info.dns.expect("DNS section must be present");

        let dkim_name = format!("aimx._domainkey.{}", config.domain);
        let has_dkim_record = dns
            .records
            .iter()
            .any(|r| r.record_type == "TXT" && r.name == dkim_name);
        assert!(
            !has_dkim_record,
            "DnsSection.records must NOT contain a DKIM TXT record with an empty p= \
             value when the local public key is absent (got: {:?})",
            dns.records
                .iter()
                .find(|r| r.name == dkim_name)
                .map(|r| r.value.as_str())
        );
    }

    #[test]
    fn gather_dns_includes_aaaa_when_ipv6_enabled() {
        let tmp = tempfile::TempDir::new().unwrap();
        let _cfg_guard = crate::config::test_env::ConfigDirOverride::set(tmp.path());

        let mut config = empty_config(tmp.path());
        config.enable_ipv6 = true;

        let net = MockNetworkOps {
            server_ipv6: Some("2001:db8::1".parse().unwrap()),
            ..Default::default()
        };

        let info = gather_status_with_ops(&config, &FakeServiceOps::new(false), &net);
        let dns = info.dns.expect("DNS section must be present");
        let names: Vec<&str> = dns.results.iter().map(|(n, _)| n.as_str()).collect();
        assert!(
            names.contains(&"AAAA"),
            "AAAA must appear when enable_ipv6 = true and server has an IPv6 address"
        );
        assert!(
            names.contains(&"SPF (IPv6)"),
            "SPF (IPv6) must appear when enable_ipv6 = true"
        );
    }

    #[test]
    fn format_status_renders_dns_section() {
        let results = vec![
            ("MX".into(), DnsVerifyResult::Pass),
            ("A".into(), DnsVerifyResult::Pass),
        ];
        let records = setup::generate_dns_records(
            "test.com",
            "1.2.3.4",
            None,
            "v=DKIM1; k=rsa; p=AAAA",
            "aimx",
        );
        let info = StatusInfo {
            domain: "test.com".to_string(),
            data_dir: "/var/lib/aimx".to_string(),
            dkim_selector: "aimx".to_string(),
            config_path: "/etc/aimx/config.toml".to_string(),
            dkim_key_present: true,
            smtp_running: true,
            stale_send_sock_present: false,
            default_trust: "none".to_string(),
            default_trusted_senders: vec![],
            mailboxes: vec![],
            dns: Some(DnsSection { results, records }),
        };

        let output = format_status(&info);
        // Header for the new section. `term::header` wraps with "===" in color
        // mode and plain text otherwise; "DNS" is the load-bearing substring.
        assert!(
            output.contains("DNS"),
            "format_status must render a DNS section header, got:\n{output}"
        );
        // Per-record lines come from `dns_verification_lines`.
        assert!(
            output.contains("MX:"),
            "format_status must render per-record DNS lines, got:\n{output}"
        );
        assert!(output.contains("A:"));
    }

    #[test]
    fn format_status_renders_dns_skipped_when_none() {
        let info = StatusInfo {
            domain: "test.com".to_string(),
            data_dir: "/var/lib/aimx".to_string(),
            dkim_selector: "aimx".to_string(),
            config_path: "/etc/aimx/config.toml".to_string(),
            dkim_key_present: true,
            smtp_running: true,
            stale_send_sock_present: false,
            default_trust: "none".to_string(),
            default_trusted_senders: vec![],
            mailboxes: vec![],
            dns: None,
        };

        let output = format_status(&info);
        assert!(
            output.contains("DNS"),
            "DNS header must still render when dns is None"
        );
        assert!(
            output.contains("skipped"),
            "format_status must mark DNS as skipped when dns is None, got:\n{output}"
        );
    }

    // ----- Logs pointer section ---------------------------------------

    #[test]
    fn gather_status_does_not_tail_service_logs() {
        let tmp = tempfile::TempDir::new().unwrap();
        let _cfg_guard = crate::config::test_env::ConfigDirOverride::set(tmp.path());
        let config = empty_config(tmp.path());
        let net = MockNetworkOps::default();
        let ops = FakeServiceOps::new(true);

        let _info = gather_status_with_ops(&config, &ops, &net);
        assert_eq!(
            ops.log_tail_calls.get(),
            0,
            "doctor must not tail service logs; it now prints a pointer to `aimx logs`"
        );
    }

    #[test]
    fn format_status_renders_logs_pointer_section() {
        let info = StatusInfo {
            domain: "test.com".to_string(),
            data_dir: "/var/lib/aimx".to_string(),
            dkim_selector: "aimx".to_string(),
            config_path: "/etc/aimx/config.toml".to_string(),
            dkim_key_present: true,
            smtp_running: true,
            stale_send_sock_present: false,
            default_trust: "none".to_string(),
            default_trusted_senders: vec![],
            mailboxes: vec![],
            dns: None,
        };
        let output = format_status(&info);
        assert!(
            output.contains("Logs"),
            "format_status must include a 'Logs' header, got:\n{output}"
        );
        assert!(
            output.contains("aimx logs"),
            "format_status must point the operator at `aimx logs`, got:\n{output}"
        );
        assert!(
            output.contains("aimx logs --follow"),
            "hint must also mention `aimx logs --follow` for tailing, got:\n{output}"
        );
        assert!(
            !output.contains("Recent logs"),
            "old 'Recent logs' header must be gone, got:\n{output}"
        );
    }

    // ----- S48-2 config path + per-mailbox trust + hooks summary -----

    #[test]
    fn format_status_renders_config_file_path() {
        let info = StatusInfo {
            domain: "test.com".to_string(),
            data_dir: "/var/lib/aimx".to_string(),
            config_path: "/etc/aimx/config.toml".to_string(),
            dkim_selector: "aimx".to_string(),
            dkim_key_present: true,
            smtp_running: true,
            stale_send_sock_present: false,
            default_trust: "verified".to_string(),
            default_trusted_senders: vec![
                "alice@example.com".to_string(),
                "bob@example.com".to_string(),
            ],
            mailboxes: vec![],
            dns: None,
        };
        let output = format_status(&info);
        assert!(
            output.contains("Config file:"),
            "Configuration section must include 'Config file:' label: {output}"
        );
        assert!(
            output.contains("/etc/aimx/config.toml"),
            "config path must render verbatim so operators can copy it: {output}"
        );
        assert!(
            output.contains("Global trust:"),
            "Configuration section must include 'Global trust:' label: {output}"
        );
        assert!(
            !output.contains("Default trust:"),
            "old 'Default trust:' label must be gone: {output}"
        );
        assert!(
            output.contains("verified"),
            "global trust value must render: {output}"
        );
        assert!(
            !output.contains("trusted_senders)"),
            "old '(N trusted_senders)' suffix must be gone: {output}"
        );
    }

    #[test]
    fn format_status_renders_trusted_senders_list() {
        let info = StatusInfo {
            domain: "test.com".to_string(),
            data_dir: "/var/lib/aimx".to_string(),
            config_path: "/etc/aimx/config.toml".to_string(),
            dkim_selector: "aimx".to_string(),
            dkim_key_present: true,
            smtp_running: true,
            stale_send_sock_present: false,
            default_trust: "verified".to_string(),
            default_trusted_senders: vec![
                "alice@example.com".to_string(),
                "bob@example.com".to_string(),
            ],
            mailboxes: vec![],
            dns: None,
        };
        let output = format_status(&info);
        assert!(
            output.contains("Trusted senders:"),
            "Configuration section must include 'Trusted senders:' label: {output}"
        );
        assert!(
            output.contains("alice@example.com, bob@example.com"),
            "Trusted senders line must render the configured list, comma-separated: {output}"
        );
    }

    #[test]
    fn format_status_renders_trusted_senders_none_when_empty() {
        let info = StatusInfo {
            domain: "test.com".to_string(),
            data_dir: "/var/lib/aimx".to_string(),
            config_path: "/etc/aimx/config.toml".to_string(),
            dkim_selector: "aimx".to_string(),
            dkim_key_present: true,
            smtp_running: true,
            stale_send_sock_present: false,
            default_trust: "none".to_string(),
            default_trusted_senders: vec![],
            mailboxes: vec![],
            dns: None,
        };
        let output = format_status(&info);
        assert!(
            output.contains("Trusted senders:  (none)"),
            "Trusted senders line must render '(none)' when list is empty: {output}"
        );
    }

    #[test]
    fn format_status_never_renders_recent_activity() {
        let info = StatusInfo {
            domain: "test.com".to_string(),
            data_dir: "/var/lib/aimx".to_string(),
            config_path: "/etc/aimx/config.toml".to_string(),
            dkim_selector: "aimx".to_string(),
            dkim_key_present: true,
            smtp_running: true,
            stale_send_sock_present: false,
            default_trust: "none".to_string(),
            default_trusted_senders: vec![],
            mailboxes: vec![MailboxStatus {
                name: "catchall".to_string(),
                address: "*@test.com".to_string(),
                total: 10,
                unread: 3,
                trust: "none".to_string(),
                trusted_senders_count: 0,
                hook_count: 0,
            }],
            dns: None,
        };
        let output = format_status(&info);
        assert!(
            !output.contains("Recent activity"),
            "'Recent activity' section must never be rendered: {output}"
        );
    }

    #[test]
    fn format_status_per_mailbox_section_includes_trust_and_hook_counts() {
        let info = StatusInfo {
            domain: "test.com".to_string(),
            data_dir: "/var/lib/aimx".to_string(),
            config_path: "/etc/aimx/config.toml".to_string(),
            dkim_selector: "aimx".to_string(),
            dkim_key_present: true,
            smtp_running: true,
            stale_send_sock_present: false,
            default_trust: "none".to_string(),
            default_trusted_senders: vec![],
            mailboxes: vec![MailboxStatus {
                name: "ops".to_string(),
                address: "ops@test.com".to_string(),
                total: 4,
                unread: 1,
                trust: "verified".to_string(),
                trusted_senders_count: 3,
                hook_count: 2,
            }],
            dns: None,
        };
        let output = format_status(&info);
        // Find the row for the `ops` mailbox and assert Trust / Senders /
        // Hooks cells are populated. The row is a single `|`-delimited line
        // containing the mailbox name, address, and the three numeric / text
        // columns under test.
        let plain = regex_like_strip(&output);
        let row = plain
            .lines()
            .find(|l| l.contains("| ops ") || l.contains("| ops  "))
            .unwrap_or_else(|| panic!("expected a row for the ops mailbox, got:\n{plain}"));
        let cells: Vec<&str> = row.split('|').map(|s| s.trim()).collect();
        // Expected cells: ["", "ops", "ops@test.com", "4", "1", "verified", "3", "2", ""]
        assert!(
            cells.contains(&"verified"),
            "Trust cell must render the bare trust string: row = {row:?}"
        );
        assert!(
            cells.contains(&"3"),
            "Senders cell must render the trusted_senders count (3): row = {row:?}"
        );
        assert!(
            cells.contains(&"2"),
            "Hooks cell must render the hook_count (2): row = {row:?}"
        );
    }

    #[test]
    fn gather_status_propagates_per_mailbox_trust_and_hook_counts() {
        let tmp = tempfile::TempDir::new().unwrap();
        let _cfg_guard = crate::config::test_env::ConfigDirOverride::set(tmp.path());

        let mut mailboxes = std::collections::HashMap::new();
        mailboxes.insert(
            "catchall".to_string(),
            crate::config::MailboxConfig {
                address: "*@test.com".to_string(),
                hooks: vec![],
                trust: None,
                trusted_senders: None,
            },
        );
        mailboxes.insert(
            "ops".to_string(),
            crate::config::MailboxConfig {
                address: "ops@test.com".to_string(),
                hooks: vec![crate::hook::Hook {
                    name: Some("docthook".to_string()),
                    event: crate::hook::HookEvent::OnReceive,
                    r#type: "cmd".to_string(),
                    cmd: "true".to_string(),
                    dangerously_support_untrusted: false,
                    origin: crate::hook::HookOrigin::Operator,
                    template: None,
                    params: std::collections::BTreeMap::new(),
                    run_as: None,
                }],
                trust: Some("verified".to_string()),
                trusted_senders: Some(vec!["alice@example.com".to_string()]),
            },
        );

        let config = Config {
            domain: "test.com".to_string(),
            data_dir: tmp.path().to_path_buf(),
            dkim_selector: "aimx".to_string(),
            trust: "none".to_string(),
            trusted_senders: vec![],
            hook_templates: Vec::new(),
            mailboxes,
            verify_host: None,
            enable_ipv6: false,
        };

        let info = gather_status_with_ops(
            &config,
            &FakeServiceOps::new(false),
            &MockNetworkOps::default(),
        );

        // The per-mailbox override must show through to the status row,
        // overriding the top-level "none" default.
        let ops = info
            .mailboxes
            .iter()
            .find(|m| m.name == "ops")
            .expect("ops mailbox must be in the snapshot");
        assert_eq!(ops.trust, "verified");
        assert_eq!(ops.trusted_senders_count, 1);
        assert_eq!(ops.hook_count, 1);

        // Catchall inherits the top-level default → "none" with zero senders.
        let catchall = info
            .mailboxes
            .iter()
            .find(|m| m.name == "catchall")
            .expect("catchall mailbox must be in the snapshot");
        assert_eq!(catchall.trust, "none");
        assert_eq!(catchall.trusted_senders_count, 0);
        assert_eq!(catchall.hook_count, 0);

        // Top-level snapshot fields surface too.
        assert_eq!(info.default_trust, "none");
        assert!(info.default_trusted_senders.is_empty());
        assert!(
            info.config_path.ends_with("config.toml"),
            "config_path should resolve to a config.toml path under the override: {}",
            info.config_path
        );
    }
}
