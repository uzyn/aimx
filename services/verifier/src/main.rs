use axum::{
    extract::{ConnectInfo, Request},
    http::{HeaderMap, StatusCode},
    middleware::{self, Next},
    response::Response,
    routing::get,
    Json, Router,
};
use serde::Serialize;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

const CLIENT_IP_HEADER: &str = "X-AIMX-Client-IP";
const SMTP_HOSTNAME: &str = "check.aimx.email";
const SMTP_CONNECTION_TIMEOUT: Duration = Duration::from_secs(30);
const SMTP_LINE_TIMEOUT: Duration = Duration::from_secs(10);
/// Hard cap on bytes consumed per SMTP request line. RFC 5321 sets the
/// maximum command line length to 512 octets; 1024 leaves generous headroom
/// for oversized but still-well-intentioned clients and closes off a trivial
/// DoS where a peer streams megabytes into a growing `String` buffer before
/// the per-line timeout fires.
const SMTP_MAX_LINE_BYTES: u64 = 1024;
/// Default cap on concurrent SMTP connections. Each per-connection session is
/// already tightly bounded (30s wall clock, 10s per-line, 1 KiB per-line), so
/// this is defense-in-depth against a distributed flood exhausting the tokio
/// runtime or file descriptors. Tunable via `SMTP_MAX_CONCURRENT` env var.
const SMTP_DEFAULT_MAX_CONCURRENT: usize = 128;
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
        service: "aimx-verifier".to_string(),
    })
}

/// Collapse an IPv4-mapped IPv6 address (`::ffff:a.b.c.d`) to its underlying
/// IPv4 form so that the Layer 4 guards and trust-boundary checks see one
/// canonical representation. Linux dual-stack sockets will silently downgrade
/// `TcpStream::connect("[::ffff:127.0.0.1]:port")` to an IPv4 loopback
/// connection, so without this canonicalization every mapped form
/// (`::ffff:127.0.0.1`, `::ffff:10.0.0.1`, `::ffff:169.254.169.254`,
/// `::ffff:0.0.0.0`, …) would bypass the guards below.
fn canonicalize_ip(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => IpAddr::V4(v4),
            None => IpAddr::V6(v6),
        },
        v4 => v4,
    }
}

/// Layer 3: resolve the caller's real IP given the TCP peer and request headers.
///
/// Trust boundary: if the TCP peer is non-loopback, the service is exposed
/// directly and the peer IP is authoritative. If the peer is loopback, the
/// request came through a reverse proxy (Caddy) and we require the proxy to
/// have set `X-AIMX-Client-IP` to the real client IP. We never parse
/// `X-Forwarded-For`. Caddy strips it, and the app must not re-introduce a
/// vulnerability by trusting it.
fn resolve_client_ip(peer: &SocketAddr, headers: &HeaderMap) -> Option<IpAddr> {
    let peer_ip = canonicalize_ip(peer.ip());
    if !peer_ip.is_loopback() {
        return Some(peer_ip);
    }

    let value = headers.get(CLIENT_IP_HEADER)?.to_str().ok()?;
    let parsed: IpAddr = value.trim().parse().ok()?;
    let ip = canonicalize_ip(parsed);

    if is_blocked_target(&ip) {
        return None;
    }

    Some(ip)
}

/// Layer 4: reject targets that should never be probed.
///
/// This blocks loopback, unspecified, link-local, RFC 1918 (IPv4 private),
/// RFC 4193 (IPv6 ULA), and similar ranges. Used by `/probe` before any
/// outbound connection is attempted, and also by `resolve_client_ip` to
/// reject bogus `X-AIMX-Client-IP` values.
///
/// IPv4-mapped IPv6 addresses are canonicalized to their IPv4 form before
/// the checks run, so `::ffff:127.0.0.1` and similar are blocked as if the
/// caller had supplied `127.0.0.1` directly.
fn is_blocked_target(ip: &IpAddr) -> bool {
    let ip = canonicalize_ip(*ip);
    if ip.is_loopback() || ip.is_unspecified() {
        return true;
    }
    match ip {
        IpAddr::V4(v4) => is_blocked_ipv4(&v4),
        IpAddr::V6(v6) => is_blocked_ipv6(&v6),
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

/// Extension handlers insert into their response so the logging middleware
/// can include the probe `reachable` outcome in the per-request log line.
/// Keeping the middleware as the single source of truth for request logs
/// avoids duplicated log lines per request.
#[derive(Clone, Copy)]
struct ReachableOutcome(bool);

/// Per-request logging middleware.
///
/// Emits exactly one `info!` line per HTTP request, containing the method,
/// path, resolved caller IP (or `unknown` if the Layer 3 trust check
/// rejected the request before the handler ran), response status, elapsed
/// ms, and (for `/probe`) the reachable outcome recorded by the handler
/// via `ReachableOutcome`.
async fn log_request(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    req: Request,
    next: Next,
) -> Response {
    let start = Instant::now();
    let method = req.method().clone();
    let path = req
        .uri()
        .path_and_query()
        .map(|p| p.as_str().to_string())
        .unwrap_or_else(|| req.uri().path().to_string());
    let caller_ip = resolve_client_ip(&peer, req.headers())
        .map(|ip| ip.to_string())
        .unwrap_or_else(|| "unknown".to_string());

    let response = next.run(req).await;

    let status = response.status().as_u16();
    let elapsed_ms = start.elapsed().as_millis();
    let reachable = response.extensions().get::<ReachableOutcome>().map(|o| o.0);

    match reachable {
        Some(r) => tracing::info!(
            method = %method,
            path = %path,
            caller_ip = %caller_ip,
            status = status,
            elapsed_ms = elapsed_ms,
            reachable = r,
            "http request"
        ),
        None => tracing::info!(
            method = %method,
            path = %path,
            caller_ip = %caller_ip,
            status = status,
            elapsed_ms = elapsed_ms,
            "http request"
        ),
    }

    response
}

async fn probe(
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Result<ProbeBody, StatusCode> {
    let caller_ip = match resolve_client_ip(&addr, &headers) {
        Some(ip) => ip,
        None => return Err(StatusCode::BAD_REQUEST),
    };

    if is_blocked_target(&caller_ip) {
        return Ok(ProbeBody(ProbeResponse {
            reachable: false,
            ip: caller_ip.to_string(),
        }));
    }

    let reachable = check_port25_ehlo(&caller_ip).await;
    Ok(ProbeBody(ProbeResponse {
        reachable,
        ip: caller_ip.to_string(),
    }))
}

/// Newtype wrapper that converts to a `Json` response and attaches a
/// `ReachableOutcome` extension so the logging middleware can record the
/// probe result alongside the standard request metadata.
struct ProbeBody(ProbeResponse);

impl axum::response::IntoResponse for ProbeBody {
    fn into_response(self) -> Response {
        let reachable = self.0.reachable;
        let mut response = Json(self.0).into_response();
        response
            .extensions_mut()
            .insert(ReachableOutcome(reachable));
        response
    }
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

    let max_concurrent = std::env::var("SMTP_MAX_CONCURRENT")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|&n| n > 0)
        .unwrap_or(SMTP_DEFAULT_MAX_CONCURRENT);
    let gate = Arc::new(Semaphore::new(max_concurrent));

    tracing::info!("SMTP listener on {bind_addr} (max_concurrent={max_concurrent})");

    loop {
        match listener.accept().await {
            Ok((stream, peer)) => {
                dispatch_smtp_connection(stream, peer, Arc::clone(&gate), max_concurrent);
            }
            Err(e) => {
                tracing::warn!("SMTP accept error: {e}");
            }
        }
    }
}

/// Non-blocking gate: if all permits are in use, drop the new connection
/// cleanly rather than blocking the accept loop. The OS TCP backlog absorbs
/// short bursts; sustained floods see a fast close without runtime
/// exhaustion. Returns `true` when a task was spawned, `false` when the
/// connection was dropped.
fn dispatch_smtp_connection(
    stream: TcpStream,
    peer: SocketAddr,
    gate: Arc<Semaphore>,
    max_concurrent: usize,
) -> bool {
    match gate.try_acquire_owned() {
        Ok(permit) => {
            tokio::spawn(spawn_smtp_connection(stream, peer, permit));
            true
        }
        Err(_) => {
            tracing::warn!(
                peer_ip = %peer.ip(),
                max_concurrent,
                "smtp connection dropped: concurrency gate saturated"
            );
            drop(stream);
            false
        }
    }
}

/// Per-connection body shared by the real accept loop and the logging test
/// so the test exercises the exact production code path instead of an
/// inlined copy that could drift.
///
/// The `_permit` argument holds a slot in the concurrency gate for the
/// lifetime of this task; dropping it when the task returns releases the slot.
async fn spawn_smtp_connection(stream: TcpStream, peer: SocketAddr, _permit: OwnedSemaphorePermit) {
    let start = Instant::now();
    let peer_ip = peer.ip();
    tracing::info!(peer_ip = %peer_ip, "smtp connection accepted");
    match handle_smtp_connection(stream).await {
        Ok(()) => tracing::info!(
            peer_ip = %peer_ip,
            elapsed_ms = start.elapsed().as_millis(),
            "smtp connection closed cleanly"
        ),
        Err(e) => tracing::warn!(
            peer_ip = %peer_ip,
            elapsed_ms = start.elapsed().as_millis(),
            error = %e,
            "smtp connection closed with error"
        ),
    }
}

/// Minimal correct SMTP responder used only as a reachability target.
///
/// Implements enough of RFC 5321 for EHLO-based reachability probes to
/// complete cleanly: banner, then (EHLO|HELO => 250 | QUIT => 221 Bye |
/// other => 500) loop. Not a real SMTP server. No MAIL FROM / RCPT TO /
/// DATA / AUTH support.
async fn handle_smtp_connection(stream: TcpStream) -> std::io::Result<()> {
    tokio::time::timeout(SMTP_CONNECTION_TIMEOUT, smtp_session(stream))
        .await
        .unwrap_or(Ok(()))
}

async fn smtp_session<S>(stream: S) -> std::io::Result<()>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite,
{
    let (reader, mut writer) = tokio::io::split(stream);
    let mut reader = BufReader::new(reader);

    let banner = format!("220 {SMTP_HOSTNAME} SMTP aimx-verifier\r\n");
    writer.write_all(banner.as_bytes()).await?;
    writer.flush().await?;

    loop {
        let mut line = Vec::new();
        // Cap per-line reads so a misbehaving peer cannot stream unbounded
        // data into our buffer before the per-line timeout trips.
        let mut limited = (&mut reader).take(SMTP_MAX_LINE_BYTES);
        let read_result =
            tokio::time::timeout(SMTP_LINE_TIMEOUT, limited.read_until(b'\n', &mut line)).await;

        let n = match read_result {
            Ok(Ok(n)) => n,
            // read error or timeout: close cleanly
            _ => return Ok(()),
        };
        if n == 0 {
            // peer closed
            return Ok(());
        }

        let text = std::str::from_utf8(&line).unwrap_or("");
        let trimmed = text.trim_end_matches(['\r', '\n']);
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
            .layer(middleware::from_fn(log_request));

        let bind_addr = std::env::var("BIND_ADDR").unwrap_or_else(|_| "127.0.0.1:3025".to_string());
        let listener = tokio::net::TcpListener::bind(&bind_addr)
            .await
            .expect("Failed to bind HTTP listener");

        tracing::info!("aimx-verifier HTTP listening on {bind_addr}");

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
        assert_eq!(response.service, "aimx-verifier");
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
            service: "aimx-verifier".to_string(),
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("\"status\":\"ok\""));
        assert!(json.contains("\"service\":\"aimx-verifier\""));
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
    // is_blocked_target: Layer 4 guard
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
    async fn probe_returns_400_on_loopback_with_private_header() {
        let addr: SocketAddr = "127.0.0.1:12345".parse().unwrap();
        let headers = headers_with_client_ip("10.0.0.1");
        let result = probe(ConnectInfo(addr), headers).await;
        assert_eq!(result.err(), Some(StatusCode::BAD_REQUEST));
    }

    // -----------------------------------------------------------------
    // handle_smtp_connection
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

        // Connection should still be open. Send QUIT.
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
    // Integration: /probe handler resolves IP via header
    // -----------------------------------------------------------------

    #[tokio::test]
    async fn probe_resolves_caller_ip_from_header_on_loopback() {
        // Peer is loopback (simulating behind-Caddy request), header carries
        // a reserved-but-unreachable public-range IP (TEST-NET-1). /probe
        // should resolve the caller IP from the header -- not the peer --
        // and attempt the EHLO handshake to that IP, which will fail (since
        // 192.0.2.1 is unroutable), yielding reachable: false with the
        // correct ip field.
        let addr: SocketAddr = "127.0.0.1:12345".parse().unwrap();
        let headers = headers_with_client_ip("192.0.2.1");
        let result = probe(ConnectInfo(addr), headers).await.unwrap();
        assert_eq!(result.0.ip, "192.0.2.1");
        assert!(!result.0.reachable);
    }

    // -----------------------------------------------------------------
    // IPv4-mapped IPv6 bypass regression tests
    // -----------------------------------------------------------------

    #[test]
    fn is_blocked_target_rejects_ipv4_mapped_ipv6() {
        // Rust's `Ipv6Addr::is_loopback`/`is_unspecified` only match the
        // canonical v6 forms, and our link-local/ULA/multicast checks likewise
        // only match canonical v6 segments. Without canonicalization, every
        // `::ffff:a.b.c.d` form would sneak past the guard and Linux dual-stack
        // sockets would silently downgrade to the underlying v4 address. These
        // are the exact payloads flagged in the PR review.
        let blocked = [
            "::ffff:127.0.0.1",
            "::ffff:10.0.0.1",
            "::ffff:10.255.255.255",
            "::ffff:172.16.0.1",
            "::ffff:192.168.1.1",
            "::ffff:169.254.1.1",
            "::ffff:169.254.169.254", // cloud instance metadata
            "::ffff:0.0.0.0",
            "::ffff:100.64.0.1", // CGNAT
        ];
        for ip_str in blocked {
            let ip: IpAddr = ip_str.parse().unwrap();
            assert!(is_blocked_target(&ip), "{ip_str} should be blocked");
        }
    }

    #[test]
    fn is_blocked_target_allows_ipv4_mapped_public() {
        // Canonicalization must not over-block: mapped public IPv4 addresses
        // should pass through, since the underlying v4 form is allowed.
        let allowed = ["::ffff:1.1.1.1", "::ffff:8.8.8.8", "::ffff:203.0.113.1"];
        for ip_str in allowed {
            let ip: IpAddr = ip_str.parse().unwrap();
            assert!(!is_blocked_target(&ip), "{ip_str} should be allowed");
        }
    }

    #[test]
    fn resolve_client_ip_rejects_ipv4_mapped_loopback_header() {
        // An attacker-controlled header carrying a mapped loopback must be
        // rejected by `resolve_client_ip`, not passed through to the handler.
        let peer: SocketAddr = "127.0.0.1:12345".parse().unwrap();
        for ip_str in [
            "::ffff:127.0.0.1",
            "::ffff:10.0.0.1",
            "::ffff:169.254.169.254",
            "::ffff:0.0.0.0",
        ] {
            let headers = headers_with_client_ip(ip_str);
            assert!(
                resolve_client_ip(&peer, &headers).is_none(),
                "mapped header {ip_str} should be rejected"
            );
        }
    }

    #[test]
    fn resolve_client_ip_treats_ipv4_mapped_loopback_peer_as_loopback() {
        // Dual-stack bind (`[::]:3025`) makes an IPv4 loopback client show up
        // with `peer.ip() == ::ffff:127.0.0.1`. That must be treated as a
        // loopback peer (i.e. require a trusted header), not as a "direct"
        // caller that the handler then probes.
        let peer: SocketAddr = "[::ffff:127.0.0.1]:12345".parse().unwrap();
        assert!(resolve_client_ip(&peer, &empty_headers()).is_none());
    }

    #[test]
    fn resolve_client_ip_canonicalizes_ipv4_mapped_public_header() {
        // A mapped public v4 address in the header should be canonicalized
        // to its v4 form before being returned, so downstream logging and
        // target-guard checks see one representation.
        let peer: SocketAddr = "127.0.0.1:12345".parse().unwrap();
        let headers = headers_with_client_ip("::ffff:203.0.113.5");
        let ip = resolve_client_ip(&peer, &headers).unwrap();
        assert_eq!(ip.to_string(), "203.0.113.5");
    }

    // -----------------------------------------------------------------
    // SMTP session: idle timeout and oversized-line hardening
    // -----------------------------------------------------------------

    #[tokio::test(start_paused = true)]
    async fn smtp_session_idle_times_out() {
        // Prove that a silent client hits SMTP_LINE_TIMEOUT and the session
        // closes cleanly without the tests actually waiting 10 seconds.
        // `start_paused` auto-advances virtual time when all tasks are idle,
        // so the timeout inside `smtp_session` fires immediately. We use an
        // in-memory duplex pipe instead of a real TCP socket specifically
        // so the runtime's virtual clock applies to the timeout.
        let (server_side, mut client_side) = tokio::io::duplex(1024);

        let server = tokio::spawn(async move { smtp_session(server_side).await });

        // Consume the 220 banner so the server is blocked in `read_line`.
        let mut banner = vec![0u8; 128];
        let n = client_side.read(&mut banner).await.unwrap();
        assert!(
            std::str::from_utf8(&banner[..n])
                .unwrap()
                .starts_with("220"),
            "expected banner, got: {:?}",
            &banner[..n]
        );
        // Stay silent. The session must give up after SMTP_LINE_TIMEOUT and
        // return Ok(()). With start_paused + auto-advance this completes
        // in virtual time.

        let result = server.await.expect("server task panicked");
        assert!(
            result.is_ok(),
            "idle-timeout close should be reported as clean"
        );
    }

    #[tokio::test]
    async fn smtp_session_oversized_line_is_bounded() {
        // Send a >2 KiB single-line "command" with no CRLF terminator. The
        // take-limited reader should return at SMTP_MAX_LINE_BYTES without
        // blowing up the session's memory. Once we then send a real QUIT,
        // the session must still respond.
        //
        // Note: because the oversized chunk contains no newline, read_until
        // will keep reading until it hits the cap. With the cap in place,
        // the session continues with a well-formed "unknown command" reply
        // to the truncated buffer (likely `500`), then handles QUIT normally.
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

        // ~2 KiB of junk, no terminator. The cap is 1024 bytes, so the
        // server reads only the first 1024 and treats it as one "line".
        let junk = vec![b'A'; 2048];
        stream.write_all(&junk).await.unwrap();
        // Terminate the chunk so the cap-bounded read completes.
        stream.write_all(b"\r\nQUIT\r\n").await.unwrap();

        // We expect at least one response line (500) followed by 221 Bye.
        // Read generously and assert both are present.
        let mut buf = vec![0u8; 1024];
        let n = stream.read(&mut buf).await.unwrap();
        let first = String::from_utf8_lossy(&buf[..n]).to_string();
        assert!(
            first.starts_with("500") || first.contains("221"),
            "unexpected first response after oversized line: {first}"
        );
        if !first.contains("221") {
            let n2 = stream.read(&mut buf).await.unwrap();
            let second = String::from_utf8_lossy(&buf[..n2]).to_string();
            assert!(
                second.starts_with("221"),
                "expected 221 Bye after QUIT, got: {second}"
            );
        }
    }

    // -----------------------------------------------------------------
    // Request logging
    // -----------------------------------------------------------------

    use std::io;
    use std::sync::{Arc, Mutex};
    use tracing_subscriber::fmt::MakeWriter;

    /// Shared buffer test writer: every tracing event writes a copy of its
    /// bytes into an `Arc<Mutex<Vec<u8>>>` that the test can inspect.
    #[derive(Clone)]
    struct BufWriter {
        buf: Arc<Mutex<Vec<u8>>>,
    }

    impl BufWriter {
        fn new() -> Self {
            Self {
                buf: Arc::new(Mutex::new(Vec::new())),
            }
        }

        fn contents(&self) -> String {
            String::from_utf8_lossy(&self.buf.lock().unwrap()).to_string()
        }
    }

    impl io::Write for BufWriter {
        fn write(&mut self, data: &[u8]) -> io::Result<usize> {
            self.buf.lock().unwrap().extend_from_slice(data);
            Ok(data.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl<'a> MakeWriter<'a> for BufWriter {
        type Writer = BufWriter;

        fn make_writer(&'a self) -> Self::Writer {
            self.clone()
        }
    }

    fn test_subscriber(writer: BufWriter) -> tracing::subscriber::DefaultGuard {
        let subscriber = tracing_subscriber::fmt()
            .with_writer(writer)
            .with_ansi(false)
            .with_max_level(tracing::Level::INFO)
            .finish();
        tracing::subscriber::set_default(subscriber)
    }

    /// Drive one real HTTP GET through the full axum router + middleware and
    /// return the captured log contents so we can assert on them.
    async fn run_http_request(path: &str, client_ip_header: Option<&str>) -> String {
        let writer = BufWriter::new();
        let _guard = test_subscriber(writer.clone());

        let app = Router::new()
            .route("/", get(health))
            .route("/health", get(health))
            .route("/probe", get(probe))
            .layer(middleware::from_fn(log_request));

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();

        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                app.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .await
            .unwrap();
        });

        // Build a minimal HTTP/1.1 request by hand. Avoids pulling in
        // reqwest/hyper-client just for tests.
        let mut req = format!("GET {path} HTTP/1.1\r\nHost: test\r\nConnection: close\r\n");
        if let Some(ip) = client_ip_header {
            req.push_str(&format!("{CLIENT_IP_HEADER}: {ip}\r\n"));
        }
        req.push_str("\r\n");

        let mut stream = TcpStream::connect(format!("127.0.0.1:{port}"))
            .await
            .unwrap();
        stream.write_all(req.as_bytes()).await.unwrap();
        stream.flush().await.unwrap();

        // Read until EOF; `Connection: close` ensures the server drops us.
        let mut response = Vec::new();
        let _ = stream.read_to_end(&mut response).await;
        drop(stream);

        server.abort();
        let _ = server.await;

        writer.contents()
    }

    #[tokio::test]
    async fn log_request_logs_health_with_caller_ip() {
        // /health has no ReachableOutcome; the middleware should still
        // emit one info line with method, path, status, elapsed, caller_ip.
        // Peer is loopback + we send a trusted X-AIMX-Client-IP so the
        // resolver returns the public header IP.
        let logs = run_http_request("/health", Some("203.0.113.42")).await;
        assert!(
            logs.contains("http request"),
            "expected 'http request' log line, got: {logs}"
        );
        assert!(
            logs.contains("caller_ip=203.0.113.42"),
            "expected resolved caller_ip in logs, got: {logs}"
        );
        assert!(
            logs.contains("path=/health"),
            "expected /health path in logs, got: {logs}"
        );
        assert!(
            logs.contains("method=GET"),
            "expected GET method in logs, got: {logs}"
        );
        assert!(
            logs.contains("status=200"),
            "expected status=200 in logs, got: {logs}"
        );
        assert!(
            logs.contains("elapsed_ms="),
            "expected elapsed_ms field in logs, got: {logs}"
        );
    }

    #[tokio::test]
    async fn log_request_logs_probe_with_reachable_outcome() {
        // /probe with a public IP in the trusted header -- TEST-NET-1 is
        // unroutable so reachable will be false, but the important thing
        // is that `reachable=false` appears in the log line alongside the
        // other fields.
        let logs = run_http_request("/probe", Some("192.0.2.1")).await;
        assert!(
            logs.contains("path=/probe"),
            "expected /probe path in logs, got: {logs}"
        );
        assert!(
            logs.contains("caller_ip=192.0.2.1"),
            "expected resolved caller_ip=192.0.2.1, got: {logs}"
        );
        assert!(
            logs.contains("reachable=false"),
            "expected reachable=false in logs, got: {logs}"
        );
    }

    #[tokio::test]
    async fn log_request_logs_bad_request_with_unknown_caller_ip() {
        // /probe with loopback peer and no trusted header. Resolver fails,
        // handler returns 400. The middleware should still log one line and
        // record caller_ip=unknown with status=400.
        let logs = run_http_request("/probe", None).await;
        assert!(
            logs.contains("status=400"),
            "expected status=400 in logs, got: {logs}"
        );
        assert!(
            logs.contains("caller_ip=unknown"),
            "expected caller_ip=unknown in logs, got: {logs}"
        );
    }

    #[tokio::test]
    async fn smtp_listener_logs_peer_ip_on_accept_and_close() {
        // Bind an ephemeral listener, accept one connection, and hand it to
        // the same `spawn_smtp_connection` helper the real accept loop uses
        // so the test exercises the production logging path directly.
        let writer = BufWriter::new();
        let _guard = test_subscriber(writer.clone());

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();

        let gate = Arc::new(Semaphore::new(1));
        let server = tokio::spawn(async move {
            let (stream, peer) = listener.accept().await.unwrap();
            let permit = gate.try_acquire_owned().unwrap();
            spawn_smtp_connection(stream, peer, permit).await;
        });

        let mut stream = TcpStream::connect(format!("127.0.0.1:{port}"))
            .await
            .unwrap();
        let _banner = read_line(&mut stream).await;
        stream.write_all(b"QUIT\r\n").await.unwrap();
        let _bye = read_line(&mut stream).await;

        // Let the server task finish so its close-path log event lands in
        // the buffer before we read it.
        server.await.unwrap();

        let logs = writer.contents();
        assert!(
            logs.contains("smtp connection accepted"),
            "expected 'smtp connection accepted' in logs, got: {logs}"
        );
        assert!(
            logs.contains("smtp connection closed cleanly"),
            "expected 'smtp connection closed cleanly' in logs, got: {logs}"
        );
        assert!(
            logs.contains("peer_ip=127.0.0.1"),
            "expected peer_ip=127.0.0.1 in logs, got: {logs}"
        );
    }

    /// Verifies `dispatch_smtp_connection` honours its upper bound: the first
    /// N connections spawn tasks (consuming permits), and the (N+1)-th is
    /// dropped without spawning. This exercises the production accept-loop
    /// body directly.
    #[tokio::test]
    async fn smtp_listener_concurrency_gate_drops_excess() {
        let writer = BufWriter::new();
        let _guard = test_subscriber(writer.clone());

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let gate = Arc::new(Semaphore::new(1));

        // First connection: acquires the only permit. We do NOT send QUIT so
        // the session sits inside its read-line timeout, holding the permit
        // for the duration of the test.
        let conn1 = TcpStream::connect(format!("127.0.0.1:{port}"))
            .await
            .unwrap();
        let (stream1, peer1) = listener.accept().await.unwrap();
        assert!(
            dispatch_smtp_connection(stream1, peer1, Arc::clone(&gate), 1),
            "first connection must acquire a permit and spawn"
        );

        // Second connection: gate saturated. `dispatch_smtp_connection` must
        // return false and log the drop event.
        let conn2 = TcpStream::connect(format!("127.0.0.1:{port}"))
            .await
            .unwrap();
        let (stream2, peer2) = listener.accept().await.unwrap();
        assert!(
            !dispatch_smtp_connection(stream2, peer2, Arc::clone(&gate), 1),
            "second connection must be dropped when gate is saturated"
        );

        // Give the tracing layer a moment to flush the drop event.
        tokio::task::yield_now().await;

        let logs = writer.contents();
        assert!(
            logs.contains("smtp connection dropped: concurrency gate saturated"),
            "expected drop log, got: {logs}"
        );

        drop(conn1);
        drop(conn2);
    }
}
