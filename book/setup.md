# Setup

Prerequisites, build, setup wizard, DNS records, verification, DKIM keys, and production hardening. For a shorter walkthrough, see [Getting Started](getting-started.md).

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

Verify port 25 connectivity before running setup:

```bash
sudo aimx portcheck
```

`aimx portcheck` requires root. When `aimx serve` is running, it performs an outbound EHLO handshake plus an inbound EHLO handshake probe. When nothing is on port 25 (fresh VPS), it spawns a temporary listener and runs checks. If port 25 is occupied by another process, portcheck tells you to stop it before setup.

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

1. **Root check**: exits if not running as root.
2. **Port 25 preflight**: checks for a foreign process on port 25, then verifies outbound and inbound port 25 connectivity. Runs before the domain prompt so a VPS that blocks SMTP fails fast without asking for a domain or writing any files.
3. **Domain prompt**: asks for domain if not provided as argument.
4. **Trusted-senders prompt**: asks for a comma-separated list of addresses or glob patterns (e.g. `you@example.com, *@company.com`) that should count as trusted. Leaving the prompt blank selects `trust = "none"` and prints a loud warning that hooks will **not** fire for inbound email. Entries are validated at prompt time; bad entries re-prompt up to five times. Under `AIMX_NONINTERACTIVE=1` the prompt is skipped, the list defaults to empty, and the same warning is *logged* (not displayed) so automated pipelines surface the misconfiguration in their log collectors. Re-entry preserves existing trust config.
5. **TLS certificate**: generates a self-signed certificate at `/etc/ssl/aimx/`.
6. **DKIM key generation + config creation**: creates a 2048-bit RSA keypair at `/etc/aimx/dkim/` (private `0600`, public `0644`) and writes `/etc/aimx/config.toml` (mode `0640`, owner `root:root`) with the catchall mailbox and the trust defaults from step 4.
7. **Catchall user (on demand)**: when you configure a catchall mailbox, setup creates the unprivileged `aimx-catchall` system user (no login shell, no home directory) and chowns the catchall mailbox to it. No `aimx-catchall` is created if you skip the catchall — and no `aimx-hook` group is ever touched; that legacy shared-group model has been retired.
8. **DNS guidance + verification loop**: shows the records to add and re-verifies on Enter. Press `q` to skip verification and run `aimx doctor` later.
9. **Service installation + success banner**: generates a systemd unit (or OpenRC init script on Alpine) with `RuntimeDirectory=aimx`, starts `aimx serve`, and prints the single-line `aimx is running for <domain>.` banner.
10. **Drop-through to `aimx agent-setup`**: when `$SUDO_USER` is set (i.e. you ran the wizard via `sudo aimx setup`, not a direct root login), the wizard re-execs `runuser -u "$SUDO_USER" -- /proc/self/exe agent-setup` so the interactive checkbox TUI takes over the terminal in one continuous flow. If `$SUDO_USER` is unset (direct root login), the wizard prints the guidance message naming `--dangerously-allow-root` as a root-login option, and exits cleanly. `AIMX_NONINTERACTIVE=1` skips the drop-through entirely. If you explicitly passed `--data-dir <path>` to `aimx setup`, the same path is threaded through to `aimx agent-setup` so activation hints reference it; the default `/var/lib/aimx` is left implicit.

No hook-template checkbox UI and no Gmail / deliverability section appear in the wizard — per-user agent wiring happens post-setup via `aimx agent-setup`, and deliverability is the DNS triple plus PTR (see below).

After initial setup, the wizard displays two clearly labeled sections:

- **[DNS]**: the records you need to add (MX, A, AAAA, SPF, DKIM, DMARC), with a retry loop so you can press Enter to re-verify after adding records (or press `q` to skip and re-verify later with `aimx doctor`)
- **[MCP]**: informational summary of the `aimx agent-setup` commands per supported agent — the actual agent wiring happens in a post-install drop-through (see [Agent integration](agent-integration.md)).

Third-party mail-client workarounds (e.g. Gmail spam-filter whitelists) are **not** part of `aimx setup`'s surface. A correct SPF / DKIM / DMARC triple plus a reverse-DNS (PTR) record at your VPS provider is the canonical deliverability story.

### Mailbox-owner prompt

For every mailbox you configure, setup asks which Linux user should own it:

```text
[Mailboxes]
Which Linux user should own `alice@agent.yourdomain.com`? [alice]
```

- The default (in brackets) is the address's local part if that user exists on the host. Press Enter to accept.
- If the default user does not exist, setup requires explicit input and rejects unknown usernames with a hint to `useradd` first.
- The catchall mailbox (if any) is always owned by the reserved `aimx-catchall` system user.

The daemon chowns `/var/lib/aimx/inbox/<mailbox>/` and `/var/lib/aimx/sent/<mailbox>/` to `<owner>:<owner>` mode `0700` at create time and keeps ownership consistent through every subsequent write (ingest, send, mark-read). Only the owner and root can read a mailbox's contents — there is no shared `aimx-hook` group in the new model.

### Catchall user (created on demand)

When you configure a catchall mailbox, setup creates the `aimx-catchall` system user via `useradd --system --no-create-home --shell /usr/sbin/nologin` (or the BusyBox `adduser` equivalent on Alpine) and chowns the catchall mailbox to it. If you skip the catchall, no `aimx-catchall` user is created. The legacy `aimx-hook` shared-group model has been retired; setup does not create or chown against `aimx-hook` under any flow.

### Registering agent templates

Per-agent hook templates (Claude Code, Codex, OpenCode, Gemini, Goose, OpenClaw, etc.) are no longer ticked from a checkbox during setup. Instead, each Linux user who wants an agent runs one command **without sudo** after setup:

```bash
aimx agent-setup claude-code
```

`aimx agent-setup` lays down the plugin files under the caller's `$HOME`, probes `$PATH` for the agent binary, and registers a matching `invoke-<agent>-<username>` template over the UDS. See [Agent integration](agent-integration.md) for the full flow and troubleshooting.

### Re-running setup

Re-run `aimx setup` on an existing install to re-verify:

```bash
sudo aimx setup agent.yourdomain.com
```

When aimx detects an existing configuration (`aimx serve` running, TLS cert present, DKIM key present), it skips install/configure and runs the port 25 preflight, DNS verification, and the output sections as a quick verification pass.

### DNS retry loop

At the DNS verification step:

- **Enter** re-checks DNS records (useful after updating DNS in another tab).
- **q** defers verification; re-run `sudo aimx setup <domain>` later.

### Uninstalling

To reverse `aimx setup`, stop the daemon and remove its init-system service file:

```bash
sudo aimx uninstall
```

Pass `--yes` to skip the confirmation prompt. Uninstall is intentionally non-destructive: it leaves your config (`/etc/aimx/`) and mailbox data (`/var/lib/aimx/`) in place so a subsequent `aimx setup` reuses them. If you also want to wipe those, remove them manually with `rm -rf`.

## DNS configuration

After the setup wizard displays the required DNS records, add them at your domain registrar:

| Type | Name | Value | Where to set |
|------|------|-------|--------------|
| A | `agent.yourdomain.com` | Your server IPv4 | Domain registrar |
| AAAA | `agent.yourdomain.com` | Your server IPv6 (if available) | Domain registrar |
| MX | `agent.yourdomain.com` | `10 agent.yourdomain.com.` | Domain registrar |
| TXT | `agent.yourdomain.com` | `v=spf1 ip4:YOUR_IP -all` (or `v=spf1 ip4:YOUR_IP ip6:YOUR_IPV6 -all` with IPv6) | Domain registrar |
| TXT | `aimx._domainkey.agent.yourdomain.com` | `v=DKIM1; k=rsa; p=...` | Domain registrar |
| TXT | `_dmarc.agent.yourdomain.com` | `v=DMARC1; p=reject` | Domain registrar |

Reverse DNS (PTR) is configured at your VPS provider's control panel and is **not** covered by `aimx setup`. It is out of scope for aimx. A correct PTR record pointing to your domain does improve deliverability. See the VPS provider's documentation for how to set it.

The `AAAA` record and SPF `ip6:` mechanism are only shown and verified by `aimx setup` when `enable_ipv6 = true` is set in `config.toml`. See [IPv6 delivery (advanced)](configuration.md#ipv6-delivery-advanced). By default, `aimx serve` delivers over IPv4 only and the single `ip4:` SPF mechanism is sufficient. Any existing AAAA record in DNS is left alone.

The DKIM public key value (`p=...`) is displayed by the setup wizard. To retrieve it again:

```bash
cat /etc/aimx/dkim/public.key
```

DNS propagation typically takes minutes but can take up to 48 hours.

### Verifying DNS records

After adding DNS records, verify them manually:

```bash
# A record
dig +short A agent.yourdomain.com
# Expected: your server IPv4 address

# AAAA record (if your server has IPv6)
dig +short AAAA agent.yourdomain.com
# Expected: your server IPv6 address

# MX record
dig +short MX agent.yourdomain.com
# Expected: 10 agent.yourdomain.com.

# SPF record
dig +short TXT agent.yourdomain.com
# Should include: v=spf1 ip4:YOUR_IP -all (or v=spf1 ip4:YOUR_IP ip6:YOUR_IPV6 -all)

# DKIM record
dig +short TXT aimx._domainkey.agent.yourdomain.com
# Should include: v=DKIM1; k=rsa; p=...

# DMARC record
dig +short TXT _dmarc.agent.yourdomain.com
# Should include: v=DMARC1; p=reject
```

## End-to-end verification

Run the automated verification:

```bash
sudo aimx portcheck
```

This tests outbound port 25 connectivity (via EHLO handshake) and inbound SMTP reachability (via EHLO probe). Requires root.

Check server health:

```bash
aimx doctor
```

### Manual testing

**Inbound:** Send an email from an external account (e.g. Gmail) to `catchall@agent.yourdomain.com`, then check:

```bash
ls /var/lib/aimx/inbox/catchall/
cat /var/lib/aimx/inbox/catchall/*.md
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
# Generate DKIM keypair (default selector: "aimx")
aimx dkim-keygen

# Force regenerate (overwrites existing keys)
aimx dkim-keygen --force

# Use a custom selector
aimx dkim-keygen --selector mykey
```

Keys are stored at:
- Private key: `/etc/aimx/dkim/private.key` (mode `0600`, root-only)
- Public key: `/etc/aimx/dkim/public.key` (mode `0644`)

After regenerating keys, update the DKIM DNS record with the new public key.

## Production hardening

### Preventing spam classification

1. Ensure all DNS records are correctly set (especially DKIM, SPF, DMARC, and AAAA if your server has IPv6).
2. (Optional but recommended) Configure a PTR / reverse-DNS record at your VPS provider pointing to your domain. This is the operator's responsibility. aimx does not check or manage PTR.
3. Third-party mail-client whitelists (e.g. Gmail filter rules) are out of scope for aimx. If your recipients' provider misclassifies well-signed mail, it is a client-side workaround; configure it on their end, not in aimx.

### Firewall

Only port 25 needs to be open for SMTP. No other ports are required by aimx.

### File permissions

The DKIM private key is created with mode `0600` (root-only). Verify:

```bash
ls -la /etc/aimx/dkim/private.key
# Should show: -rw-------
```

### Backups

Back up both `/etc/aimx/` (config + DKIM keys) and `/var/lib/aimx/` (mailboxes and emails). `/run/aimx/` is runtime-only and does not need backup.

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
   ```bash
   sudo aimx portcheck --verify-host https://verify.yourdomain.com
   ```

The verifier service provides:
- `GET /health`: health check
- `GET /probe`: connects back to caller's IP on port 25, performs EHLO handshake
- Port 25 listener: accepts TCP connections for outbound port 25 testing

See the [verifier service README](../services/verifier/README.md) for full details.

---

Next: [Configuration](configuration.md) | [Troubleshooting](troubleshooting.md)
