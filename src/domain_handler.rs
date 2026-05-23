//! Daemon-side handler for the `DOMAIN-ADD` verb of the `AIMX/1` UDS
//! protocol.
//!
//! Mirrors `mailbox_handler` and `hook_handler` in shape: root-only via
//! `auth::authorize(.., Action::DomainCrud)`, runs the
//! load-modify-write-store sequence under
//! [`crate::mailbox_handler::CONFIG_WRITE_LOCK`], and hot-swaps the
//! in-memory `Arc<Config>` plus the per-domain DKIM key map without
//! restarting the daemon.
//!
//! Correctness model:
//!
//! 1. Validate the domain syntax via [`crate::config::is_valid_domain_syntax`]
//!    after lowercasing.
//! 2. Acquire `CONFIG_WRITE_LOCK` (the same lock `MAILBOX-CRUD` /
//!    `HOOK-CRUD` use) so the load-modify-write-store sequence is
//!    serialized across every concurrent config writer.
//! 3. Reject duplicate adds (case-insensitive) without modifying state.
//! 4. Generate the per-domain DKIM keypair via the same
//!    `dkim::generate_keypair` codepath `aimx dkim-keygen` uses, so a
//!    later `aimx dkim-keygen --domain <d>` is exactly equivalent.
//!    Keys land at `<dkim_dir>/<domain>/{private,public}.key` with the
//!    canonical `0600 / 0644` modes.
//! 5. Load the freshly-generated private key into a `DkimKeyEntry`
//!    keyed by the lowercase domain; persist any operator-supplied
//!    selector under `[domain."<d>"] dkim_selector` so subsequent
//!    config loads pick it up.
//! 6. `write_atomic` the new `config.toml`, then
//!    `ConfigHandle::store(new_config)`.
//! 7. Build a fresh DKIM map by cloning the current snapshot, insert
//!    the new entry, and `ArcSwap::store` it. The lock above keeps
//!    this sequence linear with respect to other writers, so a
//!    concurrent `DOMAIN-ADD` cannot lose an entry.
//!
//! Atomicity ordering rationale: DKIM key on disk **before** the config
//! rewrite, and the in-memory DKIM map updated **before** the
//! `ConfigHandle::store`. A crash between key write and config rewrite
//! leaves an orphan key (harmless — the operator can re-run
//! `aimx domains add` cleanly; the duplicate-add check still passes
//! because the domain isn't in `domains` yet). The opposite ordering —
//! config rewritten first, then key write fails — would leave outbound
//! from the new domain broken until manual recovery. Updating the DKIM
//! map before the config snapshot also guarantees that any reader who
//! sees the new domain in `config.domains` already sees the matching
//! DKIM key entry.

use std::path::Path;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::auth::{Action, authorize};
use crate::config::{Config, DomainOverride, is_valid_domain_syntax};
use crate::dkim;
use crate::dkim_keys::{DkimKeyEntry, DkimKeyMap, SharedDkimKeyMap, resolve_selector_for_domain};
use crate::mailbox_handler::{CONFIG_WRITE_LOCK, MailboxContext};
use crate::send_protocol::{
    AckResponse, DomainAddRequest, DomainRemoveRequest, ErrCode, JsonAckResponse,
};
use crate::state_handler::StateContext;
use crate::uds_authz::{Caller, log_decision};

/// Validate, normalize, and persist a `DOMAIN-ADD` request.
///
/// Returns `AckResponse::Ok` on success; the daemon's
/// `Arc<Config>` and DKIM map are both hot-swapped before the response
/// is written, so an immediately-following `DOMAIN-LIST` or SMTP RCPT
/// to the new domain sees the change.
pub async fn handle_domain_add(
    state_ctx: &StateContext,
    mb_ctx: &MailboxContext,
    dkim_keys: &SharedDkimKeyMap,
    req: &DomainAddRequest,
    caller: &Caller,
) -> AckResponse {
    let verb = "DOMAIN-ADD";

    // Authz first — every other check is leaked-state observation, so
    // the central predicate runs at the top.
    if let Err(e) = authorize(caller.uid, Action::DomainCrud, None) {
        log_decision(
            verb,
            caller,
            Some(&req.domain),
            crate::uds_authz::LogDecision::Reject,
            Some(&format!("{e}")),
        );
        return AckResponse::Err {
            code: ErrCode::Eaccess,
            reason: format!("{e}"),
        };
    }

    // Syntactic validation: lowercase + RFC 1035. The Config
    // deserializer applies the same rule, so doing it here keeps the
    // wire response actionable instead of failing the post-write
    // re-load with a less-targeted error.
    let domain_lc = req.domain.trim().to_ascii_lowercase();
    if !is_valid_domain_syntax(&domain_lc) {
        return AckResponse::Err {
            code: ErrCode::Validation,
            reason: format!(
                "domain '{d}' is not a valid RFC 1035 hostname",
                d = req.domain,
            ),
        };
    }

    // Same selector validation the rest of the codebase uses: any
    // operator-supplied selector must be a DNS-safe label (the DKIM
    // signer ships `s=<selector>` verbatim into a header value, so a
    // malformed selector would render the resulting signatures
    // unverifiable). Reject up front rather than at sign time.
    let selector = req.selector.as_deref().map(|s| s.trim().to_string());
    if let Some(s) = &selector
        && !is_valid_dkim_selector(s)
    {
        return AckResponse::Err {
            code: ErrCode::Validation,
            reason: format!("DKIM selector '{s}' is not a valid DNS label (allowed: [a-z0-9_-]+)"),
        };
    }

    // Acquire the process-wide write lock so concurrent
    // `MAILBOX-CRUD` / `HOOK-CRUD` / `DOMAIN-ADD` requests serialize
    // their load-modify-write-store sequences. Per the hierarchy in
    // `mailbox_locks`, this is the **inner** lock — DOMAIN-ADD does
    // not need a per-mailbox lock because no mailbox state changes.
    let _config_guard = CONFIG_WRITE_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());

    let current = mb_ctx.config_handle.load();
    if current.is_configured_domain(&domain_lc) {
        return AckResponse::Err {
            code: ErrCode::Domain,
            reason: format!("domain '{domain_lc}' is already configured"),
        };
    }

    // Build the new Config first so we know exactly what we're about
    // to persist before generating the keypair on disk. If serialization
    // fails (it won't for a Config::clone()-derived struct, but the
    // belt-and-braces check costs nothing), we bail before touching
    // any filesystem state.
    let mut new_config: Config = (*current).clone();
    new_config.domains.push(domain_lc.clone());
    if let Some(s) = &selector {
        new_config.per_domain.insert(
            domain_lc.clone(),
            DomainOverride {
                dkim_selector: Some(s.clone()),
                ..Default::default()
            },
        );
    }

    // Generate the per-domain DKIM keypair into `<dkim_dir>/<domain>/`.
    // Identical codepath to `aimx dkim-keygen --domain <d>` so a
    // freshly-added domain is byte-identical to one provisioned via
    // the CLI keygen.
    let dkim_root = crate::config::dkim_dir();
    let per_domain_dir = dkim_root.join(&domain_lc);
    if let Err(e) = std::fs::create_dir_all(&per_domain_dir) {
        return AckResponse::Err {
            code: ErrCode::Io,
            reason: format!("failed to create {}: {e}", per_domain_dir.display()),
        };
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Err(e) =
            std::fs::set_permissions(&per_domain_dir, std::fs::Permissions::from_mode(0o700))
        {
            return AckResponse::Err {
                code: ErrCode::Io,
                reason: format!("failed to chmod {}: {e}", per_domain_dir.display()),
            };
        }
    }

    // Skip if the operator already pre-provisioned the keys (e.g. ran
    // `aimx dkim-keygen --domain <d>` first). Existing keys with the
    // wrong mode are left alone — the operator owns the on-disk state.
    let private_path = per_domain_dir.join("private.key");
    if !private_path.exists()
        && let Err(e) = dkim::generate_keypair(&per_domain_dir, false)
    {
        return AckResponse::Err {
            code: ErrCode::Io,
            reason: format!("failed to generate DKIM keypair for '{domain_lc}': {e}"),
        };
    }

    // Load the just-written (or pre-existing) key into the daemon's
    // map. Failure here means we have a key on disk but cannot read it
    // — surface the error directly; the orphan key on disk is harmless
    // because the domain hasn't been added to `domains` yet.
    let resolved_selector = resolve_selector_for_domain(&new_config, &domain_lc);
    let key = match dkim::load_private_key(&per_domain_dir) {
        Ok(k) => k,
        Err(e) => {
            return AckResponse::Err {
                code: ErrCode::Sign,
                reason: format!("DKIM keypair for '{domain_lc}' generated but unreadable: {e}",),
            };
        }
    };

    // Persist the new config atomically. If the rewrite fails, the
    // freshly-generated key stays on disk (orphan; harmless) and the
    // in-memory state stays put — the operator can retry cleanly.
    if let Err(e) = crate::mailbox_handler::write_config_atomic(&mb_ctx.config_path, &new_config) {
        return AckResponse::Err {
            code: ErrCode::Io,
            reason: format!("failed to write {}: {e}", mb_ctx.config_path.display()),
        };
    }

    // Hot-swap the DKIM map FIRST: clone the current snapshot, insert
    // the new entry, and `ArcSwap::store` it. Loads inside `handle_send`
    // take a `load_full()` snapshot, so a concurrent send observes
    // either the pre-swap map (its From: domain is in the old set; ok)
    // or the post-swap map (the From: domain may be in the new set;
    // ok). The swap is atomic. We update the DKIM map BEFORE the
    // ConfigHandle so that any reader who sees the new domain in
    // `config.domains` (via the subsequent `ConfigHandle::store`)
    // already sees the matching DKIM entry — there is no window where
    // a SEND can match a configured domain but find no key.
    let mut new_map: DkimKeyMap = (*dkim_keys.load_full()).clone();
    new_map.insert(
        domain_lc.clone(),
        DkimKeyEntry {
            key: Arc::new(key),
            selector: resolved_selector,
        },
    );
    dkim_keys.store(Arc::new(new_map));

    // Now hot-swap the in-memory Config.
    mb_ctx.config_handle.store(new_config);

    log_decision(
        verb,
        caller,
        Some(&domain_lc),
        if caller.is_root() {
            crate::uds_authz::LogDecision::RootBypass
        } else {
            crate::uds_authz::LogDecision::Accept
        },
        None,
    );

    // Suppress the unused-state-context warning; we don't acquire a
    // per-mailbox lock because DOMAIN-ADD touches no mailbox state.
    let _ = state_ctx;
    AckResponse::Ok
}

/// Direct on-disk fallback used by the CLI when the daemon is stopped.
/// Mirrors [`handle_domain_add`] minus the in-memory swaps — the
/// operator restarts `aimx serve` to pick up the change. Only callable
/// from root (the CLI gates this).
///
/// `config` is the loaded snapshot; the function modifies it in-place
/// via clone+rewrite and re-saves to `config_path`. The DKIM key is
/// generated under `<dkim_dir>/<domain>/`. Returns the new in-memory
/// Config (so the CLI can `aimx domains list` against it without
/// re-loading from disk).
pub fn run_direct_add(
    config_path: &Path,
    dkim_root: &Path,
    config: &Config,
    domain: &str,
    selector: Option<&str>,
) -> Result<Config, Box<dyn std::error::Error>> {
    let domain_lc = domain.trim().to_ascii_lowercase();
    if !is_valid_domain_syntax(&domain_lc) {
        return Err(format!("domain '{domain}' is not a valid RFC 1035 hostname").into());
    }
    if let Some(s) = selector
        && !is_valid_dkim_selector(s.trim())
    {
        return Err(
            format!("DKIM selector '{s}' is not a valid DNS label (allowed: [a-z0-9_-]+)").into(),
        );
    }

    if config.is_configured_domain(&domain_lc) {
        return Err(format!("domain '{domain_lc}' is already configured").into());
    }

    let mut new_config: Config = config.clone();
    new_config.domains.push(domain_lc.clone());
    if let Some(s) = selector {
        new_config.per_domain.insert(
            domain_lc.clone(),
            DomainOverride {
                dkim_selector: Some(s.trim().to_string()),
                ..Default::default()
            },
        );
    }

    let per_domain_dir = dkim_root.join(&domain_lc);
    std::fs::create_dir_all(&per_domain_dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&per_domain_dir, std::fs::Permissions::from_mode(0o700))?;
    }
    if !per_domain_dir.join("private.key").exists() {
        dkim::generate_keypair(&per_domain_dir, false)?;
    }

    crate::config::write_atomic(config_path, &new_config)?;
    Ok(new_config)
}

/// Predicate: valid DNS-label-shape DKIM selector. RFC 6376 §3.1
/// references DNS-label syntax for the `s=` tag; we accept the same
/// lowercase alphanumeric+`-_` set Linux usernames use, which is a
/// superset of every DKIM selector we have ever seen in the wild.
fn is_valid_dkim_selector(s: &str) -> bool {
    if s.is_empty() || s.len() > 63 {
        return false;
    }
    s.bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
}

/// Structured body returned by `DOMAIN-REMOVE` on every non-error
/// outcome. The CLI parses this directly so the operator-visible
/// output stays consistent across clean removes, blocked-by-mailboxes
/// refusals, and `--force` cascades.
///
/// The handler returns a `JsonAckResponse::Err` (with the canonical
/// reason string) for authz, validation, last-domain hard-block,
/// domain-not-configured, and IO failures. Every other outcome
/// (including the soft "refused because mailboxes still exist"
/// branch) returns a `JsonAckResponse::Ok` carrying this struct so
/// the CLI can render the list of blocking mailboxes verbatim.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DomainRemoveResponse {
    /// `removed` — the domain was dropped from `config.domains`.
    /// `blocked_by_mailboxes` — refused because mailboxes still
    ///    reference this domain and `force = false`. The
    ///    `blocking_mailboxes` field carries the list.
    pub outcome: DomainRemoveOutcome,
    /// FQDNs of mailboxes that still reference this domain. Populated
    /// when `outcome == BlockedByMailboxes`. Sorted for stable output.
    pub blocking_mailboxes: Vec<String>,
    /// FQDNs of mailboxes the cascade deleted. Populated when
    /// `outcome == Removed` AND `--force` was set; empty otherwise.
    /// Sorted for stable output.
    pub cascaded_mailboxes: Vec<String>,
    /// `true` iff this remove operation actually deleted a non-empty
    /// per-domain storage tree from disk (`<data_dir>/<domain>/`). The
    /// CLI uses this to decide whether to print the "Storage tree
    /// removed." line. False for:
    ///
    /// * the soft-refused `blocked_by_mailboxes` path (no cascade
    ///   ran),
    /// * the clean-no-blockers path when no per-domain dir existed
    ///   beforehand (typical — no mailboxes on the domain means no
    ///   storage tree was ever provisioned),
    /// * `--force` cascades where the per-domain dir was absent at
    ///   entry (degenerate, but possible if storage was hand-cleaned
    ///   before the remove).
    ///
    /// True only when the handler observed an on-disk per-domain tree
    /// at entry and successfully `rmdir`'d it.
    pub storage_tree_removed: bool,
    /// Absolute path to `<dkim_dir>/<domain>/` — preserved on disk
    /// regardless of `--force` so the operator can re-add the domain
    /// without re-generating a fresh keypair. The CLI prints this as
    /// the "DKIM keypair preserved at …" hint.
    pub dkim_dir: String,
}

/// Outcome tag for [`DomainRemoveResponse`]. Stringly-typed on the
/// wire so adding a future outcome (e.g. `Deferred`) doesn't break a
/// stale CLI that only branches on the variants it recognizes.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DomainRemoveOutcome {
    /// Domain dropped from `config.domains`. Cascade fields are
    /// populated when `--force` was set.
    Removed,
    /// Domain still has mailboxes referencing it; the cascade was not
    /// requested. The CLI prints the `blocking_mailboxes` list and
    /// suggests `--force` to cascade.
    BlockedByMailboxes,
}

/// Validate, lock, and persist a `DOMAIN-REMOVE` request.
///
/// Returns [`JsonAckResponse::Ok`] carrying a [`DomainRemoveResponse`]
/// body on every non-error outcome (clean remove, soft-refused
/// because blockers, `--force` cascade). Returns [`JsonAckResponse::Err`]
/// for authz failures, validation failures, the last-domain hard-block,
/// unknown-domain, and IO failures.
///
/// Lock hierarchy follows the daemon-wide convention documented in
/// [`crate::mailbox_locks`]: per-mailbox locks (acquired in sorted
/// FQDN order) are the **outer** layer; the process-wide
/// [`CONFIG_WRITE_LOCK`] is the **inner** layer. Inverting the order
/// would deadlock against a concurrent `MAILBOX-CREATE` /
/// `MAILBOX-DELETE` / `HOOK-CREATE` / `HOOK-DELETE` request, every
/// one of which takes the per-mailbox lock first then the config
/// lock. Crash-safety: the per-mailbox locks are held across the
/// entire critical section (per-mailbox wipe → `rmdir` → config
/// rewrite → `ConfigHandle::store` → DKIM map hot-swap) so any other
/// handler observes either the pre-cascade or the post-cascade state,
/// never a half-cascaded one.
///
/// **Re-run recovery contract (not strict atomicity).** The cascade
/// is *re-runnable*, not atomic in the database-transaction sense. If
/// a per-mailbox wipe or `rmdir` fails partway through, the early
/// return surfaces the IO error and:
///
/// * the per-mailbox locks and `CONFIG_WRITE_LOCK` are released
///   (RAII drop on the held guards),
/// * the in-memory `Config` and DKIM map have NOT been swapped yet
///   (both stores happen after the wipe/rmdir block), so external
///   observers (SMTP RCPT, MCP, CLI list) still see the domain and
///   its mailboxes as configured,
/// * on-disk state may be partially wiped (some mailboxes' inbox /
///   sent directories empty, others still populated).
///
/// Recovery is to re-run `aimx domains remove <domain> --force`. The
/// second invocation re-acquires every per-mailbox lock, re-walks the
/// (still-configured) mailbox set, and re-applies the wipes idempotently
/// (`wipe_mailbox_contents` tolerates already-empty / already-missing
/// directories; `rmdir` on a missing path is treated as success-and-
/// no-op). Nothing observes intermediate state outside the lock — a
/// concurrent reader either sees the pre-cascade view (locks held by
/// the first attempt) or the post-cascade view (after the second
/// attempt completes).
///
/// DKIM key files at `<dkim_dir>/<domain>/` are **never** deleted by
/// this handler. The path is echoed back in the response so the CLI
/// can print the "preserved on disk" hint. Rationale: avoid surprise
/// key destruction; the operator can remove the directory by hand if
/// they really want the keys gone.
pub async fn handle_domain_remove(
    state_ctx: &StateContext,
    mb_ctx: &MailboxContext,
    dkim_keys: &SharedDkimKeyMap,
    req: &DomainRemoveRequest,
    caller: &Caller,
) -> JsonAckResponse {
    let verb = "DOMAIN-REMOVE";

    // Authz first — every other check below would leak observable
    // state to an unauthorized caller.
    if let Err(e) = authorize(caller.uid, Action::DomainCrud, None) {
        log_decision(
            verb,
            caller,
            Some(&req.domain),
            crate::uds_authz::LogDecision::Reject,
            Some(&format!("{e}")),
        );
        return JsonAckResponse::Err {
            code: ErrCode::Eaccess,
            reason: format!("{e}"),
        };
    }

    // Lowercase + syntactic validation; the loader applies the same
    // rule but doing it up-front means the wire response is actionable
    // even before we touch the config snapshot.
    let domain_lc = req.domain.trim().to_ascii_lowercase();
    if !is_valid_domain_syntax(&domain_lc) {
        return JsonAckResponse::Err {
            code: ErrCode::Validation,
            reason: format!(
                "domain '{d}' is not a valid RFC 1035 hostname",
                d = req.domain,
            ),
        };
    }

    // Snapshot the current config under the live handle so we can
    // pre-compute the lock set and the blocking-mailbox list before
    // taking any locks. The snapshot is an `Arc` clone — cheap.
    let pre_current = mb_ctx.config_handle.load();

    if !pre_current.is_configured_domain(&domain_lc) {
        return JsonAckResponse::Err {
            code: ErrCode::Domain,
            reason: format!("domain '{domain_lc}' is not configured"),
        };
    }

    // Last-domain hard-block. The check is independent of `force` —
    // an AIMX install must have at least one domain to be functional.
    // Operators wanting a full teardown should use `aimx uninstall`.
    if pre_current.domains.len() == 1 {
        return JsonAckResponse::Err {
            code: ErrCode::Domain,
            reason: format!(
                "cannot remove '{domain_lc}': it is the last configured domain. \
                 An AIMX install must have at least one domain. \
                 Use `aimx uninstall` for a full teardown."
            ),
        };
    }

    // Compute the sorted FQDN list of mailboxes on the target domain.
    // Used both for the blocker list (no-force refusal) and for the
    // per-mailbox lock acquisition order (force cascade).
    let mut blockers: Vec<String> = pre_current
        .mailboxes
        .values()
        .filter(|mb| mailbox_address_in_domain(&mb.address, &domain_lc))
        .map(|mb| mb.address.clone())
        .collect();
    blockers.sort();
    blockers.dedup();

    let dkim_dir_path = crate::config::dkim_dir().join(&domain_lc);
    let dkim_dir_string = dkim_dir_path.display().to_string();

    // Non-force soft refusal: report the blockers via Ok body so the
    // CLI can pretty-print the list and suggest `--force`. Drop the
    // snapshot before returning so the live handle isn't pinned.
    if !req.force && !blockers.is_empty() {
        drop(pre_current);
        log_decision(
            verb,
            caller,
            Some(&domain_lc),
            crate::uds_authz::LogDecision::Accept,
            Some("refused: mailboxes still reference this domain"),
        );
        return ok_response(DomainRemoveResponse {
            outcome: DomainRemoveOutcome::BlockedByMailboxes,
            blocking_mailboxes: blockers,
            cascaded_mailboxes: vec![],
            storage_tree_removed: false,
            dkim_dir: dkim_dir_string,
        });
    }

    // We're committing to either a clean remove (no mailboxes, no
    // force needed) OR a `--force` cascade.
    //
    // Lock ordering rationale (see `mailbox_locks.rs` module docs):
    //
    // outer: per-mailbox locks from `state_ctx.locks` in **sorted FQDN
    //        order**. Inbound ingest, MARK-*, MAILBOX-CRUD, and
    //        HOOK-CRUD all take the per-mailbox lock FIRST, then the
    //        config lock — inverting that order here would deadlock
    //        against any of them.
    // inner: CONFIG_WRITE_LOCK. Serializes the load-modify-write-store
    //        sequence across every config writer.
    //
    // Sorting the FQDN list before acquiring the locks gives us a
    // deterministic order — required so two concurrent cascades on
    // overlapping mailbox sets (impossible today; future-proofing)
    // cannot deadlock against each other.
    //
    // We hold the per-mailbox locks across the per-mailbox wipe, the
    // storage-tree rmdir, the config rewrite, and the in-memory
    // hot-swaps. Concurrent ingest into mailboxes on OTHER domains
    // (e.g. `support@a.com` while we remove `b.com`) is unaffected
    // because the per-mailbox lock map is keyed by mailbox, not by
    // domain — locks on b.com mailboxes don't block locks on a.com
    // mailboxes. This is what makes the cascade compatible with
    // background ingest on the surviving domain.
    let lock_keys = blockers.clone();
    let lock_states: Vec<Arc<crate::mailbox_locks::MailboxState>> = lock_keys
        .iter()
        .map(|fqdn| state_ctx.locks.lock_for(fqdn))
        .collect();
    drop(pre_current);

    // Acquire the per-mailbox guards in order. Held until the end of
    // this function via `held_guards`.
    let mut held_guards: Vec<tokio::sync::MutexGuard<'_, ()>> =
        Vec::with_capacity(lock_states.len());
    for state in &lock_states {
        held_guards.push(state.lock.lock().await);
    }

    // Inner: process-wide config write lock.
    let _config_guard = CONFIG_WRITE_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());

    // Test-only injection point: between lock acquisition and the
    // under-lock re-snapshot. Used to pin the
    // `live_blocker_fqdns != lock_keys` conflict-detection invariant
    // by simulating a (hypothetically) racing mailbox-set mutation
    // without actually racing another handler. No-op in release
    // builds.
    #[cfg(test)]
    test_hooks::run_after_locks_hook(mb_ctx);

    // Re-snapshot under the lock so the rest of the critical section
    // runs against a coherent view. A concurrent writer that landed
    // between our pre-flight snapshot and the lock acquisition is
    // detected here.
    let current = mb_ctx.config_handle.load();

    // Re-check the last-domain invariant and the blocking-mailbox set
    // under the lock — a concurrent `DOMAIN-REMOVE` or
    // `MAILBOX-CREATE` may have changed them.
    if current.domains.len() == 1 {
        return JsonAckResponse::Err {
            code: ErrCode::Domain,
            reason: format!(
                "cannot remove '{domain_lc}': it is the last configured domain. \
                 An AIMX install must have at least one domain. \
                 Use `aimx uninstall` for a full teardown."
            ),
        };
    }
    if !current.is_configured_domain(&domain_lc) {
        return JsonAckResponse::Err {
            code: ErrCode::Domain,
            reason: format!("domain '{domain_lc}' is not configured"),
        };
    }

    // Re-collect blockers under the lock. If the set grew since the
    // pre-flight snapshot we did NOT take that mailbox's lock, so we
    // must abort the cascade rather than silently skip the new mailbox
    // (which could leak files on disk owned by a domain we then drop
    // from config). Realistically this never fires — `MAILBOX-CREATE`
    // takes its own per-mailbox lock and the config lock, and we hold
    // the config lock — but the check is the cheap belt-and-braces.
    let mut live_blockers: Vec<(String, crate::config::MailboxConfig)> = current
        .mailboxes
        .iter()
        .filter(|(_, mb)| mailbox_address_in_domain(&mb.address, &domain_lc))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    live_blockers.sort_by(|a, b| a.1.address.cmp(&b.1.address));

    let live_blocker_fqdns: Vec<String> = live_blockers
        .iter()
        .map(|(_, mb)| mb.address.clone())
        .collect();
    if live_blocker_fqdns != lock_keys {
        return JsonAckResponse::Err {
            code: ErrCode::Conflict,
            reason: format!(
                "mailbox set for '{domain_lc}' changed under the cascade lock; retry the remove"
            ),
        };
    }

    if !req.force && !live_blockers.is_empty() {
        // Should be unreachable — the pre-flight branch above returns
        // before we ever take the locks for the non-force path. Keep
        // the guard as belt-and-braces against a future refactor that
        // drops the early return.
        return JsonAckResponse::Err {
            code: ErrCode::NonEmpty,
            reason: format!(
                "domain '{domain_lc}' still has {} mailbox(es); pass --force to cascade",
                live_blockers.len()
            ),
        };
    }

    // Per-mailbox wipe (force cascade only). Each mailbox's
    // `inbox/<local>/` and `sent/<local>/` directory contents are
    // removed via the same helper `MAILBOX-DELETE --force` uses, so
    // the wipe contract (dotfile preservation, symlink handling,
    // missing-dir tolerance) is shared. The directories themselves
    // are removed below via `rmdir` after the per-domain rmdir.
    let mut cascaded: Vec<String> = Vec::with_capacity(live_blockers.len());
    for (_key, mb) in &live_blockers {
        let inbox =
            crate::storage::mailbox_storage_path(&current, mb, crate::storage::Folder::Inbox);
        let sent = crate::storage::mailbox_storage_path(&current, mb, crate::storage::Folder::Sent);
        if let Err(e) = crate::mailbox::wipe_mailbox_contents(&inbox) {
            return JsonAckResponse::Err {
                code: ErrCode::Io,
                reason: format!("failed to wipe inbox for '{}': {e}", mb.address),
            };
        }
        if let Err(e) = crate::mailbox::wipe_mailbox_contents(&sent) {
            return JsonAckResponse::Err {
                code: ErrCode::Io,
                reason: format!("failed to wipe sent for '{}': {e}", mb.address),
            };
        }
        // After wiping contents, remove the per-mailbox directories
        // themselves so the per-domain rmdir below sees an empty
        // tree. Best-effort: a missing directory is fine (e.g.
        // mailbox never received mail); a non-empty directory is a
        // bug surfaced by the per-domain rmdir below.
        let _ = std::fs::remove_dir(&inbox);
        let _ = std::fs::remove_dir(&sent);
        cascaded.push(mb.address.clone());
    }

    // Storage-tree rmdir: explicitly `rmdir`, never `remove_dir_all`.
    // The mailbox wipes above are the only safe deletion path; if the
    // per-domain dir still has children at this point it means a
    // mailbox wipe missed something or an unmanaged file leaked in.
    // Either way, surface the failure loudly rather than silently
    // recursing.
    //
    // `storage_tree_removed` is only set to `true` when we observed a
    // per-domain directory at entry AND successfully removed it. The
    // CLI uses this flag to decide whether to print "Storage tree
    // removed." — printing it on the clean-no-blockers path when no
    // tree ever existed is misleading.
    let domain_root = current.data_dir.join(&domain_lc);
    let storage_tree_removed = if domain_root.is_dir() {
        // The cascade wipes both `inbox/<local>/` and `sent/<local>/`
        // dirs out, leaving the `inbox/` and `sent/` parents behind.
        // Walk through and rmdir those as well so the per-domain
        // root is empty before the final rmdir. Best-effort on each
        // — if either is missing (e.g. an install that never had any
        // sent mail), `rmdir` returns NotFound which we ignore.
        for folder in ["inbox", "sent"] {
            let p = domain_root.join(folder);
            let _ = std::fs::remove_dir(&p);
        }
        match std::fs::remove_dir(&domain_root) {
            Ok(()) => true,
            // Race against an external cleanup — the directory was
            // there when we checked `is_dir()` but is gone now. Treat
            // as "we did not actually do the removal" rather than
            // claiming we did.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
            Err(e) => {
                return JsonAckResponse::Err {
                    code: ErrCode::Io,
                    reason: format!(
                        "failed to remove storage tree {}: {e} \
                         (per-mailbox wipes left non-empty contents — \
                         inspect manually)",
                        domain_root.display()
                    ),
                };
            }
        }
    } else {
        // No on-disk tree to remove — typically the non-force clean
        // path or a domain whose mailboxes were all created but never
        // received mail. We did not delete anything from disk.
        false
    };

    // Build the new in-memory Config.
    let mut new_config: Config = (*current).clone();
    // Drop the per-domain sub-table.
    new_config.per_domain.remove(&domain_lc);
    // Drop every [mailboxes."<local>@<domain>"] entry.
    new_config
        .mailboxes
        .retain(|_, mb| !mailbox_address_in_domain(&mb.address, &domain_lc));
    // Drop the domain string.
    new_config
        .domains
        .retain(|d| !d.eq_ignore_ascii_case(&domain_lc));

    if let Err(e) = crate::mailbox_handler::write_config_atomic(&mb_ctx.config_path, &new_config) {
        return JsonAckResponse::Err {
            code: ErrCode::Io,
            reason: format!("failed to write {}: {e}", mb_ctx.config_path.display()),
        };
    }

    // Hot-swap the DKIM map FIRST: drop the per-domain entry. Order
    // mirrors `handle_domain_add` — we keep readers consistent so any
    // reader who sees the domain GONE from `config.domains` also sees
    // the DKIM map entry gone.
    let mut new_map: DkimKeyMap = (*dkim_keys.load_full()).clone();
    new_map.remove(&domain_lc);
    dkim_keys.store(Arc::new(new_map));

    // Now hot-swap the in-memory Config.
    mb_ctx.config_handle.store(new_config);
    drop(current);

    log_decision(
        verb,
        caller,
        Some(&domain_lc),
        if caller.is_root() {
            crate::uds_authz::LogDecision::RootBypass
        } else {
            crate::uds_authz::LogDecision::Accept
        },
        None,
    );

    // Drop the per-mailbox guards explicitly so the lock scope ends
    // before the response is written.
    drop(held_guards);
    drop(lock_states);

    ok_response(DomainRemoveResponse {
        outcome: DomainRemoveOutcome::Removed,
        blocking_mailboxes: vec![],
        cascaded_mailboxes: cascaded,
        storage_tree_removed,
        dkim_dir: dkim_dir_string,
    })
}

/// Helper: serialize a `DomainRemoveResponse` into the wire body.
fn ok_response(resp: DomainRemoveResponse) -> JsonAckResponse {
    match serde_json::to_vec(&resp) {
        Ok(body) => JsonAckResponse::Ok { body },
        Err(e) => JsonAckResponse::Err {
            code: ErrCode::Io,
            reason: format!("failed to serialize DOMAIN-REMOVE response: {e}"),
        },
    }
}

/// True iff `address` belongs to `domain` (case-insensitive). Used to
/// match both regular `<local>@<domain>` mailboxes and per-domain
/// catchall (`*@<domain>`) entries against the target domain.
fn mailbox_address_in_domain(address: &str, domain: &str) -> bool {
    match address.rsplit_once('@') {
        Some((_, d)) => d.eq_ignore_ascii_case(domain),
        None => false,
    }
}

/// Direct on-disk fallback used by the CLI when the daemon is stopped.
/// Mirrors [`handle_domain_remove`] minus the in-memory swaps — the
/// operator restarts `aimx serve` to pick up the change. Only callable
/// from root (the CLI gates this).
///
/// Returns the in-memory [`DomainRemoveResponse`] for the CLI to
/// render. Last-domain hard-block, unknown-domain, validation, and
/// IO errors are surfaced as `Err`. DKIM keys are NEVER removed.
pub fn run_direct_remove(
    config_path: &Path,
    config: &Config,
    domain: &str,
    force: bool,
) -> Result<DomainRemoveResponse, Box<dyn std::error::Error>> {
    let domain_lc = domain.trim().to_ascii_lowercase();
    if !is_valid_domain_syntax(&domain_lc) {
        return Err(format!("domain '{domain}' is not a valid RFC 1035 hostname").into());
    }
    if !config.is_configured_domain(&domain_lc) {
        return Err(format!("domain '{domain_lc}' is not configured").into());
    }
    if config.domains.len() == 1 {
        return Err(format!(
            "cannot remove '{domain_lc}': it is the last configured domain. \
             An AIMX install must have at least one domain. \
             Use `aimx uninstall` for a full teardown."
        )
        .into());
    }

    let mut blockers: Vec<(String, crate::config::MailboxConfig)> = config
        .mailboxes
        .iter()
        .filter(|(_, mb)| mailbox_address_in_domain(&mb.address, &domain_lc))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    blockers.sort_by(|a, b| a.1.address.cmp(&b.1.address));

    let dkim_dir = crate::config::dkim_dir()
        .join(&domain_lc)
        .display()
        .to_string();

    if !force && !blockers.is_empty() {
        return Ok(DomainRemoveResponse {
            outcome: DomainRemoveOutcome::BlockedByMailboxes,
            blocking_mailboxes: blockers.iter().map(|(_, mb)| mb.address.clone()).collect(),
            cascaded_mailboxes: vec![],
            storage_tree_removed: false,
            dkim_dir,
        });
    }

    let mut cascaded: Vec<String> = Vec::with_capacity(blockers.len());
    for (_key, mb) in &blockers {
        let inbox = crate::storage::mailbox_storage_path(config, mb, crate::storage::Folder::Inbox);
        let sent = crate::storage::mailbox_storage_path(config, mb, crate::storage::Folder::Sent);
        crate::mailbox::wipe_mailbox_contents(&inbox)?;
        crate::mailbox::wipe_mailbox_contents(&sent)?;
        let _ = std::fs::remove_dir(&inbox);
        let _ = std::fs::remove_dir(&sent);
        cascaded.push(mb.address.clone());
    }

    let domain_root = config.data_dir.join(&domain_lc);
    let storage_tree_removed = if domain_root.is_dir() {
        for folder in ["inbox", "sent"] {
            let _ = std::fs::remove_dir(domain_root.join(folder));
        }
        match std::fs::remove_dir(&domain_root) {
            Ok(()) => true,
            // Race against an external cleanup — the directory vanished
            // between the `is_dir()` check and the `rmdir`. We did not
            // remove it.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
            Err(e) => {
                return Err(format!(
                    "failed to remove storage tree {}: {e}",
                    domain_root.display()
                )
                .into());
            }
        }
    } else {
        // No on-disk tree to remove.
        false
    };

    let mut new_config = config.clone();
    new_config.per_domain.remove(&domain_lc);
    new_config
        .mailboxes
        .retain(|_, mb| !mailbox_address_in_domain(&mb.address, &domain_lc));
    new_config
        .domains
        .retain(|d| !d.eq_ignore_ascii_case(&domain_lc));

    crate::config::write_atomic(config_path, &new_config)?;

    Ok(DomainRemoveResponse {
        outcome: DomainRemoveOutcome::Removed,
        blocking_mailboxes: vec![],
        cascaded_mailboxes: cascaded,
        storage_tree_removed,
        dkim_dir,
    })
}

/// Test-only injection points used by the unit tests in this module to
/// pin invariants that are unreachable via production codepaths today.
#[cfg(test)]
mod test_hooks {
    use super::MailboxContext;
    use std::sync::Mutex;

    /// Hook executed after `handle_domain_remove` has taken the
    /// per-mailbox locks + `CONFIG_WRITE_LOCK` but before it
    /// re-snapshots the config. Mutates the in-memory config via the
    /// supplied `MailboxContext` so the test can drive the
    /// `live_blocker_fqdns != lock_keys` conflict-detection branch
    /// deterministically.
    type Hook = Box<dyn Fn(&MailboxContext) + Send + Sync + 'static>;

    static HOOK: Mutex<Option<Hook>> = Mutex::new(None);

    pub(super) fn run_after_locks_hook(mb_ctx: &MailboxContext) {
        let guard = HOOK.lock().unwrap();
        if let Some(h) = guard.as_ref() {
            h(mb_ctx);
        }
    }

    /// RAII handle that uninstalls the hook on drop so tests can run
    /// in parallel without leaking the hook across other tests in the
    /// module.
    pub(super) struct HookGuard;

    impl Drop for HookGuard {
        fn drop(&mut self) {
            *HOOK.lock().unwrap() = None;
        }
    }

    pub(super) fn install<F>(f: F) -> HookGuard
    where
        F: Fn(&MailboxContext) + Send + Sync + 'static,
    {
        *HOOK.lock().unwrap() = Some(Box::new(f));
        HookGuard
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::test_env::ConfigDirOverride;
    use crate::config::{Config, ConfigHandle};
    use std::collections::HashMap;
    use tempfile::TempDir;

    fn install_resolver() -> crate::user_resolver::test_resolver::ResolverOverride {
        fn fake(name: &str) -> Option<crate::user_resolver::ResolvedUser> {
            match name {
                "root" => Some(crate::user_resolver::ResolvedUser {
                    name: "root".to_string(),
                    uid: 0,
                    gid: 0,
                }),
                "testowner" => Some(crate::user_resolver::ResolvedUser {
                    name: "testowner".to_string(),
                    uid: 1000,
                    gid: 1000,
                }),
                _ => None,
            }
        }
        crate::user_resolver::set_test_resolver(fake)
    }

    fn base_config(data_dir: &Path) -> Config {
        Config {
            domains: vec!["a.com".to_string()],
            data_dir: data_dir.to_path_buf(),
            dkim_selector: None,
            trust: "none".to_string(),
            trusted_senders: vec![],
            mailboxes: HashMap::new(),
            per_domain: HashMap::new(),
            verify_host: None,
            enable_ipv6: false,
            signature: None,
            upgrade: None,
        }
    }

    fn contexts(tmp: &TempDir) -> (StateContext, MailboxContext, SharedDkimKeyMap) {
        let config = base_config(tmp.path());
        let handle = ConfigHandle::new(config);
        let state_ctx = StateContext::new(tmp.path().to_path_buf(), handle.clone());
        let config_path = tmp.path().join("config.toml");
        crate::mailbox_handler::write_config_atomic(&config_path, &handle.load()).unwrap();
        let mb_ctx = MailboxContext::new(config_path, handle);
        let keys = crate::dkim_keys::empty_shared();
        (state_ctx, mb_ctx, keys)
    }

    #[tokio::test]
    async fn root_add_appends_domain_writes_config_and_hot_swaps() {
        let _r = install_resolver();
        let tmp = TempDir::new().unwrap();
        let _override = ConfigDirOverride::set(tmp.path());
        let (state_ctx, mb_ctx, keys) = contexts(&tmp);

        let req = DomainAddRequest {
            domain: "b.com".into(),
            selector: None,
        };
        let resp =
            handle_domain_add(&state_ctx, &mb_ctx, &keys, &req, &Caller::internal_root()).await;
        assert!(matches!(resp, AckResponse::Ok), "got {resp:?}");

        // In-memory hot-swap: live config carries both domains.
        let after = mb_ctx.config_handle.load();
        assert_eq!(after.domains, vec!["a.com", "b.com"]);

        // On-disk config reloads with both domains.
        let reloaded = Config::load_ignore_warnings(&mb_ctx.config_path).unwrap();
        assert_eq!(reloaded.domains, vec!["a.com", "b.com"]);

        // DKIM map hot-swapped — `b.com` resolves to an entry.
        let snapshot = keys.load_full();
        assert!(snapshot.contains_key("b.com"));

        // Keypair landed on disk at the per-domain layout.
        let dkim_root = crate::config::dkim_dir();
        assert!(dkim_root.join("b.com").join("private.key").is_file());
        assert!(dkim_root.join("b.com").join("public.key").is_file());
    }

    #[tokio::test]
    async fn duplicate_add_rejects_without_modifying_state() {
        let _r = install_resolver();
        let tmp = TempDir::new().unwrap();
        let _override = ConfigDirOverride::set(tmp.path());
        let (state_ctx, mb_ctx, keys) = contexts(&tmp);

        let req = DomainAddRequest {
            domain: "a.com".into(), // already configured
            selector: None,
        };
        let resp =
            handle_domain_add(&state_ctx, &mb_ctx, &keys, &req, &Caller::internal_root()).await;
        match resp {
            AckResponse::Err { code, reason } => {
                assert_eq!(code, ErrCode::Domain);
                assert!(reason.contains("already configured"), "{reason}");
            }
            other => panic!("expected Err Domain, got {other:?}"),
        }

        // No state change.
        let after = mb_ctx.config_handle.load();
        assert_eq!(after.domains, vec!["a.com"]);
        let snapshot = keys.load_full();
        assert!(!snapshot.contains_key("b.com"));
    }

    #[tokio::test]
    async fn non_root_caller_denied_with_eacces() {
        let _r = install_resolver();
        let tmp = TempDir::new().unwrap();
        let _override = ConfigDirOverride::set(tmp.path());
        let (state_ctx, mb_ctx, keys) = contexts(&tmp);

        let req = DomainAddRequest {
            domain: "b.com".into(),
            selector: None,
        };
        let stranger = Caller::new(1000, 1000, None);
        let resp = handle_domain_add(&state_ctx, &mb_ctx, &keys, &req, &stranger).await;
        match resp {
            AckResponse::Err { code, .. } => assert_eq!(code, ErrCode::Eaccess),
            other => panic!("expected Err EACCES, got {other:?}"),
        }
        // No state change.
        let after = mb_ctx.config_handle.load();
        assert_eq!(after.domains, vec!["a.com"]);
    }

    #[tokio::test]
    async fn invalid_domain_syntax_rejected() {
        let _r = install_resolver();
        let tmp = TempDir::new().unwrap();
        let _override = ConfigDirOverride::set(tmp.path());
        let (state_ctx, mb_ctx, keys) = contexts(&tmp);

        let req = DomainAddRequest {
            domain: "not a domain".into(),
            selector: None,
        };
        let resp =
            handle_domain_add(&state_ctx, &mb_ctx, &keys, &req, &Caller::internal_root()).await;
        match resp {
            AckResponse::Err { code, .. } => assert_eq!(code, ErrCode::Validation),
            other => panic!("expected Err Validation, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn invalid_selector_rejected() {
        let _r = install_resolver();
        let tmp = TempDir::new().unwrap();
        let _override = ConfigDirOverride::set(tmp.path());
        let (state_ctx, mb_ctx, keys) = contexts(&tmp);

        let req = DomainAddRequest {
            domain: "b.com".into(),
            selector: Some("bad selector".into()),
        };
        let resp =
            handle_domain_add(&state_ctx, &mb_ctx, &keys, &req, &Caller::internal_root()).await;
        match resp {
            AckResponse::Err { code, .. } => assert_eq!(code, ErrCode::Validation),
            other => panic!("expected Err Validation, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn selector_persisted_in_per_domain_override() {
        let _r = install_resolver();
        let tmp = TempDir::new().unwrap();
        let _override = ConfigDirOverride::set(tmp.path());
        let (state_ctx, mb_ctx, keys) = contexts(&tmp);

        let req = DomainAddRequest {
            domain: "b.com".into(),
            selector: Some("s2025".into()),
        };
        let resp =
            handle_domain_add(&state_ctx, &mb_ctx, &keys, &req, &Caller::internal_root()).await;
        assert!(matches!(resp, AckResponse::Ok), "got {resp:?}");

        let after = mb_ctx.config_handle.load();
        let over = after.per_domain.get("b.com").expect("per-domain override");
        assert_eq!(over.dkim_selector.as_deref(), Some("s2025"));

        let snapshot = keys.load_full();
        let entry = snapshot.get("b.com").expect("DKIM entry");
        assert_eq!(entry.selector, "s2025");
    }

    /// `run_direct_add` (root daemon-stopped fallback) writes config
    /// and DKIM key to disk without touching the in-memory handle.
    #[test]
    fn direct_add_writes_config_and_dkim_to_disk() {
        let _r = install_resolver();
        let tmp = TempDir::new().unwrap();
        let _override = ConfigDirOverride::set(tmp.path());
        let config_path = tmp.path().join("config.toml");
        let config = base_config(tmp.path());
        crate::config::write_atomic(&config_path, &config).unwrap();
        let dkim_root = tmp.path().join("dkim");

        let new_config = run_direct_add(&config_path, &dkim_root, &config, "b.com", None).unwrap();
        assert_eq!(new_config.domains, vec!["a.com", "b.com"]);
        let reloaded = Config::load_ignore_warnings(&config_path).unwrap();
        assert_eq!(reloaded.domains, vec!["a.com", "b.com"]);
        assert!(dkim_root.join("b.com").join("private.key").is_file());
    }

    #[test]
    fn direct_add_rejects_duplicate() {
        let _r = install_resolver();
        let tmp = TempDir::new().unwrap();
        let _override = ConfigDirOverride::set(tmp.path());
        let config_path = tmp.path().join("config.toml");
        let config = base_config(tmp.path());
        crate::config::write_atomic(&config_path, &config).unwrap();
        let dkim_root = tmp.path().join("dkim");

        let err = run_direct_add(&config_path, &dkim_root, &config, "a.com", None)
            .expect_err("duplicate must err");
        assert!(err.to_string().contains("already configured"));
    }

    #[test]
    fn is_valid_dkim_selector_accepts_canonical_values() {
        assert!(is_valid_dkim_selector("aimx"));
        assert!(is_valid_dkim_selector("s2025"));
        assert!(is_valid_dkim_selector("aimx-rotation-2"));
        assert!(!is_valid_dkim_selector(""));
        assert!(!is_valid_dkim_selector("UPPER"));
        assert!(!is_valid_dkim_selector("a b"));
        assert!(!is_valid_dkim_selector("a.b"));
    }

    /// Pin the pre-provisioned-key contract: if the operator dropped a
    /// keypair into `<dkim_dir>/<domain>/{private,public}.key` before
    /// running `aimx domains add <domain>`, the handler MUST reuse those
    /// keys and not overwrite them. This is the supported flow for
    /// rotating a known-good key into a new domain in one step.
    #[tokio::test]
    async fn pre_existing_dkim_keys_are_reused_not_overwritten() {
        let _r = install_resolver();
        let tmp = TempDir::new().unwrap();
        let _override = ConfigDirOverride::set(tmp.path());
        let (state_ctx, mb_ctx, keys) = contexts(&tmp);

        // Pre-provision a real RSA keypair at the per-domain layout
        // BEFORE the handler runs. The handler must detect the
        // existing private.key and skip the keygen so this exact
        // byte sequence ends up loaded into the DKIM map.
        let dkim_root = crate::config::dkim_dir();
        let per_domain_dir = dkim_root.join("b.com");
        std::fs::create_dir_all(&per_domain_dir).unwrap();
        crate::dkim::generate_keypair(&per_domain_dir, false).unwrap();
        let before = std::fs::read_to_string(per_domain_dir.join("private.key")).unwrap();

        let req = DomainAddRequest {
            domain: "b.com".into(),
            selector: None,
        };
        let resp =
            handle_domain_add(&state_ctx, &mb_ctx, &keys, &req, &Caller::internal_root()).await;
        assert!(matches!(resp, AckResponse::Ok), "got {resp:?}");

        let after = std::fs::read_to_string(per_domain_dir.join("private.key")).unwrap();
        assert_eq!(
            before, after,
            "pre-existing DKIM private key must not be overwritten by `aimx domains add`"
        );

        // The DKIM map carries the (unchanged) pre-existing key.
        let snapshot = keys.load_full();
        assert!(
            snapshot.contains_key("b.com"),
            "DKIM map must contain an entry for the added domain"
        );
    }

    /// Same contract for the daemon-stopped fallback path. `run_direct_add`
    /// must also preserve a pre-existing per-domain key.
    #[test]
    fn direct_add_preserves_pre_existing_dkim_keys() {
        let _r = install_resolver();
        let tmp = TempDir::new().unwrap();
        let _override = ConfigDirOverride::set(tmp.path());
        let config_path = tmp.path().join("config.toml");
        let config = base_config(tmp.path());
        crate::config::write_atomic(&config_path, &config).unwrap();
        let dkim_root = tmp.path().join("dkim");

        let per_domain_dir = dkim_root.join("b.com");
        std::fs::create_dir_all(&per_domain_dir).unwrap();
        crate::dkim::generate_keypair(&per_domain_dir, false).unwrap();
        let before = std::fs::read_to_string(per_domain_dir.join("private.key")).unwrap();

        let _ = run_direct_add(&config_path, &dkim_root, &config, "b.com", None).unwrap();
        let after = std::fs::read_to_string(per_domain_dir.join("private.key")).unwrap();
        assert_eq!(
            before, after,
            "daemon-stopped fallback must preserve pre-existing DKIM keys"
        );
    }

    // ----------------- DOMAIN-REMOVE tests --------------------------------

    use crate::config::MailboxConfig;
    use crate::send_protocol::{DomainRemoveRequest, JsonAckResponse};

    /// Build a real but throwaway DKIM private key for tests. Uses a
    /// fresh TempDir so the on-disk artifacts don't pollute the test
    /// process and disappear at the end of the call.
    fn make_test_dkim_key() -> rsa::RsaPrivateKey {
        let tmp = TempDir::new().unwrap();
        crate::dkim::generate_keypair(tmp.path(), false).unwrap();
        crate::dkim::load_private_key(tmp.path()).unwrap()
    }

    /// Build a two-domain config with `a.com` + `b.com`. Optional
    /// mailboxes can be added under `b.com` to exercise the cascade.
    fn two_domain_config(data_dir: &Path, bcom_mailboxes: &[&str]) -> Config {
        let mut mailboxes = HashMap::new();
        // Always-on a.com mailbox so the a.com side isn't empty during
        // tests that verify the cascade doesn't touch the surviving
        // domain.
        mailboxes.insert(
            "info@a.com".to_string(),
            MailboxConfig {
                address: "info@a.com".to_string(),
                owner: "testowner".to_string(),
                hooks: vec![],
                trust: None,
                trusted_senders: None,
                allow_root_catchall: false,
            },
        );
        for name in bcom_mailboxes {
            let addr = format!("{name}@b.com");
            mailboxes.insert(
                addr.clone(),
                MailboxConfig {
                    address: addr,
                    owner: "testowner".to_string(),
                    hooks: vec![],
                    trust: None,
                    trusted_senders: None,
                    allow_root_catchall: false,
                },
            );
        }
        let mut per_domain = HashMap::new();
        per_domain.insert(
            "b.com".to_string(),
            DomainOverride {
                signature: None,
                dkim_selector: Some("s2025".into()),
                trust: None,
                trusted_senders: None,
            },
        );
        Config {
            domains: vec!["a.com".to_string(), "b.com".to_string()],
            data_dir: data_dir.to_path_buf(),
            dkim_selector: None,
            trust: "none".to_string(),
            trusted_senders: vec![],
            mailboxes,
            per_domain,
            verify_host: None,
            enable_ipv6: false,
            signature: None,
            upgrade: None,
        }
    }

    fn contexts_with_config(
        tmp: &TempDir,
        config: Config,
    ) -> (StateContext, MailboxContext, SharedDkimKeyMap) {
        // Seed the v2 marker so `mailbox_storage_path` resolves to the
        // per-domain layout (`<data_dir>/<domain>/{inbox|sent}/<local>`).
        std::fs::write(tmp.path().join(".layout-version"), "2\n").unwrap();
        let handle = ConfigHandle::new(config);
        let state_ctx = StateContext::new(tmp.path().to_path_buf(), handle.clone());
        let config_path = tmp.path().join("config.toml");
        crate::mailbox_handler::write_config_atomic(&config_path, &handle.load()).unwrap();
        let mb_ctx = MailboxContext::new(config_path, handle);
        let keys = crate::dkim_keys::empty_shared();
        (state_ctx, mb_ctx, keys)
    }

    /// Provision the on-disk storage tree for a mailbox so the cascade
    /// has something to wipe. Seeds one fake `.md` file inside the
    /// inbox so the wipe path is exercised, not just the empty-dir
    /// rmdir.
    fn seed_mailbox_storage(tmp: &TempDir, domain: &str, local: &str) {
        for folder in ["inbox", "sent"] {
            let dir = tmp.path().join(domain).join(folder).join(local);
            std::fs::create_dir_all(&dir).unwrap();
        }
        let stub = tmp
            .path()
            .join(domain)
            .join("inbox")
            .join(local)
            .join("2026-05-23-fake.md");
        std::fs::write(&stub, "+++\nid = \"fake\"\n+++\n\nhi\n").unwrap();
    }

    /// Removing a domain with no mailboxes on it (`force = false`) is
    /// the clean-remove happy path. Asserts: config rewritten, DKIM map
    /// entry dropped, per-domain sub-table dropped.
    #[tokio::test]
    async fn remove_clean_no_blockers_drops_domain_and_dkim_entry() {
        let _r = install_resolver();
        let tmp = TempDir::new().unwrap();
        let _override = ConfigDirOverride::set(tmp.path());
        let (state_ctx, mb_ctx, keys) =
            contexts_with_config(&tmp, two_domain_config(tmp.path(), &[]));

        // Pre-seed the DKIM map for b.com so we can assert the entry is
        // dropped after the cascade.
        let mut seed_map: DkimKeyMap = (*keys.load_full()).clone();
        seed_map.insert(
            "b.com".to_string(),
            DkimKeyEntry {
                key: Arc::new(make_test_dkim_key()),
                selector: "aimx".to_string(),
            },
        );
        keys.store(Arc::new(seed_map));

        let req = DomainRemoveRequest {
            domain: "b.com".into(),
            force: false,
        };
        let resp =
            handle_domain_remove(&state_ctx, &mb_ctx, &keys, &req, &Caller::internal_root()).await;
        match resp {
            JsonAckResponse::Ok { body } => {
                let parsed: DomainRemoveResponse = serde_json::from_slice(&body).unwrap();
                assert_eq!(parsed.outcome, DomainRemoveOutcome::Removed);
                assert!(parsed.blocking_mailboxes.is_empty());
                assert!(parsed.cascaded_mailboxes.is_empty());
                assert!(parsed.dkim_dir.ends_with("/b.com"));
                // The per-domain storage tree never existed (no
                // mailboxes were ever provisioned on b.com), so the
                // response must report `storage_tree_removed = false`
                // — printing "Storage tree removed." in this case
                // would be misleading.
                assert!(
                    !parsed.storage_tree_removed,
                    "clean-no-blockers must NOT claim a storage tree was removed when none existed",
                );
            }
            other => panic!("expected Ok, got {other:?}"),
        }

        // In-memory hot-swap.
        let after = mb_ctx.config_handle.load();
        assert_eq!(after.domains, vec!["a.com"]);
        assert!(!after.per_domain.contains_key("b.com"));

        // On-disk config also rewritten.
        let reloaded = Config::load_ignore_warnings(&mb_ctx.config_path).unwrap();
        assert_eq!(reloaded.domains, vec!["a.com"]);

        // DKIM map dropped its b.com entry.
        let snapshot = keys.load_full();
        assert!(!snapshot.contains_key("b.com"));
    }

    /// Removing a domain that still has mailboxes (`force = false`)
    /// must refuse and return the list of blocking mailboxes
    /// alphabetically.
    #[tokio::test]
    async fn remove_blocked_by_mailboxes_returns_sorted_list_no_state_change() {
        let _r = install_resolver();
        let tmp = TempDir::new().unwrap();
        let _override = ConfigDirOverride::set(tmp.path());
        let (state_ctx, mb_ctx, keys) = contexts_with_config(
            &tmp,
            two_domain_config(tmp.path(), &["zeta", "alpha", "beta"]),
        );

        let req = DomainRemoveRequest {
            domain: "b.com".into(),
            force: false,
        };
        let resp =
            handle_domain_remove(&state_ctx, &mb_ctx, &keys, &req, &Caller::internal_root()).await;
        match resp {
            JsonAckResponse::Ok { body } => {
                let parsed: DomainRemoveResponse = serde_json::from_slice(&body).unwrap();
                assert_eq!(parsed.outcome, DomainRemoveOutcome::BlockedByMailboxes);
                assert_eq!(
                    parsed.blocking_mailboxes,
                    vec![
                        "alpha@b.com".to_string(),
                        "beta@b.com".to_string(),
                        "zeta@b.com".to_string(),
                    ],
                    "blockers must be sorted alphabetically",
                );
                assert!(parsed.cascaded_mailboxes.is_empty());
                assert!(!parsed.storage_tree_removed);
            }
            other => panic!("expected Ok, got {other:?}"),
        }

        // No state change.
        let after = mb_ctx.config_handle.load();
        assert_eq!(after.domains, vec!["a.com", "b.com"]);
        assert!(after.mailboxes.contains_key("alpha@b.com"));
    }

    /// The last remaining domain is hard-blocked from removal even
    /// with `force = true` — operators wanting a full teardown must
    /// use `aimx uninstall`.
    #[tokio::test]
    async fn remove_last_domain_is_hard_blocked_even_with_force() {
        let _r = install_resolver();
        let tmp = TempDir::new().unwrap();
        let _override = ConfigDirOverride::set(tmp.path());
        let (state_ctx, mb_ctx, keys) = contexts(&tmp); // single-domain a.com

        for force in [false, true] {
            let req = DomainRemoveRequest {
                domain: "a.com".into(),
                force,
            };
            let resp =
                handle_domain_remove(&state_ctx, &mb_ctx, &keys, &req, &Caller::internal_root())
                    .await;
            match resp {
                JsonAckResponse::Err { code, reason } => {
                    assert_eq!(code, ErrCode::Domain, "force={force}");
                    assert!(
                        reason.contains("last configured domain"),
                        "force={force}, reason={reason}",
                    );
                    assert!(
                        reason.contains("aimx uninstall"),
                        "force={force}, reason={reason}",
                    );
                }
                other => panic!("expected Err Domain, got {other:?} (force={force})"),
            }
        }
    }

    /// `--force` cascade: configure b.com with three mailboxes (each
    /// with on-disk storage), invoke remove with force, assert all
    /// three mailboxes are dropped, the storage tree is gone, the DKIM
    /// map entry is gone, and the DKIM keypair on disk is preserved.
    #[tokio::test]
    async fn force_cascade_wipes_mailboxes_storage_and_keeps_dkim_keys() {
        let _r = install_resolver();
        let tmp = TempDir::new().unwrap();
        let _override = ConfigDirOverride::set(tmp.path());
        let (state_ctx, mb_ctx, keys) = contexts_with_config(
            &tmp,
            two_domain_config(tmp.path(), &["info", "support", "alice"]),
        );

        // Provision on-disk storage for every b.com mailbox so the
        // cascade has real work to do.
        for local in ["info", "support", "alice"] {
            seed_mailbox_storage(&tmp, "b.com", local);
        }
        // Also provision a.com storage so we can assert the cascade
        // doesn't accidentally touch the surviving domain.
        seed_mailbox_storage(&tmp, "a.com", "info");

        // Pre-seed b.com DKIM keys at the canonical per-domain layout
        // so we can verify they're preserved after the cascade.
        let dkim_root = crate::config::dkim_dir();
        let b_dkim_dir = dkim_root.join("b.com");
        std::fs::create_dir_all(&b_dkim_dir).unwrap();
        std::fs::write(b_dkim_dir.join("private.key"), b"FAKE_PRIVATE_KEY").unwrap();
        std::fs::write(b_dkim_dir.join("public.key"), b"FAKE_PUBLIC_KEY").unwrap();

        // Pre-seed the DKIM map entry for b.com.
        let mut seed_map: DkimKeyMap = (*keys.load_full()).clone();
        seed_map.insert(
            "b.com".to_string(),
            DkimKeyEntry {
                key: Arc::new(make_test_dkim_key()),
                selector: "s2025".to_string(),
            },
        );
        keys.store(Arc::new(seed_map));

        let req = DomainRemoveRequest {
            domain: "b.com".into(),
            force: true,
        };
        let resp =
            handle_domain_remove(&state_ctx, &mb_ctx, &keys, &req, &Caller::internal_root()).await;

        match resp {
            JsonAckResponse::Ok { body } => {
                let parsed: DomainRemoveResponse = serde_json::from_slice(&body).unwrap();
                assert_eq!(parsed.outcome, DomainRemoveOutcome::Removed);
                assert!(parsed.storage_tree_removed);
                assert_eq!(
                    parsed.cascaded_mailboxes,
                    vec![
                        "alice@b.com".to_string(),
                        "info@b.com".to_string(),
                        "support@b.com".to_string(),
                    ],
                    "cascaded mailboxes must be sorted by FQDN",
                );
            }
            other => panic!("expected Ok, got {other:?}"),
        }

        // Config rewritten in memory and on disk.
        let after = mb_ctx.config_handle.load();
        assert_eq!(after.domains, vec!["a.com"]);
        assert!(!after.per_domain.contains_key("b.com"));
        assert!(after.mailboxes.keys().all(|k| !k.ends_with("@b.com")));
        let reloaded = Config::load_ignore_warnings(&mb_ctx.config_path).unwrap();
        assert_eq!(reloaded.domains, vec!["a.com"]);

        // Per-domain b.com storage tree is gone.
        let b_root = tmp.path().join("b.com");
        assert!(
            !b_root.exists(),
            "b.com storage tree must be removed, still at {}",
            b_root.display()
        );

        // a.com storage is untouched.
        let a_info = tmp.path().join("a.com").join("inbox").join("info");
        assert!(
            a_info.is_dir(),
            "a.com mailbox storage must survive the b.com cascade",
        );
        // a.com seeded stub file is also still there.
        assert!(a_info.join("2026-05-23-fake.md").is_file());

        // DKIM map dropped b.com.
        let snapshot = keys.load_full();
        assert!(!snapshot.contains_key("b.com"));

        // DKIM keypair on disk is preserved.
        assert!(
            b_dkim_dir.join("private.key").is_file(),
            "DKIM private.key must be preserved after --force cascade",
        );
        assert!(
            b_dkim_dir.join("public.key").is_file(),
            "DKIM public.key must be preserved after --force cascade",
        );
        let private_after = std::fs::read(b_dkim_dir.join("private.key")).unwrap();
        assert_eq!(
            private_after, b"FAKE_PRIVATE_KEY",
            "DKIM private key bytes must be untouched after cascade",
        );
    }

    /// Non-root caller is denied with EACCES.
    #[tokio::test]
    async fn remove_non_root_caller_denied_with_eacces() {
        let _r = install_resolver();
        let tmp = TempDir::new().unwrap();
        let _override = ConfigDirOverride::set(tmp.path());
        let (state_ctx, mb_ctx, keys) =
            contexts_with_config(&tmp, two_domain_config(tmp.path(), &[]));

        let req = DomainRemoveRequest {
            domain: "b.com".into(),
            force: false,
        };
        let stranger = Caller::new(1000, 1000, None);
        let resp = handle_domain_remove(&state_ctx, &mb_ctx, &keys, &req, &stranger).await;
        match resp {
            JsonAckResponse::Err { code, .. } => assert_eq!(code, ErrCode::Eaccess),
            other => panic!("expected Err EACCES, got {other:?}"),
        }
        let after = mb_ctx.config_handle.load();
        assert_eq!(after.domains, vec!["a.com", "b.com"]);
    }

    /// Removing a domain that isn't configured surfaces a canonical
    /// `ErrCode::Domain` error.
    #[tokio::test]
    async fn remove_unknown_domain_rejected() {
        let _r = install_resolver();
        let tmp = TempDir::new().unwrap();
        let _override = ConfigDirOverride::set(tmp.path());
        let (state_ctx, mb_ctx, keys) =
            contexts_with_config(&tmp, two_domain_config(tmp.path(), &[]));

        let req = DomainRemoveRequest {
            domain: "c.com".into(),
            force: false,
        };
        let resp =
            handle_domain_remove(&state_ctx, &mb_ctx, &keys, &req, &Caller::internal_root()).await;
        match resp {
            JsonAckResponse::Err { code, reason } => {
                assert_eq!(code, ErrCode::Domain);
                assert!(reason.contains("not configured"), "{reason}");
            }
            other => panic!("expected Err Domain, got {other:?}"),
        }
    }

    /// Invalid domain syntax is rejected with `ErrCode::Validation`.
    #[tokio::test]
    async fn remove_invalid_domain_syntax_rejected() {
        let _r = install_resolver();
        let tmp = TempDir::new().unwrap();
        let _override = ConfigDirOverride::set(tmp.path());
        let (state_ctx, mb_ctx, keys) =
            contexts_with_config(&tmp, two_domain_config(tmp.path(), &[]));

        let req = DomainRemoveRequest {
            domain: "not a domain".into(),
            force: false,
        };
        let resp =
            handle_domain_remove(&state_ctx, &mb_ctx, &keys, &req, &Caller::internal_root()).await;
        match resp {
            JsonAckResponse::Err { code, .. } => assert_eq!(code, ErrCode::Validation),
            other => panic!("expected Err Validation, got {other:?}"),
        }
    }

    /// The non-force clean-remove path (no blockers) succeeds even
    /// when the DKIM key files are still on disk: the handler MUST
    /// NOT delete them.
    #[tokio::test]
    async fn clean_remove_preserves_dkim_keys_on_disk() {
        let _r = install_resolver();
        let tmp = TempDir::new().unwrap();
        let _override = ConfigDirOverride::set(tmp.path());
        let (state_ctx, mb_ctx, keys) =
            contexts_with_config(&tmp, two_domain_config(tmp.path(), &[]));

        let dkim_root = crate::config::dkim_dir();
        let b_dkim_dir = dkim_root.join("b.com");
        std::fs::create_dir_all(&b_dkim_dir).unwrap();
        std::fs::write(b_dkim_dir.join("private.key"), b"FAKE").unwrap();
        std::fs::write(b_dkim_dir.join("public.key"), b"FAKE").unwrap();

        let req = DomainRemoveRequest {
            domain: "b.com".into(),
            force: false,
        };
        let _ =
            handle_domain_remove(&state_ctx, &mb_ctx, &keys, &req, &Caller::internal_root()).await;

        assert!(b_dkim_dir.join("private.key").is_file());
        assert!(b_dkim_dir.join("public.key").is_file());
    }

    /// `run_direct_remove` (root daemon-stopped fallback) writes the
    /// new config and wipes b.com storage without touching the
    /// in-memory handle. Also preserves the DKIM keys on disk.
    #[test]
    fn direct_remove_writes_config_wipes_storage_and_preserves_dkim() {
        let _r = install_resolver();
        let tmp = TempDir::new().unwrap();
        let _override = ConfigDirOverride::set(tmp.path());
        std::fs::write(tmp.path().join(".layout-version"), "2\n").unwrap();
        let config = two_domain_config(tmp.path(), &["info"]);
        let config_path = tmp.path().join("config.toml");
        crate::config::write_atomic(&config_path, &config).unwrap();
        seed_mailbox_storage(&tmp, "b.com", "info");
        let b_dkim_dir = crate::config::dkim_dir().join("b.com");
        std::fs::create_dir_all(&b_dkim_dir).unwrap();
        std::fs::write(b_dkim_dir.join("private.key"), b"FAKE").unwrap();

        let response = run_direct_remove(&config_path, &config, "b.com", true).unwrap();
        assert_eq!(response.outcome, DomainRemoveOutcome::Removed);
        assert!(response.storage_tree_removed);
        assert_eq!(response.cascaded_mailboxes, vec!["info@b.com".to_string()]);

        let reloaded = Config::load_ignore_warnings(&config_path).unwrap();
        assert_eq!(reloaded.domains, vec!["a.com"]);
        assert!(!tmp.path().join("b.com").exists());
        assert!(b_dkim_dir.join("private.key").is_file());
    }

    /// `run_direct_remove` rejects the last-domain hard-block.
    #[test]
    fn direct_remove_last_domain_hard_block() {
        let _r = install_resolver();
        let tmp = TempDir::new().unwrap();
        let _override = ConfigDirOverride::set(tmp.path());
        let config = base_config(tmp.path());
        let config_path = tmp.path().join("config.toml");
        crate::config::write_atomic(&config_path, &config).unwrap();

        let err = run_direct_remove(&config_path, &config, "a.com", true)
            .expect_err("last-domain must hard-block");
        assert!(err.to_string().contains("last configured domain"));
        assert!(err.to_string().contains("aimx uninstall"));
    }

    /// `run_direct_remove` refuses non-force with the blocker list
    /// populated.
    #[test]
    fn direct_remove_blocked_when_mailboxes_present_no_force() {
        let _r = install_resolver();
        let tmp = TempDir::new().unwrap();
        let _override = ConfigDirOverride::set(tmp.path());
        std::fs::write(tmp.path().join(".layout-version"), "2\n").unwrap();
        let config = two_domain_config(tmp.path(), &["info", "alice"]);
        let config_path = tmp.path().join("config.toml");
        crate::config::write_atomic(&config_path, &config).unwrap();

        let response = run_direct_remove(&config_path, &config, "b.com", false).unwrap();
        assert_eq!(response.outcome, DomainRemoveOutcome::BlockedByMailboxes);
        assert_eq!(
            response.blocking_mailboxes,
            vec!["alice@b.com".to_string(), "info@b.com".to_string()],
        );

        // Config unchanged on disk.
        let reloaded = Config::load_ignore_warnings(&config_path).unwrap();
        assert_eq!(reloaded.domains, vec!["a.com", "b.com"]);
    }

    /// Pin the `live_blocker_fqdns != lock_keys` conflict-detection
    /// invariant. The path is unreachable via production codepaths
    /// today because every mailbox-set mutator (`MAILBOX-CREATE`,
    /// `MAILBOX-DELETE`, `DOMAIN-REMOVE` itself) takes the same
    /// per-mailbox locks + `CONFIG_WRITE_LOCK` we hold here — but a
    /// future bug that introduces drift between the canonical
    /// pre-cascade scan and the per-mailbox lock acquisition list
    /// would silently leak (skipping or double-touching mailboxes).
    /// This test installs a test-only after-locks hook that injects
    /// a new b.com mailbox into the live config snapshot AFTER the
    /// locks have been taken — simulating exactly that drift — and
    /// asserts the handler detects the divergence and refuses with
    /// `ErrCode::Conflict`. Uses a serial-test-style coarse mutex to
    /// keep the global hook race-free with respect to other tests.
    #[tokio::test]
    async fn live_blocker_fqdns_not_equal_lock_keys_refused_with_conflict() {
        // Module-local async mutex to serialize the global hook
        // across any other test that decides to install it
        // concurrently. Use tokio's Mutex so we can hold the guard
        // across the `.await` on `handle_domain_remove`.
        static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
        let _serial = SERIAL.lock().await;

        let _r = install_resolver();
        let tmp = TempDir::new().unwrap();
        let _override = ConfigDirOverride::set(tmp.path());
        let (state_ctx, mb_ctx, keys) = contexts_with_config(
            &tmp,
            two_domain_config(tmp.path(), &["info"]), // single b.com mailbox
        );

        // Install the after-locks hook: once the handler has taken
        // the per-mailbox locks (only `info@b.com` at this point) and
        // the CONFIG_WRITE_LOCK, mutate the live config to insert a
        // *new* b.com mailbox (`stranger@b.com`) that the lock-set
        // pre-scan never saw. The under-lock re-scan must now produce
        // a `live_blocker_fqdns` list that diverges from `lock_keys`,
        // tripping the conflict-detection branch.
        let _hook = test_hooks::install(move |mb_ctx| {
            let snapshot = mb_ctx.config_handle.load();
            let mut new_config = (*snapshot).clone();
            new_config.mailboxes.insert(
                "stranger@b.com".to_string(),
                crate::config::MailboxConfig {
                    address: "stranger@b.com".to_string(),
                    owner: "testowner".to_string(),
                    hooks: vec![],
                    trust: None,
                    trusted_senders: None,
                    allow_root_catchall: false,
                },
            );
            mb_ctx.config_handle.store(new_config);
        });

        let req = DomainRemoveRequest {
            domain: "b.com".into(),
            force: true,
        };
        let resp =
            handle_domain_remove(&state_ctx, &mb_ctx, &keys, &req, &Caller::internal_root()).await;
        match resp {
            JsonAckResponse::Err { code, reason } => {
                assert_eq!(code, ErrCode::Conflict, "reason={reason}");
                assert!(
                    reason.contains("changed under the cascade lock"),
                    "expected the cascade-lock conflict reason, got: {reason}",
                );
            }
            other => panic!("expected Err Conflict, got {other:?}"),
        }

        // The handler refused before touching the in-memory config /
        // DKIM map / on-disk state. The post-hook config still carries
        // both b.com mailboxes; the on-disk config still has b.com in
        // `domains`.
        let after = mb_ctx.config_handle.load();
        assert!(
            after.domains.contains(&"b.com".to_string()),
            "domain must not be dropped on conflict path: {:?}",
            after.domains,
        );
        assert!(after.mailboxes.contains_key("info@b.com"));
        assert!(after.mailboxes.contains_key("stranger@b.com"));
    }
}
