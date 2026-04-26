# Setup

Run the interactive setup wizard, add DNS records, and verify. This chapter assumes the binary is already installed — if not, see [Installation](installation.md) first.

For a condensed walkthrough, see [Getting Started](getting-started.md).

## Prerequisites

### Server

- A Linux VPS with port 25 open inbound **and** outbound (CI covers Ubuntu, Alpine, Fedora)
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

Both checks should pass before continuing.

## Setup wizard

Run the setup wizard:

```bash
sudo aimx setup
```

The wizard prompts for the domain interactively and confirms you have DNS access before continuing.

### First-time setup flow

The wizard opens with a welcome banner and a six-line checklist, then walks each section in spec order — ticking `☐ → ☑` (or `☒` skipped) as it goes. Two operator decisions get prompts (domain and trusted senders); everything else is driven from disk + the network.

The six sections, in order:

1. **Preflight checks on port 25.** Detects a foreign process on port 25, then verifies outbound and inbound port 25 connectivity. Runs before the domain prompt so a VPS that blocks SMTP fails fast without writing any files.
2. **Set up domain and DNS.** Prompts for the domain (skipped when supplied as argument), prints the six DNS records to add, and then enters the verify loop: pressing **Enter** re-runs the DNS checks; pressing **`q`** skips and defers verification to `aimx doctor`. The `q`-to-skip prompt is deliberately prominent because DNS propagation can take minutes or hours.
3. **Set up TLS certificate.** Generates a self-signed certificate at `/etc/ssl/aimx/`. Skipped on re-entry when the cert is already present.
4. **Set up trust policy.** Asks for a comma-separated list of addresses or glob patterns (e.g. `you@example.com, *@company.com`) that should count as trusted. Leaving the prompt blank selects `trust = "none"` and prints a loud warning that hooks will **not** fire for inbound email until you add senders by editing `/etc/aimx/config.toml` under `[trust]` / `trusted_senders`, or re-running `sudo aimx setup`. Entries are validated at prompt time; bad entries re-prompt up to five times. Under `AIMX_NONINTERACTIVE=1` the prompt is skipped, the list defaults to empty, and the same warning is *logged* (not displayed) so automated pipelines surface the misconfiguration in their log collectors. Skipped on re-entry — existing trust config is preserved.
5. **Install AIMX.** Creates a 2048-bit DKIM keypair at `/etc/aimx/dkim/` (private `0600`, public `0644`) if missing, writes `/etc/aimx/config.toml` (mode `0640`, owner `root:root`) with the catchall mailbox and the trust defaults from step 4, creates the unprivileged `aimx-catchall` system user when a catchall mailbox is configured, generates the systemd unit (or OpenRC init script on Alpine) with `RuntimeDirectory=aimx`, starts `aimx serve`, and waits for port 25 to bind. Skipped on re-entry — the existing service stays running.
6. **Set up MCP for agent(s).** Prints the per-agent install commands, then re-execs `runuser -u "$SUDO_USER" -- /proc/self/exe agents setup` via `Command::status()` so the interactive TUI takes over the terminal in one continuous flow and control returns to `aimx setup` afterwards. If `$SUDO_USER` is unset (direct root login) or `runuser` is missing, step 6 marks ☒ (skipped) with a guidance line naming `--dangerously-allow-root` as the root-login option. `AIMX_NONINTERACTIVE=1` skips the drop-through entirely. If you explicitly passed `--data-dir <path>` to `aimx setup`, the same path is threaded through to `aimx agents setup` so activation hints reference it; the default `/var/lib/aimx` is left implicit.

After step 6 returns, the wizard prints the final closing message:

```
AIMX has been set up successfully.

Your agents now have access to set up, send and receive emails from @<DOMAIN> emails.

Once you have linked up your MCP to your LLM, try asking it to set up a mailbox for you, e.g.
  claude -p "Set up agent@<DOMAIN> and respond to me via email the moment you receive my instructions via email."
```

No hook-template checkbox, no Gmail / deliverability section, no `none | verified` trust toggle. Per-user agent wiring happens via the step 6 drop-through, and deliverability is the DNS triple plus PTR (covered below).

### Step 6 summary

The MCP section prints a short summary listing the `aimx agents setup` commands for each supported agent before re-execing the TUI. The summary is informational — the drop-through itself handles the actual wiring. After the TUI exits, `aimx setup` prints the closing message and the wizard returns control to the shell.

Third-party mail-client workarounds (e.g. Gmail spam-filter whitelists) are **not** part of `aimx setup`'s surface. A correct SPF / DKIM / DMARC triple plus a reverse-DNS (PTR) record at your VPS provider is the canonical deliverability story.

### Catchall user (created on demand)

When you configure a catchall mailbox, setup creates the `aimx-catchall` system user via `useradd --system --no-create-home --shell /usr/sbin/nologin` (or the BusyBox `adduser` equivalent on Alpine) and chowns the catchall mailbox to it. If you skip the catchall, no `aimx-catchall` user is created.

`aimx-catchall` is the only system user that `aimx setup` creates. The catchall mailbox itself never executes hooks — `Config::load` rejects any `[[mailbox.<catchall>.hook]]` block because the catchall user has no shell and no resolvable login uid that `setuid` can drop into. To run automation on inbound mail, create a non-catchall mailbox owned by a regular Linux user and attach hooks to it after setup.

### Provisioning your first mailbox

The setup wizard does **not** prompt for a first mailbox. After setup completes, provision mailboxes from the host CLI as root:

```bash
sudo aimx mailboxes create hi --owner ubuntu
```

This registers `hi@agent.yourdomain.com`, creates `inbox/hi/` and `sent/hi/` chowned `ubuntu:ubuntu 0700`, and hot-reloads the daemon's in-memory config. The mailbox's owner (here `ubuntu`) can then read mail, send mail, and create hooks via CLI or MCP — no further root commands required.

`aimx mailboxes create` and `aimx mailboxes delete` are root-only on both the CLI and the UDS (`MAILBOX-CREATE` / `MAILBOX-DELETE`). The `aimx mailboxes list` command is filtered to caller-owned mailboxes for non-root callers; `--all` is root-only.

### Wiring agents

The `aimx agents setup` drop-through (step 6) runs as your regular user (not as root) and presents an interactive checkbox picker. For each selected agent, it lays down plugin files under the caller's `$HOME`. The plugin teaches the agent how to call aimx's MCP tools, including a "Wiring yourself up as a mailbox hook" section with the verified `cmd` argv to use with `hook_create`. See [Agent integration](agent-integration.md) for the full flow and troubleshooting.

If you logged in directly as root (no `sudo`), the wizard prints a message pointing you at the same tool. You can either re-run `aimx agents setup` as a regular user on the box, or pass `--dangerously-allow-root` if this is a single-user VPS where you genuinely want AIMX wired into `root`'s home.

### Non-interactive setup

Setting `AIMX_NONINTERACTIVE=1` skips the trusted-senders prompt (defaults to empty list + logged warning) and skips the agents-setup drop-through (no TTY assumed). Useful for provisioning scripts and CI. The domain still must be supplied as an argument.

```bash
sudo AIMX_NONINTERACTIVE=1 aimx setup agent.example.com
```

### Re-running setup

Re-run `aimx setup` on an existing install to re-verify DNS or wire an additional agent:

```bash
sudo aimx setup
```

When aimx detects an existing configuration (`aimx serve` running, TLS cert present, DKIM key present), the wizard's checklist marks **TLS certificate**, **trust policy**, and **install AIMX** as ☒ (skipped) so you can see at a glance that nothing was rewritten. Steps 1, 2, and 6 still run: port 25 preflight, DNS verification, and the agents-setup drop-through. Re-entry is the natural "I want to wire another agent" checkpoint; the TUI's `(AIMX MCP wired)` state is self-documenting so you will not double-wire anything by accident.

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
