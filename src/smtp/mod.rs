mod session;
#[cfg(test)]
mod tests;
mod tls;

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::sync::Semaphore;

#[cfg(test)]
use crate::config::Config;
use crate::config::ConfigHandle;
use crate::mailbox_locks::MailboxLocks;

pub const DEFAULT_MAX_MESSAGE_SIZE: usize = 25 * 1024 * 1024; // 25 MB
pub const DEFAULT_IDLE_TIMEOUT: Duration = Duration::from_secs(300); // 5 min
pub const DEFAULT_TOTAL_TIMEOUT: Duration = Duration::from_secs(600); // 10 min
pub const DEFAULT_MAX_CONNECTIONS: usize = 100;
pub const DEFAULT_MAX_COMMANDS_BEFORE_DATA: usize = 50;

pub struct SmtpServer {
    /// The daemon's in-memory `Config` is live-swappable. Each inbound
    /// SMTP session resolves routing against the current snapshot via
    /// `config_handle.load()`, so a `MAILBOX-CREATE` over UDS is visible
    /// on the next RCPT TO without a restart.
    config_handle: ConfigHandle,
    tls_acceptor: Option<Arc<tokio_rustls::TlsAcceptor>>,
    max_message_size: usize,
    idle_timeout: Duration,
    total_timeout: Duration,
    max_connections: usize,
    max_commands_before_data: usize,
    /// Shared with `StateContext` and `MailboxContext` so inbound ingest,
    /// MARK-*, and MAILBOX-* all serialize on the same per-mailbox
    /// `tokio::sync::Mutex<()>`. Defaults to a freshly-constructed map
    /// for tests that only exercise the SMTP path; `aimx serve` calls
    /// `with_mailbox_locks` to inject the daemon-wide map.
    mailbox_locks: Arc<MailboxLocks>,
}

impl SmtpServer {
    /// Test-only constructor that wraps a freshly-built `ConfigHandle`
    /// around `config`. Production always owns the handle outside the
    /// server so `aimx serve` can share one `Config` across the SMTP
    /// listener, the send handler, the state handler, and the mailbox
    /// handler.
    #[cfg(test)]
    pub fn new(config: Config) -> Self {
        Self::with_handle(ConfigHandle::new(config))
    }

    pub fn with_handle(config_handle: ConfigHandle) -> Self {
        Self {
            config_handle,
            tls_acceptor: None,
            max_message_size: DEFAULT_MAX_MESSAGE_SIZE,
            idle_timeout: DEFAULT_IDLE_TIMEOUT,
            total_timeout: DEFAULT_TOTAL_TIMEOUT,
            max_connections: DEFAULT_MAX_CONNECTIONS,
            max_commands_before_data: DEFAULT_MAX_COMMANDS_BEFORE_DATA,
            mailbox_locks: Arc::new(MailboxLocks::new()),
        }
    }

    /// Share an existing `MailboxLocks` map with this SMTP server.
    /// `aimx serve` calls this so inbound ingest, MARK-*, and MAILBOX-*
    /// all take the same per-mailbox lock.
    pub fn with_mailbox_locks(mut self, locks: Arc<MailboxLocks>) -> Self {
        self.mailbox_locks = locks;
        self
    }

    pub fn with_tls(
        mut self,
        cert_path: &std::path::Path,
        key_path: &std::path::Path,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let acceptor = tls::build_tls_acceptor(cert_path, key_path)?;
        self.tls_acceptor = Some(Arc::new(acceptor));
        Ok(self)
    }

    #[cfg(test)]
    pub fn with_max_message_size(mut self, size: usize) -> Self {
        self.max_message_size = size;
        self
    }

    #[cfg(test)]
    pub fn with_max_connections(mut self, max: usize) -> Self {
        self.max_connections = max;
        self
    }

    #[cfg(test)]
    pub fn with_timeouts(mut self, idle: Duration, total: Duration) -> Self {
        self.idle_timeout = idle;
        self.total_timeout = total;
        self
    }

    #[cfg(test)]
    pub fn with_max_commands_before_data(mut self, max: usize) -> Self {
        self.max_commands_before_data = max;
        self
    }

    pub async fn run(
        &self,
        listener: TcpListener,
        mut shutdown: tokio::sync::watch::Receiver<bool>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let semaphore = Arc::new(Semaphore::new(self.max_connections));
        // `hostname` is derived from the live handle and refreshed
        // per-accept so if the operator ever hot-swaps the domain (not
        // supported in v0.2, but the plumbing is cheap) we pick it up.

        loop {
            tokio::select! {
                result = listener.accept() => {
                    let (stream, peer_addr) = match result {
                        Ok(v) => v,
                        Err(e) => {
                            eprintln!("Accept error: {e}");
                            continue;
                        }
                    };

                    let permit = match semaphore.clone().try_acquire_owned() {
                        Ok(p) => p,
                        Err(_) => {
                            Self::reject_connection(stream, peer_addr).await;
                            continue;
                        }
                    };

                    let config_handle = self.config_handle.clone();
                    let tls_acceptor = self.tls_acceptor.clone();
                    // Re-read the hostname from the current snapshot so it
                    // tracks any live Config swap.
                    let hostname = config_handle.load().domain.clone();
                    let max_message_size = self.max_message_size;
                    let idle_timeout = self.idle_timeout;
                    let total_timeout = self.total_timeout;
                    let max_commands = self.max_commands_before_data;

                    let mailbox_locks = Arc::clone(&self.mailbox_locks);
                    tokio::spawn(async move {
                        let _permit = permit;
                        let params = session::SessionParams {
                            config_handle,
                            tls_acceptor,
                            hostname,
                            peer_addr,
                            max_message_size,
                            idle_timeout,
                            total_timeout,
                            max_commands_before_data: max_commands,
                            mailbox_locks,
                        };
                        let session = session::SmtpSession::new(params);
                        if let Err(e) = session.handle(stream).await {
                            eprintln!("[{peer_addr}] Session error: {e}");
                        }
                    });
                }
                _ = shutdown.changed() => {
                    break;
                }
            }
        }

        drop(listener);

        let in_flight = self.max_connections - semaphore.available_permits();
        eprintln!("aimx SMTP listener shutting down ({in_flight} connections in-flight)");

        let grace_start = tokio::time::Instant::now();
        let grace_period = Duration::from_secs(30);
        while semaphore.available_permits() < self.max_connections {
            if grace_start.elapsed() >= grace_period {
                let remaining = self.max_connections - semaphore.available_permits();
                eprintln!("Grace period expired, forcefully closing {remaining} connections");
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }

        Ok(())
    }

    async fn reject_connection(stream: tokio::net::TcpStream, addr: SocketAddr) {
        use tokio::io::AsyncWriteExt;
        let mut stream = stream;
        let _ = stream
            .write_all(b"421 Too many connections, try again later\r\n")
            .await;
        let _ = stream.shutdown().await;
        eprintln!("[{addr}] Rejected: connection limit reached");
    }
}
