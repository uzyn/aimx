# Hooks & Trust

Hooks trigger commands on specific email events. Two events are supported:

- **`on_receive`** fires during inbound ingest, after the email is stored.
- **`after_send`** fires during outbound delivery, after the MX attempt resolves (success, failure, or deferred).

Combined with the mailbox-level `trust` policy, hooks gate shell-side automation on DKIM-verified inbound mail and on outbound delivery outcomes.

> For copy-paste agent-specific invocations (Claude Code, Codex CLI, OpenCode, Gemini CLI, Goose, OpenClaw, Hermes, webhook), see [Hook Recipes](hook-recipes.md).

## Mailbox ownership = hook authorization

Every mailbox declares a single `owner` (a Linux user on the host). Ownership is the authorization predicate for everything that touches the mailbox:

- Mailbox storage (`/var/lib/aimx/inbox/<name>/` and `/var/lib/aimx/sent/<name>/`) is `<owner>:<owner>` mode `0700`. Only the owner — and root — can read or list its contents.
- Hooks on that mailbox can be created, listed, and deleted only by root or by the owner (via CLI as the owner's uid, or via the MCP `hook_create` / `hook_delete` tools running under the owner's uid).
- Hooks always execute as the mailbox's owner uid. The daemon spawns the subprocess and `setuid`s to `mailbox.owner_uid()` before `exec`. There is no per-hook `run_as` override.

What this buys you: a hook on `alice`'s mailbox can do anything `alice` could already do (cron, `~/.bashrc`, systemd `--user`, etc.). It cannot escalate privilege, and it cannot read `bob`'s mail — `/var/lib/aimx/inbox/bob/` is `bob:bob 0700`, which `alice` cannot even traverse.

To run a hook as root, set `mailbox.owner = "root"` in `/etc/aimx/config.toml`. That requires editing the root-owned file directly. Catchall mailboxes are owned by the reserved `aimx-catchall` system user (created on demand by `aimx setup`); **hooks on the catchall mailbox are forbidden** at config-load time because the catchall user has no shell and no resolvable login uid that `setuid` can drop into.

## Hook schema

Hooks live as `[[mailboxes.<name>.hooks]]` arrays-of-tables in `/etc/aimx/config.toml`:

```toml
[[mailboxes.support.hooks]]
name = "support_notify"
event = "on_receive"
cmd = ["/bin/sh", "-c", 'echo "New mail from $AIMX_FROM: $AIMX_SUBJECT" >> /tmp/email.log']
```

`cmd` is exec'd directly as the mailbox owner — there is no shell wrapping. If you need shell expansion (env-var substitution, redirection, pipes), spell out `cmd = ["/bin/sh", "-c", "..."]` explicitly so it's visible at the call site.

### Hook properties

| Property | Type | Required | Description |
|----------|------|----------|-------------|
| `name` | string | no | Matches `^[a-zA-Z0-9_][a-zA-Z0-9_.-]{0,127}$`. When omitted, aimx derives a stable 12-char hex name from `sha256(event + joined_argv + fire_on_untrusted)`. Names must be globally unique across mailboxes — including derived ones. |
| `event` | string | yes | `"on_receive"` or `"after_send"`. |
| `type` | string | no | Trigger kind (default `"cmd"`). Only `cmd` is supported today. |
| `cmd` | array of strings | yes | Argv exec'd directly. Must be non-empty; `cmd[0]` must be an absolute path. No shell wrapping — wrap in `["/bin/sh", "-c", "..."]` explicitly when you need shell expansion. |
| `timeout_secs` | int | no | Hard subprocess timeout in seconds. Default `60`, range `[1, 600]`. SIGTERM at the limit, SIGKILL 5s later. |
| `fire_on_untrusted` | bool | no | `on_receive` only: when `true`, fire even if `trusted != "true"`. Default `false`. Rejected on `after_send` hooks at config load. |

Multiple hooks can be defined per mailbox; each is evaluated independently. Unknown fields on a hook table are rejected at config load.

## Creating hooks

### Via CLI (owner or root)

```bash
# As the mailbox owner — uses the daemon's UDS socket; takes effect on the next event
aimx hooks create \
  --mailbox support \
  --event on_receive \
  --cmd '["/bin/sh", "-c", "curl -fsS -X POST https://hooks.example.com/notify -d \"$AIMX_SUBJECT\""]' \
  --name support_notify

# As root — same UDS path; root passes every authorization check
sudo aimx hooks create \
  --mailbox catchall \
  --event on_receive \
  --cmd '["/usr/bin/logger", "-t", "aimx", "inbound mail"]' \
  --fire-on-untrusted
```

`--cmd` takes the argv as a JSON array string. `cmd[0]` must be an absolute path.

When the daemon is running, the CLI submits over `/run/aimx/aimx.sock`; the daemon hot-swaps its in-memory `Config` so the new hook is live on the very next event — **no restart required**. When the daemon is stopped, the CLI falls back to editing `config.toml` directly (root only) and prints a restart hint; non-root callers hard-error since they cannot write the root-owned config.

### Via MCP (owner's agent)

When `aimx mcp` runs under the mailbox owner's uid, the agent can create hooks programmatically:

```json
{"name": "hook_create", "arguments": {
  "mailbox": "accounts",
  "event": "on_receive",
  "cmd": ["/usr/local/bin/claude", "-p", "Read this email and act on it.", "--dangerously-skip-permissions"],
  "fire_on_untrusted": false
}}
```

See [MCP Server: hook_create](mcp.md#hook_create) for the full tool reference. Each agent's bundled skill ships a copy-paste `cmd` recipe — see [Hook Recipes](hook-recipes.md).

## How hooks fire

When an email is ingested or sent:

1. The email is parsed/saved as a `.md` file (ingest) or the outbound MX result is known (send).
2. aimx walks the mailbox's `hooks` array, picking entries whose `event` matches.
3. For `on_receive`, the trust gate is applied: the hook fires iff `trusted == "true"` on the email, OR the hook sets `fire_on_untrusted = true`.
4. The argv `cmd` is exec'd directly via `spawn_sandboxed`. The daemon `setuid`s to `mailbox.owner_uid()` before `exec`. `cmd` is not interpreted by a shell — wrap in `["/bin/sh", "-c", "..."]` explicitly when shell expansion is required.
5. On systemd the subprocess runs under `systemd-run` with `--uid=<owner>`, `ProtectSystem=strict`, `PrivateDevices=yes`, `NoNewPrivileges=yes`, `MemoryMax=256M`, and `RuntimeMaxSec=<timeout_secs>`. On OpenRC the daemon `fork+exec`s with `setresgid/setresuid` to `<owner>` plus a manual timeout.
6. stdout and stderr are captured and truncated at 64 KiB each; the daemon awaits the subprocess for predictable timing.
7. Hook failures (non-zero exit, timeout) are logged at `warn` but **never block delivery**.

Email is always stored (inbound) or attempted (outbound) regardless of whether hooks succeed or fire.

## Hook context: env vars and stdin

### Env vars

| Env var | Direction | Description |
|---------|-----------|-------------|
| `AIMX_HOOK_NAME` | both | Effective hook name (explicit or derived) |
| `AIMX_EVENT` | both | `on_receive` or `after_send` |
| `AIMX_MAILBOX` | both | Mailbox name |
| `AIMX_FROM` | both | Sender address (may include display name on inbound) |
| `AIMX_TO` | both | Recipient address |
| `AIMX_SUBJECT` | both | Subject line |
| `AIMX_FILEPATH` | both | Path to the `.md` file (inbox on `on_receive`, sent copy on `after_send`) |
| `AIMX_MESSAGE_ID` | both | RFC Message-ID |
| `AIMX_ID` | both | Filename stem |
| `AIMX_DATE` | both | Email date (inbound) or send timestamp (outbound) |
| `AIMX_SEND_STATUS` | after_send | `"delivered"`, `"failed"`, or `"deferred"` |

Always expand env vars inside double quotes (`"$AIMX_SUBJECT"`). Values from sender-controlled headers can contain `$()`, backticks, quotes, or newlines — when you wrap your hook in `["/bin/sh", "-c", "..."]`, these pass through as literal bytes inside the quoted expansion.

### Stdin

The raw `.md` (frontmatter + body) is always piped to the hook's stdin. The same path is also exposed as `$AIMX_FILEPATH`.

If your hook only needs the subject or sender, read `$AIMX_SUBJECT` / `$AIMX_FROM` and ignore stdin — the daemon writes the full email but does not require the child to consume it. Agent CLIs that don't read stdin in headless mode (OpenCode, Hermes) use `$AIMX_FILEPATH` to open the file directly.

The earlier per-hook `stdin = "email" | "none"` knob has been removed; `Config::load` rejects any `stdin` line in `[[mailboxes.<name>.hooks]]` with an error that names the offending hook.

## UDS authorization (`SO_PEERCRED`)

`/run/aimx/aimx.sock` is world-writable (`0666`). Every UDS request is authorized by reading the caller's uid via `SO_PEERCRED` and applying per-verb rules. Filesystem permissions are not the security boundary on the socket — the kernel-enforced peer uid is.

| Verb | Authorization |
|------|---------------|
| `SEND` | Caller uid must own the mailbox resolved from the `From:` local part, OR be root. |
| `MARK-READ` / `MARK-UNREAD` | Caller uid must own the target mailbox, OR be root. |
| `MAILBOX-CREATE` / `MAILBOX-DELETE` | Root only. |
| `HOOK-CREATE` / `HOOK-DELETE` | Caller uid must own the target mailbox, OR be root. |

Rejected requests return an `AIMX/1 ERR` response with `code = "EACCES"` and the canonical reason `not authorized` (no information leakage about whether the mailbox exists). Caller uid 0 (root) bypasses all mailbox-ownership checks and is logged at info level so `aimx logs` shows the escalation.

## Trust gate (`on_receive` only)

> An `on_receive` hook fires iff `email.trusted == "true"` OR the hook sets `fire_on_untrusted = true`.

`fire_on_untrusted` is the per-hook escape hatch. Setting it via MCP is the owner's choice — mailbox isolation (uid-scoped storage + uid-scoped exec) is the relevant defense, so an agent opting into untrusted mail on its own mailbox cannot escalate beyond what the owner already has. The flag is illegal on `after_send` hooks; `Config::load` rejects it with `ERR fire_on_untrusted is on_receive only`.

`email.trusted` is computed from the mailbox's `trust` + `trusted_senders` policy and written to frontmatter at ingest:

- `"none"`: effective `trust = "none"`. No evaluation performed. (Default.)
- `"true"`: effective `trust = "verified"`, sender matches `trusted_senders`, AND DKIM passed.
- `"false"`: effective `trust = "verified"`, but conditions were not met.

Recommended configuration:

1. Set `trust = "verified"` + `trusted_senders = [...]` at the top level of `config.toml`.
2. Leave per-hook `fire_on_untrusted` off for anything that invokes an agent or writes to the filesystem in an irreversible way.

### `trust` modes

| Mode | Effect on `trusted` frontmatter | Effect on hooks |
|------|---------------------------------|-----------------|
| `none` (default) | Always `"none"` | Default hooks do NOT fire |
| `verified` | `"true"` iff sender allowlisted AND DKIM pass; else `"false"` | Default hooks fire only when `trusted == "true"` |

```toml
trust = "verified"
trusted_senders = ["*@yourcompany.com"]

[mailboxes.public]
address = "hello@agent.yourdomain.com"
owner = "ubuntu"
trust = "none"
```

Per-mailbox `trusted_senders` fully **replaces** the global list (no merging).

### How trust interacts with storage

**Email is always stored regardless of trust result.** Trust only gates hook execution. An email from an unverified sender is still saved as a `.md` file and visible to the mailbox owner via `email_list` / `email_read`.

### DKIM/SPF verification

During email ingest, aimx verifies DKIM, SPF, and DMARC. Results are stored in the email frontmatter (`dkim`, `spf`, `dmarc` as `"pass" | "fail" | "none"`, with SPF additionally allowing `"softfail"` / `"neutral"`). The `verified` trust mode requires a DKIM pass specifically, combined with an allowlist match on `trusted_senders`.

## Managing hooks via CLI

All three `aimx hooks` subcommands (list/create/delete) route through the daemon's UDS socket so newly-created hooks take effect on the very next event. **No restart required** while `aimx serve` is running. If the daemon is stopped, root falls back to editing `config.toml` directly and prints a restart hint; non-root callers error out (they cannot edit the root-owned config).

### List hooks

```bash
aimx hooks list                  # mailboxes you own
aimx hooks list --mailbox support # single mailbox you own
sudo aimx hooks list --all       # every mailbox, root only
```

For non-root callers, output is filtered to mailboxes whose owner matches your euid. `--all` is rejected for non-root with `--all requires root`.

Prints a table (`NAME`, `MAILBOX`, `EVENT`, `CMD`). The `CMD` column is truncated to 60 chars with a `…` suffix when longer.

### Delete a hook

```bash
aimx hooks delete support_notify         # interactive prompt
aimx hooks delete support_notify --yes   # scripted
```

`delete` accepts either an explicit name or a derived one. Authorization is the same as `create`: caller must own the target mailbox, or be root.

## Structured hook-fire logs

Every hook fire emits one `info`-level log line with a stable shape:

```text
hook_name=<name> event=<on_receive|after_send> mailbox=<m> owner=<owner> sandbox=<systemd-run|fallback> email_id=<id> exit_code=<n> duration_ms=<n> timed_out=<true|false> stderr_tail="..."
```

`owner` is the resolved uid the subprocess ran as (matching `mailbox.owner` at fire time). When the configured owner has been removed (`userdel alice`), the daemon soft-skips the hook with a WARN carrying `reason = "owner-not-found"` — see `aimx doctor`.

Operators can build `journalctl -u aimx | grep hook_name=<name>` workflows around it to trace every fire.

## Examples

### Trigger Claude Code on verified mail

```toml
[mailboxes.schedule]
address = "schedule@agent.yourdomain.com"
owner = "alice"
trust = "verified"
trusted_senders = ["*@yourcompany.com"]

[[mailboxes.schedule.hooks]]
name = "schedule_claude"
event = "on_receive"
cmd = ["/usr/local/bin/claude", "-p", "Handle this scheduling request.", "--dangerously-skip-permissions"]
```

The hook fires only when the email's `trusted == "true"`. Claude runs as `alice` (matching the mailbox owner), reads the piped `.md` from stdin, and uses its own MCP tooling to reply.

### Notify via ntfy on every inbound (untrusted)

```toml
[mailboxes.catchall]
address = "*@agent.yourdomain.com"
owner = "aimx-catchall"
# Note: hooks on the catchall are forbidden at config load.
# Wire notifications via a non-catchall mailbox instead.

[mailboxes.notify]
address = "notify@agent.yourdomain.com"
owner = "ubuntu"

[[mailboxes.notify.hooks]]
event = "on_receive"
cmd = ["/bin/sh", "-c", 'ntfy pub agent-mail "New email: $AIMX_SUBJECT from $AIMX_FROM"']
fire_on_untrusted = true
```

### After-send audit log

```toml
[[mailboxes.alice.hooks]]
name = "after_send_audit"
event = "after_send"
cmd = ["/bin/sh", "-c", 'echo "$AIMX_SEND_STATUS $AIMX_TO $AIMX_SUBJECT" >> /var/log/aimx/alice-sent.log']
```

### Webhook (POST the email body to a URL)

```toml
[[mailboxes.alerts.hooks]]
name = "alerts_webhook"
event = "on_receive"
cmd = ["/usr/bin/curl", "-sS", "-X", "POST", "-H", "Content-Type: application/json", "--data-binary", "@-", "https://hooks.example.com/aimx"]
```

`--data-binary @-` posts whatever lands on curl's stdin verbatim, which is the raw `.md` (frontmatter + body) — the daemon always pipes the email to a hook's stdin.

---

Next: [Hook Recipes](hook-recipes.md) | [MCP Server](mcp.md) | [Configuration](configuration.md) | [Troubleshooting](troubleshooting.md)
