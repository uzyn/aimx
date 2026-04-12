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
   aimx appends `/probe` to this base URL when making HTTP checks. (A future release will also append `/reach` for pre-install preflight.)

   You can also override it per-invocation:
   ```
   aimx verify --verify-host https://check.yourdomain.com
   ```

No MTA, no email sending, no DNS records beyond the A record are needed on the verify server — it only needs:
- Port 25 open (for the built-in SMTP listener)
- HTTPS on 443 (via Caddy)

## Deployment with Docker

A `Dockerfile` and `docker-compose.yml` ship alongside this README for operators who prefer container-based deployments. This path coexists with the systemd path below — pick one.

The compose file brings up **both** the verify service and Caddy in a single command:

```bash
cd services/verify
docker compose up -d --build
```

That single command builds the multi-stage verify image (Rust builder → `debian:bookworm-slim` runtime) and pulls the official `caddy:2` image. The verify container runs as root so it can bind port 25 without capability fiddling. Both containers use `network_mode: host`, sharing the host's network namespace — verify binds `127.0.0.1:3025` (HTTP) and `0.0.0.0:25` (SMTP), while Caddy binds `0.0.0.0:443` (HTTPS) and `0.0.0.0:80` (HTTP redirect). No Docker-side port publishing is involved.

For self-hosted instances, set the domain via environment variable:

```bash
DOMAIN=check.yourdomain.com docker compose up -d --build
```

Caddy auto-provisions TLS certificates via ACME (Let's Encrypt). The `caddy_data` volume persists certs across container restarts. Ensure the domain's DNS A record points to the server before starting.

### Why `network_mode: host`?

The verify service's security model (Sprint 12) enforces a Layer 3 trust boundary: the HTTP listener binds `127.0.0.1:3025` by default, and the app only reads the `X-AIMX-Client-IP` header when the TCP peer is loopback. Combined with Caddy in front (which injects that header authoritatively), this is the only trust path the app recognises.

The "obvious" docker-compose shape — `ports: "3025:3025"` plus `BIND_ADDR=0.0.0.0:3025` inside the container — **breaks** this model. Docker's userland proxy rewrites connections so the TCP peer the app sees is the bridge gateway (a private RFC 1918 address), which:

1. Fails the Layer 3 loopback-only check, so the app refuses to trust `X-AIMX-Client-IP`.
2. Would otherwise get rejected by the Layer 4 target guard anyway, since the guard explicitly blocks loopback, link-local, and RFC 1918 / RFC 4193 ranges from being used as SMTP targets.

`network_mode: host` avoids this entirely: the container shares the host's network namespace, so `127.0.0.1:3025` inside the container IS the host's loopback, and Caddy running on the host can reverse-proxy to it exactly as it would for a systemd-native deployment. No explicit `ports:` mapping is needed (or allowed — Docker rejects `ports` in host-network mode).

### Verifying the deployment

```bash
# From the host
curl http://127.0.0.1:3025/health
# -> {"status":"ok","service":"aimx-verify"}

# SMTP listener banner
nc 127.0.0.1 25
# -> 220 check.aimx.email SMTP aimx-verify

# Caddy is proxying
curl -I https://localhost
# -> should show Caddy's TLS response (or cert error if DNS isn't pointed yet)

# Per-request logs from both containers
docker compose logs -f verify
docker compose logs -f caddy
```

From a remote machine (with DNS configured), `curl https://check.yourdomain.com/probe` and `curl https://check.yourdomain.com/reach` should return JSON with the caller's real public IP — not `127.0.0.1` or a private Docker bridge address.

### Running without compose

If you prefer to manage containers individually:

```bash
# Verify service
docker build -t aimx-verify:local services/verify
docker run -d --name aimx-verify \
  --network host \
  --restart unless-stopped \
  -e RUST_LOG=info \
  aimx-verify:local

# Caddy
docker run -d --name aimx-caddy \
  --network host \
  --restart unless-stopped \
  -v $(pwd)/services/verify/Caddyfile:/etc/caddy/Caddyfile:ro \
  -v caddy_data:/data \
  -v caddy_config:/config \
  -e DOMAIN=check.yourdomain.com \
  caddy:2
```

Same semantics as the compose shape: host networking, default loopback HTTP bind, Caddy handles TLS + client IP injection.

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
