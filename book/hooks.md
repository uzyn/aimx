# Hooks & Trust

Hooks trigger commands on specific email events. Two events are supported today:

- **`on_receive`** fires during inbound ingest, after the email is stored.
- **`after_send`** fires during outbound delivery, after the MX attempt resolves (success, failure, or deferred).

aimx supports two hook flavours:

1. **Template hooks (recommended).** The operator installs a small set of pre-vetted command shapes once; agents then pick a template and fill declared parameters via MCP. Template hooks run sandboxed as the unprivileged `aimx-hook` user and can be created without a shell.
2. **Raw-cmd hooks (power user).** The operator hand-writes a shell command in `config.toml` or via `aimx hooks create --cmd`. Raw-cmd hooks also run sandboxed as `aimx-hook` by default.

Combined with the mailbox-level `trust` policy, hooks gate shell-side automation on DKIM-verified inbound mail and on outbound delivery outcomes.

> For copy-paste agent-specific invocations (Claude Code, Codex CLI, OpenCode, Gemini CLI, Goose, OpenClaw, Hermes, Aider), see [Hook Recipes](hook-recipes.md).

## Template hooks (recommended)

Templates are the agent-native way to wire up hooks. They enable the "chat with your agent to add an automation" flow: the operator picks which templates are allowed on the box during `aimx setup`, then an AI agent talks to the `aimx mcp` server and creates hooks against those templates without ever touching a shell.

### Why templates?

- **No shell injection surface.** An agent that can create arbitrary `cmd` strings can escalate to local RCE. A template exposes only declared `{placeholder}` slots; parameter values substitute into argv entries but can never introduce new arguments or escape their slot (no shell is ever invoked).
- **Sandboxed by default.** Every template hook runs as `aimx-hook` (non-root, no login shell, no access to `/etc/aimx/dkim/`). On systemd hosts the sandbox adds `ProtectSystem=strict`, `PrivateDevices=yes`, `NoNewPrivileges=yes`, and a `MemoryMax=256M` cap via `systemd-run`.
- **Agent-friendly discovery.** MCP exposes `hook_list_templates` so an agent can enumerate what it can wire up before asking.

### Install templates with `aimx setup`

During the interactive setup flow, aimx asks which templates to enable:

```text
[Hook Templates]
Hook templates let your agents create their own on_receive / after_send
automations via MCP, safely. Each template is a pre-vetted command shape;
agents can only fill declared parameters. Hook commands run as the
unprivileged 'aimx-hook' user, never as root.

Which templates should I enable? (space to toggle, enter to confirm)
 [ ] invoke-claude     Pipe email into Claude Code with a prompt
 [ ] invoke-codex      Pipe email into Codex CLI with a prompt
 [ ] invoke-opencode   Pipe email into OpenCode with a prompt
 [ ] invoke-gemini     Pipe email into Gemini CLI with a prompt
 [ ] invoke-goose      Pipe email into a Goose recipe
 [ ] invoke-openclaw   Pipe email into OpenClaw with a prompt
 [ ] invoke-hermes     Pipe email into Hermes with a prompt
 [x] webhook           POST the email as JSON to a URL
```

Re-run `aimx setup` any time to toggle which templates are enabled; your current selection is pre-ticked. Use `aimx hooks templates` to list enabled templates without opening `config.toml`.

See the [default template catalog](configuration.md#default-hook-templates) for the exact `cmd` shapes and parameter lists each template declares.

### Create a template hook via MCP

An agent in an `aimx mcp` session discovers and creates hooks like this (see [MCP Server](mcp.md) for the full tool reference):

```json
{"name": "hook_list_templates", "arguments": {}}
```

```json
{"name": "hook_create", "arguments": {
  "mailbox": "accounts",
  "event": "on_receive",
  "template": "invoke-claude",
  "params": {"prompt": "You are the accounts agent. File this email and draft a reply with the current balance."}
}}
```

Template hooks created via MCP are tagged `origin = "mcp"` in `config.toml`. An agent can later `hook_list` and `hook_delete` its own hooks, but it cannot delete an operator-authored hook (`origin = "operator"`). See the [origin model](#hook-origin-mcp-vs-operator) below.

### Create a template hook via CLI

Operators can create template hooks from the shell too:

```bash
sudo aimx hooks create \
  --mailbox accounts \
  --event on_receive \
  --template invoke-claude \
  --param prompt="You are the accounts agent..."
```

The CLI sends the request over the same UDS verb MCP uses, so the hook ends up with `origin = "mcp"` (the tag marks the submission channel, not authorship). Use raw-cmd creation when you want `origin = "operator"`.

## Raw-cmd hooks (power user)

Raw-cmd hooks let the operator drop an arbitrary shell string directly into `config.toml`. The command is wrapped in `/bin/sh -c` and executed inside the same `aimx-hook` sandbox as template hooks. The operator can opt a raw-cmd hook back to `run_as = "root"` by hand-editing `config.toml` — this is intentionally not reachable from the CLI or MCP.

```toml
[[mailboxes.support.hooks]]
name = "support_notify"
event = "on_receive"
cmd = 'echo "New email from $AIMX_FROM: $AIMX_SUBJECT" >> /tmp/email.log'
```

### Create a raw-cmd hook

```bash
# on_receive with an explicit name
sudo aimx hooks create \
  --mailbox support \
  --event on_receive \
  --cmd 'curl -fsS -X POST https://hooks.example.com/notify -d "$AIMX_SUBJECT"' \
  --name support_notify

# after_send: log successful deliveries
sudo aimx hooks create \
  --mailbox alice \
  --event after_send \
  --cmd 'printf "%s -> %s: %s\n" "$AIMX_FROM" "$AIMX_TO" "$AIMX_SEND_STATUS" >> /var/log/aimx-outbound.log'

# on_receive: fire on untrusted mail too (verbose flag is intentional)
sudo aimx hooks create \
  --mailbox catchall \
  --event on_receive \
  --cmd 'logger -t aimx "inbound from $AIMX_FROM"' \
  --dangerously-support-untrusted
```

Raw-cmd hook creation requires `sudo` because it writes `/etc/aimx/config.toml` directly (bypassing the UDS) and then sends SIGHUP to `aimx serve` to hot-reload. If the daemon isn't running, the CLI writes the config and prints a restart hint.

### Hook properties

| Property | Type | Required | Description |
|----------|------|----------|-------------|
| `name` | string | no | Matches `^[a-zA-Z0-9_][a-zA-Z0-9_.-]{0,127}$`. When omitted, aimx derives a stable 12-char hex name from `sha256(event + cmd + dangerously_support_untrusted)`. Names must be globally unique across mailboxes, including derived ones. |
| `event` | string | yes | `"on_receive"` or `"after_send"`. |
| `type` | string | no | Trigger kind (default `"cmd"`). Only `cmd` is supported today. |
| `cmd` | string | conditional | Shell command. Required for raw-cmd hooks, forbidden for template hooks. |
| `template` | string | conditional | Template name from `[[hook_template]]`. Forbidden for raw-cmd hooks. |
| `params` | table | no | Bound parameter values for template hooks. Keys must match the template's declared `params`. |
| `dangerously_support_untrusted` | bool | no | `on_receive` only: when `true`, fire even if `trusted != "true"`. Default `false`. Rejected on MCP-origin hooks. |
| `run_as` | string | no | `"aimx-hook"` (default) or `"root"`. Settable only via hand-edit of `config.toml` — not via CLI or MCP. |
| `origin` | string | no | `"operator"` (default) or `"mcp"`. Stamped automatically; lets `hook_delete` distinguish submission channels. |

Multiple hooks can be defined per mailbox; each is evaluated independently. Unknown fields on a hook table are rejected at config load.

## How hooks fire

When an email is ingested or sent:

1. The email is parsed/saved as a `.md` file (ingest) or the outbound MX result is known (send).
2. aimx walks the mailbox's `hooks` array, picking entries whose `event` matches.
3. For `on_receive`, the trust gate is applied: the hook fires iff `trusted == "true"` on the email, OR the hook sets `dangerously_support_untrusted = true`.
4. For template hooks, the daemon looks up the matching `[[hook_template]]`, substitutes declared `params` + built-in placeholders into the argv, and launches the command via `spawn_sandboxed`. For raw-cmd hooks, `/bin/sh -c <cmd>` is spawned the same way.
5. On systemd the subprocess runs under `systemd-run` with `--uid=aimx-hook`, `ProtectSystem=strict`, `PrivateDevices=yes`, `NoNewPrivileges=yes`, `MemoryMax=256M`, and `RuntimeMaxSec=<timeout_secs>`. On OpenRC the daemon `fork+exec`s with `setgid/setuid` to `aimx-hook` plus a manual timeout.
6. stdout and stderr are captured and truncated at 64 KiB each; the daemon awaits the subprocess for predictable timing.
7. Hook failures (non-zero exit, timeout) are logged at `warn` but **never block delivery**.

Email is always stored (inbound) or attempted (outbound) regardless of whether hooks succeed or fire.

## Hook context: env vars, stdin, and placeholders

### Env vars (raw-cmd and template hooks both receive these)

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

Always expand env vars inside double quotes (`"$AIMX_SUBJECT"`). Values from sender-controlled headers can contain `$()`, backticks, quotes, or newlines — under `sh -c` these pass through as literal bytes.

### Stdin (template hooks only)

Templates declare one of three `stdin` modes:

| Mode | Payload | Notes |
|------|---------|-------|
| `"email"` | The raw `.md` (frontmatter + body) | Default. Useful when piping into a CLI that takes stdin directly. |
| `"email_json"` | `{"raw": "<markdown>"}` | Wraps the `.md` in a JSON envelope. Used by the `webhook` template. |
| `"none"` | Closed immediately | For hooks that just need env vars. |

### Template placeholders

Template `cmd` entries may reference `{name}` placeholders. Two categories:

- **Declared params** — listed in the template's `params = [...]`. Agents fill these via `hook_create`.
- **Built-ins** — `{event}`, `{mailbox}`, `{message_id}`, `{from}`, `{subject}`. Auto-populated at fire time; missing values become empty strings.

Placeholders can only appear inside string argv entries (never as `cmd[0]` or standalone argv entries). Substituted values are always exactly one argv slot — no whitespace splitting, no shell interpretation. NUL, `\r`, and other control bytes in parameter values are rejected at fire time.

## Hook origin: MCP vs operator

Every hook carries an `origin` tag:

- **`origin = "operator"`** — written to `config.toml` via CLI `--cmd` or hand-edit. Invisible to MCP `hook_delete` (the daemon returns `ERR origin-protected`).
- **`origin = "mcp"`** — created over the UDS `HOOK-CREATE` verb. Visible to MCP tools; deletable via MCP `hook_delete` or `aimx hooks delete`.

The tag is stamped by the daemon based on submission channel, not by the author identity. An operator who runs `aimx hooks create --template ... --param ...` traverses the UDS verb and ends up with an `origin = "mcp"` hook. Use `--cmd` when you want an UDS-protected `origin = "operator"` hook.

MCP `hook_list` shows all hooks but **masks `cmd` / `params` on operator-origin entries** so agents can avoid duplicates without snooping on operator logic.

## Trust gate (`on_receive` only)

> An `on_receive` hook fires iff `email.trusted == "true"` OR the hook sets `dangerously_support_untrusted = true`.

MCP-origin hooks cannot set `dangerously_support_untrusted` — if an agent could opt itself into firing on untrusted mail, the template sandbox is the only thing standing between a spoofed email and an `aimx-hook`-level subprocess. The field is only settable on operator-origin hooks in `config.toml`.

`email.trusted` is computed from the mailbox's `trust` + `trusted_senders` policy and written to frontmatter at ingest:

- `"none"`: effective `trust = "none"`. No evaluation performed. (Default.)
- `"true"`: effective `trust = "verified"`, sender matches `trusted_senders`, AND DKIM passed.
- `"false"`: effective `trust = "verified"`, but conditions were not met.

Recommended configuration:

1. Set `trust = "verified"` + `trusted_senders = [...]` at the top level of `config.toml`.
2. Leave per-hook `dangerously_support_untrusted` off for anything that invokes an agent or writes to the filesystem.

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
trust = "none"
```

Per-mailbox `trusted_senders` fully **replaces** the global list (no merging).

### How trust interacts with storage

**Email is always stored regardless of trust result.** Trust only gates hook execution. An email from an unverified sender is still saved as a `.md` file and visible via `email_list` / `email_read`.

### DKIM/SPF verification

During email ingest, aimx verifies DKIM, SPF, and DMARC. Results are stored in the email frontmatter (`dkim`, `spf`, `dmarc` as `"pass" | "fail" | "none"`, with SPF additionally allowing `"softfail"` / `"neutral"`). The `verified` trust mode requires a DKIM pass specifically, combined with an allowlist match on `trusted_senders`.

## Managing hooks via CLI

All three `aimx hooks` subcommands (list/create/delete) route through the daemon's UDS socket first so newly-created hooks take effect on the very next event. **No restart required** while `aimx serve` is running. If the daemon is stopped, the CLI falls back to editing `config.toml` directly and prints a restart hint.

### List hooks

```bash
aimx hooks list                  # all mailboxes
aimx hooks list --mailbox support # single mailbox
```

Prints a table (`NAME`, `MAILBOX`, `EVENT`, `ORIGIN`, `CMD`). The `CMD` column is truncated to 60 chars with a `…` suffix when longer, and is blank for template hooks (see `aimx hooks templates` for their argv shape).

### List templates

```bash
aimx hooks templates
```

Prints a table of enabled templates (`NAME`, `DESCRIPTION`, `PARAMS`, `EVENTS`). Empty output means no templates are enabled — re-run `sudo aimx setup` and tick the ones you need.

### Delete a hook

```bash
aimx hooks delete support_notify         # interactive prompt
aimx hooks delete support_notify --yes   # scripted
```

`delete` accepts either an explicit name or a derived one. Operator-origin hooks require `sudo`; MCP-origin hooks can be deleted without privilege via the UDS.

## Structured hook-fire logs

Every hook fire emits one `info`-level log line with a stable format:

```text
hook_name=<name> event=<on_receive|after_send> mailbox=<m> template=<name|-> run_as=<aimx-hook|root> sandbox=<systemd-run|fallback> email_id=<id> exit_code=<n> duration_ms=<n> timed_out=<true|false> stderr_tail="..."
```

`template=-` indicates a raw-cmd hook; `run_as` reflects what the daemon *attempted* (on non-root dev boxes without the `aimx-hook` user, the log still reads `run_as=aimx-hook` while the subprocess actually runs as the current user and a WARN is logged separately).

Operators can build `journalctl -u aimx | grep hook_name=<name>` workflows around it to trace every fire. `aimx doctor` parses these log lines to report 24h fire and failure counts per template.

## Examples

### Trigger Claude Code on verified mail (template hook)

```toml
# config.toml — operator installs the template once via `aimx setup`
[[hook_template]]
name = "invoke-claude"
description = "Pipe email into Claude Code with a prompt"
cmd = ["/usr/local/bin/claude", "-p", "{prompt}"]
params = ["prompt"]
stdin = "email"
```

Then an agent binds the template to a mailbox:

```bash
# Via MCP hook_create, or from the CLI:
sudo aimx hooks create \
  --mailbox schedule \
  --event on_receive \
  --template invoke-claude \
  --param prompt="Handle this scheduling request."
```

### Notify via ntfy (raw-cmd, untrusted)

```toml
[[mailboxes.catchall.hooks]]
event = "on_receive"
cmd = 'ntfy pub agent-mail "New email: $AIMX_SUBJECT from $AIMX_FROM"'
dangerously_support_untrusted = true
```

### After-send audit log (raw-cmd)

```toml
[[mailboxes.alice.hooks]]
name = "after_send_audit"
event = "after_send"
cmd = 'echo "$AIMX_SEND_STATUS $AIMX_TO $AIMX_SUBJECT" >> /var/log/aimx/alice-sent.log'
```

### Webhook (template hook)

```bash
sudo aimx hooks create \
  --mailbox alerts \
  --event on_receive \
  --template webhook \
  --param url="https://hooks.example.com/aimx"
```

The `webhook` template POSTs `{"raw": "<markdown>"}` via `curl`.

---

Next: [Hook Recipes](hook-recipes.md) | [MCP Server](mcp.md) | [Configuration](configuration.md) | [Troubleshooting](troubleshooting.md)
