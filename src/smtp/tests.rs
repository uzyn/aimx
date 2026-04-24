use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;

use crate::config::{Config, MailboxConfig};
use crate::smtp::SmtpServer;

/// Return the on-disk inbox directory for a mailbox under `data_dir`.
fn inbox(data_dir: &std::path::Path, name: &str) -> PathBuf {
    data_dir.join("inbox").join(name)
}

/// Collect every `.md` file under a mailbox directory, descending into
/// bundle subdirectories (`<stem>/<stem>.md`) as well as flat files.
fn collect_md_files(mailbox_dir: &std::path::Path) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    let entries = match std::fs::read_dir(mailbox_dir) {
        Ok(e) => e,
        Err(_) => return out,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if let Some(stem) = path.file_name().and_then(|f| f.to_str()) {
                let md = path.join(format!("{stem}.md"));
                if md.exists() {
                    out.push(md);
                }
            }
        } else if path.extension().is_some_and(|ext| ext == "md") {
            out.push(path);
        }
    }
    out.sort();
    out
}

/// Return the first attachment file matching `name` anywhere inside
/// `mailbox_dir` (including bundle subdirectories). Used by tests that
/// know the attachment filename but don't know the bundle stem.
fn find_attachment(mailbox_dir: &std::path::Path, name: &str) -> Option<PathBuf> {
    let entries = std::fs::read_dir(mailbox_dir).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            let candidate = path.join(name);
            if candidate.exists() {
                return Some(candidate);
            }
        }
    }
    None
}

fn test_config(data_dir: &std::path::Path) -> Config {
    let mut mailboxes = HashMap::new();
    mailboxes.insert(
        "catchall".to_string(),
        MailboxConfig {
            address: "*@test.local".to_string(),
            owner: "aimx-catchall".to_string(),
            hooks: vec![],
            trust: None,
            trusted_senders: None,
            allow_root_catchall: false,
        },
    );
    mailboxes.insert(
        "alice".to_string(),
        MailboxConfig {
            address: "alice@test.local".to_string(),
            owner: "root".to_string(),
            hooks: vec![],
            trust: None,
            trusted_senders: None,
            allow_root_catchall: false,
        },
    );
    Config {
        domain: "test.local".to_string(),
        data_dir: data_dir.to_path_buf(),
        dkim_selector: "aimx".to_string(),
        trust: "none".to_string(),
        trusted_senders: vec![],
        hook_templates: Vec::new(),
        mailboxes,
        verify_host: None,
        enable_ipv6: false,
        upgrade: None,
    }
}

struct TestClient {
    reader: BufReader<tokio::io::ReadHalf<TcpStream>>,
    writer: tokio::io::WriteHalf<TcpStream>,
}

impl TestClient {
    async fn connect(port: u16) -> Self {
        let stream = TcpStream::connect(format!("127.0.0.1:{port}"))
            .await
            .unwrap();
        let (reader, writer) = tokio::io::split(stream);
        Self {
            reader: BufReader::new(reader),
            writer,
        }
    }

    async fn read_line(&mut self) -> String {
        let mut buf = String::new();
        tokio::time::timeout(Duration::from_secs(5), self.reader.read_line(&mut buf))
            .await
            .expect("read timeout")
            .expect("read error");
        buf
    }

    async fn read_response(&mut self) -> String {
        let mut response = String::new();
        loop {
            let line = self.read_line().await;
            let is_last = line.len() >= 4 && line.as_bytes()[3] == b' ';
            response.push_str(&line);
            if is_last || line.is_empty() {
                break;
            }
        }
        response
    }

    async fn send(&mut self, cmd: &str) {
        self.writer
            .write_all(format!("{cmd}\r\n").as_bytes())
            .await
            .unwrap();
    }

    async fn send_and_read(&mut self, cmd: &str) -> String {
        self.send(cmd).await;
        self.read_response().await
    }
}

async fn start_server(config: Config) -> (u16, tokio::sync::watch::Sender<bool>) {
    let server = SmtpServer::new(config);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);

    tokio::spawn(async move {
        server.run(listener, shutdown_rx).await.unwrap();
    });

    tokio::time::sleep(Duration::from_millis(50)).await;
    (port, shutdown_tx)
}

async fn start_server_with_size_limit(
    config: Config,
    max_size: usize,
) -> (u16, tokio::sync::watch::Sender<bool>) {
    let server = SmtpServer::new(config).with_max_message_size(max_size);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);

    tokio::spawn(async move {
        server.run(listener, shutdown_rx).await.unwrap();
    });

    tokio::time::sleep(Duration::from_millis(50)).await;
    (port, shutdown_tx)
}

fn test_email() -> &'static str {
    "From: sender@example.com\r\nTo: alice@test.local\r\nSubject: Test\r\nDate: Mon, 01 Jan 2024 00:00:00 +0000\r\nMessage-ID: <test@example.com>\r\n\r\nHello World\r\n"
}

// --- SMTP state machine tests ---

#[tokio::test]
async fn test_ehlo_response() {
    let tmp = tempfile::TempDir::new().unwrap();
    let config = test_config(tmp.path());
    let (port, _shutdown) = start_server(config).await;
    let mut client = TestClient::connect(port).await;

    let banner = client.read_line().await;
    assert!(banner.starts_with("220 "));

    let resp = client.send_and_read("EHLO client.example.com").await;
    assert!(resp.contains("250"));
    assert!(resp.contains("SIZE"));
    assert!(resp.contains("8BITMIME"));

    client.send_and_read("QUIT").await;
}

#[tokio::test]
async fn test_helo_response() {
    let tmp = tempfile::TempDir::new().unwrap();
    let config = test_config(tmp.path());
    let (port, _shutdown) = start_server(config).await;
    let mut client = TestClient::connect(port).await;
    client.read_line().await;

    let resp = client.send_and_read("HELO client.example.com").await;
    assert!(resp.starts_with("250 "));

    client.send_and_read("QUIT").await;
}

#[tokio::test]
async fn test_full_mail_transaction() {
    let tmp = tempfile::TempDir::new().unwrap();
    let config = test_config(tmp.path());
    let (port, _shutdown) = start_server(config).await;
    let mut client = TestClient::connect(port).await;
    client.read_line().await;

    let resp = client.send_and_read("EHLO client.example.com").await;
    assert!(resp.contains("250"));

    let resp = client.send_and_read("MAIL FROM:<sender@example.com>").await;
    assert!(resp.starts_with("250 "));

    let resp = client.send_and_read("RCPT TO:<alice@test.local>").await;
    assert!(resp.starts_with("250 "));

    let resp = client.send_and_read("DATA").await;
    assert!(resp.starts_with("354"));

    client.send(test_email()).await;
    let resp = client.send_and_read(".").await;
    assert!(resp.starts_with("250 "));

    let resp = client.send_and_read("QUIT").await;
    assert!(resp.starts_with("221"));

    let alice_dir = inbox(tmp.path(), "alice");
    assert!(alice_dir.exists());
    let entries = collect_md_files(&alice_dir);
    assert_eq!(entries.len(), 1);
}

#[tokio::test]
async fn test_multi_recipient() {
    let tmp = tempfile::TempDir::new().unwrap();
    let config = test_config(tmp.path());
    let (port, _shutdown) = start_server(config).await;
    let mut client = TestClient::connect(port).await;
    client.read_line().await;

    client.send_and_read("EHLO client.example.com").await;
    client.send_and_read("MAIL FROM:<sender@example.com>").await;
    client.send_and_read("RCPT TO:<alice@test.local>").await;
    client.send_and_read("RCPT TO:<catchall@test.local>").await;

    let resp = client.send_and_read("DATA").await;
    assert!(resp.starts_with("354"));

    client.send(test_email()).await;
    let resp = client.send_and_read(".").await;
    assert!(resp.starts_with("250"));

    client.send_and_read("QUIT").await;

    let alice_mds = collect_md_files(&inbox(tmp.path(), "alice"));
    let catchall_mds = collect_md_files(&inbox(tmp.path(), "catchall"));
    assert_eq!(alice_mds.len(), 1);
    assert_eq!(catchall_mds.len(), 1);
}

#[tokio::test]
async fn test_unrecognized_command() {
    let tmp = tempfile::TempDir::new().unwrap();
    let config = test_config(tmp.path());
    let (port, _shutdown) = start_server(config).await;
    let mut client = TestClient::connect(port).await;
    client.read_line().await;

    let resp = client.send_and_read("BOGUS").await;
    assert!(resp.starts_with("500"));
}

#[tokio::test]
async fn test_out_of_sequence_mail() {
    let tmp = tempfile::TempDir::new().unwrap();
    let config = test_config(tmp.path());
    let (port, _shutdown) = start_server(config).await;
    let mut client = TestClient::connect(port).await;
    client.read_line().await;

    let resp = client.send_and_read("MAIL FROM:<test@test.com>").await;
    assert!(resp.starts_with("503"));
}

#[tokio::test]
async fn test_out_of_sequence_rcpt() {
    let tmp = tempfile::TempDir::new().unwrap();
    let config = test_config(tmp.path());
    let (port, _shutdown) = start_server(config).await;
    let mut client = TestClient::connect(port).await;
    client.read_line().await;

    client.send_and_read("EHLO test.com").await;

    let resp = client.send_and_read("RCPT TO:<test@test.com>").await;
    assert!(resp.starts_with("503"));
}

#[tokio::test]
async fn test_out_of_sequence_data() {
    let tmp = tempfile::TempDir::new().unwrap();
    let config = test_config(tmp.path());
    let (port, _shutdown) = start_server(config).await;
    let mut client = TestClient::connect(port).await;
    client.read_line().await;

    client.send_and_read("EHLO test.com").await;
    client.send_and_read("MAIL FROM:<test@test.com>").await;

    let resp = client.send_and_read("DATA").await;
    assert!(resp.starts_with("503"));
}

#[tokio::test]
async fn test_noop() {
    let tmp = tempfile::TempDir::new().unwrap();
    let config = test_config(tmp.path());
    let (port, _shutdown) = start_server(config).await;
    let mut client = TestClient::connect(port).await;
    client.read_line().await;

    let resp = client.send_and_read("NOOP").await;
    assert!(resp.starts_with("250"));
}

#[tokio::test]
async fn test_rset() {
    let tmp = tempfile::TempDir::new().unwrap();
    let config = test_config(tmp.path());
    let (port, _shutdown) = start_server(config).await;
    let mut client = TestClient::connect(port).await;
    client.read_line().await;

    client.send_and_read("EHLO test.com").await;
    client.send_and_read("MAIL FROM:<test@test.com>").await;

    let resp = client.send_and_read("RSET").await;
    assert!(resp.starts_with("250"));

    let resp = client.send_and_read("RCPT TO:<test@test.com>").await;
    assert!(resp.starts_with("503"));
}

#[tokio::test]
async fn test_quit() {
    let tmp = tempfile::TempDir::new().unwrap();
    let config = test_config(tmp.path());
    let (port, _shutdown) = start_server(config).await;
    let mut client = TestClient::connect(port).await;
    client.read_line().await;

    let resp = client.send_and_read("QUIT").await;
    assert!(resp.starts_with("221"));
}

#[tokio::test]
async fn test_ehlo_without_domain() {
    let tmp = tempfile::TempDir::new().unwrap();
    let config = test_config(tmp.path());
    let (port, _shutdown) = start_server(config).await;
    let mut client = TestClient::connect(port).await;
    client.read_line().await;

    let resp = client.send_and_read("EHLO").await;
    assert!(resp.starts_with("501"));
}

#[tokio::test]
async fn test_double_mail_from() {
    let tmp = tempfile::TempDir::new().unwrap();
    let config = test_config(tmp.path());
    let (port, _shutdown) = start_server(config).await;
    let mut client = TestClient::connect(port).await;
    client.read_line().await;

    client.send_and_read("EHLO test.com").await;
    client.send_and_read("MAIL FROM:<a@test.com>").await;
    let resp = client.send_and_read("MAIL FROM:<b@test.com>").await;
    assert!(resp.starts_with("503"));
}

// --- Relay rejection: RCPT TO for domains other than config.domain ---

#[tokio::test]
async fn test_rcpt_rejects_unrelated_domain() {
    let tmp = tempfile::TempDir::new().unwrap();
    let config = test_config(tmp.path());
    let (port, _shutdown) = start_server(config).await;
    let mut client = TestClient::connect(port).await;
    client.read_line().await;

    client.send_and_read("EHLO client.example.com").await;
    client.send_and_read("MAIL FROM:<sender@example.com>").await;

    let resp = client.send_and_read("RCPT TO:<user@evil.example>").await;
    assert!(
        resp.starts_with("550 5.7.1"),
        "Expected 550 5.7.1 for relay reject: {resp}"
    );

    let resp = client.send_and_read("DATA").await;
    assert!(
        resp.starts_with("503"),
        "Expected 503 when no RCPT accepted: {resp}"
    );

    client.send_and_read("QUIT").await;

    let catchall_dir = inbox(tmp.path(), "catchall");
    let md_files = collect_md_files(&catchall_dir);
    assert_eq!(
        md_files.len(),
        0,
        "No file should land in catchall for rejected RCPT"
    );
}

#[tokio::test]
async fn test_rcpt_rejects_subdomain() {
    let tmp = tempfile::TempDir::new().unwrap();
    let config = test_config(tmp.path());
    let (port, _shutdown) = start_server(config).await;
    let mut client = TestClient::connect(port).await;
    client.read_line().await;

    client.send_and_read("EHLO client.example.com").await;
    client.send_and_read("MAIL FROM:<sender@example.com>").await;

    let resp = client.send_and_read("RCPT TO:<user@sub.test.local>").await;
    assert!(
        resp.starts_with("550 5.7.1"),
        "Expected 550 5.7.1 for subdomain reject: {resp}"
    );

    client.send_and_read("QUIT").await;

    let catchall_dir = inbox(tmp.path(), "catchall");
    let md_files = collect_md_files(&catchall_dir);
    assert_eq!(md_files.len(), 0);
}

#[tokio::test]
async fn test_rcpt_rejects_missing_at() {
    let tmp = tempfile::TempDir::new().unwrap();
    let config = test_config(tmp.path());
    let (port, _shutdown) = start_server(config).await;
    let mut client = TestClient::connect(port).await;
    client.read_line().await;

    client.send_and_read("EHLO client.example.com").await;
    client.send_and_read("MAIL FROM:<sender@example.com>").await;

    let resp = client.send_and_read("RCPT TO:<alice>").await;
    assert!(
        resp.starts_with("550 5.7.1"),
        "Expected 550 5.7.1 for missing-@ reject: {resp}"
    );

    client.send_and_read("QUIT").await;
}

#[tokio::test]
async fn test_rcpt_mixed_valid_invalid() {
    let tmp = tempfile::TempDir::new().unwrap();
    let config = test_config(tmp.path());
    let (port, _shutdown) = start_server(config).await;
    let mut client = TestClient::connect(port).await;
    client.read_line().await;

    client.send_and_read("EHLO client.example.com").await;
    client.send_and_read("MAIL FROM:<sender@example.com>").await;

    let resp = client.send_and_read("RCPT TO:<alice@test.local>").await;
    assert!(resp.starts_with("250"), "Expected 250 for alice: {resp}");

    let resp = client.send_and_read("RCPT TO:<mallory@evil.example>").await;
    assert!(
        resp.starts_with("550 5.7.1"),
        "Expected 550 5.7.1 for evil.example: {resp}"
    );

    let resp = client.send_and_read("DATA").await;
    assert!(
        resp.starts_with("354"),
        "Expected 354 (one accepted RCPT is enough): {resp}"
    );

    client.send(test_email()).await;
    let resp = client.send_and_read(".").await;
    assert!(resp.starts_with("250"), "Expected 250 on DATA end: {resp}");

    client.send_and_read("QUIT").await;

    let alice_mds = collect_md_files(&inbox(tmp.path(), "alice"));
    let catchall_mds = collect_md_files(&inbox(tmp.path(), "catchall"));
    assert_eq!(alice_mds.len(), 1, "alice should have one message");
    assert_eq!(catchall_mds.len(), 0, "catchall should have nothing");
}

#[tokio::test]
async fn test_rcpt_accepts_case_insensitive_domain() {
    let tmp = tempfile::TempDir::new().unwrap();
    let config = test_config(tmp.path());
    let (port, _shutdown) = start_server(config).await;
    let mut client = TestClient::connect(port).await;
    client.read_line().await;

    client.send_and_read("EHLO client.example.com").await;
    client.send_and_read("MAIL FROM:<sender@example.com>").await;

    let resp = client.send_and_read("RCPT TO:<alice@TEST.LOCAL>").await;
    assert!(
        resp.starts_with("250"),
        "Expected 250 for case-insensitive domain: {resp}"
    );

    let resp = client.send_and_read("DATA").await;
    assert!(resp.starts_with("354"), "Expected 354: {resp}");

    client.send(test_email()).await;
    let resp = client.send_and_read(".").await;
    assert!(resp.starts_with("250"), "Expected 250: {resp}");

    client.send_and_read("QUIT").await;

    let alice_mds = collect_md_files(&inbox(tmp.path(), "alice"));
    assert_eq!(alice_mds.len(), 1);
}

// --- Size limit tests ---

#[tokio::test]
async fn test_message_size_limit() {
    let tmp = tempfile::TempDir::new().unwrap();
    let config = test_config(tmp.path());
    let (port, _shutdown) = start_server_with_size_limit(config, 100).await;
    let mut client = TestClient::connect(port).await;
    client.read_line().await;

    client.send_and_read("EHLO test.com").await;
    client.send_and_read("MAIL FROM:<sender@test.com>").await;
    client.send_and_read("RCPT TO:<alice@test.local>").await;
    client.send_and_read("DATA").await;

    let large_body = "X".repeat(200);
    client
        .send(&format!(
            "From: sender@test.com\r\nTo: alice@test.local\r\nSubject: Big\r\n\r\n{large_body}"
        ))
        .await;
    let resp = client.send_and_read(".").await;
    assert!(resp.starts_with("552"), "Expected 552 but got: {resp}");
}

// --- Timeout tests ---

#[tokio::test]
async fn test_idle_timeout() {
    let tmp = tempfile::TempDir::new().unwrap();
    let config = test_config(tmp.path());
    let server =
        SmtpServer::new(config).with_timeouts(Duration::from_millis(200), Duration::from_secs(10));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let (_shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);

    tokio::spawn(async move {
        server.run(listener, shutdown_rx).await.unwrap();
    });
    tokio::time::sleep(Duration::from_millis(50)).await;

    let mut client = TestClient::connect(port).await;
    client.read_line().await;

    tokio::time::sleep(Duration::from_millis(300)).await;

    let resp = client.read_line().await;
    assert!(
        resp.contains("421") || resp.is_empty(),
        "Expected timeout response, got: {resp}"
    );
}

// --- STARTTLS tests ---

#[tokio::test]
async fn test_starttls_advertised_in_ehlo() {
    let tmp = tempfile::TempDir::new().unwrap();
    let config = test_config(tmp.path());

    let (cert_path, key_path) = super::tls::generate_test_certs(tmp.path()).unwrap();

    let server = SmtpServer::new(config)
        .with_tls(&cert_path, &key_path)
        .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let (_shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);

    tokio::spawn(async move {
        server.run(listener, shutdown_rx).await.unwrap();
    });
    tokio::time::sleep(Duration::from_millis(50)).await;

    let mut client = TestClient::connect(port).await;
    client.read_line().await;

    let resp = client.send_and_read("EHLO test.com").await;
    assert!(
        resp.contains("STARTTLS"),
        "EHLO response should advertise STARTTLS: {resp}"
    );
}

#[tokio::test]
async fn test_starttls_not_available_without_tls() {
    let tmp = tempfile::TempDir::new().unwrap();
    let config = test_config(tmp.path());
    let (port, _shutdown) = start_server(config).await;
    let mut client = TestClient::connect(port).await;
    client.read_line().await;

    client.send_and_read("EHLO test.com").await;
    let resp = client.send_and_read("STARTTLS").await;
    assert!(
        resp.starts_with("502"),
        "Should reject STARTTLS without TLS config: {resp}"
    );
}

#[tokio::test]
async fn test_plain_connection_works_without_starttls() {
    let tmp = tempfile::TempDir::new().unwrap();
    let config = test_config(tmp.path());
    let (port, _shutdown) = start_server(config).await;
    let mut client = TestClient::connect(port).await;
    client.read_line().await;

    client.send_and_read("EHLO test.com").await;
    client.send_and_read("MAIL FROM:<sender@test.com>").await;
    client.send_and_read("RCPT TO:<alice@test.local>").await;
    client.send_and_read("DATA").await;
    client.send(test_email()).await;
    let resp = client.send_and_read(".").await;
    assert!(
        resp.starts_with("250"),
        "Plain connection should work: {resp}"
    );

    client.send_and_read("QUIT").await;
}

#[tokio::test]
async fn test_starttls_upgrade() {
    let tmp = tempfile::TempDir::new().unwrap();
    let config = test_config(tmp.path());

    let (cert_path, key_path) = super::tls::generate_test_certs(tmp.path()).unwrap();

    let server = SmtpServer::new(config)
        .with_tls(&cert_path, &key_path)
        .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let (_shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);

    tokio::spawn(async move {
        server.run(listener, shutdown_rx).await.unwrap();
    });
    tokio::time::sleep(Duration::from_millis(50)).await;

    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

    let stream = TcpStream::connect(format!("127.0.0.1:{port}"))
        .await
        .unwrap();
    let (reader, mut writer) = tokio::io::split(stream);
    let mut reader = BufReader::new(reader);
    let mut buf = String::new();

    // Read banner
    reader.read_line(&mut buf).await.unwrap();
    assert!(buf.starts_with("220"));

    // Send EHLO
    writer.write_all(b"EHLO test.com\r\n").await.unwrap();
    buf.clear();
    loop {
        reader.read_line(&mut buf).await.unwrap();
        if buf.contains("250 ") {
            break;
        }
    }

    // Send STARTTLS
    writer.write_all(b"STARTTLS\r\n").await.unwrap();
    buf.clear();
    reader.read_line(&mut buf).await.unwrap();
    assert!(buf.starts_with("220"), "Expected 220 Ready for TLS: {buf}");

    // Upgrade to TLS
    let inner = reader.into_inner().unsplit(writer);

    let mut root_store = rustls::RootCertStore::empty();
    let cert_pem = std::fs::read(&cert_path).unwrap();
    let certs: Vec<_> = rustls_pemfile::certs(&mut std::io::BufReader::new(cert_pem.as_slice()))
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    for cert in &certs {
        root_store.add(cert.clone()).unwrap();
    }

    let tls_config = rustls::ClientConfig::builder()
        .with_root_certificates(root_store)
        .with_no_client_auth();
    let connector = tokio_rustls::TlsConnector::from(Arc::new(tls_config));
    let server_name = rustls_pki_types::ServerName::try_from("localhost").unwrap();
    let tls_stream = connector.connect(server_name, inner).await.unwrap();

    let (tls_reader, mut tls_writer) = tokio::io::split(tls_stream);
    let mut tls_reader = BufReader::new(tls_reader);

    // After TLS, send EHLO again and complete a mail transaction
    tls_writer.write_all(b"EHLO test.com\r\n").await.unwrap();
    buf.clear();
    loop {
        tls_reader.read_line(&mut buf).await.unwrap();
        if buf.contains("250 ") {
            break;
        }
    }
    assert!(
        !buf.contains("STARTTLS"),
        "STARTTLS should not be in post-upgrade EHLO: {buf}"
    );

    tls_writer
        .write_all(b"MAIL FROM:<sender@test.com>\r\n")
        .await
        .unwrap();
    buf.clear();
    tls_reader.read_line(&mut buf).await.unwrap();
    assert!(buf.starts_with("250"));

    tls_writer
        .write_all(b"RCPT TO:<alice@test.local>\r\n")
        .await
        .unwrap();
    buf.clear();
    tls_reader.read_line(&mut buf).await.unwrap();
    assert!(buf.starts_with("250"));

    tls_writer.write_all(b"DATA\r\n").await.unwrap();
    buf.clear();
    tls_reader.read_line(&mut buf).await.unwrap();
    assert!(buf.starts_with("354"));

    tls_writer.write_all(test_email().as_bytes()).await.unwrap();
    tls_writer.write_all(b".\r\n").await.unwrap();
    buf.clear();
    tls_reader.read_line(&mut buf).await.unwrap();
    assert!(
        buf.starts_with("250"),
        "Expected 250 after DATA over TLS: {buf}"
    );

    tls_writer.write_all(b"QUIT\r\n").await.unwrap();
    buf.clear();
    tls_reader.read_line(&mut buf).await.unwrap();
    assert!(buf.starts_with("221"));
}

// --- Ingest integration tests ---

#[tokio::test]
async fn test_ingest_creates_markdown_file() {
    let tmp = tempfile::TempDir::new().unwrap();
    let config = test_config(tmp.path());
    let (port, _shutdown) = start_server(config).await;
    let mut client = TestClient::connect(port).await;
    client.read_line().await;

    client.send_and_read("EHLO test.com").await;
    client.send_and_read("MAIL FROM:<sender@example.com>").await;
    client.send_and_read("RCPT TO:<alice@test.local>").await;
    client.send_and_read("DATA").await;
    client.send(test_email()).await;
    let resp = client.send_and_read(".").await;
    assert!(resp.starts_with("250"), "Expected 250: {resp}");

    client.send_and_read("QUIT").await;

    let alice_dir = inbox(tmp.path(), "alice");
    let md_files = collect_md_files(&alice_dir);
    assert_eq!(md_files.len(), 1);

    let content = std::fs::read_to_string(&md_files[0]).unwrap();
    assert!(content.contains("+++"));
    assert!(content.contains("subject = \"Test\""));
    assert!(content.contains("Hello World"));
}

#[tokio::test]
async fn test_ingest_failure_returns_451() {
    let tmp = tempfile::TempDir::new().unwrap();
    // Create a file, then use it as a directory component so create_dir_all
    // fails even when running as root (file is not a directory)
    let blocker = tmp.path().join("blocker");
    std::fs::write(&blocker, "x").unwrap();
    let bad_data_dir = blocker.join("data");

    // A mailbox entry must exist so the RCPT preflight accepts;
    // the failure is forced later at `create_dir_all` time because the
    // data_dir parent is a regular file.
    let mut mailboxes = HashMap::new();
    mailboxes.insert(
        "alice".to_string(),
        MailboxConfig {
            address: "alice@test.local".to_string(),
            owner: "root".to_string(),
            hooks: vec![],
            trust: None,
            trusted_senders: None,
            allow_root_catchall: false,
        },
    );
    let config = Config {
        domain: "test.local".to_string(),
        data_dir: bad_data_dir,
        dkim_selector: "aimx".to_string(),
        trust: "none".to_string(),
        trusted_senders: vec![],
        hook_templates: Vec::new(),
        mailboxes,
        verify_host: None,
        enable_ipv6: false,
        upgrade: None,
    };
    let (port, _shutdown) = start_server(config).await;
    let mut client = TestClient::connect(port).await;
    client.read_line().await;

    client.send_and_read("EHLO test.com").await;
    client.send_and_read("MAIL FROM:<sender@test.com>").await;
    let resp = client.send_and_read("RCPT TO:<alice@test.local>").await;
    assert!(resp.starts_with("250"), "RCPT should accept: {resp}");
    client.send_and_read("DATA").await;
    client.send(test_email()).await;
    let resp = client.send_and_read(".").await;
    assert!(
        resp.starts_with("451"),
        "Expected 451 on ingest failure: {resp}"
    );
}

// --- Connection hardening tests ---

#[tokio::test]
async fn test_connection_limit() {
    let tmp = tempfile::TempDir::new().unwrap();
    let config = test_config(tmp.path());
    let server = SmtpServer::new(config).with_max_connections(2);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let (_shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);

    tokio::spawn(async move {
        server.run(listener, shutdown_rx).await.unwrap();
    });
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Open 2 connections (at the limit)
    let mut c1 = TestClient::connect(port).await;
    c1.read_line().await;
    let mut c2 = TestClient::connect(port).await;
    c2.read_line().await;

    // Third connection should be rejected
    let mut c3 = TestClient::connect(port).await;
    let resp = c3.read_line().await;
    assert!(
        resp.starts_with("421"),
        "Expected 421 for connection limit: {resp}"
    );

    // Close one connection, then new one should work
    c1.send_and_read("QUIT").await;
    tokio::time::sleep(Duration::from_millis(100)).await;

    let mut c4 = TestClient::connect(port).await;
    let banner = c4.read_line().await;
    assert!(
        banner.starts_with("220"),
        "Expected 220 after freeing a slot: {banner}"
    );
    c4.send_and_read("QUIT").await;
    c2.send_and_read("QUIT").await;
}

#[tokio::test]
async fn test_command_flood_limit() {
    let tmp = tempfile::TempDir::new().unwrap();
    let config = test_config(tmp.path());
    let server = SmtpServer::new(config).with_max_commands_before_data(5);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let (_shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);

    tokio::spawn(async move {
        server.run(listener, shutdown_rx).await.unwrap();
    });
    tokio::time::sleep(Duration::from_millis(50)).await;

    let mut client = TestClient::connect(port).await;
    client.read_line().await;

    for _ in 0..5 {
        let resp = client.send_and_read("NOOP").await;
        assert!(resp.starts_with("250"));
    }

    let resp = client.send_and_read("NOOP").await;
    assert!(
        resp.contains("421"),
        "Expected 421 for command flood: {resp}"
    );
}

#[tokio::test]
async fn test_bare_lf_rejected_in_data() {
    let tmp = tempfile::TempDir::new().unwrap();
    let config = test_config(tmp.path());
    let (port, _shutdown) = start_server(config).await;

    let stream = TcpStream::connect(format!("127.0.0.1:{port}"))
        .await
        .unwrap();
    let (reader, mut writer) = tokio::io::split(stream);
    let mut reader = BufReader::new(reader);
    let mut buf = String::new();

    // Read banner
    reader.read_line(&mut buf).await.unwrap();

    writer.write_all(b"EHLO test.com\r\n").await.unwrap();
    buf.clear();
    loop {
        reader.read_line(&mut buf).await.unwrap();
        if buf.contains("250 ") {
            break;
        }
    }

    writer
        .write_all(b"MAIL FROM:<test@test.com>\r\n")
        .await
        .unwrap();
    buf.clear();
    reader.read_line(&mut buf).await.unwrap();

    writer
        .write_all(b"RCPT TO:<alice@test.local>\r\n")
        .await
        .unwrap();
    buf.clear();
    reader.read_line(&mut buf).await.unwrap();

    writer.write_all(b"DATA\r\n").await.unwrap();
    buf.clear();
    reader.read_line(&mut buf).await.unwrap();

    // Send data with bare LF (no CR)
    writer
        .write_all(b"Subject: test\nBare LF line\r\n.\r\n")
        .await
        .unwrap();
    buf.clear();
    reader.read_line(&mut buf).await.unwrap();
    assert!(buf.starts_with("500"), "Expected 500 for bare LF: {buf}");
}

// --- Multiple transactions per connection ---

#[tokio::test]
async fn test_multiple_transactions_per_connection() {
    let tmp = tempfile::TempDir::new().unwrap();
    let config = test_config(tmp.path());
    let (port, _shutdown) = start_server(config).await;
    let mut client = TestClient::connect(port).await;
    client.read_line().await;

    client.send_and_read("EHLO test.com").await;

    // First message
    client.send_and_read("MAIL FROM:<sender@test.com>").await;
    client.send_and_read("RCPT TO:<alice@test.local>").await;
    client.send_and_read("DATA").await;
    client.send(test_email()).await;
    let resp = client.send_and_read(".").await;
    assert!(resp.starts_with("250"));

    // Second message
    client.send_and_read("MAIL FROM:<sender2@test.com>").await;
    client.send_and_read("RCPT TO:<alice@test.local>").await;
    client.send_and_read("DATA").await;
    client.send(test_email()).await;
    let resp = client.send_and_read(".").await;
    assert!(resp.starts_with("250"));

    client.send_and_read("QUIT").await;

    let alice_dir = inbox(tmp.path(), "alice");
    let md_files = collect_md_files(&alice_dir);
    assert_eq!(md_files.len(), 2);
}

use std::sync::Arc;

// --- Attachment integration tests ---

#[tokio::test]
async fn test_text_attachment_from_real_file() {
    let tmp = tempfile::TempDir::new().unwrap();
    let config = test_config(tmp.path());
    let (port, _shutdown) = start_server(config).await;

    let readme_bytes =
        std::fs::read(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("README.md")).unwrap();
    let readme_b64 =
        base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &readme_bytes);

    let email = format!(
        "From: sender@example.com\r\n\
         To: alice@test.local\r\n\
         Subject: Text attachment test\r\n\
         Date: Mon, 01 Jan 2024 00:00:00 +0000\r\n\
         Message-ID: <text-att@example.com>\r\n\
         MIME-Version: 1.0\r\n\
         Content-Type: multipart/mixed; boundary=\"textbound\"\r\n\
         \r\n\
         --textbound\r\n\
         Content-Type: text/plain; charset=utf-8\r\n\
         \r\n\
         See the attached README.\r\n\
         --textbound\r\n\
         Content-Type: text/markdown; name=\"README.md\"\r\n\
         Content-Disposition: attachment; filename=\"README.md\"\r\n\
         Content-Transfer-Encoding: base64\r\n\
         \r\n\
         {readme_b64}\r\n\
         --textbound--\r\n"
    );

    let mut client = TestClient::connect(port).await;
    client.read_line().await;
    client.send_and_read("EHLO test.com").await;
    client.send_and_read("MAIL FROM:<sender@example.com>").await;
    client.send_and_read("RCPT TO:<alice@test.local>").await;
    client.send_and_read("DATA").await;
    client.send(&email).await;
    let resp = client.send_and_read(".").await;
    assert!(resp.starts_with("250"), "Expected 250: {resp}");
    client.send_and_read("QUIT").await;

    let attachment_path =
        find_attachment(&inbox(tmp.path(), "alice"), "README.md").expect("attachment missing");
    assert!(attachment_path.exists(), "Attachment file should exist");
    let received = std::fs::read(&attachment_path).unwrap();
    assert_eq!(
        received, readme_bytes,
        "Attachment content must match the original README.md byte-for-byte"
    );
}

#[tokio::test]
async fn test_binary_attachment_roundtrip() {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    let tmp = tempfile::TempDir::new().unwrap();
    let config = test_config(tmp.path());
    let (port, _shutdown) = start_server(config).await;

    // Generate deterministic pseudo-random binary blob of 500 ± rand(0..50) bytes.
    // Use a hash-based PRNG seeded from the port so it varies per run but is reproducible.
    let mut hasher = DefaultHasher::new();
    port.hash(&mut hasher);
    let seed = hasher.finish();
    let size = 500 + ((seed % 51) as usize); // 500..=550
    let binary_data: Vec<u8> = (0..size)
        .map(|i| {
            let mut h = DefaultHasher::new();
            (seed, i).hash(&mut h);
            h.finish() as u8
        })
        .collect();

    let binary_b64 =
        base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &binary_data);

    let email = format!(
        "From: sender@example.com\r\n\
         To: alice@test.local\r\n\
         Subject: Binary attachment test\r\n\
         Date: Mon, 01 Jan 2024 00:00:00 +0000\r\n\
         Message-ID: <bin-att@example.com>\r\n\
         MIME-Version: 1.0\r\n\
         Content-Type: multipart/mixed; boundary=\"binbound\"\r\n\
         \r\n\
         --binbound\r\n\
         Content-Type: text/plain; charset=utf-8\r\n\
         \r\n\
         Binary blob attached.\r\n\
         --binbound\r\n\
         Content-Type: application/octet-stream; name=\"random.bin\"\r\n\
         Content-Disposition: attachment; filename=\"random.bin\"\r\n\
         Content-Transfer-Encoding: base64\r\n\
         \r\n\
         {binary_b64}\r\n\
         --binbound--\r\n"
    );

    let mut client = TestClient::connect(port).await;
    client.read_line().await;
    client.send_and_read("EHLO test.com").await;
    client.send_and_read("MAIL FROM:<sender@example.com>").await;
    client.send_and_read("RCPT TO:<alice@test.local>").await;
    client.send_and_read("DATA").await;
    client.send(&email).await;
    let resp = client.send_and_read(".").await;
    assert!(resp.starts_with("250"), "Expected 250: {resp}");
    client.send_and_read("QUIT").await;

    let attachment_path =
        find_attachment(&inbox(tmp.path(), "alice"), "random.bin").expect("bin attachment");
    assert!(
        attachment_path.exists(),
        "Binary attachment file should exist"
    );
    let received = std::fs::read(&attachment_path).unwrap();
    assert_eq!(
        received.len(),
        binary_data.len(),
        "Binary attachment size mismatch: expected {}, got {}",
        binary_data.len(),
        received.len()
    );
    assert_eq!(
        received, binary_data,
        "Binary attachment must be byte-identical to the original"
    );
}

#[tokio::test]
async fn test_mixed_text_and_binary_attachments_with_body() {
    let tmp = tempfile::TempDir::new().unwrap();
    let config = test_config(tmp.path());
    let (port, _shutdown) = start_server(config).await;

    // Text attachment: the real README.md
    let readme_bytes =
        std::fs::read(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("README.md")).unwrap();
    let readme_b64 =
        base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &readme_bytes);

    // Binary attachment: deterministic pseudo-random blob
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    port.hash(&mut hasher);
    let seed = hasher.finish();
    let size = 500 + ((seed % 51) as usize);
    let binary_data: Vec<u8> = (0..size)
        .map(|i| {
            let mut h = DefaultHasher::new();
            (seed, i).hash(&mut h);
            h.finish() as u8
        })
        .collect();
    let binary_b64 =
        base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &binary_data);

    let body_text = "This email has both a text and binary attachment.\r\nPlease verify everything arrives intact.";

    let email = format!(
        "From: sender@example.com\r\n\
         To: alice@test.local\r\n\
         Subject: Mixed attachments test\r\n\
         Date: Mon, 01 Jan 2024 00:00:00 +0000\r\n\
         Message-ID: <mixed-att@example.com>\r\n\
         MIME-Version: 1.0\r\n\
         Content-Type: multipart/mixed; boundary=\"mixbound\"\r\n\
         \r\n\
         --mixbound\r\n\
         Content-Type: text/plain; charset=utf-8\r\n\
         \r\n\
         {body_text}\r\n\
         --mixbound\r\n\
         Content-Type: text/markdown; name=\"README.md\"\r\n\
         Content-Disposition: attachment; filename=\"README.md\"\r\n\
         Content-Transfer-Encoding: base64\r\n\
         \r\n\
         {readme_b64}\r\n\
         --mixbound\r\n\
         Content-Type: application/octet-stream; name=\"data.bin\"\r\n\
         Content-Disposition: attachment; filename=\"data.bin\"\r\n\
         Content-Transfer-Encoding: base64\r\n\
         \r\n\
         {binary_b64}\r\n\
         --mixbound--\r\n"
    );

    let mut client = TestClient::connect(port).await;
    client.read_line().await;
    client.send_and_read("EHLO test.com").await;
    client.send_and_read("MAIL FROM:<sender@example.com>").await;
    client.send_and_read("RCPT TO:<alice@test.local>").await;
    client.send_and_read("DATA").await;
    client.send(&email).await;
    let resp = client.send_and_read(".").await;
    assert!(resp.starts_with("250"), "Expected 250: {resp}");
    client.send_and_read("QUIT").await;

    // Verify text attachment. Both should be inside the same bundle dir.
    let alice_dir = inbox(tmp.path(), "alice");
    let text_att_path =
        find_attachment(&alice_dir, "README.md").expect("README attachment missing");
    assert!(text_att_path.exists(), "Text attachment should exist");
    let received_text = std::fs::read(&text_att_path).unwrap();
    assert_eq!(
        received_text, readme_bytes,
        "Text attachment must match the original README.md"
    );

    // Verify binary attachment
    let bin_att_path = find_attachment(&alice_dir, "data.bin").expect("bin attachment missing");
    assert!(bin_att_path.exists(), "Binary attachment should exist");
    let received_bin = std::fs::read(&bin_att_path).unwrap();
    assert_eq!(
        received_bin, binary_data,
        "Binary attachment must be byte-identical to the original"
    );

    // Verify the email body is intact
    let md_files = collect_md_files(&alice_dir);
    assert_eq!(md_files.len(), 1, "Should have exactly one email file");

    let content = std::fs::read_to_string(&md_files[0]).unwrap();
    assert!(
        content.contains("subject = \"Mixed attachments test\""),
        "Frontmatter should contain the subject"
    );
    assert!(
        content.contains("This email has both a text and binary attachment."),
        "Body text should be preserved"
    );
    assert!(
        content.contains("Please verify everything arrives intact."),
        "Full body text should be preserved"
    );

    // Verify frontmatter lists both attachments
    assert!(
        content.contains("README.md"),
        "Frontmatter should reference README.md attachment"
    );
    assert!(
        content.contains("data.bin"),
        "Frontmatter should reference data.bin attachment"
    );
}

// ---------------------------------------------------------------------------
// RCPT-time mailbox-routing preflight
// ---------------------------------------------------------------------------

fn test_config_no_catchall(data_dir: &std::path::Path) -> Config {
    let mut mailboxes = HashMap::new();
    mailboxes.insert(
        "alice".to_string(),
        MailboxConfig {
            address: "alice@test.local".to_string(),
            owner: "root".to_string(),
            hooks: vec![],
            trust: None,
            trusted_senders: None,
            allow_root_catchall: false,
        },
    );
    Config {
        domain: "test.local".to_string(),
        data_dir: data_dir.to_path_buf(),
        dkim_selector: "aimx".to_string(),
        trust: "none".to_string(),
        trusted_senders: vec![],
        hook_templates: Vec::new(),
        mailboxes,
        verify_host: None,
        enable_ipv6: false,
        upgrade: None,
    }
}

#[tokio::test]
async fn test_rcpt_rejects_unknown_mailbox_no_catchall() {
    let tmp = tempfile::TempDir::new().unwrap();
    let config = test_config_no_catchall(tmp.path());
    let (port, _shutdown) = start_server(config).await;
    let mut client = TestClient::connect(port).await;
    client.read_line().await;

    client.send_and_read("EHLO client.example.com").await;
    client.send_and_read("MAIL FROM:<sender@example.com>").await;

    let resp = client.send_and_read("RCPT TO:<bob@test.local>").await;
    assert!(
        resp.starts_with("550 5.1.1"),
        "Expected 550 5.1.1 for unknown recipient: {resp}"
    );

    let resp = client.send_and_read("RCPT TO:<alice@test.local>").await;
    assert!(
        resp.starts_with("250"),
        "Expected 250 for known recipient: {resp}"
    );

    client.send_and_read("QUIT").await;
}

#[tokio::test]
async fn test_rcpt_catchall_accepts_any_local_part() {
    let tmp = tempfile::TempDir::new().unwrap();
    let config = test_config(tmp.path());
    let (port, _shutdown) = start_server(config).await;
    let mut client = TestClient::connect(port).await;
    client.read_line().await;

    client.send_and_read("EHLO client.example.com").await;
    client.send_and_read("MAIL FROM:<sender@example.com>").await;

    // With a catchall present, an unknown local part still resolves.
    let resp = client
        .send_and_read("RCPT TO:<randomuser@test.local>")
        .await;
    assert!(
        resp.starts_with("250"),
        "Expected 250 with catchall present: {resp}"
    );

    client.send_and_read("QUIT").await;
}

// ---------------------------------------------------------------------------
// DATA partial-success contract
// ---------------------------------------------------------------------------

fn test_config_two_mailboxes(data_dir: &std::path::Path) -> Config {
    let mut mailboxes = HashMap::new();
    mailboxes.insert(
        "alice".to_string(),
        MailboxConfig {
            address: "alice@test.local".to_string(),
            owner: "root".to_string(),
            hooks: vec![],
            trust: None,
            trusted_senders: None,
            allow_root_catchall: false,
        },
    );
    mailboxes.insert(
        "bob".to_string(),
        MailboxConfig {
            address: "bob@test.local".to_string(),
            owner: "root".to_string(),
            hooks: vec![],
            trust: None,
            trusted_senders: None,
            allow_root_catchall: false,
        },
    );
    Config {
        domain: "test.local".to_string(),
        data_dir: data_dir.to_path_buf(),
        dkim_selector: "aimx".to_string(),
        trust: "none".to_string(),
        trusted_senders: vec![],
        hook_templates: Vec::new(),
        mailboxes,
        verify_host: None,
        enable_ipv6: false,
        upgrade: None,
    }
}

fn mixed_test_email() -> &'static str {
    "From: sender@example.com\r\nTo: alice@test.local, bob@test.local\r\nSubject: Mixed\r\nDate: Mon, 01 Jan 2024 00:00:00 +0000\r\nMessage-ID: <mixed@example.com>\r\n\r\nHello both\r\n"
}

/// RAII guard for the `AIMX_TEST_INGEST_FAIL_FOR` process-wide env
/// seam. Sets the var on construction and clears it on drop — even if
/// an assertion panics mid-test — so the var can never leak into a
/// subsequent serial test and force spurious failures. Pair with
/// `#[serial_test::serial]` on the test function.
struct IngestFailForGuard;

impl IngestFailForGuard {
    fn new(rcpt: &str) -> Self {
        // SAFETY: env mutation is process-wide; callers gate their
        // tests with `#[serial_test::serial]` so no other thread is
        // concurrently reading the var.
        unsafe {
            std::env::set_var("AIMX_TEST_INGEST_FAIL_FOR", rcpt);
        }
        Self
    }
}

impl Drop for IngestFailForGuard {
    fn drop(&mut self) {
        // SAFETY: same as above. Runs on normal exit AND on panic
        // unwind, which is the whole point of the guard.
        unsafe {
            std::env::remove_var("AIMX_TEST_INGEST_FAIL_FOR");
        }
    }
}

#[tokio::test]
#[serial_test::serial]
async fn test_partial_success_returns_451() {
    // `AIMX_TEST_INGEST_FAIL_FOR` is a process-wide seam. The RAII
    // guard clears it on drop even if an assertion panics mid-test,
    // so the var can never leak into the next serial test.
    let _fail_guard = IngestFailForGuard::new("bob@test.local");
    let tmp = tempfile::TempDir::new().unwrap();
    let config = test_config_two_mailboxes(tmp.path());
    let (port, _shutdown) = start_server(config).await;
    let mut client = TestClient::connect(port).await;
    client.read_line().await;

    client.send_and_read("EHLO client.example.com").await;
    client.send_and_read("MAIL FROM:<sender@example.com>").await;
    let resp = client.send_and_read("RCPT TO:<alice@test.local>").await;
    assert!(resp.starts_with("250"), "alice RCPT: {resp}");
    let resp = client.send_and_read("RCPT TO:<bob@test.local>").await;
    assert!(resp.starts_with("250"), "bob RCPT: {resp}");

    let resp = client.send_and_read("DATA").await;
    assert!(resp.starts_with("354"), "DATA preamble: {resp}");

    client.send(mixed_test_email()).await;
    let resp = client.send_and_read(".").await;
    assert!(
        resp.starts_with("451 4.3.0"),
        "Expected 451 4.3.0 on partial-success: {resp}"
    );

    client.send_and_read("QUIT").await;

    // alice succeeded, bob's ingest was forced to fail.
    let alice_mds = collect_md_files(&inbox(tmp.path(), "alice"));
    let bob_mds = collect_md_files(&inbox(tmp.path(), "bob"));
    assert_eq!(alice_mds.len(), 1, "alice should have one message");
    assert_eq!(bob_mds.len(), 0, "bob should have no message");
}

#[tokio::test]
#[serial_test::serial]
async fn test_multi_rcpt_all_success_returns_250() {
    // Defensive: if an earlier test somehow bypassed `IngestFailForGuard`
    // and left the seam set, clear it before we start.
    unsafe {
        std::env::remove_var("AIMX_TEST_INGEST_FAIL_FOR");
    }
    let tmp = tempfile::TempDir::new().unwrap();
    let config = test_config_two_mailboxes(tmp.path());
    let (port, _shutdown) = start_server(config).await;
    let mut client = TestClient::connect(port).await;
    client.read_line().await;

    client.send_and_read("EHLO client.example.com").await;
    client.send_and_read("MAIL FROM:<sender@example.com>").await;
    client.send_and_read("RCPT TO:<alice@test.local>").await;
    client.send_and_read("RCPT TO:<bob@test.local>").await;
    client.send_and_read("DATA").await;

    client.send(mixed_test_email()).await;
    let resp = client.send_and_read(".").await;
    assert!(
        resp.starts_with("250"),
        "Expected 250 on all-success: {resp}"
    );

    client.send_and_read("QUIT").await;

    let alice_mds = collect_md_files(&inbox(tmp.path(), "alice"));
    let bob_mds = collect_md_files(&inbox(tmp.path(), "bob"));
    assert_eq!(alice_mds.len(), 1);
    assert_eq!(bob_mds.len(), 1);
}
