//! Per-mailbox write lock map and dedup cache shared by every path
//! that mutates a mailbox's on-disk tree.
//!
//! # Why a shared lock
//!
//! Inbound ingest, MARK-READ/UNREAD, and MAILBOX-CREATE/DELETE all do
//! "read → modify → write" on files under the same mailbox tree. They
//! could in principle target disjoint files (ingest creates a fresh
//! `YYYY-MM-DD-HHMMSS-<slug>.md`; MARK rewrites an existing stem), but
//! relying on file-path disjointness is fragile: any future verb that
//! re-opens an existing `.md` on the ingest side, or that creates files
//! on the MARK side, would quietly lose correctness. A single
//! per-mailbox lock unifies all three writers so the safety argument no
//! longer depends on file-path disjointness.
//!
//! # Lock hierarchy
//!
//! Always acquire in this order; never the reverse:
//!
//! 1. **Outer**: per-mailbox [`tokio::sync::Mutex<()>`] held on
//!    [`MailboxState::lock`]. Held for the full read-modify-write
//!    critical section on any file in `<data_dir>/inbox/<mailbox>/`
//!    or `<data_dir>/sent/<mailbox>/`.
//! 2. **Inner**: process-wide `CONFIG_WRITE_LOCK`
//!    (`std::sync::Mutex<()>` in [`crate::mailbox_handler`]). Held only
//!    for the `load → modify → write → store` sequence on
//!    `config.toml`. `MAILBOX-CREATE` / `MAILBOX-DELETE` acquire the
//!    outer lock first, then the inner one, so a concurrent MARK on the
//!    same mailbox can never observe a half-written config while it
//!    holds the outer lock.
//!
//! The outer then inner order is the only safe one: the config write is
//! short and bounded, so holding it while a longer ingest critical
//! section waits on the per-mailbox lock would be a straight-up
//! priority-inversion. Inverting would deadlock: two concurrent
//! `MAILBOX-CREATE` on different names both take the config lock, then
//! race for their per-mailbox locks.
//!
//! # Dedup cache
//!
//! Each [`MailboxState`] also carries a lazily-initialized SparKV-backed
//! cache of recently-seen `Message-Id` headers. Inbound ingest consults
//! it before touching disk so that legitimate SMTP retries (which
//! re-deliver the same `Message-Id`) don't re-fire `on_receive` hooks.
//! Entries auto-evict after 24h, matching the practical upper bound of
//! MTA retry queues. The cache is not the source of truth — it is
//! rebuilt on demand from the frontmatter of files already on disk.
//!
//! # Contention notes
//!
//! The map-level `std::sync::Mutex` around the hashmap is held only for
//! the brief insert-if-absent step; the per-mailbox Mutex is what
//! callers actually wait on. Hot paths (ingest under heavy inbound
//! load) take one map-level lock cycle plus one per-mailbox lock per
//! message, which is cheap in practice.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::sync::Mutex as AsyncMutex;

/// Per-mailbox state held by [`MailboxLocks`]. Exposes the async write
/// lock plus a lazily-initialized Message-Id dedup cache.
pub struct MailboxState {
    /// Outer lock from the hierarchy described in the module docs.
    /// Acquired via `.lock().await` from async contexts (UDS handlers)
    /// and `.blocking_lock()` from sync contexts (ingest under
    /// `spawn_blocking`).
    pub lock: AsyncMutex<()>,

    /// Message-Id dedup cache. `None` until the first ingest into this
    /// mailbox after daemon startup; populated lazily from disk on
    /// first use. Wrapped in `std::sync::Mutex` because `SparKV`
    /// mutates on every read/write.
    pub seen_message_ids: Mutex<Option<sparkv::SparKV>>,
}

impl MailboxState {
    fn new() -> Self {
        Self {
            lock: AsyncMutex::new(()),
            seen_message_ids: Mutex::new(None),
        }
    }
}

/// Shared per-mailbox state map. Every writer (inbound ingest, MARK-*,
/// MAILBOX-*) acquires its mailbox's [`MailboxState::lock`] before
/// touching files under that mailbox's tree.
pub struct MailboxLocks {
    // `std::sync::Mutex` around the map itself because the insert-if-
    // absent step is synchronous and uncontended. The per-mailbox
    // `AsyncMutex` is what callers actually hold across `.await` points.
    states: Mutex<HashMap<String, Arc<MailboxState>>>,
}

impl MailboxLocks {
    pub fn new() -> Self {
        Self {
            states: Mutex::new(HashMap::new()),
        }
    }

    /// Return (lazy-inserting if needed) the per-mailbox state handle.
    /// The caller decides whether to `.lock.lock().await` (async
    /// contexts) or `.lock.blocking_lock()` (sync contexts like
    /// blocking `ingest_email` under `spawn_blocking`).
    pub fn lock_for(&self, mailbox: &str) -> Arc<MailboxState> {
        let mut map = self
            .states
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        map.entry(mailbox.to_string())
            .or_insert_with(|| Arc::new(MailboxState::new()))
            .clone()
    }
}

impl Default for MailboxLocks {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lock_for_same_mailbox_returns_same_state() {
        let locks = MailboxLocks::new();
        let a = locks.lock_for("alice");
        let b = locks.lock_for("alice");
        // Same underlying Arc => same state struct => `Arc::ptr_eq`.
        assert!(Arc::ptr_eq(&a, &b));
    }

    #[test]
    fn lock_for_different_mailboxes_returns_distinct_state() {
        let locks = MailboxLocks::new();
        let a = locks.lock_for("alice");
        let b = locks.lock_for("bob");
        assert!(!Arc::ptr_eq(&a, &b));
    }

    #[tokio::test]
    async fn async_lock_serializes_two_holders() {
        let locks = Arc::new(MailboxLocks::new());
        let s1 = locks.lock_for("alice");
        let g1 = s1.lock.lock().await;

        // Second waiter times out fast when the first holder is still up.
        let s2 = locks.lock_for("alice");
        let res = tokio::time::timeout(std::time::Duration::from_millis(50), s2.lock.lock()).await;
        assert!(res.is_err(), "second lock must block while first is held");

        drop(g1);
        // Now acquirable.
        let _g2 = tokio::time::timeout(std::time::Duration::from_millis(100), s2.lock.lock())
            .await
            .expect("second lock must acquire once first is dropped");
    }

    #[test]
    fn seen_message_ids_starts_uninitialized() {
        let locks = MailboxLocks::new();
        let state = locks.lock_for("alice");
        let guard = state.seen_message_ids.lock().unwrap();
        assert!(guard.is_none());
    }
}
