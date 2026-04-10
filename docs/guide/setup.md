# Setup

This guide covers every step of setting up aimx in detail -- from prerequisites through production hardening.

For a shorter walkthrough, see [Getting Started](getting-started.md).

## Prerequisites

### Server

- Linux VPS (Debian/Ubuntu) with port 25 open inbound **and** outbound
- A domain you control with access to DNS management
- Root access (required for OpenSMTPD installation)

### System tools

Install required system tools:

```bash
sudo apt-get update && sudo apt-get install -y dnsutils openssl curl
```

### Firewall

Ensure port 25 is open:

```bash
# If using ufw:
sudo ufw allow 25/tcp

# If using iptables:
sudo iptables -A INPUT -p tcp --dport 25 -j ACCEPT
```

## Building from source

```bash
# Install Rust (if needed)
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source $HOME/.cargo/env

# Clone and build
git clone https://github.com/uzyn/aimx.git
cd aimx
cargo build --release
sudo cp target/release/aimx /usr/local/bin/

# Verify
aimx --version
```

## Preflight checks

Before running setup, you can run preflight checks independently:

```bash
aimx preflight
```

This checks three things without installing anything:

| Check | What it does | Fix if it fails |
|-------|-------------|-----------------|
| Outbound port 25 | Connects to a well-known MX server on port 25 | Ask VPS provider to unblock outbound SMTP |
| Inbound port 25 | Calls `check.aimx.email/probe` to connect back to your IP on port 25 | Open firewall, ask VPS provider to unblock inbound SMTP |
| PTR record | Reverse DNS lookup on your IP | Set PTR at your VPS provider's control panel |

All three should show PASS. PTR may show WARN, which is acceptable but should be fixed for deliverability.

## Setup wizard

```bash
sudo aimx setup agent.yourdomain.com
```

The setup wizard performs these steps automatically:

1. **Root check** -- exits if not running as root
2. **Port 25 conflict detection** -- checks for existing MTA on port 25
3. **Preflight checks** -- outbound/inbound port 25 and PTR record
4. **OpenSMTPD installation** -- installs via `apt-get install opensmtpd`
5. **TLS certificate** -- generates a self-signed certificate at `/etc/ssl/aimx/`
6. **OpenSMTPD configuration** -- writes `/etc/smtpd.conf` (backs up any existing config)
7. **Service restart** -- restarts OpenSMTPD with the new configuration
8. **DKIM key generation** -- creates a 2048-bit RSA keypair at `/var/lib/aimx/dkim/`
9. **Config creation** -- writes `/var/lib/aimx/config.toml` with a catchall mailbox
10. **DNS record display** -- shows the records you need to add
11. **DNS verification** -- waits for you to add records, then verifies them
12. **MCP config snippet** -- displays the Claude Code integration config

## DNS configuration

After the setup wizard displays the required DNS records, add them at your domain registrar:

| Type | Name | Value | Where to set |
|------|------|-------|--------------|
| A | `agent.yourdomain.com` | Your server IP | Domain registrar |
| MX | `agent.yourdomain.com` | `10 agent.yourdomain.com.` | Domain registrar |
| TXT | `agent.yourdomain.com` | `v=spf1 ip4:YOUR_IP -all` | Domain registrar |
| TXT | `dkim._domainkey.agent.yourdomain.com` | `v=DKIM1; k=rsa; p=...` | Domain registrar |
| TXT | `_dmarc.agent.yourdomain.com` | `v=DMARC1; p=reject` | Domain registrar |
| PTR | Your server IP | `agent.yourdomain.com.` | VPS provider panel |

The DKIM public key value (`p=...`) is displayed by the setup wizard. To retrieve it again:

```bash
cat /var/lib/aimx/dkim/public.key
```

DNS propagation typically takes minutes but can take up to 48 hours.

### Verifying DNS records

After adding DNS records, verify them manually:

```bash
# A record
dig +short A agent.yourdomain.com
# Expected: your server IP

# MX record
dig +short MX agent.yourdomain.com
# Expected: 10 agent.yourdomain.com.

# SPF record
dig +short TXT agent.yourdomain.com
# Should include: v=spf1 ip4:YOUR_IP -all

# DKIM record
dig +short TXT dkim._domainkey.agent.yourdomain.com
# Should include: v=DKIM1; k=rsa; p=...

# DMARC record
dig +short TXT _dmarc.agent.yourdomain.com
# Should include: v=DMARC1; p=reject

# PTR record (reverse DNS)
dig +short -x YOUR_SERVER_IP
# Expected: agent.yourdomain.com.
```

## End-to-end verification

Run the automated verification:

```bash
aimx verify
```

This tests outbound port 25 connectivity, inbound SMTP reachability via EHLO probe, and PTR record resolution.

Check server status at any time:

```bash
aimx status
```

### Manual testing

**Inbound:** Send an email from an external account (e.g. Gmail) to `catchall@agent.yourdomain.com`, then check:

```bash
ls /var/lib/aimx/catchall/
cat /var/lib/aimx/catchall/*.md
```

**Outbound:** Send a test email:

```bash
aimx send \
    --from catchall@agent.yourdomain.com \
    --to your-personal@gmail.com \
    --subject "aimx test" \
    --body "Hello from aimx"
```

## DKIM key management

DKIM keys are generated automatically during setup. To manage them independently:

```bash
# Generate DKIM keypair (default selector: "dkim")
aimx dkim-keygen

# Force regenerate (overwrites existing keys)
aimx dkim-keygen --force

# Use a custom selector
aimx dkim-keygen --selector mykey
```

Keys are stored at:
- Private key: `/var/lib/aimx/dkim/private.key` (mode `0600`)
- Public key: `/var/lib/aimx/dkim/public.key`

After regenerating keys, update the DKIM DNS record with the new public key.

## Production hardening

### Preventing spam classification

1. Ensure all 6 DNS records are correctly set (especially DKIM, SPF, DMARC)
2. Set a PTR record at your VPS provider
3. In Gmail: Settings > Filters > Create filter for `*@agent.yourdomain.com` > Never send to Spam
4. Alternatively, reply to an email from the domain -- Gmail learns it's not spam

### Firewall

Only port 25 needs to be open for SMTP. No other ports are required by aimx.

### File permissions

The DKIM private key is created with mode `0600` (owner read/write only). Verify:

```bash
ls -la /var/lib/aimx/dkim/private.key
# Should show: -rw-------
```

### Backups

Back up `/var/lib/aimx/` -- it contains everything: config, DKIM keys, all mailboxes and emails. Additionally, back up `/etc/smtpd.conf` if you've customized it.

## Verify service

The verify service is used during setup to test port 25 reachability. aimx uses a public instance at `check.aimx.email` by default.

### Self-hosting the verify service

If you prefer not to use the public instance:

1. Build the verify service:
   ```bash
   cd services/verify
   cargo build --release
   sudo cp target/release/aimx-verify /usr/local/bin/
   ```

2. Deploy with systemd:
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

3. Set up a reverse proxy (e.g. Caddy) for HTTPS on the probe endpoint.

4. Point aimx to your instance in `config.toml`:
   ```toml
   probe_url = "https://verify.yourdomain.com/probe"
   ```

The verify service provides:
- `GET /health` -- health check
- `GET /probe` -- connects back to caller's IP on port 25, performs EHLO handshake
- Port 25 listener -- accepts TCP connections for outbound port 25 testing

See the [verify service README](../../services/verify/README.md) for full details.

---

Next: [Configuration](configuration.md) | [Troubleshooting](troubleshooting.md)
