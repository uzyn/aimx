use axum::{extract::ConnectInfo, routing::get, Json, Router};
use serde::Serialize;
use std::net::SocketAddr;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;

#[derive(Serialize)]
struct ProbeResponse {
    reachable: bool,
    ip: String,
}

#[derive(Serialize)]
struct HealthResponse {
    status: String,
    service: String,
}

async fn health() -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "ok".to_string(),
        service: "aimx-verify".to_string(),
    })
}

async fn probe(ConnectInfo(addr): ConnectInfo<SocketAddr>) -> Json<ProbeResponse> {
    let target_ip = addr.ip().to_string();
    let reachable = check_port25_ehlo(&target_ip).await;

    Json(ProbeResponse {
        reachable,
        ip: target_ip,
    })
}

async fn check_port25_ehlo(ip: &str) -> bool {
    let addr = format!("{ip}:25");

    let result = tokio::time::timeout(std::time::Duration::from_secs(45), async {
        let stream = TcpStream::connect(&addr).await?;
        smtp_ehlo_handshake(stream).await
    })
    .await;

    matches!(result, Ok(Ok(true)))
}

async fn smtp_ehlo_handshake(stream: TcpStream) -> std::io::Result<bool> {
    let (reader, mut writer) = tokio::io::split(stream);
    let mut reader = BufReader::new(reader);

    // Read 220 banner
    let mut banner = String::new();
    reader.read_line(&mut banner).await?;
    if !banner.starts_with("220") {
        return Ok(false);
    }

    // Send EHLO
    writer.write_all(b"EHLO check.aimx.email\r\n").await?;
    writer.flush().await?;

    // Read EHLO response (may be multiline: "250-..." then "250 ...")
    loop {
        let mut line = String::new();
        let n = reader.read_line(&mut line).await?;
        if n == 0 {
            return Ok(false);
        }
        if line.starts_with("250 ") {
            break;
        }
        if !line.starts_with("250-") {
            return Ok(false);
        }
    }

    // Send QUIT
    writer.write_all(b"QUIT\r\n").await?;
    writer.flush().await?;

    // Read 221 (best effort, don't fail if missing)
    let mut quit_resp = String::new();
    let _ = reader.read_line(&mut quit_resp).await;

    Ok(true)
}

const SMTP_BANNER: &[u8] = b"220 check.aimx.email SMTP aimx-verify\r\n";
const SMTP_BYE: &[u8] = b"221 Bye\r\n";

async fn run_smtp_listener() {
    let bind_addr = std::env::var("SMTP_BIND_ADDR").unwrap_or_else(|_| "0.0.0.0:25".to_string());

    let listener = tokio::net::TcpListener::bind(&bind_addr)
        .await
        .expect("Failed to bind SMTP listener");

    tracing::info!("SMTP listener on {bind_addr}");

    loop {
        match listener.accept().await {
            Ok((stream, peer)) => {
                tokio::spawn(async move {
                    if let Err(e) = handle_smtp_connection(stream).await {
                        tracing::debug!("SMTP connection from {peer} error: {e}");
                    }
                });
            }
            Err(e) => {
                tracing::warn!("SMTP accept error: {e}");
            }
        }
    }
}

async fn handle_smtp_connection(mut stream: TcpStream) -> std::io::Result<()> {
    stream.write_all(SMTP_BANNER).await?;
    stream.flush().await?;

    // Wait for any input or timeout after 10 seconds
    let mut buf = [0u8; 512];
    let _ = tokio::time::timeout(std::time::Duration::from_secs(10), stream.read(&mut buf)).await;

    stream.write_all(SMTP_BYE).await?;
    stream.flush().await?;

    Ok(())
}

fn main() {
    let rt = tokio::runtime::Runtime::new().expect("Failed to create runtime");
    rt.block_on(async {
        tracing_subscriber::fmt::init();

        let app = Router::new()
            .route("/", get(health))
            .route("/health", get(health))
            .route("/probe", get(probe));

        let bind_addr = std::env::var("BIND_ADDR").unwrap_or_else(|_| "0.0.0.0:3025".to_string());
        let listener = tokio::net::TcpListener::bind(&bind_addr)
            .await
            .expect("Failed to bind HTTP listener");

        tracing::info!("aimx-verify HTTP listening on {bind_addr}");

        // Spawn SMTP listener concurrently
        tokio::spawn(run_smtp_listener());

        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .expect("Server error");
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncReadExt;

    #[tokio::test]
    async fn health_returns_ok() {
        let response = health().await;
        assert_eq!(response.status, "ok");
        assert_eq!(response.service, "aimx-verify");
    }

    #[tokio::test]
    async fn check_port25_ehlo_unreachable_host() {
        let result = check_port25_ehlo("192.0.2.1").await;
        assert!(!result);
    }

    #[test]
    fn probe_response_serializes() {
        let resp = ProbeResponse {
            reachable: true,
            ip: "1.2.3.4".to_string(),
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("\"reachable\":true"));
        assert!(json.contains("\"ip\":\"1.2.3.4\""));
    }

    #[test]
    fn probe_response_false_serializes() {
        let resp = ProbeResponse {
            reachable: false,
            ip: "5.6.7.8".to_string(),
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("\"reachable\":false"));
    }

    #[test]
    fn health_response_serializes() {
        let resp = HealthResponse {
            status: "ok".to_string(),
            service: "aimx-verify".to_string(),
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("\"status\":\"ok\""));
        assert!(json.contains("\"service\":\"aimx-verify\""));
    }

    #[test]
    fn smtp_banner_format() {
        let banner = std::str::from_utf8(SMTP_BANNER).unwrap();
        assert!(banner.starts_with("220"));
        assert!(banner.contains("check.aimx.email"));
        assert!(banner.ends_with("\r\n"));
    }

    #[test]
    fn smtp_bye_format() {
        let bye = std::str::from_utf8(SMTP_BYE).unwrap();
        assert!(bye.starts_with("221"));
        assert!(bye.ends_with("\r\n"));
    }

    #[tokio::test]
    async fn smtp_listener_sends_banner_and_bye() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();

        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            handle_smtp_connection(stream).await.unwrap();
        });

        let mut stream = TcpStream::connect(format!("127.0.0.1:{port}"))
            .await
            .unwrap();

        // Read banner
        let mut buf = vec![0u8; 256];
        let n = stream.read(&mut buf).await.unwrap();
        let banner = std::str::from_utf8(&buf[..n]).unwrap();
        assert!(
            banner.starts_with("220"),
            "Expected 220 banner, got: {banner}"
        );
        assert!(banner.contains("check.aimx.email"));

        // Send something (like EHLO)
        stream.write_all(b"EHLO test\r\n").await.unwrap();

        // Read 221 Bye
        let mut buf2 = vec![0u8; 256];
        let n2 = stream.read(&mut buf2).await.unwrap();
        let bye = std::str::from_utf8(&buf2[..n2]).unwrap();
        assert!(bye.starts_with("221"), "Expected 221 Bye, got: {bye}");
    }

    #[tokio::test]
    async fn smtp_ehlo_handshake_valid_server() {
        // Mock a minimal SMTP server
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();

        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            stream
                .write_all(b"220 mock.server ESMTP\r\n")
                .await
                .unwrap();
            stream.flush().await.unwrap();

            let mut buf = vec![0u8; 256];
            let n = stream.read(&mut buf).await.unwrap();
            let cmd = std::str::from_utf8(&buf[..n]).unwrap();
            assert!(cmd.starts_with("EHLO"));

            stream.write_all(b"250 mock.server\r\n").await.unwrap();
            stream.flush().await.unwrap();

            let mut buf2 = vec![0u8; 256];
            let _ = stream.read(&mut buf2).await;
            stream.write_all(b"221 Bye\r\n").await.unwrap();
            stream.flush().await.unwrap();
        });

        // check_port25_ehlo connects to port 25 which differs from our mock,
        // so we test the handshake function directly
        let stream = TcpStream::connect(format!("127.0.0.1:{port}"))
            .await
            .unwrap();
        let ok = smtp_ehlo_handshake(stream).await.unwrap();
        assert!(ok, "Valid SMTP handshake should return true");
    }

    #[tokio::test]
    async fn smtp_ehlo_handshake_no_banner() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();

        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            // Send non-220 banner
            stream
                .write_all(b"421 Service not available\r\n")
                .await
                .unwrap();
            stream.flush().await.unwrap();
        });

        let stream = TcpStream::connect(format!("127.0.0.1:{port}"))
            .await
            .unwrap();
        let ok = smtp_ehlo_handshake(stream).await.unwrap();
        assert!(!ok, "Non-220 banner should return false");
    }

    #[tokio::test]
    async fn smtp_ehlo_handshake_ehlo_rejected() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();

        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            stream
                .write_all(b"220 mock.server ESMTP\r\n")
                .await
                .unwrap();
            stream.flush().await.unwrap();

            let mut buf = vec![0u8; 256];
            let _ = stream.read(&mut buf).await;

            // Reject EHLO
            stream.write_all(b"550 Access denied\r\n").await.unwrap();
            stream.flush().await.unwrap();
        });

        let stream = TcpStream::connect(format!("127.0.0.1:{port}"))
            .await
            .unwrap();
        let ok = smtp_ehlo_handshake(stream).await.unwrap();
        assert!(!ok, "Rejected EHLO should return false");
    }

    #[tokio::test]
    async fn smtp_ehlo_handshake_multiline_250() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();

        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            stream
                .write_all(b"220 mock.server ESMTP\r\n")
                .await
                .unwrap();
            stream.flush().await.unwrap();

            let mut buf = vec![0u8; 256];
            let _ = stream.read(&mut buf).await;

            // Multiline EHLO response
            stream
                .write_all(b"250-mock.server\r\n250-SIZE 10240000\r\n250 STARTTLS\r\n")
                .await
                .unwrap();
            stream.flush().await.unwrap();

            let mut buf2 = vec![0u8; 256];
            let _ = stream.read(&mut buf2).await;
            stream.write_all(b"221 Bye\r\n").await.unwrap();
            stream.flush().await.unwrap();
        });

        let stream = TcpStream::connect(format!("127.0.0.1:{port}"))
            .await
            .unwrap();
        let ok = smtp_ehlo_handshake(stream).await.unwrap();
        assert!(ok, "Multiline 250 response should return true");
    }

    #[tokio::test]
    async fn probe_uses_caller_ip() {
        let addr = "127.0.0.1:12345".parse::<SocketAddr>().unwrap();
        let response = probe(ConnectInfo(addr)).await;
        assert_eq!(response.ip, "127.0.0.1");
    }
}
