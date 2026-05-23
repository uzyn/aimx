//! Per-domain DKIM key map loaded at daemon startup.
//!
//! `aimx serve` loads one DKIM keypair per configured domain into a
//! shared [`DkimKeyMap`] (wrapped in an `ArcSwap` so future domain CRUD
//! verbs can hot-swap an entry without restarting the daemon). The
//! outbound send path resolves the From: domain to its [`DkimKeyEntry`]
//! and uses the entry's selector + key for signing.
//!
//! Per-domain selector resolution: per-domain `[domain.<d>] dkim_selector`
//! → top-level `Config.dkim_selector` → built-in `"aimx"`. Missing
//! per-domain keys at startup are logged as warnings; the daemon still
//! starts. Attempting to sign for that domain later fails with the
//! canonical "no DKIM key for domain X" error.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use arc_swap::ArcSwap;
use rsa::RsaPrivateKey;

use crate::config::Config;
use crate::dkim;

/// One per-domain DKIM keypair plus its resolved selector. The selector
/// is computed once at load time so the hot-path signer doesn't reach
/// back into `Config` on every send.
#[derive(Clone)]
pub struct DkimKeyEntry {
    pub key: Arc<RsaPrivateKey>,
    pub selector: String,
}

/// Shared map keyed by lowercase domain. Domains absent from the map
/// (e.g. a freshly added domain whose key generation hasn't run yet)
/// trip the canonical "no DKIM key for domain X" error.
pub type DkimKeyMap = HashMap<String, DkimKeyEntry>;

/// Atomic-swap wrapper around the map. The send path takes a cheap
/// `load_full()` snapshot at the top of every request; domain CRUD
/// verbs (future DOMAIN-ADD / DOMAIN-REMOVE work) replace the inner `Arc` without
/// blocking concurrent reads.
pub type SharedDkimKeyMap = Arc<ArcSwap<DkimKeyMap>>;

/// Build a fresh `SharedDkimKeyMap` wrapping an empty map. Test fixtures
/// that don't need DKIM use this.
#[allow(dead_code)]
pub fn empty_shared() -> SharedDkimKeyMap {
    Arc::new(ArcSwap::from_pointee(HashMap::new()))
}

/// Resolve the per-domain DKIM selector via the documented order:
/// per-domain override → top-level `Config.dkim_selector` → built-in
/// `"aimx"` default. Thin wrapper around
/// [`Config::dkim_selector_for_domain`] returning the resolved
/// selector as an owned `String` so the keymap loader can stash it on
/// [`DkimKeyEntry`].
pub fn resolve_selector_for_domain(config: &Config, domain: &str) -> String {
    config.dkim_selector_for_domain(domain).to_string()
}

/// Outcome of a per-domain key load attempt. Used by the startup loader
/// to log a warning per missing entry without aborting the daemon.
#[derive(Debug)]
pub enum LoadOutcome {
    Loaded,
    MissingKey {
        path: std::path::PathBuf,
        error: String,
    },
}

/// Per-domain report from [`load_dkim_keys`], used by the daemon to
/// emit a single startup line summarising every domain's key status.
#[derive(Debug)]
pub struct DomainLoadReport {
    pub domain: String,
    pub outcome: LoadOutcome,
}

/// Load one keypair per configured domain into a fresh `DkimKeyMap`.
///
/// For each domain, the loader looks under `<dkim_dir>/<domain>/private.key`
/// first (canonical multi-domain layout). When that file is missing the
/// loader falls back to `<dkim_dir>/private.key` for the **default
/// domain only** — legacy single-key installs that ran `aimx setup
/// <domain>` have the keypair at the un-namespaced root
/// (`<dkim_dir>/{private,public}.key`), and the daemon continues to
/// load it from there until the operator runs `aimx dkim-keygen
/// --domain <d>` to migrate to the per-domain layout
/// (`<dkim_dir>/<domain>/{private,public}.key`).
///
/// Missing keys for non-default domains surface as
/// [`LoadOutcome::MissingKey`] in the returned report; the caller logs
/// at WARN and the daemon keeps running.
pub fn load_dkim_keys(config: &Config, dkim_dir: &Path) -> (DkimKeyMap, Vec<DomainLoadReport>) {
    let mut map: DkimKeyMap = HashMap::with_capacity(config.domains.len());
    let mut reports: Vec<DomainLoadReport> = Vec::with_capacity(config.domains.len());

    let default_domain = config.default_domain();

    for domain in &config.domains {
        let domain_lc = domain.to_ascii_lowercase();
        let per_domain_dir = dkim_dir.join(&domain_lc);
        let per_domain_key = per_domain_dir.join("private.key");
        let legacy_key = dkim_dir.join("private.key");

        // Per-domain layout takes precedence; fall back to the legacy
        // single-key location only for the default domain (any other
        // domain has no business reading the default-domain key).
        let key_root = if per_domain_key.is_file() {
            per_domain_dir.clone()
        } else if domain_lc == default_domain && legacy_key.is_file() {
            dkim_dir.to_path_buf()
        } else {
            reports.push(DomainLoadReport {
                domain: domain_lc.clone(),
                outcome: LoadOutcome::MissingKey {
                    path: per_domain_key.clone(),
                    error: "no DKIM private.key under per-domain or legacy path".to_string(),
                },
            });
            continue;
        };

        match dkim::load_private_key(&key_root) {
            Ok(k) => {
                let selector = resolve_selector_for_domain(config, &domain_lc);
                map.insert(
                    domain_lc.clone(),
                    DkimKeyEntry {
                        key: Arc::new(k),
                        selector,
                    },
                );
                reports.push(DomainLoadReport {
                    domain: domain_lc,
                    outcome: LoadOutcome::Loaded,
                });
            }
            Err(e) => {
                reports.push(DomainLoadReport {
                    domain: domain_lc,
                    outcome: LoadOutcome::MissingKey {
                        path: key_root.join("private.key"),
                        error: e.to_string(),
                    },
                });
            }
        }
    }

    (map, reports)
}

/// Resolve a `DkimKeyEntry` from the live map for a sender's domain.
/// Returns `None` when no key is loaded for that domain — the caller
/// surfaces the canonical "no DKIM key for domain" error.
pub fn entry_for_domain<'a>(map: &'a DkimKeyMap, domain: &str) -> Option<&'a DkimKeyEntry> {
    map.get(&domain.to_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use tempfile::TempDir;

    fn write_keypair(dir: &Path) {
        std::fs::create_dir_all(dir).unwrap();
        crate::dkim::generate_keypair(dir, false).unwrap();
    }

    fn cfg(domains: &[&str]) -> Config {
        Config {
            domains: domains.iter().map(|s| s.to_string()).collect(),
            data_dir: std::path::PathBuf::from("/tmp/test"),
            dkim_selector: None,
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
    fn resolve_selector_per_domain_overrides_top_level() {
        use crate::config::DomainOverride;
        let mut c = cfg(&["a.com", "b.com"]);
        c.dkim_selector = Some("global2025".to_string());
        c.per_domain.insert(
            "b.com".to_string(),
            DomainOverride {
                dkim_selector: Some("bdkim".to_string()),
                ..Default::default()
            },
        );
        assert_eq!(resolve_selector_for_domain(&c, "a.com"), "global2025");
        assert_eq!(resolve_selector_for_domain(&c, "b.com"), "bdkim");
    }

    #[test]
    fn resolve_selector_defaults_to_aimx() {
        let c = cfg(&["a.com"]);
        assert_eq!(resolve_selector_for_domain(&c, "a.com"), "aimx");
    }

    #[test]
    fn load_per_domain_keys_lands_under_per_domain_layout() {
        let tmp = TempDir::new().unwrap();
        let dkim_dir = tmp.path().to_path_buf();
        write_keypair(&dkim_dir.join("a.com"));
        write_keypair(&dkim_dir.join("b.com"));

        let c = cfg(&["a.com", "b.com"]);
        let (map, reports) = load_dkim_keys(&c, &dkim_dir);
        assert_eq!(map.len(), 2);
        assert!(map.contains_key("a.com"));
        assert!(map.contains_key("b.com"));
        for r in &reports {
            assert!(
                matches!(r.outcome, LoadOutcome::Loaded),
                "{}: {:?}",
                r.domain,
                r.outcome
            );
        }
    }

    #[test]
    fn load_missing_domain_key_emits_warning_report_and_continues() {
        let tmp = TempDir::new().unwrap();
        let dkim_dir = tmp.path().to_path_buf();
        // Only a.com has a key on disk.
        write_keypair(&dkim_dir.join("a.com"));

        let c = cfg(&["a.com", "b.com"]);
        let (map, reports) = load_dkim_keys(&c, &dkim_dir);
        // Daemon still loads one key.
        assert_eq!(map.len(), 1);
        assert!(map.contains_key("a.com"));
        // Report for b.com flags the miss without bailing out.
        let b_report = reports.iter().find(|r| r.domain == "b.com").unwrap();
        match &b_report.outcome {
            LoadOutcome::MissingKey { .. } => {}
            other => panic!("expected MissingKey, got {other:?}"),
        }
    }

    #[test]
    fn load_legacy_layout_falls_back_for_default_domain_only() {
        let tmp = TempDir::new().unwrap();
        let dkim_dir = tmp.path().to_path_buf();
        // Legacy single-key layout: <dkim_dir>/private.key only.
        write_keypair(&dkim_dir);

        let c = cfg(&["x.com"]);
        let (map, reports) = load_dkim_keys(&c, &dkim_dir);
        assert_eq!(map.len(), 1, "default domain picks up legacy key");
        let r = &reports[0];
        assert!(matches!(r.outcome, LoadOutcome::Loaded));

        // Two-domain install with legacy key — only the default picks it up.
        let c = cfg(&["x.com", "y.com"]);
        let (map, reports) = load_dkim_keys(&c, &dkim_dir);
        assert_eq!(map.len(), 1);
        assert!(map.contains_key("x.com"));
        let y = reports.iter().find(|r| r.domain == "y.com").unwrap();
        assert!(matches!(y.outcome, LoadOutcome::MissingKey { .. }));
    }

    #[test]
    fn entry_for_domain_is_case_insensitive() {
        let tmp = TempDir::new().unwrap();
        let dkim_dir = tmp.path().to_path_buf();
        write_keypair(&dkim_dir.join("a.com"));

        let c = cfg(&["a.com"]);
        let (map, _r) = load_dkim_keys(&c, &dkim_dir);
        assert!(entry_for_domain(&map, "A.COM").is_some());
        assert!(entry_for_domain(&map, "a.com").is_some());
        assert!(entry_for_domain(&map, "other.example").is_none());
    }
}
