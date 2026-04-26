//! Daemon-side handlers for the `HOOK-CREATE` and `HOOK-DELETE` verbs of
//! the `AIMX/1` UDS protocol.
//!
//! Authorization: caller uid must match the target mailbox's owner uid,
//! or be root. The check runs **before** any state mutation so a
//! non-owner never observes a partial write.
//!
//! On success the handler rewrites `config.toml` atomically (write-temp-
//! then-rename) and swaps the live `Config` snapshot through
//! [`ConfigHandle::store`]. The same lock hierarchy as `MAILBOX-CRUD`
//! applies: outer per-mailbox lock, inner process-wide
//! [`mailbox_handler::CONFIG_WRITE_LOCK`].
//!
//! Body parser (`HOOK-CREATE`): JSON object with `cmd: [String]` plus
//! optional `fire_on_untrusted: bool` and `type: String` (default
//! `"cmd"`). The legacy `template`, `params`, `run_as`, `origin`,
//! `dangerously_support_untrusted` fields are rejected with
//! `ERR PROTOCOL` for symmetry with `Config::load`.

use serde::Deserialize;

use crate::config::{Config, validate_hooks, validate_single_hook};
use crate::hook::{Hook, HookEvent, effective_hook_name, is_valid_hook_name};
use crate::mailbox_handler::{CONFIG_WRITE_LOCK, MailboxContext, write_config_atomic};
use crate::send_protocol::{AckResponse, ErrCode, HookCreateRequest, HookDeleteRequest};
use crate::state_handler::StateContext;
use crate::uds_authz::{Caller, enforce_mailbox_owner_or_root};

/// Wire-shape of the `HOOK-CREATE` JSON body. Mirrors the trimmed
/// `Hook` schema in `src/hook.rs` minus the `event` and `name` fields,
/// which travel as request headers. `deny_unknown_fields` causes a
/// legacy `template` / `params` / `run_as` / `origin` /
/// `dangerously_support_untrusted` to reject at body-parse time, before
/// the handler ever touches `config.toml`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct HookCreateBody {
    cmd: Vec<String>,
    #[serde(default)]
    fire_on_untrusted: bool,
    #[serde(default = "default_hook_type")]
    r#type: String,
}

fn default_hook_type() -> String {
    "cmd".to_string()
}

pub async fn handle_hook_create(
    state_ctx: &StateContext,
    mb_ctx: &MailboxContext,
    req: &HookCreateRequest,
    caller: &Caller,
) -> AckResponse {
    // Resolve the target mailbox up front so the authz check runs
    // against the same snapshot the rest of the handler will build on.
    let snapshot = mb_ctx.config_handle.load();
    let mailbox_cfg = match snapshot.mailboxes.get(&req.mailbox) {
        Some(m) => m.clone(),
        None => {
            return AckResponse::Err {
                code: ErrCode::Enoent,
                reason: format!("mailbox '{}' does not exist", req.mailbox),
            };
        }
    };

    if let Err(reject) =
        enforce_mailbox_owner_or_root("HOOK-CREATE", caller, &req.mailbox, &mailbox_cfg)
    {
        return AckResponse::Err {
            code: reject.code,
            reason: reject.reason,
        };
    }

    // Hooks on the catchall are forbidden at config-load time; mirror
    // the rejection here so a stale UDS client (e.g. one that
    // bypassed the load-time validator by submitting against a freshly
    // mutated config) cannot land one through the side door.
    if mailbox_cfg.is_catchall(&snapshot) {
        return AckResponse::Err {
            code: ErrCode::Mailbox,
            reason: format!(
                "mailbox '{}' is the catchall; hooks on the catchall are forbidden",
                req.mailbox
            ),
        };
    }

    let event = match parse_event(&req.event) {
        Ok(e) => e,
        Err(reason) => {
            return AckResponse::Err {
                code: ErrCode::Validation,
                reason,
            };
        }
    };

    if let Some(name) = &req.name
        && !is_valid_hook_name(name)
    {
        return AckResponse::Err {
            code: ErrCode::Validation,
            reason: format!(
                "invalid hook name '{name}': must match \
                 [a-zA-Z0-9_][a-zA-Z0-9_.-]{{0,127}}"
            ),
        };
    }

    // Pre-screen for the known-removed legacy fields so the wire error
    // mirrors `Config::load` (operators see "book/hooks.md" / "renamed
    // to fire_on_untrusted" / etc. on either surface). `deny_unknown_fields`
    // below is the backstop for any other unknown key.
    if let Err(reason) = reject_legacy_body_fields(&req.body) {
        return AckResponse::Err {
            code: ErrCode::Protocol,
            reason,
        };
    }

    // Body shape: JSON object. `serde(deny_unknown_fields)` is the
    // catch-all for any other unknown key after the legacy pre-screen.
    let body: HookCreateBody = match serde_json::from_slice(&req.body) {
        Ok(b) => b,
        Err(e) => {
            return AckResponse::Err {
                code: ErrCode::Protocol,
                reason: format!("invalid HOOK-CREATE body: {e}"),
            };
        }
    };

    let hook = Hook {
        name: req.name.clone(),
        event,
        r#type: body.r#type,
        cmd: body.cmd,
        fire_on_untrusted: body.fire_on_untrusted,
    };

    if let Err(reason) = validate_single_hook(&hook) {
        return AckResponse::Err {
            code: ErrCode::Validation,
            reason,
        };
    }

    // Acquire the same lock hierarchy mailbox CRUD takes: outer per-
    // mailbox lock (shared with ingest / MARK-* / MAILBOX-CRUD), inner
    // process-wide config write lock. Always outer → inner.
    let lock = state_ctx.lock_for(&req.mailbox);
    let _guard = lock.lock().await;
    let _config_guard = CONFIG_WRITE_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());

    let current = mb_ctx.config_handle.load();
    let mut new_config: Config = (*current).clone();
    let mb = match new_config.mailboxes.get_mut(&req.mailbox) {
        Some(m) => m,
        None => {
            return AckResponse::Err {
                code: ErrCode::Enoent,
                reason: format!(
                    "mailbox '{}' does not exist (race with concurrent MAILBOX-DELETE)",
                    req.mailbox
                ),
            };
        }
    };

    let effective = effective_hook_name(&hook);
    mb.hooks.push(hook);

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
    tracing::info!(
        target: "aimx::hook",
        verb = "HOOK-CREATE",
        mailbox = %req.mailbox,
        hook_name = %effective,
        caller_uid = caller.uid,
        "hook created"
    );
    AckResponse::Ok
}

pub async fn handle_hook_delete(
    state_ctx: &StateContext,
    mb_ctx: &MailboxContext,
    req: &HookDeleteRequest,
    caller: &Caller,
) -> AckResponse {
    // Locate the hook by effective name across every mailbox before
    // taking any lock. The mailbox name we resolve here drives both
    // authz and the per-mailbox lock; without resolving up front we
    // cannot answer "which mailbox does this hook belong to?"
    let snapshot = mb_ctx.config_handle.load();
    let mailbox_name = match find_hook_owner(&snapshot, &req.name) {
        Some(name) => name,
        None => {
            return AckResponse::Err {
                code: ErrCode::Enoent,
                reason: format!("hook '{}' not found", req.name),
            };
        }
    };

    // Re-lookup against the same snapshot so the authz check sees a
    // consistent view of the mailbox's owner.
    let mailbox_cfg = match snapshot.mailboxes.get(&mailbox_name) {
        Some(m) => m.clone(),
        None => {
            return AckResponse::Err {
                code: ErrCode::Enoent,
                reason: format!("hook '{}' not found", req.name),
            };
        }
    };

    if let Err(reject) =
        enforce_mailbox_owner_or_root("HOOK-DELETE", caller, &mailbox_name, &mailbox_cfg)
    {
        return AckResponse::Err {
            code: reject.code,
            reason: reject.reason,
        };
    }

    let lock = state_ctx.lock_for(&mailbox_name);
    let _guard = lock.lock().await;
    let _config_guard = CONFIG_WRITE_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());

    // Re-resolve under the lock so a concurrent HOOK-CREATE / DELETE on
    // the same mailbox doesn't slip past us.
    let current = mb_ctx.config_handle.load();
    let mut new_config: Config = (*current).clone();
    let mb = match new_config.mailboxes.get_mut(&mailbox_name) {
        Some(m) => m,
        None => {
            return AckResponse::Err {
                code: ErrCode::Enoent,
                reason: format!("hook '{}' not found", req.name),
            };
        }
    };
    let before = mb.hooks.len();
    mb.hooks.retain(|h| effective_hook_name(h) != req.name);
    if mb.hooks.len() == before {
        return AckResponse::Err {
            code: ErrCode::Enoent,
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
    tracing::info!(
        target: "aimx::hook",
        verb = "HOOK-DELETE",
        mailbox = %mailbox_name,
        hook_name = %req.name,
        caller_uid = caller.uid,
        "hook deleted"
    );
    AckResponse::Ok
}

/// Pre-screen the raw `HOOK-CREATE` body for the known-removed legacy
/// fields and return an error message that mirrors `Config::load` /
/// `reject_legacy_schema` (so a migrated operator hitting the daemon
/// over UDS sees the same actionable text as one editing `config.toml`).
///
/// On success returns `Ok(())` and the caller continues to the normal
/// serde parse, which still rejects any other unknown field.
fn reject_legacy_body_fields(body: &[u8]) -> Result<(), String> {
    // Best-effort parse as a JSON object. If the body isn't a JSON
    // object the regular serde parse below will surface the right error;
    // we only short-circuit when one of the named removed fields is
    // present.
    let value: serde_json::Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(_) => return Ok(()),
    };
    let Some(obj) = value.as_object() else {
        return Ok(());
    };
    if obj.contains_key("template") || obj.contains_key("params") {
        return Err(
            "hook sets `template`/`params`; template hooks were removed. \
             See book/hooks.md for the supported raw-cmd schema"
                .to_string(),
        );
    }
    if obj.contains_key("run_as") {
        return Err(
            "hook sets `run_as`; the field was removed — hooks now run as the mailbox's `owner`"
                .to_string(),
        );
    }
    if obj.contains_key("origin") {
        return Err("hook sets `origin`; the field was removed".to_string());
    }
    if obj.contains_key("dangerously_support_untrusted") {
        return Err(
            "hook sets `dangerously_support_untrusted`; the field was renamed to `fire_on_untrusted`"
                .to_string(),
        );
    }
    Ok(())
}

fn parse_event(s: &str) -> Result<HookEvent, String> {
    match s {
        "on_receive" => Ok(HookEvent::OnReceive),
        "after_send" => Ok(HookEvent::AfterSend),
        other => Err(format!(
            "invalid event '{other}': expected 'on_receive' or 'after_send'"
        )),
    }
}

fn find_hook_owner(config: &Config, name: &str) -> Option<String> {
    for (mailbox_name, mb) in &config.mailboxes {
        for hook in &mb.hooks {
            if effective_hook_name(hook) == name {
                return Some(mailbox_name.clone());
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, ConfigHandle, MailboxConfig};
    use crate::send_protocol::{HookCreateRequest, HookDeleteRequest};
    use std::collections::HashMap;
    use tempfile::TempDir;

    fn install_tester_resolver() -> crate::user_resolver::test_resolver::ResolverOverride {
        fn fake(name: &str) -> Option<crate::user_resolver::ResolvedUser> {
            if name == "testowner" || name == "root" {
                let uid = unsafe { libc::geteuid() };
                let gid = unsafe { libc::getegid() };
                Some(crate::user_resolver::ResolvedUser {
                    name: name.to_string(),
                    uid,
                    gid,
                })
            } else {
                None
            }
        }
        crate::user_resolver::set_test_resolver(fake)
    }

    fn base_config(data_dir: &std::path::Path) -> Config {
        let mut mailboxes = HashMap::new();
        mailboxes.insert(
            "catchall".to_string(),
            MailboxConfig {
                address: "*@example.com".to_string(),
                owner: "aimx-catchall".to_string(),
                hooks: vec![],
                trust: None,
                trusted_senders: None,
                allow_root_catchall: false,
            },
        );
        mailboxes.insert(
            "alice".to_string(),
            MailboxConfig {
                address: "alice@example.com".to_string(),
                owner: "testowner".to_string(),
                hooks: vec![],
                trust: None,
                trusted_senders: None,
                allow_root_catchall: false,
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
            upgrade: None,
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

    fn body_for(cmd: &[&str], fire_on_untrusted: bool) -> Vec<u8> {
        let cmd_arr: Vec<String> = cmd.iter().map(|s| s.to_string()).collect();
        let value = serde_json::json!({
            "cmd": cmd_arr,
            "fire_on_untrusted": fire_on_untrusted,
        });
        serde_json::to_vec(&value).unwrap()
    }

    #[tokio::test]
    async fn root_can_create_hook_on_any_mailbox() {
        let _r = install_tester_resolver();
        let tmp = TempDir::new().unwrap();
        let (state_ctx, mb_ctx) = contexts(&tmp);
        let req = HookCreateRequest {
            mailbox: "alice".into(),
            event: "on_receive".into(),
            name: Some("greet".into()),
            body: body_for(&["/bin/echo", "hi"], false),
        };
        let resp = handle_hook_create(&state_ctx, &mb_ctx, &req, &Caller::internal_root()).await;
        assert!(matches!(resp, AckResponse::Ok), "{resp:?}");
        let cfg = mb_ctx.config_handle.load();
        assert_eq!(cfg.mailboxes["alice"].hooks.len(), 1);
        assert_eq!(
            cfg.mailboxes["alice"].hooks[0].name.as_deref(),
            Some("greet")
        );
    }

    #[tokio::test]
    async fn create_rejects_unknown_mailbox_with_enoent() {
        let _r = install_tester_resolver();
        let tmp = TempDir::new().unwrap();
        let (state_ctx, mb_ctx) = contexts(&tmp);
        let req = HookCreateRequest {
            mailbox: "ghost".into(),
            event: "on_receive".into(),
            name: None,
            body: body_for(&["/bin/echo", "hi"], false),
        };
        match handle_hook_create(&state_ctx, &mb_ctx, &req, &Caller::internal_root()).await {
            AckResponse::Err { code, .. } => assert_eq!(code, ErrCode::Enoent),
            other => panic!("expected ENOENT, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn create_rejects_legacy_template_field_at_body_parse() {
        let _r = install_tester_resolver();
        let tmp = TempDir::new().unwrap();
        let (state_ctx, mb_ctx) = contexts(&tmp);
        let body = serde_json::to_vec(&serde_json::json!({
            "cmd": ["/bin/echo", "hi"],
            "template": "invoke-claude",
        }))
        .unwrap();
        let req = HookCreateRequest {
            mailbox: "alice".into(),
            event: "on_receive".into(),
            name: None,
            body,
        };
        match handle_hook_create(&state_ctx, &mb_ctx, &req, &Caller::internal_root()).await {
            AckResponse::Err { code, reason } => {
                assert_eq!(code, ErrCode::Protocol);
                assert!(reason.contains("template"), "{reason}");
                // Mirror `Config::load`: removed-field rejection points
                // operators at the same docs page on either surface.
                assert!(reason.contains("book/hooks.md"), "{reason}");
            }
            other => panic!("expected PROTOCOL, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn create_rejects_legacy_params_field_at_body_parse() {
        let _r = install_tester_resolver();
        let tmp = TempDir::new().unwrap();
        let (state_ctx, mb_ctx) = contexts(&tmp);
        let body = serde_json::to_vec(&serde_json::json!({
            "cmd": ["/bin/echo", "hi"],
            "params": {"foo": "bar"},
        }))
        .unwrap();
        let req = HookCreateRequest {
            mailbox: "alice".into(),
            event: "on_receive".into(),
            name: None,
            body,
        };
        match handle_hook_create(&state_ctx, &mb_ctx, &req, &Caller::internal_root()).await {
            AckResponse::Err { code, reason } => {
                assert_eq!(code, ErrCode::Protocol);
                assert!(reason.contains("book/hooks.md"), "{reason}");
            }
            other => panic!("expected PROTOCOL, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn create_rejects_legacy_run_as_field_at_body_parse() {
        let _r = install_tester_resolver();
        let tmp = TempDir::new().unwrap();
        let (state_ctx, mb_ctx) = contexts(&tmp);
        let body = serde_json::to_vec(&serde_json::json!({
            "cmd": ["/bin/echo", "hi"],
            "run_as": "aimx-hook",
        }))
        .unwrap();
        let req = HookCreateRequest {
            mailbox: "alice".into(),
            event: "on_receive".into(),
            name: None,
            body,
        };
        match handle_hook_create(&state_ctx, &mb_ctx, &req, &Caller::internal_root()).await {
            AckResponse::Err { code, reason } => {
                assert_eq!(code, ErrCode::Protocol);
                assert!(
                    reason.contains("run_as") && reason.contains("removed"),
                    "{reason}"
                );
            }
            other => panic!("expected PROTOCOL, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn create_rejects_legacy_origin_field_at_body_parse() {
        let _r = install_tester_resolver();
        let tmp = TempDir::new().unwrap();
        let (state_ctx, mb_ctx) = contexts(&tmp);
        let body = serde_json::to_vec(&serde_json::json!({
            "cmd": ["/bin/echo", "hi"],
            "origin": "operator",
        }))
        .unwrap();
        let req = HookCreateRequest {
            mailbox: "alice".into(),
            event: "on_receive".into(),
            name: None,
            body,
        };
        match handle_hook_create(&state_ctx, &mb_ctx, &req, &Caller::internal_root()).await {
            AckResponse::Err { code, reason } => {
                assert_eq!(code, ErrCode::Protocol);
                assert!(
                    reason.contains("origin") && reason.contains("removed"),
                    "{reason}"
                );
            }
            other => panic!("expected PROTOCOL, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn create_rejects_legacy_dangerously_support_untrusted_at_body_parse() {
        let _r = install_tester_resolver();
        let tmp = TempDir::new().unwrap();
        let (state_ctx, mb_ctx) = contexts(&tmp);
        let body = serde_json::to_vec(&serde_json::json!({
            "cmd": ["/bin/echo", "hi"],
            "dangerously_support_untrusted": true,
        }))
        .unwrap();
        let req = HookCreateRequest {
            mailbox: "alice".into(),
            event: "on_receive".into(),
            name: None,
            body,
        };
        match handle_hook_create(&state_ctx, &mb_ctx, &req, &Caller::internal_root()).await {
            AckResponse::Err { code, reason } => {
                assert_eq!(code, ErrCode::Protocol);
                assert!(
                    reason.contains("dangerously_support_untrusted")
                        && reason.contains("fire_on_untrusted"),
                    "{reason}"
                );
            }
            other => panic!("expected PROTOCOL, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn create_rejects_empty_cmd_at_validation() {
        let _r = install_tester_resolver();
        let tmp = TempDir::new().unwrap();
        let (state_ctx, mb_ctx) = contexts(&tmp);
        let body = serde_json::to_vec(&serde_json::json!({
            "cmd": [],
        }))
        .unwrap();
        let req = HookCreateRequest {
            mailbox: "alice".into(),
            event: "on_receive".into(),
            name: None,
            body,
        };
        match handle_hook_create(&state_ctx, &mb_ctx, &req, &Caller::internal_root()).await {
            AckResponse::Err { code, reason } => {
                assert_eq!(code, ErrCode::Validation);
                assert!(reason.contains("empty"), "{reason}");
            }
            other => panic!("expected VALIDATION, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn create_rejects_non_absolute_program_at_validation() {
        let _r = install_tester_resolver();
        let tmp = TempDir::new().unwrap();
        let (state_ctx, mb_ctx) = contexts(&tmp);
        let req = HookCreateRequest {
            mailbox: "alice".into(),
            event: "on_receive".into(),
            name: None,
            body: body_for(&["echo", "hi"], false),
        };
        match handle_hook_create(&state_ctx, &mb_ctx, &req, &Caller::internal_root()).await {
            AckResponse::Err { code, reason } => {
                assert_eq!(code, ErrCode::Validation);
                assert!(reason.contains("absolute"), "{reason}");
            }
            other => panic!("expected VALIDATION, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn create_rejects_fire_on_untrusted_on_after_send() {
        let _r = install_tester_resolver();
        let tmp = TempDir::new().unwrap();
        let (state_ctx, mb_ctx) = contexts(&tmp);
        let req = HookCreateRequest {
            mailbox: "alice".into(),
            event: "after_send".into(),
            name: None,
            body: body_for(&["/bin/echo", "hi"], true),
        };
        match handle_hook_create(&state_ctx, &mb_ctx, &req, &Caller::internal_root()).await {
            AckResponse::Err { code, reason } => {
                assert_eq!(code, ErrCode::Validation);
                assert!(reason.contains("on_receive"), "{reason}");
            }
            other => panic!("expected VALIDATION, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn create_rejects_invalid_event() {
        let _r = install_tester_resolver();
        let tmp = TempDir::new().unwrap();
        let (state_ctx, mb_ctx) = contexts(&tmp);
        let req = HookCreateRequest {
            mailbox: "alice".into(),
            event: "on_open".into(),
            name: None,
            body: body_for(&["/bin/echo"], false),
        };
        match handle_hook_create(&state_ctx, &mb_ctx, &req, &Caller::internal_root()).await {
            AckResponse::Err { code, .. } => assert_eq!(code, ErrCode::Validation),
            other => panic!("expected VALIDATION, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn create_rejects_hook_on_catchall() {
        let _r = install_tester_resolver();
        let tmp = TempDir::new().unwrap();
        let (state_ctx, mb_ctx) = contexts(&tmp);
        let req = HookCreateRequest {
            mailbox: "catchall".into(),
            event: "on_receive".into(),
            name: None,
            body: body_for(&["/bin/echo"], false),
        };
        match handle_hook_create(&state_ctx, &mb_ctx, &req, &Caller::internal_root()).await {
            AckResponse::Err { code, reason } => {
                assert_eq!(code, ErrCode::Mailbox);
                assert!(reason.contains("catchall"), "{reason}");
            }
            other => panic!("expected MAILBOX, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn non_owner_cannot_create_hook() {
        let _r = install_tester_resolver();
        let tmp = TempDir::new().unwrap();
        let (state_ctx, mb_ctx) = contexts(&tmp);
        // Caller uid is some non-root, non-owner uid. The username
        // resolves to something that is NOT 'testowner' so the authz
        // check rejects.
        let attacker = Caller::with_username(99999, 99999, "attacker");
        let req = HookCreateRequest {
            mailbox: "alice".into(),
            event: "on_receive".into(),
            name: None,
            body: body_for(&["/bin/echo"], false),
        };
        match handle_hook_create(&state_ctx, &mb_ctx, &req, &attacker).await {
            AckResponse::Err { code, .. } => assert_eq!(code, ErrCode::Eaccess),
            other => panic!("expected EACCES, got {other:?}"),
        }
        // Mailbox config is unchanged.
        let cfg = mb_ctx.config_handle.load();
        assert!(cfg.mailboxes["alice"].hooks.is_empty());
    }

    #[tokio::test]
    async fn delete_unknown_hook_returns_enoent() {
        let _r = install_tester_resolver();
        let tmp = TempDir::new().unwrap();
        let (state_ctx, mb_ctx) = contexts(&tmp);
        let req = HookDeleteRequest {
            name: "ghost".into(),
        };
        match handle_hook_delete(&state_ctx, &mb_ctx, &req, &Caller::internal_root()).await {
            AckResponse::Err { code, .. } => assert_eq!(code, ErrCode::Enoent),
            other => panic!("expected ENOENT, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn delete_works_after_create_round_trip() {
        let _r = install_tester_resolver();
        let tmp = TempDir::new().unwrap();
        let (state_ctx, mb_ctx) = contexts(&tmp);

        let create = HookCreateRequest {
            mailbox: "alice".into(),
            event: "on_receive".into(),
            name: Some("greet".into()),
            body: body_for(&["/bin/echo", "hi"], false),
        };
        assert!(matches!(
            handle_hook_create(&state_ctx, &mb_ctx, &create, &Caller::internal_root()).await,
            AckResponse::Ok
        ));

        let del = HookDeleteRequest {
            name: "greet".into(),
        };
        assert!(matches!(
            handle_hook_delete(&state_ctx, &mb_ctx, &del, &Caller::internal_root()).await,
            AckResponse::Ok
        ));
        assert!(
            mb_ctx.config_handle.load().mailboxes["alice"]
                .hooks
                .is_empty()
        );
    }

    #[tokio::test]
    async fn non_owner_cannot_delete_hook() {
        let _r = install_tester_resolver();
        let tmp = TempDir::new().unwrap();
        let (state_ctx, mb_ctx) = contexts(&tmp);

        let create = HookCreateRequest {
            mailbox: "alice".into(),
            event: "on_receive".into(),
            name: Some("greet".into()),
            body: body_for(&["/bin/echo", "hi"], false),
        };
        assert!(matches!(
            handle_hook_create(&state_ctx, &mb_ctx, &create, &Caller::internal_root()).await,
            AckResponse::Ok
        ));

        let attacker = Caller::with_username(99999, 99999, "attacker");
        let del = HookDeleteRequest {
            name: "greet".into(),
        };
        match handle_hook_delete(&state_ctx, &mb_ctx, &del, &attacker).await {
            AckResponse::Err { code, .. } => assert_eq!(code, ErrCode::Eaccess),
            other => panic!("expected EACCES, got {other:?}"),
        }
        assert_eq!(
            mb_ctx.config_handle.load().mailboxes["alice"].hooks.len(),
            1
        );
    }

    #[tokio::test]
    async fn create_persists_to_config_toml() {
        let _r = install_tester_resolver();
        let tmp = TempDir::new().unwrap();
        let (state_ctx, mb_ctx) = contexts(&tmp);
        let req = HookCreateRequest {
            mailbox: "alice".into(),
            event: "on_receive".into(),
            name: Some("greet".into()),
            body: body_for(&["/bin/echo", "hi"], false),
        };
        assert!(matches!(
            handle_hook_create(&state_ctx, &mb_ctx, &req, &Caller::internal_root()).await,
            AckResponse::Ok
        ));
        let reloaded = Config::load_ignore_warnings(&mb_ctx.config_path).unwrap();
        assert_eq!(reloaded.mailboxes["alice"].hooks.len(), 1);
        assert_eq!(
            reloaded.mailboxes["alice"].hooks[0].cmd,
            vec!["/bin/echo".to_string(), "hi".to_string()]
        );
    }

    #[tokio::test]
    async fn duplicate_explicit_hook_name_is_rejected() {
        let _r = install_tester_resolver();
        let tmp = TempDir::new().unwrap();
        let (state_ctx, mb_ctx) = contexts(&tmp);
        let req1 = HookCreateRequest {
            mailbox: "alice".into(),
            event: "on_receive".into(),
            name: Some("greet".into()),
            body: body_for(&["/bin/echo", "hi"], false),
        };
        assert!(matches!(
            handle_hook_create(&state_ctx, &mb_ctx, &req1, &Caller::internal_root()).await,
            AckResponse::Ok
        ));
        let req2 = HookCreateRequest {
            mailbox: "alice".into(),
            event: "on_receive".into(),
            name: Some("greet".into()),
            body: body_for(&["/bin/echo", "again"], false),
        };
        match handle_hook_create(&state_ctx, &mb_ctx, &req2, &Caller::internal_root()).await {
            AckResponse::Err { code, reason } => {
                assert_eq!(code, ErrCode::Validation);
                assert!(reason.contains("greet"), "{reason}");
            }
            other => panic!("expected VALIDATION, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn invalid_hook_name_rejected() {
        let _r = install_tester_resolver();
        let tmp = TempDir::new().unwrap();
        let (state_ctx, mb_ctx) = contexts(&tmp);
        let req = HookCreateRequest {
            mailbox: "alice".into(),
            event: "on_receive".into(),
            name: Some("bad name with spaces".into()),
            body: body_for(&["/bin/echo"], false),
        };
        match handle_hook_create(&state_ctx, &mb_ctx, &req, &Caller::internal_root()).await {
            AckResponse::Err { code, .. } => assert_eq!(code, ErrCode::Validation),
            other => panic!("expected VALIDATION, got {other:?}"),
        }
    }
}
