use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::config::{Config, ConfigHandle};
use crate::dkim;
use crate::mailbox_handler::MailboxContext;
use crate::send_handler::SendContext;
use crate::send_protocol;
use crate::smtp::SmtpServer;
use crate::state_handler::StateContext;
use crate::term;
use crate::transport::{FileDropTransport, LettreTransport, MailTransport};

/// Resolve DKIM TXT records for the startup sanity check (S44-2). Separated
/// into a trait so tests can inject a resolver that returns a mismatched
/// `p=` value without reaching real DNS.
pub trait DkimTxtResolver {
    fn resolve_dkim_txt(
        &self,
        fqdn: &str,
    ) -> Result<Vec<String>, Box<dyn std::error::Error + Send + Sync>>;
}

/// Production implementation backed by `hickory-resolver`. Creates the
/// resolver inside each call — setup is cheap and the check runs once at
/// startup. Keeps the trait object trivially constructible.
pub struct HickoryDkimResolver;

impl DkimTxtResolver for HickoryDkimResolver {
    fn resolve_dkim_txt(
        &self,
        fqdn: &str,
    ) -> Result<Vec<String>, Box<dyn std::error::Error + Send + Sync>> {
        // S47-1: `block_in_place` + `Handle::current().block_on(...)` only
        // works from a multi-threaded tokio runtime. `aimx serve` always
        // uses the multi-thread flavour via `tokio::runtime::Runtime::new()`,
        // but a future caller that switches to the current-thread flavour
        // would hit a runtime panic on the first DKIM TXT lookup. Debug-
        // assert the flavour at entry so the coupling is documented and
        // caught in test builds rather than at startup in production.
        use tokio::runtime::RuntimeFlavor;
        debug_assert!(
            matches!(
                tokio::runtime::Handle::current().runtime_flavor(),
                RuntimeFlavor::MultiThread
            ),
            "HickoryDkimResolver::resolve_dkim_txt requires a multi-thread tokio runtime \
             because it uses `block_in_place` + `Handle::block_on`. `aimx serve` meets this \
             contract; if you're calling it from a current-thread runtime, async-ify the \
             trait instead."
        );

        // S47-1 (test-only): `AIMX_TEST_DKIM_RESOLVER_OVERRIDE` lets the
        // integration test spin a real `aimx serve` against a canned
        // resolver result without touching DNS. Format:
        //   `"ok:<txt1>||<txt2>"`  — resolver returns Ok(vec![...])
        //   `"err:<message>"`       — resolver returns Err
        //   `"no-record"`           — resolver returns Ok(vec![]) (empty)
        // Gated by the env var so production binaries never short-circuit.
        if let Some(override_val) = std::env::var_os("AIMX_TEST_DKIM_RESOLVER_OVERRIDE") {
            let s = override_val.to_string_lossy().into_owned();
            if let Some(err_msg) = s.strip_prefix("err:") {
                return Err(err_msg.to_string().into());
            }
            if s == "no-record" {
                return Ok(Vec::new());
            }
            if let Some(rest) = s.strip_prefix("ok:") {
                let records: Vec<String> = rest.split("||").map(|s| s.to_string()).collect();
                return Ok(records);
            }
            // Unrecognized format — let the real resolver run.
        }

        // Synchronous helper: the startup check runs inside the tokio
        // runtime but treats the DNS lookup as a one-shot with a short
        // deadline. Build an inline resolver rather than reusing the mx
        // helper so we can keep the hot path (MX lookups during outbound
        // delivery) isolated.
        use hickory_resolver::TokioResolver;
        let handle = tokio::runtime::Handle::current();
        let result: Result<Vec<String>, Box<dyn std::error::Error + Send + Sync>> =
            tokio::task::block_in_place(|| {
                handle.block_on(async move {
                    let resolver = TokioResolver::builder_tokio()
                        .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> {
                            format!("failed to create DNS resolver: {e}").into()
                        })?
                        .build();
                    let lookup = resolver.txt_lookup(fqdn).await.map_err(
                        |e| -> Box<dyn std::error::Error + Send + Sync> {
                            format!("TXT lookup failed for {fqdn}: {e}").into()
                        },
                    )?;
                    let mut records = Vec::new();
                    for txt in lookup.iter() {
                        // A TXT record may be split across multiple strings
                        // by resolvers — join them so the `p=` value parses
                        // cleanly after whitespace stripping.
                        let joined: String = txt
                            .iter()
                            .map(|b| String::from_utf8_lossy(b).into_owned())
                            .collect::<Vec<_>>()
                            .join("");
                        records.push(joined);
                    }
                    Ok(records)
                })
            });
        result
    }
}

/// Outcome of the startup DKIM DNS sanity check. Used by tests to assert
/// which branch the daemon took without having to parse stderr.
#[derive(Debug, PartialEq, Eq)]
pub enum DkimStartupCheck {
    /// DNS `p=` matches the on-disk public key.
    Match,
    /// DNS `p=` differs from the on-disk key — receivers will see DKIM fail.
    Mismatch { dns: String, local: String },
    /// No `dkim._domainkey.<domain>` TXT record present.
    NoRecord,
    /// TXT record exists but has no `p=` tag.
    NoPTag,
    /// DNS resolution itself failed (NXDOMAIN, timeout, etc.). Non-fatal —
    /// may be a transient propagation issue after a fresh setup.
    ResolveError(String),
}

/// Pure logic: compare the on-disk SPKI base64 against one or more TXT
/// records. Extracted so it's unit-testable without a DNS roundtrip.
pub fn evaluate_dkim_startup(local_spki_b64: &str, txt_records: &[String]) -> DkimStartupCheck {
    let dkim_records: Vec<&String> = txt_records
        .iter()
        .filter(|r| r.contains("v=DKIM1"))
        .collect();
    if dkim_records.is_empty() {
        return DkimStartupCheck::NoRecord;
    }
    let mut seen_dns: Option<String> = None;
    for r in &dkim_records {
        if let Some(dns_p) = dkim::extract_dkim_p_value(r) {
            if dns_p == local_spki_b64 {
                return DkimStartupCheck::Match;
            }
            seen_dns = Some(dns_p);
        }
    }
    match seen_dns {
        Some(dns) => DkimStartupCheck::Mismatch {
            dns,
            local: local_spki_b64.to_string(),
        },
        None => DkimStartupCheck::NoPTag,
    }
}

/// Run the startup DKIM DNS sanity check against `resolver`. Never blocks
/// startup — every outcome is returned to the caller which logs an
/// appropriate message and continues binding listeners.
///
/// This closes finding #10 from the 2026-04-17 manual test run: the on-disk
/// private key and the DNS-published public key had drifted, every outbound
/// signature silently failed at Gmail, and the running daemon had no
/// visibility into the mismatch.
pub fn run_dkim_startup_check(
    resolver: &dyn DkimTxtResolver,
    domain: &str,
    selector: &str,
    dkim_root: &Path,
) -> DkimStartupCheck {
    let local_b64 = match dkim::public_key_spki_base64(dkim_root) {
        Ok(b) => b,
        Err(e) => return DkimStartupCheck::ResolveError(format!("local key error: {e}")),
    };
    let fqdn = format!("{selector}._domainkey.{domain}");
    match resolver.resolve_dkim_txt(&fqdn) {
        Ok(records) => evaluate_dkim_startup(&local_b64, &records),
        Err(e) => DkimStartupCheck::ResolveError(e.to_string()),
    }
}

/// Render and emit the warning for a non-Match outcome. On `Match` this
/// emits nothing. Kept separate from the evaluator so the formatting can be
/// adjusted without touching the decision logic.
pub fn log_dkim_startup_check(outcome: &DkimStartupCheck, domain: &str, selector: &str) {
    let fqdn = format!("{selector}._domainkey.{domain}");
    match outcome {
        DkimStartupCheck::Match => {}
        DkimStartupCheck::Mismatch { dns, local } => {
            // Loud multi-line warning — this silent-fail mode cost a full
            // round of manual test time on 2026-04-17.
            eprintln!(
                "{} DKIM key mismatch between on-disk private key and DNS-published public key",
                term::error("ERROR:")
            );
            eprintln!(
                "  DNS record at {fqdn} advertises a different p= value than the \
                 local public.key."
            );
            eprintln!("  Outbound signatures will FAIL DKIM verification at receivers.");
            eprintln!("  Run `sudo aimx setup {domain}` to republish the DNS record.");
            eprintln!("  (DNS p= first 32 chars: {}…)", truncate(dns, 32));
            eprintln!("  (Local p= first 32 chars: {}…)", truncate(local, 32));
        }
        DkimStartupCheck::NoRecord => {
            eprintln!(
                "{} No DKIM TXT record found at {fqdn} — outbound mail will FAIL DKIM \
                 verification at receivers. Run `sudo aimx setup {domain}` to publish the record.",
                term::warn("Warning:")
            );
        }
        DkimStartupCheck::NoPTag => {
            eprintln!(
                "{} DKIM TXT record at {fqdn} has no non-empty p= value. Outbound mail will \
                 FAIL DKIM verification. Re-run `sudo aimx setup {domain}`.",
                term::warn("Warning:")
            );
        }
        DkimStartupCheck::ResolveError(msg) => {
            // Non-fatal: DNS may not have propagated yet, or the host may
            // be offline mid-deploy. Log at warn and move on.
            eprintln!(
                "{} DKIM DNS sanity check skipped: {msg}. \
                 Verify manually with `dig +short TXT {fqdn}` once DNS has propagated.",
                term::warn("Warning:")
            );
        }
    }
}

fn truncate(s: &str, n: usize) -> String {
    if s.len() <= n {
        s.to_string()
    } else {
        s.chars().take(n).collect()
    }
}

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

    // S44-2: compare the on-disk public key to the DNS-published `p=` value.
    // Never fatal — DNS may not have propagated yet after a fresh setup. A
    // mismatch was the silent root cause of finding #10 in the 2026-04-17
    // manual test run.
    let resolver = HickoryDkimResolver;
    let outcome =
        run_dkim_startup_check(&resolver, &config.domain, &config.dkim_selector, &dkim_root);
    log_dkim_startup_check(&outcome, &config.domain, &config.dkim_selector);

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

    // Wrap the starting Config in a live, swappable handle. Every
    // daemon-side context (send, state, mailbox, SMTP server) reads
    // through this same handle so MAILBOX-CREATE/DELETE is reflected
    // everywhere at once on a successful atomic `config.toml` write.
    let data_dir = config.data_dir.clone();
    let dkim_selector = config.dkim_selector.clone();
    let config_handle = ConfigHandle::new(config);

    let send_ctx = Arc::new(SendContext {
        dkim_key,
        dkim_selector,
        config_handle: config_handle.clone(),
        transport,
        data_dir: data_dir.clone(),
    });

    // A single `MailboxLocks` map is shared across every writer (inbound
    // ingest, MARK-*, MAILBOX-*) so they all serialize on the same
    // per-mailbox `tokio::sync::Mutex<()>`. See `crate::mailbox_locks`
    // for the lock hierarchy.
    let mailbox_locks = Arc::new(crate::mailbox_locks::MailboxLocks::new());

    // Shared state context for MARK-READ / MARK-UNREAD verbs and the
    // per-mailbox write lock used by MAILBOX-CREATE / MAILBOX-DELETE
    // plus inbound ingest.
    let state_ctx = Arc::new(StateContext::with_locks(
        data_dir.clone(),
        config_handle.clone(),
        Arc::clone(&mailbox_locks),
    ));

    // MailboxContext owns the on-disk config.toml path + the handle it
    // writes through.
    let mb_ctx = Arc::new(MailboxContext::new(
        crate::config::config_path(),
        config_handle.clone(),
    ));

    let server = SmtpServer::with_handle(config_handle.clone())
        .with_mailbox_locks(Arc::clone(&mailbox_locks));
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
        run_send_listener(uds_listener, send_ctx, state_ctx, mb_ctx, uds_shutdown).await;
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
    send_ctx: Arc<SendContext>,
    state_ctx: Arc<StateContext>,
    mb_ctx: Arc<MailboxContext>,
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
                        let send_ctx = Arc::clone(&send_ctx);
                        let state_ctx = Arc::clone(&state_ctx);
                        let mb_ctx = Arc::clone(&mb_ctx);
                        tokio::spawn(async move {
                            handle_uds_connection(stream, send_ctx, state_ctx, mb_ctx).await;
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
/// entire `AIMX/1` request frame within this window or the connection is
/// dropped. Prevents slow-loris abuse on the world-writable socket.
const UDS_REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

async fn handle_uds_connection(
    stream: tokio::net::UnixStream,
    send_ctx: Arc<SendContext>,
    state_ctx: Arc<StateContext>,
    mb_ctx: Arc<MailboxContext>,
) {
    handle_uds_connection_with_timeout(stream, send_ctx, state_ctx, mb_ctx, UDS_REQUEST_TIMEOUT)
        .await;
}

/// One-frame-per-connection dispatcher. Reads a single `AIMX/1` request
/// (SEND, MARK-READ, MARK-UNREAD, MAILBOX-CREATE, MAILBOX-DELETE) within
/// `timeout`, runs the matching handler, and writes the framed response.
/// The same slow-loris defence and parse-failure drain logic applies to
/// every verb.
async fn handle_uds_connection_with_timeout(
    stream: tokio::net::UnixStream,
    send_ctx: Arc<SendContext>,
    state_ctx: Arc<StateContext>,
    mb_ctx: Arc<MailboxContext>,
    timeout: std::time::Duration,
) {
    use send_protocol::{AckResponse, ErrCode, ParseError, Request, SendResponse};

    #[allow(clippy::large_enum_variant)]
    enum Reply {
        Send(SendResponse),
        Ack(AckResponse),
    }

    let (mut reader, mut writer) = stream.into_split();
    let (reply, parse_failed) =
        match tokio::time::timeout(timeout, send_protocol::parse_request(&mut reader)).await {
            Ok(Ok(Request::Send(req))) => (
                Reply::Send(crate::send_handler::handle_send(req, &send_ctx).await),
                false,
            ),
            Ok(Ok(Request::Mark(req))) => (
                Reply::Ack(crate::state_handler::handle_mark(&state_ctx, &req).await),
                false,
            ),
            Ok(Ok(Request::MailboxCrud(req))) => (
                Reply::Ack(
                    crate::mailbox_handler::handle_mailbox_crud(&state_ctx, &mb_ctx, &req).await,
                ),
                false,
            ),
            Ok(Err(ParseError::ClosedBeforeRequest)) => {
                return;
            }
            Ok(Err(ParseError::UnknownVerb(v))) => (
                Reply::Ack(AckResponse::Err {
                    code: ErrCode::Protocol,
                    reason: format!("unknown verb '{v}'"),
                }),
                true,
            ),
            Ok(Err(e)) => (
                Reply::Ack(AckResponse::Err {
                    code: ErrCode::Malformed,
                    reason: e.to_string(),
                }),
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

    let write_result = match reply {
        Reply::Send(r) => send_protocol::write_response(&mut writer, &r).await,
        Reply::Ack(r) => send_protocol::write_ack_response(&mut writer, &r).await,
    };
    if let Err(e) = write_result {
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
        // stop. The UDS send socket landing inside is world-writable —
        // authorization is out of scope in v0.2.
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
            "v0.2 does not use an `aimx` group — the systemd unit must \
             not declare Group=aimx. The UDS send socket is \
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
             root:root (no aimx group is used): {script}"
        );
        assert!(
            !script.contains("command_user=\"root:aimx\""),
            "OpenRC script must not declare command_user with an aimx group"
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
                    trust: None,
                    trusted_senders: None,
                },
            );
            let config = crate::config::Config {
                domain: "test.local".to_string(),
                data_dir: tmp.path().to_path_buf(),
                dkim_selector: "dkim".to_string(),
                trust: "none".to_string(),
                trusted_senders: vec![],
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

    // ----- S44-2 DKIM startup DNS sanity check ------------------------

    struct FakeDkimResolver {
        result: Result<Vec<String>, String>,
    }

    impl DkimTxtResolver for FakeDkimResolver {
        fn resolve_dkim_txt(
            &self,
            _fqdn: &str,
        ) -> Result<Vec<String>, Box<dyn std::error::Error + Send + Sync>> {
            match &self.result {
                Ok(v) => Ok(v.clone()),
                Err(e) => Err(e.clone().into()),
            }
        }
    }

    #[test]
    fn evaluate_dkim_startup_match() {
        let local = "AAABBBCCC";
        let record = "v=DKIM1; k=rsa; p=AAABBBCCC".to_string();
        assert_eq!(
            evaluate_dkim_startup(local, &[record]),
            DkimStartupCheck::Match
        );
    }

    #[test]
    fn evaluate_dkim_startup_mismatch() {
        let local = "LOCALKEY";
        let record = "v=DKIM1; k=rsa; p=DNSKEY".to_string();
        match evaluate_dkim_startup(local, &[record]) {
            DkimStartupCheck::Mismatch { dns, local: l } => {
                assert_eq!(dns, "DNSKEY");
                assert_eq!(l, "LOCALKEY");
            }
            other => panic!("expected Mismatch, got {other:?}"),
        }
    }

    #[test]
    fn evaluate_dkim_startup_no_record() {
        let local = "whatever";
        // Record present but not a DKIM1 record (e.g. unrelated TXT)
        let record = "v=spf1 -all".to_string();
        assert_eq!(
            evaluate_dkim_startup(local, &[record]),
            DkimStartupCheck::NoRecord
        );
    }

    #[test]
    fn evaluate_dkim_startup_no_p_tag() {
        let local = "whatever";
        let record = "v=DKIM1; k=rsa".to_string();
        assert_eq!(
            evaluate_dkim_startup(local, &[record]),
            DkimStartupCheck::NoPTag
        );
    }

    #[test]
    fn evaluate_dkim_startup_matches_second_record_when_first_mismatches() {
        // Split DKIM (key rotation in flight) — one matches, one does not.
        let local = "GOODKEY";
        let records = vec![
            "v=DKIM1; k=rsa; p=OLDKEY".to_string(),
            "v=DKIM1; k=rsa; p=GOODKEY".to_string(),
        ];
        assert_eq!(
            evaluate_dkim_startup(local, &records),
            DkimStartupCheck::Match
        );
    }

    #[test]
    fn evaluate_dkim_startup_strips_whitespace_in_p_value() {
        let local = "ABCDEFGHI";
        // Mimic a DNS resolver that left internal whitespace in the joined
        // string (sometimes happens with multi-string TXT records).
        let record = "v=DKIM1; k=rsa; p=ABC DEF GHI".to_string();
        assert_eq!(
            evaluate_dkim_startup(local, &[record]),
            DkimStartupCheck::Match
        );
    }

    #[test]
    fn run_dkim_startup_check_resolve_error_when_dns_fails() {
        let tmp = tempfile::TempDir::new().unwrap();
        crate::dkim::generate_keypair(tmp.path(), false).unwrap();
        let resolver = FakeDkimResolver {
            result: Err("simulated NXDOMAIN".into()),
        };
        match run_dkim_startup_check(&resolver, "example.com", "dkim", tmp.path()) {
            DkimStartupCheck::ResolveError(msg) => {
                assert!(msg.contains("NXDOMAIN"), "got: {msg}");
            }
            other => panic!("expected ResolveError, got {other:?}"),
        }
    }

    #[test]
    fn run_dkim_startup_check_match_against_own_key() {
        // End-to-end: generate a real key, assemble its own DKIM TXT record,
        // feed that through the fake resolver — the evaluator must say Match.
        let tmp = tempfile::TempDir::new().unwrap();
        crate::dkim::generate_keypair(tmp.path(), false).unwrap();
        let record = crate::dkim::dns_record_value(tmp.path()).unwrap();
        let resolver = FakeDkimResolver {
            result: Ok(vec![record]),
        };
        assert_eq!(
            run_dkim_startup_check(&resolver, "example.com", "dkim", tmp.path()),
            DkimStartupCheck::Match
        );
    }

    #[test]
    fn run_dkim_startup_check_mismatch_logs_do_not_panic() {
        let tmp = tempfile::TempDir::new().unwrap();
        crate::dkim::generate_keypair(tmp.path(), false).unwrap();
        let resolver = FakeDkimResolver {
            result: Ok(vec!["v=DKIM1; k=rsa; p=SOMEOTHERKEY".to_string()]),
        };
        let outcome = run_dkim_startup_check(&resolver, "example.com", "dkim", tmp.path());
        assert!(matches!(outcome, DkimStartupCheck::Mismatch { .. }));
        // Smoke: the logging path does not panic on a long base64 payload.
        log_dkim_startup_check(&outcome, "example.com", "dkim");
    }

    #[test]
    fn run_dkim_startup_check_no_record_when_unrelated_txt_present() {
        let tmp = tempfile::TempDir::new().unwrap();
        crate::dkim::generate_keypair(tmp.path(), false).unwrap();
        let resolver = FakeDkimResolver {
            result: Ok(vec!["v=spf1 -all".to_string()]),
        };
        assert_eq!(
            run_dkim_startup_check(&resolver, "example.com", "dkim", tmp.path()),
            DkimStartupCheck::NoRecord
        );
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
    fn build_test_config(data_dir: &Path) -> crate::config::Config {
        use std::collections::HashMap;
        let mut mailboxes = HashMap::new();
        mailboxes.insert(
            "catchall".to_string(),
            crate::config::MailboxConfig {
                address: "*@example.com".to_string(),
                on_receive: vec![],
                trust: None,
                trusted_senders: None,
            },
        );
        mailboxes.insert(
            "alice".to_string(),
            crate::config::MailboxConfig {
                address: "alice@example.com".to_string(),
                on_receive: vec![],
                trust: None,
                trusted_senders: None,
            },
        );
        crate::config::Config {
            domain: "example.com".to_string(),
            data_dir: data_dir.to_path_buf(),
            dkim_selector: "dkim".to_string(),
            trust: "none".to_string(),
            trusted_senders: vec![],
            mailboxes,
            verify_host: None,
            enable_ipv6: false,
        }
    }

    #[cfg(unix)]
    fn build_test_send_ctx_with_handle(
        data_dir: &Path,
        handle: ConfigHandle,
    ) -> Arc<crate::send_handler::SendContext> {
        let dkim_tmp = tempfile::TempDir::new().unwrap();
        crate::dkim::generate_keypair(dkim_tmp.path(), false).unwrap();
        let key = crate::dkim::load_private_key(dkim_tmp.path()).unwrap();
        let transport: Arc<dyn MailTransport + Send + Sync> = Arc::new(NoopTransport);
        Arc::new(crate::send_handler::SendContext {
            dkim_key: Arc::new(key),
            dkim_selector: "dkim".to_string(),
            config_handle: handle,
            transport,
            data_dir: data_dir.to_path_buf(),
        })
    }

    #[cfg(unix)]
    fn build_test_state_ctx_with_handle(
        data_dir: &Path,
        handle: ConfigHandle,
    ) -> Arc<StateContext> {
        Arc::new(StateContext::new(data_dir.to_path_buf(), handle))
    }

    #[cfg(unix)]
    fn build_test_mailbox_ctx(
        config_path: std::path::PathBuf,
        handle: ConfigHandle,
    ) -> Arc<MailboxContext> {
        Arc::new(MailboxContext::new(config_path, handle))
    }

    #[cfg(unix)]
    #[test]
    fn uds_accept_reads_peer_credentials() {
        let tmp = tempfile::TempDir::new().unwrap();
        let sock = tmp.path().join("send.sock");

        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let listener = bind_send_socket(&sock).unwrap();

            let handle_cfg = ConfigHandle::new(build_test_config(tmp.path()));
            let send_ctx = build_test_send_ctx_with_handle(tmp.path(), handle_cfg.clone());
            let state_ctx = build_test_state_ctx_with_handle(tmp.path(), handle_cfg.clone());
            let mb_ctx = build_test_mailbox_ctx(tmp.path().join("config.toml"), handle_cfg.clone());

            let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
            let handle = tokio::spawn(async move {
                run_send_listener(listener, send_ctx, state_ctx, mb_ctx, shutdown_rx).await;
            });

            // Connect from the same process.
            let mut client = tokio::net::UnixStream::connect(&sock).await.unwrap();
            let cred = client
                .peer_cred()
                .expect("peer_cred should succeed on local UDS");
            assert_eq!(cred.uid(), unsafe { libc::geteuid() });

            // Close without sending anything — server handler should fall
            // back to the "ClosedBeforeRequest" branch and drop cleanly.
            use tokio::io::AsyncWriteExt;
            let _ = client.shutdown().await;
            drop(client);

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
        use std::collections::HashMap;
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

            let mut mailboxes = HashMap::new();
            mailboxes.insert(
                "alice".to_string(),
                crate::config::MailboxConfig {
                    address: "alice@example.com".to_string(),
                    on_receive: vec![],
                    trust: None,
                    trusted_senders: None,
                },
            );
            let config = crate::config::Config {
                domain: "example.com".to_string(),
                data_dir: tmp.path().to_path_buf(),
                dkim_selector: "dkim".to_string(),
                trust: "none".to_string(),
                trusted_senders: vec![],
                mailboxes,
                verify_host: None,
                enable_ipv6: false,
            };
            let handle_cfg = ConfigHandle::new(config);

            let send_ctx = Arc::new(crate::send_handler::SendContext {
                dkim_key: Arc::new(key),
                dkim_selector: "dkim".to_string(),
                config_handle: handle_cfg.clone(),
                transport,
                data_dir: tmp.path().to_path_buf(),
            });

            let state_ctx = Arc::new(StateContext::new(
                tmp.path().to_path_buf(),
                handle_cfg.clone(),
            ));
            let mb_ctx = Arc::new(MailboxContext::new(
                tmp.path().join("config.toml"),
                handle_cfg.clone(),
            ));

            let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
            let handle = tokio::spawn(async move {
                run_send_listener(listener, send_ctx, state_ctx, mb_ctx, shutdown_rx).await;
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
        let tmp = tempfile::TempDir::new().unwrap();
        let sock = tmp.path().join("send.sock");

        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let listener = bind_send_socket(&sock).unwrap();

            let handle_cfg = ConfigHandle::new(build_test_config(tmp.path()));
            let send_ctx = build_test_send_ctx_with_handle(tmp.path(), handle_cfg.clone());
            let state_ctx = build_test_state_ctx_with_handle(tmp.path(), handle_cfg.clone());
            let mb_ctx = build_test_mailbox_ctx(tmp.path().join("config.toml"), handle_cfg.clone());

            // Accept one connection and handle it with a 1-second timeout.
            let accept_handle = {
                let send_ctx = Arc::clone(&send_ctx);
                let state_ctx = Arc::clone(&state_ctx);
                let mb_ctx = Arc::clone(&mb_ctx);
                tokio::spawn(async move {
                    let (stream, _) = listener.accept().await.unwrap();
                    handle_uds_connection_with_timeout(
                        stream,
                        send_ctx,
                        state_ctx,
                        mb_ctx,
                        std::time::Duration::from_secs(1),
                    )
                    .await;
                })
            };

            // Connect and send only the request line but then stall.
            let mut client = tokio::net::UnixStream::connect(&sock).await.unwrap();
            use tokio::io::AsyncWriteExt;
            client.write_all(b"AIMX/1 SEND\n").await.unwrap();
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
