//! Daemon-side handler for the `MAILBOX-LIST` verb of the `AIMX/1`
//! UDS protocol.
//!
//! The MCP `mailbox_list` tool used to read `/etc/aimx/config.toml`
//! directly from a non-root process and would always fail on the
//! `0640 root:root` permission. After this rework the MCP tool is a
//! thin UDS client: it ships `AIMX/1 MAILBOX-LIST`, the daemon
//! resolves the caller via `SO_PEERCRED`, and answers with a JSON
//! array filtered to mailboxes the caller owns.
//!
//! Filtering rules (mirror the previous MCP-side semantics):
//!
//! - Root sees every mailbox the daemon knows about, including the
//!   catchall and every stray on-disk directory.
//! - A non-root caller sees only the mailboxes whose configured
//!   `owner` resolves to the caller's uid. Stray on-disk directories
//!   appear only when the directory's owning uid matches the caller.
//! - The catchall is owned by the dedicated `aimx-catchall` system
//!   user; non-root callers who are not that user do not see it.
//!
//! No locks are taken: the response reads from the in-memory
//! `Arc<Config>` snapshot via [`ConfigHandle::load`] plus a single
//! pass over the on-disk inbox/sent counters. A concurrent
//! `MAILBOX-CREATE` / `MAILBOX-DELETE` may swap the snapshot in
//! between two MAILBOX-LIST calls, but each individual call observes
//! a consistent snapshot.

use std::os::unix::fs::MetadataExt;
use std::path::Path;

#[cfg(test)]
use serde::Deserialize;
use serde::Serialize;

use crate::config::Config;
use crate::mailbox;
use crate::send_protocol::{ErrCode, JsonAckResponse};
use crate::state_handler::StateContext;
use crate::uds_authz::Caller;

/// One row of the JSON array returned by `MAILBOX-LIST`. Field order
/// matches the PRD: identity → paths → counts → registration flag.
#[derive(Debug, Serialize)]
#[cfg_attr(test, derive(Deserialize))]
pub struct MailboxListRow {
    pub name: String,
    pub inbox_path: String,
    pub sent_path: String,
    pub total: usize,
    pub unread: usize,
    pub sent_count: usize,
    pub registered: bool,
}

/// Build the JSON ack response for an `AIMX/1 MAILBOX-LIST` request.
pub async fn handle_mailbox_list(state_ctx: &StateContext, caller: &Caller) -> JsonAckResponse {
    let config = state_ctx.config_handle.load();
    let rows = collect_rows(&config, caller.uid);
    let body = match serde_json::to_vec(&rows) {
        Ok(b) => b,
        Err(e) => {
            return JsonAckResponse::Err {
                code: ErrCode::Io,
                reason: format!("failed to serialize mailbox list: {e}"),
            };
        }
    };
    JsonAckResponse::Ok { body }
}

/// Pure helper: walk `discover_mailbox_names`, count messages, and
/// keep only the rows visible to `caller_uid`. Extracted for unit
/// testing without spinning up a real `Caller` / `StateContext`.
fn collect_rows(config: &Config, caller_uid: u32) -> Vec<MailboxListRow> {
    let names = mailbox::discover_mailbox_names(config);
    let mut rows: Vec<MailboxListRow> = Vec::with_capacity(names.len());
    for name in names {
        if !is_visible_to(config, &name, caller_uid) {
            continue;
        }
        let inbox_dir = config.inbox_dir(&name);
        let sent_dir = config.sent_dir(&name);
        let (total, unread) = count_inbox(&inbox_dir);
        let sent_count = mailbox::count_messages(&sent_dir);
        rows.push(MailboxListRow {
            name: name.clone(),
            inbox_path: inbox_dir.to_string_lossy().into_owned(),
            sent_path: sent_dir.to_string_lossy().into_owned(),
            total,
            unread,
            sent_count,
            registered: mailbox::is_registered(config, &name),
        });
    }
    rows.sort_by(|a, b| a.name.cmp(&b.name));
    rows
}

/// Visibility rule for the listing. Root sees every mailbox. A non-
/// root caller sees a mailbox when:
///
/// - The mailbox is registered in `config.mailboxes` and its `owner`
///   resolves to the caller's uid; or
/// - The mailbox is a stray on-disk directory (no config entry) and
///   the inbox directory's filesystem owner matches the caller.
fn is_visible_to(config: &Config, name: &str, caller_uid: u32) -> bool {
    if caller_uid == 0 {
        return true;
    }
    if mailbox::is_registered(config, name) {
        return mailbox::caller_owns(config, name, caller_uid);
    }
    // Unregistered stray dir: trust the inbox dir's filesystem owner.
    let inbox_dir = config.inbox_dir(name);
    inbox_owner_uid(&inbox_dir).is_some_and(|uid| uid == caller_uid)
}

fn inbox_owner_uid(dir: &Path) -> Option<u32> {
    std::fs::symlink_metadata(dir).ok().map(|m| m.uid())
}

fn count_inbox(dir: &Path) -> (usize, usize) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return (0, 0),
    };
    let mut total = 0usize;
    let mut unread = 0usize;
    for entry in entries.filter_map(|e| e.ok()) {
        let path = entry.path();
        let md_path = if path.is_dir() {
            let stem = match path.file_name().and_then(|f| f.to_str()) {
                Some(s) => s.to_string(),
                None => continue,
            };
            let inner = path.join(format!("{stem}.md"));
            if !inner.exists() {
                continue;
            }
            inner
        } else if path.extension().is_some_and(|ext| ext == "md") {
            path
        } else {
            continue;
        };
        total += 1;
        if !is_marked_read(&md_path) {
            unread += 1;
        }
    }
    (total, unread)
}

/// Cheap "is this email already marked read?" check that parses the
/// frontmatter just enough to find the `read = true|false` line.
/// Avoids the full TOML decode the older `mcp::list_emails` did — the
/// listing path stays cheap on huge mailboxes.
fn is_marked_read(md_path: &Path) -> bool {
    let content = match std::fs::read_to_string(md_path) {
        Ok(c) => c,
        Err(_) => return false,
    };
    let parts: Vec<&str> = content.splitn(3, "+++").collect();
    if parts.len() < 3 {
        return false;
    }
    for line in parts[1].lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("read") {
            let rest = rest.trim_start();
            if let Some(value) = rest.strip_prefix('=') {
                return value.trim() == "true";
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, ConfigHandle, MailboxConfig};
    use std::collections::HashMap;
    use tempfile::TempDir;

    /// Map `"alice"` to uid/gid 4242 (and `"root"` to 0). Static fn
    /// because the user_resolver test seam takes a function pointer,
    /// not a closure.
    fn fake_resolver(name: &str) -> Option<crate::user_resolver::ResolvedUser> {
        match name {
            "alice" => Some(crate::user_resolver::ResolvedUser {
                name: "alice".to_string(),
                uid: 4242,
                gid: 4242,
            }),
            "root" => Some(crate::user_resolver::ResolvedUser {
                name: "root".to_string(),
                uid: 0,
                gid: 0,
            }),
            _ => None,
        }
    }

    fn install_tester_resolver() -> crate::user_resolver::test_resolver::ResolverOverride {
        crate::user_resolver::set_test_resolver(fake_resolver)
    }

    fn base_config(data_dir: &Path, owner: &str) -> Config {
        let mut mailboxes = HashMap::new();
        mailboxes.insert(
            "catchall".to_string(),
            MailboxConfig {
                address: "*@example.com".to_string(),
                owner: "aimx-catchall".to_string(),
                hooks: vec![],
                trust: None,
                trusted_senders: None,
                allow_root_catchall: false,
            },
        );
        mailboxes.insert(
            "alice".to_string(),
            MailboxConfig {
                address: "alice@example.com".to_string(),
                owner: owner.to_string(),
                hooks: vec![],
                trust: None,
                trusted_senders: None,
                allow_root_catchall: false,
            },
        );
        Config {
            domain: "example.com".to_string(),
            data_dir: data_dir.to_path_buf(),
            dkim_selector: "aimx".to_string(),
            trust: "none".to_string(),
            trusted_senders: vec![],
            mailboxes,
            verify_host: None,
            enable_ipv6: false,
            upgrade: None,
        }
    }

    fn seed_dirs(tmp: &Path) {
        for sub in [
            "inbox/catchall",
            "sent/catchall",
            "inbox/alice",
            "sent/alice",
        ] {
            std::fs::create_dir_all(tmp.join(sub)).unwrap();
        }
    }

    /// Root sees every mailbox in the configured set.
    #[tokio::test]
    async fn root_sees_every_mailbox() {
        let tmp = TempDir::new().unwrap();
        seed_dirs(tmp.path());
        let config = base_config(tmp.path(), "alice");
        let handle = ConfigHandle::new(config);
        let state_ctx = StateContext::new(tmp.path().to_path_buf(), handle);

        let resp = handle_mailbox_list(&state_ctx, &Caller::internal_root()).await;
        let body = match resp {
            JsonAckResponse::Ok { body } => body,
            other => panic!("expected Ok, got {other:?}"),
        };
        let rows: Vec<MailboxListRow> = serde_json::from_slice(&body).unwrap();
        let names: Vec<String> = rows.iter().map(|r| r.name.clone()).collect();
        assert!(names.contains(&"catchall".to_string()));
        assert!(names.contains(&"alice".to_string()));
    }

    /// A non-root caller whose uid matches `alice`'s owner sees alice
    /// but does not see the catchall (different owner).
    #[tokio::test]
    async fn non_root_owner_sees_only_owned_mailboxes() {
        let tmp = TempDir::new().unwrap();
        seed_dirs(tmp.path());
        let _r = install_tester_resolver();
        let config = base_config(tmp.path(), "alice");
        let handle = ConfigHandle::new(config);
        let state_ctx = StateContext::new(tmp.path().to_path_buf(), handle);

        let caller = Caller::new(4242, 4242, None);
        let resp = handle_mailbox_list(&state_ctx, &caller).await;
        let body = match resp {
            JsonAckResponse::Ok { body } => body,
            other => panic!("expected Ok, got {other:?}"),
        };
        let rows: Vec<MailboxListRow> = serde_json::from_slice(&body).unwrap();
        let names: Vec<String> = rows.iter().map(|r| r.name.clone()).collect();
        assert_eq!(names, vec!["alice".to_string()]);
        assert!(rows[0].registered);
        assert!(rows[0].inbox_path.ends_with("/inbox/alice"));
        assert!(rows[0].sent_path.ends_with("/sent/alice"));
    }

    /// A stranger uid sees an empty array — never `null` and never a
    /// human-readable "no mailboxes" string.
    #[tokio::test]
    async fn stranger_sees_empty_array() {
        let tmp = TempDir::new().unwrap();
        seed_dirs(tmp.path());
        let _r = install_tester_resolver();
        let config = base_config(tmp.path(), "alice");
        let handle = ConfigHandle::new(config);
        let state_ctx = StateContext::new(tmp.path().to_path_buf(), handle);

        let stranger = Caller::new(99999, 99999, None);
        let resp = handle_mailbox_list(&state_ctx, &stranger).await;
        let body = match resp {
            JsonAckResponse::Ok { body } => body,
            other => panic!("expected Ok, got {other:?}"),
        };
        assert_eq!(body, b"[]");
    }

    /// Inbox total / unread counts populate from on-disk `.md` files
    /// even when frontmatter parsing finds the `read = true` line via
    /// the cheap line-scan.
    #[tokio::test]
    async fn counts_match_on_disk_state() {
        let tmp = TempDir::new().unwrap();
        seed_dirs(tmp.path());
        let _r = install_tester_resolver();

        // Two emails: one read, one unread.
        let inbox = tmp.path().join("inbox").join("alice");
        let unread = "+++\nid = \"2025-06-01-001\"\nread = false\n+++\n\nbody\n";
        let read = "+++\nid = \"2025-06-02-002\"\nread = true\n+++\n\nbody\n";
        std::fs::write(inbox.join("2025-06-01-001.md"), unread).unwrap();
        std::fs::write(inbox.join("2025-06-02-002.md"), read).unwrap();

        let config = base_config(tmp.path(), "alice");
        let handle = ConfigHandle::new(config);
        let state_ctx = StateContext::new(tmp.path().to_path_buf(), handle);
        let caller = Caller::new(4242, 4242, None);
        let resp = handle_mailbox_list(&state_ctx, &caller).await;
        let body = match resp {
            JsonAckResponse::Ok { body } => body,
            other => panic!("expected Ok, got {other:?}"),
        };
        let rows: Vec<MailboxListRow> = serde_json::from_slice(&body).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].name, "alice");
        assert_eq!(rows[0].total, 2);
        assert_eq!(rows[0].unread, 1);
    }
}
