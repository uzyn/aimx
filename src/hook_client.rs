//! Client-side UDS helpers for `aimx hooks create` / `aimx hooks delete`.
//!
//! Mirrors the `mailbox_crud` submission path in [`crate::mcp`]: try the
//! daemon UDS first so `Arc<Config>` hot-swaps and the new hook fires on
//! the next event, and surface socket-missing errors distinctly so the
//! caller can fall back to a direct `config.toml` edit + restart hint.

use crate::hook::Hook;
use crate::mcp::{MarkOutcome, is_socket_missing, parse_ack_response};
use crate::send_protocol::{self, HookCreateRequest, HookDeleteRequest};

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

/// Submit a `HOOK-CREATE` request over UDS. The hook stanza is serialized
/// to TOML and shipped as the body; the daemon validates + appends +
/// hot-swaps the in-memory `Arc<Config>` atomically.
pub(crate) fn submit_hook_create_via_daemon(
    mailbox: &str,
    hook: &Hook,
) -> Result<(), HookCrudFallback> {
    let hook_toml = match toml::to_string_pretty(hook) {
        Ok(s) => s.into_bytes(),
        Err(e) => {
            return Err(HookCrudFallback::Daemon(format!(
                "failed to serialize hook: {e}"
            )));
        }
    };
    let request = HookCreateRequest {
        mailbox: mailbox.to_string(),
        hook_toml,
    };
    let socket = crate::serve::send_socket_path();

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
    let socket = crate::serve::send_socket_path();

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
