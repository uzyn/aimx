//! Daemon-side handlers for the `MAILBOX-CREATE` and `MAILBOX-DELETE`
//! verbs of the `AIMX/1` UDS protocol.
//!
//! `aimx mailbox create` / `delete` route through the daemon over UDS so
//! the in-memory `Config` is hot-swapped under a `RwLock<Arc<Config>>`
//! whenever `config.toml` on disk changes. Inbound mail to a just-created
//! mailbox routes correctly on the very next SMTP session — no restart,
//! no "silent fall through to catchall" surprise.
//!
//! Correctness model:
//!
//! 1. Validate the mailbox name using the same `mailbox::validate_mailbox_name`
//!    rules every caller already trusts.
//! 2. Load the *current* `Config` snapshot through the shared
//!    [`ConfigHandle`]. Re-derive the new snapshot in memory (either
//!    inserting or removing the stanza). Fail fast on duplicates /
//!    missing / attempts to delete catchall / non-empty directories.
//! 3. Write the new `Config` to disk **atomically** via
//!    `write-temp-then-rename` — either we see the old snapshot or the
//!    new one, never a partial write.
//! 4. Only after the rename succeeds, swap the in-memory `Config` via
//!    [`ConfigHandle::store`]. This ordering matters: if we swapped
//!    first and the rename failed, we would be handing inbound mail a
//!    routing table that doesn't match disk.
//!
//! Locking: acquired in the order the
//! [`crate::mailbox_locks`] module documents. The **outer** per-mailbox
//! `tokio::sync::Mutex<()>` (shared with inbound ingest and MARK-*) is
//! taken for the duration of the write so no other writer on the same
//! mailbox can see the file system half-created / half-deleted. The
//! **inner** process-wide [`CONFIG_WRITE_LOCK`] serializes the
//! `load → modify → write → store` critical section across *all* mailbox
//! names — without it, two concurrent `MAILBOX-CREATE alice` +
//! `MAILBOX-CREATE bob` requests hold disjoint per-mailbox locks and can
//! interleave their loads + writes, clobbering one stanza on disk and in
//! memory. Always outer → inner.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::config::{Config, ConfigHandle, MailboxConfig};
use crate::send_protocol::{AckResponse, ErrCode, MailboxCrudRequest};
use crate::state_handler::StateContext;

/// Process-wide mutex around the `config.toml` read-modify-write critical
/// section. Symmetric in spirit to `send_handler::SENT_WRITE_LOCK`: a
/// single short-lived `std::sync::Mutex` that serializes *only* the
/// load-modify-write-store sequence so two concurrent
/// `MAILBOX-CREATE`/`DELETE` requests on different mailbox names cannot
/// clobber each other. Held only for the duration of the critical
/// section (not across the full request lifetime).
static CONFIG_WRITE_LOCK: Mutex<()> = Mutex::new(());

/// Shared context for MAILBOX-CREATE / MAILBOX-DELETE. Kept separate from
/// `StateContext` so the state-lock map is not accidentally bypassed (the
/// handler calls through `state_ctx.lock_for(...)` before touching disk).
pub struct MailboxContext {
    /// Location of `config.toml` on disk. Injected rather than taken from
    /// `crate::config::config_path()` so tests can point at a tempdir
    /// without mutating the process-wide `AIMX_CONFIG_DIR` env var.
    pub config_path: PathBuf,
    /// Live handle to the daemon's `Config`. Written through on success.
    pub config_handle: ConfigHandle,
}

impl MailboxContext {
    pub fn new(config_path: PathBuf, config_handle: ConfigHandle) -> Self {
        Self {
            config_path,
            config_handle,
        }
    }
}

/// Dispatch a `MAILBOX-CREATE` or `MAILBOX-DELETE` request to the matching
/// handler. Holds the same per-mailbox lock the MARK-* path takes so a
/// concurrent MARK on the same mailbox cannot observe a half-created or
/// half-deleted file system.
pub async fn handle_mailbox_crud(
    state_ctx: &StateContext,
    mb_ctx: &MailboxContext,
    req: &MailboxCrudRequest,
) -> AckResponse {
    if let Err(e) = crate::mailbox::validate_mailbox_name(&req.name) {
        return AckResponse::Err {
            code: ErrCode::Validation,
            reason: e,
        };
    }

    let lock = state_ctx.lock_for(&req.name);
    let _guard = lock.lock().await;

    // Serialize the load → modify → write → store critical section across
    // *all* mailbox names. The per-mailbox lock above keeps MARK-* on the
    // same mailbox from racing; this global lock keeps
    // `MAILBOX-CREATE alice` + `MAILBOX-CREATE bob` from losing one of
    // the two stanzas via interleaved load-write-store sequences.
    let _config_guard = CONFIG_WRITE_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());

    if req.create {
        handle_create(state_ctx, mb_ctx, &req.name)
    } else {
        handle_delete(state_ctx, mb_ctx, &req.name)
    }
}

fn handle_create(state_ctx: &StateContext, mb_ctx: &MailboxContext, name: &str) -> AckResponse {
    let current = mb_ctx.config_handle.load();

    if current.mailboxes.contains_key(name) {
        return AckResponse::Err {
            code: ErrCode::Mailbox,
            reason: format!("mailbox '{name}' already exists"),
        };
    }

    let data_dir = &state_ctx.data_dir;
    let inbox = data_dir.join("inbox").join(name);
    let sent = data_dir.join("sent").join(name);

    if let Err(e) = std::fs::create_dir_all(&inbox) {
        return AckResponse::Err {
            code: ErrCode::Io,
            reason: format!("failed to create {}: {e}", inbox.display()),
        };
    }
    if let Err(e) = std::fs::create_dir_all(&sent) {
        // Roll back the inbox dir so we never leave half a mailbox behind.
        let _ = std::fs::remove_dir(&inbox);
        return AckResponse::Err {
            code: ErrCode::Io,
            reason: format!("failed to create {}: {e}", sent.display()),
        };
    }

    // Build the new Config with the mailbox stanza inserted.
    let mut new_config: Config = (*current).clone();
    let address = format!("{name}@{}", new_config.domain);
    new_config.mailboxes.insert(
        name.to_string(),
        MailboxConfig {
            address,
            on_receive: vec![],
            trust: "none".to_string(),
            trusted_senders: vec![],
        },
    );

    if let Err(e) = write_config_atomic(&mb_ctx.config_path, &new_config) {
        // Rename failed — leave the in-memory Config untouched so the
        // daemon continues running against the pre-call state on both
        // disk and memory. Best-effort clean up of the freshly-created
        // directories so the operator can retry cleanly.
        let _ = std::fs::remove_dir(&inbox);
        let _ = std::fs::remove_dir(&sent);
        return AckResponse::Err {
            code: ErrCode::Io,
            reason: format!("failed to write {}: {e}", mb_ctx.config_path.display()),
        };
    }

    mb_ctx.config_handle.store(new_config);
    AckResponse::Ok
}

fn handle_delete(state_ctx: &StateContext, mb_ctx: &MailboxContext, name: &str) -> AckResponse {
    if name == "catchall" {
        return AckResponse::Err {
            code: ErrCode::Mailbox,
            reason: "cannot delete the catchall mailbox".into(),
        };
    }

    let current = mb_ctx.config_handle.load();
    if !current.mailboxes.contains_key(name) {
        return AckResponse::Err {
            code: ErrCode::Mailbox,
            reason: format!("mailbox '{name}' does not exist"),
        };
    }

    let data_dir = &state_ctx.data_dir;
    let inbox = data_dir.join("inbox").join(name);
    let sent = data_dir.join("sent").join(name);

    let inbox_files = count_files_if_exists(&inbox);
    let sent_files = count_files_if_exists(&sent);
    if inbox_files + sent_files > 0 {
        return AckResponse::Err {
            code: ErrCode::NonEmpty,
            reason: format!(
                "mailbox '{name}' has {} files ({} in inbox, {} in sent); \
                 archive or remove them first",
                inbox_files + sent_files,
                inbox_files,
                sent_files
            ),
        };
    }

    let mut new_config: Config = (*current).clone();
    new_config.mailboxes.remove(name);

    if let Err(e) = write_config_atomic(&mb_ctx.config_path, &new_config) {
        return AckResponse::Err {
            code: ErrCode::Io,
            reason: format!("failed to write {}: {e}", mb_ctx.config_path.display()),
        };
    }

    mb_ctx.config_handle.store(new_config);
    // Directories are left on disk (operator owns cleanup). The non-empty
    // check above guarantees they're empty, so the operator only ever has
    // to `rmdir` empty directories if they want to tidy up.
    AckResponse::Ok
}

/// Count the entries directly inside `dir`. Treats missing directory as
/// zero files. Does not recurse — the mailbox layout only writes at the
/// top level (flat `.md` files) and one level deep (bundle directories);
/// a non-empty bundle still trips the top-level count.
fn count_files_if_exists(dir: &Path) -> usize {
    match std::fs::read_dir(dir) {
        Ok(entries) => entries.filter_map(|e| e.ok()).count(),
        Err(_) => 0,
    }
}

/// Atomically replace `path` with the TOML serialization of `config`.
///
/// Writes to a sibling `<path>.tmp.<pid>` file first, syncs it, then
/// renames over the target. On POSIX `rename(2)` is atomic for same-
/// filesystem targets, so readers see either the old snapshot or the new
/// one — never a truncated file. On failure the temp file is cleaned up
/// best-effort so subsequent retries don't trip over stale state.
///
/// **Unknown-key / comment behaviour (v1):** this function re-serializes
/// the `Config` struct through `toml::to_string_pretty`, which means any
/// TOML fields the operator added that are **not** modeled in the `Config`
/// struct are dropped on rewrite, and any human-authored comments are
/// erased. This is symmetric with the pre-existing `Config::save` path
/// and matches the v1 assumption that `config.toml` is machine-authored
/// (the only supported edits are through `aimx setup` / `aimx mailbox
/// create|delete`). Adopting a preserving editor (e.g. `toml_edit`) is
/// tracked for v2; see the test `unknown_stanza_is_dropped_on_rewrite`
/// below for the contract check.
pub(crate) fn write_config_atomic(path: &Path, config: &Config) -> std::io::Result<()> {
    let serialized = toml::to_string_pretty(config)
        .map_err(|e| std::io::Error::other(format!("toml serialize: {e}")))?;

    let parent = path.parent().unwrap_or(Path::new("."));
    let file_name = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("config.toml");
    let tmp_name = format!(".{file_name}.tmp.{}", std::process::id());
    let tmp_path = parent.join(tmp_name);

    // Scope the file handle so it closes before rename (paranoia on
    // platforms where an open handle can block a rename).
    {
        let mut f = std::fs::File::create(&tmp_path)?;
        f.write_all(serialized.as_bytes())?;
        f.sync_all()?;
    }

    if let Err(e) = std::fs::rename(&tmp_path, path) {
        let _ = std::fs::remove_file(&tmp_path);
        return Err(e);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, MailboxConfig};
    use std::collections::HashMap;
    use tempfile::TempDir;

    fn base_config(data_dir: &Path) -> Config {
        let mut mailboxes = HashMap::new();
        mailboxes.insert(
            "catchall".to_string(),
            MailboxConfig {
                address: "*@example.com".to_string(),
                on_receive: vec![],
                trust: "none".to_string(),
                trusted_senders: vec![],
            },
        );
        Config {
            domain: "example.com".to_string(),
            data_dir: data_dir.to_path_buf(),
            dkim_selector: "dkim".to_string(),
            mailboxes,
            verify_host: None,
            enable_ipv6: false,
        }
    }

    fn contexts(tmp: &TempDir) -> (StateContext, MailboxContext) {
        let config = base_config(tmp.path());
        let handle = ConfigHandle::new(config);
        let state_ctx = StateContext::new(tmp.path().to_path_buf(), handle.clone());
        let config_path = tmp.path().join("config.toml");
        // Seed the on-disk file so write-temp-then-rename has a real
        // target to replace.
        write_config_atomic(&config_path, &handle.load()).unwrap();
        let mb_ctx = MailboxContext::new(config_path, handle);
        (state_ctx, mb_ctx)
    }

    #[tokio::test]
    async fn create_mailbox_creates_dirs_writes_config_swaps_handle() {
        let tmp = TempDir::new().unwrap();
        let (state_ctx, mb_ctx) = contexts(&tmp);

        let req = MailboxCrudRequest {
            name: "alice".into(),
            create: true,
        };
        assert!(matches!(
            handle_mailbox_crud(&state_ctx, &mb_ctx, &req).await,
            AckResponse::Ok
        ));

        // Dirs exist
        assert!(tmp.path().join("inbox").join("alice").is_dir());
        assert!(tmp.path().join("sent").join("alice").is_dir());

        // config.toml reloads the new mailbox
        let reloaded = Config::load(&mb_ctx.config_path).unwrap();
        assert!(reloaded.mailboxes.contains_key("alice"));
        assert_eq!(reloaded.mailboxes["alice"].address, "alice@example.com");

        // In-memory handle reflects the swap immediately.
        assert!(mb_ctx.config_handle.load().mailboxes.contains_key("alice"));
    }

    #[tokio::test]
    async fn create_duplicate_mailbox_returns_mailbox_error() {
        let tmp = TempDir::new().unwrap();
        let (state_ctx, mb_ctx) = contexts(&tmp);

        let req = MailboxCrudRequest {
            name: "catchall".into(),
            create: true,
        };
        match handle_mailbox_crud(&state_ctx, &mb_ctx, &req).await {
            AckResponse::Err { code, reason } => {
                assert_eq!(code, ErrCode::Mailbox);
                assert!(reason.contains("already exists"), "{reason}");
            }
            other => panic!("expected Err(MAILBOX), got {other:?}"),
        }
    }

    #[tokio::test]
    async fn create_rejects_invalid_name_empty() {
        let tmp = TempDir::new().unwrap();
        let (state_ctx, mb_ctx) = contexts(&tmp);

        let req = MailboxCrudRequest {
            name: "".into(),
            create: true,
        };
        match handle_mailbox_crud(&state_ctx, &mb_ctx, &req).await {
            AckResponse::Err { code, .. } => assert_eq!(code, ErrCode::Validation),
            other => panic!("expected Err(VALIDATION), got {other:?}"),
        }
    }

    #[tokio::test]
    async fn create_rejects_invalid_name_path_traversal() {
        let tmp = TempDir::new().unwrap();
        let (state_ctx, mb_ctx) = contexts(&tmp);

        for bad in ["../etc", "foo/bar", "foo\\bar", "a\0b"] {
            let req = MailboxCrudRequest {
                name: bad.into(),
                create: true,
            };
            match handle_mailbox_crud(&state_ctx, &mb_ctx, &req).await {
                AckResponse::Err { code, .. } => {
                    assert_eq!(code, ErrCode::Validation, "name {bad:?}")
                }
                other => panic!("{bad:?}: expected Err(VALIDATION), got {other:?}"),
            }
        }
    }

    #[tokio::test]
    async fn delete_mailbox_removes_stanza_and_swaps_handle() {
        let tmp = TempDir::new().unwrap();
        let (state_ctx, mb_ctx) = contexts(&tmp);

        // Pre-create
        let req = MailboxCrudRequest {
            name: "alice".into(),
            create: true,
        };
        assert!(matches!(
            handle_mailbox_crud(&state_ctx, &mb_ctx, &req).await,
            AckResponse::Ok
        ));

        // Delete with empty dirs — should succeed.
        let req = MailboxCrudRequest {
            name: "alice".into(),
            create: false,
        };
        assert!(matches!(
            handle_mailbox_crud(&state_ctx, &mb_ctx, &req).await,
            AckResponse::Ok
        ));

        let reloaded = Config::load(&mb_ctx.config_path).unwrap();
        assert!(!reloaded.mailboxes.contains_key("alice"));
        assert!(!mb_ctx.config_handle.load().mailboxes.contains_key("alice"));
    }

    #[tokio::test]
    async fn delete_refuses_nonempty_mailbox() {
        let tmp = TempDir::new().unwrap();
        let (state_ctx, mb_ctx) = contexts(&tmp);

        // Create
        let req = MailboxCrudRequest {
            name: "alice".into(),
            create: true,
        };
        assert!(matches!(
            handle_mailbox_crud(&state_ctx, &mb_ctx, &req).await,
            AckResponse::Ok
        ));

        // Write a stray file into inbox
        let inbox = tmp.path().join("inbox").join("alice");
        std::fs::write(inbox.join("2025-06-01-001.md"), "content").unwrap();

        // Delete must be refused
        let req = MailboxCrudRequest {
            name: "alice".into(),
            create: false,
        };
        match handle_mailbox_crud(&state_ctx, &mb_ctx, &req).await {
            AckResponse::Err { code, reason } => {
                assert_eq!(code, ErrCode::NonEmpty);
                assert!(
                    reason.contains("alice") && reason.contains("1 files"),
                    "{reason}"
                );
            }
            other => panic!("expected Err(NONEMPTY), got {other:?}"),
        }

        // Stanza still present
        assert!(mb_ctx.config_handle.load().mailboxes.contains_key("alice"));
    }

    #[tokio::test]
    async fn delete_catchall_forbidden() {
        let tmp = TempDir::new().unwrap();
        let (state_ctx, mb_ctx) = contexts(&tmp);

        let req = MailboxCrudRequest {
            name: "catchall".into(),
            create: false,
        };
        match handle_mailbox_crud(&state_ctx, &mb_ctx, &req).await {
            AckResponse::Err { code, reason } => {
                assert_eq!(code, ErrCode::Mailbox);
                assert!(reason.contains("catchall"), "{reason}");
            }
            other => panic!("expected Err(MAILBOX), got {other:?}"),
        }
    }

    #[tokio::test]
    async fn delete_nonexistent_returns_mailbox_error() {
        let tmp = TempDir::new().unwrap();
        let (state_ctx, mb_ctx) = contexts(&tmp);

        let req = MailboxCrudRequest {
            name: "ghost".into(),
            create: false,
        };
        match handle_mailbox_crud(&state_ctx, &mb_ctx, &req).await {
            AckResponse::Err { code, reason } => {
                assert_eq!(code, ErrCode::Mailbox);
                assert!(reason.contains("does not exist"), "{reason}");
            }
            other => panic!("expected Err(MAILBOX), got {other:?}"),
        }
    }

    #[tokio::test]
    async fn create_failure_at_disk_write_leaves_handle_and_disk_unchanged() {
        // S47-3: this test used to force failure by pointing config_path
        // at a non-existent parent directory — which tripped
        // `File::create` before the temp write even started, so the
        // rename-rollback branch was never exercised. The rewritten form
        // below makes the temp write succeed (parent is writable) and the
        // subsequent `rename(2)` fail because the target path is a
        // **non-empty directory**. `rename("foo.tmp", "config.toml/")`
        // with a non-empty directory as the target returns `EISDIR` /
        // `ENOTEMPTY` on Linux + macOS, which is what the real failure
        // mode looks like in production (e.g. a filesystem error mid-rename).
        let tmp = TempDir::new().unwrap();
        let (state_ctx, mb_ctx) = contexts(&tmp);

        // `config.toml` as a non-empty *directory* — the temp write to
        // `.config.toml.tmp.<pid>` succeeds, then rename-over fails.
        let dir_path = tmp.path().join("configdir").join("config.toml");
        std::fs::create_dir_all(&dir_path).unwrap();
        // Put a file inside so the directory is non-empty (rename semantics
        // over an empty directory differ on some kernels).
        std::fs::write(dir_path.join("blocker"), "x").unwrap();
        let bad_ctx = MailboxContext::new(dir_path.clone(), mb_ctx.config_handle.clone());

        let req = MailboxCrudRequest {
            name: "alice".into(),
            create: true,
        };
        match handle_mailbox_crud(&state_ctx, &bad_ctx, &req).await {
            AckResponse::Err { code, .. } => assert_eq!(code, ErrCode::Io),
            other => panic!("expected Err(IO), got {other:?}"),
        }

        // Config stays unchanged on both disk and memory.
        assert!(!bad_ctx.config_handle.load().mailboxes.contains_key("alice"));
        // Rollback cleaned up the freshly-created dirs so we don't leave
        // half a mailbox behind.
        assert!(!tmp.path().join("inbox").join("alice").exists());
        assert!(!tmp.path().join("sent").join("alice").exists());

        // No `.config.toml.tmp.<pid>` was left behind — the rollback path
        // cleans up the temp file after a rename failure.
        let parent = dir_path.parent().unwrap();
        let strays: Vec<_> = std::fs::read_dir(parent)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| n.contains(".tmp."))
            .collect();
        assert!(
            strays.is_empty(),
            "no stray temp file should remain after rename failure: {strays:?}"
        );
    }

    #[tokio::test]
    async fn unknown_stanza_is_dropped_on_rewrite() {
        // S47-3: this pins the **documented** v1 behaviour of
        // `write_config_atomic`. The rewrite goes through
        // `toml::to_string_pretty(&Config)`, so any stanza the operator
        // added that isn't modeled in `Config` is not preserved. If this
        // test starts failing because the behaviour changed (e.g. we
        // adopted `toml_edit`), update the doc comment on
        // `write_config_atomic` and flip this assertion accordingly.
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("config.toml");

        let base = base_config(tmp.path());
        let serialized = toml::to_string_pretty(&base).unwrap();
        let with_unknown = format!(
            "# operator-added comment\n\
             {serialized}\n\
             [experimental.extra]\n\
             opaque_value = 42\n"
        );
        std::fs::write(&path, with_unknown).unwrap();

        // Rewrite through the canonical path.
        write_config_atomic(&path, &base).unwrap();

        let after = std::fs::read_to_string(&path).unwrap();
        assert!(
            !after.contains("operator-added comment"),
            "v1 documented behaviour: rewrite erases human comments. Got: {after:?}"
        );
        assert!(
            !after.contains("experimental"),
            "v1 documented behaviour: rewrite drops unknown stanzas. Got: {after:?}"
        );
    }

    #[tokio::test]
    async fn write_config_atomic_replaces_existing_file() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("config.toml");
        let c1 = base_config(tmp.path());
        write_config_atomic(&path, &c1).unwrap();
        assert!(path.exists());

        // Overwrite with a different Config
        let mut c2 = c1.clone();
        c2.mailboxes.insert(
            "alice".to_string(),
            MailboxConfig {
                address: "alice@example.com".into(),
                on_receive: vec![],
                trust: "none".into(),
                trusted_senders: vec![],
            },
        );
        write_config_atomic(&path, &c2).unwrap();
        let reloaded = Config::load(&path).unwrap();
        assert!(reloaded.mailboxes.contains_key("alice"));

        // No `.tmp.*` file left behind
        let entries: Vec<_> = std::fs::read_dir(tmp.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();
        let has_tmp = entries.iter().any(|n| n.contains(".tmp."));
        assert!(
            !has_tmp,
            "no stray tmp file left after atomic rename: {entries:?}"
        );
    }

    #[tokio::test]
    async fn mailbox_crud_create_then_mark_works_on_same_mailbox() {
        // Verify the `MAILBOX-CREATE` → `MARK-READ` chain works in-process
        // without a restart: the new mailbox is routable by MARK-*.
        let tmp = TempDir::new().unwrap();
        let (state_ctx, mb_ctx) = contexts(&tmp);

        let req = MailboxCrudRequest {
            name: "alice".into(),
            create: true,
        };
        assert!(matches!(
            handle_mailbox_crud(&state_ctx, &mb_ctx, &req).await,
            AckResponse::Ok
        ));

        // Drop a pre-existing email into the newly-created inbox and
        // mark it read via the same StateContext.
        use crate::frontmatter::InboundFrontmatter;
        let meta = InboundFrontmatter {
            id: "2025-06-01-001".into(),
            message_id: "<msg@example.com>".into(),
            thread_id: "0123456789abcdef".into(),
            from: "sender@example.com".into(),
            to: "alice@example.com".into(),
            cc: None,
            reply_to: None,
            delivered_to: "alice@example.com".into(),
            subject: "Hi".into(),
            date: "2025-06-01T12:00:00Z".into(),
            received_at: "2025-06-01T12:00:01Z".into(),
            received_from_ip: None,
            size_bytes: 10,
            in_reply_to: None,
            references: None,
            attachments: vec![],
            list_id: None,
            auto_submitted: None,
            dkim: "none".into(),
            spf: "none".into(),
            dmarc: "none".into(),
            trusted: "none".into(),
            mailbox: "alice".into(),
            read: false,
            labels: vec![],
        };
        let inbox = tmp.path().join("inbox").join("alice");
        let toml = toml::to_string(&meta).unwrap();
        let content = format!("+++\n{toml}+++\n\nbody\n");
        std::fs::write(inbox.join("2025-06-01-001.md"), content).unwrap();

        let mark_req = crate::send_protocol::MarkRequest {
            mailbox: "alice".into(),
            id: "2025-06-01-001".into(),
            folder: crate::send_protocol::MarkFolder::Inbox,
            read: true,
        };
        let resp = crate::state_handler::handle_mark(&state_ctx, &mark_req).await;
        assert!(matches!(resp, AckResponse::Ok), "{resp:?}");

        let reread = std::fs::read_to_string(inbox.join("2025-06-01-001.md")).unwrap();
        assert!(reread.contains("read = true"));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_create_different_names_both_stanzas_survive() {
        // Regression test for the lost-update race on `config.toml`:
        // two concurrent `MAILBOX-CREATE` calls on *different* names
        // held disjoint per-mailbox locks and could interleave their
        // load-modify-write sequences, clobbering one stanza. The
        // process-wide `CONFIG_WRITE_LOCK` closes the race — every
        // stanza must survive on disk and in the live handle.
        //
        // We fan out to many concurrent names to give the scheduler a
        // realistic chance of interleaving without the lock — a
        // single-pair test is too easy for tokio's multi-thread runtime
        // to serialize accidentally.
        let tmp = TempDir::new().unwrap();
        let (state_ctx, mb_ctx) = contexts(&tmp);
        let state_ctx = std::sync::Arc::new(state_ctx);
        let mb_ctx = std::sync::Arc::new(mb_ctx);

        let names: Vec<String> = (0..16).map(|i| format!("mbox{i:02}")).collect();
        let mut handles = Vec::with_capacity(names.len());
        for name in &names {
            let s = state_ctx.clone();
            let m = mb_ctx.clone();
            let n = name.clone();
            handles.push(tokio::spawn(async move {
                let req = MailboxCrudRequest {
                    name: n,
                    create: true,
                };
                handle_mailbox_crud(&s, &m, &req).await
            }));
        }
        for h in handles {
            assert!(matches!(h.await.unwrap(), AckResponse::Ok));
        }

        // Every stanza survives on disk.
        let reloaded = Config::load(&mb_ctx.config_path).unwrap();
        for name in &names {
            assert!(
                reloaded.mailboxes.contains_key(name),
                "{name} missing on disk: {:?}",
                reloaded.mailboxes.keys().collect::<Vec<_>>()
            );
        }

        // Every stanza survives in the live handle.
        let in_mem = mb_ctx.config_handle.load();
        for name in &names {
            assert!(
                in_mem.mailboxes.contains_key(name),
                "{name} missing in handle: {:?}",
                in_mem.mailboxes.keys().collect::<Vec<_>>()
            );
        }

        // Every directory pair exists.
        for name in &names {
            assert!(tmp.path().join("inbox").join(name).is_dir());
            assert!(tmp.path().join("sent").join(name).is_dir());
        }
    }
}
