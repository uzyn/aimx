# Setup

This guide covers every step of setting up aimx in detail -- from prerequisites through production hardening.

For a shorter walkthrough, see [Getting Started](getting-started.md).

## Prerequisites

### Server

- Any Unix VPS with port 25 open inbound **and** outbound (CI tests Ubuntu, Alpine, Fedora)
- A domain you control with access to DNS management
- Root access (required for service installation and binding port 25)

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

## Pre-setup verification

Before running setup, you can verify port 25 connectivity:

```bash
sudo aimx verify
```

`aimx verify` requires root. When `aimx serve` is running, it performs an outbound EHLO handshake plus an inbound EHLO handshake probe. When nothing is on port 25 (fresh VPS), it spawns a temporary listener and runs checks. If port 25 is occupied by another process, verify tells you to stop it before setup.

| Check | What it does | Fix if it fails |
|-------|-------------|-----------------|
| Outbound port 25 | Performs EHLO handshake to `check.aimx.email` on port 25 | Ask VPS provider to unblock outbound SMTP |
| Inbound port 25 | Calls the verify service to perform EHLO handshake back to your IP on port 25 | Open firewall, ask VPS provider to unblock inbound SMTP |

All checks should show PASS before proceeding with setup.

## Setup wizard

The setup wizard can be run with or without a domain argument:

```bash
# With domain argument (scripting-friendly, no prompts):
sudo aimx setup agent.yourdomain.com

# Without argument (interactive, prompts for domain):
sudo aimx setup
```

When run without a domain argument, setup will prompt you to enter the domain and confirm you have DNS access.

### First-time setup flow

1. **Root check** -- exits if not running as root
2. **Domain prompt** -- asks for domain if not provided as argument
3. **Port 25 conflict detection** -- checks for another process on port 25
4. **TLS certificate** -- generates a self-signed certificate at `/etc/ssl/aimx/`
5. **DKIM key generation** -- creates a 2048-bit RSA keypair at `/var/lib/aimx/dkim/`
6. **Config creation** -- writes `/var/lib/aimx/config.toml` with a catchall mailbox
7. **Service installation** -- generates a systemd unit file (or OpenRC init script on Alpine) and starts `aimx serve`
8. **Port 25 checks** -- verifies outbound and inbound port 25 connectivity

After initial setup, the wizard displays three clearly labeled sections:

- **[DNS]** -- the records you need to add (MX, A, SPF, DKIM, DMARC), with a retry loop so you can press Enter to re-verify after adding records
- **[MCP]** -- configuration snippet for MCP-compatible AI agents (Claude Code, OpenClaw, Codex, OpenCode, etc.)
- **[Deliverability Improvement (Optional)]** -- PTR record guidance and Gmail filter/whitelist instructions

### Re-running setup

If you've already completed setup and want to re-verify, simply run `aimx setup` again:

```bash
sudo aimx setup agent.yourdomain.com
```

When aimx detects an existing configuration (`aimx serve` running, TLS cert present, DKIM key present), it skips the install/configure steps and proceeds directly to port 25 checks, DNS verification, and the output sections. This makes re-runs a quick verification pass.

### DNS retry loop

At the DNS verification step, you can:

- Press **Enter** to re-check DNS records (useful when you've just updated DNS in another tab)
- Press **q** to finish and verify later with `sudo aimx setup <domain>`

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
sudo aimx verify
```

This tests outbound port 25 connectivity (via EHLO handshake) and inbound SMTP reachability (via EHLO probe). Requires root.

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

Back up `/var/lib/aimx/` -- it contains everything: config, DKIM keys, all mailboxes and emails.

## Verifier service

The verifier service is used during setup to test port 25 reachability. aimx uses a public instance at `check.aimx.email` by default.

### Self-hosting the verifier service

If you prefer not to use the public instance:

1. Build the verifier service:
   ```bash
   cd services/verifier
   cargo build --release
   sudo cp target/release/aimx-verifier /usr/local/bin/
   ```

2. Deploy with systemd:
   ```ini
   [Unit]
   Description=aimx verifier service
   After=network.target

   [Service]
   ExecStart=/usr/local/bin/aimx-verifier
   Environment=BIND_ADDR=127.0.0.1:3025
   Environment=SMTP_BIND_ADDR=0.0.0.0:25
   Restart=always
   User=aimx-verifier
   AmbientCapabilities=CAP_NET_BIND_SERVICE

   [Install]
   WantedBy=multi-user.target
   ```

3. Set up a reverse proxy (e.g. Caddy) for HTTPS on the probe endpoint.

4. Point aimx to your instance in `config.toml`:
   ```toml
   verify_host = "https://verify.yourdomain.com"
   ```

   Or override it per-invocation with `--verify-host`:
   ```
   sudo aimx verify --verify-host https://verify.yourdomain.com
   ```

The verifier service provides:
- `GET /health` -- health check
- `GET /probe` -- connects back to caller's IP on port 25, performs EHLO handshake
- Port 25 listener -- accepts TCP connections for outbound port 25 testing

See the [verifier service README](../services/verifier/README.md) for full details.

---

Next: [Configuration](configuration.md) | [Troubleshooting](troubleshooting.md)
