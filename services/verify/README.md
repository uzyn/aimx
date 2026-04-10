# aimx-verify

Verification service for [aimx](https://github.com/uzyn/aimx). Provides two functions:

1. **Port Probe** (`/probe`) - HTTP endpoint that connects back to a caller's IP on port 25 to verify inbound SMTP reachability.
2. **Email Echo** (`echo` subcommand) - Receives email via stdin (MDA pipe), parses DKIM/SPF results from Authentication-Results headers, and sends an auto-reply with the verification status.

## Building

```bash
cd services/verify
cargo build --release
```

## Running the Probe Service

```bash
# Default: listens on 0.0.0.0:3025
./target/release/aimx-verify

# Custom bind address
BIND_ADDR=0.0.0.0:8080 ./target/release/aimx-verify
```

### API Endpoints

#### `GET /health`
Health check. Returns `{"status": "ok", "service": "aimx-verify"}`.

#### `GET /probe?ip=<target_ip>`
Connects to `<target_ip>:25` and returns whether port 25 is reachable.

If `ip` is omitted, uses the caller's IP address.

Response: `{"reachable": true, "ip": "1.2.3.4"}`

#### `POST /probe`
Same as GET but accepts JSON body: `{"ip": "1.2.3.4"}`.

## Running the Email Echo

Configure your MTA to pipe incoming mail for `verify@yourdomain` to the echo command:

```bash
# OpenSMTPD example:
action "verify" mda "/path/to/aimx-verify echo"
match from any for rcpt-to "verify@aimx.email" action "verify"
```

The echo service reads raw email from stdin, extracts Authentication-Results, and sends an auto-reply via `sendmail` with the DKIM/SPF verification results.

## Self-Hosting

To self-host (replacing `check.aimx.email`):

1. Deploy the binary on a server
2. Point your domain's DNS to the server
3. Configure a reverse proxy (nginx/caddy) for HTTPS
4. Run with `BIND_ADDR=127.0.0.1:3025`
5. In your aimx `config.yaml`, set `probe_url` and `verify_address`:
   ```yaml
   probe_url: "https://verify.yourdomain.com/probe"
   verify_address: "verify@yourdomain.com"
   ```

For the email echo, additionally:

1. Set up OpenSMTPD (or any MTA) on the verify server
2. Configure MDA to pipe to `aimx-verify echo`
3. Set up DNS (MX, SPF, DKIM) for the verify domain

## Deployment with systemd

```ini
[Unit]
Description=aimx verify service
After=network.target

[Service]
ExecStart=/usr/local/bin/aimx-verify
Environment=BIND_ADDR=127.0.0.1:3025
Restart=always
User=aimx-verify

[Install]
WantedBy=multi-user.target
```

## Testing

```bash
cargo test
```
