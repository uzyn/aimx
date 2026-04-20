# Troubleshooting

Diagnostic commands and solutions for common issues.

## Diagnostic commands

```bash
# Check port 25 connectivity (outbound + inbound EHLO handshake)
# Requires root
sudo aimx portcheck

# Show server health, configuration, mailbox counts, and a tail of the service log
aimx doctor

# Stream the service log on its own (last 50 lines by default)
aimx logs
aimx logs --lines 200
aimx logs --follow

# Test against a self-hosted verify service instead of the default
sudo aimx portcheck --verify-host https://verify.yourdomain.com
```

The `--verify-host` flag is also accepted by `aimx setup`, and overrides the `verify_host` value from `config.toml` for the current invocation.

## Common issues

| Problem | Cause | Fix |
|---------|-------|-----|
| Verify: outbound port 25 blocked | VPS provider blocks SMTP | Switch providers or request unblock (see [compatible providers](getting-started.md#compatible-vps-providers)) |
| Verify: inbound port 25 not reachable | Firewall or VPS blocks inbound | `sudo ufw allow 25/tcp`, check VPS firewall settings |
| DNS records not resolving | Propagation delay | Wait (up to 48h), re-check with `dig` (see [verifying DNS](setup.md#verifying-dns-records)) |
| `sudo aimx portcheck` times out | DNS not propagated or verify service down | Run again later; check `curl https://check.aimx.email/health` |
| Emails landing in spam | Missing DNS records, bad reverse DNS, or receiver spam filter | Add all [DNS records](setup.md#dns-configuration), configure a PTR record at your VPS provider, use a Gmail filter |
| `aimx serve` not running | Service crashed or not started | Check status and logs (see below) |
| Emails not delivered to mailbox | `aimx serve` not running or misconfigured | Check service status with `systemctl status aimx` |
| Hooks not firing | Trust gate | `on_receive` hooks fire iff `trusted == "true"` OR the hook sets `dangerously_support_untrusted = true`. Check `trust` / `trusted_senders` and the email's DKIM result. See [trust gate](hooks.md#trust-gate-on_receive-only). |
| DKIM verification failing | DNS record mismatch or key regenerated | Ensure DKIM DNS record matches current public key |

## `aimx serve` diagnostics

```bash
# Check if aimx serve is running
sudo systemctl status aimx

# View recent aimx serve logs
journalctl -u aimx -e

# Restart the service
sudo systemctl restart aimx

# Clear a rate-limited service after repeated crashes
# (the unit caps restarts at StartLimitBurst=5 within StartLimitIntervalSec=60s)
sudo systemctl reset-failed aimx
```

If `systemctl status aimx` reports `start-limit-hit`, the service has restarted too often in a short window. Run `sudo systemctl reset-failed aimx` to clear the counter, then `sudo systemctl start aimx` to try again. Investigate the underlying crash in `journalctl -u aimx -e` before restarting.

On Alpine Linux (OpenRC):

```bash
# Check service status
rc-service aimx status

# View recent logs (OpenRC logs to the supervise-daemon log output)
less /var/log/messages

# Restart
rc-service aimx restart
```

## Where are the logs?

AIMX does not write its own log files. Output from `aimx serve` goes to stdout/stderr and is captured by the init system. The first-line debugging command is `aimx logs`, which wraps the right tool for the running init system:

```bash
# Tail the last 50 lines (default)
aimx logs

# Tail a custom number of lines
aimx logs --lines 200

# Follow new lines as they arrive (Ctrl-C to stop)
aimx logs --follow
```

`aimx doctor` prints a `Logs` pointer section at the bottom of its output that reminds you to run `aimx logs` (or `aimx logs --follow`) rather than dumping log lines itself.

**systemd (Ubuntu, Fedora, Debian, etc.)**

The systemd unit declares `StandardOutput=journal` and `StandardError=journal`, so all daemon output is routed to journald. `aimx logs` shells out to `journalctl -u aimx -n <N>` (and `journalctl -f -u aimx` with `--follow`). You can also call journalctl directly:

```bash
# Follow logs in real time
journalctl -u aimx -f

# Show today's logs
journalctl -u aimx --since today

# Show last 200 lines
journalctl -u aimx -n 200
```

**OpenRC (Alpine)**

The generated OpenRC init script uses `supervise-daemon`, which routes daemon output to the system logger (typically `/var/log/messages` or syslog). Check your OpenRC logging configuration for the exact destination. `aimx logs` makes a best-effort read of `/var/log/aimx/*.log` and falls back to `/var/log/messages`; `aimx logs --follow` is unsupported on OpenRC and will direct you to tail your syslog file directly.

## DKIM/SPF debugging

### Check DKIM DNS record

```bash
dig +short TXT aimx._domainkey.agent.yourdomain.com
```

The output should contain `v=DKIM1; k=rsa; p=...` matching your public key.

To see the current public key:

```bash
cat /etc/aimx/dkim/public.key
```

### Check SPF record

```bash
dig +short TXT agent.yourdomain.com
```

Should include `v=spf1 ip4:YOUR_SERVER_IP -all`.

### Check DMARC record

```bash
dig +short TXT _dmarc.agent.yourdomain.com
```

Should include `v=DMARC1; p=reject`.

### Verify email authentication results

Read an email's frontmatter to check inbound verification results:

```bash
head -20 /var/lib/aimx/inbox/catchall/*.md
```

Look at the `dkim` and `spf` fields -- they should show `pass` for properly authenticated senders.

## Spam prevention

If outbound emails land in spam:

1. **Check all DNS records** -- DKIM, SPF, and DMARC must all be set correctly. See [DNS configuration](setup.md#dns-configuration).
2. **Configure reverse DNS (PTR)** at your VPS provider's control panel so the PTR for your server IP points to your mail domain. This is the operator's responsibility and is out of scope for aimx, but is critical for deliverability with Gmail/Outlook.
3. **Gmail filter workaround** -- In Gmail: Settings > Filters > Create filter for `*@agent.yourdomain.com` > Never send to Spam.
4. **Reply trick** -- Reply to one email from the domain. Gmail learns it's not spam.

## File permissions

Verify the DKIM private key has correct permissions:

```bash
ls -la /etc/aimx/dkim/private.key
# Should show: -rw------- (mode 0600)
```

If permissions are wrong:

```bash
sudo chmod 600 /etc/aimx/dkim/private.key
```

## How portcheck works

`aimx portcheck` requires root and auto-detects what is listening on port 25:

| Scenario | What happens |
|----------|-------------|
| **`aimx serve` running** | Outbound EHLO + inbound EHLO probe |
| **Other process on port 25** (Postfix, Exim, etc.) | Fails -- advises to stop the conflicting process |
| **Nothing on port 25** (fresh VPS) | Spawns temporary SMTP listener, then runs outbound + inbound EHLO checks |

If portcheck fails with EHLO probe after setup, the issue is likely in the `aimx serve` configuration rather than firewall/port access. Run `sudo systemctl status aimx` to check.

## Useful commands reference

| Command | Purpose |
|---------|---------|
| `sudo aimx portcheck` | Check port 25 connectivity (requires root) |
| `aimx doctor` | Show config, mailboxes, message counts, DNS record verification, and recent log lines |
| `aimx logs [--lines N] [--follow]` | Tail or follow the aimx service log |
| `aimx mailboxes list` | List all mailboxes |
| `aimx dkim-keygen` | Generate DKIM keypair |
| `aimx dkim-keygen --force` | Regenerate DKIM keys (update DNS record after) |
| `aimx --data-dir /path doctor` | Use a custom data directory |
| `sudo systemctl status aimx` | Check aimx serve service |
| `journalctl -u aimx -e` | View aimx serve logs (or use `aimx logs`) |
| `dig +short TXT agent.yourdomain.com` | Check DNS records |
| `cat /etc/aimx/dkim/public.key` | View DKIM public key |

---

Back to: [Index](index.md) | [Setup](setup.md) | [Configuration](configuration.md)
