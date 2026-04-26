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
    /// Hook templates section. Always present even when empty so
    /// operators can confirm "no templates enabled" rather than wonder if
    /// doctor failed to gather the data.
    pub hook_templates: HookTemplatesSection,
}

/// Snapshot of the hook-templates feature for `aimx doctor`.
///
/// Surfaces three operator concerns from PRD §7.3 + §10:
/// 1. Which templates are enabled, and is each one's `cmd[0]` actually
///    executable on this box (catches the binary-path-drift risk).
/// 2. How often each template fired in the last 24 hours, and how many
///    of those fires were failures (non-zero exit or timeout).
/// 3. Whether the `aimx-hook` service user exists with the expected
///    UID/GID and group-read access to the mailbox dirs.
pub struct HookTemplatesSection {
    pub templates: Vec<HookTemplateStatus>,
    pub hook_user: HookUserStatus,
    /// `Some(_)` when fire counts were derived from the structured log
    /// stream; `None` when neither journalctl nor the OpenRC log-file
    /// fallback returned data. Counts in the `templates` slice are zero
    /// when the source is `None`; the rendered output prints `-` instead
    /// of `0` so an operator can tell "no fires in 24h" apart from
    /// "logs unavailable, count unknown".
    pub log_source: Option<&'static str>,
}

impl Default for HookTemplatesSection {
    fn default() -> Self {
        Self {
            templates: Vec::new(),
            hook_user: HookUserStatus {
                user: "aimx-hook".to_string(),
                user_exists: false,
                uid_gid: None,
                datadir_readable: false,
            },
            log_source: None,
        }
    }
}

pub struct HookTemplateStatus {
    pub name: String,
    pub description: String,
    /// Path of `cmd[0]` from the template definition. Surfaced verbatim
    /// so the operator can copy-paste it into a `which` / `ls` to debug.
    pub cmd_path: String,
    /// True iff `cmd_path` resolves to an existing, executable file (or,
    /// for the systemd-run path, exists at all — the executor will set
    /// up its own exec env). False when missing or not marked +x.
    pub cmd_path_executable: bool,
    /// 24h fire count parsed from the structured `aimx::hook` log line
    /// (`template={name}` field). `None` when no log source was readable.
    pub fire_count_24h: Option<u32>,
    /// 24h failure count: fires whose log line carries `exit_code != 0`
    /// or `timed_out=true`. `None` when no log source was readable.
    pub failure_count_24h: Option<u32>,
}

pub struct HookUserStatus {
    pub user: String,
    /// True when `id <user>` resolves on the host. False on a fresh box
    /// where `aimx setup` was never run.
    pub user_exists: bool,
    /// `Some((uid, gid))` when resolvable. `None` when the user does not
    /// exist or the lookup failed (e.g. running on a non-Unix host —
    /// caught at compile time elsewhere but defended here too).
    pub uid_gid: Option<(u32, u32)>,
    /// True iff the `aimx-hook` user (or the current user, when running
    /// as the hook user already) can read both `<datadir>/inbox` and
    /// `<datadir>/sent`. False when the chown step from `aimx setup`
    /// hasn't been applied; surfaces as a WARN with a remediation hint.
    pub datadir_readable: bool,
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
    let hook_templates = gather_hook_templates_section(config, sys);

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
        hook_templates,
    }
}

/// Build the `Hook templates` section of `aimx doctor`. Reads:
///
/// 1. `Config::hook_templates` for the enabled set + `cmd[0]` / description.
/// 2. The host's `aimx-hook` user via the injected `SystemOps`.
/// 3. The last 24h of structured log lines via `tail_service_logs`. The
///    parser tolerates an empty / errored log source — counts come back as
///    `None` and the renderer prints `-` instead of `0`.
///
/// All three lookups are best-effort and never fail the overall doctor
/// report; an unhealthy box should still get a snapshot.
fn gather_hook_templates_section<S: SystemOps>(config: &Config, sys: &S) -> HookTemplatesSection {
    // Legacy doctor note: surface any lingering `aimx-hook` service
    // user. The hook-template feature was removed; only the legacy
    // user check survives until the user-cleanup work lands.
    const LEGACY_HOOK_USER: &str = "aimx-hook";

    let user_exists = sys.user_exists(LEGACY_HOOK_USER);
    let uid_gid = if user_exists {
        sys.lookup_user_uid_gid(LEGACY_HOOK_USER)
    } else {
        None
    };
    let datadir_readable = if user_exists {
        is_datadir_readable_by(uid_gid, &config.data_dir)
    } else {
        false
    };

    HookTemplatesSection {
        templates: Vec::new(),
        hook_user: HookUserStatus {
            user: LEGACY_HOOK_USER.to_string(),
            user_exists,
            uid_gid,
            datadir_readable,
        },
        log_source: None,
    }
}

/// Parse `(fire_count, failure_count)` per template from the structured
/// `aimx::hook` log lines emitted by `run_and_log` in `src/hook.rs`. The
/// parser is intentionally lenient: any line without a `template=<name>`
/// token is skipped, malformed lines do not panic, and lines whose
/// template doesn't appear in `Config::hook_templates` simply land in
/// the returned map (the caller filters by current template names).
///
/// Fire count is "this template's name appeared in a hook-fire line".
/// Failure count is the subset where `exit_code=` is non-zero OR
/// `timed_out=true` is present. `template=-` lines (raw-cmd hooks)
/// are skipped — they don't bind to a template entry.
#[allow(dead_code)]
fn parse_hook_fire_counts(text: &str) -> std::collections::HashMap<String, (u32, u32)> {
    let mut out = std::collections::HashMap::new();
    for line in text.lines() {
        // Cheap pre-filter: skip lines that obviously aren't hook-fire
        // records. We require both `template=` and `exit_code=` in the
        // payload — both are present on every `run_and_log` info line.
        if !line.contains("template=") || !line.contains("exit_code=") {
            continue;
        }
        let template = match extract_kv(line, "template=") {
            Some("-") | None => continue,
            Some(v) => v.to_string(),
        };
        let exit_code: i32 = extract_kv(line, "exit_code=")
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        let timed_out = matches!(extract_kv(line, "timed_out="), Some("true"));
        let entry = out.entry(template).or_insert((0u32, 0u32));
        entry.0 = entry.0.saturating_add(1);
        if exit_code != 0 || timed_out {
            entry.1 = entry.1.saturating_add(1);
        }
    }
    out
}

/// Extract the value of a `key=...` token from a structured log line.
/// Stops at the first ASCII whitespace or end-of-string. Returns `None`
/// when the key is absent. The structured logger never quotes scalar
/// values (only `stderr_tail`), so a bare whitespace split is safe.
#[allow(dead_code)]
fn extract_kv<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let start = line.find(key)? + key.len();
    let rest = &line[start..];
    let end = rest
        .find(|c: char| c.is_ascii_whitespace())
        .unwrap_or(rest.len());
    Some(&rest[..end])
}

/// True when `path` exists and is marked executable for at least one
/// permission class. Best-effort: a file the daemon can't `stat` (e.g.
/// because the path doesn't exist) returns false. We don't try to
/// simulate the `aimx-hook` user's view here — the goal is just to
/// catch the obvious "binary not on box" failure mode flagged in
/// PRD §10 risks.
#[allow(dead_code)]
fn is_executable(path: &Path) -> bool {
    if path.as_os_str().is_empty() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let Ok(meta) = std::fs::metadata(path) else {
            return false;
        };
        if !meta.is_file() {
            return false;
        }
        meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        path.exists()
    }
}

/// Best-effort check that the `aimx-hook` user can read the inbox + sent
/// directories under `data_dir`. We can't `seteuid` from a non-root
/// doctor invocation, so this is conservative:
///
/// * If the directories don't exist (fresh box), return `false`.
/// * If the directories exist and the file mode grants other-read
///   (`o+r`), return `true` regardless of group ownership.
/// * Otherwise check that the directory's group matches the hook
///   user's gid AND the mode grants group-read.
///
/// A `false` result surfaces as a WARN with a remediation hint
/// pointing at `aimx setup`'s chown step.
fn is_datadir_readable_by(uid_gid: Option<(u32, u32)>, data_dir: &Path) -> bool {
    let inbox = data_dir.join("inbox");
    let sent = data_dir.join("sent");
    [&inbox, &sent]
        .into_iter()
        .all(|p| dir_readable_by(uid_gid, p))
}

fn dir_readable_by(uid_gid: Option<(u32, u32)>, path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let Ok(meta) = std::fs::metadata(path) else {
            return false;
        };
        if !meta.is_dir() {
            return false;
        }
        let mode = meta.permissions().mode();
        // World-read + execute → anyone can read.
        if mode & 0o005 == 0o005 {
            return true;
        }
        let Some((uid, gid)) = uid_gid else {
            return false;
        };
        if meta.uid() == uid && mode & 0o500 == 0o500 {
            return true;
        }
        if meta.gid() == gid && mode & 0o050 == 0o050 {
            return true;
        }
        false
    }
    #[cfg(not(unix))]
    {
        let _ = uid_gid;
        path.is_dir()
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

    out.push_str(&format!("\n{}\n", term::header("Hook templates")));
    out.push_str(&format_hook_templates_section(&info.hook_templates));

    out.push_str(&format!("\n{}\n", term::header("Logs")));
    out.push_str(&format!(
        "  {}\n",
        term::dim("Run `aimx logs` to view recent logs, or `aimx logs --follow` to tail."),
    ));

    out
}

/// Render the `Hook templates` section. Layout mirrors the existing
/// `Mailboxes` table: a one-line summary then an ASCII-framed grid.
/// When no templates are enabled the table is suppressed and a single
/// hint line points the operator at `aimx setup`.
fn format_hook_templates_section(section: &HookTemplatesSection) -> String {
    let mut out = String::new();

    // Service-user line is always rendered so operators can see whether
    // the chown step from `aimx setup` ran on this box.
    let user_line = if section.hook_user.user_exists {
        let uid_gid = match section.hook_user.uid_gid {
            Some((uid, gid)) => format!("uid={uid} gid={gid}"),
            None => "uid=? gid=?".to_string(),
        };
        format!(
            "  {} {} ({})\n",
            term::success("aimx-hook user:"),
            term::highlight(&section.hook_user.user),
            uid_gid,
        )
    } else {
        format!(
            "  {} {} - run `sudo aimx setup` to create the service user\n",
            term::warn("aimx-hook user:"),
            term::dim("missing"),
        )
    };
    out.push_str(&user_line);

    if section.hook_user.user_exists {
        let datadir_line = if section.hook_user.datadir_readable {
            format!(
                "  {} inbox/ + sent/ readable by group {}\n",
                term::success("Datadir access:"),
                term::highlight(&section.hook_user.user),
            )
        } else {
            format!(
                "  {} inbox/ + sent/ not group-readable - re-run `sudo aimx setup` to chown\n",
                term::warn("Datadir access:"),
            )
        };
        out.push_str(&datadir_line);
    }

    if section.templates.is_empty() {
        out.push_str(&format!(
            "  {}\n",
            term::dim("No hook templates enabled. Run `aimx agents setup` to install one."),
        ));
        return out;
    }

    out.push('\n');
    out.push_str(&render_hook_templates_table(section));

    if section.log_source.is_none() {
        out.push_str(&format!(
            "  {}\n",
            term::dim(
                "Fire counts unavailable: no log source readable (journalctl missing on OpenRC, or service not yet started)."
            ),
        ));
    }

    out
}

fn render_hook_templates_table(section: &HookTemplatesSection) -> String {
    const HEADERS: [&str; 5] = [
        "Template",
        "Description",
        "cmd[0]",
        "Fires 24h",
        "Fails 24h",
    ];
    const RIGHT: [bool; 5] = [false, false, false, true, true];

    fn truncate(s: &str, max: usize) -> String {
        if s.chars().count() <= max {
            return s.to_string();
        }
        let head: String = s.chars().take(max.saturating_sub(1)).collect();
        format!("{head}…")
    }

    fn fmt_count(c: Option<u32>) -> String {
        match c {
            Some(n) => n.to_string(),
            None => "-".to_string(),
        }
    }

    let rows: Vec<[String; 5]> = section
        .templates
        .iter()
        .map(|t| {
            let cmd_cell = if t.cmd_path_executable {
                t.cmd_path.clone()
            } else {
                format!("{} (MISSING)", t.cmd_path)
            };
            [
                t.name.clone(),
                truncate(&t.description, 40),
                truncate(&cmd_cell, 40),
                fmt_count(t.fire_count_24h),
                fmt_count(t.failure_count_24h),
            ]
        })
        .collect();

    let mut widths = [0usize; 5];
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
                    out.push_str(&term::highlight(cell).to_string());
                } else if i == 2 && cell.contains("(MISSING)") {
                    out.push_str(&term::warn(cell).to_string());
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

/// Stable check IDs used to discriminate orphan-cleanup failures from
/// genuine configuration errors in `aimx hooks prune --orphans`. The
/// prune command refuses to write when any `Fail`-severity finding's
/// `check` is not in this set.
#[allow(dead_code)]
pub const ORPHAN_CHECK_IDS: &[&str] = &[
    "ORPHAN-STORAGE",
    "ORPHAN-CONFIG",
    "ORPHAN-TEMPLATE-RUN_AS",
    "ORPHAN-HOOK-RUN_AS",
];

/// Run every doctor check against `config`, merging in
/// `load_warnings` returned by [`Config::load`]. The returned vector is
/// in deterministic section order (mailbox ownership → templates →
/// hook invariants → catchall presence → load warnings → legacy user).
pub fn run_checks(config: &Config, load_warnings: &[LoadWarning]) -> Vec<DoctorFinding> {
    run_checks_with_runner(config, load_warnings, &RealAccessRunner)
}

fn run_checks_with_runner(
    config: &Config,
    load_warnings: &[LoadWarning],
    runner: &dyn AccessRunner,
) -> Vec<DoctorFinding> {
    let mut out = Vec::new();
    out.extend(check_mailbox_ownership(config));
    out.extend(check_templates_with_runner(config, runner));
    out.extend(check_hook_invariants(config));
    out.extend(check_catchall_user(config));
    out.extend(translate_load_warnings(load_warnings));
    out.extend(check_legacy_aimx_hook_user());
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

/// Validate every `[[hook_template]]`: `run_as` resolves, `cmd[0]`
/// exists + is executable, and the `run_as` user can `access(X_OK)` it.
///
/// Production callers go through [`run_checks`] / [`run_checks_with_runner`],
/// which inject the [`AccessRunner`] seam. The private
/// [`check_templates_with_runner`] below is the implementation; tests
/// call it directly with a mock runner.
fn check_templates_with_runner(_config: &Config, _runner: &dyn AccessRunner) -> Vec<DoctorFinding> {
    // Hook templates were removed; nothing to inspect today.
    Vec::new()
}

/// Re-run the hook/owner invariant against every hook in `config`.
/// With the legacy `run_as` schema gone, hooks always inherit the
/// mailbox owner, so the invariant is structurally true and there is
/// nothing to check today. The wrapper is kept so external callers
/// keep compiling until the doctor rewrite lands.
pub fn check_hook_invariants(_config: &Config) -> Vec<DoctorFinding> {
    Vec::new()
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

/// Emit an `info` note when the legacy `aimx-hook` system
/// user is still present. aimx no longer creates this user; doctor just
/// reminds the operator it can be removed.
pub fn check_legacy_aimx_hook_user() -> Vec<DoctorFinding> {
    if resolve_user("aimx-hook").is_some() {
        vec![
            DoctorFinding::new(
                "LEGACY-AIMX-HOOK-USER",
                FindingSeverity::Info,
                "legacy 'aimx-hook' system user present; aimx no longer manages it",
            )
            .with_fix("remove via `sudo userdel aimx-hook` when safe"),
        ]
    } else {
        Vec::new()
    }
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

/// Outcome of `access(X_OK)` evaluated as a target user.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
enum AccessResult {
    Allowed,
    Denied,
    /// The runner could not evaluate the check (e.g. `runuser` not on
    /// PATH and not running as root). Surfaced as `Info` in doctor
    /// output so the operator knows the check was skipped.
    Unknown,
}

/// Injectable seam for the `access(X_OK)`-as-user probe. Production
/// uses [`RealAccessRunner`] (runuser first, then fork+seteuid when
/// root). Tests implement this trait to bypass the subprocess spawn.
#[allow(dead_code)]
trait AccessRunner {
    fn access_x_ok_as(&self, user: &str, path: &str) -> AccessResult;
}

struct RealAccessRunner;

impl AccessRunner for RealAccessRunner {
    fn access_x_ok_as(&self, user: &str, path: &str) -> AccessResult {
        // First try `runuser -u <user> -- test -x <path>`. `runuser`
        // requires root but sidesteps PAM, making it safer than `su` in
        // non-interactive contexts. When `runuser` is absent we fall
        // back to a fork + `seteuid` + `access(path, X_OK)` probe —
        // only viable as root, since `seteuid` to another user requires
        // CAP_SETUID. Non-root doctor runs that fail both paths return
        // `Unknown`; the doctor renderer surfaces that as an `INFO`
        // finding so operators know the check was inconclusive.
        match run_runuser(user, path) {
            Some(r) => r,
            None => run_fork_seteuid(user, path),
        }
    }
}

#[allow(dead_code)]
fn run_runuser(user: &str, path: &str) -> Option<AccessResult> {
    let output = std::process::Command::new("runuser")
        .args(["-u", user, "--", "test", "-x", path])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .output();
    match output {
        Ok(out) => {
            if out.status.success() {
                Some(AccessResult::Allowed)
            } else {
                match out.status.code() {
                    // `test -x` returns 1 on "not executable". Higher
                    // codes mean runuser itself failed (user unknown,
                    // PAM error, missing shell, etc.) — log the detail
                    // at debug level so operators can diagnose PAM /
                    // permission issues without being conflated with
                    // "binary not executable", then fall through to the
                    // fork+seteuid path which returns Unknown.
                    Some(1) => Some(AccessResult::Denied),
                    Some(code) => {
                        tracing::debug!(
                            run_as = user,
                            path = path,
                            exit_code = code,
                            stderr = %String::from_utf8_lossy(&out.stderr).trim(),
                            "runuser returned non-test exit; falling back to fork+seteuid probe"
                        );
                        None
                    }
                    None => {
                        tracing::debug!(
                            run_as = user,
                            path = path,
                            stderr = %String::from_utf8_lossy(&out.stderr).trim(),
                            "runuser terminated by signal; falling back to fork+seteuid probe"
                        );
                        None
                    }
                }
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => {
            tracing::debug!(
                run_as = user,
                path = path,
                error = %e,
                "runuser spawn failed; falling back to fork+seteuid probe"
            );
            None
        }
    }
}

#[allow(dead_code)]
fn run_fork_seteuid(user: &str, path: &str) -> AccessResult {
    #[cfg(unix)]
    {
        // `seteuid` on Linux requires CAP_SETUID. A non-root doctor
        // invocation cannot drop euid to another user, so bail out
        // with `Unknown` — the renderer translates that into an INFO
        // finding explaining the check was skipped.
        // SAFETY: `geteuid` is async-signal-safe.
        let effective_uid = unsafe { libc::geteuid() };
        if effective_uid != 0 {
            return AccessResult::Unknown;
        }
        let Some(resolved) = resolve_user(user) else {
            return AccessResult::Unknown;
        };
        let path_c = match std::ffi::CString::new(path) {
            Ok(c) => c,
            Err(_) => return AccessResult::Unknown,
        };
        // SAFETY: fork() is defined on POSIX; we only touch
        // async-signal-safe functions in the child. We do not call
        // any Rust destructors between fork and _exit.
        let pid = unsafe { libc::fork() };
        if pid < 0 {
            return AccessResult::Unknown;
        }
        if pid == 0 {
            // Child: drop to the target user and probe access(X_OK).
            // Any failure reports code 2 so the parent can map to
            // Unknown; code 1 is Denied, code 0 is Allowed.
            let setgid_rc = unsafe { libc::setegid(resolved.gid) };
            if setgid_rc != 0 {
                unsafe { libc::_exit(2) };
            }
            let setuid_rc = unsafe { libc::seteuid(resolved.uid) };
            if setuid_rc != 0 {
                unsafe { libc::_exit(2) };
            }
            let rc = unsafe { libc::access(path_c.as_ptr(), libc::X_OK) };
            if rc == 0 {
                unsafe { libc::_exit(0) };
            } else {
                unsafe { libc::_exit(1) };
            }
        }
        // Parent: waitpid and interpret the exit status.
        let mut status: libc::c_int = 0;
        let rc = unsafe { libc::waitpid(pid, &mut status, 0) };
        if rc < 0 {
            return AccessResult::Unknown;
        }
        if libc::WIFEXITED(status) {
            match libc::WEXITSTATUS(status) {
                0 => AccessResult::Allowed,
                1 => AccessResult::Denied,
                _ => AccessResult::Unknown,
            }
        } else {
            AccessResult::Unknown
        }
    }
    #[cfg(not(unix))]
    {
        let _ = (user, path);
        AccessResult::Unknown
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
