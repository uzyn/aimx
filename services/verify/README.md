# aimx-verify

Verification service for [aimx](https://github.com/uzyn/aimx). Provides two complementary HTTP endpoints plus a built-in port 25 listener:

1. **`/probe`** — full SMTP EHLO handshake back to the caller's IP on port 25. Used by `aimx setup` and `aimx verify` to confirm a real SMTP server is responding after OpenSMTPD is installed.
2. **`/reach`** — plain TCP connect to the caller's IP on port 25 (no SMTP handshake). Used by `aimx preflight` to confirm port 25 is reachable on a fresh VPS before OpenSMTPD is installed.
3. **Port 25 listener** — built-in TCP listener on port 25 that implements a minimal but correct SMTP exchange (banner → EHLO/HELO → 250 → QUIT → 221 Bye), allowing aimx clients to test outbound port 25 reachability from their end.

No MTA is required on the verify server.

## Building

```bash
cd services/verify
cargo build --release
```

## Running

The HTTP listener binds to **loopback by default** (`127.0.0.1:3025`). In production it is expected to run behind a reverse proxy (Caddy) that terminates TLS and injects the trusted `X-AIMX-Client-IP` header. See the "Caddy deployment" section below.

```bash
# Default: HTTP on 127.0.0.1:3025, SMTP on 0.0.0.0:25
./target/release/aimx-verify

# Override binds (advanced — see security note below)
BIND_ADDR=0.0.0.0:8080 SMTP_BIND_ADDR=0.0.0.0:2525 ./target/release/aimx-verify
```

**Security note:** Direct `BIND_ADDR=0.0.0.0:3025` exposure is **not supported in production**. Without a trusted reverse proxy in front, there is no trust boundary for the `X-AIMX-Client-IP` header and the app has no way to authoritatively identify the real caller. The `BIND_ADDR` override exists for local testing and for operators running the service inside a container/network that enforces the trust boundary externally.

### API Endpoints

#### `GET /health`
Health check. Returns `{"status": "ok", "service": "aimx-verify"}`.

#### `GET /probe`
Connects back to the caller's IP on port 25 and performs a full SMTP EHLO handshake. Returns `reachable: true` only if a real SMTP server responds with a valid `220` banner, accepts `EHLO`, and replies `250`. Used by `aimx setup` and `aimx verify`, both of which run after OpenSMTPD is installed and should validate that a real SMTP responder is live.

Response: `{"reachable": true, "ip": "1.2.3.4"}`

Returns **HTTP 400** if the service is behind a reverse proxy (TCP peer is loopback) and the `X-AIMX-Client-IP` header is missing, unparseable, or points at a loopback/private/link-local address. This indicates a Caddyfile misconfiguration — the proxy should be injecting the header.

#### `GET /reach`
Connects back to the caller's IP on port 25 with a plain TCP connect (10-second timeout). Does NOT perform any SMTP handshake. Returns `reachable: true` as long as the TCP connection succeeds — any listening socket on port 25 counts. Used by `aimx preflight` to check reachability on a fresh VPS before OpenSMTPD (or any other MTA) is installed. Same response shape and same 400 behavior as `/probe`.

Response: `{"reachable": true, "ip": "1.2.3.4"}`

### Port 25 Listener

The service also listens on port 25 (configurable via `SMTP_BIND_ADDR`). When an aimx client connects, it receives a `220` banner, can complete a full EHLO/HELO/QUIT exchange, and receives a `221 Bye` on disconnect. This is a minimal but correct SMTP responder — not a real mail server (no `MAIL FROM`, `RCPT TO`, `DATA`, or `AUTH` support) — used solely as a target for outbound port 25 reachability tests.

## Caddy Deployment

The verify service is designed to run behind Caddy. Caddy terminates TLS and injects `X-AIMX-Client-IP` with the real TCP peer address, which the app trusts only because the backend is loopback-bound.

A canonical `Caddyfile` is committed at `services/verify/Caddyfile`:

```caddyfile
{$DOMAIN:check.aimx.email} {
    reverse_proxy 127.0.0.1:3025 {
        header_up -X-Forwarded-For
        header_up X-AIMX-Client-IP {remote_host}
    }
}
```

Two directives are load-bearing for security:

- **`header_up -X-Forwarded-For`** strips any client-supplied `X-Forwarded-For`. The app never reads this header; stripping it defense-in-depth prevents anyone downstream from accidentally re-introducing a vulnerability.
- **`header_up X-AIMX-Client-IP {remote_host}`** authoritatively sets a dedicated header to Caddy's view of the real TCP peer. Caddy's `header_up <name> <value>` **replaces** rather than appends, so a client cannot pre-seed this header — Caddy always overwrites.

`{$DOMAIN:check.aimx.email}` uses Caddy's env-var interpolation with a default. For the production `check.aimx.email` deployment, no env vars are needed. For a self-hosted instance, set the hostname via env var:

```bash
DOMAIN=check.yourdomain.com caddy run
```

(If you run Caddy as a systemd service, add `Environment=DOMAIN=check.yourdomain.com` to the unit.)

## Self-Hosting

To self-host (replacing `check.aimx.email`):

1. Deploy the binary on a server with port 25 open inbound and outbound.
2. Point your domain's DNS to the server.
3. Install Caddy and drop in the canonical `services/verify/Caddyfile` (set `DOMAIN` as above).
4. Run `aimx-verify` with its default loopback bind (`BIND_ADDR=127.0.0.1:3025`).
5. In your aimx `config.toml`, set `verify_host` to the base URL of your instance (no path):
   ```toml
   verify_host = "https://check.yourdomain.com"
   ```
   aimx appends `/probe` (or `/reach`) to this base URL when making HTTP checks.

   You can also override it per-invocation:
   ```
   aimx verify --verify-host https://check.yourdomain.com
   ```

No MTA, no email sending, no DNS records beyond the A record are needed on the verify server — it only needs:
- Port 25 open (for the built-in SMTP listener)
- HTTPS on 443 (via Caddy)

## Deployment with systemd

```ini
[Unit]
Description=aimx verify service
After=network.target

[Service]
ExecStart=/usr/local/bin/aimx-verify
Environment=BIND_ADDR=127.0.0.1:3025
Environment=SMTP_BIND_ADDR=0.0.0.0:25
Restart=always
User=aimx-verify
AmbientCapabilities=CAP_NET_BIND_SERVICE

[Install]
WantedBy=multi-user.target
```

`BIND_ADDR=127.0.0.1:3025` is the default and is shown here for clarity. `SMTP_BIND_ADDR=0.0.0.0:25` exposes the SMTP listener directly on port 25; `CAP_NET_BIND_SERVICE` lets the non-root user bind the privileged port.

## Testing

```bash
cargo test
```
