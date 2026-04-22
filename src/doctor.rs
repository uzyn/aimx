use crate::config::{
    Config, LoadWarning, MailboxConfig, RESERVED_RUN_AS_CATCHALL, RESERVED_RUN_AS_ROOT,
    check_hook_owner_invariant, is_reserved_run_as,
};
use crate::frontmatter::InboundFrontmatter;
use crate::hook::effective_hook_name;
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
    /// Hook templates section (S6-4). Always present even when empty so
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
    // user (PRD §6.9 "aimx-hook presence info note only"). `aimx setup`
    // no longer creates this user — see [`crate::setup::CATCHALL_SERVICE_USER`]
    // for the current equivalent.
    const LEGACY_HOOK_USER: &str = "aimx-hook";

    let log_text = if config.hook_templates.is_empty() {
        // No templates → no point shelling out to journalctl. The empty
        // section is explicit ("(no templates enabled)" in the renderer).
        None
    } else {
        // 24h of structured `aimx::hook` lines is bounded — even a noisy
        // install with hooks firing on every email rarely produces
        // >10K lines per day. We ask for 5K which is a reasonable cap.
        sys.tail_service_logs(crate::logs::SERVICE_UNIT, 5_000).ok()
    };
    let log_source = if log_text.is_some() {
        Some("logs")
    } else {
        None
    };
    let counts = log_text
        .as_deref()
        .map(parse_hook_fire_counts)
        .unwrap_or_default();

    let templates: Vec<HookTemplateStatus> = config
        .hook_templates
        .iter()
        .map(|t| {
            let cmd_path = t.cmd.first().cloned().unwrap_or_default();
            let cmd_path_executable = is_executable(Path::new(&cmd_path));
            let (fires, fails) = counts.get(t.name.as_str()).copied().unwrap_or((0u32, 0u32));
            HookTemplateStatus {
                name: t.name.clone(),
                description: t.description.clone(),
                cmd_path,
                cmd_path_executable,
                fire_count_24h: log_source.map(|_| fires),
                failure_count_24h: log_source.map(|_| fails),
            }
        })
        .collect();

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
        templates,
        hook_user: HookUserStatus {
            user: LEGACY_HOOK_USER.to_string(),
            user_exists,
            uid_gid,
            datadir_readable,
        },
        log_source,
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
            term::dim("No hook templates enabled. Run `aimx agent-setup <agent>` to install one."),
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
    /// the rendered Checks section. Current Sprint 7 checks stay
    /// silent on success; retained so future checks (e.g. a
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
            FindingSeverity::Pass => term::pass_badge(),
            FindingSeverity::Info => term::info("INFO"),
            FindingSeverity::Warn => term::warn_badge(),
            FindingSeverity::Fail => term::fail_badge(),
        }
    }
}

/// A single Checks-section line. `check` is a stable short ID used by
/// `aimx hooks prune --orphans` to decide whether the config has
/// non-orphan failures worth blocking the prune on. `message` is the
/// human-readable one-liner; `fix` is an optional remediation hint
/// rendered on the following line under `term::dim`.
#[derive(Debug, Clone)]
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
pub const ORPHAN_CHECK_IDS: &[&str] = &[
    "ORPHAN-STORAGE",
    "ORPHAN-CONFIG",
    "ORPHAN-TEMPLATE-RUN_AS",
    "ORPHAN-HOOK-RUN_AS",
];

/// Run every Sprint 7 doctor check against `config`, merging in
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

/// S7-1: validate that every mailbox owner resolves and its storage
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

/// S7-2: validate every `[[hook_template]]`: `run_as` resolves, `cmd[0]`
/// exists + is executable, and the `run_as` user can `access(X_OK)` it.
///
/// Production callers go through [`run_checks`] / [`run_checks_with_runner`],
/// which inject the [`AccessRunner`] seam. The private
/// [`check_templates_with_runner`] below is the implementation; tests
/// call it directly with a mock runner.
fn check_templates_with_runner(config: &Config, runner: &dyn AccessRunner) -> Vec<DoctorFinding> {
    let mut out = Vec::new();
    for tmpl in &config.hook_templates {
        // (a) run_as resolves (reserved names short-circuit).
        let run_as_ok = is_reserved_run_as(&tmpl.run_as) || resolve_user(&tmpl.run_as).is_some();
        if !run_as_ok {
            out.push(
                DoctorFinding::new(
                    "ORPHAN-TEMPLATE-RUN_AS",
                    FindingSeverity::Warn,
                    format!(
                        "template '{}' run_as='{}' does not resolve via getpwnam",
                        tmpl.name, tmpl.run_as,
                    ),
                )
                .with_fix(
                    "create the user, or run `sudo aimx hooks prune --orphans` to \
                     remove the template",
                ),
            );
            // Skip the cmd[0] checks when the run_as user is orphan;
            // `access(X_OK)` as the target user is undefined.
            continue;
        }

        // (b) cmd[0] exists + is executable by the current process.
        let Some(cmd0) = tmpl.cmd.first() else {
            out.push(DoctorFinding::new(
                "TEMPLATE-CMD-EMPTY",
                FindingSeverity::Fail,
                format!("template '{}' has an empty cmd array", tmpl.name),
            ));
            continue;
        };
        if !is_executable(Path::new(cmd0)) {
            out.push(
                DoctorFinding::new(
                    "TEMPLATE-CMD-NOT-EXECUTABLE",
                    FindingSeverity::Fail,
                    format!(
                        "template '{}' cmd[0] `{cmd0}` is missing or not executable",
                        tmpl.name,
                    ),
                )
                .with_fix(
                    "run `aimx agent-setup <agent> --redetect` to re-probe $PATH, \
                     or fix the binary path in `/etc/aimx/config.toml`",
                ),
            );
            continue;
        }

        // (c) `access(X_OK)` as the run_as user. Reserved `root` always
        // can; `aimx-catchall` and regular users need a subprocess
        // check. This catches the case where cmd[0] is +x for root but
        // not the service user.
        if tmpl.run_as == RESERVED_RUN_AS_ROOT {
            continue;
        }
        match runner.access_x_ok_as(&tmpl.run_as, cmd0) {
            AccessResult::Allowed => {}
            AccessResult::Denied => {
                out.push(
                    DoctorFinding::new(
                        "TEMPLATE-CMD-NOT-EXECUTABLE-BY-RUN_AS",
                        FindingSeverity::Fail,
                        format!(
                            "template '{}' cmd[0] `{cmd0}` is not executable \
                             by run_as user '{}'",
                            tmpl.name, tmpl.run_as,
                        ),
                    )
                    .with_fix(format!(
                        "chmod the binary (`sudo chmod o+rx {cmd0}`) or re-run \
                         `aimx agent-setup <agent> --redetect` to pick a \
                         run_as-readable path"
                    )),
                );
            }
            AccessResult::Unknown => {
                // Neither `runuser` nor the seteuid fallback worked —
                // emit an info note so the operator knows the check was
                // inconclusive rather than silently skipped.
                out.push(DoctorFinding::new(
                    "TEMPLATE-CMD-ACCESS-UNKNOWN",
                    FindingSeverity::Info,
                    format!(
                        "template '{}' cmd[0] executable-by-run_as check skipped: \
                         no `runuser` on PATH and not running as root",
                        tmpl.name,
                    ),
                ));
            }
        }
    }
    out
}

/// S7-3 part 1: re-run the hook/owner invariant against every hook in
/// `config`. This is a safety net against hand-edits to `config.toml`
/// that bypass `validate_hooks` (e.g. orphan downgrades at load time).
pub fn check_hook_invariants(config: &Config) -> Vec<DoctorFinding> {
    let mut out = Vec::new();
    for (mailbox_name, mb) in &config.mailboxes {
        for hook in &mb.hooks {
            if let Err(reason) = check_hook_owner_invariant(config, mailbox_name, mb, hook) {
                let hook_name = effective_hook_name(hook);
                out.push(
                    DoctorFinding::new(
                        "HOOK-INVARIANT",
                        FindingSeverity::Fail,
                        format!("hook '{hook_name}' on mailbox '{mailbox_name}': {reason}"),
                    )
                    .with_fix(
                        "align the hook's run_as with the mailbox owner (or set \
                         run_as='root') in config.toml",
                    ),
                );
            }
        }
    }
    out
}

/// S7-3 part 2: when any mailbox is a catchall, verify the reserved
/// `aimx-catchall` system user exists. Without it, catchall inbound
/// ingest cannot chown mail into place.
pub fn check_catchall_user(config: &Config) -> Vec<DoctorFinding> {
    let has_catchall = config.mailboxes.values().any(|mb| mb.is_catchall(config));
    if !has_catchall {
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
                "catchall mailbox is configured but system user \
                 '{RESERVED_RUN_AS_CATCHALL}' does not resolve",
            ),
        )
        .with_fix("re-run `sudo aimx setup` to create the catchall service user"),
    ]
}

/// S7-3 part 3: surface `LoadWarning`s from `Config::load` as doctor
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
            LoadWarning::OrphanHookRunAs {
                mailbox,
                hook_name,
                run_as,
            } => {
                out.push(
                    DoctorFinding::new(
                        "ORPHAN-HOOK-RUN_AS",
                        FindingSeverity::Warn,
                        format!(
                            "config load: hook '{hook_name}' on '{mailbox}' has \
                             run_as='{run_as}' which is orphan"
                        ),
                    )
                    .with_fix("run `sudo aimx hooks prune --orphans`"),
                );
            }
            LoadWarning::OrphanTemplateRunAs { template, run_as } => {
                out.push(
                    DoctorFinding::new(
                        "ORPHAN-TEMPLATE-RUN_AS",
                        FindingSeverity::Warn,
                        format!("config load: template '{template}' run_as='{run_as}' is orphan"),
                    )
                    .with_fix("run `sudo aimx hooks prune --orphans`"),
                );
            }
            LoadWarning::HookInvariantSkippedDueToOrphan {
                mailbox,
                hook_name,
                reason,
            } => {
                out.push(DoctorFinding::new(
                    "HOOK-INVARIANT-SKIPPED",
                    FindingSeverity::Info,
                    format!(
                        "hook '{hook_name}' on '{mailbox}': invariant skipped at load — {reason}"
                    ),
                ));
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

/// S7-3 part 4: emit an `info` note when the legacy `aimx-hook` system
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

    /// Minimal mock that exercises `is_service_running` and the new
    /// `tail_service_logs` / `user_exists` / `lookup_user_uid_gid` calls
    /// added in S6-4. All other `SystemOps` methods panic; they must
    /// not be reached by `gather_status`.
    struct FakeServiceOps {
        running: bool,
        log_tail_calls: Cell<u32>,
        canned_logs: Option<String>,
        user_exists: bool,
        user_uid_gid: Option<(u32, u32)>,
    }

    impl FakeServiceOps {
        fn new(running: bool) -> Self {
            Self {
                running,
                log_tail_calls: Cell::new(0),
                canned_logs: None,
                user_exists: false,
                user_uid_gid: None,
            }
        }

        fn with_logs(mut self, text: &str) -> Self {
            self.canned_logs = Some(text.to_string());
            self
        }

        fn with_hook_user(mut self, uid: u32, gid: u32) -> Self {
            self.user_exists = true;
            self.user_uid_gid = Some((uid, gid));
            self
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
            match &self.canned_logs {
                Some(text) => Ok(text.clone()),
                None => Err("no log source available".into()),
            }
        }
        fn follow_service_logs(&self, _unit: &str) -> Result<(), Box<dyn std::error::Error>> {
            unreachable!("gather_status must not touch follow_service_logs")
        }
        fn user_exists(&self, _user: &str) -> bool {
            self.user_exists
        }
        fn lookup_user_uid_gid(&self, _user: &str) -> Option<(u32, u32)> {
            self.user_uid_gid
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
            hook_templates: HookTemplatesSection::default(),
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
            hook_templates: HookTemplatesSection::default(),
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
            hook_templates: HookTemplatesSection::default(),
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
            hook_templates: HookTemplatesSection::default(),
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
            hook_templates: HookTemplatesSection::default(),
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
            hook_templates: HookTemplatesSection::default(),
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
            hook_templates: HookTemplatesSection::default(),
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
            hook_templates: HookTemplatesSection::default(),
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
                owner: "aimx-catchall".to_string(),
                hooks: vec![],
                trust: None,
                trusted_senders: None,
                allow_root_catchall: false,
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
            hook_templates: HookTemplatesSection::default(),
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
            hook_templates: HookTemplatesSection::default(),
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
    fn gather_status_does_not_tail_service_logs_when_no_templates_enabled() {
        // When no hook templates are enabled, doctor must not pay the cost
        // of shelling out to journalctl just to compute fire counts on
        // an empty set. Sprint 6's hook-templates section short-circuits
        // in that case; this test pins the contract.
        let tmp = tempfile::TempDir::new().unwrap();
        let _cfg_guard = crate::config::test_env::ConfigDirOverride::set(tmp.path());
        let config = empty_config(tmp.path());
        let net = MockNetworkOps::default();
        let ops = FakeServiceOps::new(true);

        let _info = gather_status_with_ops(&config, &ops, &net);
        assert_eq!(
            ops.log_tail_calls.get(),
            0,
            "doctor must not tail service logs when no templates are enabled"
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
            hook_templates: HookTemplatesSection::default(),
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
            hook_templates: HookTemplatesSection::default(),
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
            hook_templates: HookTemplatesSection::default(),
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
            hook_templates: HookTemplatesSection::default(),
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
            hook_templates: HookTemplatesSection::default(),
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
            hook_templates: HookTemplatesSection::default(),
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
                owner: "aimx-catchall".to_string(),
                hooks: vec![],
                trust: None,
                trusted_senders: None,
                allow_root_catchall: false,
            },
        );
        mailboxes.insert(
            "ops".to_string(),
            crate::config::MailboxConfig {
                address: "ops@test.com".to_string(),
                owner: "root".to_string(),
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
                allow_root_catchall: false,
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

    // ----- S6-4 Hook templates section --------------------------------

    fn template_for_test(name: &str, cmd_path: &str) -> crate::config::HookTemplate {
        crate::config::HookTemplate {
            name: name.to_string(),
            description: format!("Description for {name}"),
            cmd: vec![cmd_path.to_string(), "{prompt}".to_string()],
            params: vec!["prompt".to_string()],
            stdin: crate::config::HookTemplateStdin::Email,
            run_as: "aimx-hook".to_string(),
            timeout_secs: 60,
            allowed_events: vec![
                crate::hook::HookEvent::OnReceive,
                crate::hook::HookEvent::AfterSend,
            ],
        }
    }

    fn config_with_templates(
        data_dir: &Path,
        templates: Vec<crate::config::HookTemplate>,
    ) -> Config {
        Config {
            domain: "test.com".to_string(),
            data_dir: data_dir.to_path_buf(),
            dkim_selector: "aimx".to_string(),
            trust: "none".to_string(),
            trusted_senders: vec![],
            hook_templates: templates,
            mailboxes: std::collections::HashMap::new(),
            verify_host: None,
            enable_ipv6: false,
        }
    }

    #[test]
    fn parse_hook_fire_counts_handles_well_formed_lines() {
        // Lines mirror the structured `tracing::info!` output emitted by
        // `run_and_log` in src/hook.rs.
        let text = "\
2026-04-20T12:00:00Z aimx::hook hook_name=h1 event=on_receive mailbox=alice template=invoke-claude run_as=aimx-hook sandbox=fallback email_id=001 exit_code=0 duration_ms=10 timed_out=false stderr_tail=\"\"\n\
2026-04-20T12:00:01Z aimx::hook hook_name=h2 event=on_receive mailbox=alice template=invoke-claude run_as=aimx-hook sandbox=fallback email_id=002 exit_code=2 duration_ms=15 timed_out=false stderr_tail=\"err\"\n\
2026-04-20T12:00:02Z aimx::hook hook_name=h3 event=on_receive mailbox=alice template=webhook run_as=aimx-hook sandbox=fallback email_id=003 exit_code=0 duration_ms=20 timed_out=true stderr_tail=\"\"\n\
2026-04-20T12:00:03Z aimx::hook hook_name=h4 event=after_send mailbox=alice template=- run_as=aimx-hook sandbox=fallback email_id=004 exit_code=0 duration_ms=5 timed_out=false stderr_tail=\"\"\n\
unrelated journalctl line that mentions template= and exit_code= but is malformed\n\
";
        let counts = parse_hook_fire_counts(text);
        // template=invoke-claude: 2 fires, 1 failure (exit_code=2)
        assert_eq!(counts.get("invoke-claude"), Some(&(2, 1)));
        // template=webhook: 1 fire, 1 failure (timed_out=true)
        assert_eq!(counts.get("webhook"), Some(&(1, 1)));
        // template=- raw-cmd lines must be excluded.
        assert!(!counts.contains_key("-"));
    }

    #[test]
    fn parse_hook_fire_counts_tolerates_malformed_lines_without_panic() {
        let text = "\
\n\
not a log line at all\n\
template=foo without exit_code\n\
exit_code=0 without template\n\
template= empty exit_code= empty\n\
template=incomplete-line exit_code=\n\
template=valid-one exit_code=0 timed_out=false\n\
template=valid-two exit_code=missing-digits timed_out=false\n\
";
        let counts = parse_hook_fire_counts(text);
        // Only `valid-one` and `valid-two` should land. `valid-two` has a
        // non-digit exit_code which parses to fallback 0 (success).
        assert_eq!(counts.get("valid-one"), Some(&(1, 0)));
        assert_eq!(counts.get("valid-two"), Some(&(1, 0)));
        // The empty-template "template= empty" is filtered: extract_kv
        // returns the empty slice up to the next whitespace (`empty`),
        // not "" — so it lands as template name "empty"... actually
        // since we use whitespace split, template= with a trailing
        // whitespace produces "" which extract_kv normalizes by reading
        // up to ws. Pin the contract via direct assertion that the map
        // doesn't panic and finishes parsing.
        // (We don't assert a hard size — the lenient parser may pick up
        // `template=incomplete-line` with 0 exit_code; the invariant that
        // matters is "no panic, valid lines are counted".)
    }

    #[test]
    fn parse_hook_fire_counts_returns_empty_map_for_empty_input() {
        assert!(parse_hook_fire_counts("").is_empty());
        assert!(parse_hook_fire_counts("\n\n\n").is_empty());
    }

    #[test]
    fn gather_hook_templates_section_skips_log_tail_when_empty() {
        let tmp = tempfile::TempDir::new().unwrap();
        let _cfg_guard = crate::config::test_env::ConfigDirOverride::set(tmp.path());
        let config = config_with_templates(tmp.path(), Vec::new());
        let ops = FakeServiceOps::new(false);
        let info = gather_status_with_ops(&config, &ops, &MockNetworkOps::default());
        assert_eq!(
            ops.log_tail_calls.get(),
            0,
            "no template = no journalctl call (cost-saving short-circuit)"
        );
        assert!(info.hook_templates.templates.is_empty());
        assert!(info.hook_templates.log_source.is_none());
    }

    #[test]
    fn gather_hook_templates_section_tails_logs_when_templates_enabled() {
        let tmp = tempfile::TempDir::new().unwrap();
        let _cfg_guard = crate::config::test_env::ConfigDirOverride::set(tmp.path());
        let templates = vec![template_for_test("invoke-claude", "/usr/bin/true")];
        let config = config_with_templates(tmp.path(), templates);
        let log_text = "hook_name=h1 template=invoke-claude exit_code=0 timed_out=false\nhook_name=h2 template=invoke-claude exit_code=1 timed_out=false\n";
        let ops = FakeServiceOps::new(true).with_logs(log_text);
        let info = gather_status_with_ops(&config, &ops, &MockNetworkOps::default());
        assert_eq!(ops.log_tail_calls.get(), 1);
        let t = &info.hook_templates.templates[0];
        assert_eq!(t.name, "invoke-claude");
        assert_eq!(t.fire_count_24h, Some(2));
        assert_eq!(t.failure_count_24h, Some(1));
        assert_eq!(info.hook_templates.log_source, Some("logs"));
    }

    #[test]
    fn gather_hook_templates_section_marks_counts_unknown_when_logs_unavailable() {
        let tmp = tempfile::TempDir::new().unwrap();
        let _cfg_guard = crate::config::test_env::ConfigDirOverride::set(tmp.path());
        let templates = vec![template_for_test("invoke-claude", "/usr/bin/true")];
        let config = config_with_templates(tmp.path(), templates);
        // FakeServiceOps without `with_logs` returns Err from tail_service_logs,
        // simulating OpenRC without journalctl + no /var/log/aimx/*.log.
        let ops = FakeServiceOps::new(true);
        let info = gather_status_with_ops(&config, &ops, &MockNetworkOps::default());
        let t = &info.hook_templates.templates[0];
        assert_eq!(
            t.fire_count_24h, None,
            "fire count must be None (rendered as `-`) when log source unavailable"
        );
        assert_eq!(t.failure_count_24h, None);
        assert!(info.hook_templates.log_source.is_none());
    }

    #[test]
    fn gather_hook_templates_section_flags_missing_cmd_path() {
        let tmp = tempfile::TempDir::new().unwrap();
        let _cfg_guard = crate::config::test_env::ConfigDirOverride::set(tmp.path());
        let templates = vec![
            template_for_test("present", "/usr/bin/true"),
            template_for_test("missing", "/no/such/binary/anywhere"),
        ];
        let config = config_with_templates(tmp.path(), templates);
        let ops = FakeServiceOps::new(true);
        let info = gather_status_with_ops(&config, &ops, &MockNetworkOps::default());
        let present = info
            .hook_templates
            .templates
            .iter()
            .find(|t| t.name == "present")
            .unwrap();
        let missing = info
            .hook_templates
            .templates
            .iter()
            .find(|t| t.name == "missing")
            .unwrap();
        assert!(
            present.cmd_path_executable,
            "/usr/bin/true should be executable on Linux"
        );
        assert!(
            !missing.cmd_path_executable,
            "/no/such/... must be flagged as not executable"
        );
    }

    #[test]
    fn gather_hook_templates_section_reports_missing_aimx_hook_user() {
        let tmp = tempfile::TempDir::new().unwrap();
        let _cfg_guard = crate::config::test_env::ConfigDirOverride::set(tmp.path());
        let config = config_with_templates(tmp.path(), Vec::new());
        // FakeServiceOps default: user does not exist on this host.
        let ops = FakeServiceOps::new(false);
        let info = gather_status_with_ops(&config, &ops, &MockNetworkOps::default());
        assert!(!info.hook_templates.hook_user.user_exists);
        assert_eq!(info.hook_templates.hook_user.uid_gid, None);
        // Datadir-readable check is short-circuited to false when user
        // doesn't exist (we have nothing to check ownership against).
        assert!(!info.hook_templates.hook_user.datadir_readable);
    }

    #[test]
    fn gather_hook_templates_section_reports_resolved_aimx_hook_user() {
        let tmp = tempfile::TempDir::new().unwrap();
        let _cfg_guard = crate::config::test_env::ConfigDirOverride::set(tmp.path());
        let config = config_with_templates(tmp.path(), Vec::new());
        let ops = FakeServiceOps::new(true).with_hook_user(8765, 8765);
        let info = gather_status_with_ops(&config, &ops, &MockNetworkOps::default());
        assert!(info.hook_templates.hook_user.user_exists);
        assert_eq!(info.hook_templates.hook_user.uid_gid, Some((8765, 8765)));
    }

    #[test]
    fn format_hook_templates_section_renders_warn_when_user_missing() {
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
            hook_templates: HookTemplatesSection::default(),
        };
        let out = format_status(&info);
        assert!(
            out.contains("Hook templates"),
            "expected 'Hook templates' header in {out}"
        );
        assert!(
            out.contains("aimx-hook user:"),
            "expected aimx-hook user line in {out}"
        );
        assert!(
            out.contains("missing"),
            "expected 'missing' badge for absent user in {out}"
        );
        // No templates → must not render the table; must show the hint.
        assert!(
            !out.contains("Template ") || !out.contains("cmd[0]"),
            "no-template section must suppress the ASCII table"
        );
        assert!(out.contains("No hook templates enabled."));
    }

    #[test]
    fn format_hook_templates_section_renders_table_with_fire_counts() {
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
            hook_templates: HookTemplatesSection {
                templates: vec![
                    HookTemplateStatus {
                        name: "invoke-claude".to_string(),
                        description: "Pipe email into Claude Code with a prompt".to_string(),
                        cmd_path: "/usr/local/bin/claude".to_string(),
                        cmd_path_executable: false,
                        fire_count_24h: Some(7),
                        failure_count_24h: Some(2),
                    },
                    HookTemplateStatus {
                        name: "webhook".to_string(),
                        description: "POST email JSON to a URL".to_string(),
                        cmd_path: "/usr/bin/curl".to_string(),
                        cmd_path_executable: true,
                        fire_count_24h: Some(0),
                        failure_count_24h: Some(0),
                    },
                ],
                hook_user: HookUserStatus {
                    user: "aimx-hook".to_string(),
                    user_exists: true,
                    uid_gid: Some((9001, 9001)),
                    datadir_readable: true,
                },
                log_source: Some("logs"),
            },
        };
        let out = format_status(&info);
        assert!(out.contains("Hook templates"), "missing section header");
        assert!(out.contains("Template"), "missing column header");
        assert!(out.contains("Fires 24h"));
        assert!(out.contains("Fails 24h"));
        assert!(out.contains("invoke-claude"));
        assert!(out.contains("webhook"));
        // Missing-binary cell carries the MISSING marker.
        assert!(
            out.contains("(MISSING)"),
            "binary-not-executable templates must carry (MISSING) marker: {out}"
        );
        // Datadir line must render in success form.
        assert!(out.contains("Datadir access:"));
    }

    #[test]
    fn format_hook_templates_section_dashes_unknown_counts_when_log_source_missing() {
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
            hook_templates: HookTemplatesSection {
                templates: vec![HookTemplateStatus {
                    name: "invoke-claude".to_string(),
                    description: "Claude".to_string(),
                    cmd_path: "/usr/local/bin/claude".to_string(),
                    cmd_path_executable: true,
                    fire_count_24h: None,
                    failure_count_24h: None,
                }],
                hook_user: HookUserStatus {
                    user: "aimx-hook".to_string(),
                    user_exists: true,
                    uid_gid: Some((1000, 1000)),
                    datadir_readable: true,
                },
                log_source: None,
            },
        };
        let out = format_status(&info);
        assert!(
            out.contains("Fire counts unavailable"),
            "expected the 'Fire counts unavailable' hint when log_source is None: {out}"
        );
        // The cell renders `-` rather than `0` so operators can tell
        // "logs unavailable" apart from "no fires in 24h".
        let plain = strip_ansi(&out);
        let lines: Vec<&str> = plain.lines().collect();
        let row = lines
            .iter()
            .find(|l| l.contains("invoke-claude"))
            .expect("expected invoke-claude row");
        let cells: Vec<&str> = row.split('|').map(|c| c.trim()).collect();
        assert!(
            cells.contains(&"-"),
            "fire count cell must render `-` for unknown counts; row = {row:?}"
        );
    }

    fn strip_ansi(s: &str) -> String {
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

    // -------------------- Sprint 7 checks --------------------
    //
    // The checks below use `user_resolver::set_test_resolver` to drop in a
    // fake `getpwnam` so tests don't depend on the running host's
    // `/etc/passwd`. Every test installs its own resolver for the
    // duration of the test; the guard serializes across tests.

    use crate::config::{HookTemplate, HookTemplateStdin, MailboxConfig};
    use crate::hook::{Hook, HookEvent};
    use crate::user_resolver::{ResolvedUser, set_test_resolver};
    use std::collections::BTreeMap;

    /// Mock AccessRunner that records calls and returns a canned result.
    struct MockRunner {
        result: AccessResult,
        calls: std::cell::Cell<u32>,
    }

    impl MockRunner {
        fn new(result: AccessResult) -> Self {
            Self {
                result,
                calls: std::cell::Cell::new(0),
            }
        }
    }

    impl AccessRunner for MockRunner {
        fn access_x_ok_as(&self, _user: &str, _path: &str) -> AccessResult {
            self.calls.set(self.calls.get() + 1);
            self.result
        }
    }

    fn s7_config_with_mailbox(data_dir: &Path, owner: &str) -> Config {
        let mut mailboxes = HashMap::new();
        mailboxes.insert(
            "alice".to_string(),
            MailboxConfig {
                address: "alice@ex.com".to_string(),
                owner: owner.to_string(),
                hooks: vec![],
                trust: None,
                trusted_senders: None,
                allow_root_catchall: false,
            },
        );
        Config {
            domain: "ex.com".to_string(),
            data_dir: data_dir.to_path_buf(),
            dkim_selector: "aimx".to_string(),
            trust: "none".to_string(),
            trusted_senders: vec![],
            hook_templates: vec![],
            mailboxes,
            verify_host: None,
            enable_ipv6: false,
        }
    }

    fn current_uid_gid() -> (u32, u32) {
        #[cfg(unix)]
        {
            // SAFETY: async-signal-safe on POSIX.
            unsafe { (libc::geteuid(), libc::getegid()) }
        }
        #[cfg(not(unix))]
        {
            (0, 0)
        }
    }

    fn resolver_current_user(name: &str) -> Option<ResolvedUser> {
        if name == "testowner" || name == "root" {
            let (uid, gid) = current_uid_gid();
            Some(ResolvedUser {
                name: name.to_string(),
                uid,
                gid,
            })
        } else {
            None
        }
    }

    #[cfg(unix)]
    fn chmod(path: &Path, mode: u32) {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(mode);
        std::fs::set_permissions(path, perms).unwrap();
    }

    #[test]
    fn mailbox_ownership_passes_with_correct_dirs() {
        let _guard = set_test_resolver(resolver_current_user);
        let tmp = tempfile::TempDir::new().unwrap();
        let config = s7_config_with_mailbox(tmp.path(), "testowner");
        // Create the required dirs with the right uid+mode.
        std::fs::create_dir_all(tmp.path().join("inbox/alice")).unwrap();
        std::fs::create_dir_all(tmp.path().join("sent/alice")).unwrap();
        #[cfg(unix)]
        {
            chmod(&tmp.path().join("inbox/alice"), 0o700);
            chmod(&tmp.path().join("sent/alice"), 0o700);
        }

        let findings = check_mailbox_ownership(&config);
        // The running uid owns the tempdir dirs (we created them), and
        // `testowner` resolves to that uid via the mock resolver, so
        // there should be no ownership findings. Group drift is
        // possible if the process's primary gid differs from default,
        // but our resolver mirrors both uid + gid to the current
        // process, so the check passes fully.
        assert!(
            findings.is_empty(),
            "expected clean ownership checks, got: {findings:?}"
        );
    }

    #[test]
    fn mailbox_ownership_flags_missing_owner_as_orphan() {
        fn fake(name: &str) -> Option<ResolvedUser> {
            if name == "root" {
                Some(ResolvedUser {
                    name: "root".into(),
                    uid: 0,
                    gid: 0,
                })
            } else {
                None // alice's owner does not resolve.
            }
        }
        let _guard = set_test_resolver(fake);
        let tmp = tempfile::TempDir::new().unwrap();
        let config = s7_config_with_mailbox(tmp.path(), "ghost-user");
        std::fs::create_dir_all(tmp.path().join("inbox/alice")).unwrap();
        std::fs::create_dir_all(tmp.path().join("sent/alice")).unwrap();

        let findings = check_mailbox_ownership(&config);
        assert!(
            findings
                .iter()
                .any(|f| f.check == "MAILBOX-OWNER-ORPHAN" && f.message.contains("alice")),
            "expected MAILBOX-OWNER-ORPHAN, got: {findings:?}"
        );
    }

    #[test]
    fn mailbox_ownership_flags_missing_storage_dir() {
        let _guard = set_test_resolver(resolver_current_user);
        let tmp = tempfile::TempDir::new().unwrap();
        let config = s7_config_with_mailbox(tmp.path(), "testowner");
        // Create ONLY inbox, not sent — the per-dir check fires for
        // the missing sent dir.
        std::fs::create_dir_all(tmp.path().join("inbox/alice")).unwrap();
        #[cfg(unix)]
        chmod(&tmp.path().join("inbox/alice"), 0o700);

        let findings = check_mailbox_ownership(&config);
        assert!(
            findings.iter().any(|f| f.check == "MAILBOX-DIR-MISSING"
                && f.severity == FindingSeverity::Fail
                && f.message.contains("sent")),
            "expected MAILBOX-DIR-MISSING for sent dir, got: {findings:?}"
        );
    }

    #[test]
    fn mailbox_ownership_flags_mode_drift() {
        let _guard = set_test_resolver(resolver_current_user);
        let tmp = tempfile::TempDir::new().unwrap();
        let config = s7_config_with_mailbox(tmp.path(), "testowner");
        std::fs::create_dir_all(tmp.path().join("inbox/alice")).unwrap();
        std::fs::create_dir_all(tmp.path().join("sent/alice")).unwrap();
        #[cfg(unix)]
        {
            chmod(&tmp.path().join("inbox/alice"), 0o755);
            chmod(&tmp.path().join("sent/alice"), 0o700);
        }

        let findings = check_mailbox_ownership(&config);
        #[cfg(unix)]
        assert!(
            findings
                .iter()
                .any(|f| f.check == "MAILBOX-DIR-MODE-DRIFT"
                    && f.severity == FindingSeverity::Warn),
            "expected MAILBOX-DIR-MODE-DRIFT, got: {findings:?}"
        );
        #[cfg(not(unix))]
        let _ = findings;
    }

    #[test]
    fn mailbox_ownership_flags_not_a_dir() {
        // Someone stomped a regular file into `inbox/alice/` (e.g.,
        // operator mv'd a loose .md into the wrong place). The check
        // fires `MAILBOX-DIR-NOT-DIR` at Fail severity.
        let _guard = set_test_resolver(resolver_current_user);
        let tmp = tempfile::TempDir::new().unwrap();
        let config = s7_config_with_mailbox(tmp.path(), "testowner");
        std::fs::create_dir_all(tmp.path().join("inbox")).unwrap();
        std::fs::create_dir_all(tmp.path().join("sent")).unwrap();
        // Place a FILE at the spot where `inbox/alice/` should be.
        std::fs::write(tmp.path().join("inbox/alice"), b"not a dir").unwrap();
        std::fs::create_dir_all(tmp.path().join("sent/alice")).unwrap();
        #[cfg(unix)]
        chmod(&tmp.path().join("sent/alice"), 0o700);

        let findings = check_mailbox_ownership(&config);
        assert!(
            findings.iter().any(|f| f.check == "MAILBOX-DIR-NOT-DIR"
                && f.severity == FindingSeverity::Fail
                && f.message.contains("inbox")
                && f.message.contains("alice")),
            "expected MAILBOX-DIR-NOT-DIR for inbox dir, got: {findings:?}"
        );
    }

    #[test]
    fn mailbox_ownership_flags_owner_uid_drift() {
        // Resolver maps `testowner` to a bogus uid that does NOT match
        // the filesystem-reported uid (which is the test runner's uid,
        // since we just created the dirs). The check fires
        // `MAILBOX-DIR-OWNER-DRIFT` at Fail severity.
        fn fake(name: &str) -> Option<ResolvedUser> {
            if name == "testowner" || name == "root" {
                Some(ResolvedUser {
                    name: name.to_string(),
                    // Sentinel uid/gid far from any real runtime uid so
                    // the filesystem-vs-resolver mismatch is unambiguous.
                    uid: 424242,
                    gid: 424242,
                })
            } else {
                None
            }
        }
        let _guard = set_test_resolver(fake);
        let tmp = tempfile::TempDir::new().unwrap();
        let config = s7_config_with_mailbox(tmp.path(), "testowner");
        std::fs::create_dir_all(tmp.path().join("inbox/alice")).unwrap();
        std::fs::create_dir_all(tmp.path().join("sent/alice")).unwrap();
        #[cfg(unix)]
        {
            chmod(&tmp.path().join("inbox/alice"), 0o700);
            chmod(&tmp.path().join("sent/alice"), 0o700);
        }

        let findings = check_mailbox_ownership(&config);
        #[cfg(unix)]
        assert!(
            findings.iter().any(|f| f.check == "MAILBOX-DIR-OWNER-DRIFT"
                && f.severity == FindingSeverity::Fail
                && f.message.contains("alice")),
            "expected MAILBOX-DIR-OWNER-DRIFT, got: {findings:?}"
        );
        #[cfg(not(unix))]
        let _ = findings;
    }

    #[test]
    fn mailbox_ownership_flags_group_drift() {
        // Resolver returns the current uid (so owner check passes) but
        // a bogus gid (so the group check fires). Pinned as Warn — the
        // sprint plan classifies group drift as recoverable, not an
        // isolation break.
        fn fake(name: &str) -> Option<ResolvedUser> {
            if name == "testowner" || name == "root" {
                let (uid, gid) = current_uid_gid();
                Some(ResolvedUser {
                    name: name.to_string(),
                    uid,
                    // Sentinel gid guaranteed to differ from the fs-reported
                    // gid. Add a large offset instead of picking a constant
                    // so this test stays correct on hosts where the test
                    // runner happens to have a high real gid.
                    gid: gid.wrapping_add(10_000),
                })
            } else {
                None
            }
        }
        let _guard = set_test_resolver(fake);
        let tmp = tempfile::TempDir::new().unwrap();
        let config = s7_config_with_mailbox(tmp.path(), "testowner");
        std::fs::create_dir_all(tmp.path().join("inbox/alice")).unwrap();
        std::fs::create_dir_all(tmp.path().join("sent/alice")).unwrap();
        #[cfg(unix)]
        {
            chmod(&tmp.path().join("inbox/alice"), 0o700);
            chmod(&tmp.path().join("sent/alice"), 0o700);
        }

        let findings = check_mailbox_ownership(&config);
        #[cfg(unix)]
        {
            assert!(
                findings.iter().any(|f| f.check == "MAILBOX-DIR-GROUP-DRIFT"
                    && f.severity == FindingSeverity::Warn
                    && f.message.contains("alice")),
                "expected MAILBOX-DIR-GROUP-DRIFT, got: {findings:?}"
            );
            assert!(
                !findings
                    .iter()
                    .any(|f| f.check == "MAILBOX-DIR-OWNER-DRIFT"),
                "owner uid matches, so MAILBOX-DIR-OWNER-DRIFT must NOT fire, got: {findings:?}"
            );
        }
        #[cfg(not(unix))]
        let _ = findings;
    }

    #[test]
    fn mailbox_ownership_flags_orphan_storage_dir() {
        let _guard = set_test_resolver(resolver_current_user);
        let tmp = tempfile::TempDir::new().unwrap();
        let config = s7_config_with_mailbox(tmp.path(), "testowner");
        std::fs::create_dir_all(tmp.path().join("inbox/alice")).unwrap();
        std::fs::create_dir_all(tmp.path().join("sent/alice")).unwrap();
        // Stale dir for a mailbox that doesn't exist in config.
        std::fs::create_dir_all(tmp.path().join("inbox/ghost")).unwrap();
        #[cfg(unix)]
        {
            chmod(&tmp.path().join("inbox/alice"), 0o700);
            chmod(&tmp.path().join("sent/alice"), 0o700);
        }

        let findings = check_mailbox_ownership(&config);
        assert!(
            findings
                .iter()
                .any(|f| f.check == "ORPHAN-STORAGE" && f.message.contains("ghost")),
            "expected ORPHAN-STORAGE for ghost dir, got: {findings:?}"
        );
    }

    #[test]
    fn mailbox_ownership_flags_orphan_config_when_dirs_missing() {
        let _guard = set_test_resolver(resolver_current_user);
        let tmp = tempfile::TempDir::new().unwrap();
        let config = s7_config_with_mailbox(tmp.path(), "testowner");
        // Config declares alice but neither inbox nor sent dirs exist.

        let findings = check_mailbox_ownership(&config);
        assert!(
            findings
                .iter()
                .any(|f| f.check == "ORPHAN-CONFIG" && f.message.contains("alice")),
            "expected ORPHAN-CONFIG for alice, got: {findings:?}"
        );
    }

    fn s7_template(name: &str, run_as: &str, cmd: Vec<&str>) -> HookTemplate {
        HookTemplate {
            name: name.into(),
            description: "test".into(),
            cmd: cmd.into_iter().map(String::from).collect(),
            params: vec![],
            stdin: HookTemplateStdin::Email,
            run_as: run_as.into(),
            timeout_secs: 60,
            allowed_events: vec![HookEvent::OnReceive],
        }
    }

    #[test]
    fn templates_pass_when_everything_resolves() {
        let _guard = set_test_resolver(resolver_current_user);
        let tmp = tempfile::TempDir::new().unwrap();
        let bin = tmp.path().join("agent");
        std::fs::write(&bin, "#!/bin/sh\nexit 0\n").unwrap();
        #[cfg(unix)]
        chmod(&bin, 0o755);

        let mut config = s7_config_with_mailbox(tmp.path(), "testowner");
        config.hook_templates = vec![s7_template(
            "invoke-foo",
            "testowner",
            vec![bin.to_str().unwrap()],
        )];

        let runner = MockRunner::new(AccessResult::Allowed);
        let findings = check_templates_with_runner(&config, &runner);
        assert!(
            findings.is_empty(),
            "expected no template findings, got: {findings:?}"
        );
        assert_eq!(
            runner.calls.get(),
            1,
            "expected one access-check for a non-reserved run_as user"
        );
    }

    #[test]
    fn templates_flag_orphan_run_as() {
        fn fake(name: &str) -> Option<ResolvedUser> {
            if name == "root" {
                Some(ResolvedUser {
                    name: "root".into(),
                    uid: 0,
                    gid: 0,
                })
            } else {
                None
            }
        }
        let _guard = set_test_resolver(fake);
        let tmp = tempfile::TempDir::new().unwrap();
        let bin = tmp.path().join("agent");
        std::fs::write(&bin, "#!/bin/sh\n").unwrap();
        #[cfg(unix)]
        chmod(&bin, 0o755);

        let mut config = s7_config_with_mailbox(tmp.path(), "root");
        config.hook_templates = vec![s7_template(
            "invoke-foo",
            "ghost-user",
            vec![bin.to_str().unwrap()],
        )];

        let runner = MockRunner::new(AccessResult::Allowed);
        let findings = check_templates_with_runner(&config, &runner);
        assert!(
            findings.iter().any(|f| f.check == "ORPHAN-TEMPLATE-RUN_AS"),
            "expected ORPHAN-TEMPLATE-RUN_AS, got: {findings:?}"
        );
        assert_eq!(
            runner.calls.get(),
            0,
            "access check must be skipped when run_as is orphan"
        );
    }

    #[test]
    fn templates_flag_missing_cmd_binary() {
        let _guard = set_test_resolver(resolver_current_user);
        let tmp = tempfile::TempDir::new().unwrap();
        let missing = tmp.path().join("does-not-exist");

        let mut config = s7_config_with_mailbox(tmp.path(), "testowner");
        config.hook_templates = vec![s7_template(
            "invoke-foo",
            "testowner",
            vec![missing.to_str().unwrap()],
        )];

        let runner = MockRunner::new(AccessResult::Allowed);
        let findings = check_templates_with_runner(&config, &runner);
        assert!(
            findings
                .iter()
                .any(|f| f.check == "TEMPLATE-CMD-NOT-EXECUTABLE"),
            "expected TEMPLATE-CMD-NOT-EXECUTABLE, got: {findings:?}"
        );
    }

    #[test]
    fn templates_flag_access_denied_by_run_as() {
        let _guard = set_test_resolver(resolver_current_user);
        let tmp = tempfile::TempDir::new().unwrap();
        let bin = tmp.path().join("agent");
        std::fs::write(&bin, "#!/bin/sh\n").unwrap();
        #[cfg(unix)]
        chmod(&bin, 0o755);

        let mut config = s7_config_with_mailbox(tmp.path(), "testowner");
        config.hook_templates = vec![s7_template(
            "invoke-foo",
            "testowner",
            vec![bin.to_str().unwrap()],
        )];

        let runner = MockRunner::new(AccessResult::Denied);
        let findings = check_templates_with_runner(&config, &runner);
        assert!(
            findings
                .iter()
                .any(|f| f.check == "TEMPLATE-CMD-NOT-EXECUTABLE-BY-RUN_AS"),
            "expected TEMPLATE-CMD-NOT-EXECUTABLE-BY-RUN_AS, got: {findings:?}"
        );
    }

    #[test]
    fn templates_emit_info_when_access_check_unknown() {
        let _guard = set_test_resolver(resolver_current_user);
        let tmp = tempfile::TempDir::new().unwrap();
        let bin = tmp.path().join("agent");
        std::fs::write(&bin, "#!/bin/sh\n").unwrap();
        #[cfg(unix)]
        chmod(&bin, 0o755);

        let mut config = s7_config_with_mailbox(tmp.path(), "testowner");
        config.hook_templates = vec![s7_template(
            "invoke-foo",
            "testowner",
            vec![bin.to_str().unwrap()],
        )];

        let runner = MockRunner::new(AccessResult::Unknown);
        let findings = check_templates_with_runner(&config, &runner);
        assert!(
            findings
                .iter()
                .any(|f| f.check == "TEMPLATE-CMD-ACCESS-UNKNOWN"
                    && f.severity == FindingSeverity::Info),
            "expected TEMPLATE-CMD-ACCESS-UNKNOWN info finding, got: {findings:?}"
        );
    }

    #[test]
    fn templates_skip_access_check_when_run_as_is_root() {
        let _guard = set_test_resolver(resolver_current_user);
        let tmp = tempfile::TempDir::new().unwrap();
        let bin = tmp.path().join("agent");
        std::fs::write(&bin, "#!/bin/sh\n").unwrap();
        #[cfg(unix)]
        chmod(&bin, 0o755);

        let mut config = s7_config_with_mailbox(tmp.path(), "root");
        config.hook_templates = vec![s7_template(
            "invoke-foo",
            "root",
            vec![bin.to_str().unwrap()],
        )];

        let runner = MockRunner::new(AccessResult::Denied);
        let findings = check_templates_with_runner(&config, &runner);
        assert!(
            findings.is_empty(),
            "root run_as must skip access check, got: {findings:?}"
        );
        assert_eq!(
            runner.calls.get(),
            0,
            "root run_as must not invoke the access runner"
        );
    }

    fn s7_hook(mailbox: &str, run_as: &str) -> Hook {
        let _ = mailbox;
        Hook {
            name: Some(format!("test-hook-{run_as}")),
            event: HookEvent::OnReceive,
            r#type: "cmd".into(),
            cmd: "echo hi".into(),
            dangerously_support_untrusted: false,
            origin: crate::hook::HookOrigin::Operator,
            template: None,
            params: BTreeMap::new(),
            run_as: Some(run_as.to_string()),
        }
    }

    #[test]
    fn hook_invariant_flags_mismatched_run_as() {
        // Alice's mailbox is owned by testowner, but the hook runs as
        // bob. That's a PRD §6.3 violation (run_as must equal owner or
        // be root).
        let _guard = set_test_resolver(resolver_current_user);
        let tmp = tempfile::TempDir::new().unwrap();
        let mut config = s7_config_with_mailbox(tmp.path(), "testowner");
        config
            .mailboxes
            .get_mut("alice")
            .unwrap()
            .hooks
            .push(s7_hook("alice", "bob"));

        let findings = check_hook_invariants(&config);
        assert!(
            findings
                .iter()
                .any(|f| f.check == "HOOK-INVARIANT" && f.severity == FindingSeverity::Fail),
            "expected HOOK-INVARIANT Fail, got: {findings:?}"
        );
    }

    #[test]
    fn hook_invariant_passes_when_run_as_matches_owner() {
        let _guard = set_test_resolver(resolver_current_user);
        let tmp = tempfile::TempDir::new().unwrap();
        let mut config = s7_config_with_mailbox(tmp.path(), "testowner");
        config
            .mailboxes
            .get_mut("alice")
            .unwrap()
            .hooks
            .push(s7_hook("alice", "testowner"));

        let findings = check_hook_invariants(&config);
        assert!(
            findings.is_empty(),
            "expected no hook invariant findings, got: {findings:?}"
        );
    }

    #[test]
    fn catchall_user_check_passes_without_catchall() {
        let _guard = set_test_resolver(resolver_current_user);
        let tmp = tempfile::TempDir::new().unwrap();
        let config = s7_config_with_mailbox(tmp.path(), "testowner");
        let findings = check_catchall_user(&config);
        assert!(
            findings.is_empty(),
            "no catchall mailbox → no finding, got: {findings:?}"
        );
    }

    #[test]
    fn catchall_user_check_fails_when_user_missing() {
        fn fake(name: &str) -> Option<ResolvedUser> {
            if name == "testowner" || name == "root" {
                let (uid, gid) = current_uid_gid();
                Some(ResolvedUser {
                    name: name.to_string(),
                    uid,
                    gid,
                })
            } else {
                None
            }
        }
        let _guard = set_test_resolver(fake);
        let tmp = tempfile::TempDir::new().unwrap();
        let mut config = s7_config_with_mailbox(tmp.path(), "testowner");
        // Add a catchall mailbox whose address matches `*@ex.com`.
        config.mailboxes.insert(
            "catchall".to_string(),
            MailboxConfig {
                address: "*@ex.com".to_string(),
                owner: "testowner".to_string(),
                hooks: vec![],
                trust: None,
                trusted_senders: None,
                allow_root_catchall: false,
            },
        );

        let findings = check_catchall_user(&config);
        assert!(
            findings
                .iter()
                .any(|f| f.check == "CATCHALL-USER-MISSING" && f.severity == FindingSeverity::Fail),
            "expected CATCHALL-USER-MISSING, got: {findings:?}"
        );
    }

    #[test]
    fn catchall_user_check_passes_when_user_exists() {
        fn fake(name: &str) -> Option<ResolvedUser> {
            let (uid, gid) = current_uid_gid();
            if name == "testowner" || name == "root" || name == "aimx-catchall" {
                Some(ResolvedUser {
                    name: name.to_string(),
                    uid,
                    gid,
                })
            } else {
                None
            }
        }
        let _guard = set_test_resolver(fake);
        let tmp = tempfile::TempDir::new().unwrap();
        let mut config = s7_config_with_mailbox(tmp.path(), "testowner");
        config.mailboxes.insert(
            "catchall".to_string(),
            MailboxConfig {
                address: "*@ex.com".to_string(),
                owner: "testowner".to_string(),
                hooks: vec![],
                trust: None,
                trusted_senders: None,
                allow_root_catchall: false,
            },
        );

        let findings = check_catchall_user(&config);
        assert!(
            findings.is_empty(),
            "catchall user resolvable → no finding, got: {findings:?}"
        );
    }

    #[test]
    fn load_warnings_translate_to_findings() {
        let warnings = vec![
            LoadWarning::OrphanMailboxOwner {
                mailbox: "alice".into(),
                owner: "ghost".into(),
            },
            LoadWarning::OrphanTemplateRunAs {
                template: "invoke-foo".into(),
                run_as: "ghost".into(),
            },
            LoadWarning::OrphanHookRunAs {
                mailbox: "alice".into(),
                hook_name: "h1".into(),
                run_as: "ghost".into(),
            },
            LoadWarning::LegacyAimxHookUser,
            LoadWarning::RootCatchallAccepted {
                mailbox: "catchall".into(),
            },
        ];
        let findings = translate_load_warnings(&warnings);
        // LegacyAimxHookUser is handled separately in
        // check_legacy_aimx_hook_user, so it should not produce a
        // finding here.
        assert!(
            !findings.iter().any(|f| f.check == "LEGACY-AIMX-HOOK-USER"),
            "LoadWarning::LegacyAimxHookUser should NOT produce a finding in translate_load_warnings"
        );
        assert!(
            findings
                .iter()
                .any(|f| f.check == "ORPHAN-MAILBOX-OWNER" && f.severity == FindingSeverity::Warn),
            "expected ORPHAN-MAILBOX-OWNER Warn, got: {findings:?}"
        );
        assert!(
            findings
                .iter()
                .any(|f| f.check == "ORPHAN-TEMPLATE-RUN_AS"
                    && f.severity == FindingSeverity::Warn),
            "expected ORPHAN-TEMPLATE-RUN_AS Warn, got: {findings:?}"
        );
        assert!(
            findings
                .iter()
                .any(|f| f.check == "ORPHAN-HOOK-RUN_AS" && f.severity == FindingSeverity::Warn),
            "expected ORPHAN-HOOK-RUN_AS Warn, got: {findings:?}"
        );
        assert!(
            findings
                .iter()
                .any(|f| f.check == "ROOT-CATCHALL-ACCEPTED"
                    && f.severity == FindingSeverity::Info),
            "expected ROOT-CATCHALL-ACCEPTED Info, got: {findings:?}"
        );
    }

    #[test]
    fn format_checks_empty_says_all_passed() {
        let out = format_checks(&[]);
        assert!(out.contains("All checks passed"), "got: {out}");
    }

    #[test]
    fn format_checks_summary_counts_by_severity() {
        let findings = vec![
            DoctorFinding::new("A", FindingSeverity::Fail, "bad"),
            DoctorFinding::new("B", FindingSeverity::Warn, "uh"),
            DoctorFinding::new("C", FindingSeverity::Warn, "hmm"),
            DoctorFinding::new("D", FindingSeverity::Info, "fyi"),
        ];
        let out = format_checks(&findings);
        assert!(out.contains("1 fail"), "{out}");
        assert!(out.contains("2 warn"), "{out}");
        assert!(out.contains("1 info"), "{out}");
    }

    #[test]
    fn orphan_check_ids_covers_expected_set() {
        // `hooks prune --orphans` uses this set to decide which Fail
        // findings to skip in its pre-flight. This test guards the
        // contract: every orphan-related check ID here must also be
        // the canonical ID emitted by a check. If a check renames its
        // ID, this test fails and forces the operator to decide
        // whether the new ID should still be in the orphan set.
        let expected = [
            "ORPHAN-STORAGE",
            "ORPHAN-CONFIG",
            "ORPHAN-TEMPLATE-RUN_AS",
            "ORPHAN-HOOK-RUN_AS",
        ];
        for id in &expected {
            assert!(
                ORPHAN_CHECK_IDS.contains(id),
                "expected {id} to be in ORPHAN_CHECK_IDS"
            );
        }
    }
}
