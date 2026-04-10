# aimx Manual Setup & Verification Guide

This guide walks through every step needed to launch aimx, from deploying the verification infrastructure to running a production mail server. It is split into two parts:

- **Part A** — Deploy the verify service (done once, before anything else)
- **Part B** — Set up an aimx mail server (done per server)

The verify service must be running before any mail server can complete setup, because `aimx setup` calls the probe endpoint to test port 25 reachability, and `aimx verify` sends a test email to the echo endpoint.

---

## Part A: Deploy the aimx-verify Service

The aimx-verify service provides two functions:

1. **Port probe** (`/probe`) — HTTP endpoint that connects back to a target IP on port 25 to test inbound SMTP reachability. Used by `aimx setup` and `aimx preflight`.
2. **Email echo** (`echo` subcommand) — Receives email via MDA pipe, reads DKIM/SPF results from Authentication-Results headers, and auto-replies with verification status. Used by `aimx verify`.

### A1: Prerequisites

- A server or VPS (can be separate from the mail server)
- Two domains (or subdomains) pointed to it:
  - One for the HTTP probe (e.g. `check.aimx.email`)
  - One for receiving verification emails (e.g. `aimx.email` with MX record)
- Rust toolchain (`rustup`)
- Caddy (for automatic HTTPS on the probe endpoint)
- OpenSMTPD (for the email echo component)

### A2: Build aimx-verify

```bash
git clone https://github.com/uzyn/aimx.git
cd aimx/services/verify
cargo build --release
sudo cp target/release/aimx-verify /usr/local/bin/
```

### A3: Deploy the Probe Service (HTTP)

Create a systemd service:

```bash
sudo useradd --system --no-create-home aimx-verify
```

```bash
sudo tee /etc/systemd/system/aimx-verify.service > /dev/null << 'EOF'
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
EOF
```

Enable and start:

```bash
sudo systemctl daemon-reload
sudo systemctl enable --now aimx-verify
```

Set up Caddy as a reverse proxy (handles TLS automatically via Let's Encrypt):

```bash
sudo apt-get install -y caddy
```

Add to `/etc/caddy/Caddyfile`:

```
check.aimx.email {
    reverse_proxy 127.0.0.1:3025
}
```

```bash
sudo systemctl reload caddy
```

Verify:

```bash
curl https://check.aimx.email/health
# Expected: {"status":"ok","service":"aimx-verify"}
```

### A4: Deploy the Email Echo

The echo component requires an MTA to receive email and pipe it to `aimx-verify echo`.

**Install OpenSMTPD:**

```bash
sudo apt-get update && sudo apt-get install -y opensmtpd
```

**Generate a TLS certificate** (self-signed or use Let's Encrypt):

```bash
sudo mkdir -p /etc/ssl/aimx-verify
sudo openssl req -x509 -newkey rsa:2048 \
    -keyout /etc/ssl/aimx-verify/key.pem \
    -out /etc/ssl/aimx-verify/cert.pem \
    -days 3650 -nodes \
    -subj "/CN=aimx.email"
```

**Configure OpenSMTPD** (`/etc/smtpd.conf`):

```
pki aimx.email cert "/etc/ssl/aimx-verify/cert.pem"
pki aimx.email key "/etc/ssl/aimx-verify/key.pem"

listen on 0.0.0.0 tls pki aimx.email
listen on :: tls pki aimx.email

action "verify" mda "/usr/local/bin/aimx-verify echo"
action "relay" relay

match from any for rcpt-to "verify@aimx.email" action "verify"
match for any action "relay"
```

**Restart OpenSMTPD:**

```bash
sudo systemctl restart opensmtpd
```

**Set up DNS for the echo domain** (e.g. `aimx.email`):

| Type | Name | Value |
|------|------|-------|
| A | aimx.email | Verify server IP |
| MX | aimx.email | 10 aimx.email. |
| TXT | aimx.email | v=spf1 ip4:VERIFY_SERVER_IP -all |

Note: The echo reply is sent via `sendmail -t`, so outbound port 25 must also be open on the verify server.

### A5: Test the Verify Service

**Test the probe:**

```bash
# Replace with any IP that has port 25 open
curl "https://check.aimx.email/probe?ip=1.2.3.4"
# Expected: {"reachable":true,"ip":"1.2.3.4"} (if port 25 is open on that IP)
# Or: {"reachable":false,"ip":"1.2.3.4"} (if not)
```

**Test the echo:**

Send an email from any account to `verify@aimx.email`. You should receive an auto-reply containing:

```
aimx verification result
========================

DKIM: pass (or fail/none)
SPF:  pass (or fail/none)

Your email was received and processed by the aimx verify service.
```

---

## Part B: Set Up aimx on a Mail Server

### B1: Prerequisites

**Server:**

- Linux VPS (Debian/Ubuntu) with port 25 open **inbound and outbound**
- A domain you control with access to DNS management
- System tools: `openssl`, `curl`, `dig` (from `dnsutils`), `hostname`

**Compatible VPS providers:**

| Provider | Port 25 | Notes |
|----------|---------|-------|
| Hetzner Cloud | After unblock request | Request via support after first invoice |
| OVH / Kimsufi | Open by default | |
| BuyVM (Frantech) | Open by default | |
| Vultr | On request | |
| Linode / Akamai | On request | Submit support ticket |

**Install Rust toolchain** (if not already installed):

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source $HOME/.cargo/env
```

**Install system tools:**

```bash
sudo apt-get update && sudo apt-get install -y dnsutils openssl curl
```

### B2: Build & Install aimx

```bash
git clone https://github.com/uzyn/aimx.git
cd aimx
cargo build --release
sudo cp target/release/aimx /usr/local/bin/
```

Verify the binary:

```bash
aimx --version
```

### B3: Open Port 25

Ensure port 25 is open in your firewall:

```bash
# If using ufw:
sudo ufw allow 25/tcp

# If using iptables directly:
sudo iptables -A INPUT -p tcp --dport 25 -j ACCEPT
```

### B4: Preflight Checks

Run preflight to confirm your server is ready:

```bash
aimx preflight
```

This checks three things:

| Check | What it does | Fix if it fails |
|-------|-------------|-----------------|
| Outbound port 25 | Connects to `gmail-smtp-in.l.google.com:25` | Ask VPS provider to unblock outbound SMTP |
| Inbound port 25 | Calls `check.aimx.email/probe` to connect back to your IP:25 | Open firewall, ask VPS provider to unblock inbound SMTP |
| PTR record | Reverse DNS lookup on your IP | Set PTR at your VPS provider's control panel |

All three should show PASS (PTR may show WARN, which is acceptable but should be fixed for deliverability).

### B5: Run Setup Wizard

```bash
sudo aimx setup agent.yourdomain.com
```

The setup wizard performs these steps automatically:

1. Runs preflight checks (port 25 outbound/inbound, PTR)
2. Installs OpenSMTPD via `apt-get install opensmtpd`
3. Generates a self-signed TLS certificate at `/etc/ssl/aimx/`
4. Writes `/etc/smtpd.conf` (backs up any existing config)
5. Restarts OpenSMTPD
6. Generates DKIM keypair at `/var/lib/aimx/dkim/`
7. Creates `/var/lib/aimx/config.toml` with a catchall mailbox
8. Creates the catchall mailbox directory
9. Displays the DNS records you need to add (see next step)
10. Waits for you to add DNS records, then verifies them
11. Optionally runs end-to-end verification

### B6: DNS Configuration

After the setup wizard displays the required DNS records, add them at your domain registrar:

| Type | Name | Value | Where to set |
|------|------|-------|--------------|
| A | agent.yourdomain.com | Your server IP | Domain registrar |
| MX | agent.yourdomain.com | `10 agent.yourdomain.com.` | Domain registrar |
| TXT | agent.yourdomain.com | `v=spf1 ip4:YOUR_IP -all` | Domain registrar |
| TXT | dkim._domainkey.agent.yourdomain.com | `v=DKIM1; k=rsa; p=...` | Domain registrar |
| TXT | _dmarc.agent.yourdomain.com | `v=DMARC1; p=reject; rua=mailto:postmaster@agent.yourdomain.com` | Domain registrar |
| PTR | Your server IP | `agent.yourdomain.com.` | VPS provider panel |

The DKIM public key value (`p=...`) is displayed by the setup wizard. If you need to retrieve it again:

```bash
cat /var/lib/aimx/dkim/public.key
```

DNS propagation typically takes minutes but can take up to 48 hours.

### B7: Verify DNS Records

After adding DNS records, press Enter in the setup wizard to run automatic DNS verification.

To verify manually at any time:

```bash
# A record
dig +short A agent.yourdomain.com
# Should return: your server IP

# MX record
dig +short MX agent.yourdomain.com
# Should return: 10 agent.yourdomain.com.

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
# Should return: agent.yourdomain.com.
```

### B8: End-to-End Verification

**Automated verification:**

```bash
aimx verify
```

This sends a test email from `catchall@agent.yourdomain.com` to `verify@aimx.email` and waits up to 120 seconds for an echo reply with DKIM/SPF results. A successful result means your outbound signing and DNS are all correct.

**Check server status:**

```bash
aimx status
```

**Manual inbound test:**

Send an email from an external account (e.g. Gmail) to `catchall@agent.yourdomain.com`. Then check for the delivered file:

```bash
ls /var/lib/aimx/catchall/
cat /var/lib/aimx/catchall/*.md
```

You should see a `.md` file with TOML frontmatter containing `dkim` and `spf` fields.

**Manual outbound test:**

```bash
aimx send \
    --from catchall@agent.yourdomain.com \
    --to your-personal@gmail.com \
    --subject "aimx test" \
    --body "Hello from aimx"
```

Check your inbox (and spam folder). If the email lands in spam, see the Production Hardening section.

### B9: Create Mailboxes & Channel Rules

Create additional mailboxes beyond the default catchall:

```bash
aimx mailbox create support
aimx mailbox create notifications
aimx mailbox list
```

Edit `/var/lib/aimx/config.toml` to configure channel rules (triggers on incoming mail):

```toml
[mailboxes.support]
address = "support@agent.yourdomain.com"
trust = "verified"
trusted_senders = ["*@yourcompany.com"]

[[mailboxes.support.on_receive]]
type = "cmd"
command = 'echo "New email from {from}: {subject}" >> /tmp/email.log'

[mailboxes.support.on_receive.match]
from = "*@gmail.com"
subject = "urgent"
```

Available template variables in commands: `{filepath}`, `{from}`, `{to}`, `{subject}`, `{mailbox}`, `{id}`, `{date}`.

Trust policies:
- `trust = "none"` (default) — triggers fire for all emails
- `trust = "verified"` — triggers only fire when DKIM passes
- `trusted_senders` — glob patterns that bypass DKIM verification

### B10: MCP Integration

To give AI agents access to the email system, add the MCP server to your agent's configuration.

For Claude Code (`~/.claude/settings.json`):

```json
{
  "mcpServers": {
    "email": {
      "command": "/usr/local/bin/aimx",
      "args": ["mcp"]
    }
  }
}
```

If using a custom data directory:

```json
{
  "mcpServers": {
    "email": {
      "command": "/usr/local/bin/aimx",
      "args": ["--data-dir", "/custom/path", "mcp"]
    }
  }
}
```

Available MCP tools: `mailbox_list`, `mailbox_create`, `mailbox_delete`, `email_list`, `email_read`, `email_send`, `email_reply`, `email_mark_read`, `email_mark_unread`.

### B11: Production Hardening

**Prevent spam classification:**

1. Ensure all 6 DNS records are correctly set (especially DKIM, SPF, DMARC)
2. Set a PTR record at your VPS provider
3. In Gmail: Settings > Filters > Create filter for `*@agent.yourdomain.com` > Never send to Spam
4. Alternatively, reply to an email from the domain — Gmail learns it's not spam

**Firewall:**

Only port 25 needs to be open for SMTP. No other ports are required by aimx.

**File permissions:**

The DKIM private key at `/var/lib/aimx/dkim/private.key` is created with mode `0600` (owner read/write only). Verify this is intact:

```bash
ls -la /var/lib/aimx/dkim/private.key
# Should show: -rw------- 
```

**Backups:**

The only directory to back up is `/var/lib/aimx/`. It contains everything: config, DKIM keys, all mailboxes and emails. Additionally, back up `/etc/smtpd.conf` if you've customized it beyond what setup generates.

---

## Appendix: Troubleshooting

| Problem | Diagnosis | Fix |
|---------|-----------|-----|
| Preflight: outbound port 25 blocked | VPS provider blocks SMTP | Switch providers or request unblock (see compatible providers table) |
| Preflight: inbound port 25 not reachable | Firewall or VPS blocks inbound | `sudo ufw allow 25/tcp`, check VPS firewall settings |
| DNS records not resolving | Propagation delay | Wait (up to 48h), re-check with `dig` |
| `aimx verify` times out | DNS not propagated, or verify service down | Run `aimx verify` later; check `curl https://check.aimx.email/health` |
| Emails landing in spam | Missing DNS records or no PTR | Add all DNS records, set PTR, use Gmail filter |
| OpenSMTPD not running | Service crashed or misconfigured | `sudo systemctl status opensmtpd` and `journalctl -u opensmtpd -e` |
| Emails not being delivered to mailbox | OpenSMTPD MDA misconfigured | Check `/etc/smtpd.conf` has correct `aimx ingest` path |

**Useful commands:**

```bash
aimx preflight              # Re-run port and PTR checks
aimx status                 # Show config, mailboxes, and message counts
aimx verify                 # End-to-end email verification test
systemctl status opensmtpd  # Check OpenSMTPD service status
journalctl -u opensmtpd -e  # View OpenSMTPD logs
```
