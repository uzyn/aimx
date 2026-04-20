# Hooks & Trust

Hooks trigger shell commands on specific email events. Two events are supported today:

- **`on_receive`** fires during inbound ingest, after the email is stored.
- **`after_send`** fires during outbound delivery, after the MX attempt resolves (success, failure, or deferred).

Combined with the mailbox-level `trust` policy, hooks gate shell-side automation on DKIM-verified inbound mail and on outbound delivery outcomes.

> For copy-paste agent-specific invocations (Claude Code, Codex CLI, OpenCode, Gemini CLI, Goose, OpenClaw, Aider), see [Hook Recipes](hook-recipes.md).

## How hooks work

When an email is ingested or sent:

1. The email is parsed/saved as a `.md` file (ingest) or the outbound MX result is known (send).
2. aimx walks the mailbox's `hooks` array, picking entries whose `event` matches.
3. For `on_receive`, the trust gate is applied: the hook fires iff `trusted == "true"` on the email, OR the hook sets `dangerously_support_untrusted = true`.
4. The command executes synchronously under `sh -c`. The daemon awaits the subprocess for predictable timing; exit codes are discarded.
5. Hook failures are logged at `warn` but **never block delivery** or the send result.

Email is always stored (inbound) or attempted (outbound) regardless of whether hooks succeed or fire.

## Configuring hooks

Hooks are defined in `config.toml` under each mailbox. See [Configuration](configuration.md) for the full config reference.

```toml
[mailboxes.support]
address = "support@agent.yourdomain.com"

[[mailboxes.support.hooks]]
# name is optional. A stable 12-char hex id is derived from event+cmd if omitted.
name = "support_notify"
event = "on_receive"
cmd = 'echo "New email from $AIMX_FROM: $AIMX_SUBJECT" >> /tmp/email.log'
```

### Hook properties

| Property | Type | Required | Description |
|----------|------|----------|-------------|
| `name` | string | no | Matches `^[a-zA-Z0-9_][a-zA-Z0-9_.-]{0,127}$`. When omitted, aimx derives a stable 12-char hex name from `sha256(event + cmd + dangerously_support_untrusted)`. Names must be globally unique across mailboxes, including derived ones. |
| `event` | string | yes | `"on_receive"` or `"after_send"`. |
| `type` | string | no | Trigger kind (default `"cmd"`). Only `cmd` is supported today. |
| `cmd` | string | yes | Shell command to execute. |
| `dangerously_support_untrusted` | bool | no | `on_receive` only: when `true`, fire even if `trusted != "true"`. Default `false`. |

Multiple hooks can be defined per mailbox; each is evaluated independently. Unknown fields on a hook table (including stale filter fields from older drafts) are rejected at config load.

## Managing hooks via CLI

Use `aimx hooks` (alias: `aimx hook`) to manage hooks without hand-editing `config.toml`. All three sub-subcommands route through the daemon's UDS socket first so newly-created or -deleted hooks take effect on the very next event. **No restart required** while `aimx serve` is running. If the daemon is stopped, the CLI falls back to editing `config.toml` directly and prints a restart hint.

### List hooks

```bash
aimx hooks list                  # all mailboxes
aimx hooks list --mailbox support # single mailbox
```

Prints a table (`NAME`, `MAILBOX`, `EVENT`, `CMD`). The `CMD` column is truncated to 60 chars with a `…` suffix when longer. Anonymous hooks (those without an explicit `name =` entry) show up with their derived name, so `aimx hooks delete <name>` works uniformly.

### Create a hook

`create` is flag-based (not interactive). Pass `--name` to pick an explicit name; omit it to let aimx derive one. Either way, the final name is printed on success.

```bash
# on_receive with an explicit name
aimx hooks create \
  --mailbox support \
  --event on_receive \
  --cmd 'curl -fsS -X POST https://hooks.example.com/notify -d "$AIMX_SUBJECT"' \
  --name support_notify

# after_send: log successful deliveries
aimx hooks create \
  --mailbox alice \
  --event after_send \
  --cmd 'printf "%s -> %s: %s\n" "$AIMX_FROM" "$AIMX_TO" "$AIMX_SEND_STATUS" >> /var/log/aimx-outbound.log'

# on_receive: fire on untrusted mail too (verbose flag is intentional)
aimx hooks create \
  --mailbox catchall \
  --event on_receive \
  --cmd 'logger -t aimx "inbound from $AIMX_FROM"' \
  --dangerously-support-untrusted
```

`--dangerously-support-untrusted` is only valid on `--event on_receive`. Empty `--cmd` is rejected. `--name` (when given) must match the charset above.

### Delete a hook

```bash
aimx hooks delete support_notify         # interactive prompt
aimx hooks delete support_notify --yes   # scripted
```

`delete` accepts either an explicit name or a derived one. The prompt shows the hook's `name`, `mailbox`, `event`, and `cmd` (truncated to 60 chars) before asking `[y/N]`. There is no `update` verb: delete the old hook and create a new one if you want to tweak anything.

## Hook context: env vars and placeholders

User-controlled header fields are delivered to the hook shell as **env vars**; aimx-controlled fields are substituted into the command string as `{...}` placeholders.

### `on_receive` env vars

| Env var | Description | Example |
|---------|-------------|---------|
| `AIMX_HOOK_NAME` | The hook's effective name (explicit or derived) | `support_notify` |
| `AIMX_FILEPATH` | Full path to the saved `.md` file | `/var/lib/aimx/inbox/support/2025-01-15-103000-meeting.md` |
| `AIMX_FROM` | Sender email address (may include display name) | `Alice <alice@example.com>` |
| `AIMX_TO` | Recipient email address | `support@agent.yourdomain.com` |
| `AIMX_SUBJECT` | Email subject | `Meeting next Thursday` |
| `AIMX_MAILBOX` | Mailbox name | `support` |

### `after_send` env vars

| Env var | Description | Example |
|---------|-------------|---------|
| `AIMX_HOOK_NAME` | The hook's effective name (explicit or derived) | `after_send_audit` |
| `AIMX_FROM` | Sender address (your mailbox) | `alice@agent.yourdomain.com` |
| `AIMX_TO` | Recipient address | `bob@example.com` |
| `AIMX_SUBJECT` | Subject line | `Re: meeting` |
| `AIMX_MAILBOX` | Sending mailbox name | `alice` |
| `AIMX_FILEPATH` | Path to the sent-copy `.md` under `sent/<mailbox>/` (empty when the send wasn't persisted) | `/var/lib/aimx/sent/alice/2025-01-15-143022-re-meeting.md` |
| `AIMX_SEND_STATUS` | `"delivered"`, `"failed"`, or `"deferred"` | `delivered` |

Always expand env vars inside double quotes (`"$AIMX_SUBJECT"`) so whitespace and special characters pass through safely. Because these values come from sender-controlled headers, they can contain `$()`, backticks, quotes, or newlines. Env-var expansion under `sh -c` preserves them as literal bytes. The subprocess env is cleared before `PATH`, `HOME`, and the `AIMX_*` set are selectively re-added (defense in depth).

### Placeholders (aimx-controlled)

| Placeholder | Description | Example |
|-------------|-------------|---------|
| `{id}` | Email ID (filename stem) | `2025-01-15-103000-meeting` |
| `{date}` | Email date | `2025-01-15T10:30:00Z` |

These two fields are generated by aimx (ISO-8601 timestamps and slugs), safe to splice into the command string.

## Trust gate (`on_receive` only)

> An `on_receive` hook fires iff `email.trusted == "true"` OR the hook sets `dangerously_support_untrusted = true`.

`email.trusted` is computed from the mailbox's `trust` + `trusted_senders` policy and written to the email's frontmatter on ingest. The frontmatter value is always one of:

- `"none"`: effective `trust = "none"`. No evaluation performed. (Default.)
- `"true"`: effective `trust = "verified"`, sender matches `trusted_senders`, AND DKIM passed.
- `"false"`: effective `trust = "verified"`, but conditions were not met.

The recommended configuration is:

1. Set `trust = "verified"` + `trusted_senders = [...]` at the top level of `config.toml`.
2. Leave per-hook `dangerously_support_untrusted` off.

For hooks that should still fire on untrusted mail (e.g. a generic notifier that does not invoke an agent), set `dangerously_support_untrusted = true` on that hook explicitly. The flag name is deliberately verbose to make the security tradeoff visible in review. It is rejected at config load on any event other than `on_receive`.

### `trust` modes

| Mode | Effect on `trusted` frontmatter | Effect on hooks |
|------|---------------------------------|-----------------|
| `none` (default) | Always `"none"` | Default hooks do NOT fire |
| `verified` | `"true"` iff sender allowlisted AND DKIM pass; else `"false"` | Default hooks fire only when `trusted == "true"` |

```toml
# Global default. Applies to every mailbox unless overridden.
trust = "verified"
trusted_senders = ["*@yourcompany.com"]

[mailboxes.public]
address = "hello@agent.yourdomain.com"
# Per-mailbox override. This mailbox evaluates no trust, so hooks must opt in.
trust = "none"
```

Per-mailbox `trusted_senders` fully **replaces** the global list (no merging).

### How trust interacts with storage

**Email is always stored regardless of trust result.** Trust only gates hook execution. An email from an unverified sender is still saved as a `.md` file and visible via `email_list` / `email_read`.

### DKIM/SPF verification

During email ingest, aimx verifies DKIM, SPF, and DMARC. Results are stored in the email frontmatter (`dkim`, `spf`, `dmarc` as `"pass" | "fail" | "none"`, with SPF additionally allowing `"softfail"` / `"neutral"`). The `verified` trust mode requires a DKIM pass specifically, combined with an allowlist match on `trusted_senders`.

## Structured hook-fire logs

Every hook fire emits one `info`-level log line to the system logger (journald on systemd, syslog on OpenRC) with a stable format:

```text
hook_name=<name> event=<on_receive|after_send> mailbox=<m> (email_id=<id>|message_id=<id>) exit_code=<n> duration_ms=<n>
```

The format is stable. Operators can build `journalctl -u aimx | grep hook_name=<name>` workflows around it to trace every fire of a given hook. Non-zero exits and subprocess runtimes > 5 seconds are additionally logged at `warn` so slow or flaky hooks are visible in monitoring dashboards.

## Examples

### Notify via ntfy (anonymous hook)

```toml
[[mailboxes.catchall.hooks]]
# name is optional. A stable 12-char hex id is derived from event+cmd if omitted.
event = "on_receive"
cmd = 'ntfy pub agent-mail "New email: $AIMX_SUBJECT from $AIMX_FROM"'
dangerously_support_untrusted = true
```

### Trigger Claude Code on verified mail

```toml
[[mailboxes.schedule.hooks]]
name = "claude_schedule"
event = "on_receive"
cmd = 'claude -p "Handle this scheduling request: $(cat \"$AIMX_FILEPATH\")"'
```

### After-send audit log

```toml
[[mailboxes.alice.hooks]]
name = "after_send_audit"
event = "after_send"
cmd = 'echo "$AIMX_SEND_STATUS $AIMX_TO $AIMX_SUBJECT" >> /var/log/aimx/alice-sent.log'
```

### Verified-sender-gated automation

Set mailbox trust to `verified` plus an allowlist; a default hook will only fire on verified senders.

```toml
[mailboxes.accounting]
address = "accounting@agent.yourdomain.com"
trust = "verified"
trusted_senders = ["*@vendor.com"]

[[mailboxes.accounting.hooks]]
name = "invoice_parser"
event = "on_receive"
cmd = 'claude -p "Process this invoice: $(cat \"$AIMX_FILEPATH\")"'
```

---

Next: [Hook Recipes](hook-recipes.md) | [MCP Server](mcp.md) | [Configuration](configuration.md) | [Troubleshooting](troubleshooting.md)
