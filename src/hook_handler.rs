//! Daemon-side handlers for the `HOOK-CREATE` and `HOOK-DELETE` verbs of
//! the `AIMX/1` UDS protocol.
//!
//! `aimx hooks create` / `hooks delete` route through the daemon over UDS
//! so the in-memory `Config` is hot-swapped under a `RwLock<Arc<Config>>`
//! whenever `config.toml` on disk changes. Newly-created hooks fire on
//! the very next ingest / after-send event — no restart required.
//!
//! Correctness model is symmetric to [`crate::mailbox_handler`]:
//!
//! 1. Validate the submitted hook (well-formed name if present, non-empty
//!    cmd, supported type, trust gate only on `on_receive`).
//! 2. Load the current `Config` snapshot through the shared
//!    `ConfigHandle`. Re-derive the new snapshot in memory, either
//!    appending the new hook to the addressed mailbox's `hooks` array
//!    (CREATE) or removing the hook whose effective name matches (DELETE).
//! 3. Write atomically via `write_config_atomic` (write-temp-then-rename
//!    — shared with `mailbox_handler`).
//! 4. After the rename succeeds, swap the in-memory `Config` via
//!    `ConfigHandle::store`.
//!
//! Locking follows the same outer-per-mailbox / inner-`CONFIG_WRITE_LOCK`
//! hierarchy as the MAILBOX-CRUD path (see [`crate::mailbox_locks`]).

use crate::config::{Config, validate_hooks, validate_single_hook};
use crate::hook::{Hook, effective_hook_name, is_valid_hook_name};
use crate::mailbox_handler::{CONFIG_WRITE_LOCK, MailboxContext, write_config_atomic};
use crate::send_protocol::{AckResponse, ErrCode, HookCreateRequest, HookDeleteRequest};
use crate::state_handler::StateContext;

/// Handle an `AIMX/1 HOOK-CREATE` request. Takes the per-mailbox write
/// lock for the addressed mailbox (outer) plus `CONFIG_WRITE_LOCK`
/// (inner) while the config rewrite + handle swap runs.
pub async fn handle_hook_create(
    state_ctx: &StateContext,
    mb_ctx: &MailboxContext,
    req: &HookCreateRequest,
) -> AckResponse {
    let hook = match decode_hook_body(&req.hook_toml) {
        Ok(h) => h,
        Err(e) => {
            return AckResponse::Err {
                code: ErrCode::Validation,
                reason: e,
            };
        }
    };

    if let Err(reason) = validate_single_hook(&hook) {
        return AckResponse::Err {
            code: ErrCode::Validation,
            reason,
        };
    }

    let lock = state_ctx.lock_for(&req.mailbox);
    let _guard = lock.lock().await;

    let _config_guard = CONFIG_WRITE_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());

    let current = mb_ctx.config_handle.load();

    if !current.mailboxes.contains_key(&req.mailbox) {
        return AckResponse::Err {
            code: ErrCode::Mailbox,
            reason: format!("mailbox '{}' does not exist", req.mailbox),
        };
    }

    let new_effective = effective_hook_name(&hook);
    for (mb_name, mb) in &current.mailboxes {
        for existing in &mb.hooks {
            if effective_hook_name(existing) == new_effective {
                return AckResponse::Err {
                    code: ErrCode::Validation,
                    reason: format!(
                        "hook name '{new_effective}' already exists on mailbox '{mb_name}'"
                    ),
                };
            }
        }
    }

    let mut new_config: Config = (*current).clone();
    if let Some(mb) = new_config.mailboxes.get_mut(&req.mailbox) {
        mb.hooks.push(hook);
    }

    // Re-run the full load-time validator so the daemon refuses to write
    // a config that would fail on next start.
    if let Err(reason) = validate_hooks(&new_config) {
        return AckResponse::Err {
            code: ErrCode::Validation,
            reason,
        };
    }

    if let Err(e) = write_config_atomic(&mb_ctx.config_path, &new_config) {
        return AckResponse::Err {
            code: ErrCode::Io,
            reason: format!("failed to write {}: {e}", mb_ctx.config_path.display()),
        };
    }

    mb_ctx.config_handle.store(new_config);
    AckResponse::Ok
}

/// Handle an `AIMX/1 HOOK-DELETE` request. Locates the hook by effective
/// name across every configured mailbox. Takes the per-mailbox lock for
/// the owning mailbox once it has been resolved, plus the global
/// `CONFIG_WRITE_LOCK`.
pub async fn handle_hook_delete(
    state_ctx: &StateContext,
    mb_ctx: &MailboxContext,
    req: &HookDeleteRequest,
) -> AckResponse {
    if !is_valid_hook_name(&req.name) {
        return AckResponse::Err {
            code: ErrCode::Validation,
            reason: format!(
                "invalid hook name '{}': must match \
                 [a-zA-Z0-9_][a-zA-Z0-9_.-]{{0,127}}",
                req.name
            ),
        };
    }

    let current = mb_ctx.config_handle.load();
    let owner = current.mailboxes.iter().find_map(|(name, mb)| {
        mb.hooks
            .iter()
            .any(|h| effective_hook_name(h) == req.name)
            .then(|| name.clone())
    });
    let owner = match owner {
        Some(n) => n,
        None => {
            return AckResponse::Err {
                code: ErrCode::NotFound,
                reason: format!("hook '{}' not found", req.name),
            };
        }
    };

    let lock = state_ctx.lock_for(&owner);
    let _guard = lock.lock().await;

    let _config_guard = CONFIG_WRITE_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());

    // Re-resolve under the lock: owner may have changed if somebody
    // reconfigured between our snapshot above and acquiring the lock.
    let current = mb_ctx.config_handle.load();
    let mut new_config: Config = (*current).clone();
    let mut removed = false;
    for mb in new_config.mailboxes.values_mut() {
        let before = mb.hooks.len();
        mb.hooks.retain(|h| effective_hook_name(h) != req.name);
        if mb.hooks.len() != before {
            removed = true;
            break;
        }
    }
    if !removed {
        return AckResponse::Err {
            code: ErrCode::NotFound,
            reason: format!("hook '{}' not found", req.name),
        };
    }

    if let Err(e) = write_config_atomic(&mb_ctx.config_path, &new_config) {
        return AckResponse::Err {
            code: ErrCode::Io,
            reason: format!("failed to write {}: {e}", mb_ctx.config_path.display()),
        };
    }

    mb_ctx.config_handle.store(new_config);
    AckResponse::Ok
}

fn decode_hook_body(bytes: &[u8]) -> Result<Hook, String> {
    let s = std::str::from_utf8(bytes).map_err(|e| format!("hook body is not valid UTF-8: {e}"))?;
    toml::from_str::<Hook>(s).map_err(|e| format!("malformed hook TOML: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{ConfigHandle, MailboxConfig};
    use crate::hook::{Hook, HookEvent};
    use std::collections::HashMap;
    use std::path::Path;
    use tempfile::TempDir;

    fn base_config(data_dir: &Path) -> Config {
        let mut mailboxes = HashMap::new();
        mailboxes.insert(
            "catchall".to_string(),
            MailboxConfig {
                address: "*@example.com".to_string(),
                hooks: vec![],
                trust: None,
                trusted_senders: None,
            },
        );
        mailboxes.insert(
            "alice".to_string(),
            MailboxConfig {
                address: "alice@example.com".to_string(),
                hooks: vec![],
                trust: None,
                trusted_senders: None,
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
        }
    }

    fn contexts(tmp: &TempDir) -> (StateContext, MailboxContext) {
        let config = base_config(tmp.path());
        let handle = ConfigHandle::new(config);
        let state_ctx = StateContext::new(tmp.path().to_path_buf(), handle.clone());
        let config_path = tmp.path().join("config.toml");
        write_config_atomic(&config_path, &handle.load()).unwrap();
        let mb_ctx = MailboxContext::new(config_path, handle);
        (state_ctx, mb_ctx)
    }

    fn hook_toml(hook: &Hook) -> Vec<u8> {
        toml::to_string_pretty(hook).unwrap().into_bytes()
    }

    fn sample_hook(name: Option<&str>, cmd: &str) -> Hook {
        Hook {
            name: name.map(str::to_string),
            event: HookEvent::OnReceive,
            r#type: "cmd".into(),
            cmd: cmd.into(),
            dangerously_support_untrusted: false,
        }
    }

    #[tokio::test]
    async fn hook_create_appends_and_swaps_handle() {
        let tmp = TempDir::new().unwrap();
        let (state_ctx, mb_ctx) = contexts(&tmp);

        let hook = sample_hook(Some("my_hook"), "echo hi");
        let req = HookCreateRequest {
            mailbox: "alice".into(),
            hook_toml: hook_toml(&hook),
        };
        match handle_hook_create(&state_ctx, &mb_ctx, &req).await {
            AckResponse::Ok => {}
            other => panic!("expected Ok, got {other:?}"),
        }

        let live = mb_ctx.config_handle.load();
        assert_eq!(live.mailboxes["alice"].hooks.len(), 1);
        assert_eq!(
            live.mailboxes["alice"].hooks[0].name.as_deref(),
            Some("my_hook")
        );

        let reloaded = Config::load(&mb_ctx.config_path).unwrap();
        assert_eq!(reloaded.mailboxes["alice"].hooks.len(), 1);
    }

    #[tokio::test]
    async fn hook_create_anonymous_succeeds() {
        let tmp = TempDir::new().unwrap();
        let (state_ctx, mb_ctx) = contexts(&tmp);

        let hook = sample_hook(None, "echo anon");
        let req = HookCreateRequest {
            mailbox: "alice".into(),
            hook_toml: hook_toml(&hook),
        };
        assert!(matches!(
            handle_hook_create(&state_ctx, &mb_ctx, &req).await,
            AckResponse::Ok
        ));
        let reloaded = Config::load(&mb_ctx.config_path).unwrap();
        assert!(reloaded.mailboxes["alice"].hooks[0].name.is_none());
    }

    #[tokio::test]
    async fn hook_create_rejects_unknown_mailbox() {
        let tmp = TempDir::new().unwrap();
        let (state_ctx, mb_ctx) = contexts(&tmp);

        let req = HookCreateRequest {
            mailbox: "ghost".into(),
            hook_toml: hook_toml(&sample_hook(Some("h"), "echo hi")),
        };
        match handle_hook_create(&state_ctx, &mb_ctx, &req).await {
            AckResponse::Err { code, reason } => {
                assert_eq!(code, ErrCode::Mailbox);
                assert!(reason.contains("ghost"), "{reason}");
            }
            other => panic!("expected Err(MAILBOX), got {other:?}"),
        }
    }

    #[tokio::test]
    async fn hook_create_rejects_dangerous_on_after_send() {
        let tmp = TempDir::new().unwrap();
        let (state_ctx, mb_ctx) = contexts(&tmp);

        let hook = Hook {
            name: Some("h1".into()),
            event: HookEvent::AfterSend,
            r#type: "cmd".into(),
            cmd: "echo hi".into(),
            dangerously_support_untrusted: true,
        };
        let req = HookCreateRequest {
            mailbox: "alice".into(),
            hook_toml: hook_toml(&hook),
        };
        match handle_hook_create(&state_ctx, &mb_ctx, &req).await {
            AckResponse::Err { code, reason } => {
                assert_eq!(code, ErrCode::Validation);
                assert!(reason.contains("dangerously_support_untrusted"), "{reason}");
            }
            other => panic!("expected Err(VALIDATION), got {other:?}"),
        }
    }

    #[tokio::test]
    async fn hook_create_rejects_bad_name() {
        let tmp = TempDir::new().unwrap();
        let (state_ctx, mb_ctx) = contexts(&tmp);

        let hook = sample_hook(Some("bad name!"), "echo hi");
        let req = HookCreateRequest {
            mailbox: "alice".into(),
            hook_toml: hook_toml(&hook),
        };
        match handle_hook_create(&state_ctx, &mb_ctx, &req).await {
            AckResponse::Err { code, reason } => {
                assert_eq!(code, ErrCode::Validation);
                assert!(reason.contains("invalid hook name"), "{reason}");
            }
            other => panic!("expected Err(VALIDATION), got {other:?}"),
        }
    }

    #[tokio::test]
    async fn hook_create_rejects_duplicate_name() {
        let tmp = TempDir::new().unwrap();
        let (state_ctx, mb_ctx) = contexts(&tmp);

        let hook = sample_hook(Some("dup"), "echo hi");
        let req = HookCreateRequest {
            mailbox: "alice".into(),
            hook_toml: hook_toml(&hook),
        };
        assert!(matches!(
            handle_hook_create(&state_ctx, &mb_ctx, &req).await,
            AckResponse::Ok
        ));

        let req2 = HookCreateRequest {
            mailbox: "catchall".into(),
            hook_toml: hook_toml(&hook),
        };
        match handle_hook_create(&state_ctx, &mb_ctx, &req2).await {
            AckResponse::Err { code, reason } => {
                assert_eq!(code, ErrCode::Validation);
                assert!(reason.contains("already exists"), "{reason}");
            }
            other => panic!("expected Err(VALIDATION), got {other:?}"),
        }
    }

    #[tokio::test]
    async fn hook_create_rejects_malformed_toml() {
        let tmp = TempDir::new().unwrap();
        let (state_ctx, mb_ctx) = contexts(&tmp);

        let req = HookCreateRequest {
            mailbox: "alice".into(),
            hook_toml: b"not toml at all".to_vec(),
        };
        match handle_hook_create(&state_ctx, &mb_ctx, &req).await {
            AckResponse::Err { code, reason } => {
                assert_eq!(code, ErrCode::Validation);
                assert!(
                    reason.to_lowercase().contains("toml")
                        || reason.to_lowercase().contains("expected"),
                    "{reason}"
                );
            }
            other => panic!("expected Err(VALIDATION), got {other:?}"),
        }
    }

    #[tokio::test]
    async fn hook_delete_removes_and_swaps_handle() {
        let tmp = TempDir::new().unwrap();
        let (state_ctx, mb_ctx) = contexts(&tmp);

        let hook = sample_hook(Some("to_delete"), "echo hi");
        let create = HookCreateRequest {
            mailbox: "alice".into(),
            hook_toml: hook_toml(&hook),
        };
        assert!(matches!(
            handle_hook_create(&state_ctx, &mb_ctx, &create).await,
            AckResponse::Ok
        ));

        let del = HookDeleteRequest {
            name: "to_delete".into(),
        };
        assert!(matches!(
            handle_hook_delete(&state_ctx, &mb_ctx, &del).await,
            AckResponse::Ok
        ));

        let live = mb_ctx.config_handle.load();
        assert!(live.mailboxes["alice"].hooks.is_empty());
        let reloaded = Config::load(&mb_ctx.config_path).unwrap();
        assert!(reloaded.mailboxes["alice"].hooks.is_empty());
    }

    #[tokio::test]
    async fn hook_delete_anonymous_by_derived_name() {
        let tmp = TempDir::new().unwrap();
        let (state_ctx, mb_ctx) = contexts(&tmp);

        let hook = sample_hook(None, "echo anon");
        let derived = effective_hook_name(&hook);
        let create = HookCreateRequest {
            mailbox: "alice".into(),
            hook_toml: hook_toml(&hook),
        };
        assert!(matches!(
            handle_hook_create(&state_ctx, &mb_ctx, &create).await,
            AckResponse::Ok
        ));
        let del = HookDeleteRequest { name: derived };
        assert!(matches!(
            handle_hook_delete(&state_ctx, &mb_ctx, &del).await,
            AckResponse::Ok
        ));
    }

    #[tokio::test]
    async fn hook_delete_unknown_name_returns_notfound() {
        let tmp = TempDir::new().unwrap();
        let (state_ctx, mb_ctx) = contexts(&tmp);
        let del = HookDeleteRequest {
            name: "nope".into(),
        };
        match handle_hook_delete(&state_ctx, &mb_ctx, &del).await {
            AckResponse::Err { code, .. } => assert_eq!(code, ErrCode::NotFound),
            other => panic!("expected Err(NOTFOUND), got {other:?}"),
        }
    }

    #[tokio::test]
    async fn hook_delete_invalid_name_rejected() {
        let tmp = TempDir::new().unwrap();
        let (state_ctx, mb_ctx) = contexts(&tmp);
        let del = HookDeleteRequest {
            name: "bad name!".into(),
        };
        match handle_hook_delete(&state_ctx, &mb_ctx, &del).await {
            AckResponse::Err { code, .. } => assert_eq!(code, ErrCode::Validation),
            other => panic!("expected Err(VALIDATION), got {other:?}"),
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_create_different_mailboxes_both_survive() {
        // Mirrors the `MAILBOX-CREATE` lost-update regression test: two
        // concurrent `HOOK-CREATE` on different mailboxes must both land
        // in the final config.
        let tmp = TempDir::new().unwrap();
        let (state_ctx, mb_ctx) = contexts(&tmp);
        let state_ctx = std::sync::Arc::new(state_ctx);
        let mb_ctx = std::sync::Arc::new(mb_ctx);

        let mut handles = Vec::new();
        let pairs = [("alice", "hook_a"), ("catchall", "hook_b")];
        for (mbox, name) in pairs {
            let s = state_ctx.clone();
            let m = mb_ctx.clone();
            let mbox = mbox.to_string();
            let name = name.to_string();
            handles.push(tokio::spawn(async move {
                let req = HookCreateRequest {
                    mailbox: mbox.clone(),
                    hook_toml: hook_toml(&sample_hook(Some(&name), "echo hi")),
                };
                handle_hook_create(&s, &m, &req).await
            }));
        }
        for h in handles {
            assert!(matches!(h.await.unwrap(), AckResponse::Ok));
        }

        let reloaded = Config::load(&mb_ctx.config_path).unwrap();
        assert_eq!(reloaded.mailboxes["alice"].hooks.len(), 1);
        assert_eq!(reloaded.mailboxes["catchall"].hooks.len(), 1);
    }
}
