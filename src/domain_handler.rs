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

use crate::auth::{Action, authorize};
use crate::config::{Config, DomainOverride, is_valid_domain_syntax};
use crate::dkim;
use crate::dkim_keys::{DkimKeyEntry, DkimKeyMap, SharedDkimKeyMap, resolve_selector_for_domain};
use crate::mailbox_handler::{CONFIG_WRITE_LOCK, MailboxContext};
use crate::send_protocol::{AckResponse, DomainAddRequest, ErrCode};
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
}
