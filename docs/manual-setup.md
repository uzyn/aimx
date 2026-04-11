# aimx Manual Setup & Verification Guide

This guide walks through every step needed to launch aimx, from deploying the verification infrastructure to running a production mail server. It is split into two parts:

- **Part A** — Deploy the verify service (done once, before anything else)
- **Part B** — Set up an aimx mail server (done per server)

The verify service must be running before any mail server can complete setup, because `aimx setup`, `aimx preflight`, and `aimx verify` all call its `/probe` endpoint (to test inbound port 25) and connect to its built-in port 25 listener (to test outbound port 25).

---

## Part A: Deploy the aimx-verify Service

The aimx-verify service provides two functions, both exposed from a single binary:

1. **Port probe** (`GET /probe`) — HTTPS endpoint that opens a TCP connection back to the caller's own IP on port 25 and performs an SMTP EHLO handshake. Used to test inbound SMTP reachability. The probe always targets the caller's IP; there is no way to probe an arbitrary address.
2. **Port 25 listener** — Built-in TCP listener on `:25` that accepts incoming connections and returns a `220 check.aimx.email SMTP aimx-verify` banner, then `221 Bye` after 10 seconds. Used by aimx clients to test that their outbound port 25 is not blocked by their VPS provider. This is a plain tokio listener built into the `aimx-verify` binary — **no OpenSMTPD or other MTA is required on the verify server**.

### A1: Prerequisites

- A server or VPS (can be separate from the mail server) with both **inbound and outbound port 25 open**
- One domain or subdomain pointed at it (e.g. `check.aimx.email`) for the HTTPS probe endpoint
- Rust toolchain (`rustup`)
- Caddy (for automatic HTTPS on the probe endpoint)

No MTA, SMTP TLS certificate, or MX record is required on the verify server.

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
Wants=network-online.target
After=network-online.target

[Service]
ExecStart=/usr/local/bin/aimx-verify
Environment=BIND_ADDR=127.0.0.1:3025
Environment=SMTP_BIND_ADDR=0.0.0.0:25
Restart=always
User=aimx-verify
AmbientCapabilities=CAP_NET_BIND_SERVICE

[Install]
WantedBy=multi-user.target
EOF
```

`BIND_ADDR=127.0.0.1:3025` keeps HTTP behind Caddy; `SMTP_BIND_ADDR=0.0.0.0:25` exposes the TCP listener directly to the internet; `AmbientCapabilities=CAP_NET_BIND_SERVICE` lets the non-root `aimx-verify` user bind the privileged port 25.

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

### A4: Open Inbound Port 25

The SMTP listener runs inside the `aimx-verify` binary — no OpenSMTPD, TLS certificate, or MX record is needed. Just make sure port 25 is open inbound in your firewall and at your VPS provider:

```bash
sudo ufw allow 25/tcp
```

If your provider blocks port 25 on the verify host, the listener will bind successfully but remote aimx clients will not be able to reach it, and their outbound-port-25 check will fail even though their own provider is fine.

### A5: Test the Verify Service

**Health check** (confirms HTTPS + service is up):

```bash
curl https://check.aimx.email/health
# Expected: {"status":"ok","service":"aimx-verify"}
```

**SMTP listener smoke test** (confirms port 25 is open and the listener is running):

```bash
nc check.aimx.email 25
# Expected: 220 check.aimx.email SMTP aimx-verify
```

Press Ctrl-C or wait 10 seconds; the listener sends `221 Bye` and closes. The banner hostname is hardcoded as `check.aimx.email` in the binary even on self-hosted instances — don't be surprised to see it on your own domain.

**Functional `/probe` test:**

The `/probe` endpoint always probes the caller's own IP — there is no `?ip=` parameter. To exercise it end-to-end, run `aimx preflight` (or `aimx verify`) from a real mail server with port 25 open. A `PASS` on the "Inbound port 25" line means the verify service reached back and completed an EHLO handshake.

### A6: Point aimx Mail Servers at Your Verify Instance

The default verify host is `https://check.aimx.email`. If you deployed your own instance in the steps above, tell aimx to use it either via `config.toml`:

```toml
verify_host = "https://check.yourdomain.com"
```

…or per-invocation with the `--verify-host` flag (accepted by `aimx verify`, `aimx setup`, and `aimx preflight`):

```bash
aimx verify --verify-host https://check.yourdomain.com
aimx preflight --verify-host https://check.yourdomain.com
sudo aimx setup agent.yourdomain.com --verify-host https://check.yourdomain.com
```

Precedence is **CLI flag > `verify_host` in `config.toml` > default** (`https://check.aimx.email`). The value must be a base URL starting with `http://` or `https://`; aimx appends `/probe` internally when calling the probe endpoint, and derives the outbound port 25 target (`host:25`) from the same URL. A trailing slash is accepted and stripped.

Because aimx derives the outbound port-25 target from the same `verify_host` URL, the verify service's HTTP probe and its TCP port-25 listener **must run on the same host** (or at least the same hostname in DNS). Self-hosting on a provider that blocks port 25 on the verify host will leave aimx clients unable to exercise the outbound check.

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
| Outbound port 25 | Connects to the verify service host on port 25 | Ask VPS provider to unblock outbound SMTP |
| Inbound port 25 | Calls `<verify_host>/probe` to connect back to your IP:25 | Open firewall, ask VPS provider to unblock inbound SMTP |
| PTR record | Reverse DNS lookup on your IP | Set PTR at your VPS provider's control panel |

All three should show PASS (PTR may show WARN, which is acceptable but should be fixed for deliverability).

The default verify host is `https://check.aimx.email`. To point at a self-hosted instance instead (see Part A), set `verify_host` in `config.toml` or pass `--verify-host` on the command line:

```bash
aimx preflight --verify-host https://check.yourdomain.com
```

The same flag is accepted by `aimx verify` and `aimx setup`, and takes precedence over the config value.

### B5: Run Setup Wizard

```bash
sudo aimx setup agent.yourdomain.com
```

The setup wizard performs these steps automatically:

1. Checks for root privileges and scans port 25 for existing MTA conflicts
2. Installs OpenSMTPD via `apt-get install --no-install-recommends opensmtpd` (skips `opensmtpd-extras`, which pulls in unused MySQL/PostgreSQL/Redis/SQLite client libraries)
3. Generates a self-signed TLS certificate at `/etc/ssl/aimx/`
4. Writes `/etc/smtpd.conf` (backs up any existing config) and restarts OpenSMTPD
5. Runs the three port/PTR checks (outbound 25, inbound 25 via `/probe`, PTR)
6. Generates the DKIM keypair at `/var/lib/aimx/dkim/`, writes `/var/lib/aimx/config.toml`, and creates the catchall mailbox directory
7. Displays the DNS records you need to add (see next step)
8. After you add the records and press Enter, resolves and validates each one

Setup ends after DNS verification. There is no end-to-end email test — run `aimx verify` (or `aimx preflight`) later to re-check port 25 connectivity.

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

`aimx verify` runs the same three checks as `aimx preflight`: outbound port 25 (to the verify service's built-in TCP listener), inbound port 25 (via `/probe`, which connects back to your server), and PTR record. Use whichever command reads better in your workflow — they are functionally equivalent. Neither sends an actual email or waits for an echo reply.

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
| `aimx verify` shows inbound FAIL | The verify service's `/probe` endpoint could not reach your port 25 (firewall, VPS block, OpenSMTPD down, or DNS `A` record not yet resolving) | `sudo systemctl status opensmtpd`, `sudo ufw status`, `dig +short A agent.yourdomain.com`, check VPS firewall |
| Emails landing in spam | Missing DNS records or no PTR | Add all DNS records, set PTR, use Gmail filter |
| OpenSMTPD not running | Service crashed or misconfigured | `sudo systemctl status opensmtpd` and `journalctl -u opensmtpd -e` |
| Emails not being delivered to mailbox | OpenSMTPD MDA misconfigured | Check `/etc/smtpd.conf` has correct `aimx ingest` path |

**Useful commands:**

```bash
aimx preflight              # Re-run port and PTR checks
aimx status                 # Show config, mailboxes, and message counts
aimx verify                 # Port 25 + PTR check (equivalent to aimx preflight)
systemctl status opensmtpd  # Check OpenSMTPD service status
journalctl -u opensmtpd -e  # View OpenSMTPD logs
```
