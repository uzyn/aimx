# aimx troubleshooting: error codes and recovery

## UDS send protocol error codes

When `aimx send` or `email_send` fails, the daemon returns an error code and
reason via the `AIMX/1` wire protocol. Error responses follow the format:

```
AIMX/1 ERR <CODE> <reason>
```

### Error codes

| Code       | Meaning | Common cause | Recovery |
|-----------|---------|--------------|----------|
| `MAILBOX` | From-mailbox not found | The `from_mailbox` does not exist in `config.toml` or is not owned by you | Provision a mailbox owned by your uid via `mailbox_create(name)` over MCP. Cross-uid creates remain operator-only — `sudo aimx mailboxes create <name> --owner <other>`. |
| `EACCES`  | Not authorized | Caller uid does not own the target mailbox | Confirm via `mailbox_list` that you own the mailbox you are targeting |
| `DOMAIN`  | Sender domain mismatch | The sender domain does not match the configured primary domain. **Through `email_send` over MCP this is unreachable** — the daemon derives the domain from `mailbox_list().address` automatically | If you saw this, you took the CLI path with a hand-typed `--from <addr>`. Switch to `email_send(from_mailbox: "<local-part>", ...)` |
| `SIGN`    | DKIM signing failed | DKIM private key missing or corrupted | Re-run `aimx setup` to regenerate keys |
| `DELIVERY`| Remote MX rejected mail | Recipient server refused the message (permanent) | Check the reason: invalid recipient, blocked sender, policy rejection |
| `TEMP`    | Temporary delivery failure | Recipient server unavailable or rate-limiting | Retry later. Transient network or server issue |
| `MALFORMED`| Request parsing failed | Malformed `AIMX/1 SEND` request frame | Internal error. Ensure `aimx send` version matches `aimx serve` |

### Exit codes for `aimx send`

| Code | Meaning |
|------|---------|
| `0`  | OK, message delivered |
| `1`  | Daemon returned ERR |
| `2`  | Socket missing, connect failure, or running as root |
| `3`  | Malformed response from daemon |

## Common misconfigurations

### "aimx daemon not running"

`aimx send` or `email_send` fails with "aimx daemon not running" when the
UDS socket at `/run/aimx/aimx.sock` is absent.

**Cause:** `aimx serve` is not running or was not started by systemd.

**Recovery:**
```bash
sudo systemctl status aimx
sudo systemctl start aimx
# or
sudo aimx serve
```

### "sender domain does not match aimx domain"

The `From:` address domain does not match the configured primary domain.

**Through `email_send` over MCP this error cannot occur** — the daemon
resolves the full From address from `mailbox_list().address` for the
mailbox you named. If you hit this, you took the CLI path with a
hand-typed `--from <addr>`.

**Recovery:** Switch to MCP. Call
`email_send(from_mailbox: "<local-part>", ...)` and let the daemon
construct the full address. If you genuinely need the domain string
for some other reason, call `mailbox_list()` and use the substring
after `@` in any `address` field — there is no other API for it.

### Mailbox not found

`email_send` or `email_reply` fails because the from-mailbox does not
exist or is not owned by you.

**Cause:** Attempting to send from a mailbox that was not created, or
one whose `owner` is a different Linux user.

**Recovery:** Provision the mailbox yourself over MCP — `mailbox_create`
synthesizes the owner from your uid via `SO_PEERCRED`, so the new
mailbox always lands owned by you with no operator intervention:

```
mailbox_create(name: "your-mailbox")
```

To create a mailbox owned by a **different** Linux user (e.g. a service
account), the operator must run the host CLI under sudo:

```
sudo aimx mailboxes create their-mailbox --owner their-username
```

Cross-uid creates remain operator-only by design — agents can only CRUD
mailboxes owned by the uid the MCP server runs under.

### DKIM signature failure

Outbound mail fails DKIM checks at the recipient. The `dkim` field on
inbound replies shows `"fail"`.

**Possible causes:**
- DKIM DNS record (`default._domainkey.domain.com`) is missing or incorrect.
- DKIM private key at `/etc/aimx/dkim/private.key` was replaced without
  updating the DNS TXT record.
- Message was modified in transit (rare with direct SMTP delivery).

**Recovery:**
1. Re-run `aimx setup`. It will display the DKIM DNS record.
2. Update the DNS TXT record to match the public key.
3. Wait for DNS propagation and test again.

### SPF failure on sent mail

Recipient's server rejects mail with SPF `fail`.

**Possible causes:**
- SPF DNS record does not include the server's IP address.
- Sending from an IP not covered by the SPF mechanism.
- IPv6 is enabled but `ip6:` mechanism is missing from SPF.

**Recovery:**
1. Check the SPF record: `dig TXT domain.com`
2. Ensure it includes `ip4:<server-ip>` (and `ip6:<server-ipv6>` if
   `enable_ipv6 = true` is set).
3. Re-run `aimx setup` to regenerate the correct DNS instructions.

### Email not appearing in inbox

Inbound mail is not showing up in the expected mailbox.

**Possible causes:**
- Mail was routed to `catchall` because the local part does not match a
  configured mailbox. The catchall is owned by `aimx-catchall` and not
  visible from your MCP session — ask the operator to inspect.
- Mail was rejected during SMTP session (check `journalctl -u aimx`).
- `aimx serve` is not running.

**Recovery:**
1. Verify the mailbox exists and is owned by you: `mailbox_list()`
2. Ask the operator to check the catchall and the daemon logs:
   `journalctl -u aimx -n 50`

### Permission denied reading emails

Cannot read `.md` files from the filesystem directly.

**Cause:** Mailbox directories are `0700 <owner>:<owner>`. Only the
mailbox owner (and root) can traverse into `/var/lib/aimx/inbox/<mailbox>/`
or `/var/lib/aimx/sent/<mailbox>/`.

**Recovery:** Confirm via `mailbox_list()` that the mailbox you are
trying to read is owned by your Linux uid. If you need a mailbox that
is currently owned by a different user, ask the operator to re-create
it with you as `--owner`, or run the MCP server as the owning user.

### Large attachment bundle

An email with attachments produces a directory instead of a flat `.md` file.

**Not an error.** aimx uses Zola-style bundles. When an email has one or
more attachments, a directory is created containing the `.md` file and
attachment files as siblings:

```
2026-04-15-153300-invoice-march/
├── 2026-04-15-153300-invoice-march.md
└── invoice.pdf
```

The `id` for this email is `2026-04-15-153300-invoice-march` (the directory
name), and `email_read` works with this ID normally.

## Hook-related errors

Full coverage lives in `references/hooks.md`. Quick entry points:

- **`hook_create` returned `not authorized`.** The mailbox is not
  owned by your Linux uid. Confirm via `mailbox_list()`; ask the
  operator to provision the mailbox with you as `--owner` if needed.
- **`hook_create` returned `Mailbox '…' does not exist`.** Mailbox
  CRUD is root-only on the host CLI. Ask the operator to run
  `sudo aimx mailboxes create <name> --owner <user>`.
- **`hook_create` returned `cmd[0] must be an absolute path`.** Use
  the full path to the binary; the daemon refuses bare command
  names.
- **`hook_create` returned `fire_on_untrusted is on_receive only`.**
  Drop the flag, or change the event to `on_receive`. There is no
  untrusted gate to bypass on outbound mail.
- **`hook_delete` returned `Hook '…' not found`.** Either the name
  is wrong, or the hook lives on a mailbox you do not own (the
  daemon collapses "exists but unauthorized" into not-found so
  foreign mailbox names do not leak). Re-run `hook_list()` to see
  your hooks.
- **My hook does not fire on inbound mail.** Check the target
  email's `trusted` field via `email_read`. By default `on_receive`
  hooks fire only on trusted mail; set `fire_on_untrusted = true`
  on the hook (or switch the mailbox to `trust = "verified"` with
  an allowlist) to widen the gate. See `references/hooks.md` for
  the full checklist.
