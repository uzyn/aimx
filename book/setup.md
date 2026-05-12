# Setup

Run the setup wizard, add DNS records, and verify. Assumes the binary is installed; see [Installation](installation.md) first if not.

For a condensed walkthrough, see [Getting Started](getting-started.md).

## Prerequisites

### Server

- A Linux server with port 25 open inbound **and** outbound
- A domain (or subdomain) you control with access to DNS management
- Root access on the server (required for service installation and binding port 25)

### Firewall

Ensure port 25 is open:

```bash
# If using ufw:
sudo ufw allow 25/tcp

# If using iptables:
sudo iptables -A INPUT -p tcp --dport 25 -j ACCEPT
```

## Setup wizard

Run the setup wizard:

```bash
sudo aimx setup
```

The wizard prompts for the domain interactively and confirms you have DNS access before continuing.

### First-time setup flow

The wizard prints a six-line checklist and walks each step, advancing each entry from `☐` pending to one of `☑` done, `☒` skipped, or `⎘` handoff as it goes. Only the domain and trusted-sender list need operator input; everything else comes from disk and the network.

1. **Port 25 preflight.** Verifies outbound and inbound port 25 connectivity, refusing to continue when SMTP is blocked. Runs before any file is written.
2. **Domain and DNS.** Prompts for the domain (skipped when supplied as an argument), prints the DNS records, and enters the verify loop. Press **Enter** to re-run the checks, **`q`** to skip and defer to `aimx doctor`.
3. **STARTTLS certificate.** Generates a self-signed cert at `/etc/ssl/aimx/`. Skipped on re-entry.
4. **Trust policy.** Asks for a comma-separated list of addresses or globs (e.g. `you@example.com, *@company.com`). An empty list sets `trust = "none"` with a loud warning that hooks will not fire for inbound email until trusted senders are added. Skipped on re-entry. Under `AIMX_NONINTERACTIVE=1` the prompt is skipped and the warning is logged.
5. **Install AIMX.** Generates a 2048-bit DKIM keypair under `/etc/aimx/dkim/`, writes `/etc/aimx/config.toml` with the catchall mailbox, creates the `aimx-catchall` system user, generates the systemd (or OpenRC) service unit, starts `aimx serve`, and waits for port 25 to bind. Skipped on re-entry.
6. **Set up MCP for agent(s).** Prints the `🐦‍⬛ AIMX setup — complete` banner first, then a guidance section listing every supported AI agent and harness (driven by `agents_setup::registry()` so the list extends automatically), and a boxed `→ aimx agents setup` callout under a sentence directing the operator to run the agents guided setup as a regular user (not root). The wizard does not spawn a subprocess — agent wiring is operator-initiated as a separate step (the same idiom as `apt install` / `gh auth login`). Marked `⎘ Handoff` on the checklist regardless of `$SUDO_USER` or `AIMX_NONINTERACTIVE`; the checklist itself is rendered immediately after Step 5 completes (with Step 6 still as `☐`) and is not reprinted under the banner.

After step 6 returns, the wizard prints the final closing message:

```
🐦‍⬛ AIMX setup — complete

Step 6: Set up MCP for agent(s)

AIMX bundles MCP and skill files for the following AI agents & harnesses:

  • Claude Code
  • Codex CLI
  • OpenCode
  • Gemini CLI
  • Goose
  • OpenClaw
  • Hermes
  • NanoClaw

To wire AIMX into the harness you have installed, run the agents guided setup as a regular user (not root):

  ┌─────────────────────────────┐
  │  → aimx agents setup        │
  └─────────────────────────────┘


☑ AIMX has been set up successfully.

AIMX is now running at @<DOMAIN> and ready to send and receive email.

Once you've wired your MCP to your LLM, try asking your agent, e.g.
  claude -p "Set up agent@<DOMAIN> and respond to me via email the moment you receive my instructions via email."
```

Third-party mail-client workarounds (Gmail spam-filter whitelists and similar) are not part of `aimx setup`. The canonical deliverability story is the SPF / DKIM / DMARC triple plus a reverse-DNS (PTR) record at your VPS provider.

### Catchall user

When the catchall is configured, setup creates the `aimx-catchall` system user (`useradd --system --no-create-home --shell /usr/sbin/nologin`, or the BusyBox `adduser` equivalent on Alpine) and chowns the catchall mailbox to it. Skipping the catchall skips the user.

The catchall is inbound-only and cannot run hooks: `aimx-catchall` has no shell and no resolvable login uid for `setuid` to drop into, so `Config::load` rejects any hook attached to a catchall mailbox. Wire automation on a non-catchall mailbox owned by a regular Linux user.

### Provisioning your first mailbox

The wizard does not prompt for a mailbox. Provision one yourself after setup:

```bash
# As yourself, no sudo:
aimx mailboxes create hi
```

This registers `hi@agent.yourdomain.com`, creates `inbox/hi/` and `sent/hi/` chowned to your uid, and hot-reloads the daemon. To provision a mailbox owned by a different Linux user (a service account, an agent uid), pass `--owner <user>` under `sudo`:

```bash
sudo aimx mailboxes create support --owner support-agent
```

Mailbox CRUD is owner-gated. See [Mailboxes § Managing mailboxes](mailboxes.md#managing-mailboxes) for the full rules.

### Wiring agents

Step 6 of `aimx setup` prints the supported-agent list and the `→ aimx agents setup` callout, then exits. It does not spawn the agent picker. To wire AIMX into your agents, run `aimx agents setup` yourself as your regular user after setup exits:

```bash
aimx agents setup
```

This launches the interactive picker. For each selected agent it writes plugin files under `$HOME` and (for Claude Code and Codex CLI) auto-registers the MCP server. The plugin teaches the agent how to call AIMX's MCP tools and includes a "Wiring yourself up as a mailbox hook" recipe. See [Agent Integration](agent-integration.md).

Step 6's terminal state in the checklist is always `⎘ Handoff` — it's a handoff back to the operator, not a wizard-completed step. On a single-user root-login VPS where there is no separate regular user, pass `aimx agents setup --dangerously-allow-root` to wire AIMX into root's home.

### Non-interactive setup

Setting `AIMX_NONINTERACTIVE=1` skips the trusted-senders prompt (defaults to empty list + logged warning) and the mailbox-owner prompt. Useful for provisioning scripts and CI. The domain still must be supplied as an argument. Step 6 is unaffected — it always prints the same `→ aimx agents setup` guidance regardless of `AIMX_NONINTERACTIVE`, because there is no subprocess to skip.

```bash
sudo AIMX_NONINTERACTIVE=1 aimx setup agent.example.com
```

### Re-running setup

Re-run `sudo aimx setup` to re-verify DNS. The wizard detects existing config (`aimx serve` running, STARTTLS cert present, DKIM key present) and marks the STARTTLS / trust / install steps as ☒ skipped — only the preflight and DNS verification steps run, and Step 6 reprints the agent-wiring guidance with the `⎘ Handoff` marker. To (re-)wire an agent after re-entry, run `aimx agents setup` yourself as your regular user; the picker shows `(AIMX MCP wired)` next to already-wired agents so it won't double-wire anything.

### DNS retry loop

At the DNS verification step:

- **Enter** re-checks DNS records (useful after updating DNS in another tab).
- **`q`** defers verification; re-run `sudo aimx setup` later or use `aimx doctor`.

### Uninstalling

To reverse `aimx setup`, stop the daemon, remove its init-system service file, and delete the installed `aimx` binary:

```bash
sudo aimx uninstall
```

Pass `--yes` to skip the confirmation prompt. Uninstall is intentionally scoped: it removes the service and the binary so a subsequent `install.sh` run starts fresh, but leaves your config (`/etc/aimx/`) and mailbox data (`/var/lib/aimx/`) in place so a subsequent `aimx setup` reuses them. If you also want to wipe those, remove them manually with `rm -rf`.

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

Reverse DNS (PTR) is configured at your VPS provider's control panel and is **not** covered by `aimx setup`. It is out of scope for AIMX. A correct PTR record pointing to your domain does improve deliverability. See the VPS provider's documentation for how to set it.

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

- **Deliverability.** Set DKIM, SPF, DMARC (and AAAA if you serve IPv6) correctly. Configure a PTR record at your VPS provider — AIMX does not manage PTR.
- **Firewall.** Only port 25 needs to be open. No other ports are required.
- **File permissions.** The DKIM private key is `0600 root:root`. Verify with `ls -la /etc/aimx/dkim/private.key`.
- **Backups.** Back up `/etc/aimx/` (config + DKIM keys) and `/var/lib/aimx/` (mailboxes). `/run/aimx/` is runtime-only.

## Verifier service

The verifier service is used during setup to test port 25 reachability. AIMX uses a public instance at `check.aimx.email` by default.

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

4. Point AIMX to your instance in `config.toml`:
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

See the [verifier service README](https://github.com/uzyn/aimx/blob/main/services/verifier/README.md) for full details.
