//! Storage path helper for mailbox `inbox/` and `sent/` directories.
//!
//! Every consumer that builds a mailbox storage path goes through
//! [`mailbox_storage_path`]. Raw string concatenation of `<data_dir>/inbox/`
//! or `<data_dir>/sent/` outside this module is rejected by the CI
//! `storage-paths` grep job — adding a new layout (or fixing a path bug)
//! must remain a one-file change.
//!
//! The helper is layout-aware: on a v2 (post-migration) install the
//! `.layout-version` marker is present and the per-mailbox tree lives
//! under `<data_dir>/<domain>/{inbox|sent}/<local>/`. On a v1 (never
//! migrated) install the legacy `<data_dir>/{inbox|sent}/<local>/` shape
//! is returned so single-domain installs keep functioning before the
//! upgrade migration has run.

use std::path::PathBuf;

use crate::config::{Config, MailboxConfig};

/// Which side of a mailbox the caller is addressing — the inbox tree
/// (`/inbox/<local>/`) or the sent tree (`/sent/<local>/`). Used by
/// [`mailbox_storage_path`] to disambiguate without overloading function
/// names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Folder {
    Inbox,
    Sent,
}

impl Folder {
    /// Path component string used by the layout (`"inbox"` / `"sent"`).
    pub fn as_str(self) -> &'static str {
        match self {
            Folder::Inbox => "inbox",
            Folder::Sent => "sent",
        }
    }
}

/// Path to the per-mailbox `<folder>/<name>/` directory under
/// `<data_dir>`, resolved through the current layout.
///
/// Behaviour:
///
/// - **v2 (per-domain) layout** — `.layout-version` marker is present
///   under `data_dir`. The returned path is
///   `<data_dir>/<domain>/<folder>/<name>/` where `<domain>` is the
///   domain portion of `mailbox.address` and `<name>` is the operator-
///   friendly key (the local-part for legacy installs, the FQDN-keyed
///   stem for canonical configs). The local-part of the address is
///   used so a v1-shape config that still carries
///   `[mailboxes.info]` with `address = "info@x.com"` resolves to
///   `<data_dir>/x.com/inbox/info/`, exactly where the upgrade migration
///   relocated the mail.
/// - **v1 (legacy) layout** — no marker. The returned path is the
///   legacy `<data_dir>/<folder>/<name>/`. Same single-domain layout as
///   pre-multi-domain installs.
///
/// The helper accepts the operator-friendly `name` directly so it can be
/// used both by callers that already have a `MailboxConfig` reference
/// and by callers that only have a key string (e.g. the daemon's
/// MAILBOX-CRUD handler before it has constructed the `MailboxConfig`).
pub fn mailbox_storage_path(config: &Config, mailbox: &MailboxConfig, folder: Folder) -> PathBuf {
    let domain = mailbox
        .address
        .rsplit_once('@')
        .map(|(_, d)| d.to_string())
        .unwrap_or_else(|| config.default_domain().to_string());
    let name = mailbox_dir_name(mailbox);
    storage_path_for(config, &domain, &name, folder)
}

/// Lower-level variant for callers that don't yet have a `MailboxConfig`.
/// Resolves `<data_dir>/<domain>/<folder>/<name>/` on v2 layouts and
/// `<data_dir>/<folder>/<name>/` on v1 layouts.
pub fn storage_path_for(config: &Config, domain: &str, name: &str, folder: Folder) -> PathBuf {
    if config.data_dir.join(".layout-version").is_file() {
        config
            .data_dir
            .join(domain)
            .join(folder.as_str())
            .join(name)
    } else {
        config.data_dir.join(folder.as_str()).join(name)
    }
}

/// On-disk directory name used by a mailbox under `inbox/` and `sent/`.
///
/// The on-disk directory name continues to be the local-part for
/// legacy installs (and for catchall mailboxes, which use the special
/// `"catchall"` directory historically). Per-domain catchall mailboxes
/// keyed `*@<domain>` map to the `catchall` directory under that
/// domain's tree.
pub fn mailbox_dir_name(mailbox: &MailboxConfig) -> String {
    let local = mailbox
        .address
        .rsplit_once('@')
        .map(|(local, _)| local)
        .unwrap_or(mailbox.address.as_str());
    if local == "*" {
        "catchall".to_string()
    } else {
        local.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use tempfile::TempDir;

    fn mb(address: &str) -> MailboxConfig {
        MailboxConfig {
            address: address.to_string(),
            owner: "ops".to_string(),
            hooks: vec![],
            trust: None,
            trusted_senders: None,
            allow_root_catchall: false,
        }
    }

    fn cfg(data_dir: &std::path::Path, domains: &[&str]) -> Config {
        Config {
            domains: domains.iter().map(|s| s.to_string()).collect(),
            data_dir: data_dir.to_path_buf(),
            dkim_selector: Some("aimx".into()),
            trust: "none".into(),
            trusted_senders: vec![],
            mailboxes: HashMap::new(),
            per_domain: HashMap::new(),
            verify_host: None,
            enable_ipv6: false,
            signature: None,
            upgrade: None,
        }
    }

    #[test]
    fn v1_layout_uses_legacy_path() {
        let tmp = TempDir::new().unwrap();
        let c = cfg(tmp.path(), &["x.com"]);
        let m = mb("info@x.com");
        assert_eq!(
            mailbox_storage_path(&c, &m, Folder::Inbox),
            tmp.path().join("inbox").join("info"),
        );
        assert_eq!(
            mailbox_storage_path(&c, &m, Folder::Sent),
            tmp.path().join("sent").join("info"),
        );
    }

    #[test]
    fn v2_layout_uses_per_domain_path() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join(".layout-version"), "2\n").unwrap();
        let c = cfg(tmp.path(), &["x.com"]);
        let m = mb("info@x.com");
        assert_eq!(
            mailbox_storage_path(&c, &m, Folder::Inbox),
            tmp.path().join("x.com").join("inbox").join("info"),
        );
        assert_eq!(
            mailbox_storage_path(&c, &m, Folder::Sent),
            tmp.path().join("x.com").join("sent").join("info"),
        );
    }

    #[test]
    fn v2_layout_two_domains_route_per_message_domain() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join(".layout-version"), "2\n").unwrap();
        let c = cfg(tmp.path(), &["a.com", "b.com"]);
        assert_eq!(
            mailbox_storage_path(&c, &mb("info@a.com"), Folder::Inbox),
            tmp.path().join("a.com").join("inbox").join("info"),
        );
        assert_eq!(
            mailbox_storage_path(&c, &mb("info@b.com"), Folder::Sent),
            tmp.path().join("b.com").join("sent").join("info"),
        );
    }

    #[test]
    fn catchall_lives_under_catchall_dirname() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join(".layout-version"), "2\n").unwrap();
        let c = cfg(tmp.path(), &["x.com"]);
        let m = mb("*@x.com");
        assert_eq!(
            mailbox_storage_path(&c, &m, Folder::Inbox),
            tmp.path().join("x.com").join("inbox").join("catchall"),
        );
    }
}
