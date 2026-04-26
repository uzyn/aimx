use crate::config::{
    Config, LoadWarning, MailboxConfig, RESERVED_RUN_AS_CATCHALL, RESERVED_RUN_AS_ROOT,
};
use crate::frontmatter::InboundFrontmatter;
use crate::setup::{
    self, DnsRecord, DnsVerifyResult, NetworkOps, RealNetworkOps, RealSystemOps, SystemOps,
};
use crate::term;
use crate::user_resolver::resolve_user;
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

/// Severity of a [`DoctorFinding`]. `Pass` findings are emitted by
/// checks that want to show a green PASS line (e.g. "mailbox dirs
/// chowned correctly"); most checks simply return no finding on the
/// happy path. `Info` is reserved for advisory notes (legacy
/// `aimx-hook` user present). `Warn` and `Fail` drive the summary
/// counts printed at the end of the Checks section; `Fail` additionally
/// makes `aimx doctor` exit non-zero.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FindingSeverity {
    /// Reserved for checks that want to emit a positive PASS line in
    /// the rendered Checks section. Current checks stay silent on
    /// success; retained so future checks (e.g. a
    /// "mailbox storage chowned correctly" PASS banner) can opt in
    /// without widening the enum.
    #[allow(dead_code)]
    Pass,
    Info,
    Warn,
    Fail,
}

impl FindingSeverity {
    fn badge(self) -> colored::ColoredString {
        match self {
            FindingSeverity::Pass => term::success_mark(),
            // Branding §5.4 does not define an info mark; the literal "INFO"
            // text is a deliberate pragmatic fallback until the spec is
            // extended (or a Unicode mark is chosen).
            FindingSeverity::Info => term::info("INFO"),
            FindingSeverity::Warn => term::warn_mark(),
            FindingSeverity::Fail => term::fail_mark(),
        }
    }
}

/// A single Checks-section line. `check` is a stable short ID used by
/// `aimx hooks prune --orphans` to decide whether the config has
/// non-orphan failures worth blocking the prune on. `message` is the
/// human-readable one-liner; `fix` is an optional remediation hint
/// rendered on the following line under `term::dim`.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct DoctorFinding {
    pub check: &'static str,
    pub severity: FindingSeverity,
    pub message: String,
    pub fix: Option<String>,
}

impl DoctorFinding {
    fn new(check: &'static str, severity: FindingSeverity, message: impl Into<String>) -> Self {
        Self {
            check,
            severity,
            message: message.into(),
            fix: None,
        }
    }

    fn with_fix(mut self, fix: impl Into<String>) -> Self {
        self.fix = Some(fix.into());
        self
    }
}

/// Run every doctor check against `config`, merging in
/// `load_warnings` returned by [`Config::load`]. The returned vector is
/// in deterministic section order (mailbox ownership → catchall presence →
/// load warnings).
pub fn run_checks(config: &Config, load_warnings: &[LoadWarning]) -> Vec<DoctorFinding> {
    let mut out = Vec::new();
    out.extend(check_mailbox_ownership(config));
    out.extend(check_catchall_user(config));
    out.extend(translate_load_warnings(load_warnings));
    out
}

/// Validate that every mailbox owner resolves and its storage
/// directories exist + are chowned `owner:owner` mode `0700`.
pub fn check_mailbox_ownership(config: &Config) -> Vec<DoctorFinding> {
    let mut out = Vec::new();

    // (a) `getpwnam(owner)` succeeds for every mailbox. Orphans get a
    // Warn finding (PRD §6.1 keeps the daemon up on user deletion).
    // (b) `inbox/<name>/` and `sent/<name>/` exist, are chowned
    //     `owner:owner`, and have mode `0700`.
    let mut configured_names: std::collections::HashSet<String> = std::collections::HashSet::new();
    for (name, mb) in &config.mailboxes {
        configured_names.insert(name.clone());

        let resolved = resolve_owner(mb);
        let Some((uid, gid)) = resolved else {
            out.push(
                DoctorFinding::new(
                    "MAILBOX-OWNER-ORPHAN",
                    FindingSeverity::Warn,
                    format!(
                        "mailbox '{name}' owner '{owner}' does not resolve via getpwnam",
                        owner = mb.owner,
                    ),
                )
                .with_fix(
                    "create the user or run `sudo aimx hooks prune --orphans` \
                     to remove the residue from config.toml",
                ),
            );
            continue;
        };

        for (label, dir) in [
            ("inbox", config.inbox_dir(name)),
            ("sent", config.sent_dir(name)),
        ] {
            match dir_ownership(&dir) {
                DirOwnership::Missing => out.push(
                    DoctorFinding::new(
                        "MAILBOX-DIR-MISSING",
                        FindingSeverity::Fail,
                        format!(
                            "mailbox '{name}' {label} directory is missing: {}",
                            dir.display()
                        ),
                    )
                    .with_fix(
                        "re-run `sudo aimx setup` to re-create mailbox \
                         storage directories",
                    ),
                ),
                DirOwnership::NotADir => out.push(DoctorFinding::new(
                    "MAILBOX-DIR-NOT-DIR",
                    FindingSeverity::Fail,
                    format!(
                        "mailbox '{name}' {label} path is not a directory: {}",
                        dir.display()
                    ),
                )),
                DirOwnership::Ok {
                    owner_uid,
                    owner_gid,
                    mode,
                } => {
                    if owner_uid != uid {
                        out.push(
                            DoctorFinding::new(
                                "MAILBOX-DIR-OWNER-DRIFT",
                                FindingSeverity::Fail,
                                format!(
                                    "mailbox '{name}' {label} dir is owned by uid {owner_uid} but \
                                     config owner '{}' resolves to uid {uid}",
                                    mb.owner
                                ),
                            )
                            .with_fix(format!(
                                "chown the directory: `sudo chown -R {owner}:{owner} {}`",
                                dir.display(),
                                owner = mb.owner,
                            )),
                        );
                    } else if owner_gid != gid {
                        out.push(
                            DoctorFinding::new(
                                "MAILBOX-DIR-GROUP-DRIFT",
                                FindingSeverity::Warn,
                                format!(
                                    "mailbox '{name}' {label} dir group gid {owner_gid} does not \
                                     match owner '{}' primary gid {gid}",
                                    mb.owner
                                ),
                            )
                            .with_fix(format!(
                                "chgrp the directory: `sudo chgrp -R {} {}`",
                                mb.owner,
                                dir.display(),
                            )),
                        );
                    }
                    if mode & 0o777 != 0o700 {
                        out.push(
                            DoctorFinding::new(
                                "MAILBOX-DIR-MODE-DRIFT",
                                FindingSeverity::Warn,
                                format!(
                                    "mailbox '{name}' {label} dir mode is {:#o}, expected 0700",
                                    mode & 0o777,
                                ),
                            )
                            .with_fix(format!(
                                "tighten permissions: `sudo chmod 0700 {}`",
                                dir.display(),
                            )),
                        );
                    }
                }
            }
        }
    }

    // Orphan-storage findings: directories under `inbox/` / `sent/`
    // with no matching config entry (left behind by a removed mailbox).
    for root in ["inbox", "sent"] {
        let root_dir = config.data_dir.join(root);
        let Ok(entries) = std::fs::read_dir(&root_dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(meta) = entry.metadata() else { continue };
            if !meta.is_dir() {
                continue;
            }
            let name = match entry.file_name().into_string() {
                Ok(n) => n,
                Err(_) => continue,
            };
            if configured_names.contains(&name) {
                continue;
            }
            out.push(
                DoctorFinding::new(
                    "ORPHAN-STORAGE",
                    FindingSeverity::Warn,
                    format!(
                        "storage directory `{root}/{name}/` has no matching \
                         mailbox in config.toml"
                    ),
                )
                .with_fix(format!(
                    "remove the stale directory or add a matching `[mailboxes.{name}]` \
                     block to config.toml",
                )),
            );
        }
    }

    // ORPHAN-CONFIG: config mailboxes with no storage dirs at all
    // (both `inbox/<name>/` and `sent/<name>/` missing). This is distinct
    // from the per-dir MAILBOX-DIR-MISSING finding above: we only flag
    // the mailbox as a whole ORPHAN-CONFIG when *both* dirs are absent,
    // which typically indicates the operator added the mailbox stanza
    // by hand without running `aimx mailboxes create`.
    for name in config.mailboxes.keys() {
        let inbox = config.inbox_dir(name);
        let sent = config.sent_dir(name);
        if !inbox.exists() && !sent.exists() {
            out.push(
                DoctorFinding::new(
                    "ORPHAN-CONFIG",
                    FindingSeverity::Warn,
                    format!(
                        "mailbox '{name}' has a config entry but no storage directories \
                         (inbox/sent both missing)"
                    ),
                )
                .with_fix(format!(
                    "run `sudo aimx mailboxes create {name}` to provision storage, \
                     or remove the stanza from config.toml",
                )),
            );
        }
    }

    out
}

/// When a catchall mailbox is owned by the reserved
/// `aimx-catchall` user (the default from `aimx setup`), verify that
/// user actually resolves. Without it, catchall inbound ingest cannot
/// chown mail into place. If the operator has deliberately assigned a
/// different owner to the catchall, ingest chowns as that owner
/// instead, so the reserved user is not required and the check is a
/// no-op.
pub fn check_catchall_user(config: &Config) -> Vec<DoctorFinding> {
    let needs_reserved_user = config
        .mailboxes
        .values()
        .any(|mb| mb.is_catchall(config) && mb.owner == RESERVED_RUN_AS_CATCHALL);
    if !needs_reserved_user {
        return Vec::new();
    }
    if resolve_user(RESERVED_RUN_AS_CATCHALL).is_some() {
        return Vec::new();
    }
    vec![
        DoctorFinding::new(
            "CATCHALL-USER-MISSING",
            FindingSeverity::Fail,
            format!(
                "catchall mailbox is configured with owner \
                 '{RESERVED_RUN_AS_CATCHALL}' but that system user does \
                 not resolve",
            ),
        )
        .with_fix("re-run `sudo aimx setup` to create the catchall service user"),
    ]
}

/// Surface `LoadWarning`s from `Config::load` as doctor
/// findings so warnings the daemon logged on start-up are visible
/// without scraping the journal.
pub fn translate_load_warnings(warnings: &[LoadWarning]) -> Vec<DoctorFinding> {
    let mut out = Vec::new();
    for w in warnings {
        match w {
            LoadWarning::OrphanMailboxOwner { mailbox, owner } => {
                out.push(
                    DoctorFinding::new(
                        "ORPHAN-MAILBOX-OWNER",
                        FindingSeverity::Warn,
                        format!("config load: mailbox '{mailbox}' owner '{owner}' is orphan"),
                    )
                    .with_fix("create the user or run `sudo aimx hooks prune --orphans`"),
                );
            }
            LoadWarning::LegacyAimxHookUser => {
                // Translated separately in `check_legacy_aimx_hook_user`
                // so the doctor output is consistent regardless of
                // whether `load` surfaced the warning yet.
            }
            LoadWarning::RootCatchallAccepted { mailbox } => {
                out.push(DoctorFinding::new(
                    "ROOT-CATCHALL-ACCEPTED",
                    FindingSeverity::Info,
                    format!(
                        "catchall '{mailbox}' is running with owner='root' + \
                         allow_root_catchall=true (escape hatch)"
                    ),
                ));
            }
        }
    }
    out
}

fn resolve_owner(mb: &MailboxConfig) -> Option<(u32, u32)> {
    if mb.owner == RESERVED_RUN_AS_ROOT {
        return Some((0, 0));
    }
    resolve_user(&mb.owner).map(|u| (u.uid, u.gid))
}

/// Result of inspecting a mailbox storage directory for ownership +
/// permission drift. `Missing` is a `Fail` upstream; `NotADir` signals
/// someone stomped a file into the expected location.
enum DirOwnership {
    Missing,
    NotADir,
    Ok {
        owner_uid: u32,
        owner_gid: u32,
        mode: u32,
    },
}

fn dir_ownership(path: &Path) -> DirOwnership {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let meta = match std::fs::metadata(path) {
            Ok(m) => m,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return DirOwnership::Missing,
            Err(_) => return DirOwnership::Missing,
        };
        if !meta.is_dir() {
            return DirOwnership::NotADir;
        }
        DirOwnership::Ok {
            owner_uid: meta.uid(),
            owner_gid: meta.gid(),
            mode: meta.permissions().mode(),
        }
    }
    #[cfg(not(unix))]
    {
        if !path.exists() {
            DirOwnership::Missing
        } else if !path.is_dir() {
            DirOwnership::NotADir
        } else {
            DirOwnership::Ok {
                owner_uid: 0,
                owner_gid: 0,
                mode: 0o700,
            }
        }
    }
}

/// Format the Checks section of `aimx doctor`. Groups findings by
/// severity (Fail, Warn, Info, Pass) and prints a summary footer.
pub fn format_checks(findings: &[DoctorFinding]) -> String {
    let mut out = String::new();
    out.push_str(&format!("{}\n", term::header("Checks")));
    if findings.is_empty() {
        out.push_str(&format!("  {}\n", term::success("All checks passed."),));
        return out;
    }

    for f in findings {
        out.push_str(&format!("  [{}] {}\n", f.severity.badge(), f.message));
        if let Some(fix) = &f.fix {
            out.push_str(&format!("        {} {}\n", term::dim("→"), term::dim(fix)));
        }
    }

    let fails = findings
        .iter()
        .filter(|f| f.severity == FindingSeverity::Fail)
        .count();
    let warns = findings
        .iter()
        .filter(|f| f.severity == FindingSeverity::Warn)
        .count();
    let infos = findings
        .iter()
        .filter(|f| f.severity == FindingSeverity::Info)
        .count();
    out.push_str(&format!(
        "\n  Summary: {} fail, {} warn, {} info\n",
        fails, warns, infos,
    ));
    out
}

pub fn run(config: Config) -> Result<(), Box<dyn std::error::Error>> {
    // Re-run `Config::load` so doctor sees the same warnings the daemon
    // would on startup (orphan mailbox owners, orphan run_as users,
    // etc.). If the reload errors — e.g. the config was removed
    // between the main-dispatch load and this call — fall back to an
    // empty warning list so we still print the rest of the report.
    let load_warnings = match crate::config::Config::load_resolved() {
        Ok((_, w)) => w,
        Err(_) => Vec::new(),
    };

    let info = gather_status(&config);
    print!("{}", format_status(&info));

    let findings = run_checks(&config, &load_warnings);
    println!();
    print!("{}", format_checks(&findings));

    let any_fail = findings.iter().any(|f| f.severity == FindingSeverity::Fail);
    if any_fail {
        // `main.rs` converts any `Err` from this function into a
        // non-zero exit. The error message is deliberately short; the
        // Checks section above already listed each failure.
        return Err("aimx doctor found failing checks (see Checks section above)".into());
    }
    Ok(())
}
