use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;

use crate::config::{Config, MailboxConfig};
use crate::smtp::SmtpServer;

fn test_config(data_dir: &std::path::Path) -> Config {
    let mut mailboxes = HashMap::new();
    mailboxes.insert(
        "catchall".to_string(),
        MailboxConfig {
            address: "*@test.local".to_string(),
            on_receive: vec![],
            trust: "none".to_string(),
            trusted_senders: vec![],
        },
    );
    mailboxes.insert(
        "alice".to_string(),
        MailboxConfig {
            address: "alice@test.local".to_string(),
            on_receive: vec![],
            trust: "none".to_string(),
            trusted_senders: vec![],
        },
    );
    Config {
        domain: "test.local".to_string(),
        data_dir: data_dir.to_path_buf(),
        dkim_selector: "dkim".to_string(),
        mailboxes,
        verify_host: None,
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

// --- S19.1: SMTP state machine tests ---

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

    let alice_dir = tmp.path().join("alice");
    assert!(alice_dir.exists());
    let entries: Vec<_> = std::fs::read_dir(&alice_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|ext| ext == "md"))
        .collect();
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

    let alice_mds: Vec<_> = std::fs::read_dir(tmp.path().join("alice"))
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|ext| ext == "md"))
        .collect();
    let catchall_mds: Vec<_> = std::fs::read_dir(tmp.path().join("catchall"))
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|ext| ext == "md"))
        .collect();
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

// --- S19.1: Size limit tests ---

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

// --- S19.1: Timeout tests ---

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

// --- S19.2: STARTTLS tests ---

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

// --- S19.3: Ingest integration tests ---

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

    let alice_dir = tmp.path().join("alice");
    let md_files: Vec<_> = std::fs::read_dir(&alice_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|ext| ext == "md"))
        .collect();
    assert_eq!(md_files.len(), 1);

    let content = std::fs::read_to_string(md_files[0].path()).unwrap();
    assert!(content.contains("+++"));
    assert!(content.contains("subject = \"Test\""));
    assert!(content.contains("Hello World"));
}

#[tokio::test]
async fn test_ingest_failure_returns_451() {
    let tmp = tempfile::TempDir::new().unwrap();
    let config = Config {
        domain: "test.local".to_string(),
        data_dir: PathBuf::from("/nonexistent/path/that/does/not/exist"),
        dkim_selector: "dkim".to_string(),
        mailboxes: HashMap::new(),
        verify_host: None,
    };
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
        resp.starts_with("451"),
        "Expected 451 on ingest failure: {resp}"
    );

    drop(tmp);
}

// --- S19.4: Connection hardening tests ---

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

    let alice_dir = tmp.path().join("alice");
    let md_files: Vec<_> = std::fs::read_dir(&alice_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|ext| ext == "md"))
        .collect();
    assert_eq!(md_files.len(), 2);
}

use std::sync::Arc;
