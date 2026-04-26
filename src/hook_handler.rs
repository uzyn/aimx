//! Daemon-side handlers for the `HOOK-CREATE` and `HOOK-DELETE` verbs of
//! the `AIMX/1` UDS protocol.
//!
//! With the legacy template / origin / `dangerously_*` schema removed,
//! the UDS hook verbs are stubs that return `ERR PROTOCOL` until the
//! UDS hardening pass rewires them under the new auth predicate.
//! Operators continue to manage hooks via `sudo aimx hooks create
//! --cmd` / `sudo aimx hooks delete`, both of which write
//! `config.toml` directly and SIGHUP the daemon.

use crate::mailbox_handler::MailboxContext;
use crate::send_protocol::{AckResponse, ErrCode, HookCreateRequest, HookDeleteRequest};
use crate::state_handler::StateContext;
use crate::uds_authz::Caller;

pub async fn handle_hook_create(
    _state_ctx: &StateContext,
    _mb_ctx: &MailboxContext,
    _req: &HookCreateRequest,
    _caller: &Caller,
) -> AckResponse {
    AckResponse::Err {
        code: ErrCode::Protocol,
        reason: "HOOK-CREATE over UDS is not supported in this build; \
                 use `sudo aimx hooks create --cmd ...`"
            .to_string(),
    }
}

pub async fn handle_hook_delete(
    _state_ctx: &StateContext,
    _mb_ctx: &MailboxContext,
    _req: &HookDeleteRequest,
    _caller: &Caller,
) -> AckResponse {
    AckResponse::Err {
        code: ErrCode::Protocol,
        reason: "HOOK-DELETE over UDS is not supported in this build; \
                 use `sudo aimx hooks delete <name>`"
            .to_string(),
    }
}
