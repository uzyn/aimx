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
| `MAILBOX` | From-mailbox not found | The `from_mailbox` does not exist in `config.toml` | Create the mailbox first with `mailbox_create` |
| `DOMAIN`  | Sender domain mismatch | The sender domain does not match the configured primary domain | Use the correct domain; check `/etc/aimx/config.toml` |
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

The `From:` address domain does not match the primary domain configured in
`/etc/aimx/config.toml`.

**Cause:** Sending from a domain aimx is not configured for.

**Recovery:** Verify the `domain` field in `config.toml`. aimx only allows
sending from the configured primary domain. Any local part is accepted, but
the domain must match exactly (case-insensitive).

### Mailbox not found

`email_send` or `email_reply` fails because the from-mailbox does not exist.

**Cause:** Attempting to send from a mailbox that was not created.

**Recovery:**
```
mailbox_create(name: "your-mailbox")
```

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
  configured mailbox.
- Mail was rejected during SMTP session (check `journalctl -u aimx`).
- `aimx serve` is not running.

**Recovery:**
1. Check `catchall` inbox: `email_list(mailbox: "catchall")`
2. Verify the mailbox exists: `mailbox_list()`
3. Check daemon logs: `journalctl -u aimx -n 50`

### Permission denied reading emails

Cannot read `.md` files from the filesystem directly.

**Cause:** The data directory is world-readable by default, but the process
may not have traversal permissions on parent directories.

**Recovery:** Ensure the user running the agent has read access to
`/var/lib/aimx/`. The data directory is `root:root 755`. Any local user
should have read access.

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

- **`hook_list_templates` returned `[]`.** The operator has not
  enabled any templates. Ask them to run `sudo aimx setup` on the
  host and tick the templates they want.
- **`hook_create` returned `Unknown template`.** Re-call
  `hook_list_templates`; the operator may have disabled it or your
  spelling is off.
- **`hook_create` returned `missing-param` / `unknown-param` /
  `param-invalid`.** The param map must match the template's `params`
  list exactly, and values cannot contain ASCII control chars or NUL
  bytes. Sanitize and retry.
- **`hook_delete` returned `origin-protected`.** The hook is operator-
  origin; MCP cannot delete it. Tell the user to run
  `sudo aimx hooks delete <name>` on the host.
- **My hook does not fire on inbound mail.** Check the target email's
  `trusted` field via `email_read`. MCP-origin hooks fire only on
  trusted mail. See `references/hooks.md` for the full trust checklist.
