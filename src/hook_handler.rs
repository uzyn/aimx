//! Daemon-side handlers for the `HOOK-CREATE` and `HOOK-DELETE` verbs of
//! the `AIMX/1` UDS protocol.
//!
//! `HOOK-CREATE` over UDS is template-only (Sprint 3 S3-1). The verb never
//! accepts a raw `cmd`, `run_as`, `timeout_secs`, `stdin`, or
//! `dangerously_support_untrusted`. Those live on the `[[hook_template]]`
//! block the operator installed at setup time. A local user on the
//! world-writable `aimx.sock` can only wire up pre-vetted commands.
//!
//! `HOOK-DELETE` is origin-protected (Sprint 3 S3-2): MCP may delete hooks
//! it created (`origin = "mcp"`) but cannot touch operator-origin hooks —
//! those can only be removed by `sudo aimx hooks delete` or by editing
//! `config.toml` directly.
//!
//! Correctness model is symmetric to [`crate::mailbox_handler`]:
//!
//! 1. Validate the submitted request (template exists + is enabled, event
//!    in `template.allowed_events`, declared params bound exactly once,
//!    substitution succeeds against the template's argv, resulting hook
//!    name unique).
//! 2. Load the current `Config` snapshot through the shared
//!    `ConfigHandle`. Re-derive the new snapshot in memory (append on
//!    CREATE, filter on DELETE).
//! 3. Write atomically via `write_config_atomic` (write-temp-then-rename
//!    shared with `mailbox_handler`).
//! 4. After the rename succeeds, swap the in-memory `Config` via
//!    `ConfigHandle::store`.
//!
//! Locking follows the same outer-per-mailbox / inner-`CONFIG_WRITE_LOCK`
//! hierarchy as the MAILBOX-CRUD path (see [`crate::mailbox_locks`]).

use std::collections::BTreeMap;

use crate::config::{
    Config, HookTemplate, OrphanSkipContext, validate_hooks, validate_single_hook,
};
use crate::hook::{Hook, HookEvent, HookOrigin, effective_hook_name, is_valid_hook_name};
use crate::hook_substitute::{BuiltinContext, substitute_argv};
use crate::mailbox_handler::{CONFIG_WRITE_LOCK, MailboxContext, write_config_atomic};
use crate::send_protocol::{
    AckResponse, ErrCode, HookCreateRequest, HookDeleteRequest, HookTemplateCreateBody,
};
use crate::state_handler::StateContext;

/// Handle an `AIMX/1 HOOK-CREATE` request. Template-only. Takes the
/// per-mailbox write lock for the addressed mailbox (outer) plus
/// `CONFIG_WRITE_LOCK` (inner) while the config rewrite + handle swap
/// runs.
pub async fn handle_hook_create(
    state_ctx: &StateContext,
    mb_ctx: &MailboxContext,
    req: &HookCreateRequest,
) -> AckResponse {
    // --- Parse JSON body (rejects `cmd`, `run_as`, etc. via `deny_unknown_fields`).
    let body: HookTemplateCreateBody = match serde_json::from_slice(&req.body) {
        Ok(b) => b,
        Err(e) => {
            return AckResponse::Err {
                code: ErrCode::Validation,
                reason: format!("malformed HOOK-CREATE body: {e}"),
            };
        }
    };

    // --- Parse event string.
    let event = match parse_event_str(&req.event) {
        Ok(e) => e,
        Err(r) => {
            return AckResponse::Err {
                code: ErrCode::Validation,
                reason: r,
            };
        }
    };

    // --- Validate explicit hook name (if supplied) up front.
    if let Some(n) = &req.name
        && !is_valid_hook_name(n)
    {
        return AckResponse::Err {
            code: ErrCode::Validation,
            reason: format!(
                "invalid hook name '{n}': must match [a-zA-Z0-9_][a-zA-Z0-9_.-]{{0,127}}"
            ),
        };
    }

    let lock = state_ctx.lock_for(&req.mailbox);
    let _guard = lock.lock().await;

    let _config_guard = CONFIG_WRITE_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());

    let current = mb_ctx.config_handle.load();

    // --- Resolve mailbox.
    if !current.mailboxes.contains_key(&req.mailbox) {
        return AckResponse::Err {
            code: ErrCode::Mailbox,
            reason: format!("mailbox '{}' does not exist", req.mailbox),
        };
    }

    // --- Resolve template.
    let template = match current
        .hook_templates
        .iter()
        .find(|t| t.name == req.template)
    {
        Some(t) => t.clone(),
        None => {
            return AckResponse::Err {
                code: ErrCode::Validation,
                reason: format!(
                    "unknown-template '{name}': run `aimx hooks templates` to list \
                     enabled templates, or ask the operator to install it via \
                     `sudo aimx setup`",
                    name = req.template
                ),
            };
        }
    };

    // --- Enforce template's allowed_events gate.
    if !template.allowed_events.contains(&event) {
        return AckResponse::Err {
            code: ErrCode::Validation,
            reason: format!(
                "event-not-allowed: template '{}' does not permit event '{}' \
                 (allowed: {})",
                template.name,
                event.as_str(),
                template
                    .allowed_events
                    .iter()
                    .map(|e| e.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        };
    }

    // --- Validate params against the template's declared set.
    if let Err(reason) = validate_params_against_template(&template, &body.params) {
        return AckResponse::Err {
            code: ErrCode::Validation,
            reason,
        };
    }

    // --- Pre-flight substitution: catches whitespace / NUL / etc. in
    // param values before the hook lands in config.toml. Built-in
    // placeholders are substituted with empty strings here — the real
    // values arrive at fire time — which is safe because the validator
    // permits empty strings in builtins.
    let builtins = BuiltinContext::default();
    if let Err(e) = substitute_argv(&template.cmd, &body.params, &builtins) {
        return AckResponse::Err {
            code: ErrCode::Validation,
            reason: format!("param-invalid: {e}"),
        };
    }

    // --- Construct the Hook record. `origin = Mcp` is stamped here; the
    // client's wire payload has no `origin` slot.
    let hook = Hook {
        name: req.name.clone(),
        event,
        r#type: "cmd".into(),
        cmd: String::new(),
        dangerously_support_untrusted: false,
        origin: HookOrigin::Mcp,
        template: Some(template.name.clone()),
        params: body.params,
        run_as: None,
    };

    // Single-hook sanity: catches bad hook name shape (already checked
    // above) and any future invariants that `validate_single_hook`
    // enforces without needing the full config.
    if let Err(reason) = validate_single_hook(&hook) {
        return AckResponse::Err {
            code: ErrCode::Validation,
            reason,
        };
    }

    // Global uniqueness of effective name.
    let new_effective = effective_hook_name(&hook);
    for (mb_name, mb) in &current.mailboxes {
        for existing in &mb.hooks {
            if effective_hook_name(existing) == new_effective {
                return AckResponse::Err {
                    code: ErrCode::Validation,
                    reason: format!(
                        "name-conflict: hook name '{new_effective}' already exists on mailbox '{mb_name}'"
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
    // a config that would fail on next start. UDS HOOK-CREATE is a fresh
    // create path, not a migration load, so we pass the strict context:
    // orphan-skip only applies when the daemon is booting an existing
    // config with a now-missing user (PRD §6.1). Operators creating hooks
    // through MCP/UDS must point at resolvable users.
    if let Err(reason) = validate_hooks(&new_config, &OrphanSkipContext::strict()) {
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
///
/// Origin-protected (Sprint 3 S3-2): refuses to delete operator-origin
/// hooks. Those can only be removed via `sudo aimx hooks delete` or by
/// editing `config.toml` directly.
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

    // Origin check: before acquiring any locks, refuse operator-origin
    // hooks up front so callers get a precise error without waiting on
    // the global config lock. The origin of the target hook is stable
    // across concurrent mutations (no verb edits `origin` in place), so
    // the snapshot read is sound even though we re-resolve under the
    // lock below.
    let origin = current
        .mailboxes
        .get(&owner)
        .and_then(|mb| {
            mb.hooks
                .iter()
                .find(|h| effective_hook_name(h) == req.name)
                .map(|h| h.origin)
        })
        .unwrap_or(HookOrigin::Operator);
    if origin == HookOrigin::Operator {
        return AckResponse::Err {
            code: ErrCode::Validation,
            reason: format!(
                "origin-protected: hook '{}' was created by the operator — \
                 remove via `sudo aimx hooks delete` instead",
                req.name
            ),
        };
    }

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
        mb.hooks.retain(|h| {
            if effective_hook_name(h) == req.name {
                // Origin check re-asserted under the lock: if the hook
                // became operator-origin between snapshots, leave it
                // in place (the earlier snapshot-based check would
                // have returned origin-protected). This branch is
                // structurally unreachable in current code: no verb
                // mutates `origin` on an existing hook — hooks are
                // created with their origin and only ever deleted.
                // Kept as defensive coding so a future verb that
                // rewrites `origin` in place cannot silently bypass
                // origin protection. If that future verb is added,
                // this `retain` path would return NotFound for a
                // hook that exists (the positive break below never
                // fires), which is a misleading error but still
                // refuses the destructive operation; update to emit
                // origin-protected explicitly at that point.
                h.origin != HookOrigin::Mcp
            } else {
                true
            }
        });
        if mb.hooks.len() != before {
            removed = true;
            // Safe to break: `validate_hooks` guarantees effective-name
            // uniqueness globally, so at most one hook ever matches.
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

fn parse_event_str(s: &str) -> Result<HookEvent, String> {
    match s {
        "on_receive" => Ok(HookEvent::OnReceive),
        "after_send" => Ok(HookEvent::AfterSend),
        other => Err(format!(
            "invalid event '{other}': expected 'on_receive' or 'after_send'"
        )),
    }
}

/// Every declared param on the template must be bound in `params`, and
/// no unknown params may appear. Returns an actionable error on mismatch.
fn validate_params_against_template(
    template: &HookTemplate,
    params: &BTreeMap<String, String>,
) -> Result<(), String> {
    for required in &template.params {
        if !params.contains_key(required) {
            return Err(format!(
                "missing-param: template '{}' requires '{required}'",
                template.name
            ));
        }
    }
    for supplied in params.keys() {
        if !template.params.iter().any(|p| p == supplied) {
            return Err(format!(
                "unknown-param: template '{}' does not declare '{supplied}' \
                 (declared: {})",
                template.name,
                template.params.join(", ")
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{ConfigHandle, HookTemplate, HookTemplateStdin, MailboxConfig};
    use crate::hook::HookEvent;
    use std::collections::HashMap;
    use std::path::Path;
    use tempfile::TempDir;

    fn template(name: &str) -> HookTemplate {
        HookTemplate {
            name: name.to_string(),
            description: "test".into(),
            cmd: vec!["/usr/bin/echo".into(), "{prompt}".into()],
            params: vec!["prompt".into()],
            stdin: HookTemplateStdin::Email,
            // Sprint 1 S1-3 invariant: hook.run_as must equal
            // mailbox.owner OR "root". The alice fixture is owned by
            // `root`, so `root` satisfies the invariant on every host.
            run_as: "root".into(),
            timeout_secs: 60,
            allowed_events: vec![HookEvent::OnReceive, HookEvent::AfterSend],
        }
    }

    fn base_config(data_dir: &Path) -> Config {
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
                owner: "ops".to_string(),
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
            hook_templates: vec![template("invoke-claude")],
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

    fn body(params: &[(&str, &str)]) -> Vec<u8> {
        let map: BTreeMap<String, String> = params
            .iter()
            .map(|(k, v)| ((*k).into(), (*v).into()))
            .collect();
        serde_json::to_vec(&HookTemplateCreateBody { params: map }).unwrap()
    }

    #[tokio::test]
    async fn hook_create_template_succeeds_and_stamps_mcp_origin() {
        let tmp = TempDir::new().unwrap();
        let (state_ctx, mb_ctx) = contexts(&tmp);

        let req = HookCreateRequest {
            mailbox: "alice".into(),
            event: "on_receive".into(),
            template: "invoke-claude".into(),
            name: Some("my_hook".into()),
            body: body(&[("prompt", "hello world")]),
        };
        match handle_hook_create(&state_ctx, &mb_ctx, &req).await {
            AckResponse::Ok => {}
            other => panic!("expected Ok, got {other:?}"),
        }

        let live = mb_ctx.config_handle.load();
        let hooks = &live.mailboxes["alice"].hooks;
        assert_eq!(hooks.len(), 1);
        assert_eq!(hooks[0].name.as_deref(), Some("my_hook"));
        assert_eq!(hooks[0].origin, HookOrigin::Mcp);
        assert_eq!(hooks[0].template.as_deref(), Some("invoke-claude"));
        assert_eq!(
            hooks[0].params.get("prompt").map(String::as_str),
            Some("hello world")
        );
        assert_eq!(hooks[0].cmd, "");

        let reloaded = Config::load_ignore_warnings(&mb_ctx.config_path).unwrap();
        assert_eq!(reloaded.mailboxes["alice"].hooks[0].origin, HookOrigin::Mcp);
    }

    #[tokio::test]
    async fn hook_create_anonymous_derives_name() {
        let tmp = TempDir::new().unwrap();
        let (state_ctx, mb_ctx) = contexts(&tmp);

        let req = HookCreateRequest {
            mailbox: "alice".into(),
            event: "on_receive".into(),
            template: "invoke-claude".into(),
            name: None,
            body: body(&[("prompt", "anon")]),
        };
        assert!(matches!(
            handle_hook_create(&state_ctx, &mb_ctx, &req).await,
            AckResponse::Ok
        ));
        let live = mb_ctx.config_handle.load();
        assert!(live.mailboxes["alice"].hooks[0].name.is_none());
    }

    #[tokio::test]
    async fn hook_create_rejects_body_with_cmd() {
        let tmp = TempDir::new().unwrap();
        let (state_ctx, mb_ctx) = contexts(&tmp);

        let json = br#"{"params":{"prompt":"hi"},"cmd":"/bin/evil"}"#;
        let req = HookCreateRequest {
            mailbox: "alice".into(),
            event: "on_receive".into(),
            template: "invoke-claude".into(),
            name: None,
            body: json.to_vec(),
        };
        match handle_hook_create(&state_ctx, &mb_ctx, &req).await {
            AckResponse::Err { code, reason } => {
                assert_eq!(code, ErrCode::Validation);
                assert!(reason.contains("cmd"), "{reason}");
            }
            other => panic!("expected Err(VALIDATION), got {other:?}"),
        }
        let live = mb_ctx.config_handle.load();
        assert!(live.mailboxes["alice"].hooks.is_empty());
    }

    #[tokio::test]
    async fn hook_create_rejects_body_with_run_as() {
        let tmp = TempDir::new().unwrap();
        let (state_ctx, mb_ctx) = contexts(&tmp);

        let json = br#"{"params":{"prompt":"hi"},"run_as":"root"}"#;
        let req = HookCreateRequest {
            mailbox: "alice".into(),
            event: "on_receive".into(),
            template: "invoke-claude".into(),
            name: None,
            body: json.to_vec(),
        };
        match handle_hook_create(&state_ctx, &mb_ctx, &req).await {
            AckResponse::Err { code, reason } => {
                assert_eq!(code, ErrCode::Validation);
                assert!(reason.contains("run_as"), "{reason}");
            }
            other => panic!("expected Err(VALIDATION), got {other:?}"),
        }
    }

    #[tokio::test]
    async fn hook_create_rejects_body_with_dangerously_flag() {
        let tmp = TempDir::new().unwrap();
        let (state_ctx, mb_ctx) = contexts(&tmp);

        let json = br#"{"params":{"prompt":"hi"},"dangerously_support_untrusted":true}"#;
        let req = HookCreateRequest {
            mailbox: "alice".into(),
            event: "on_receive".into(),
            template: "invoke-claude".into(),
            name: None,
            body: json.to_vec(),
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
    async fn hook_create_rejects_unknown_template() {
        let tmp = TempDir::new().unwrap();
        let (state_ctx, mb_ctx) = contexts(&tmp);

        let req = HookCreateRequest {
            mailbox: "alice".into(),
            event: "on_receive".into(),
            template: "no-such".into(),
            name: None,
            body: body(&[]),
        };
        match handle_hook_create(&state_ctx, &mb_ctx, &req).await {
            AckResponse::Err { code, reason } => {
                assert_eq!(code, ErrCode::Validation);
                assert!(reason.contains("unknown-template"), "{reason}");
                assert!(reason.contains("aimx hooks templates"), "{reason}");
            }
            other => panic!("expected Err(VALIDATION), got {other:?}"),
        }
    }

    #[tokio::test]
    async fn hook_create_rejects_missing_param() {
        let tmp = TempDir::new().unwrap();
        let (state_ctx, mb_ctx) = contexts(&tmp);

        let req = HookCreateRequest {
            mailbox: "alice".into(),
            event: "on_receive".into(),
            template: "invoke-claude".into(),
            name: None,
            body: body(&[]),
        };
        match handle_hook_create(&state_ctx, &mb_ctx, &req).await {
            AckResponse::Err { code, reason } => {
                assert_eq!(code, ErrCode::Validation);
                assert!(reason.contains("missing-param"), "{reason}");
                assert!(reason.contains("prompt"), "{reason}");
            }
            other => panic!("expected Err(VALIDATION), got {other:?}"),
        }
    }

    #[tokio::test]
    async fn hook_create_rejects_unknown_param() {
        let tmp = TempDir::new().unwrap();
        let (state_ctx, mb_ctx) = contexts(&tmp);

        let req = HookCreateRequest {
            mailbox: "alice".into(),
            event: "on_receive".into(),
            template: "invoke-claude".into(),
            name: None,
            body: body(&[("prompt", "hi"), ("bogus", "zz")]),
        };
        match handle_hook_create(&state_ctx, &mb_ctx, &req).await {
            AckResponse::Err { code, reason } => {
                assert_eq!(code, ErrCode::Validation);
                assert!(reason.contains("unknown-param"), "{reason}");
                assert!(reason.contains("bogus"), "{reason}");
            }
            other => panic!("expected Err(VALIDATION), got {other:?}"),
        }
    }

    #[tokio::test]
    async fn hook_create_rejects_event_not_allowed() {
        let tmp = TempDir::new().unwrap();
        let config = {
            let mut c = base_config(tmp.path());
            c.hook_templates[0].allowed_events = vec![HookEvent::OnReceive];
            c
        };
        let handle = ConfigHandle::new(config);
        let state_ctx = StateContext::new(tmp.path().to_path_buf(), handle.clone());
        let config_path = tmp.path().join("config.toml");
        write_config_atomic(&config_path, &handle.load()).unwrap();
        let mb_ctx = MailboxContext::new(config_path, handle);

        let req = HookCreateRequest {
            mailbox: "alice".into(),
            event: "after_send".into(),
            template: "invoke-claude".into(),
            name: None,
            body: body(&[("prompt", "hi")]),
        };
        match handle_hook_create(&state_ctx, &mb_ctx, &req).await {
            AckResponse::Err { code, reason } => {
                assert_eq!(code, ErrCode::Validation);
                assert!(reason.contains("event-not-allowed"), "{reason}");
            }
            other => panic!("expected Err(VALIDATION), got {other:?}"),
        }
    }

    #[tokio::test]
    async fn hook_create_rejects_unknown_mailbox() {
        let tmp = TempDir::new().unwrap();
        let (state_ctx, mb_ctx) = contexts(&tmp);

        let req = HookCreateRequest {
            mailbox: "ghost".into(),
            event: "on_receive".into(),
            template: "invoke-claude".into(),
            name: None,
            body: body(&[("prompt", "hi")]),
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
    async fn hook_create_rejects_invalid_event_string() {
        let tmp = TempDir::new().unwrap();
        let (state_ctx, mb_ctx) = contexts(&tmp);

        let req = HookCreateRequest {
            mailbox: "alice".into(),
            event: "garbage".into(),
            template: "invoke-claude".into(),
            name: None,
            body: body(&[("prompt", "hi")]),
        };
        match handle_hook_create(&state_ctx, &mb_ctx, &req).await {
            AckResponse::Err { code, reason } => {
                assert_eq!(code, ErrCode::Validation);
                assert!(reason.contains("invalid event"), "{reason}");
            }
            other => panic!("expected Err(VALIDATION), got {other:?}"),
        }
    }

    #[tokio::test]
    async fn hook_create_rejects_bad_name() {
        let tmp = TempDir::new().unwrap();
        let (state_ctx, mb_ctx) = contexts(&tmp);

        let req = HookCreateRequest {
            mailbox: "alice".into(),
            event: "on_receive".into(),
            template: "invoke-claude".into(),
            name: Some("bad name!".into()),
            body: body(&[("prompt", "hi")]),
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
    async fn hook_create_rejects_name_conflict() {
        let tmp = TempDir::new().unwrap();
        let (state_ctx, mb_ctx) = contexts(&tmp);

        let req1 = HookCreateRequest {
            mailbox: "alice".into(),
            event: "on_receive".into(),
            template: "invoke-claude".into(),
            name: Some("dup".into()),
            body: body(&[("prompt", "hi")]),
        };
        assert!(matches!(
            handle_hook_create(&state_ctx, &mb_ctx, &req1).await,
            AckResponse::Ok
        ));

        let req2 = HookCreateRequest {
            mailbox: "catchall".into(),
            event: "on_receive".into(),
            template: "invoke-claude".into(),
            name: Some("dup".into()),
            body: body(&[("prompt", "hi2")]),
        };
        match handle_hook_create(&state_ctx, &mb_ctx, &req2).await {
            AckResponse::Err { code, reason } => {
                assert_eq!(code, ErrCode::Validation);
                assert!(reason.contains("name-conflict"), "{reason}");
            }
            other => panic!("expected Err(VALIDATION), got {other:?}"),
        }
    }

    #[tokio::test]
    async fn hook_create_rejects_malformed_json() {
        let tmp = TempDir::new().unwrap();
        let (state_ctx, mb_ctx) = contexts(&tmp);

        let req = HookCreateRequest {
            mailbox: "alice".into(),
            event: "on_receive".into(),
            template: "invoke-claude".into(),
            name: None,
            body: b"not-json-at-all".to_vec(),
        };
        match handle_hook_create(&state_ctx, &mb_ctx, &req).await {
            AckResponse::Err { code, reason } => {
                assert_eq!(code, ErrCode::Validation);
                assert!(reason.contains("malformed HOOK-CREATE body"), "{reason}");
            }
            other => panic!("expected Err(VALIDATION), got {other:?}"),
        }
    }

    #[tokio::test]
    async fn hook_create_rejects_nul_in_param() {
        let tmp = TempDir::new().unwrap();
        let (state_ctx, mb_ctx) = contexts(&tmp);

        // NUL bytes in a param value would truncate argv entries when
        // they traverse `execvp`; our substitution validator rejects
        // them up front (tabs and newlines are permitted per Sprint 2).
        let req = HookCreateRequest {
            mailbox: "alice".into(),
            event: "on_receive".into(),
            template: "invoke-claude".into(),
            name: None,
            body: body(&[("prompt", "a\0b")]),
        };
        match handle_hook_create(&state_ctx, &mb_ctx, &req).await {
            AckResponse::Err { code, reason } => {
                assert_eq!(code, ErrCode::Validation);
                assert!(reason.contains("param-invalid"), "{reason}");
            }
            other => panic!("expected Err(VALIDATION), got {other:?}"),
        }
    }

    // ---- HOOK-DELETE -------------------------------------------------

    fn insert_raw_cmd_hook(mb_ctx: &MailboxContext, mailbox: &str, name: &str, origin: HookOrigin) {
        let current = mb_ctx.config_handle.load();
        let mut new_config: Config = (*current).clone();
        let hook = Hook {
            name: Some(name.into()),
            event: HookEvent::OnReceive,
            r#type: "cmd".into(),
            cmd: "echo hi".into(),
            dangerously_support_untrusted: false,
            origin,
            template: None,
            params: BTreeMap::new(),
            run_as: None,
        };
        new_config
            .mailboxes
            .get_mut(mailbox)
            .unwrap()
            .hooks
            .push(hook);
        write_config_atomic(&mb_ctx.config_path, &new_config).unwrap();
        mb_ctx.config_handle.store(new_config);
    }

    #[tokio::test]
    async fn hook_delete_removes_mcp_origin_hook() {
        let tmp = TempDir::new().unwrap();
        let (state_ctx, mb_ctx) = contexts(&tmp);

        // Create via UDS so the hook lands with origin = Mcp.
        let req = HookCreateRequest {
            mailbox: "alice".into(),
            event: "on_receive".into(),
            template: "invoke-claude".into(),
            name: Some("mcp_hook".into()),
            body: body(&[("prompt", "hi")]),
        };
        assert!(matches!(
            handle_hook_create(&state_ctx, &mb_ctx, &req).await,
            AckResponse::Ok
        ));

        let del = HookDeleteRequest {
            name: "mcp_hook".into(),
        };
        assert!(matches!(
            handle_hook_delete(&state_ctx, &mb_ctx, &del).await,
            AckResponse::Ok
        ));

        let live = mb_ctx.config_handle.load();
        assert!(live.mailboxes["alice"].hooks.is_empty());
    }

    /// S3-2: operator-origin hooks must refuse deletion over UDS.
    #[tokio::test]
    async fn hook_delete_refuses_operator_origin() {
        let tmp = TempDir::new().unwrap();
        let (state_ctx, mb_ctx) = contexts(&tmp);

        insert_raw_cmd_hook(&mb_ctx, "alice", "operator_hook", HookOrigin::Operator);

        let del = HookDeleteRequest {
            name: "operator_hook".into(),
        };
        match handle_hook_delete(&state_ctx, &mb_ctx, &del).await {
            AckResponse::Err { code, reason } => {
                assert_eq!(code, ErrCode::Validation);
                assert!(reason.contains("origin-protected"), "{reason}");
                assert!(reason.contains("sudo aimx hooks delete"), "{reason}");
            }
            other => panic!("expected Err(VALIDATION), got {other:?}"),
        }

        // Hook must still be there.
        let live = mb_ctx.config_handle.load();
        assert_eq!(live.mailboxes["alice"].hooks.len(), 1);
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
        // Two concurrent HOOK-CREATE on different mailboxes must both
        // land in the final config. Regression test for the lost-update
        // path.
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
                    event: "on_receive".into(),
                    template: "invoke-claude".into(),
                    name: Some(name.clone()),
                    body: body(&[("prompt", "hi")]),
                };
                handle_hook_create(&s, &m, &req).await
            }));
        }
        for h in handles {
            assert!(matches!(h.await.unwrap(), AckResponse::Ok));
        }

        let reloaded = Config::load_ignore_warnings(&mb_ctx.config_path).unwrap();
        assert_eq!(reloaded.mailboxes["alice"].hooks.len(), 1);
        assert_eq!(reloaded.mailboxes["catchall"].hooks.len(), 1);
    }
}
