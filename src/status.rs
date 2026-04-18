use crate::config::Config;
use crate::frontmatter::InboundFrontmatter;
use crate::setup::{RealSystemOps, SystemOps};
use crate::term;
use std::path::{Path, PathBuf};

pub struct StatusInfo {
    pub domain: String,
    pub data_dir: String,
    pub dkim_selector: String,
    pub dkim_key_present: bool,
    pub smtp_running: bool,
    pub mailboxes: Vec<MailboxStatus>,
    pub recent_activity: Vec<RecentEmail>,
}

pub struct MailboxStatus {
    pub name: String,
    pub address: String,
    pub total: usize,
    pub unread: usize,
}

pub struct RecentEmail {
    pub mailbox: String,
    pub id: String,
    pub from: String,
    pub subject: String,
    pub date: String,
}

pub fn gather_status(config: &Config) -> StatusInfo {
    gather_status_with_ops(config, &RealSystemOps)
}

/// Injectable seam for testing: takes a `SystemOps` implementation so tests
/// can mock `is_service_running` without shelling out to `systemctl`/`rc-service`.
pub fn gather_status_with_ops<S: SystemOps>(config: &Config, sys: &S) -> StatusInfo {
    let dkim_key_present = crate::config::dkim_dir().join("private.key").exists();
    let smtp_running = sys.is_service_running("aimx");

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
            }
        })
        .collect();

    mailboxes.sort_by(|a, b| a.name.cmp(&b.name));

    let recent_activity = gather_recent_activity(config);

    StatusInfo {
        domain: config.domain.clone(),
        data_dir: config.data_dir.to_string_lossy().to_string(),
        dkim_selector: config.dkim_selector.clone(),
        dkim_key_present,
        smtp_running,
        mailboxes,
        recent_activity,
    }
}

fn gather_recent_activity(config: &Config) -> Vec<RecentEmail> {
    let mut all: Vec<RecentEmail> = Vec::new();

    for name in config.mailboxes.keys() {
        let dir = config.inbox_dir(name);
        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(_) => continue,
        };

        // Collect both flat `<stem>.md` and bundle `<stem>/<stem>.md`
        // paths; sort newest-first by stem (UTC timestamp prefix orders
        // lexicographically).
        let mut md_paths: Vec<PathBuf> = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if let Some(stem) = path.file_name().and_then(|f| f.to_str()) {
                    let md = path.join(format!("{stem}.md"));
                    if md.exists() {
                        md_paths.push(md);
                    }
                }
            } else if path.extension().is_some_and(|ext| ext == "md") {
                md_paths.push(path);
            }
        }
        md_paths.sort_by(|a, b| b.file_name().cmp(&a.file_name()));

        for path in md_paths.into_iter().take(3) {
            let content = match std::fs::read_to_string(&path) {
                Ok(c) => c,
                Err(_) => continue,
            };

            let fm = match extract_frontmatter(&content) {
                Some(fm) => fm,
                None => continue,
            };

            let meta: InboundFrontmatter = match toml::from_str(&fm) {
                Ok(m) => m,
                Err(_) => continue,
            };

            all.push(RecentEmail {
                mailbox: name.clone(),
                id: meta.id,
                from: meta.from,
                subject: meta.subject,
                date: meta.date,
            });
        }
    }

    all.sort_by(|a, b| b.date.cmp(&a.date));
    all.truncate(5);
    all
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

pub fn format_status(info: &StatusInfo) -> String {
    let mut out = String::new();

    out.push_str(&format!("{}\n", term::header("Configuration")));
    out.push_str(&format!("Domain:           {}\n", info.domain));
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

    out.push_str(&format!("\n{}\n", term::header("Service")));
    out.push_str(&format!(
        "SMTP server:      {}\n",
        if info.smtp_running {
            term::success("running")
        } else {
            term::warn("not running")
        }
    ));

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
        out.push_str(&format!(
            "  {:<20} {:<30} {:>8} {:>8}\n",
            "MAILBOX", "ADDRESS", "TOTAL", "UNREAD"
        ));
        for mb in &info.mailboxes {
            let name_pad = 20usize.saturating_sub(mb.name.chars().count());
            out.push_str(&format!(
                "  {}{:pad$} {:<30} {:>8} {:>8}\n",
                term::highlight(&mb.name),
                "",
                mb.address,
                mb.total,
                mb.unread,
                pad = name_pad,
            ));
        }
    }

    if !info.recent_activity.is_empty() {
        out.push_str(&format!("\n{}\n", term::header("Recent activity:")));
        for email in &info.recent_activity {
            out.push_str(&format!(
                "  [{}] {} {} - {} ({})\n",
                email.mailbox, email.id, email.from, email.subject, email.date,
            ));
        }
    }

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

    /// Minimal mock that only exercises `is_service_running`. All other
    /// `SystemOps` methods panic — they must not be reached by `gather_status`.
    struct FakeServiceOps {
        running: bool,
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
    }

    fn empty_config(data_dir: &Path) -> Config {
        Config {
            domain: "test.com".to_string(),
            data_dir: data_dir.to_path_buf(),
            dkim_selector: "dkim".to_string(),
            trust: "none".to_string(),
            trusted_senders: vec![],
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
        let info = gather_status_with_ops(&config, &FakeServiceOps { running: true });
        assert!(info.smtp_running);
    }

    #[test]
    fn gather_status_reports_not_running_when_systemops_returns_false() {
        let tmp = tempfile::TempDir::new().unwrap();
        let _cfg_guard = crate::config::test_env::ConfigDirOverride::set(tmp.path());

        let config = empty_config(tmp.path());
        let info = gather_status_with_ops(&config, &FakeServiceOps { running: false });
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
            dkim_selector: "dkim".to_string(),
            dkim_key_present: true,
            smtp_running: true,
            mailboxes: vec![],
            recent_activity: vec![],
        };
        let output = format_status(&info);
        assert!(output.contains("test.example.com"));
        assert!(output.contains("present"));
        assert!(output.contains("running"));
        assert!(output.contains("0 (0 messages, 0 unread)"));
    }

    #[test]
    fn format_status_with_mailboxes() {
        let info = StatusInfo {
            domain: "agent.example.com".to_string(),
            data_dir: "/var/lib/aimx".to_string(),
            dkim_selector: "dkim".to_string(),
            dkim_key_present: true,
            smtp_running: false,
            mailboxes: vec![
                MailboxStatus {
                    name: "catchall".to_string(),
                    address: "*@agent.example.com".to_string(),
                    total: 10,
                    unread: 3,
                },
                MailboxStatus {
                    name: "support".to_string(),
                    address: "support@agent.example.com".to_string(),
                    total: 5,
                    unread: 1,
                },
            ],
            recent_activity: vec![],
        };
        let output = format_status(&info);
        assert!(output.contains("agent.example.com"));
        assert!(output.contains("not running"));
        assert!(output.contains("2 (15 messages, 4 unread)"));
        assert!(output.contains("catchall"));
        assert!(output.contains("support"));
        assert!(output.contains("MAILBOX"));
        assert!(output.contains("TOTAL"));
        assert!(output.contains("UNREAD"));
    }

    #[test]
    fn mailbox_table_columns_align_regardless_of_color() {
        let info = StatusInfo {
            domain: "ex.com".to_string(),
            data_dir: "/var/lib/aimx".to_string(),
            dkim_selector: "dkim".to_string(),
            dkim_key_present: true,
            smtp_running: true,
            mailboxes: vec![
                MailboxStatus {
                    name: "ops".to_string(),
                    address: "ops@ex.com".to_string(),
                    total: 1,
                    unread: 0,
                },
                MailboxStatus {
                    name: "catchall".to_string(),
                    address: "*@ex.com".to_string(),
                    total: 2,
                    unread: 1,
                },
            ],
            recent_activity: vec![],
        };

        // Returns the visible column where the ADDRESS field starts on each
        // mailbox data row (the second non-whitespace token after the two-space indent).
        fn address_column(output: &str) -> Vec<usize> {
            let ansi = regex_like_strip(output);
            ansi.lines()
                .filter(|l| l.contains("@ex.com"))
                .map(|l| {
                    let trimmed = l.trim_start();
                    let name_end = trimmed.find(char::is_whitespace).unwrap_or(0);
                    let rest = &trimmed[name_end..];
                    let addr_start_in_rest = rest.len() - rest.trim_start().len();
                    (l.len() - trimmed.len()) + name_end + addr_start_in_rest
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
            dkim_selector: "dkim".to_string(),
            dkim_key_present: false,
            smtp_running: false,
            mailboxes: vec![],
            recent_activity: vec![],
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
                on_receive: vec![],
                trust: None,
                trusted_senders: None,
            },
        );

        let config = Config {
            domain: "test.com".to_string(),
            data_dir: data_dir.to_path_buf(),
            dkim_selector: "dkim".to_string(),
            trust: "none".to_string(),
            trusted_senders: vec![],
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
    fn format_status_includes_recent_activity() {
        let info = StatusInfo {
            domain: "test.com".to_string(),
            data_dir: "/var/lib/aimx".to_string(),
            dkim_selector: "dkim".to_string(),
            dkim_key_present: true,
            smtp_running: true,
            mailboxes: vec![],
            recent_activity: vec![
                RecentEmail {
                    mailbox: "catchall".to_string(),
                    id: "2025-01-15-001".to_string(),
                    from: "alice@example.com".to_string(),
                    subject: "Hello".to_string(),
                    date: "2025-01-15T10:30:00Z".to_string(),
                },
                RecentEmail {
                    mailbox: "support".to_string(),
                    id: "2025-01-14-001".to_string(),
                    from: "bob@example.com".to_string(),
                    subject: "Help needed".to_string(),
                    date: "2025-01-14T08:00:00Z".to_string(),
                },
            ],
        };
        let output = format_status(&info);
        assert!(
            output.contains("Recent activity:"),
            "Output should contain recent activity section"
        );
        assert!(output.contains("alice@example.com"));
        assert!(output.contains("Hello"));
        assert!(output.contains("bob@example.com"));
        assert!(output.contains("Help needed"));
        assert!(output.contains("[catchall]"));
        assert!(output.contains("[support]"));
    }

    #[test]
    fn format_status_no_recent_activity() {
        let info = StatusInfo {
            domain: "test.com".to_string(),
            data_dir: "/var/lib/aimx".to_string(),
            dkim_selector: "dkim".to_string(),
            dkim_key_present: true,
            smtp_running: true,
            mailboxes: vec![],
            recent_activity: vec![],
        };
        let output = format_status(&info);
        assert!(!output.contains("Recent activity:"));
    }

    #[test]
    fn gather_recent_activity_reads_emails() {
        let tmp = tempfile::TempDir::new().unwrap();
        let data_dir = tmp.path();
        let catchall = data_dir.join("inbox").join("catchall");
        std::fs::create_dir_all(&catchall).unwrap();

        let email_content = "+++\nid = \"2025-01-15-001\"\nmessage_id = \"<a@b>\"\nfrom = \"alice@example.com\"\nto = \"c@d\"\nsubject = \"Test email\"\ndate = \"2025-01-15T10:30:00Z\"\nin_reply_to = \"\"\nreferences = \"\"\nattachments = []\nmailbox = \"catchall\"\nread = false\ndkim = \"none\"\nspf = \"none\"\n+++\nBody here";
        std::fs::write(catchall.join("2025-01-15-001.md"), email_content).unwrap();

        let mut mailboxes = std::collections::HashMap::new();
        mailboxes.insert(
            "catchall".to_string(),
            crate::config::MailboxConfig {
                address: "*@test.com".to_string(),
                on_receive: vec![],
                trust: None,
                trusted_senders: None,
            },
        );

        let config = Config {
            domain: "test.com".to_string(),
            data_dir: data_dir.to_path_buf(),
            dkim_selector: "dkim".to_string(),
            trust: "none".to_string(),
            trusted_senders: vec![],
            mailboxes,
            verify_host: None,
            enable_ipv6: false,
        };

        let activity = gather_recent_activity(&config);
        assert_eq!(activity.len(), 1);
        assert_eq!(activity[0].from, "alice@example.com");
        assert_eq!(activity[0].subject, "Test email");
        assert_eq!(activity[0].mailbox, "catchall");
    }
}
