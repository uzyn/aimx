//! Daemon-side handler for the `HOOK-LIST` verb of the `AIMX/1` UDS
//! protocol.
//!
//! Mirrors `mailbox_list_handler` line-for-line: the daemon resolves
//! the caller via `SO_PEERCRED`, walks the in-memory `Arc<Config>`, and
//! returns a JSON array of the hooks visible to the caller. Root sees
//! every hook on every mailbox; non-root sees only hooks on mailboxes
//! it owns.
//!
//! The MCP `hook_list` tool used to read `/etc/aimx/config.toml`
//! directly from a non-root process and would always fail on the
//! `0640 root:root` permission. After this rework the MCP tool is a
//! thin UDS client and the daemon answers from its `Arc<Config>`
//! snapshot — no disk I/O, no caller-supplied filtering.
//!
//! No locks are taken; reads of `Arc<Config>` are wait-free. A
//! concurrent `HOOK-CREATE` / `HOOK-DELETE` may swap the snapshot in
//! between two `HOOK-LIST` calls, but each individual call observes a
//! consistent snapshot.

use serde::{Deserialize, Serialize};

use crate::config::Config;
use crate::hook::{HookEvent, effective_hook_name};
use crate::mailbox;
use crate::send_protocol::{ErrCode, JsonAckResponse};
use crate::state_handler::StateContext;
use crate::uds_authz::Caller;

/// One row of the JSON array returned by `HOOK-LIST`. Schema is fixed
/// by the user-mailbox PRD R34. `event` is the typed `HookEvent` enum
/// (snake_case on the wire) so client and server share a single source
/// of truth and a stale wire string cannot drift past the codec.
#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct HookListRow {
    /// Effective hook name — matches the CLI's `aimx hooks list`
    /// output (explicit `name` if set, derived 12-char hex otherwise).
    pub name: String,
    /// Mailbox the hook is attached to.
    pub mailbox: String,
    /// Triggering event (`on_receive` / `after_send`).
    pub event: HookEvent,
    /// Argv form, not joined — agents can render a shell-safe display
    /// without losing token boundaries.
    pub cmd: Vec<String>,
    /// Whether the hook fires on inbound emails the trust gate marks
    /// untrusted. `false` for `after_send` hooks (no trust gate).
    pub fire_on_untrusted: bool,
    /// Hard subprocess timeout, seconds. Mirrors the config field.
    pub timeout_secs: u32,
}

/// Build the JSON ack response for an `AIMX/1 HOOK-LIST` request.
pub async fn handle_hook_list(state_ctx: &StateContext, caller: &Caller) -> JsonAckResponse {
    let config = state_ctx.config_handle.load();
    let rows = collect_rows(&config, caller.uid);
    let body = match serde_json::to_vec(&rows) {
        Ok(b) => b,
        Err(e) => {
            return JsonAckResponse::Err {
                code: ErrCode::Io,
                reason: format!("failed to serialize hook list: {e}"),
            };
        }
    };
    JsonAckResponse::Ok { body }
}

/// Pure helper: walk every mailbox in `config`, keep only those visible
/// to `caller_uid`, and emit a `HookListRow` per attached hook. Sorted
/// stably by `(mailbox, event, name)` so agents get deterministic
/// output across calls.
fn collect_rows(config: &Config, caller_uid: u32) -> Vec<HookListRow> {
    let mut rows: Vec<HookListRow> = Vec::new();
    for (mailbox_name, mb) in &config.mailboxes {
        if !is_visible_to(config, mailbox_name, caller_uid) {
            continue;
        }
        for hook in &mb.hooks {
            rows.push(HookListRow {
                name: effective_hook_name(hook),
                mailbox: mailbox_name.clone(),
                event: hook.event,
                cmd: hook.cmd.clone(),
                fire_on_untrusted: hook.fire_on_untrusted,
                timeout_secs: hook.timeout_secs,
            });
        }
    }
    rows.sort_by(|a, b| {
        a.mailbox
            .cmp(&b.mailbox)
            .then_with(|| a.event.as_str().cmp(b.event.as_str()))
            .then_with(|| a.name.cmp(&b.name))
    });
    rows
}

/// Visibility rule: root sees every mailbox; a non-root caller sees a
/// mailbox only when its configured `owner` resolves to the caller's
/// uid. Stray on-disk directories without a config entry have no hooks
/// (hooks live only in `[mailboxes.<name>].hooks`), so this filter is
/// strictly tighter than the `MAILBOX-LIST` filter.
fn is_visible_to(config: &Config, name: &str, caller_uid: u32) -> bool {
    if caller_uid == 0 {
        return true;
    }
    mailbox::caller_owns(config, name, caller_uid)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, ConfigHandle, MailboxConfig};
    use crate::hook::Hook;
    use std::collections::HashMap;
    use tempfile::TempDir;

    fn fake_resolver(name: &str) -> Option<crate::user_resolver::ResolvedUser> {
        match name {
            "alice" => Some(crate::user_resolver::ResolvedUser {
                name: "alice".to_string(),
                uid: 4242,
                gid: 4242,
            }),
            "bob" => Some(crate::user_resolver::ResolvedUser {
                name: "bob".to_string(),
                uid: 5050,
                gid: 5050,
            }),
            "root" => Some(crate::user_resolver::ResolvedUser {
                name: "root".to_string(),
                uid: 0,
                gid: 0,
            }),
            _ => None,
        }
    }

    fn install_resolver() -> crate::user_resolver::test_resolver::ResolverOverride {
        crate::user_resolver::set_test_resolver(fake_resolver)
    }

    fn hook_for(event: HookEvent, cmd: Vec<&str>, name: Option<&str>) -> Hook {
        Hook {
            name: name.map(|s| s.to_string()),
            event,
            r#type: "cmd".to_string(),
            cmd: cmd.into_iter().map(|s| s.to_string()).collect(),
            fire_on_untrusted: false,
            timeout_secs: 60,
        }
    }

    fn config_with_hooks(data_dir: &std::path::Path) -> Config {
        let mut mailboxes = HashMap::new();
        mailboxes.insert(
            "alice".to_string(),
            MailboxConfig {
                address: "alice@example.com".to_string(),
                owner: "alice".to_string(),
                hooks: vec![
                    hook_for(HookEvent::OnReceive, vec!["/bin/true"], Some("a-recv")),
                    hook_for(HookEvent::AfterSend, vec!["/bin/echo", "sent"], None),
                ],
                trust: None,
                trusted_senders: None,
                allow_root_catchall: false,
            },
        );
        mailboxes.insert(
            "bob".to_string(),
            MailboxConfig {
                address: "bob@example.com".to_string(),
                owner: "bob".to_string(),
                hooks: vec![hook_for(
                    HookEvent::OnReceive,
                    vec!["/bin/true"],
                    Some("b-recv"),
                )],
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

    /// Root sees every hook on every mailbox.
    #[tokio::test]
    async fn root_sees_every_hook() {
        let tmp = TempDir::new().unwrap();
        let _r = install_resolver();
        let config = config_with_hooks(tmp.path());
        let handle = ConfigHandle::new(config);
        let state_ctx = StateContext::new(tmp.path().to_path_buf(), handle);

        let resp = handle_hook_list(&state_ctx, &Caller::internal_root()).await;
        let body = match resp {
            JsonAckResponse::Ok { body } => body,
            other => panic!("expected Ok, got {other:?}"),
        };
        let rows: Vec<HookListRow> = serde_json::from_slice(&body).unwrap();
        let mailboxes: Vec<&str> = rows.iter().map(|r| r.mailbox.as_str()).collect();
        assert_eq!(mailboxes, vec!["alice", "alice", "bob"]);
    }

    /// A non-root caller whose uid matches `alice`'s owner sees only
    /// alice's hooks; bob's are filtered out.
    #[tokio::test]
    async fn non_root_owner_sees_only_owned_hooks() {
        let tmp = TempDir::new().unwrap();
        let _r = install_resolver();
        let config = config_with_hooks(tmp.path());
        let handle = ConfigHandle::new(config);
        let state_ctx = StateContext::new(tmp.path().to_path_buf(), handle);

        let caller = Caller::new(4242, 4242, None);
        let resp = handle_hook_list(&state_ctx, &caller).await;
        let body = match resp {
            JsonAckResponse::Ok { body } => body,
            other => panic!("expected Ok, got {other:?}"),
        };
        let rows: Vec<HookListRow> = serde_json::from_slice(&body).unwrap();
        let mailboxes: Vec<&str> = rows.iter().map(|r| r.mailbox.as_str()).collect();
        assert_eq!(mailboxes, vec!["alice", "alice"]);
        // Sorted by (mailbox, event, name) — after_send comes before
        // on_receive lexicographically.
        assert_eq!(rows[0].event, HookEvent::AfterSend);
        assert_eq!(rows[1].event, HookEvent::OnReceive);
    }

    /// A stranger uid sees an empty array — never `null` and never a
    /// human-readable "no hooks" string.
    #[tokio::test]
    async fn stranger_sees_empty_array() {
        let tmp = TempDir::new().unwrap();
        let _r = install_resolver();
        let config = config_with_hooks(tmp.path());
        let handle = ConfigHandle::new(config);
        let state_ctx = StateContext::new(tmp.path().to_path_buf(), handle);

        let stranger = Caller::new(99999, 99999, None);
        let resp = handle_hook_list(&state_ctx, &stranger).await;
        let body = match resp {
            JsonAckResponse::Ok { body } => body,
            other => panic!("expected Ok, got {other:?}"),
        };
        assert_eq!(body, b"[]");
    }

    /// Hooks on mailboxes the caller doesn't own are filtered out even
    /// when other mailboxes the caller does own carry hooks of the
    /// same name. The visibility filter runs per-mailbox.
    #[tokio::test]
    async fn hooks_on_unowned_mailboxes_are_filtered() {
        let tmp = TempDir::new().unwrap();
        let _r = install_resolver();
        let config = config_with_hooks(tmp.path());
        let handle = ConfigHandle::new(config);
        let state_ctx = StateContext::new(tmp.path().to_path_buf(), handle);

        // Bob (uid 5050) should only see bob's mailbox's hooks.
        let caller = Caller::new(5050, 5050, None);
        let resp = handle_hook_list(&state_ctx, &caller).await;
        let body = match resp {
            JsonAckResponse::Ok { body } => body,
            other => panic!("expected Ok, got {other:?}"),
        };
        let rows: Vec<HookListRow> = serde_json::from_slice(&body).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].mailbox, "bob");
        assert_eq!(rows[0].name, "b-recv");
    }

    /// `HookListRow` round-trips through serde: a row encoded by the
    /// daemon decodes identically client-side. Pins the schema so a
    /// future field rename can't silently desync the wire shape.
    #[test]
    fn hook_list_row_serde_round_trip() {
        let row = HookListRow {
            name: "recv-1".to_string(),
            mailbox: "alice".to_string(),
            event: HookEvent::OnReceive,
            cmd: vec!["/bin/echo".to_string(), "hi".to_string()],
            fire_on_untrusted: true,
            timeout_secs: 120,
        };
        let json = serde_json::to_string(&row).unwrap();
        // event must serialize as snake_case so the wire matches the
        // CLI's existing rendering.
        assert!(json.contains("\"event\":\"on_receive\""), "{json}");
        let decoded: HookListRow = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, row);
    }
}
