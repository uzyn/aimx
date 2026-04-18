//! Daemon-side handlers for `AIMX/1` state-mutation verbs.
//!
//! Sprint 45 introduces `MARK-READ` and `MARK-UNREAD`. Both rewrite the
//! target email's TOML frontmatter in place so the `read` field persists
//! across restarts. Files on disk are owned by `root:root 0644` — writable
//! only by the daemon, which runs as root — so the MCP server (running as
//! the invoking non-root user) routes through these verbs instead of
//! touching the files directly.
//!
//! Concurrency model: a per-mailbox `RwLock<()>` guards the "read →
//! rewrite" critical section. The lock is shared with `ingest::INGEST_*`
//! semantics — for now each write takes the write side, which is
//! sufficient because the only other writer to a mailbox is the inbound
//! ingest path. The map is lazily populated so tests that never touch a
//! mailbox never allocate a lock for it.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tokio::sync::RwLock;

use crate::frontmatter::InboundFrontmatter;
use crate::send_protocol::{AckResponse, ErrCode, MarkFolder, MarkRequest};

/// Per-connection shared state for the MARK verbs (and future state-
/// mutation verbs added in Sprint 46+).
pub struct StateContext {
    /// Data directory root — `<data_dir>/inbox/<mailbox>/<id>.md` etc.
    pub data_dir: PathBuf,
    /// Mailboxes known at daemon startup. The MARK handlers gate on
    /// presence here before touching the filesystem so a typo'd mailbox
    /// name produces `ERR MAILBOX` instead of `ERR NOTFOUND` on the file.
    pub mailboxes: std::collections::HashSet<String>,
    /// Per-mailbox lock map. `Mutex` because the map-level mutation
    /// (lazy insert) is short; the per-mailbox `RwLock` handles the
    /// actual file write ordering.
    locks: Mutex<HashMap<String, Arc<RwLock<()>>>>,
}

impl StateContext {
    pub fn new(data_dir: PathBuf, mailboxes: std::collections::HashSet<String>) -> Self {
        Self {
            data_dir,
            mailboxes,
            locks: Mutex::new(HashMap::new()),
        }
    }

    /// Acquire (lazy-inserting if needed) the per-mailbox write lock.
    /// Returned guard is released when dropped.
    fn lock_for(&self, mailbox: &str) -> Arc<RwLock<()>> {
        let mut map = self
            .locks
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        map.entry(mailbox.to_string())
            .or_insert_with(|| Arc::new(RwLock::new(())))
            .clone()
    }
}

/// Validate that the email id is path-traversal-safe. Mirrors the checks
/// in `mcp::validate_email_id` but lives on the daemon side so a malicious
/// local client cannot bypass them.
fn validate_id(id: &str) -> Result<(), AckResponse> {
    if id.is_empty()
        || id.contains("..")
        || id.contains('/')
        || id.contains('\\')
        || id.contains('\0')
    {
        return Err(AckResponse::Err {
            code: ErrCode::NotFound,
            reason: format!("email id '{id}' contains invalid characters"),
        });
    }
    Ok(())
}

/// Resolve the on-disk path for an email id under `<folder>/<mailbox>/`,
/// handling both flat `<id>.md` and Zola bundle `<id>/<id>.md` layouts.
fn resolve_email_path(mailbox_dir: &Path, id: &str) -> Option<PathBuf> {
    let flat = mailbox_dir.join(format!("{id}.md"));
    if flat.exists() {
        return Some(flat);
    }
    let bundle_md = mailbox_dir.join(id).join(format!("{id}.md"));
    if bundle_md.exists() {
        return Some(bundle_md);
    }
    None
}

fn folder_dir(data_dir: &Path, mailbox: &str, folder: MarkFolder) -> PathBuf {
    let sub = match folder {
        MarkFolder::Inbox => "inbox",
        MarkFolder::Sent => "sent",
    };
    data_dir.join(sub).join(mailbox)
}

/// Handle a `MARK-READ` / `MARK-UNREAD` request. Takes the per-mailbox
/// write lock around the read → rewrite critical section so the update is
/// atomic with respect to inbound ingest writes on the same mailbox
/// (inbound takes the same lock shape via `INGEST_WRITE_LOCK` today;
/// moving both to a shared map is tracked as follow-up).
pub async fn handle_mark(ctx: &StateContext, req: &MarkRequest) -> AckResponse {
    if let Err(e) = validate_id(&req.id) {
        return e;
    }

    if !ctx.mailboxes.contains(&req.mailbox) {
        return AckResponse::Err {
            code: ErrCode::Mailbox,
            reason: format!("mailbox '{}' does not exist", req.mailbox),
        };
    }

    let lock = ctx.lock_for(&req.mailbox);
    let _guard = lock.write().await;

    let mailbox_dir = folder_dir(&ctx.data_dir, &req.mailbox, req.folder);
    let filepath = match resolve_email_path(&mailbox_dir, &req.id) {
        Some(p) => p,
        None => {
            return AckResponse::Err {
                code: ErrCode::NotFound,
                reason: format!(
                    "email '{}' not found in {}/{}",
                    req.id,
                    req.folder.as_str(),
                    req.mailbox
                ),
            };
        }
    };

    let content = match std::fs::read_to_string(&filepath) {
        Ok(c) => c,
        Err(e) => {
            return AckResponse::Err {
                code: ErrCode::Io,
                reason: format!("failed to read {}: {e}", filepath.display()),
            };
        }
    };

    let parts: Vec<&str> = content.splitn(3, "+++").collect();
    if parts.len() < 3 {
        return AckResponse::Err {
            code: ErrCode::Io,
            reason: format!("malformed frontmatter in {}", filepath.display()),
        };
    }

    let toml_str = parts[1].trim();
    let mut meta: InboundFrontmatter = match toml::from_str(toml_str) {
        Ok(m) => m,
        Err(e) => {
            return AckResponse::Err {
                code: ErrCode::Io,
                reason: format!("failed to parse frontmatter in {}: {e}", filepath.display()),
            };
        }
    };

    meta.read = req.read;

    let new_toml = match toml::to_string(&meta) {
        Ok(t) => t,
        Err(e) => {
            return AckResponse::Err {
                code: ErrCode::Io,
                reason: format!("failed to serialize frontmatter: {e}"),
            };
        }
    };
    let body = parts[2];

    let mut out = String::new();
    out.push_str("+++\n");
    out.push_str(&new_toml);
    out.push_str("+++");
    out.push_str(body);

    if let Err(e) = std::fs::write(&filepath, out) {
        return AckResponse::Err {
            code: ErrCode::Io,
            reason: format!("failed to write {}: {e}", filepath.display()),
        };
    }

    AckResponse::Ok
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frontmatter::InboundFrontmatter;
    use std::collections::HashSet;
    use tempfile::TempDir;

    fn sample_meta(id: &str, read: bool) -> InboundFrontmatter {
        InboundFrontmatter {
            id: id.to_string(),
            message_id: format!("<{id}@test.com>"),
            thread_id: "0123456789abcdef".to_string(),
            from: "sender@test.com".to_string(),
            to: "alice@test.com".to_string(),
            cc: None,
            reply_to: None,
            delivered_to: "alice@test.com".to_string(),
            subject: "Hello".to_string(),
            date: "2025-06-01T12:00:00Z".to_string(),
            received_at: "2025-06-01T12:00:01Z".to_string(),
            received_from_ip: None,
            size_bytes: 100,
            in_reply_to: None,
            references: None,
            attachments: vec![],
            list_id: None,
            auto_submitted: None,
            dkim: "none".to_string(),
            spf: "none".to_string(),
            dmarc: "none".to_string(),
            trusted: "none".to_string(),
            mailbox: "alice".to_string(),
            read,
            labels: vec![],
        }
    }

    fn write_email(dir: &Path, id: &str, meta: &InboundFrontmatter) {
        std::fs::create_dir_all(dir).unwrap();
        let toml = toml::to_string(meta).unwrap();
        let content = format!("+++\n{toml}+++\n\nbody of {id}\n");
        std::fs::write(dir.join(format!("{id}.md")), content).unwrap();
    }

    fn ctx(data_dir: &Path) -> StateContext {
        let mut boxes = HashSet::new();
        boxes.insert("alice".to_string());
        StateContext::new(data_dir.to_path_buf(), boxes)
    }

    #[tokio::test]
    async fn mark_read_toggles_frontmatter() {
        let tmp = TempDir::new().unwrap();
        let meta = sample_meta("2025-06-01-001", false);
        let inbox = tmp.path().join("inbox").join("alice");
        write_email(&inbox, "2025-06-01-001", &meta);

        let sctx = ctx(tmp.path());
        let req = MarkRequest {
            mailbox: "alice".to_string(),
            id: "2025-06-01-001".to_string(),
            folder: MarkFolder::Inbox,
            read: true,
        };
        match handle_mark(&sctx, &req).await {
            AckResponse::Ok => {}
            other => panic!("expected Ok, got {other:?}"),
        }

        let content = std::fs::read_to_string(inbox.join("2025-06-01-001.md")).unwrap();
        let parts: Vec<&str> = content.splitn(3, "+++").collect();
        let fm: InboundFrontmatter = toml::from_str(parts[1].trim()).unwrap();
        assert!(fm.read);
        // Body preserved
        assert!(parts[2].contains("body of 2025-06-01-001"));
    }

    #[tokio::test]
    async fn mark_unread_after_read() {
        let tmp = TempDir::new().unwrap();
        let meta = sample_meta("2025-06-01-001", true);
        let inbox = tmp.path().join("inbox").join("alice");
        write_email(&inbox, "2025-06-01-001", &meta);

        let sctx = ctx(tmp.path());
        let req = MarkRequest {
            mailbox: "alice".to_string(),
            id: "2025-06-01-001".to_string(),
            folder: MarkFolder::Inbox,
            read: false,
        };
        assert!(matches!(handle_mark(&sctx, &req).await, AckResponse::Ok));

        let content = std::fs::read_to_string(inbox.join("2025-06-01-001.md")).unwrap();
        let fm: InboundFrontmatter =
            toml::from_str(content.split("+++").nth(1).unwrap().trim()).unwrap();
        assert!(!fm.read);
    }

    #[tokio::test]
    async fn mark_unknown_mailbox_errors() {
        let tmp = TempDir::new().unwrap();
        let sctx = ctx(tmp.path());
        let req = MarkRequest {
            mailbox: "ghost".to_string(),
            id: "2025-06-01-001".to_string(),
            folder: MarkFolder::Inbox,
            read: true,
        };
        match handle_mark(&sctx, &req).await {
            AckResponse::Err { code, reason } => {
                assert_eq!(code, ErrCode::Mailbox);
                assert!(reason.contains("ghost"), "{reason}");
            }
            other => panic!("expected Err, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn mark_missing_email_errors() {
        let tmp = TempDir::new().unwrap();
        std::fs::create_dir_all(tmp.path().join("inbox").join("alice")).unwrap();
        let sctx = ctx(tmp.path());
        let req = MarkRequest {
            mailbox: "alice".to_string(),
            id: "2099-01-01-missing".to_string(),
            folder: MarkFolder::Inbox,
            read: true,
        };
        match handle_mark(&sctx, &req).await {
            AckResponse::Err { code, .. } => assert_eq!(code, ErrCode::NotFound),
            other => panic!("expected Err, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn mark_rejects_path_traversal_id() {
        let tmp = TempDir::new().unwrap();
        let sctx = ctx(tmp.path());
        let req = MarkRequest {
            mailbox: "alice".to_string(),
            id: "../../etc/passwd".to_string(),
            folder: MarkFolder::Inbox,
            read: true,
        };
        match handle_mark(&sctx, &req).await {
            AckResponse::Err { code, reason } => {
                assert_eq!(code, ErrCode::NotFound);
                assert!(reason.contains("invalid characters"), "{reason}");
            }
            other => panic!("expected Err, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn mark_read_in_sent_folder() {
        let tmp = TempDir::new().unwrap();
        let meta = sample_meta("2025-06-02-001", false);
        let sent = tmp.path().join("sent").join("alice");
        write_email(&sent, "2025-06-02-001", &meta);

        let sctx = ctx(tmp.path());
        let req = MarkRequest {
            mailbox: "alice".to_string(),
            id: "2025-06-02-001".to_string(),
            folder: MarkFolder::Sent,
            read: true,
        };
        assert!(matches!(handle_mark(&sctx, &req).await, AckResponse::Ok));

        let fm: InboundFrontmatter = toml::from_str(
            std::fs::read_to_string(sent.join("2025-06-02-001.md"))
                .unwrap()
                .split("+++")
                .nth(1)
                .unwrap()
                .trim(),
        )
        .unwrap();
        assert!(fm.read);
    }

    #[tokio::test]
    async fn mark_read_in_bundle_layout() {
        // Bundle form: `<mailbox>/<id>/<id>.md`.
        let tmp = TempDir::new().unwrap();
        let mailbox = tmp.path().join("inbox").join("alice");
        let bundle = mailbox.join("2025-06-03-001");
        std::fs::create_dir_all(&bundle).unwrap();
        let meta = sample_meta("2025-06-03-001", false);
        let toml = toml::to_string(&meta).unwrap();
        let content = format!("+++\n{toml}+++\n\nbody\n");
        std::fs::write(bundle.join("2025-06-03-001.md"), content).unwrap();

        let sctx = ctx(tmp.path());
        let req = MarkRequest {
            mailbox: "alice".to_string(),
            id: "2025-06-03-001".to_string(),
            folder: MarkFolder::Inbox,
            read: true,
        };
        assert!(matches!(handle_mark(&sctx, &req).await, AckResponse::Ok));

        let content = std::fs::read_to_string(bundle.join("2025-06-03-001.md")).unwrap();
        let fm: InboundFrontmatter =
            toml::from_str(content.split("+++").nth(1).unwrap().trim()).unwrap();
        assert!(fm.read);
    }
}
