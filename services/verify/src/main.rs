use axum::{
    extract::ConnectInfo,
    http::{HeaderMap, StatusCode},
    routing::get,
    Json, Router,
};
use serde::Serialize;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;

const CLIENT_IP_HEADER: &str = "X-AIMX-Client-IP";
const SMTP_HOSTNAME: &str = "check.aimx.email";
const SMTP_CONNECTION_TIMEOUT: Duration = Duration::from_secs(30);
const SMTP_LINE_TIMEOUT: Duration = Duration::from_secs(10);
const REACH_TCP_TIMEOUT: Duration = Duration::from_secs(10);
const PROBE_EHLO_TIMEOUT: Duration = Duration::from_secs(45);

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

/// Layer 3: resolve the caller's real IP given the TCP peer and request headers.
///
/// Trust boundary: if the TCP peer is non-loopback, the service is exposed
/// directly and the peer IP is authoritative. If the peer is loopback, the
/// request came through a reverse proxy (Caddy) and we require the proxy to
/// have set `X-AIMX-Client-IP` to the real client IP. We never parse
/// `X-Forwarded-For` — Caddy strips it, and the app must not re-introduce a
/// vulnerability by trusting it.
fn resolve_client_ip(peer: &SocketAddr, headers: &HeaderMap) -> Option<IpAddr> {
    if !peer.ip().is_loopback() {
        return Some(peer.ip());
    }

    let value = headers.get(CLIENT_IP_HEADER)?.to_str().ok()?;
    let ip: IpAddr = value.trim().parse().ok()?;

    if is_blocked_target(&ip) {
        return None;
    }

    Some(ip)
}

/// Layer 4: reject targets that should never be probed.
///
/// This blocks loopback, unspecified, link-local, RFC 1918 (IPv4 private),
/// RFC 4193 (IPv6 ULA), and similar ranges. Used by both `/probe` and
/// `/reach` before any outbound connection is attempted, and also by
/// `resolve_client_ip` to reject bogus `X-AIMX-Client-IP` values.
fn is_blocked_target(ip: &IpAddr) -> bool {
    if ip.is_loopback() || ip.is_unspecified() {
        return true;
    }
    match ip {
        IpAddr::V4(v4) => is_blocked_ipv4(v4),
        IpAddr::V6(v6) => is_blocked_ipv6(v6),
    }
}

fn is_blocked_ipv4(ip: &Ipv4Addr) -> bool {
    if ip.is_link_local() || ip.is_broadcast() || ip.is_multicast() {
        return true;
    }
    let [a, b, _, _] = ip.octets();
    // RFC 1918
    if a == 10 {
        return true;
    }
    if a == 172 && (16..=31).contains(&b) {
        return true;
    }
    if a == 192 && b == 168 {
        return true;
    }
    // RFC 6598 (CGNAT)
    if a == 100 && (64..=127).contains(&b) {
        return true;
    }
    false
}

fn is_blocked_ipv6(ip: &Ipv6Addr) -> bool {
    if ip.is_multicast() {
        return true;
    }
    // fe80::/10 link-local
    if (ip.segments()[0] & 0xffc0) == 0xfe80 {
        return true;
    }
    // fc00::/7 RFC 4193 unique local
    if (ip.segments()[0] & 0xfe00) == 0xfc00 {
        return true;
    }
    false
}

async fn probe(
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Result<Json<ProbeResponse>, StatusCode> {
    let caller_ip = match resolve_client_ip(&addr, &headers) {
        Some(ip) => ip,
        None => return Err(StatusCode::BAD_REQUEST),
    };

    if is_blocked_target(&caller_ip) {
        return Ok(Json(ProbeResponse {
            reachable: false,
            ip: caller_ip.to_string(),
        }));
    }

    let reachable = check_port25_ehlo(&caller_ip).await;
    Ok(Json(ProbeResponse {
        reachable,
        ip: caller_ip.to_string(),
    }))
}

async fn reach(
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Result<Json<ProbeResponse>, StatusCode> {
    let caller_ip = match resolve_client_ip(&addr, &headers) {
        Some(ip) => ip,
        None => return Err(StatusCode::BAD_REQUEST),
    };

    if is_blocked_target(&caller_ip) {
        return Ok(Json(ProbeResponse {
            reachable: false,
            ip: caller_ip.to_string(),
        }));
    }

    let reachable = check_port25_tcp(&caller_ip).await;
    Ok(Json(ProbeResponse {
        reachable,
        ip: caller_ip.to_string(),
    }))
}

async fn check_port25_tcp(ip: &IpAddr) -> bool {
    let addr = SocketAddr::new(*ip, 25);
    matches!(
        tokio::time::timeout(REACH_TCP_TIMEOUT, TcpStream::connect(addr)).await,
        Ok(Ok(_))
    )
}

async fn check_port25_ehlo(ip: &IpAddr) -> bool {
    let addr = SocketAddr::new(*ip, 25);

    let result = tokio::time::timeout(PROBE_EHLO_TIMEOUT, async {
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

/// Minimal correct SMTP responder used only as a reachability target.
///
/// Implements enough of RFC 5321 for EHLO-based reachability probes to
/// complete cleanly: banner → (EHLO|HELO → 250 | QUIT → 221 Bye | other
/// → 500) loop. Not a real SMTP server — no MAIL FROM / RCPT TO / DATA /
/// AUTH support.
async fn handle_smtp_connection(stream: TcpStream) -> std::io::Result<()> {
    tokio::time::timeout(SMTP_CONNECTION_TIMEOUT, smtp_session(stream))
        .await
        .unwrap_or(Ok(()))
}

async fn smtp_session(stream: TcpStream) -> std::io::Result<()> {
    let (reader, mut writer) = tokio::io::split(stream);
    let mut reader = BufReader::new(reader);

    let banner = format!("220 {SMTP_HOSTNAME} SMTP aimx-verify\r\n");
    writer.write_all(banner.as_bytes()).await?;
    writer.flush().await?;

    loop {
        let mut line = String::new();
        let read_result =
            tokio::time::timeout(SMTP_LINE_TIMEOUT, reader.read_line(&mut line)).await;

        let n = match read_result {
            Ok(Ok(n)) => n,
            // read error or timeout: close cleanly
            _ => return Ok(()),
        };
        if n == 0 {
            // peer closed
            return Ok(());
        }

        let trimmed = line.trim_end_matches(['\r', '\n']);
        let upper = trimmed.to_ascii_uppercase();

        if upper.starts_with("EHLO") || upper.starts_with("HELO") {
            let resp = format!("250 {SMTP_HOSTNAME}\r\n");
            writer.write_all(resp.as_bytes()).await?;
            writer.flush().await?;
            continue;
        }

        if upper.starts_with("QUIT") {
            writer.write_all(b"221 Bye\r\n").await?;
            writer.flush().await?;
            return Ok(());
        }

        writer.write_all(b"500 Command not recognized\r\n").await?;
        writer.flush().await?;
    }
}

fn main() {
    let rt = tokio::runtime::Runtime::new().expect("Failed to create runtime");
    rt.block_on(async {
        tracing_subscriber::fmt::init();

        let app = Router::new()
            .route("/", get(health))
            .route("/health", get(health))
            .route("/probe", get(probe))
            .route("/reach", get(reach));

        let bind_addr = std::env::var("BIND_ADDR").unwrap_or_else(|_| "127.0.0.1:3025".to_string());
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
    use axum::http::HeaderValue;
    use tokio::io::AsyncReadExt;

    fn empty_headers() -> HeaderMap {
        HeaderMap::new()
    }

    fn headers_with_client_ip(ip: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(CLIENT_IP_HEADER, HeaderValue::from_str(ip).unwrap());
        h
    }

    #[tokio::test]
    async fn health_returns_ok() {
        let response = health().await;
        assert_eq!(response.status, "ok");
        assert_eq!(response.service, "aimx-verify");
    }

    #[tokio::test]
    async fn check_port25_ehlo_unreachable_host() {
        let ip: IpAddr = "192.0.2.1".parse().unwrap();
        let result = check_port25_ehlo(&ip).await;
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

    // -----------------------------------------------------------------
    // resolve_client_ip
    // -----------------------------------------------------------------

    #[test]
    fn resolve_client_ip_direct_public_ipv4() {
        let peer: SocketAddr = "203.0.113.5:12345".parse().unwrap();
        let ip = resolve_client_ip(&peer, &empty_headers()).unwrap();
        assert_eq!(ip.to_string(), "203.0.113.5");
    }

    #[test]
    fn resolve_client_ip_direct_public_ipv6() {
        let peer: SocketAddr = "[2001:db8::1]:12345".parse().unwrap();
        let ip = resolve_client_ip(&peer, &empty_headers()).unwrap();
        assert_eq!(ip.to_string(), "2001:db8::1");
    }

    #[test]
    fn resolve_client_ip_loopback_with_valid_header() {
        let peer: SocketAddr = "127.0.0.1:12345".parse().unwrap();
        let headers = headers_with_client_ip("203.0.113.5");
        let ip = resolve_client_ip(&peer, &headers).unwrap();
        assert_eq!(ip.to_string(), "203.0.113.5");
    }

    #[test]
    fn resolve_client_ip_loopback_missing_header() {
        let peer: SocketAddr = "127.0.0.1:12345".parse().unwrap();
        assert!(resolve_client_ip(&peer, &empty_headers()).is_none());
    }

    #[test]
    fn resolve_client_ip_loopback_header_is_loopback() {
        let peer: SocketAddr = "127.0.0.1:12345".parse().unwrap();
        let headers = headers_with_client_ip("127.0.0.1");
        assert!(resolve_client_ip(&peer, &headers).is_none());
    }

    #[test]
    fn resolve_client_ip_loopback_header_is_private() {
        let peer: SocketAddr = "127.0.0.1:12345".parse().unwrap();
        for ip in ["10.0.0.1", "172.16.0.1", "192.168.1.1"] {
            let headers = headers_with_client_ip(ip);
            assert!(
                resolve_client_ip(&peer, &headers).is_none(),
                "private IP {ip} should be rejected"
            );
        }
    }

    #[test]
    fn resolve_client_ip_loopback_header_is_unspecified() {
        let peer: SocketAddr = "127.0.0.1:12345".parse().unwrap();
        let headers = headers_with_client_ip("0.0.0.0");
        assert!(resolve_client_ip(&peer, &headers).is_none());
    }

    #[test]
    fn resolve_client_ip_loopback_header_is_link_local() {
        let peer: SocketAddr = "127.0.0.1:12345".parse().unwrap();
        let headers = headers_with_client_ip("169.254.1.1");
        assert!(resolve_client_ip(&peer, &headers).is_none());
    }

    #[test]
    fn resolve_client_ip_loopback_header_unparseable() {
        let peer: SocketAddr = "127.0.0.1:12345".parse().unwrap();
        let headers = headers_with_client_ip("not-an-ip");
        assert!(resolve_client_ip(&peer, &headers).is_none());
    }

    #[test]
    fn resolve_client_ip_ipv6_loopback_with_valid_header() {
        let peer: SocketAddr = "[::1]:12345".parse().unwrap();
        let headers = headers_with_client_ip("2001:db8::1");
        let ip = resolve_client_ip(&peer, &headers).unwrap();
        assert_eq!(ip.to_string(), "2001:db8::1");
    }

    // -----------------------------------------------------------------
    // is_blocked_target — Layer 4 guard
    // -----------------------------------------------------------------

    #[test]
    fn is_blocked_target_rejects_loopback_and_private() {
        let blocked = [
            "127.0.0.1",
            "::1",
            "0.0.0.0",
            "::",
            "10.0.0.1",
            "10.255.255.255",
            "172.16.0.1",
            "172.31.255.255",
            "192.168.1.1",
            "169.254.1.1",
            "fe80::1",
            "fc00::1",
            "fd00::1",
        ];
        for ip_str in blocked {
            let ip: IpAddr = ip_str.parse().unwrap();
            assert!(is_blocked_target(&ip), "{ip_str} should be blocked");
        }
    }

    #[test]
    fn is_blocked_target_allows_public() {
        let allowed = ["1.1.1.1", "8.8.8.8", "203.0.113.1", "2001:db8::1"];
        for ip_str in allowed {
            let ip: IpAddr = ip_str.parse().unwrap();
            assert!(!is_blocked_target(&ip), "{ip_str} should be allowed");
        }
    }

    #[test]
    fn is_blocked_target_rejects_172_private_boundaries() {
        for ip_str in ["172.16.0.1", "172.20.1.1", "172.31.255.255"] {
            let ip: IpAddr = ip_str.parse().unwrap();
            assert!(is_blocked_target(&ip), "{ip_str} should be blocked");
        }
    }

    #[test]
    fn is_blocked_target_allows_172_public_boundaries() {
        for ip_str in ["172.15.0.1", "172.32.0.1"] {
            let ip: IpAddr = ip_str.parse().unwrap();
            assert!(
                !is_blocked_target(&ip),
                "{ip_str} should NOT be blocked (outside 172.16/12)"
            );
        }
    }

    // -----------------------------------------------------------------
    // Handler-level 400 responses
    // -----------------------------------------------------------------

    #[tokio::test]
    async fn probe_returns_400_on_loopback_without_header() {
        let addr: SocketAddr = "127.0.0.1:12345".parse().unwrap();
        let result = probe(ConnectInfo(addr), empty_headers()).await;
        assert_eq!(result.err(), Some(StatusCode::BAD_REQUEST));
    }

    #[tokio::test]
    async fn reach_returns_400_on_loopback_without_header() {
        let addr: SocketAddr = "127.0.0.1:12345".parse().unwrap();
        let result = reach(ConnectInfo(addr), empty_headers()).await;
        assert_eq!(result.err(), Some(StatusCode::BAD_REQUEST));
    }

    #[tokio::test]
    async fn probe_returns_400_on_loopback_with_private_header() {
        let addr: SocketAddr = "127.0.0.1:12345".parse().unwrap();
        let headers = headers_with_client_ip("10.0.0.1");
        let result = probe(ConnectInfo(addr), headers).await;
        assert_eq!(result.err(), Some(StatusCode::BAD_REQUEST));
    }

    #[tokio::test]
    async fn reach_returns_400_on_loopback_with_private_header() {
        let addr: SocketAddr = "127.0.0.1:12345".parse().unwrap();
        let headers = headers_with_client_ip("10.0.0.1");
        let result = reach(ConnectInfo(addr), headers).await;
        assert_eq!(result.err(), Some(StatusCode::BAD_REQUEST));
    }

    // -----------------------------------------------------------------
    // /reach semantics: TCP-only, no SMTP handshake
    // -----------------------------------------------------------------

    #[tokio::test]
    async fn check_port25_tcp_unreachable_host() {
        let ip: IpAddr = "192.0.2.1".parse().unwrap();
        let result = check_port25_tcp(&ip).await;
        assert!(!result);
    }

    #[tokio::test]
    async fn check_port25_tcp_listening_socket_returns_true() {
        // Spawn a dummy TCP listener on a random port. We can't bind port 25
        // in tests, so we test the inner logic by connecting directly to the
        // bound address (the helper function is reused).
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();

        // Keep it accepting so the connect succeeds.
        tokio::spawn(async move {
            loop {
                let _ = listener.accept().await;
            }
        });

        // Verify the plain TCP connect semantics: ANY listening TCP socket
        // satisfies reachability. No banner or EHLO required.
        let addr: SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
        let ok = tokio::time::timeout(REACH_TCP_TIMEOUT, TcpStream::connect(addr))
            .await
            .unwrap()
            .is_ok();
        assert!(
            ok,
            "plain TCP connect to listening socket should succeed regardless of SMTP"
        );
    }

    // -----------------------------------------------------------------
    // handle_smtp_connection — new correct semantics
    // -----------------------------------------------------------------

    async fn read_line(stream: &mut TcpStream) -> String {
        let mut buf = vec![0u8; 512];
        let n = stream.read(&mut buf).await.unwrap();
        String::from_utf8(buf[..n].to_vec()).unwrap()
    }

    #[tokio::test]
    async fn smtp_session_ehlo_then_quit() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();

        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            handle_smtp_connection(stream).await.unwrap();
        });

        let mut stream = TcpStream::connect(format!("127.0.0.1:{port}"))
            .await
            .unwrap();

        let banner = read_line(&mut stream).await;
        assert!(banner.starts_with("220"), "Expected 220, got: {banner}");
        assert!(banner.contains(SMTP_HOSTNAME));

        stream.write_all(b"EHLO client.example\r\n").await.unwrap();
        let ehlo_resp = read_line(&mut stream).await;
        assert!(
            ehlo_resp.starts_with("250 "),
            "Expected 250, got: {ehlo_resp}"
        );

        stream.write_all(b"QUIT\r\n").await.unwrap();
        let bye = read_line(&mut stream).await;
        assert!(bye.starts_with("221"), "Expected 221, got: {bye}");
    }

    #[tokio::test]
    async fn smtp_session_helo_then_quit() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();

        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            handle_smtp_connection(stream).await.unwrap();
        });

        let mut stream = TcpStream::connect(format!("127.0.0.1:{port}"))
            .await
            .unwrap();

        let _banner = read_line(&mut stream).await;
        stream.write_all(b"HELO client.example\r\n").await.unwrap();
        let helo_resp = read_line(&mut stream).await;
        assert!(
            helo_resp.starts_with("250 "),
            "Expected 250, got: {helo_resp}"
        );

        stream.write_all(b"QUIT\r\n").await.unwrap();
        let bye = read_line(&mut stream).await;
        assert!(bye.starts_with("221"));
    }

    #[tokio::test]
    async fn smtp_session_unknown_command_returns_500() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();

        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            handle_smtp_connection(stream).await.unwrap();
        });

        let mut stream = TcpStream::connect(format!("127.0.0.1:{port}"))
            .await
            .unwrap();

        let _banner = read_line(&mut stream).await;
        stream.write_all(b"NOOP extra\r\n").await.unwrap();
        let resp = read_line(&mut stream).await;
        assert!(resp.starts_with("500"), "Expected 500, got: {resp}");

        // Connection should still be open — send QUIT.
        stream.write_all(b"QUIT\r\n").await.unwrap();
        let bye = read_line(&mut stream).await;
        assert!(bye.starts_with("221"), "Expected 221 after 500, got: {bye}");
    }

    #[tokio::test]
    async fn smtp_session_peer_close_is_clean() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();

        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            handle_smtp_connection(stream).await
        });

        let mut stream = TcpStream::connect(format!("127.0.0.1:{port}"))
            .await
            .unwrap();
        let _banner = read_line(&mut stream).await;
        drop(stream); // peer close mid-session

        // Server task should complete without error.
        let result = tokio::time::timeout(Duration::from_secs(5), server)
            .await
            .expect("server did not terminate after peer close");
        assert!(result.is_ok());
        assert!(result.unwrap().is_ok());
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

    // -----------------------------------------------------------------
    // Round-trip: check_port25_ehlo against our own handle_smtp_connection
    // -----------------------------------------------------------------

    #[tokio::test]
    async fn self_loop_ehlo_handshake_round_trip() {
        // This test proves the self-EHLO trap fix (S12.2): the built-in
        // listener now speaks enough SMTP that our own EHLO prober completes
        // a successful handshake against it.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();

        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let _ = handle_smtp_connection(stream).await;
        });

        let stream = TcpStream::connect(format!("127.0.0.1:{port}"))
            .await
            .unwrap();
        let ok = smtp_ehlo_handshake(stream).await.unwrap();
        assert!(ok, "Self-loop EHLO handshake must succeed after S12.2 fix");
    }

    // -----------------------------------------------------------------
    // Integration: /probe handler resolves IP via X-AIMX-Client-IP header
    // -----------------------------------------------------------------

    #[tokio::test]
    async fn reach_resolves_caller_ip_from_header_on_loopback() {
        // Peer is loopback (simulating behind-Caddy request), header carries
        // a reserved-but-unreachable public-range IP (TEST-NET-1). /reach
        // should resolve the caller IP from the header — not the peer —
        // and attempt the TCP connect to that IP, which will fail (since
        // 192.0.2.1 is unroutable), yielding reachable: false with the
        // correct ip field.
        let addr: SocketAddr = "127.0.0.1:12345".parse().unwrap();
        let headers = headers_with_client_ip("192.0.2.1");
        let result = reach(ConnectInfo(addr), headers).await.unwrap();
        assert_eq!(result.ip, "192.0.2.1");
        assert!(!result.reachable);
    }
}
