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
| Hooks not firing | Trust gate | `on_receive` hooks fire iff `trusted == "true"` OR the hook sets `fire_on_untrusted = true`. Check `trust` / `trusted_senders` and the email's DKIM result. See [trust gate](hooks.md#trust-gate-on_receive-only). |
| DKIM verification failing | DNS record mismatch or key regenerated | Ensure DKIM DNS record matches current public key |

## Restarting setup from scratch

`aimx setup` is idempotent: re-running it preserves the operator's prior trust policy and skips TLS / install steps once they are in place. The wizard now generates the DKIM keypair early (step 2, while rendering the DNS guidance table), so a hard reset means clearing more than just `config.toml`.

To wipe a partially-installed host and start from a clean slate:

```bash
# Stop the daemon if it's running.
sudo systemctl stop aimx 2>/dev/null || sudo rc-service aimx stop 2>/dev/null || true

# Remove config + DKIM keys + (optionally) the self-signed TLS cert.
sudo rm -rf /etc/aimx/config.toml /etc/aimx/dkim/
sudo rm -rf /etc/ssl/aimx/   # only if you want a fresh TLS cert

# Re-run the wizard.
sudo aimx setup
```

Mailbox data under `/var/lib/aimx/` is preserved across re-runs by design — delete it explicitly if you want to start with empty `inbox/` and `sent/` trees as well. Aborting the wizard at the trust prompt leaves the DKIM keypair on disk; the next `sudo aimx setup` picks up where you left off, so wiping is only needed when you actually want a fresh DKIM key (e.g. after publishing the wrong public key to DNS).

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

### Version drift between client and daemon

`aimx doctor` renders two version lines under the Service section:

```
Client version:   v1.2.4 (a1b2c3d4)
Server version:   v1.2.3 (9e8f7d6c)
```

The Client line reports the build of the `aimx` binary you just invoked. The Server line reports what the running `aimx serve` daemon advertises over the UDS `VERSION` verb. They drift apart when an upgrade replaces the on-disk binary but does not restart the long-running daemon — typically a `curl | sh` re-install on a host where systemd is present-but-inactive, or a manually-launched `aimx serve` outside the service manager.

The lines are informational only — `aimx doctor` does not flag a finding and does not change its exit code. To resolve drift, restart the service so the daemon picks up the new binary:

```bash
sudo systemctl restart aimx
# or, on OpenRC:
sudo rc-service aimx restart
```

If the Server line renders `(daemon not running)` the daemon is offline; start it with `sudo systemctl start aimx`. A `(<reason>)` placeholder means the socket exists but the probe failed within the 500 ms budget — check `aimx logs` for the daemon-side error.

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

The generated OpenRC init script uses `supervise-daemon`, which routes daemon output to the system logger (typically `/var/log/messages` or syslog). Check your OpenRC logging configuration for the exact destination. `aimx logs` makes a best-effort read of `/var/log/aimx/*.log` and falls back to `/var/log/messages`. `aimx logs --follow` is unsupported on OpenRC and will direct you to tail your syslog file directly.

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

Look at the `dkim` and `spf` fields. They should show `pass` for properly authenticated senders.

## Hooks and ownership

### Mailbox owner does not exist on the host

Symptom: `aimx doctor` Ownership section flags a mailbox with `[FAIL] user not found` for its owner, and hook fires for that mailbox are soft-skipped with a WARN carrying `reason = "owner-not-found"`.

Fix: the mailbox's `owner =` value points at a Linux user that does not resolve via `getpwnam(3)` on this host (typo in `config.toml`, or the user was removed via `userdel`). Either create the missing user (`sudo useradd --system --no-create-home --shell /usr/sbin/nologin <name>`) and fix up mailbox directory ownership manually:

```bash
sudo chown -R <owner>:<owner> /var/lib/aimx/inbox/<mailbox> /var/lib/aimx/sent/<mailbox>
sudo chmod -R u+rwX,go-rwx /var/lib/aimx/inbox/<mailbox> /var/lib/aimx/sent/<mailbox>
```

Or re-assign the mailbox to a user that does exist by hand-editing `[mailboxes.<name>]` in `/etc/aimx/config.toml` and `sudo systemctl reload aimx`. Doctor's overall exit code is non-zero whenever any mailbox has an unresolvable owner so monitoring can detect orphans.

### Hook on catchall is forbidden

Symptom: `Config::load` fails on daemon startup with `catchall does not support hooks`, or `aimx hooks create --mailbox catchall` returns `EACCES catchall does not support hooks`.

Fix: `aimx-catchall` has no shell and no resolvable login uid that `setuid` can drop into, so hooks on the catchall mailbox have no safe owner to execute as. Move the hook to a non-catchall mailbox owned by a regular user, or — if the goal is "notify on every inbound mail" — create a separate non-catchall mailbox (`sudo aimx mailboxes create notify --owner ubuntu`) and attach the hook there.

### `fire_on_untrusted` rejected on `after_send`

Symptom: `Config::load` fails on daemon startup with `fire_on_untrusted is on_receive only`, or `aimx hooks create --event after_send --fire-on-untrusted` is rejected.

Fix: `fire_on_untrusted` is the trust-gate escape hatch for `on_receive` hooks (which fire only on trusted mail by default). It has no meaning on `after_send` because there is no trust gate on outbound delivery. Remove the flag from any `after_send` hook entry.

### `MAILBOX-CREATE` / `MAILBOX-DELETE` rejected for non-root

Symptom: a non-root call to `aimx mailboxes create` or `aimx mailboxes delete` is rejected with exit code 2, or a `MAILBOX-CREATE` / `MAILBOX-DELETE` UDS request from a non-root caller returns `EACCES not authorized`.

Fix: mailbox CRUD is root-only — both verbs check the caller's uid via `SO_PEERCRED` and refuse anything other than uid 0. Provision mailboxes with `sudo aimx mailboxes create <name> --owner <user>`; the named owner can then CRUD hooks and read/send mail without further root commands. The previous "any local user can create mailboxes via UDS" stance has been retired.

### `aimx send` returns `not authorized: <local_part>@<domain>`

Symptom: `aimx send --from alice@agent.yourdomain.com ...` exits 1 with `not authorized: alice@agent.yourdomain.com` even though the mailbox exists.

Fix: `aimx send` validates the `From:` local part against the caller's owned mailboxes. The mailbox owner (the Linux user named in `[mailboxes.<name>]` `owner =`) is the only non-root caller authorized to send as that address. Run `aimx send` as the mailbox's owner (`sudo -u <owner> aimx send ...` if you're already root), or re-assign ownership in `config.toml`. `aimx send` refuses uid 0 — root cannot run it.

### Hook reads "Permission denied" on stdin

Symptom: hook logs show `exit_code != 0` and stderr tail like `cat: '/var/lib/aimx/inbox/...': Permission denied`.

Fix: the running subprocess is not the mailbox owner. Each mailbox directory is `<owner>:<owner> 0700`, so only the owner (and root) can read the piped email content. The daemon `setuid`s to `mailbox.owner_uid()` before `exec`, so a fresh hook should always run with the right uid; mismatches usually mean someone hand-edited `config.toml` and the on-disk perms drifted. Re-chown to match:

```bash
sudo chown -R <owner>:<owner> /var/lib/aimx/inbox/<mailbox> /var/lib/aimx/sent/<mailbox>
sudo chmod -R u+rwX,go-rwx /var/lib/aimx/inbox/<mailbox> /var/lib/aimx/sent/<mailbox>
```

### SIGHUP reload failed

Symptom: editing `config.toml` and `sudo systemctl reload aimx` reports success but the new hook never fires. journalctl shows a `config reloaded with error` warn line.

Fix: the new `config.toml` failed validation. Common culprits: a `stdin` line on a hook (the field was removed; `Config::load` now refuses any value with `hook '<name>' carries removed field 'stdin' — remove this line and restart aimx serve; the email is always piped to hooks`); legacy `template`, `params`, `run_as`, `origin`, or `dangerously_support_untrusted` fields on a hook (all rejected at config load with a pointer to `book/hooks.md`); duplicate hook name across mailboxes; `cmd[0]` not an absolute path; `fire_on_untrusted = true` on an `after_send` hook. Check the log:

```bash
journalctl -u aimx --since="5 minutes ago" | grep -i reload
```

Fix the offending field in `/etc/aimx/config.toml`, then `sudo systemctl reload aimx`.

### Hook's `cmd[0]` binary not found

Symptom: hook fires log `exit_code = -1` with `spawn-failed` kind.

Fix: the absolute path written into the hook's `cmd[0]` does not exist on the host. Run `which <agent>` as the mailbox owner to confirm the right path, then delete and re-create the hook with the corrected `cmd[0]`:

```bash
aimx hooks delete <name> --yes
aimx hooks create --mailbox <m> --event on_receive --cmd '["/correct/path/to/agent", "..."]' --name <name>
```

## Spam prevention

If outbound emails land in spam:

1. **Check all DNS records.** DKIM, SPF, and DMARC must all be set correctly. See [DNS configuration](setup.md#dns-configuration).
2. **Configure reverse DNS (PTR)** at your VPS provider's control panel so the PTR for your server IP points to your mail domain. This is the operator's responsibility and is out of scope for aimx, but is critical for deliverability with Gmail/Outlook.
3. **Gmail filter workaround.** In Gmail: Settings > Filters > Create filter for `*@agent.yourdomain.com` > Never send to Spam.
4. **Reply trick.** Reply to one email from the domain. Gmail learns it's not spam.

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
| **Other process on port 25** (Postfix, Exim, etc.) | Fails. Advises to stop the conflicting process |
| **Nothing on port 25** (fresh VPS) | Spawns temporary SMTP listener, then runs outbound + inbound EHLO checks |

If portcheck fails with EHLO probe after setup, the issue is likely in the `aimx serve` configuration rather than firewall/port access. Run `sudo systemctl status aimx` to check.

## Useful commands reference

| Command | Purpose |
|---------|---------|
| `sudo aimx portcheck` | Check port 25 connectivity (requires root) |
| `aimx doctor` | Show config, mailboxes, message counts, DNS record verification, and a pointer to `aimx logs` |
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
