use std::path::Path;

use crate::config::Config;
use crate::smtp::SmtpServer;

const DEFAULT_BIND: &str = "0.0.0.0:25";
const DEFAULT_TLS_CERT: &str = "/etc/ssl/aimx/cert.pem";
const DEFAULT_TLS_KEY: &str = "/etc/ssl/aimx/key.pem";

pub fn run(
    bind: Option<&str>,
    tls_cert: Option<&str>,
    tls_key: Option<&str>,
    data_dir: Option<&Path>,
) -> Result<(), Box<dyn std::error::Error>> {
    let config = match data_dir {
        Some(dir) => Config::load_from_data_dir(dir)?,
        None => Config::load_default()?,
    };

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
            "Warning: TLS cert/key not found at {cert_path} / {key_path}, running without STARTTLS"
        );
    }

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
    eprintln!("AIMX SMTP listener started on {actual_addr}");

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

    let in_flight_msg = server.run(listener, shutdown_rx).await;

    eprintln!("AIMX SMTP listener shut down");

    in_flight_msg
}

fn can_read_tls(cert: &std::path::Path, key: &std::path::Path) -> bool {
    std::fs::metadata(cert).is_ok_and(|m| m.is_file()) && std::fs::File::open(key).is_ok()
}

pub mod service {
    pub fn generate_systemd_unit(aimx_path: &str, data_dir: &str) -> String {
        format!(
            "[Unit]\n\
             Description=AIMX SMTP server\n\
             After=network.target\n\
             \n\
             [Service]\n\
             Type=simple\n\
             ExecStart={aimx_path} serve --data-dir {data_dir}\n\
             Restart=on-failure\n\
             RestartSec=5s\n\
             StandardOutput=journal\n\
             StandardError=journal\n\
             \n\
             [Install]\n\
             WantedBy=multi-user.target\n"
        )
    }

    pub fn generate_openrc_script(aimx_path: &str, data_dir: &str) -> String {
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
}

#[cfg(test)]
mod tests {
    use super::service::*;

    #[test]
    fn systemd_unit_contains_required_fields() {
        let unit = generate_systemd_unit("/usr/local/bin/aimx", "/var/lib/aimx");
        assert!(unit.contains("After=network.target"));
        assert!(unit.contains("ExecStart=/usr/local/bin/aimx serve --data-dir /var/lib/aimx"));
        assert!(unit.contains("Restart=on-failure"));
        assert!(unit.contains("RestartSec=5s"));
        assert!(unit.contains("StandardOutput=journal"));
        assert!(unit.contains("[Unit]"));
        assert!(unit.contains("[Service]"));
        assert!(unit.contains("[Install]"));
        assert!(unit.contains("WantedBy=multi-user.target"));
    }

    #[test]
    fn systemd_unit_custom_paths() {
        let unit = generate_systemd_unit("/opt/bin/aimx", "/data/aimx");
        assert!(unit.contains("ExecStart=/opt/bin/aimx serve --data-dir /data/aimx"));
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
}
