//! Per-mailbox ownership helper used by every daemon-side write path.
//!
//! Sprint 2 of the agent-integration track (PRD §6.3) requires every file
//! and directory the daemon creates under `<data_dir>/{inbox,sent}/<mailbox>/`
//! to land `<owner>:<owner>` with mode `0600` (files) or `0700`
//! (directories) so that only the owning Linux user (and root) can read
//! the mailbox. The daemon itself stays root; the privilege drop lives in
//! the hook child processes, not the daemon.
//!
//! This module exposes a single helper [`chown_as_owner`] that:
//!
//! 1. Resolves the mailbox owner's uid+gid via [`MailboxConfig::owner_uid`]
//!    / [`MailboxConfig::owner_gid`] (which loop through the Sprint-1
//!    `validate_run_as` helper and the pluggable [`user_resolver`] seam).
//! 2. Calls `chown(2)` via `libc::chown` to apply ownership.
//! 3. Calls `chmod(2)` via `libc::chmod` to set the caller-supplied mode
//!    explicitly, rather than relying on the current umask.
//!
//! The helper is test-friendly: the chown/chmod calls bail early with a
//! clear `io::Error` on any failure, and tests that run without root
//! privileges get `io::ErrorKind::PermissionDenied` back — the caller
//! decides whether to escalate or swallow.
//!
//! Used by `ingest.rs`, `send_handler.rs`, `state_handler.rs`, and
//! `mailbox_handler.rs`.

use std::ffi::CString;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

use crate::config::MailboxConfig;

/// Chown `path` to `<mailbox.owner>:<mailbox.owner>` and set its mode to
/// `mode` (the caller-supplied value, typically `0o600` for files and
/// `0o700` for directories).
///
/// Returns `Ok(())` on success; `io::Error` on any resolver or syscall
/// failure. The caller decides how to react — ingest rolls back the
/// freshly-created file, `mailbox_handler` reports `ECONFLICT`, etc.
///
/// This helper is the single source of truth for the post-write chown
/// pattern. Every daemon-side writer that lands a file in a mailbox tree
/// must call it immediately after the `fs::write` / `fs::create_dir*` /
/// `rename(2)` that publishes the file.
pub fn chown_as_owner(path: &Path, mailbox: &MailboxConfig, mode: u32) -> io::Result<()> {
    let uid = mailbox
        .owner_uid()
        .map_err(|e| io::Error::other(format!("resolve owner uid for '{}': {e}", mailbox.owner)))?;
    let gid = mailbox
        .owner_gid()
        .map_err(|e| io::Error::other(format!("resolve owner gid for '{}': {e}", mailbox.owner)))?;

    let c_path = CString::new(path.as_os_str().as_bytes())
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, format!("path has NUL: {e}")))?;

    // SAFETY: `c_path` is a valid NUL-terminated C string pointing at a
    // path owned by the caller. `chown`/`chmod` are POSIX syscalls with
    // no Rust-level invariants to uphold beyond that.
    let rc = unsafe { libc::chown(c_path.as_ptr(), uid, gid) };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }

    let rc = unsafe { libc::chmod(c_path.as_ptr(), mode as libc::mode_t) };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Process-wide umask setter called at `aimx serve` startup. Uses the
/// system `umask(2)` syscall so freshly-created files default to
/// `0600` / directories to `0700` before any explicit chown/chmod in
/// [`chown_as_owner`]. Defense in depth: if a code path forgets the
/// post-write chown, the file is still not world-readable.
///
/// Returns the previous umask value so tests can restore it; production
/// callers ignore the return.
pub fn set_process_umask(new_mask: u32) -> u32 {
    // SAFETY: `umask(2)` is a thread-unsafe POSIX syscall but the daemon
    // calls it exactly once at startup, before any listener is bound, so
    // no other thread is running. Tests that need to toggle the mask
    // serialize through this helper.
    unsafe { libc::umask(new_mask as libc::mode_t) as u32 }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::MailboxConfig;
    use crate::user_resolver::{ResolvedUser, set_test_resolver};
    use std::os::unix::fs::MetadataExt;
    use std::sync::Mutex;

    /// Process-wide serialization mutex for tests that mutate `umask`.
    /// `umask(2)` is a thread-unsafe POSIX syscall — cargo runs unit
    /// tests in parallel, so any test that temporarily changes the mask
    /// must hold this lock for the read-modify-restore critical section.
    /// Mirrors the pattern in `user_resolver::test_resolver::SERIALIZE`.
    /// Every umask-touching test in this module (current and future)
    /// must acquire this lock to keep the assertion deterministic.
    static UMASK_SERIALIZE: Mutex<()> = Mutex::new(());

    fn mb_with_owner(owner: &str) -> MailboxConfig {
        MailboxConfig {
            address: format!("{owner}@example.com"),
            owner: owner.to_string(),
            hooks: vec![],
            trust: None,
            trusted_senders: None,
            allow_root_catchall: false,
        }
    }

    #[test]
    fn chown_as_owner_noop_to_current_user_succeeds() {
        // Resolve "me" to the current euid/egid so the syscall succeeds
        // without requiring root. This exercises the happy path: the
        // syscall runs, the mode is set, no permission error.
        let uid = unsafe { libc::geteuid() };
        let gid = unsafe { libc::getegid() };
        fn fake(name: &str) -> Option<ResolvedUser> {
            // Only the static entries the test registers below; other
            // names fall through to `None`.
            if name == "me" {
                let uid = unsafe { libc::geteuid() };
                let gid = unsafe { libc::getegid() };
                Some(ResolvedUser {
                    name: "me".into(),
                    uid,
                    gid,
                })
            } else {
                None
            }
        }
        let _guard = set_test_resolver(fake);
        let _ = uid;
        let _ = gid;

        let tmp = tempfile::TempDir::new().unwrap();
        let f = tmp.path().join("alice.md");
        std::fs::write(&f, b"hi").unwrap();

        let mb = mb_with_owner("me");
        chown_as_owner(&f, &mb, 0o600).unwrap();

        let meta = std::fs::metadata(&f).unwrap();
        assert_eq!(meta.uid(), unsafe { libc::geteuid() });
        assert_eq!(meta.mode() & 0o777, 0o600);
    }

    #[test]
    fn chown_as_owner_missing_file_returns_error() {
        fn fake(name: &str) -> Option<ResolvedUser> {
            if name == "me" {
                Some(ResolvedUser {
                    name: "me".into(),
                    uid: unsafe { libc::geteuid() },
                    gid: unsafe { libc::getegid() },
                })
            } else {
                None
            }
        }
        let _guard = set_test_resolver(fake);
        let tmp = tempfile::TempDir::new().unwrap();
        let missing = tmp.path().join("does-not-exist.md");
        let mb = mb_with_owner("me");
        let err = chown_as_owner(&missing, &mb, 0o600).unwrap_err();
        // ENOENT on both the chown and chmod paths; we just care that
        // the error propagates.
        assert!(
            matches!(
                err.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::PermissionDenied
            ),
            "expected NotFound or PermissionDenied, got {err:?}"
        );
    }

    #[test]
    fn chown_as_owner_unresolvable_owner_returns_error() {
        fn fake(_name: &str) -> Option<ResolvedUser> {
            None
        }
        let _guard = set_test_resolver(fake);

        let tmp = tempfile::TempDir::new().unwrap();
        let f = tmp.path().join("x.md");
        std::fs::write(&f, b"hi").unwrap();

        let mb = mb_with_owner("ghost");
        let err = chown_as_owner(&f, &mb, 0o600).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("ghost"), "error must name missing user: {msg}");
    }

    #[test]
    fn chown_as_owner_root_owner_reserved_name_resolves_to_uid_zero() {
        // `owner = "root"` is a reserved name in validate_run_as and
        // always resolves to uid 0 without hitting the pluggable
        // resolver. On non-root tests the chown syscall itself will
        // fail (EPERM) but the resolver path is exercised up through
        // the syscall attempt, which is what we care about here.
        let tmp = tempfile::TempDir::new().unwrap();
        let f = tmp.path().join("x.md");
        std::fs::write(&f, b"hi").unwrap();
        let mb = mb_with_owner("root");
        let result = chown_as_owner(&f, &mb, 0o600);
        let is_root = unsafe { libc::geteuid() == 0 };
        if is_root {
            result.unwrap();
            let meta = std::fs::metadata(&f).unwrap();
            assert_eq!(meta.uid(), 0);
            assert_eq!(meta.mode() & 0o777, 0o600);
        } else {
            let err = result.unwrap_err();
            assert!(
                matches!(err.kind(), io::ErrorKind::PermissionDenied),
                "non-root chown to uid 0 must fail with PermissionDenied: {err:?}"
            );
        }
    }

    #[test]
    fn set_process_umask_returns_previous_value() {
        // umask is process-global, not thread-local. Cargo runs unit
        // tests in parallel, so hold `UMASK_SERIALIZE` across the
        // read-modify-restore so a concurrent umask-touching test can't
        // race the "second call returns 0o077" assertion. Any future
        // umask-mutating test must hold the same lock.
        let _guard = UMASK_SERIALIZE
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let prior = set_process_umask(0o077);
        let again = set_process_umask(prior);
        assert_eq!(again, 0o077);
    }
}
