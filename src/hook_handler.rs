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
//! 1. Validate the submitted hook (well-formed id, event-specific filter
//!    rules, non-empty cmd, supported type, trust gate only on
//!    `on_receive`).
//! 2. Load the current `Config` snapshot through the shared
//!    `ConfigHandle`. Re-derive the new snapshot in memory, either
//!    appending the new hook to the addressed mailbox's `hooks` array
//!    (CREATE) or removing the hook with the matching `id` across all
//!    mailboxes (DELETE).
//! 3. Write atomically via `write_config_atomic` (write-temp-then-rename
//!    — shared with `mailbox_handler`).
//! 4. After the rename succeeds, swap the in-memory `Config` via
//!    `ConfigHandle::store`.
//!
//! Locking follows the same outer-per-mailbox / inner-`CONFIG_WRITE_LOCK`
//! hierarchy as the MAILBOX-CRUD path (see [`crate::mailbox_locks`]).

use crate::config::{Config, validate_hooks};
use crate::hook::{Hook, HookEvent, is_valid_hook_id};
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

    if let Err(reason) = validate_submitted_hook(&hook) {
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

    // Duplicate id across any mailbox is rejected (hook ids are globally
    // unique — same invariant `validate_hooks` enforces on load).
    for (mb_name, mb) in &current.mailboxes {
        for existing in &mb.hooks {
            if existing.id == hook.id {
                return AckResponse::Err {
                    code: ErrCode::Validation,
                    reason: format!(
                        "hook id '{}' already exists on mailbox '{mb_name}'",
                        hook.id
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
    // a config that would fail on next start — catches any cross-hook
    // invariant we forgot to check above.
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

/// Handle an `AIMX/1 HOOK-DELETE` request. Locates the hook by id across
/// every configured mailbox. Takes the per-mailbox lock for the owning
/// mailbox once it has been resolved, plus the global `CONFIG_WRITE_LOCK`.
pub async fn handle_hook_delete(
    state_ctx: &StateContext,
    mb_ctx: &MailboxContext,
    req: &HookDeleteRequest,
) -> AckResponse {
    if !is_valid_hook_id(&req.id) {
        return AckResponse::Err {
            code: ErrCode::Validation,
            reason: format!(
                "invalid hook id '{}': must be 12 chars, [a-z0-9] only",
                req.id
            ),
        };
    }

    // Resolve owning mailbox from a snapshot. If somebody mutates the
    // config between this lookup and our write, the outer lock still
    // serializes us on that mailbox's tree; the inner `CONFIG_WRITE_LOCK`
    // serializes the rewrite across all names.
    let current = mb_ctx.config_handle.load();
    let owner = current.mailboxes.iter().find_map(|(name, mb)| {
        mb.hooks
            .iter()
            .any(|h| h.id == req.id)
            .then(|| name.clone())
    });
    let owner = match owner {
        Some(n) => n,
        None => {
            return AckResponse::Err {
                code: ErrCode::NotFound,
                reason: format!("hook id '{}' not found", req.id),
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
        mb.hooks.retain(|h| h.id != req.id);
        if mb.hooks.len() != before {
            removed = true;
            break;
        }
    }
    if !removed {
        return AckResponse::Err {
            code: ErrCode::NotFound,
            reason: format!("hook id '{}' not found", req.id),
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

/// Pre-disk validation of a submitted hook stanza. Mirrors the per-hook
/// checks [`validate_hooks`] applies on load so the daemon returns the
/// same errors the operator would see from a hand-edited file.
fn validate_submitted_hook(hook: &Hook) -> Result<(), String> {
    if !is_valid_hook_id(&hook.id) {
        return Err(format!(
            "invalid hook id '{}': must be 12 chars, [a-z0-9] only",
            hook.id
        ));
    }
    if hook.cmd.trim().is_empty() {
        return Err("hook has empty `cmd`".into());
    }
    if hook.r#type != "cmd" {
        return Err(format!(
            "hook has unsupported type '{}': only `cmd` is supported",
            hook.r#type
        ));
    }
    match hook.event {
        HookEvent::OnReceive => {
            if hook.to.is_some() {
                return Err("`to` filter is only valid on `after_send` hooks".into());
            }
        }
        HookEvent::AfterSend => {
            if hook.from.is_some() {
                return Err("`from` filter is only valid on `on_receive` hooks".into());
            }
            if hook.has_attachment.is_some() {
                return Err(
                    "`has_attachment` filter is only valid on `on_receive` hooks (outbound \
                     submissions via UDS are text-only in v0.2)"
                        .into(),
                );
            }
            if hook.dangerously_support_untrusted {
                return Err(
                    "`dangerously_support_untrusted = true` only applies to `on_receive` hooks"
                        .into(),
                );
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{ConfigHandle, MailboxConfig};
    use crate::hook::Hook;
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

    fn sample_hook(id: &str) -> Hook {
        Hook {
            id: id.to_string(),
            event: HookEvent::OnReceive,
            r#type: "cmd".into(),
            cmd: "echo hi".into(),
            from: None,
            to: None,
            subject: None,
            has_attachment: None,
            dangerously_support_untrusted: false,
        }
    }

    #[tokio::test]
    async fn hook_create_appends_and_swaps_handle() {
        let tmp = TempDir::new().unwrap();
        let (state_ctx, mb_ctx) = contexts(&tmp);

        let hook = Hook {
            from: Some("*@example.com".into()),
            ..sample_hook("aaaabbbbcccc")
        };
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
        assert_eq!(live.mailboxes["alice"].hooks[0].id, "aaaabbbbcccc");

        let reloaded = Config::load(&mb_ctx.config_path).unwrap();
        assert_eq!(reloaded.mailboxes["alice"].hooks.len(), 1);
    }

    #[tokio::test]
    async fn hook_create_rejects_unknown_mailbox() {
        let tmp = TempDir::new().unwrap();
        let (state_ctx, mb_ctx) = contexts(&tmp);

        let req = HookCreateRequest {
            mailbox: "ghost".into(),
            hook_toml: hook_toml(&sample_hook("aaaabbbbcccc")),
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
    async fn hook_create_rejects_bad_event_filter_combo() {
        let tmp = TempDir::new().unwrap();
        let (state_ctx, mb_ctx) = contexts(&tmp);

        let hook = Hook {
            event: HookEvent::AfterSend,
            from: Some("*@example.com".into()),
            ..sample_hook("aaaabbbbcccc")
        };
        let req = HookCreateRequest {
            mailbox: "alice".into(),
            hook_toml: hook_toml(&hook),
        };
        match handle_hook_create(&state_ctx, &mb_ctx, &req).await {
            AckResponse::Err { code, reason } => {
                assert_eq!(code, ErrCode::Validation);
                assert!(reason.contains("from"), "{reason}");
            }
            other => panic!("expected Err(VALIDATION), got {other:?}"),
        }
    }

    #[tokio::test]
    async fn hook_create_rejects_dangerous_on_after_send() {
        let tmp = TempDir::new().unwrap();
        let (state_ctx, mb_ctx) = contexts(&tmp);

        let hook = Hook {
            event: HookEvent::AfterSend,
            dangerously_support_untrusted: true,
            ..sample_hook("aaaabbbbcccc")
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
    async fn hook_create_rejects_bad_id() {
        let tmp = TempDir::new().unwrap();
        let (state_ctx, mb_ctx) = contexts(&tmp);

        let hook = sample_hook("TOO-SHORT");
        let req = HookCreateRequest {
            mailbox: "alice".into(),
            hook_toml: hook_toml(&hook),
        };
        match handle_hook_create(&state_ctx, &mb_ctx, &req).await {
            AckResponse::Err { code, reason } => {
                assert_eq!(code, ErrCode::Validation);
                assert!(reason.contains("invalid hook id"), "{reason}");
            }
            other => panic!("expected Err(VALIDATION), got {other:?}"),
        }
    }

    #[tokio::test]
    async fn hook_create_rejects_duplicate_id() {
        let tmp = TempDir::new().unwrap();
        let (state_ctx, mb_ctx) = contexts(&tmp);

        let hook = sample_hook("aaaabbbbcccc");
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

        let hook = sample_hook("aaaabbbbcccc");
        let create = HookCreateRequest {
            mailbox: "alice".into(),
            hook_toml: hook_toml(&hook),
        };
        assert!(matches!(
            handle_hook_create(&state_ctx, &mb_ctx, &create).await,
            AckResponse::Ok
        ));

        let del = HookDeleteRequest {
            id: "aaaabbbbcccc".into(),
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
    async fn hook_delete_unknown_id_returns_notfound() {
        let tmp = TempDir::new().unwrap();
        let (state_ctx, mb_ctx) = contexts(&tmp);
        let del = HookDeleteRequest {
            id: "aaaabbbbcccc".into(),
        };
        match handle_hook_delete(&state_ctx, &mb_ctx, &del).await {
            AckResponse::Err { code, .. } => assert_eq!(code, ErrCode::NotFound),
            other => panic!("expected Err(NOTFOUND), got {other:?}"),
        }
    }

    #[tokio::test]
    async fn hook_delete_invalid_id_rejected() {
        let tmp = TempDir::new().unwrap();
        let (state_ctx, mb_ctx) = contexts(&tmp);
        let del = HookDeleteRequest { id: "BOGUS".into() };
        match handle_hook_delete(&state_ctx, &mb_ctx, &del).await {
            AckResponse::Err { code, .. } => assert_eq!(code, ErrCode::Validation),
            other => panic!("expected Err(VALIDATION), got {other:?}"),
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_create_different_mailboxes_both_survive() {
        // Mirrors the `MAILBOX-CREATE` lost-update regression test: two
        // concurrent `HOOK-CREATE` on different mailboxes must both land
        // in the final config. Without `CONFIG_WRITE_LOCK`, two threads
        // would each clone the snapshot, append their hook to disjoint
        // names, and write-temp-then-rename — clobbering one stanza.
        let tmp = TempDir::new().unwrap();
        let (state_ctx, mb_ctx) = contexts(&tmp);
        let state_ctx = std::sync::Arc::new(state_ctx);
        let mb_ctx = std::sync::Arc::new(mb_ctx);

        let mut handles = Vec::new();
        let names = ["alice", "catchall"];
        let ids = ["aaaabbbbcccc", "ddddeeeeffff"];
        for (mbox, id) in names.iter().zip(ids.iter()) {
            let s = state_ctx.clone();
            let m = mb_ctx.clone();
            let mbox = mbox.to_string();
            let id = id.to_string();
            handles.push(tokio::spawn(async move {
                let req = HookCreateRequest {
                    mailbox: mbox.clone(),
                    hook_toml: hook_toml(&sample_hook(&id)),
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
