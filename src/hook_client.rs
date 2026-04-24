//! Client-side UDS helpers for `aimx hooks create --template` and
//! `aimx hooks delete`.
//!
//! Raw-cmd hooks (`aimx hooks create --cmd`) do NOT traverse the UDS —
//! they write `config.toml` directly and SIGHUP the daemon. Only
//! template-bound hook creation reaches `aimx.sock`, matching the PRD
//! §6.5 authorization split: the socket is world-writable, the DKIM
//! key isolation is the trust boundary, and nothing submittable over
//! the socket is allowed to pivot into arbitrary argv.
//!
//! Mirrors the `mailbox_crud` submission path in [`crate::mcp`]: try the
//! daemon UDS first so `Arc<Config>` hot-swaps and the new hook fires on
//! the next event, and surface socket-missing errors distinctly so the
//! caller can fall back (or fail explicitly — raw-cmd never falls back).

use std::collections::BTreeMap;

use crate::hook::HookEvent;
use crate::mcp::{MarkOutcome, is_socket_missing, parse_ack_response};
use crate::send_protocol::{
    self, HookCreateRequest, HookDeleteRequest, HookTemplateCreateBody, TemplateCreateRequest,
    TemplateDeleteRequest, TemplateUpdateRequest,
};

/// Outcome of a hook CRUD submission that didn't succeed via UDS. Tracks
/// socket-missing distinctly from daemon-side errors so the CLI can
/// decide whether to fall back to a direct on-disk edit.
pub(crate) enum HookCrudFallback {
    /// Socket not present / not connectable (daemon stopped, socket
    /// cleaned up, first-time setup). Callers fall back to direct edit.
    SocketMissing,
    /// Daemon connected and answered but reported an error (validation,
    /// NOTFOUND, IO, ...). Caller should surface this verbatim.
    Daemon(String),
}

/// Submit a template-bound `HOOK-CREATE` request over UDS. The daemon
/// validates the template, binds `params`, stamps `origin = "mcp"`, and
/// hot-swaps the in-memory `Arc<Config>` atomically.
pub(crate) fn submit_hook_template_create_via_daemon(
    mailbox: &str,
    event: HookEvent,
    template: &str,
    params: BTreeMap<String, String>,
    name: Option<&str>,
) -> Result<(), HookCrudFallback> {
    let body_bytes = match serde_json::to_vec(&HookTemplateCreateBody { params }) {
        Ok(v) => v,
        Err(e) => {
            return Err(HookCrudFallback::Daemon(format!(
                "failed to serialize HOOK-CREATE body: {e}"
            )));
        }
    };
    let request = HookCreateRequest {
        mailbox: mailbox.to_string(),
        event: event.as_str().to_string(),
        template: template.to_string(),
        name: name.map(str::to_string),
        body: body_bytes,
    };
    let socket = crate::serve::aimx_socket_path();

    let rt = tokio::runtime::Handle::try_current();
    let io_result: Result<MarkOutcome, std::io::Error> = match rt {
        Ok(handle) => tokio::task::block_in_place(|| {
            handle.block_on(submit_hook_create_request(&socket, &request))
        }),
        Err(_) => {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|e| {
                    HookCrudFallback::Daemon(format!("Failed to create tokio runtime: {e}"))
                })?;
            rt.block_on(submit_hook_create_request(&socket, &request))
        }
    };

    map_io_outcome(io_result, &socket)
}

/// Submit a `HOOK-DELETE` request over UDS.
pub(crate) fn submit_hook_delete_via_daemon(name: &str) -> Result<(), HookCrudFallback> {
    let request = HookDeleteRequest {
        name: name.to_string(),
    };
    let socket = crate::serve::aimx_socket_path();

    let rt = tokio::runtime::Handle::try_current();
    let io_result: Result<MarkOutcome, std::io::Error> = match rt {
        Ok(handle) => tokio::task::block_in_place(|| {
            handle.block_on(submit_hook_delete_request(&socket, &request))
        }),
        Err(_) => {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|e| {
                    HookCrudFallback::Daemon(format!("Failed to create tokio runtime: {e}"))
                })?;
            rt.block_on(submit_hook_delete_request(&socket, &request))
        }
    };

    map_io_outcome(io_result, &socket)
}

fn map_io_outcome(
    io_result: Result<MarkOutcome, std::io::Error>,
    socket: &std::path::Path,
) -> Result<(), HookCrudFallback> {
    match io_result {
        Ok(MarkOutcome::Ok) => Ok(()),
        Ok(MarkOutcome::Err { code, reason }) => Err(HookCrudFallback::Daemon(format!(
            "[{}] {reason}",
            code.as_str()
        ))),
        Ok(MarkOutcome::Malformed(reason)) => Err(HookCrudFallback::Daemon(format!(
            "Malformed response from aimx daemon: {reason}"
        ))),
        Err(e) => {
            if is_socket_missing(&e) {
                Err(HookCrudFallback::SocketMissing)
            } else {
                Err(HookCrudFallback::Daemon(format!(
                    "Failed to connect to aimx daemon at {}: {e}",
                    socket.display()
                )))
            }
        }
    }
}

async fn submit_hook_create_request(
    socket_path: &std::path::Path,
    request: &HookCreateRequest,
) -> Result<MarkOutcome, std::io::Error> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let stream = tokio::net::UnixStream::connect(socket_path).await?;
    let (mut reader, mut writer) = stream.into_split();

    send_protocol::write_hook_create_request(&mut writer, request).await?;
    writer.shutdown().await.ok();

    let mut buf = Vec::with_capacity(128);
    reader.read_to_end(&mut buf).await?;

    Ok(parse_ack_response(&buf))
}

async fn submit_hook_delete_request(
    socket_path: &std::path::Path,
    request: &HookDeleteRequest,
) -> Result<MarkOutcome, std::io::Error> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let stream = tokio::net::UnixStream::connect(socket_path).await?;
    let (mut reader, mut writer) = stream.into_split();

    send_protocol::write_hook_delete_request(&mut writer, request).await?;
    writer.shutdown().await.ok();

    let mut buf = Vec::with_capacity(128);
    reader.read_to_end(&mut buf).await?;

    Ok(parse_ack_response(&buf))
}

/// Outcome of a template-CRUD submission that didn't succeed. Keeps
/// socket-missing distinct from daemon-reported errors so `agent-setup`
/// can map each case to its own exit code and user-facing message per
/// §6.6 (daemon-down → exit 2, ECONFLICT → "use --redetect", etc.).
#[derive(Debug)]
pub(crate) enum TemplateCrudFallback {
    /// Socket not present / not connectable (daemon stopped, socket
    /// cleaned up, first-time setup).
    SocketMissing,
    /// Daemon answered with `AIMX/1 ERR <code> <reason>`.
    Daemon { code: String, reason: String },
    /// Local I/O or protocol-framing error surfaced after a successful
    /// connect (malformed response, runtime setup failure, etc.).
    Local(String),
}

/// Submit an `AIMX/1 TEMPLATE-CREATE` request over UDS. Mirrors
/// [`submit_hook_template_create_via_daemon`] but for the template-scope
/// template-scope verbs. Used by `aimx agent-setup` to register the
/// `invoke-<agent>-<username>` template after the plugin files are
/// installed.
pub(crate) fn submit_template_create_via_daemon(
    request: &TemplateCreateRequest,
) -> Result<(), TemplateCrudFallback> {
    let socket = crate::serve::aimx_socket_path();
    let io_result: Result<MarkOutcome, std::io::Error> =
        run_on_runtime(|| async { submit_template_create_request(&socket, request).await })?;
    map_template_outcome(io_result, &socket)
}

/// Submit an `AIMX/1 TEMPLATE-UPDATE` request over UDS. Used by
/// `aimx agent-setup --redetect` to refresh the registered template's
/// `cmd[0]` after the agent binary has moved.
pub(crate) fn submit_template_update_via_daemon(
    request: &TemplateUpdateRequest,
) -> Result<(), TemplateCrudFallback> {
    let socket = crate::serve::aimx_socket_path();
    let io_result: Result<MarkOutcome, std::io::Error> =
        run_on_runtime(|| async { submit_template_update_request(&socket, request).await })?;
    map_template_outcome(io_result, &socket)
}

/// Submit an `AIMX/1 TEMPLATE-DELETE` request over UDS. Used by
/// `aimx agent-cleanup <agent>` to drop the caller's
/// `invoke-<agent>-<username>` template when the operator uninstalls
/// the agent. The daemon enforces caller-uid = template.run_as so
/// each user can only delete their own templates.
pub(crate) fn submit_template_delete_via_daemon(
    request: &TemplateDeleteRequest,
) -> Result<(), TemplateCrudFallback> {
    let socket = crate::serve::aimx_socket_path();
    let io_result: Result<MarkOutcome, std::io::Error> =
        run_on_runtime(|| async { submit_template_delete_request(&socket, request).await })?;
    map_template_outcome(io_result, &socket)
}

fn run_on_runtime<F, Fut>(
    make_fut: F,
) -> Result<Result<MarkOutcome, std::io::Error>, TemplateCrudFallback>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Result<MarkOutcome, std::io::Error>>,
{
    match tokio::runtime::Handle::try_current() {
        Ok(handle) => Ok(tokio::task::block_in_place(|| handle.block_on(make_fut()))),
        Err(_) => {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|e| {
                    TemplateCrudFallback::Local(format!("Failed to create tokio runtime: {e}"))
                })?;
            Ok(rt.block_on(make_fut()))
        }
    }
}

fn map_template_outcome(
    io_result: Result<MarkOutcome, std::io::Error>,
    socket: &std::path::Path,
) -> Result<(), TemplateCrudFallback> {
    match io_result {
        Ok(MarkOutcome::Ok) => Ok(()),
        Ok(MarkOutcome::Err { code, reason }) => Err(TemplateCrudFallback::Daemon {
            code: code.as_str().to_string(),
            reason,
        }),
        Ok(MarkOutcome::Malformed(reason)) => Err(TemplateCrudFallback::Local(format!(
            "Malformed response from aimx daemon: {reason}"
        ))),
        Err(e) => {
            if is_socket_missing(&e) {
                Err(TemplateCrudFallback::SocketMissing)
            } else {
                Err(TemplateCrudFallback::Local(format!(
                    "Failed to connect to aimx daemon at {}: {e}",
                    socket.display()
                )))
            }
        }
    }
}

async fn submit_template_create_request(
    socket_path: &std::path::Path,
    request: &TemplateCreateRequest,
) -> Result<MarkOutcome, std::io::Error> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let stream = tokio::net::UnixStream::connect(socket_path).await?;
    let (mut reader, mut writer) = stream.into_split();

    send_protocol::write_template_create_request(&mut writer, request).await?;
    writer.shutdown().await.ok();

    let mut buf = Vec::with_capacity(128);
    reader.read_to_end(&mut buf).await?;

    Ok(parse_ack_response(&buf))
}

async fn submit_template_update_request(
    socket_path: &std::path::Path,
    request: &TemplateUpdateRequest,
) -> Result<MarkOutcome, std::io::Error> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let stream = tokio::net::UnixStream::connect(socket_path).await?;
    let (mut reader, mut writer) = stream.into_split();

    send_protocol::write_template_update_request(&mut writer, request).await?;
    writer.shutdown().await.ok();

    let mut buf = Vec::with_capacity(128);
    reader.read_to_end(&mut buf).await?;

    Ok(parse_ack_response(&buf))
}

async fn submit_template_delete_request(
    socket_path: &std::path::Path,
    request: &TemplateDeleteRequest,
) -> Result<MarkOutcome, std::io::Error> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let stream = tokio::net::UnixStream::connect(socket_path).await?;
    let (mut reader, mut writer) = stream.into_split();

    send_protocol::write_template_delete_request(&mut writer, request).await?;
    writer.shutdown().await.ok();

    let mut buf = Vec::with_capacity(128);
    reader.read_to_end(&mut buf).await?;

    Ok(parse_ack_response(&buf))
}
