# AIMX Manual Setup & Verification Guide

This guide walks through every step needed to launch AIMX, from deploying the verification infrastructure to running a production mail server. It is split into two parts:

- **Part A** — Deploy the verifier service (done once, before anything else)
- **Part B** — Set up an AIMX mail server (done per server)

The verifier service must be running before any mail server can complete setup, because `aimx setup` and `aimx portcheck` both call its HTTP `/probe` endpoint (to test inbound port 25) and connect to its built-in port 25 listener (to test outbound port 25 via EHLO handshake). Both commands use `/probe` (full SMTP EHLO handshake).

---

## Part A: Deploy the aimx-verifier Service

The aimx-verifier service provides two functions, both exposed from a single binary:

1. **Port probe** (`GET /probe`) — HTTPS endpoint that opens a TCP connection back to the caller's own IP on port 25 and performs a full SMTP EHLO handshake. Used by `aimx setup` and `aimx portcheck` to confirm a real SMTP server is responding. The probe always targets the caller's IP; there is no way to probe an arbitrary address.
2. **Port 25 listener** — Built-in TCP listener on `:25` that implements a minimal but correct SMTP exchange: banner → `EHLO`/`HELO` → `250` → `QUIT` → `221 Bye`. Used by `aimx` clients to test that their outbound port 25 is not blocked by their VPS provider via EHLO handshake. This is a plain tokio listener built into the `aimx-verifier` binary — **no MTA is required on the verifier server**.

`/probe` identifies the caller via Caddy's `X-AIMX-Client-IP` header (injected by the canonical `Caddyfile`). Direct exposure of the backend without Caddy is not supported — see the security note in `services/verifier/README.md`.

### A1: Prerequisites

- A server or VPS (can be separate from the mail server) with both **inbound and outbound port 25 open**
- One domain or subdomain pointed at it (e.g. `check.aimx.email`) for the HTTPS endpoints
- Rust toolchain (`rustup`)
- Caddy (for automatic HTTPS and for setting the trusted `X-AIMX-Client-IP` header — **required**, not optional)

No MTA, SMTP TLS certificate, or MX record is required on the verifier server.

### A2: Build aimx-verifier

```bash
git clone https://github.com/uzyn/aimx.git
cd aimx/services/verifier
cargo build --release
sudo cp target/release/aimx-verifier /usr/local/bin/
```

### A3: Deploy the Probe Service (HTTP)

Create a systemd service:

```bash
sudo useradd --system --no-create-home aimx-verifier
```

```bash
sudo tee /etc/systemd/system/aimx-verifier.service > /dev/null << 'EOF'
[Unit]
Description=aimx verifier service
Wants=network-online.target
After=network-online.target

[Service]
ExecStart=/usr/local/bin/aimx-verifier
Environment=BIND_ADDR=127.0.0.1:3025
Environment=SMTP_BIND_ADDR=0.0.0.0:25
Restart=always
User=aimx-verifier
AmbientCapabilities=CAP_NET_BIND_SERVICE

[Install]
WantedBy=multi-user.target
EOF
```

`BIND_ADDR=127.0.0.1:3025` is the default and keeps HTTP behind Caddy (the app rejects requests from loopback peers that don't carry a trusted `X-AIMX-Client-IP` header, so there's no way for external callers to bypass Caddy and hit the backend directly). `SMTP_BIND_ADDR=0.0.0.0:25` exposes the TCP listener directly to the internet. `AmbientCapabilities=CAP_NET_BIND_SERVICE` lets the non-root `aimx-verifier` user bind the privileged port 25.

Enable and start:

```bash
sudo systemctl daemon-reload
sudo systemctl enable --now aimx-verifier
```

Set up Caddy as a reverse proxy (handles TLS automatically via Let's Encrypt):

```bash
sudo apt-get install -y caddy
```

Copy the canonical Caddyfile from the repo (`services/verifier/Caddyfile`) to `/etc/caddy/Caddyfile`:

```caddyfile
{$DOMAIN:check.aimx.email} {
    reverse_proxy 127.0.0.1:3025 {
        header_up -X-Forwarded-For
        header_up X-AIMX-Client-IP {remote_host}
    }
}
```

The two `header_up` directives are load-bearing for security:

- `header_up -X-Forwarded-For` strips any client-supplied XFF — the app never reads it, but stripping it defense-in-depth prevents future regressions.
- `header_up X-AIMX-Client-IP {remote_host}` authoritatively sets the dedicated header to Caddy's view of the real TCP peer. This **replaces** rather than appends, so a client cannot pre-seed the header — Caddy always overwrites.

For a self-hosted instance on a different hostname, set the `DOMAIN` env var. The easiest way is a systemd drop-in for Caddy:

```bash
sudo systemctl edit caddy
# Add:
# [Service]
# Environment=DOMAIN=check.yourdomain.com
```

Then reload:

```bash
sudo systemctl reload caddy
```

Verify:

```bash
curl https://check.aimx.email/health
# Expected: {"status":"ok","service":"aimx-verifier"}
```

### A4: Open Inbound Port 25

The SMTP listener runs inside the `aimx-verifier` binary — no OpenSMTPD, TLS certificate, or MX record is needed. Just make sure port 25 is open inbound in your firewall and at your VPS provider:

```bash
sudo ufw allow 25/tcp
```

If your provider blocks port 25 on the verifier host, the listener will bind successfully but remote AIMX clients will not be able to reach it, and their outbound-port-25 check will fail even though their own provider is fine.

### A5: Test the Verifier Service

**Health check** (confirms HTTPS + service is up):

```bash
curl https://check.aimx.email/health
# Expected: {"status":"ok","service":"aimx-verifier"}
```

**SMTP listener smoke test** (confirms port 25 is open and the listener speaks correct SMTP):

```bash
nc check.aimx.email 25
# Expected: 220 check.aimx.email SMTP aimx-verifier
# Then type: EHLO test
# Expected: 250 check.aimx.email
# Then type: QUIT
# Expected: 221 Bye
```

The listener implements a minimal but correct SMTP exchange (banner → `EHLO`/`HELO` → `250` → `QUIT` → `221 Bye`). The banner hostname is hardcoded as `check.aimx.email` in the binary even on self-hosted instances — don't be surprised to see it on your own domain.

**Functional `/probe` test:**

The `/probe` endpoint always probes the caller's own IP — there is no `?ip=` parameter. To exercise it end-to-end, run `sudo aimx portcheck` (which hits `/probe`) from a real mail server with port 25 open. A `PASS` on the "Inbound port 25" line means the verifier service reached back and completed a full EHLO handshake.

A `curl` from the mail server is useful for debugging:

```bash
curl https://check.aimx.email/probe
# Expected: {"reachable":true,"ip":"<your-server-ip>"}
```

### A6: Point AIMX Mail Servers at Your Verifier Instance

The default verify host is `https://check.aimx.email`. If you deployed your own instance in the steps above, tell AIMX to use it either via `config.toml`:

```toml
verify_host = "https://check.yourdomain.com"
```

…or per-invocation with the `--verify-host` flag (accepted by `sudo aimx portcheck` and `sudo aimx setup`):

```bash
sudo aimx portcheck --verify-host https://check.yourdomain.com
sudo aimx setup agent.yourdomain.com --verify-host https://check.yourdomain.com
```

Precedence is **CLI flag > `verify_host` in `config.toml` > default** (`https://check.aimx.email`). The value must be a base URL starting with `http://` or `https://`; AIMX appends `/probe` internally when calling the probe endpoint, and derives the outbound port 25 target (`host:25`) from the same URL. A trailing slash is accepted and stripped.

Because AIMX derives the outbound port-25 target from the same `verify_host` URL, the verifier service's HTTP probe and its TCP port-25 listener **must run on the same host** (or at least the same hostname in DNS). Self-hosting on a provider that blocks port 25 on the verifier host will leave AIMX clients unable to exercise the outbound check.

---

## Part B: Set Up AIMX on a Mail Server

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

### B2: Build & Install AIMX

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

### B4: Pre-Setup Verification

Run verify to confirm your server is ready before setup:

```bash
sudo aimx portcheck
```

This checks two things:

| Check | What it does | Fix if it fails |
|-------|-------------|-----------------|
| Outbound port 25 | EHLO handshake to the verifier service host on port 25 | Ask VPS provider to unblock outbound SMTP |
| Inbound port 25 | Calls `<verify_host>/probe` to connect back to your IP:25 | Open firewall, ask VPS provider to unblock inbound SMTP |

Both should show PASS.

The default verify host is `https://check.aimx.email`. To point at a self-hosted instance instead (see Part A), set `verify_host` in `config.toml` or pass `--verify-host` on the command line:

```bash
sudo aimx portcheck --verify-host https://check.yourdomain.com
```

The same flag is accepted by `aimx setup`, and takes precedence over the config value.

### B5: Run Setup Wizard

```bash
sudo aimx setup agent.yourdomain.com
```

The setup wizard performs these steps automatically:

1. Checks for root privileges and scans port 25 for existing process conflicts
2. Generates a self-signed TLS certificate at `/etc/ssl/aimx/`
3. Installs and starts `aimx serve` as a systemd/OpenRC service
4. Runs port checks (outbound 25, inbound 25 via `/probe`)
5. Generates the DKIM keypair at `/etc/aimx/dkim/` (private `0600`, public `0644`), writes `/etc/aimx/config.toml` (mode `0640`, `root:root`), and creates the catchall mailbox directory under `/var/lib/aimx/`
6. Displays the DNS records you need to add (see next step)
7. After you add the records and press Enter, resolves and validates each one

Setup ends after DNS verification. There is no end-to-end email test — run `sudo aimx portcheck` later to re-check port 25 connectivity.

### B6: DNS Configuration

After the setup wizard displays the required DNS records, add them at your domain registrar:

| Type | Name | Value | Where to set |
|------|------|-------|--------------|
| A | agent.yourdomain.com | Your server IP | Domain registrar |
| MX | agent.yourdomain.com | `10 agent.yourdomain.com.` | Domain registrar |
| TXT | agent.yourdomain.com | `v=spf1 ip4:YOUR_IP -all` | Domain registrar |
| TXT | dkim._domainkey.agent.yourdomain.com | `v=DKIM1; k=rsa; p=...` | Domain registrar |
| TXT | _dmarc.agent.yourdomain.com | `v=DMARC1; p=reject; rua=mailto:postmaster@agent.yourdomain.com` | Domain registrar |

Reverse DNS (PTR) is configured at your VPS provider's control panel and is **not** covered by `aimx setup`. A correct PTR for your server IP pointing to your mail domain improves deliverability but is the operator's responsibility as of Sprint 33.1.

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
```

### B8: End-to-End Verification

**Automated verification:**

```bash
sudo aimx portcheck
```

`aimx portcheck` runs two checks: outbound port 25 (EHLO handshake to the verifier service's built-in TCP listener) and inbound port 25 (via `/probe`, which connects back to your server). It does not send an actual email or wait for an echo reply.

**Check server health:**

```bash
aimx doctor
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

### B9: Create Mailboxes & Hooks

Create additional mailboxes beyond the default catchall:

```bash
aimx mailboxes create support
aimx mailboxes create notifications
aimx mailboxes list
```

Edit `/etc/aimx/config.toml` to configure hooks (fire on `on_receive` inbound events and `after_send` outbound events):

```toml
[mailboxes.support]
address = "support@agent.yourdomain.com"
trust = "verified"
trusted_senders = ["*@yourcompany.com"]

[[mailboxes.support.hooks]]
# name is optional — a stable 12-char hex id is derived from event+cmd if omitted
name = "support_notify"
event = "on_receive"
cmd = 'echo "New email from $AIMX_FROM: $AIMX_SUBJECT" >> /tmp/email.log'
```

Available template placeholders in commands: `{id}` and `{date}` (aimx-controlled, substituted literally). User-controlled fields are passed as environment variables: `$AIMX_HOOK_NAME`, `$AIMX_FROM`, `$AIMX_TO`, `$AIMX_SUBJECT`, `$AIMX_MAILBOX`, `$AIMX_FILEPATH`. `after_send` hooks additionally receive `$AIMX_SEND_STATUS` (`delivered`/`failed`/`deferred`). Quote env-var expansions (`"$AIMX_SUBJECT"`) to stay safe against shell metacharacters in sender-supplied values.

Trust policies:
- `trust = "none"` (default) — no trust evaluation performed. Default hooks do NOT fire. Set `dangerously_support_untrusted = true` on a hook to opt in.
- `trust = "verified"` — emails with an allowlisted sender AND a DKIM pass get `trusted = "true"`, which fires default hooks. Everything else gets `trusted = "false"` and fires no default hooks.
- `trusted_senders` — glob patterns that bypass DKIM verification

### B10: MCP Integration

To give AI agents access to the email system, install the per-agent
integration with one command:

```bash
aimx agent-setup claude-code    # or codex / opencode / gemini / goose / openclaw
```

Run `aimx agent-setup --list` to see every supported agent and its
destination path. See [`book/agent-integration.md`](../book/agent-integration.md)
for per-agent activation steps and manual MCP wiring.

Available MCP tools: `mailbox_list`, `mailbox_create`, `mailbox_delete`, `email_list`, `email_read`, `email_send`, `email_reply`, `email_mark_read`, `email_mark_unread`.

### B11: Production Hardening

**Prevent spam classification:**

1. Ensure all DNS records are correctly set (especially DKIM, SPF, DMARC).
2. (Optional but recommended) Configure a PTR / reverse-DNS record at your VPS provider pointing to your domain. This is the operator's responsibility — aimx does not check or manage PTR.
3. In Gmail: Settings > Filters > Create filter for `*@agent.yourdomain.com` > Never send to Spam.
4. Alternatively, reply to an email from the domain — Gmail learns it's not spam.

**Firewall:**

Only port 25 needs to be open for SMTP. No other ports are required by AIMX.

**File permissions:**

The DKIM private key at `/var/lib/aimx/dkim/private.key` is created with mode `0600` (owner read/write only). Verify this is intact:

```bash
ls -la /var/lib/aimx/dkim/private.key
# Should show: -rw------- 
```

**Backups:**

The only directory to back up is `/var/lib/aimx/`. It contains everything: config, DKIM keys, all mailboxes and emails.

---

## Appendix: Troubleshooting

| Problem | Diagnosis | Fix |
|---------|-----------|-----|
| Verify: outbound port 25 blocked | VPS provider blocks SMTP | Switch providers or request unblock (see compatible providers table) |
| Verify: inbound port 25 not reachable | Firewall or VPS blocks inbound | `sudo ufw allow 25/tcp`, check VPS firewall settings |
| DNS records not resolving | Propagation delay | Wait (up to 48h), re-check with `dig` |
| `sudo aimx portcheck` shows inbound FAIL | The verifier service's `/probe` endpoint could not reach your port 25 (firewall, VPS block, `aimx serve` down, or DNS `A` record not yet resolving) | `sudo systemctl status aimx`, `sudo ufw status`, `dig +short A agent.yourdomain.com`, check VPS firewall |
| Emails landing in spam | Missing DNS records, bad reverse DNS, or receiver spam filter | Add all DNS records, configure a PTR record at your VPS provider, use a Gmail filter |
| `aimx serve` not running | Service crashed or misconfigured | `sudo systemctl status aimx` and `journalctl -u aimx -e` |
| Emails not being delivered to mailbox | Ingest misconfigured | Check config and mailbox setup with `aimx doctor` |

**Useful commands:**

```bash
sudo aimx portcheck            # Re-run port 25 checks
aimx doctor                 # Show config, mailboxes, and message counts
aimx logs                   # Tail recent aimx serve logs
sudo systemctl status aimx  # Check aimx serve service status
journalctl -u aimx -e       # View aimx serve logs
```
