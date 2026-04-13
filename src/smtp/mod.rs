mod session;
#[cfg(test)]
mod tests;
mod tls;

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::sync::Semaphore;

use crate::config::Config;

pub const DEFAULT_MAX_MESSAGE_SIZE: usize = 25 * 1024 * 1024; // 25 MB
pub const DEFAULT_IDLE_TIMEOUT: Duration = Duration::from_secs(300); // 5 min
pub const DEFAULT_TOTAL_TIMEOUT: Duration = Duration::from_secs(600); // 10 min
pub const DEFAULT_MAX_CONNECTIONS: usize = 100;
pub const DEFAULT_MAX_COMMANDS_BEFORE_DATA: usize = 50;

pub struct SmtpServer {
    config: Arc<Config>,
    tls_acceptor: Option<Arc<tokio_rustls::TlsAcceptor>>,
    max_message_size: usize,
    idle_timeout: Duration,
    total_timeout: Duration,
    max_connections: usize,
    max_commands_before_data: usize,
}

impl SmtpServer {
    pub fn new(config: Config) -> Self {
        Self {
            config: Arc::new(config),
            tls_acceptor: None,
            max_message_size: DEFAULT_MAX_MESSAGE_SIZE,
            idle_timeout: DEFAULT_IDLE_TIMEOUT,
            total_timeout: DEFAULT_TOTAL_TIMEOUT,
            max_connections: DEFAULT_MAX_CONNECTIONS,
            max_commands_before_data: DEFAULT_MAX_COMMANDS_BEFORE_DATA,
        }
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
        let hostname = self.config.domain.clone();

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

                    let config = Arc::clone(&self.config);
                    let tls_acceptor = self.tls_acceptor.clone();
                    let hostname = hostname.clone();
                    let max_message_size = self.max_message_size;
                    let idle_timeout = self.idle_timeout;
                    let total_timeout = self.total_timeout;
                    let max_commands = self.max_commands_before_data;

                    tokio::spawn(async move {
                        let _permit = permit;
                        let params = session::SessionParams {
                            config,
                            tls_acceptor,
                            hostname,
                            peer_addr,
                            max_message_size,
                            idle_timeout,
                            total_timeout,
                            max_commands_before_data: max_commands,
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
