# Hooks & Trust

Hooks trigger commands on specific email events. Two events are supported today:

- **`on_receive`** fires during inbound ingest, after the email is stored.
- **`after_send`** fires during outbound delivery, after the MX attempt resolves (success, failure, or deferred).

aimx supports two hook flavours:

1. **Template hooks (recommended).** Per-agent templates are registered by each user who runs `aimx agents setup <agent>` (no sudo); an agent-neutral `webhook` template ships pre-bundled. Agents then pick a template and fill declared parameters via MCP. Each template's `run_as` equals the registering user's Linux username, so hook children drop into that user's uid before exec.
2. **Raw-cmd hooks (power user).** The operator hand-writes a shell command in `config.toml` or via `sudo aimx hooks create --cmd`. Raw-cmd hooks carry an explicit `run_as` username (any existing Linux user, or the reserved `aimx-catchall` / `root`).

Combined with the mailbox-level `trust` policy, hooks gate shell-side automation on DKIM-verified inbound mail and on outbound delivery outcomes.

> For copy-paste agent-specific invocations (Claude Code, Codex CLI, OpenCode, Gemini CLI, Goose, OpenClaw, Hermes, Aider), see [Hook Recipes](hook-recipes.md).

## Template hooks (recommended)

Templates are the agent-native way to wire up hooks. They enable the "chat with your agent to add an automation" flow: the operator picks which templates are allowed on the box during `aimx setup`, then an AI agent talks to the `aimx mcp` server and creates hooks against those templates without ever touching a shell.

### Why templates?

- **No shell injection surface.** An agent that can create arbitrary `cmd` strings can escalate to local RCE. A template exposes only declared `{placeholder}` slots; parameter values substitute into argv entries but can never introduce new arguments or escape their slot (no shell is ever invoked).
- **Sandboxed by default.** Every template hook drops to the template's `run_as` user before exec (non-root, no login shell, no access to `/etc/aimx/dkim/`). On systemd hosts the sandbox adds `ProtectSystem=strict`, `PrivateDevices=yes`, `NoNewPrivileges=yes`, and a `MemoryMax=256M` cap via `systemd-run`.
- **Agent-friendly discovery.** MCP exposes `hook_list_templates` (filtered to the caller's visibility) so an agent can enumerate what it can wire up before asking.

### Install templates

Per-agent templates are registered on demand by the user who wants them:

```bash
aimx agents setup claude-code     # runs as the user, no sudo
```

This probes `$PATH` for the agent binary, submits `TEMPLATE-CREATE` over the UDS, and lands a template named `invoke-<agent>-<username>` (for example `invoke-claude-alice`). The daemon hot-swaps its in-memory config so the template is live immediately — no SIGHUP, no restart. See [Agent integration](agent-integration.md) for the full flow.

Only the agent-neutral `webhook` template ships pre-bundled. All other templates are user-registered.

Use `aimx hooks templates` to list the templates visible on your box. An operator can always drop a `[[hook_template]]` block into `config.toml` by hand for a hand-built template — the UDS verb only accepts templates whose `run_as` matches the caller's uid, but root's `config.toml` edit can install anything (for example `run_as = "aimx-catchall"` for a catchall-bound template, or `run_as = "root"` for a rare privileged handler).

### Create a template hook via MCP

An agent in an `aimx mcp` session discovers and creates hooks like this (see [MCP Server](mcp.md) for the full tool reference):

```json
{"name": "hook_list_templates", "arguments": {}}
```

```json
{"name": "hook_create", "arguments": {
  "mailbox": "accounts",
  "event": "on_receive",
  "template": "invoke-claude-alice",
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
  --template invoke-claude-alice \
  --param prompt="You are the accounts agent..."
```

The CLI sends the request over the same UDS verb MCP uses, so the hook ends up with `origin = "mcp"` (the tag marks the submission channel, not authorship). Use raw-cmd creation when you want `origin = "operator"`.

## Raw-cmd hooks (power user)

Raw-cmd hooks let the operator drop an argv array directly into `config.toml`. The argv is exec'd as the mailbox's `owner` inside the privilege-dropped sandbox — there is no shell wrapping. If you need shell expansion (env-var substitution, redirection), spell out `cmd = ["/bin/sh", "-c", "..."]` explicitly.

```toml
[[mailboxes.support.hooks]]
name = "support_notify"
event = "on_receive"
cmd = ["/bin/sh", "-c", 'echo "New email from $AIMX_FROM: $AIMX_SUBJECT" >> /tmp/email.log']
```

### Create a raw-cmd hook

`--cmd` takes the argv as a JSON array string. The first element must be an absolute path:

```bash
# on_receive with an explicit name (shell wrapper for env-var expansion)
sudo aimx hooks create \
  --mailbox support \
  --event on_receive \
  --cmd '["/bin/sh", "-c", "curl -fsS -X POST https://hooks.example.com/notify -d \"$AIMX_SUBJECT\""]' \
  --name support_notify

# after_send: log successful deliveries
sudo aimx hooks create \
  --mailbox alice \
  --event after_send \
  --cmd '["/bin/sh", "-c", "printf \"%s -> %s: %s\\n\" \"$AIMX_FROM\" \"$AIMX_TO\" \"$AIMX_SEND_STATUS\" >> /var/log/aimx-outbound.log"]'

# on_receive: direct argv exec, no shell required
sudo aimx hooks create \
  --mailbox catchall \
  --event on_receive \
  --cmd '["/usr/bin/logger", "-t", "aimx", "inbound mail"]' \
  --fire-on-untrusted
```

Raw-cmd hook creation requires `sudo` because it writes `/etc/aimx/config.toml` directly (bypassing the UDS) and then sends SIGHUP to `aimx serve` to hot-reload. If the daemon isn't running, the CLI writes the config and prints a restart hint.

### Hook properties

| Property | Type | Required | Description |
|----------|------|----------|-------------|
| `name` | string | no | Matches `^[a-zA-Z0-9_][a-zA-Z0-9_.-]{0,127}$`. When omitted, aimx derives a stable 12-char hex name from `sha256(event + joined_argv + fire_on_untrusted)`. Names must be globally unique across mailboxes, including derived ones. |
| `event` | string | yes | `"on_receive"` or `"after_send"`. |
| `type` | string | no | Trigger kind (default `"cmd"`). Only `cmd` is supported today. |
| `cmd` | array of strings | yes | Argv exec'd directly. Must be non-empty; `cmd[0]` must be an absolute path. No shell wrapping — wrap in `["/bin/sh", "-c", "..."]` explicitly when you need shell expansion. |
| `fire_on_untrusted` | bool | no | `on_receive` only: when `true`, fire even if `trusted != "true"`. Default `false`. |

Multiple hooks can be defined per mailbox; each is evaluated independently. Unknown fields on a hook table are rejected at config load.

## How hooks fire

When an email is ingested or sent:

1. The email is parsed/saved as a `.md` file (ingest) or the outbound MX result is known (send).
2. aimx walks the mailbox's `hooks` array, picking entries whose `event` matches.
3. For `on_receive`, the trust gate is applied: the hook fires iff `trusted == "true"` on the email, OR the hook sets `fire_on_untrusted = true`.
4. The argv `cmd` is exec'd directly via `spawn_sandboxed` — there is no shell interpretation. Spell out `cmd = ["/bin/sh", "-c", "..."]` explicitly when shell expansion is required.
5. On systemd the subprocess runs under `systemd-run` with `--uid=<owner>`, `ProtectSystem=strict`, `PrivateDevices=yes`, `NoNewPrivileges=yes`, `MemoryMax=256M`, and a 60s `RuntimeMaxSec`. On OpenRC the daemon `fork+exec`s with `setresgid/setresuid` to `<owner>` plus a manual timeout.
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

Always expand env vars inside double quotes (`"$AIMX_SUBJECT"`). Values from sender-controlled headers can contain `$()`, backticks, quotes, or newlines — when you wrap your hook in `["/bin/sh", "-c", "..."]`, these pass through as literal bytes.

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

## UDS authorization (`SO_PEERCRED`)

`/run/aimx/aimx.sock` is world-writable (`0666`). Every UDS request is authorized by reading the caller's uid via `SO_PEERCRED` and applying per-verb rules. Filesystem permissions are not the security boundary on the socket — the kernel-enforced peer uid is.

| Verb | Authorization |
|------|---------------|
| `SEND` | Caller uid must own the mailbox resolved from the `From:` local part, OR be root. |
| `MARK-READ` / `MARK-UNREAD` | Caller uid must own the target mailbox, OR be root. |
| `MAILBOX-CREATE` / `MAILBOX-DELETE` | Root only. |
| `HOOK-CREATE` | Caller uid must own the target mailbox (so alice cannot attach hooks to bob's mailbox). |
| `HOOK-DELETE` | Caller uid must own the target mailbox. Origin-protection rules (operator-origin hooks are CLI-only to remove) still apply. |
| `TEMPLATE-CREATE` | Caller's username must equal the submitted `run_as`. `root` and `aimx-catchall` templates can only be created by a root-executed `config.toml` edit; the UDS verb rejects those values regardless of caller. |
| `TEMPLATE-DELETE` | Caller can delete only templates whose `run_as` equals their username. |

Rejected requests return an `AIMX/1 ERR` response with `code = "EACCES"` and a human-readable reason. Caller uid 0 (root) bypasses all mailbox-ownership checks and is logged at info level so `aimx logs` shows the escalation.

### Reserved `run_as` values

Two usernames are reserved and accepted even on hosts where no matching Linux account exists:

| Reserved value | Purpose | Who can set it |
|----------------|---------|----------------|
| `aimx-catchall` | System user for catchall-mailbox hooks and templates. Created on demand by `aimx setup` when the operator configures a catchall mailbox. | Anyone editing `config.toml` as root; never accepted via UDS `TEMPLATE-CREATE`. |
| `root` | Unusual: hooks that need root privileges (rare). | Operator only, via hand-edit of `config.toml`; never accepted via UDS. |

All other `run_as` values must resolve via `getpwnam(3)` at `Config::load`. Unresolvable usernames are retained as orphan-flagged in the in-memory config (the daemon stays up) and soft-skipped at fire time; `aimx doctor` surfaces them.

## Trust gate (`on_receive` only)

> An `on_receive` hook fires iff `email.trusted == "true"` OR the hook sets `dangerously_support_untrusted = true`.

MCP-origin hooks cannot set `dangerously_support_untrusted` — if an agent could opt itself into firing on untrusted mail, the template sandbox is the only thing standing between a spoofed email and a subprocess running as the template's `run_as`. The field is only settable on operator-origin hooks in `config.toml`.

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

Prints a table of enabled templates (`NAME`, `DESCRIPTION`, `PARAMS`, `EVENTS`, `RUN_AS`). Empty output means no per-agent templates are registered for your user yet — run `aimx agents setup <agent>` (no sudo) for the agent you want.

### Delete a hook

```bash
aimx hooks delete support_notify         # interactive prompt
aimx hooks delete support_notify --yes   # scripted
```

`delete` accepts either an explicit name or a derived one. Operator-origin hooks require `sudo`; MCP-origin hooks can be deleted without privilege via the UDS.

### Prune orphaned templates and hooks

```bash
sudo aimx hooks prune --orphans          # print diff, ask to confirm
sudo aimx hooks prune --orphans --yes    # scripted
sudo aimx hooks prune --orphans --dry-run  # show what would be removed without writing
```

`hooks prune --orphans` removes every `HookTemplate` whose `run_as` user is gone, plus every `Hook` whose `run_as` user is gone or whose referenced template is gone. It is root-only, atomically rewrites `config.toml`, and prints a summary of what was removed (names plus the reason for each). To avoid cascading a half-broken config, it refuses to run if `aimx doctor` reports non-orphan issues — fix those first. Typical operator flow: delete a Linux user with `userdel alice`, run `aimx doctor` to confirm only orphan warnings remain, then `sudo aimx hooks prune --orphans` to clean up.

## Structured hook-fire logs

Every hook fire emits one `info`-level log line with a stable format:

```text
hook_name=<name> event=<on_receive|after_send> mailbox=<m> template=<name|-> run_as=<username|root|aimx-catchall> sandbox=<systemd-run|fallback> email_id=<id> exit_code=<n> duration_ms=<n> timed_out=<true|false> stderr_tail="..."
```

`template=-` indicates a raw-cmd hook; `run_as` reflects the configured user (resolved at fire time via `getpwnam`). When the user has been removed (`userdel alice`), the daemon soft-skips the hook with a WARN carrying `reason = "run_as_missing"` — see `aimx doctor` and `hooks prune --orphans`.

Operators can build `journalctl -u aimx | grep hook_name=<name>` workflows around it to trace every fire. `aimx doctor` parses these log lines to report 24h fire and failure counts per template.

## Examples

### Trigger Claude Code on verified mail (template hook)

```toml
# config.toml — alice's `aimx agents setup claude-code` writes this block over UDS
[[hook_template]]
name = "invoke-claude-alice"
description = "Pipe email into Claude Code with a prompt"
cmd = ["/home/alice/.local/bin/claude", "-p", "{prompt}"]
params = ["prompt"]
stdin = "email"
run_as = "alice"
timeout_secs = 60
allowed_events = ["on_receive", "after_send"]
```

Then an agent binds the template to a mailbox:

```bash
# Via MCP hook_create, or from the CLI (no sudo needed — the UDS verifies the caller owns the mailbox):
aimx hooks create \
  --mailbox schedule \
  --event on_receive \
  --template invoke-claude-alice \
  --param prompt="Handle this scheduling request."
```

### Notify via ntfy (raw-cmd, untrusted)

```toml
[[mailboxes.catchall.hooks]]
event = "on_receive"
cmd = ["/bin/sh", "-c", 'ntfy pub agent-mail "New email: $AIMX_SUBJECT from $AIMX_FROM"']
fire_on_untrusted = true
```

### After-send audit log (raw-cmd)

```toml
[[mailboxes.alice.hooks]]
name = "after_send_audit"
event = "after_send"
cmd = ["/bin/sh", "-c", 'echo "$AIMX_SEND_STATUS $AIMX_TO $AIMX_SUBJECT" >> /var/log/aimx/alice-sent.log']
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
