use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::config::Config;
use crate::dkim;
use crate::send_handler::SendContext;
use crate::send_protocol;
use crate::smtp::SmtpServer;
use crate::term;
use crate::transport::{FileDropTransport, LettreTransport, MailTransport};

const DEFAULT_BIND: &str = "0.0.0.0:25";
const DEFAULT_TLS_CERT: &str = "/etc/ssl/aimx/cert.pem";
const DEFAULT_TLS_KEY: &str = "/etc/ssl/aimx/key.pem";
const DEFAULT_RUNTIME_DIR: &str = "/run/aimx";
const RUNTIME_DIR_ENV: &str = "AIMX_RUNTIME_DIR";
const SEND_SOCKET_NAME: &str = "send.sock";

/// Resolve the runtime directory that holds `/run/aimx/send.sock`.
///
/// Precedence:
/// 1. `AIMX_RUNTIME_DIR` env var (tests and non-standard installs)
/// 2. `/run/aimx/` default (provided by systemd `RuntimeDirectory=aimx`
///    or the OpenRC `start_pre` `checkpath` hook)
pub fn runtime_dir() -> PathBuf {
    std::env::var_os(RUNTIME_DIR_ENV)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(DEFAULT_RUNTIME_DIR))
}

/// Path to the world-writable `AIMX/1 SEND` UDS.
pub fn send_socket_path() -> PathBuf {
    runtime_dir().join(SEND_SOCKET_NAME)
}

pub fn run(
    bind: Option<&str>,
    tls_cert: Option<&str>,
    tls_key: Option<&str>,
    config: Config,
) -> Result<(), Box<dyn std::error::Error>> {
    let bind_addr = bind.unwrap_or(DEFAULT_BIND);
    let tls_explicit = tls_cert.is_some() || tls_key.is_some();
    let cert_path = tls_cert.unwrap_or(DEFAULT_TLS_CERT);
    let key_path = tls_key.unwrap_or(DEFAULT_TLS_KEY);

    let rt = tokio::runtime::Runtime::new()
        .map_err(|e| format!("Failed to create tokio runtime: {e}"))?;

    rt.block_on(run_serve(
        config,
        bind_addr,
        cert_path,
        key_path,
        tls_explicit,
    ))
}

async fn run_serve(
    config: Config,
    bind_addr: &str,
    cert_path: &str,
    key_path: &str,
    tls_explicit: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    // Refresh the agent-facing README if the baked-in version differs from
    // what is on disk. Runs before any listener is bound so the file is
    // up-to-date by the time agents read it.
    if let Err(e) = crate::datadir_readme::refresh_if_outdated(&config.data_dir) {
        eprintln!(
            "{} Failed to refresh datadir README: {e}",
            term::warn("Warning:")
        );
    }

    let cert = std::path::Path::new(cert_path);
    let key = std::path::Path::new(key_path);

    let tls_available = can_read_tls(cert, key);
    if !tls_available {
        if tls_explicit {
            return Err(
                format!("TLS cert/key not readable at {} / {}", cert_path, key_path).into(),
            );
        }
        eprintln!(
            "{} TLS cert/key not found at {cert_path} / {key_path}, running without STARTTLS",
            term::warn("Warning:")
        );
    }

    // Load DKIM key once at startup — every accepted UDS send reuses this
    // in-memory key. A failure here is fatal: the daemon cannot sign
    // outbound mail without it.
    let dkim_root = crate::config::dkim_dir();
    let dkim_key = match dkim::load_private_key(&dkim_root) {
        Ok(k) => Arc::new(k),
        Err(e) => {
            return Err(format!(
                "Failed to load DKIM private key from {}: {e}. \
                 `aimx serve` requires a readable DKIM private key \
                 (generate with `aimx setup` or `aimx dkim-keygen`).",
                dkim_root.join("private.key").display()
            )
            .into());
        }
    };

    // Build the SendContext shared across every per-connection UDS task.
    //
    // `AIMX_TEST_MAIL_DROP` (test-only) replaces the real MX transport with
    // a file-drop transport so integration tests can observe the signed
    // outbound message without reaching the network. In production this env
    // var is never set — if it leaks in, emit a loud warning so the operator
    // notices that outbound mail is being siloed to disk instead of delivered.
    let transport: Arc<dyn MailTransport + Send + Sync> = match std::env::var_os(
        "AIMX_TEST_MAIL_DROP",
    ) {
        Some(path) => {
            let drop_path = PathBuf::from(&path);
            eprintln!(
                "{} AIMX_TEST_MAIL_DROP is set — outbound mail will be written to {} and NOT delivered to recipients. This must only be used in tests; unset the env var on production hosts.",
                term::warn("Warning:"),
                drop_path.display()
            );
            Arc::new(FileDropTransport::new(drop_path))
        }
        None => Arc::new(LettreTransport::new(config.enable_ipv6)),
    };
    let send_ctx = Arc::new(SendContext {
        dkim_key,
        primary_domain: config.domain.clone(),
        dkim_selector: config.dkim_selector.clone(),
        registered_mailboxes: config.mailboxes.keys().cloned().collect(),
        transport,
        data_dir: config.data_dir.clone(),
    });

    let server = SmtpServer::new(config);
    let server = if tls_available {
        server.with_tls(cert, key)?
    } else {
        server
    };

    let listener = tokio::net::TcpListener::bind(bind_addr)
        .await
        .map_err(|e| format!("Failed to bind to {bind_addr}: {e}"))?;

    let actual_addr = listener.local_addr()?;
    eprintln!("{}", term::header("AIMX SMTP listener"));
    eprintln!("  bind:  {}", term::highlight(&actual_addr.to_string()));
    eprintln!(
        "  tls:   {}",
        if tls_available {
            term::success("enabled")
        } else {
            term::warn("disabled")
        }
    );

    // Bind the UDS send socket. Fatal on failure.
    let socket_path = send_socket_path();
    let uds_listener =
        bind_send_socket(&socket_path).map_err(|e| -> Box<dyn std::error::Error> {
            format!(
                "Failed to bind UDS send socket at {}: {e}",
                socket_path.display()
            )
            .into()
        })?;
    eprintln!(
        "  send:  {} (mode 0o666, world-writable)",
        term::highlight(&socket_path.display().to_string())
    );

    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);

    tokio::spawn(async move {
        let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to install SIGTERM handler");
        let sigint = tokio::signal::ctrl_c();

        tokio::select! {
            _ = sigterm.recv() => {},
            _ = sigint => {},
        }

        let _ = shutdown_tx.send(true);
    });

    // Run the SMTP server and UDS listener concurrently. Both observe the
    // same shutdown watch — a SIGTERM drains both accept loops.
    let uds_shutdown = shutdown_rx.clone();
    let uds_socket_path = socket_path.clone();
    let uds_handle = tokio::spawn(async move {
        run_send_listener(uds_listener, send_ctx, uds_shutdown).await;
        // Clean up the socket file on clean shutdown so the next start does
        // not trip the "stale socket" fallback path.
        let _ = std::fs::remove_file(&uds_socket_path);
    });

    let in_flight_msg = server.run(listener, shutdown_rx).await;

    // Wait for the UDS listener to drain too so we don't leak its task.
    let _ = uds_handle.await;

    eprintln!("{}", term::info("AIMX SMTP listener shut down"));

    in_flight_msg
}

/// Bind the UDS send socket at `path` with mode `0o666`.
///
/// Handles the stale-socket-after-crash case: if the path already refers to
/// a socket, unlink it and retry once. A second failure is fatal.
pub fn bind_send_socket(path: &Path) -> std::io::Result<tokio::net::UnixListener> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let listener = match tokio::net::UnixListener::bind(path) {
        Ok(l) => l,
        Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
            // Stale socket from a prior crash. Unlink and retry once.
            std::fs::remove_file(path)?;
            tokio::net::UnixListener::bind(path)?
        }
        Err(e) => return Err(e),
    };
    set_socket_mode(path, 0o666)?;
    Ok(listener)
}

#[cfg(unix)]
fn set_socket_mode(path: &Path, mode: u32) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
}

#[cfg(not(unix))]
fn set_socket_mode(_path: &Path, _mode: u32) -> std::io::Result<()> {
    Ok(())
}

async fn run_send_listener(
    listener: tokio::net::UnixListener,
    ctx: Arc<SendContext>,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
) {
    loop {
        tokio::select! {
            accept = listener.accept() => {
                match accept {
                    Ok((stream, _addr)) => {
                        let peer = peer_credentials(&stream);
                        eprintln!(
                            "[send] accepted: peer_uid={} peer_pid={}",
                            peer.uid_str(),
                            peer.pid_str()
                        );
                        let ctx = Arc::clone(&ctx);
                        tokio::spawn(async move {
                            handle_send_connection(stream, ctx).await;
                        });
                    }
                    Err(e) => {
                        eprintln!("[send] accept error: {e}");
                        // Transient — do not kill the listener.
                        continue;
                    }
                }
            }
            _ = shutdown.changed() => {
                break;
            }
        }
    }
    eprintln!("[send] UDS accept loop drained");
}

/// Per-connection UDS request timeout. A connected client must deliver its
/// entire `AIMX/1 SEND` request frame within this window or the connection
/// is dropped. Prevents slow-loris abuse on the world-writable socket.
const UDS_REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

async fn handle_send_connection(stream: tokio::net::UnixStream, ctx: Arc<SendContext>) {
    handle_send_connection_with_timeout(stream, ctx, UDS_REQUEST_TIMEOUT).await;
}

async fn handle_send_connection_with_timeout(
    stream: tokio::net::UnixStream,
    ctx: Arc<SendContext>,
    timeout: std::time::Duration,
) {
    let (mut reader, mut writer) = stream.into_split();
    let (response, parse_failed) =
        match tokio::time::timeout(timeout, send_protocol::parse_request(&mut reader)).await {
            Ok(Ok(req)) => (crate::send_handler::handle_send(req, &ctx).await, false),
            Ok(Err(send_protocol::ParseError::ClosedBeforeRequest)) => {
                // Client connected and closed without sending anything. No
                // response to write — just drop the connection.
                return;
            }
            Ok(Err(e)) => (
                send_protocol::SendResponse::Err {
                    code: send_protocol::ErrCode::Malformed,
                    reason: e.to_string(),
                },
                true,
            ),
            Err(_elapsed) => {
                eprintln!(
                    "[send] request timed out after {}s, dropping connection",
                    timeout.as_secs()
                );
                return;
            }
        };

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    // When the parser rejects the request (typically on the first line of
    // a malformed frame), the client may still have bytes in flight that
    // the kernel has queued on our receive side. If we close with unread
    // bytes the kernel issues an abortive close and the client's pending
    // `read` races the teardown and sees `ECONNRESET` instead of the
    // framed reply we are about to write. Drain here — but only on parse
    // failure, because a well-formed request has already been consumed up
    // through `Content-Length`, and further `read` would block on the
    // peer, deadlocking well-behaved clients that keep the write half
    // open until they have read our response.
    if parse_failed {
        let mut sink = [0u8; 1024];
        loop {
            match tokio::time::timeout(std::time::Duration::from_millis(50), reader.read(&mut sink))
                .await
            {
                Ok(Ok(0)) | Err(_) => break,
                Ok(Ok(_)) => continue,
                Ok(Err(_)) => break,
            }
        }
    }

    if let Err(e) = send_protocol::write_response(&mut writer, &response).await {
        eprintln!("[send] failed to write response: {e}");
    }
    let _ = writer.shutdown().await;
}

/// Peer-credential snapshot for logging. `None` on platforms/errors where
/// the kernel could not supply credentials — e.g. the client closed before
/// we asked. Used only for journald diagnostics (FR-18b) — never for
/// authorization.
struct PeerCred {
    uid: Option<u32>,
    pid: Option<i32>,
}

impl PeerCred {
    fn uid_str(&self) -> String {
        self.uid
            .map(|v| v.to_string())
            .unwrap_or_else(|| "?".into())
    }
    fn pid_str(&self) -> String {
        self.pid
            .map(|v| v.to_string())
            .unwrap_or_else(|| "?".into())
    }
}

fn peer_credentials(stream: &tokio::net::UnixStream) -> PeerCred {
    match stream.peer_cred() {
        Ok(c) => PeerCred {
            uid: Some(c.uid()),
            pid: c.pid(),
        },
        Err(_) => PeerCred {
            uid: None,
            pid: None,
        },
    }
}

fn can_read_tls(cert: &std::path::Path, key: &std::path::Path) -> bool {
    // Use the same check for both so a permissions mismatch between cert and
    // key is not masked: `metadata().is_file()` hides read-permission issues
    // that only surface at `File::open()`, while `File::open()` actually
    // touches the file descriptor path rustls will ultimately traverse.
    std::fs::File::open(cert).is_ok() && std::fs::File::open(key).is_ok()
}

pub mod service {
    pub fn generate_systemd_unit(aimx_path: &str, data_dir: &str) -> String {
        // `RuntimeDirectory=aimx` makes systemd create `/run/aimx/` at
        // service start (default mode 0755, root:root) and tear it down on
        // stop. The UDS send socket landing inside (Sprint 34) is
        // world-writable — authorization is out of scope in v0.2.
        format!(
            "[Unit]\n\
             Description=AIMX SMTP server\n\
             After=network-online.target nss-lookup.target\n\
             Wants=network-online.target\n\
             StartLimitBurst=5\n\
             StartLimitIntervalSec=60s\n\
             \n\
             [Service]\n\
             Type=simple\n\
             User=root\n\
             ExecStart={aimx_path} serve --data-dir {data_dir}\n\
             Restart=on-failure\n\
             RestartSec=5s\n\
             LimitNOFILE=65536\n\
             TasksMax=4096\n\
             ReadWritePaths={data_dir}\n\
             RuntimeDirectory=aimx\n\
             StandardOutput=journal\n\
             StandardError=journal\n\
             \n\
             [Install]\n\
             WantedBy=multi-user.target\n"
        )
    }

    pub fn generate_openrc_script(aimx_path: &str, data_dir: &str) -> String {
        // OpenRC has no direct `RuntimeDirectory=` analogue — use
        // `checkpath` in `start_pre` to mint `/run/aimx/` with mode 0755
        // every service start.
        format!(
            "#!/sbin/openrc-run\n\
             \n\
             description=\"AIMX SMTP server\"\n\
             command={aimx_path}\n\
             command_args=\"serve --data-dir {data_dir}\"\n\
             supervisor=supervise-daemon\n\
             \n\
             depend() {{\n\
             \tafter net\n\
             }}\n\
             \n\
             start_pre() {{\n\
             \tcheckpath -d -m 0755 -o root:root /run/aimx || return 1\n\
             }}\n"
        )
    }

    #[derive(Debug, PartialEq)]
    pub enum InitSystem {
        Systemd,
        OpenRC,
        Unknown,
    }

    pub fn detect_init_system() -> InitSystem {
        if std::path::Path::new("/run/systemd/system").exists() {
            InitSystem::Systemd
        } else if std::path::Path::new("/sbin/openrc").exists() {
            InitSystem::OpenRC
        } else {
            InitSystem::Unknown
        }
    }

    /// Pure dispatch table for `restart_service`: returns the `(program,
    /// args)` the real implementation will invoke. Split out so the
    /// systemd-vs-OpenRC routing can be unit-tested without shelling out.
    pub fn restart_service_command(
        init: &InitSystem,
        service: &str,
    ) -> Option<(&'static str, Vec<String>)> {
        match init {
            InitSystem::Systemd => Some((
                "sudo",
                vec![
                    "systemctl".to_string(),
                    "restart".to_string(),
                    service.to_string(),
                ],
            )),
            InitSystem::OpenRC => Some((
                "sudo",
                vec![
                    "rc-service".to_string(),
                    service.to_string(),
                    "restart".to_string(),
                ],
            )),
            InitSystem::Unknown => None,
        }
    }

    /// Pure dispatch table for `is_service_running`: returns the `(program,
    /// args)` to probe the service's running state.
    pub fn is_service_running_command(
        init: &InitSystem,
        service: &str,
    ) -> Option<(&'static str, Vec<String>)> {
        match init {
            InitSystem::Systemd => Some((
                "systemctl",
                vec![
                    "is-active".to_string(),
                    "--quiet".to_string(),
                    service.to_string(),
                ],
            )),
            InitSystem::OpenRC => Some((
                "rc-service",
                vec![service.to_string(), "status".to_string()],
            )),
            InitSystem::Unknown => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::service::*;
    use super::*;

    #[test]
    fn systemd_unit_contains_required_fields() {
        let unit = generate_systemd_unit("/usr/local/bin/aimx", "/var/lib/aimx");
        assert!(unit.contains("After=network-online.target nss-lookup.target"));
        assert!(unit.contains("Wants=network-online.target"));
        assert!(unit.contains("StartLimitBurst=5"));
        assert!(unit.contains("StartLimitIntervalSec=60s"));
        assert!(unit.contains("Type=simple"));
        assert!(unit.contains("ExecStart=/usr/local/bin/aimx serve --data-dir /var/lib/aimx"));
        assert!(unit.contains("Restart=on-failure"));
        assert!(unit.contains("RestartSec=5s"));
        assert!(unit.contains("LimitNOFILE=65536"));
        assert!(unit.contains("TasksMax=4096"));
        assert!(unit.contains("ReadWritePaths=/var/lib/aimx"));
        assert!(unit.contains("StandardOutput=journal"));
        assert!(unit.contains("StandardError=journal"));
        assert!(unit.contains("[Unit]"));
        assert!(unit.contains("[Service]"));
        assert!(unit.contains("[Install]"));
        assert!(unit.contains("WantedBy=multi-user.target"));
        // Intentionally omitted directives
        assert!(
            !unit.contains("ExecReload="),
            "ExecReload must not be set (no SIGHUP handler)"
        );
        assert!(
            !unit.contains("StateDirectory="),
            "StateDirectory must not be set (conflicts with --data-dir)"
        );
    }

    #[test]
    fn systemd_unit_custom_paths() {
        let unit = generate_systemd_unit("/opt/bin/aimx", "/data/aimx");
        assert!(unit.contains("ExecStart=/opt/bin/aimx serve --data-dir /data/aimx"));
    }

    #[test]
    fn systemd_unit_declares_runtime_dir_without_group() {
        let unit = generate_systemd_unit("/usr/local/bin/aimx", "/var/lib/aimx");
        assert!(
            unit.contains("RuntimeDirectory=aimx"),
            "systemd unit must declare RuntimeDirectory=aimx so `/run/aimx/` \
             is created by systemd"
        );
        assert!(
            !unit.contains("Group=aimx"),
            "Sprint 33.1 dropped the `aimx` group — the systemd unit must \
             not declare Group=aimx. The UDS send socket (Sprint 34) is \
             world-writable; authorization is out of scope in v0.2."
        );
        assert!(
            !unit.contains("RuntimeDirectoryMode="),
            "no explicit RuntimeDirectoryMode — default (0755, root:root) \
             is correct for a world-writable UDS socket"
        );
        assert!(
            unit.contains("User=root"),
            "User=root remains — the daemon still binds port 25"
        );
    }

    #[test]
    fn openrc_script_creates_runtime_dir_without_aimx_group() {
        let script = generate_openrc_script("/usr/local/bin/aimx", "/var/lib/aimx");
        assert!(
            script.contains("checkpath -d -m 0755 -o root:root /run/aimx"),
            "OpenRC script must mint /run/aimx with mode 0755 and owner \
             root:root (Sprint 33.1 dropped the aimx group): {script}"
        );
        assert!(
            !script.contains("command_user=\"root:aimx\""),
            "OpenRC script must not declare command_user with the aimx group \
             (Sprint 33.1)"
        );
        assert!(
            !script.contains("root:aimx"),
            "no remaining root:aimx references"
        );
        assert!(
            script.contains("start_pre()"),
            "start_pre hook is how OpenRC emulates systemd's RuntimeDirectory"
        );
    }

    #[test]
    fn systemd_unit_readwritepaths_follows_data_dir() {
        let unit = generate_systemd_unit("/opt/bin/aimx", "/custom/dir");
        assert!(
            unit.contains("ReadWritePaths=/custom/dir"),
            "ReadWritePaths must substitute the data_dir argument"
        );
        assert!(
            !unit.contains("ReadWritePaths=/var/lib/aimx"),
            "ReadWritePaths must not leak the default data_dir"
        );
    }

    #[test]
    fn openrc_script_contains_required_fields() {
        let script = generate_openrc_script("/usr/local/bin/aimx", "/var/lib/aimx");
        assert!(script.contains("#!/sbin/openrc-run"));
        assert!(script.contains("command=/usr/local/bin/aimx"));
        assert!(script.contains("command_args=\"serve --data-dir /var/lib/aimx\""));
        assert!(script.contains("supervisor=supervise-daemon"));
        assert!(script.contains("after net"));
    }

    #[test]
    fn openrc_script_custom_paths() {
        let script = generate_openrc_script("/opt/bin/aimx", "/data/aimx");
        assert!(script.contains("command=/opt/bin/aimx"));
        assert!(script.contains("command_args=\"serve --data-dir /data/aimx\""));
    }

    #[test]
    fn restart_service_systemd_dispatch() {
        let (prog, args) = restart_service_command(&InitSystem::Systemd, "aimx").unwrap();
        assert_eq!(prog, "sudo");
        assert_eq!(args, vec!["systemctl", "restart", "aimx"]);
    }

    #[test]
    fn restart_service_openrc_dispatch() {
        let (prog, args) = restart_service_command(&InitSystem::OpenRC, "aimx").unwrap();
        assert_eq!(prog, "sudo");
        assert_eq!(
            args,
            vec!["rc-service", "aimx", "restart"],
            "OpenRC restart must invoke `rc-service <svc> restart`, not systemctl"
        );
    }

    #[test]
    fn restart_service_unknown_init_returns_none() {
        assert!(restart_service_command(&InitSystem::Unknown, "aimx").is_none());
    }

    #[test]
    fn is_service_running_systemd_dispatch() {
        let (prog, args) = is_service_running_command(&InitSystem::Systemd, "aimx").unwrap();
        assert_eq!(prog, "systemctl");
        assert_eq!(args, vec!["is-active", "--quiet", "aimx"]);
    }

    #[test]
    fn is_service_running_openrc_dispatch() {
        let (prog, args) = is_service_running_command(&InitSystem::OpenRC, "aimx").unwrap();
        assert_eq!(prog, "rc-service");
        assert_eq!(
            args,
            vec!["aimx", "status"],
            "OpenRC status must invoke `rc-service <svc> status`, not systemctl"
        );
    }

    #[test]
    fn is_service_running_unknown_init_returns_none() {
        assert!(is_service_running_command(&InitSystem::Unknown, "aimx").is_none());
    }

    #[test]
    fn shutdown_signal_stops_accept_loop() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let tmp = tempfile::TempDir::new().unwrap();
            let mut mailboxes = std::collections::HashMap::new();
            mailboxes.insert(
                "catchall".to_string(),
                crate::config::MailboxConfig {
                    address: "*@test.local".to_string(),
                    on_receive: vec![],
                    trust: "none".to_string(),
                    trusted_senders: vec![],
                },
            );
            let config = crate::config::Config {
                domain: "test.local".to_string(),
                data_dir: tmp.path().to_path_buf(),
                dkim_selector: "dkim".to_string(),
                mailboxes,
                verify_host: None,
                enable_ipv6: false,
            };

            let server = crate::smtp::SmtpServer::new(config);
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let port = listener.local_addr().unwrap().port();
            let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);

            let handle = tokio::spawn(async move {
                server.run(listener, shutdown_rx).await.unwrap();
            });

            tokio::time::sleep(std::time::Duration::from_millis(50)).await;

            // Verify server is accepting connections
            let stream = tokio::net::TcpStream::connect(format!("127.0.0.1:{port}")).await;
            assert!(stream.is_ok());
            drop(stream);

            // Send shutdown signal
            shutdown_tx.send(true).unwrap();

            // Server task should complete
            let result = tokio::time::timeout(std::time::Duration::from_secs(5), handle).await;
            assert!(result.is_ok(), "Server should shut down within 5s");

            // New connections should be refused
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            let stream = tokio::net::TcpStream::connect(format!("127.0.0.1:{port}")).await;
            assert!(
                stream.is_err(),
                "Connection should be refused after shutdown"
            );
        });
    }

    #[test]
    fn runtime_dir_env_override_takes_precedence() {
        // Serialized via the same lock shape that `config::test_env` uses
        // would be ideal, but AIMX_RUNTIME_DIR is only touched in tests,
        // and these tests are all in this module, so a simple guard is
        // sufficient. See note in the body.
        use std::sync::Mutex;
        static GUARD: Mutex<()> = Mutex::new(());
        let _g = GUARD.lock().unwrap_or_else(|e| e.into_inner());

        let prev = std::env::var_os(RUNTIME_DIR_ENV);
        // SAFETY: serialized by `GUARD`; test restores the previous value
        // before returning.
        unsafe {
            std::env::set_var(RUNTIME_DIR_ENV, "/tmp/some-override");
        }
        assert_eq!(
            runtime_dir(),
            std::path::PathBuf::from("/tmp/some-override")
        );
        assert_eq!(
            send_socket_path(),
            std::path::PathBuf::from("/tmp/some-override/send.sock")
        );
        unsafe {
            match prev {
                Some(v) => std::env::set_var(RUNTIME_DIR_ENV, v),
                None => std::env::remove_var(RUNTIME_DIR_ENV),
            }
        }
    }

    #[test]
    fn runtime_dir_default_when_env_unset() {
        use std::sync::Mutex;
        static GUARD: Mutex<()> = Mutex::new(());
        let _g = GUARD.lock().unwrap_or_else(|e| e.into_inner());

        let prev = std::env::var_os(RUNTIME_DIR_ENV);
        unsafe {
            std::env::remove_var(RUNTIME_DIR_ENV);
        }
        assert_eq!(runtime_dir(), std::path::PathBuf::from("/run/aimx"));
        assert_eq!(
            send_socket_path(),
            std::path::PathBuf::from("/run/aimx/send.sock")
        );
        unsafe {
            if let Some(v) = prev {
                std::env::set_var(RUNTIME_DIR_ENV, v);
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn bind_send_socket_sets_world_writable_mode() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::TempDir::new().unwrap();
        let sock = tmp.path().join("send.sock");

        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let _listener = bind_send_socket(&sock).unwrap();
            let mode = std::fs::metadata(&sock).unwrap().permissions().mode() & 0o777;
            assert_eq!(
                mode, 0o666,
                "UDS send socket must be world-writable (0o666); got {mode:o}"
            );
        });
    }

    #[cfg(unix)]
    #[test]
    fn bind_send_socket_reclaims_stale_path() {
        let tmp = tempfile::TempDir::new().unwrap();
        let sock = tmp.path().join("send.sock");

        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            // Bind once, drop the listener — the file persists on disk,
            // simulating the "daemon crashed with no cleanup" case.
            {
                let _l = bind_send_socket(&sock).unwrap();
            }
            assert!(sock.exists(), "stale socket should remain");

            // Bind again — must succeed by unlinking + retry.
            let _l2 = bind_send_socket(&sock).unwrap();
            assert!(sock.exists());
        });
    }

    #[cfg(unix)]
    #[test]
    fn uds_accept_reads_peer_credentials() {
        use std::collections::HashSet;

        let tmp = tempfile::TempDir::new().unwrap();
        let sock = tmp.path().join("send.sock");

        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let listener = bind_send_socket(&sock).unwrap();

            // Build a throwaway SendContext. Key generation is the slow
            // step so we use a tempdir-backed one.
            let dkim_tmp = tempfile::TempDir::new().unwrap();
            crate::dkim::generate_keypair(dkim_tmp.path(), false).unwrap();
            let key = crate::dkim::load_private_key(dkim_tmp.path()).unwrap();
            let mut mboxes = HashSet::new();
            mboxes.insert("catchall".to_string());
            let transport: Arc<dyn MailTransport + Send + Sync> = Arc::new(NoopTransport);
            let ctx = Arc::new(crate::send_handler::SendContext {
                dkim_key: Arc::new(key),
                primary_domain: "example.com".to_string(),
                dkim_selector: "dkim".to_string(),
                registered_mailboxes: mboxes,
                transport,
                data_dir: tmp.path().to_path_buf(),
            });

            let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
            let handle = tokio::spawn(async move {
                run_send_listener(listener, ctx, shutdown_rx).await;
            });

            // Connect from the same process.
            let mut client = tokio::net::UnixStream::connect(&sock).await.unwrap();
            // Assert the client stream has peer credentials for the server
            // side too (mirrors the server-side read).
            let cred = client
                .peer_cred()
                .expect("peer_cred should succeed on local UDS");
            assert_eq!(cred.uid(), unsafe { libc::geteuid() });

            // Close without sending anything — server handler should fall
            // back to the "ClosedBeforeRequest" branch and drop cleanly.
            use tokio::io::AsyncWriteExt;
            let _ = client.shutdown().await;
            drop(client);

            // Shut down the listener.
            shutdown_tx.send(true).unwrap();
            let _ = tokio::time::timeout(std::time::Duration::from_secs(5), handle).await;
        });
    }

    struct NoopTransport;
    impl MailTransport for NoopTransport {
        fn send(
            &self,
            _: &str,
            _: &str,
            _: &[u8],
        ) -> Result<String, crate::transport::TransportError> {
            Ok("noop".into())
        }
    }

    #[cfg(unix)]
    #[test]
    fn uds_end_to_end_signed_delivery() {
        use std::collections::HashSet;
        use std::sync::Mutex;

        let tmp = tempfile::TempDir::new().unwrap();
        let sock = tmp.path().join("send.sock");

        // Transport that captures whatever is delivered.
        struct CapturingTransport {
            captured: Mutex<Vec<Vec<u8>>>,
        }
        impl MailTransport for CapturingTransport {
            fn send(
                &self,
                _: &str,
                _: &str,
                message: &[u8],
            ) -> Result<String, crate::transport::TransportError> {
                self.captured.lock().unwrap().push(message.to_vec());
                Ok("mock-mx".into())
            }
        }

        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let listener = bind_send_socket(&sock).unwrap();

            let dkim_tmp = tempfile::TempDir::new().unwrap();
            crate::dkim::generate_keypair(dkim_tmp.path(), false).unwrap();
            let key = crate::dkim::load_private_key(dkim_tmp.path()).unwrap();
            let pub_pem = std::fs::read_to_string(dkim_tmp.path().join("public.key")).unwrap();

            let captor = Arc::new(CapturingTransport {
                captured: Mutex::new(vec![]),
            });
            let transport: Arc<dyn MailTransport + Send + Sync> = captor.clone();

            let mut mboxes = HashSet::new();
            mboxes.insert("alice".to_string());
            let ctx = Arc::new(crate::send_handler::SendContext {
                dkim_key: Arc::new(key),
                primary_domain: "example.com".to_string(),
                dkim_selector: "dkim".to_string(),
                registered_mailboxes: mboxes,
                transport,
                data_dir: tmp.path().to_path_buf(),
            });

            let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
            let handle = tokio::spawn(async move {
                run_send_listener(listener, ctx, shutdown_rx).await;
            });

            // Build a minimal RFC 5322 body.
            let body = b"From: alice@example.com\r\n\
                         To: user@gmail.com\r\n\
                         Subject: Hi\r\n\
                         Date: Thu, 01 Jan 2025 12:00:00 +0000\r\n\
                         Message-ID: <abc@example.com>\r\n\
                         \r\n\
                         hello\r\n";
            let req = crate::send_protocol::SendRequest {
                from_mailbox: "alice".to_string(),
                body: body.to_vec(),
            };

            let mut client = tokio::net::UnixStream::connect(&sock).await.unwrap();
            let (mut r, mut w) = client.split();
            crate::send_protocol::write_request(&mut w, &req)
                .await
                .unwrap();
            use tokio::io::AsyncReadExt;
            let mut resp = String::new();
            r.read_to_string(&mut resp).await.unwrap();
            assert!(
                resp.starts_with("AIMX/1 OK <abc@example.com>"),
                "unexpected response: {resp:?}"
            );

            // Verify the transport received a DKIM-signed message. Clone
            // the captured bytes out of the guard so we can drop the lock
            // before any further `.await` (clippy::await-holding-lock).
            let signed_bytes = {
                let captured = captor.captured.lock().unwrap();
                assert_eq!(captured.len(), 1);
                captured[0].clone()
            };
            let signed = String::from_utf8_lossy(&signed_bytes);
            assert!(signed.starts_with("DKIM-Signature:"));
            assert!(signed.contains("d=example.com"));
            assert!(signed.contains("s=dkim"));

            // Cryptographically verify the DKIM signature using our test
            // public key. We feed the key to `mail-auth` through an
            // in-memory TXT cache so no DNS lookup is performed.
            verify_dkim_with_pubkey(&signed_bytes, &pub_pem, "example.com", "dkim").await;

            shutdown_tx.send(true).unwrap();
            let _ = tokio::time::timeout(std::time::Duration::from_secs(5), handle).await;
        });
    }

    #[cfg(unix)]
    #[test]
    fn uds_slow_loris_times_out() {
        use std::collections::HashSet;

        let tmp = tempfile::TempDir::new().unwrap();
        let sock = tmp.path().join("send.sock");

        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let listener = bind_send_socket(&sock).unwrap();

            let dkim_tmp = tempfile::TempDir::new().unwrap();
            crate::dkim::generate_keypair(dkim_tmp.path(), false).unwrap();
            let key = crate::dkim::load_private_key(dkim_tmp.path()).unwrap();
            let mut mboxes = HashSet::new();
            mboxes.insert("catchall".to_string());
            let transport: Arc<dyn MailTransport + Send + Sync> = Arc::new(NoopTransport);
            let ctx = Arc::new(crate::send_handler::SendContext {
                dkim_key: Arc::new(key),
                primary_domain: "example.com".to_string(),
                dkim_selector: "dkim".to_string(),
                registered_mailboxes: mboxes,
                transport,
                data_dir: tmp.path().to_path_buf(),
            });

            // Accept one connection and handle it with a 1-second timeout.
            let accept_handle = {
                let ctx = Arc::clone(&ctx);
                tokio::spawn(async move {
                    let (stream, _) = listener.accept().await.unwrap();
                    handle_send_connection_with_timeout(
                        stream,
                        ctx,
                        std::time::Duration::from_secs(1),
                    )
                    .await;
                })
            };

            // Connect and send only the request line but then stall.
            let mut client = tokio::net::UnixStream::connect(&sock).await.unwrap();
            use tokio::io::AsyncWriteExt;
            client
                .write_all(b"AIMX/1 SEND\nFrom-Mailbox: catchall\n")
                .await
                .unwrap();
            // Don't send Content-Length or body — stall.

            // The handler should drop the connection within ~1s.
            let result =
                tokio::time::timeout(std::time::Duration::from_secs(5), accept_handle).await;
            assert!(
                result.is_ok(),
                "handler should complete within 5s (1s timeout + margin)"
            );

            // The client should see a disconnect (read returns 0 bytes).
            use tokio::io::AsyncReadExt;
            let mut buf = [0u8; 64];
            let n = client.read(&mut buf).await.unwrap_or(0);
            assert_eq!(n, 0, "server should have closed the connection");
        });
    }

    /// Crypto-verify a DKIM-signed message against a specific public key by
    /// populating a `mail-auth` TXT cache with a synthetic DKIM1 record and
    /// running the verifier. Panics on any verification failure — used by
    /// the S34-3 integration test.
    async fn verify_dkim_with_pubkey(signed: &[u8], pub_pem: &str, domain: &str, selector: &str) {
        use base64::Engine;
        use mail_auth::{
            AuthenticatedMessage, DkimResult, MessageAuthenticator, Parameters, ResolverCache, Txt,
            common::{parse::TxtRecordParser, verify::DomainKey},
        };
        use rsa::{RsaPublicKey, pkcs8::DecodePublicKey, pkcs8::EncodePublicKey};
        use std::borrow::Borrow;
        use std::collections::HashMap;
        use std::hash::Hash;
        use std::sync::Mutex;
        use std::time::Instant;

        // Convert the stored public PEM into a DKIM1 TXT-record string.
        let pk = RsaPublicKey::from_public_key_pem(pub_pem).expect("parse public PEM");
        let spki_der = pk.to_public_key_der().expect("encode SPKI");
        let b64 = base64::engine::general_purpose::STANDARD.encode(spki_der.as_ref());
        let txt_record = format!("v=DKIM1; k=rsa; p={b64}");

        // Build a cache that returns the DomainKey for the selector + domain.
        struct InMemCache<K, V>(Mutex<HashMap<K, V>>);
        impl<K: Hash + Eq, V: Clone> ResolverCache<K, V> for InMemCache<K, V> {
            fn get<Q>(&self, key: &Q) -> Option<V>
            where
                K: Borrow<Q>,
                Q: Hash + Eq + ?Sized,
            {
                self.0.lock().unwrap().get(key).cloned()
            }
            fn remove<Q>(&self, key: &Q) -> Option<V>
            where
                K: Borrow<Q>,
                Q: Hash + Eq + ?Sized,
            {
                self.0.lock().unwrap().remove(key)
            }
            fn insert(&self, key: K, value: V, _: Instant) {
                self.0.lock().unwrap().insert(key, value);
            }
        }

        let dk = DomainKey::parse(txt_record.as_bytes()).expect("parse DKIM1 record");
        let txt_cache: InMemCache<String, Txt> = InMemCache(Mutex::new(HashMap::new()));
        // mail-auth's `IntoFqdn` normalizes to `<selector>._domainkey.<domain>.`
        // with a trailing dot — match it.
        let key = format!("{selector}._domainkey.{domain}.");
        txt_cache.0.lock().unwrap().insert(
            key,
            Txt::DomainKey(std::sync::Arc::new(dk)),
            // valid_until is unused by the Insert impl; any Instant works.
        );

        let authenticator = MessageAuthenticator::new_system_conf()
            .or_else(|_| MessageAuthenticator::new_cloudflare())
            .expect("build MessageAuthenticator (DNS-independent here; cache short-circuits)");

        let auth_msg = AuthenticatedMessage::parse(signed)
            .expect("parse AuthenticatedMessage from signed bytes");
        assert!(
            !auth_msg.dkim_headers.is_empty(),
            "signed message must carry at least one DKIM-Signature header"
        );

        let params = Parameters::new(&auth_msg).with_txt_cache(&txt_cache);
        let outputs = authenticator.verify_dkim(params).await;
        assert!(
            !outputs.is_empty(),
            "verifier returned no outputs; header parse probably failed"
        );
        let pass = outputs
            .iter()
            .any(|o| matches!(o.result(), DkimResult::Pass));
        assert!(
            pass,
            "DKIM signature failed to verify against the test public key; outputs: {:?}",
            outputs
                .iter()
                .map(|o| o.result().clone())
                .collect::<Vec<_>>()
        );
    }
}
