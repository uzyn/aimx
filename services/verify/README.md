# aimx-verify

Verification service for [aimx](https://github.com/uzyn/aimx). Provides two functions:

1. **Port Probe** (`/probe`) - HTTP endpoint that performs an SMTP EHLO handshake back to the caller's IP on port 25, confirming a real SMTP server is responding.
2. **Port 25 Listener** - TCP listener on port 25 that accepts connections and sends a 220 banner, allowing aimx clients to test outbound port 25 reachability.

No MTA is required on the verify server.

## Building

```bash
cd services/verify
cargo build --release
```

## Running

```bash
# Default: HTTP on 0.0.0.0:3025, SMTP on 0.0.0.0:25
./target/release/aimx-verify

# Custom bind addresses
BIND_ADDR=0.0.0.0:8080 SMTP_BIND_ADDR=0.0.0.0:2525 ./target/release/aimx-verify
```

### API Endpoints

#### `GET /health`
Health check. Returns `{"status": "ok", "service": "aimx-verify"}`.

#### `GET /probe`
Connects back to the caller's IP on port 25, performs an SMTP EHLO handshake, and returns whether a real SMTP server is responding.

Response: `{"reachable": true, "ip": "1.2.3.4"}`

### Port 25 Listener

The service also listens on port 25 (configurable via `SMTP_BIND_ADDR`). When an aimx client connects, it receives a 220 banner, confirming that outbound port 25 is not blocked by the client's VPS provider.

## Self-Hosting

To self-host (replacing `check.aimx.email`):

1. Deploy the binary on a server with port 25 open
2. Point your domain's DNS to the server
3. Configure a reverse proxy (Caddy/nginx) for HTTPS on the HTTP port
4. Run with `BIND_ADDR=127.0.0.1:3025`
5. In your aimx `config.toml`, set `verify_host` to the base URL of your instance (no path):
   ```toml
   verify_host = "https://verify.yourdomain.com"
   ```
   aimx appends `/probe` to this base URL when making the HTTP check.

   You can also override it per-invocation:
   ```
   aimx verify --verify-host https://verify.yourdomain.com
   ```

No MTA, no email sending, no DNS records needed on the verify server -- it only needs:
- Port 25 open (for the SMTP listener)
- HTTPS (for the probe endpoint, via reverse proxy)

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

## Testing

```bash
cargo test
```
