//! Daemon-side handlers for `AIMX/1` state-mutation verbs.
//!
//! `MARK-READ` and `MARK-UNREAD` rewrite the target email's TOML
//! frontmatter in place so the `read` field persists across restarts.
//! Files on disk are owned by `root:root 0644` — writable only by the
//! daemon, which runs as root — so the MCP server (running as the
//! invoking non-root user) routes through these verbs instead of
//! touching the files directly.
//!
//! # Concurrency model
//!
//! MARK-*, inbound ingest, and MAILBOX-* all acquire the **same**
//! per-mailbox `tokio::sync::Mutex<()>` from the shared
//! [`crate::mailbox_locks::MailboxLocks`] map before touching any file
//! under that mailbox's tree.
//!
//! Lock hierarchy (see [`crate::mailbox_locks`] for the full rationale):
//!
//! ```text
//!   outer: per-mailbox mailbox_locks::MailboxLocks  (tokio::sync::Mutex)
//!   inner: process-wide mailbox_handler::CONFIG_WRITE_LOCK (std::sync::Mutex)
//! ```
//!
//! Always acquire outer → inner, never the reverse.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::Mutex as AsyncMutex;

use crate::config::ConfigHandle;
use crate::frontmatter::InboundFrontmatter;
use crate::mailbox_locks::MailboxLocks;
use crate::mcp::resolve_email_path;
use crate::send_protocol::{AckResponse, ErrCode, MarkFolder, MarkRequest};

/// Per-connection shared state for the MARK verbs (and the MAILBOX-CRUD
/// verbs, which share the per-mailbox lock map so their config.toml
/// rewrite does not race with an in-flight MARK).
pub struct StateContext {
    /// Data directory root — `<data_dir>/inbox/<mailbox>/<id>.md` etc.
    ///
    /// Invariant: `data_dir == config_handle.load().data_dir` for the
    /// life of the daemon. `data_dir` is captured once at startup and
    /// never changes; the `Config` swap path (MAILBOX-CRUD in
    /// `mailbox_handler.rs`) deliberately never rewrites `data_dir`, so
    /// this snapshot and the live handle's `data_dir` cannot diverge in
    /// practice.
    pub data_dir: PathBuf,
    /// Live handle to the daemon's `Config`. MARK-* uses it to validate
    /// that the referenced mailbox exists at the time of the call (rather
    /// than at startup); MAILBOX-* uses it to append / remove stanzas and
    /// hot-swap the in-memory snapshot.
    pub config_handle: ConfigHandle,
    /// Shared per-mailbox write-lock map. Inbound ingest, MARK-*, and
    /// MAILBOX-* all serialize through these locks (see
    /// [`crate::mailbox_locks`]).
    pub locks: Arc<MailboxLocks>,
}

impl StateContext {
    /// Fresh lock map — convenient for tests. Production callers share
    /// the daemon-wide map via [`StateContext::with_locks`].
    #[cfg(test)]
    pub fn new(data_dir: PathBuf, config_handle: ConfigHandle) -> Self {
        Self::with_locks(data_dir, config_handle, Arc::new(MailboxLocks::new()))
    }

    /// Construct a `StateContext` that shares an existing
    /// [`MailboxLocks`] map with other daemon-side contexts. Used by
    /// `run_serve` so the SMTP session (inbound ingest) and the UDS
    /// handlers all take the same lock per mailbox.
    pub fn with_locks(
        data_dir: PathBuf,
        config_handle: ConfigHandle,
        locks: Arc<MailboxLocks>,
    ) -> Self {
        Self {
            data_dir,
            config_handle,
            locks,
        }
    }

    /// Acquire (lazy-inserting if needed) the per-mailbox write lock.
    /// Returned `Arc` owns the lock; callers `.lock().await` (async) or
    /// `.blocking_lock()` (inside `spawn_blocking`) it.
    pub(crate) fn lock_for(&self, mailbox: &str) -> Arc<AsyncMutex<()>> {
        self.locks.lock_for(mailbox)
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

fn folder_dir(data_dir: &Path, mailbox: &str, folder: MarkFolder) -> PathBuf {
    let sub = match folder {
        MarkFolder::Inbox => "inbox",
        MarkFolder::Sent => "sent",
    };
    data_dir.join(sub).join(mailbox)
}

/// Handle a `MARK-READ` / `MARK-UNREAD` request. Takes the shared
/// per-mailbox write lock (see [`crate::mailbox_locks`]) for the full
/// read → rewrite critical section. The lock is shared with the
/// inbound-ingest writer so MARK and ingest serialize against each
/// other — not just against other MARK calls.
pub async fn handle_mark(ctx: &StateContext, req: &MarkRequest) -> AckResponse {
    if let Err(e) = validate_id(&req.id) {
        return e;
    }

    // Mailbox existence is resolved live through the handle so a
    // freshly-created mailbox is immediately target-able from MARK.
    if !ctx
        .config_handle
        .load()
        .mailboxes
        .contains_key(&req.mailbox)
    {
        return AckResponse::Err {
            code: ErrCode::Mailbox,
            reason: format!("mailbox '{}' does not exist", req.mailbox),
        };
    }

    let lock = ctx.lock_for(&req.mailbox);
    let _guard = lock.lock().await;

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
        let mut mailboxes = std::collections::HashMap::new();
        mailboxes.insert(
            "alice".to_string(),
            crate::config::MailboxConfig {
                address: "alice@example.com".to_string(),
                on_receive: vec![],
                trust: None,
                trusted_senders: None,
            },
        );
        let config = crate::config::Config {
            domain: "example.com".to_string(),
            data_dir: data_dir.to_path_buf(),
            dkim_selector: "dkim".to_string(),
            trust: "none".to_string(),
            trusted_senders: vec![],
            mailboxes,
            verify_host: None,
            enable_ipv6: false,
        };
        StateContext::new(data_dir.to_path_buf(), ConfigHandle::new(config))
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

    #[tokio::test]
    async fn mark_rejects_truncated_frontmatter_as_io_error() {
        // Target file exists but has only one `+++` delimiter so the
        // splitn(3) produces fewer than 3 parts. Handler must surface
        // this as `ErrCode::Io` rather than panicking or silently
        // rewriting garbage.
        let tmp = TempDir::new().unwrap();
        let inbox = tmp.path().join("inbox").join("alice");
        std::fs::create_dir_all(&inbox).unwrap();
        std::fs::write(
            inbox.join("2025-06-01-002.md"),
            "+++\nid = \"2025-06-01-002\"\nno closing delimiter here\n",
        )
        .unwrap();

        let sctx = ctx(tmp.path());
        let req = MarkRequest {
            mailbox: "alice".to_string(),
            id: "2025-06-01-002".to_string(),
            folder: MarkFolder::Inbox,
            read: true,
        };
        match handle_mark(&sctx, &req).await {
            AckResponse::Err { code, reason } => {
                assert_eq!(code, ErrCode::Io);
                assert!(
                    reason.contains("malformed frontmatter"),
                    "expected 'malformed frontmatter' in reason, got: {reason}"
                );
            }
            other => panic!("expected Err(Io), got {other:?}"),
        }
    }

    #[tokio::test]
    async fn mark_rejects_unparseable_frontmatter_toml_as_io_error() {
        // File has the `+++` delimiters but the TOML body is malformed.
        // Handler must return `ErrCode::Io` (read-failure-after-lock
        // contract).
        let tmp = TempDir::new().unwrap();
        let inbox = tmp.path().join("inbox").join("alice");
        std::fs::create_dir_all(&inbox).unwrap();
        std::fs::write(
            inbox.join("2025-06-01-003.md"),
            "+++\nthis is = not valid = toml\n+++\n\nbody\n",
        )
        .unwrap();

        let sctx = ctx(tmp.path());
        let req = MarkRequest {
            mailbox: "alice".to_string(),
            id: "2025-06-01-003".to_string(),
            folder: MarkFolder::Inbox,
            read: true,
        };
        match handle_mark(&sctx, &req).await {
            AckResponse::Err { code, .. } => assert_eq!(code, ErrCode::Io),
            other => panic!("expected Err(Io), got {other:?}"),
        }
    }
}
