//! Daemon-side handler for the `DOMAIN-LIST` verb of the `AIMX/1` UDS
//! protocol.
//!
//! Mirrors `mailbox_list_handler` / `hook_list_handler` line-for-line:
//! the daemon resolves the caller via `SO_PEERCRED`, runs the central
//! `auth::authorize(.., Action::DomainCrud)` predicate (root-only),
//! walks the in-memory `Arc<Config>`, and returns a JSON array of
//! [`DomainListRow`].
//!
//! Unlike `MAILBOX-LIST` (owner-filtered) and `HOOK-LIST` (owner-
//! filtered), `DOMAIN-LIST` is strictly operator-scoped: a non-root
//! caller receives an `ERR EACCES` response. The single-operator
//! model means domains are not per-mailbox state.
//!
//! No locks are taken; reads of `Arc<Config>` are wait-free.

use std::path::Path;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::auth::{Action, authorize};
use crate::config::Config;
use crate::dkim_keys::SharedDkimKeyMap;
use crate::mailbox;
use crate::send_protocol::{ErrCode, JsonAckResponse};
use crate::state_handler::StateContext;
use crate::uds_authz::Caller;

/// One row of the JSON array returned by `DOMAIN-LIST`. Captures the
/// per-domain summary needed by `aimx domains list`; a future
/// follow-up may lift `aimx doctor` onto the same shape so both
/// surfaces report identical per-domain status.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DomainListRow {
    /// Fully-qualified domain (lowercased, RFC 1035 valid).
    pub domain: String,
    /// `true` iff this is `domains[0]` — the default domain.
    pub default: bool,
    /// `true` iff a DKIM private key is loaded for this domain in the
    /// daemon's in-memory map. A `false` here means the daemon could
    /// not read a private key at startup; outbound from this domain
    /// will fail until `aimx dkim-keygen --domain <d>` runs.
    pub dkim_loaded: bool,
    /// Resolved DKIM selector for this domain. Falls back through the
    /// documented order (per-domain → top-level → `"aimx"`).
    pub dkim_selector: String,
    /// Count of mailboxes whose `address` carries this domain. Catchall
    /// (`*@<domain>`) counts as one entry.
    pub mailbox_count: usize,
    /// Count of unread inbox messages across every mailbox in this
    /// domain. Catchall is included.
    pub unread: usize,
    /// Compact comma-separated summary of per-domain overrides that
    /// are set (`signature`, `dkim_selector`, `trust`,
    /// `trusted_senders`). Empty string when no `[domain."<d>"]`
    /// sub-table exists. UI-only convenience; the CLI renders this
    /// verbatim in the `Overrides` column.
    pub overrides: String,
}

/// Build the JSON ack response for an `AIMX/1 DOMAIN-LIST` request.
pub async fn handle_domain_list(
    state_ctx: &StateContext,
    dkim_keys: &SharedDkimKeyMap,
    caller: &Caller,
) -> JsonAckResponse {
    if let Err(e) = authorize(caller.uid, Action::DomainCrud, None) {
        return JsonAckResponse::Err {
            code: ErrCode::Eaccess,
            reason: format!("{e}"),
        };
    }

    let config = state_ctx.config_handle.load();
    let dkim_snapshot = dkim_keys.load_full();
    let rows = collect_rows(&config, &dkim_snapshot);
    let body = match serde_json::to_vec(&rows) {
        Ok(b) => b,
        Err(e) => {
            return JsonAckResponse::Err {
                code: ErrCode::Io,
                reason: format!("failed to serialize domain list: {e}"),
            };
        }
    };
    JsonAckResponse::Ok { body }
}

/// Pure helper: walk `config.domains` and emit a `DomainListRow` per
/// entry. Sorted in operator-declared order — the first entry is the
/// default, so we deliberately keep the declared order rather than
/// alphabetizing.
fn collect_rows(
    config: &Config,
    dkim_keys: &Arc<crate::dkim_keys::DkimKeyMap>,
) -> Vec<DomainListRow> {
    let mut rows: Vec<DomainListRow> = Vec::with_capacity(config.domains.len());
    let default_domain = config.default_domain().to_ascii_lowercase();

    for (idx, domain) in config.domains.iter().enumerate() {
        let domain_lc = domain.to_ascii_lowercase();
        let dkim_entry = crate::dkim_keys::entry_for_domain(dkim_keys, &domain_lc);
        let dkim_loaded = dkim_entry.is_some();
        let dkim_selector = match dkim_entry {
            Some(e) => e.selector.clone(),
            None => crate::dkim_keys::resolve_selector_for_domain(config, &domain_lc),
        };

        let (mailbox_count, unread) = count_mailboxes_and_unread(config, &domain_lc);

        rows.push(DomainListRow {
            domain: domain_lc.clone(),
            default: idx == 0 || domain_lc == default_domain,
            dkim_loaded,
            dkim_selector,
            mailbox_count,
            unread,
            overrides: format_overrides(config, &domain_lc),
        });
    }

    rows
}

/// Count the mailboxes whose `address` is `<local>@<domain>` (including
/// the catchall) and the unread message count across their inboxes.
fn count_mailboxes_and_unread(config: &Config, domain: &str) -> (usize, usize) {
    let mut count = 0usize;
    let mut unread = 0usize;
    let suffix = format!("@{domain}");

    // Use the same name set the listing path uses so on-disk-only
    // mailboxes (registered or not) are visible. We only count
    // registered mailboxes for the per-domain rollup because the
    // address-to-domain mapping requires a config entry.
    let names = mailbox::discover_mailbox_names(config);
    for name in names {
        let Some((_, mb)) = config.resolve_mailbox_by_name(&name) else {
            continue;
        };
        if !mb.address.to_ascii_lowercase().ends_with(&suffix) {
            continue;
        }
        count += 1;
        let inbox_dir = config.inbox_dir(&name);
        unread += count_unread(&inbox_dir);
    }
    (count, unread)
}

/// Walk `dir`, counting `.md` files (and bundle dirs containing a
/// matching inner `<stem>.md`) whose frontmatter does not carry
/// `read = true`. Mirrors the cheap line-scan from
/// `mailbox_list_handler::count_inbox`.
fn count_unread(dir: &Path) -> usize {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return 0,
    };
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
        if !is_marked_read(&md_path) {
            unread += 1;
        }
    }
    unread
}

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

/// Format the per-domain override summary for the `Overrides` column.
/// Empty string when no override exists; otherwise a comma-separated
/// list of the field names that are set.
fn format_overrides(config: &Config, domain: &str) -> String {
    let Some(over) = config.per_domain.get(domain) else {
        return String::new();
    };
    let mut parts: Vec<&'static str> = Vec::new();
    if over.signature.is_some() {
        parts.push("signature");
    }
    if over.dkim_selector.is_some() {
        parts.push("dkim_selector");
    }
    if over.trust.is_some() {
        parts.push("trust");
    }
    if over.trusted_senders.is_some() {
        parts.push("trusted_senders");
    }
    parts.join(",")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, ConfigHandle, DomainOverride, MailboxConfig};
    use crate::dkim_keys::DkimKeyEntry;
    use rsa::RsaPrivateKey;
    use std::collections::HashMap;
    use tempfile::TempDir;

    fn fake_resolver(name: &str) -> Option<crate::user_resolver::ResolvedUser> {
        match name {
            "root" => Some(crate::user_resolver::ResolvedUser {
                name: "root".to_string(),
                uid: 0,
                gid: 0,
            }),
            "aimx-catchall" => Some(crate::user_resolver::ResolvedUser {
                name: "aimx-catchall".to_string(),
                uid: 4242,
                gid: 4242,
            }),
            _ => None,
        }
    }

    fn install_resolver() -> crate::user_resolver::test_resolver::ResolverOverride {
        crate::user_resolver::set_test_resolver(fake_resolver)
    }

    fn dummy_key() -> Arc<RsaPrivateKey> {
        // Generate once per test — 2048-bit keygen is ~200ms, acceptable
        // for the handful of test cases below.
        let mut rng = rsa::rand_core::OsRng;
        Arc::new(RsaPrivateKey::new(&mut rng, 2048).unwrap())
    }

    fn config_two_domains(data_dir: &Path) -> Config {
        let mut mailboxes = HashMap::new();
        mailboxes.insert(
            "info@a.com".to_string(),
            MailboxConfig {
                address: "info@a.com".to_string(),
                owner: "root".to_string(),
                hooks: vec![],
                trust: None,
                trusted_senders: None,
                allow_root_catchall: false,
            },
        );
        mailboxes.insert(
            "support@b.com".to_string(),
            MailboxConfig {
                address: "support@b.com".to_string(),
                owner: "root".to_string(),
                hooks: vec![],
                trust: None,
                trusted_senders: None,
                allow_root_catchall: false,
            },
        );
        let mut per_domain = HashMap::new();
        per_domain.insert(
            "b.com".to_string(),
            DomainOverride {
                dkim_selector: Some("s2025".to_string()),
                trust: Some("verified".to_string()),
                ..Default::default()
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

    #[tokio::test]
    async fn root_sees_every_domain_with_default_marker() {
        let _r = install_resolver();
        let tmp = TempDir::new().unwrap();
        let config = config_two_domains(tmp.path());
        let handle = ConfigHandle::new(config);
        let state_ctx = StateContext::new(tmp.path().to_path_buf(), handle);

        let key = dummy_key();
        let mut map: crate::dkim_keys::DkimKeyMap = HashMap::new();
        map.insert(
            "a.com".to_string(),
            DkimKeyEntry {
                key: Arc::clone(&key),
                selector: "aimx".to_string(),
            },
        );
        map.insert(
            "b.com".to_string(),
            DkimKeyEntry {
                key: Arc::clone(&key),
                selector: "s2025".to_string(),
            },
        );
        let shared: SharedDkimKeyMap = Arc::new(arc_swap::ArcSwap::from_pointee(map));

        let resp = handle_domain_list(&state_ctx, &shared, &Caller::internal_root()).await;
        let body = match resp {
            JsonAckResponse::Ok { body } => body,
            other => panic!("expected Ok, got {other:?}"),
        };
        let rows: Vec<DomainListRow> = serde_json::from_slice(&body).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].domain, "a.com");
        assert!(rows[0].default);
        assert!(rows[0].dkim_loaded);
        assert_eq!(rows[0].dkim_selector, "aimx");
        assert_eq!(rows[0].mailbox_count, 1);
        assert_eq!(rows[0].overrides, "");

        assert_eq!(rows[1].domain, "b.com");
        assert!(!rows[1].default);
        assert!(rows[1].dkim_loaded);
        assert_eq!(rows[1].dkim_selector, "s2025");
        assert_eq!(rows[1].mailbox_count, 1);
        assert!(rows[1].overrides.contains("dkim_selector"));
        assert!(rows[1].overrides.contains("trust"));
    }

    /// A non-root caller is denied with an EACCES error. Domain
    /// management is operator-only.
    #[tokio::test]
    async fn non_root_caller_denied_with_eacces() {
        let _r = install_resolver();
        let tmp = TempDir::new().unwrap();
        let config = config_two_domains(tmp.path());
        let handle = ConfigHandle::new(config);
        let state_ctx = StateContext::new(tmp.path().to_path_buf(), handle);

        let empty: SharedDkimKeyMap = crate::dkim_keys::empty_shared();
        let stranger = Caller::new(1000, 1000, None);
        let resp = handle_domain_list(&state_ctx, &empty, &stranger).await;
        match resp {
            JsonAckResponse::Err { code, .. } => {
                assert_eq!(code, ErrCode::Eaccess);
            }
            other => panic!("expected Err EACCES, got {other:?}"),
        }
    }

    /// Missing DKIM key for a configured domain renders `dkim_loaded =
    /// false` and falls back to the resolved selector (no panic on
    /// absent entry).
    #[tokio::test]
    async fn missing_dkim_key_renders_unloaded_with_fallback_selector() {
        let _r = install_resolver();
        let tmp = TempDir::new().unwrap();
        let config = config_two_domains(tmp.path());
        let handle = ConfigHandle::new(config);
        let state_ctx = StateContext::new(tmp.path().to_path_buf(), handle);

        // Empty map → both domains report dkim_loaded = false but the
        // selector comes from the resolution helper.
        let empty: SharedDkimKeyMap = crate::dkim_keys::empty_shared();
        let resp = handle_domain_list(&state_ctx, &empty, &Caller::internal_root()).await;
        let body = match resp {
            JsonAckResponse::Ok { body } => body,
            other => panic!("expected Ok, got {other:?}"),
        };
        let rows: Vec<DomainListRow> = serde_json::from_slice(&body).unwrap();
        assert!(!rows[0].dkim_loaded);
        assert_eq!(rows[0].dkim_selector, "aimx");
        assert!(!rows[1].dkim_loaded);
        // b.com still picks up its per-domain selector override.
        assert_eq!(rows[1].dkim_selector, "s2025");
    }

    /// Per-domain overrides are surfaced as a comma-separated list of
    /// the field names that are set.
    #[test]
    fn format_overrides_reports_each_set_field() {
        let mut config = Config {
            domains: vec!["x.com".into()],
            data_dir: std::path::PathBuf::from("/tmp/x"),
            dkim_selector: None,
            trust: "none".into(),
            trusted_senders: vec![],
            mailboxes: HashMap::new(),
            per_domain: HashMap::new(),
            verify_host: None,
            enable_ipv6: false,
            signature: None,
            upgrade: None,
        };
        config.per_domain.insert(
            "x.com".to_string(),
            DomainOverride {
                signature: Some("sig".into()),
                dkim_selector: Some("s".into()),
                trust: None,
                trusted_senders: Some(vec!["*@a.com".into()]),
            },
        );
        let summary = format_overrides(&config, "x.com");
        assert!(summary.contains("signature"));
        assert!(summary.contains("dkim_selector"));
        assert!(summary.contains("trusted_senders"));
        assert!(!summary.contains("trust,"));
    }

    /// `DomainListRow` round-trips through serde, pinning the wire
    /// shape against accidental drift.
    #[test]
    fn domain_list_row_serde_round_trip() {
        let row = DomainListRow {
            domain: "a.com".into(),
            default: true,
            dkim_loaded: true,
            dkim_selector: "aimx".into(),
            mailbox_count: 2,
            unread: 0,
            overrides: String::new(),
        };
        let json = serde_json::to_string(&row).unwrap();
        let decoded: DomainListRow = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, row);
    }
}
