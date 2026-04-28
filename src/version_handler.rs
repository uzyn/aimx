//! Daemon-side handler for the `VERSION` verb of the `AIMX/1` UDS
//! protocol.
//!
//! The handler is intentionally trivial: it reads the four
//! `crate::version` build-metadata helpers and writes a
//! [`VersionResponse`] frame. There is no `SO_PEERCRED` filter — the
//! payload is build-time metadata and is not sensitive, mirroring the
//! posture of `MAILBOX-LIST` which also returns daemon-derived data
//! without an authz gate.
//!
//! Used by `aimx doctor` to detect drift between the on-disk binary
//! (`crate::version::release_tag()` of the invoking process) and the
//! still-running pre-upgrade daemon. Drift is informational only: the
//! Doctor surface renders an inline warn suffix but does not raise a
//! finding or change the exit code.

use std::path::Path;
use std::time::Duration;

use crate::send_protocol::{VersionResponse, read_version_response, write_version_request};
use crate::version;

/// Total budget for the doctor's daemon version probe (connect plus
/// write plus read). Doctor must not stall when the daemon is hung;
/// 500 ms is an order of magnitude beyond the steady-state UDS
/// round-trip and fits inside an interactive shell prompt.
const PROBE_TIMEOUT: Duration = Duration::from_millis(500);

/// Reason the doctor probe came back empty. Distinguished so the
/// rendered Doctor line can say "(daemon not reachable…)" only when
/// we actually failed to talk to the daemon — a missing socket on a
/// freshly-installed host is not a drift signal at all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProbeError {
    /// `aimx.sock` does not exist / cannot be connected to. Treated by
    /// the doctor as "daemon not running" rather than a hard failure.
    SocketMissing,
    /// Connect, write, read, parse, or timeout failure. The contained
    /// string is operator-facing and safe to render.
    Io(String),
}

impl std::fmt::Display for ProbeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProbeError::SocketMissing => write!(f, "socket missing"),
            ProbeError::Io(s) => write!(f, "{s}"),
        }
    }
}

/// Compose a `VersionResponse` from the local build's
/// [`crate::version`] helpers. The daemon's UDS dispatcher
/// ([`handle_uds_connection_with_timeout`](crate::serve)) calls this
/// for the `Request::Version` arm and writes the resulting frame via
/// [`crate::send_protocol::write_version_response`].
pub fn current_version_response() -> VersionResponse {
    VersionResponse {
        tag: version::release_tag().to_string(),
        git_hash: version::git_hash().to_string(),
        target: version::target_triple().to_string(),
        build_date: version::build_date().to_string(),
    }
}

/// Probe the running daemon's `VERSION` over `aimx.sock` with a hard
/// [`PROBE_TIMEOUT`] budget. Returns `Err(ProbeError::SocketMissing)`
/// when the socket file does not exist or refuses connection so the
/// doctor can degrade gracefully on freshly-installed hosts.
///
/// Sync facade: builds a current-thread tokio runtime, since
/// [`crate::doctor::gather_status_with_ops`] is sync and `aimx
/// doctor` is not running inside an existing tokio context.
pub fn probe_daemon_version(socket_path: &Path) -> Result<VersionResponse, ProbeError> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| ProbeError::Io(format!("failed to build tokio runtime: {e}")))?;
    rt.block_on(probe_daemon_version_async(socket_path))
}

/// Async core of [`probe_daemon_version`]; exposed for tests that
/// already run inside a tokio runtime.
pub async fn probe_daemon_version_async(socket_path: &Path) -> Result<VersionResponse, ProbeError> {
    if !socket_path.exists() {
        return Err(ProbeError::SocketMissing);
    }

    let result = tokio::time::timeout(PROBE_TIMEOUT, async move {
        use tokio::io::AsyncWriteExt;
        let stream = tokio::net::UnixStream::connect(socket_path)
            .await
            .map_err(|e| {
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::NotFound
                        | std::io::ErrorKind::ConnectionRefused
                        | std::io::ErrorKind::PermissionDenied
                ) {
                    ProbeError::SocketMissing
                } else {
                    ProbeError::Io(format!("connect: {e}"))
                }
            })?;
        let (mut reader, mut writer) = stream.into_split();
        write_version_request(&mut writer)
            .await
            .map_err(|e| ProbeError::Io(format!("write: {e}")))?;
        // Half-close so the daemon flushes the response and drops the
        // connection cleanly. Mirrors how `aimx send` ends its
        // request frame.
        writer
            .shutdown()
            .await
            .map_err(|e| ProbeError::Io(format!("shutdown: {e}")))?;
        read_version_response(&mut reader)
            .await
            .map_err(|e| ProbeError::Io(format!("read: {e}")))
    })
    .await;

    match result {
        Ok(inner) => inner,
        Err(_) => Err(ProbeError::Io(format!(
            "probe timed out after {} ms",
            PROBE_TIMEOUT.as_millis()
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::send_protocol::{read_version_response, write_version_response};
    use std::io::Cursor;

    /// Missing socket file resolves to [`ProbeError::SocketMissing`]
    /// rather than a generic I/O error so the doctor can render
    /// "(daemon not reachable…)" instead of a confusing
    /// "no such file or directory".
    #[tokio::test]
    async fn probe_returns_socket_missing_when_path_absent() {
        let tmp = tempfile::TempDir::new().unwrap();
        let sock = tmp.path().join("aimx.sock");
        match probe_daemon_version_async(&sock).await {
            Err(ProbeError::SocketMissing) => {}
            other => panic!("expected SocketMissing, got {other:?}"),
        }
    }

    /// A fake server bound to a tempdir-backed UDS replies with a
    /// fixed `VersionResponse`; the probe parses every field
    /// faithfully.
    #[tokio::test]
    async fn probe_round_trips_against_fake_server() {
        use crate::send_protocol::write_version_response;
        use tokio::io::AsyncWriteExt;
        let tmp = tempfile::TempDir::new().unwrap();
        let sock = tmp.path().join("aimx.sock");
        let listener = tokio::net::UnixListener::bind(&sock).unwrap();

        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            // Drain the request frame so the codec sees a clean EOF.
            let mut req = Vec::with_capacity(64);
            use tokio::io::AsyncReadExt;
            // Reading up to 64 bytes is enough for the bare request
            // frame ("AIMX/1 VERSION\n\n") plus padding.
            let _ = stream.read_buf(&mut req).await;
            let resp = VersionResponse {
                tag: "v9.9.9".into(),
                git_hash: "deadbeef".into(),
                target: "x86_64-unknown-linux-gnu".into(),
                build_date: "2026-04-28".into(),
            };
            write_version_response(&mut stream, &resp).await.unwrap();
            stream.shutdown().await.unwrap();
        });

        let parsed = probe_daemon_version_async(&sock).await.unwrap();
        assert_eq!(parsed.tag, "v9.9.9");
        assert_eq!(parsed.git_hash, "deadbeef");
        server.await.unwrap();
    }

    /// `current_version_response()` writes a frame the codec parser
    /// accepts and every field round-trips through the wire.
    #[tokio::test]
    async fn current_response_round_trips_through_codec() {
        let resp = current_version_response();
        let mut buf: Vec<u8> = Vec::new();
        write_version_response(&mut buf, &resp).await.unwrap();
        let mut reader = Cursor::new(buf);
        let parsed = read_version_response(&mut reader).await.unwrap();
        assert_eq!(parsed.tag, version::release_tag());
        assert_eq!(parsed.git_hash, version::git_hash());
        assert_eq!(parsed.target, version::target_triple());
        assert_eq!(parsed.build_date, version::build_date());
        // Git hash should be 8 chars in production builds (or
        // "unknown" outside a git checkout).
        assert!(parsed.git_hash.len() == 8 || parsed.git_hash == "unknown");
    }
}
