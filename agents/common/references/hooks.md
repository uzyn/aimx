# aimx hooks: creating and managing automations via MCP

Hooks are shell commands the aimx daemon fires on mail events:

- `on_receive`: an inbound email has been ingested. Fires only on
  **trusted** mail (`trusted == "true"` in frontmatter) unless the
  operator explicitly opted the hook out of the trust gate — and that
  opt-out is not settable via MCP.
- `after_send`: an outbound email has resolved (delivered, deferred,
  or failed). Fires on every send regardless of trust.

You do not ship arbitrary shell. You pick a **template** (either the
bundled `webhook`, or an `invoke-<agent>-<username>` template the
caller — you, effectively — registered via `aimx agents setup <agent>`)
and fill its declared parameters. The daemon substitutes your values
into the template's argv, drops privileges to the template's `run_as`
user, and spawns the child with a per-hook timeout. The local
operator is the only party who can install raw-cmd hooks (via
`sudo aimx hooks create --cmd "..."` on the host).

## The four MCP hook tools

### `hook_list_templates`

List every hook template visible to the caller's Linux user. A
template is visible when its `run_as` equals the caller's username, or
when `run_as` is a reserved sentinel (`aimx-catchall` or `root`). Call
this first, always, before `hook_create`. The list is empty until the
caller has run `aimx agents setup <agent>` (no sudo) to register their
`invoke-<agent>-<username>` template.

**Parameters:** none.

**Returns:** JSON array. Each entry:

```json
{
  "name": "invoke-claude-alice",
  "description": "Pipe the received/sent email into Claude Code with a custom prompt.",
  "params": ["prompt"],
  "allowed_events": ["on_receive", "after_send"]
}
```

The `params` array lists every parameter you must bind when calling
`hook_create`. The `allowed_events` array tells you which events this
template may be wired to — passing a disallowed event returns an error.

### `hook_create(mailbox, event, template, params, name?)`

Attach a template-bound hook to a mailbox.

**Parameters:**

| Name       | Type              | Required | Description |
|------------|-------------------|----------|-------------|
| `mailbox`  | string            | yes      | Mailbox name (e.g. `"alice"`) — must exist |
| `event`    | string            | yes      | `"on_receive"` or `"after_send"` |
| `template` | string            | yes      | Template name from `hook_list_templates` |
| `params`   | object (str→str)  | yes      | Key=value map matching the template's declared params exactly |
| `name`     | string (optional) | no       | Explicit hook name. When omitted, the daemon derives a 12-hex-char name from `(event, template, sorted params)` |

**Returns:** JSON with the effective name and the final substituted
argv so you can confirm the wiring in your reply to the user:

```json
{
  "effective_name": "mcp_test_hook",
  "substituted_argv": ["/home/alice/.local/bin/claude", "-p", "You are an assistant"]
}
```

**Errors:**

- `Mailbox '…' does not exist.`: create it first with `mailbox_create`.
- `Unknown template '…'.`: call `hook_list_templates` to see the list.
- `Template '…' does not permit event '…'.`: pick a different event
  or a different template.
- `missing-param: template '…' requires '…'.`: bind every declared
  param.
- `unknown-param: template '…' does not declare '…'.`: you sent a key
  not in the template's `params` list. Remove it.
- `param-invalid: parameter '…' contains an ASCII control character`
  or `contains a NUL byte` or `is N bytes (max 8192)`: sanitize your
  value.
- `name-conflict: hook name '…' already exists`: pick a different
  explicit name, or omit `name` to get a derived one.

### `hook_list(mailbox?)`

List hooks visible to MCP across all mailboxes (or a single one).

**Parameters:**

| Name      | Type              | Required | Description |
|-----------|-------------------|----------|-------------|
| `mailbox` | string (optional) | no       | Filter to one mailbox |

**Returns:** JSON array. MCP-origin rows (created by you or by a
CLI `--template` invocation) expose full details:

```json
{
  "name": "agent_hook",
  "mailbox": "alice",
  "event": "on_receive",
  "origin": "mcp",
  "template": "invoke-claude-alice",
  "params": {"prompt": "You are an assistant"}
}
```

Operator-origin rows (raw-cmd hooks the operator wrote on the host)
are **masked** — only these four fields survive:

```json
{
  "name": "daily-report",
  "mailbox": "alice",
  "event": "after_send",
  "origin": "operator"
}
```

This lets you see that a slot is taken without inspecting the operator's
automation. Do not try to work around the masking.

### `hook_delete(name)`

Delete a hook by effective name.

**Parameters:**

| Name   | Type   | Required | Description |
|--------|--------|----------|-------------|
| `name` | string | yes      | Effective name from `hook_list` |

**Returns:** confirmation string.

**Errors:**

- `ERR origin-protected: hook '…' was created by the operator —
  remove via \`sudo aimx hooks delete\` instead`: the target hook is
  operator-origin. MCP cannot delete it. Tell the user to run
  `sudo aimx hooks delete <name>` on the host.
- `hook '…' not found`: no hook with that name exists. Re-list to
  find the right name.
- `daemon not running`: the `aimx serve` process is down. Hooks
  cannot be deleted until it restarts.

Note: the `origin` tag is a *submission channel*, not an authorship
marker. Any hook that reached the daemon through the UDS (MCP
`hook_create`, or `aimx hooks create --template` on the host) carries
`origin = "mcp"` and is deletable via `hook_delete`. Operator-only
hooks are the ones the operator direct-wrote to `config.toml` via
`aimx hooks create --cmd "..."`.

## The template model, in detail

A template is one `[[hook_template]]` block the operator installed in
`/etc/aimx/config.toml` at setup time. It looks like:

```toml
[[hook_template]]
name = "invoke-claude-alice"
description = "Pipe the email into Claude Code with a custom prompt."
cmd = ["/home/alice/.local/bin/claude", "-p", "{prompt}"]
params = ["prompt"]
stdin = "email"
run_as = "alice"
timeout_secs = 60
allowed_events = ["on_receive", "after_send"]
```

Key facts for agents:

- `{placeholder}` tokens inside a `cmd` entry are the only places your
  `params` values land. They never split into new argv entries — if
  your value contains spaces, quotes, or shell metacharacters, they are
  passed verbatim into one argv slot.
- Built-in placeholders `{event}`, `{mailbox}`, `{message_id}`,
  `{from}`, `{subject}` are populated by the daemon at fire time. You
  do not bind them; you cannot supply them.
- `cmd[0]` (the binary path) is never substituted. An operator who
  trusts a template trusts the exact binary it will exec.
- `stdin = "email"` pipes the raw Markdown (frontmatter + body) of the
  email to the hook's child process on stdin. `"email_json"` wraps it
  in `{frontmatter, body}` JSON. `"none"` closes stdin.
- `run_as` is a Linux username. `aimx agents setup` sets it to the
  caller's username so each user's hooks drop into that user's uid.
  The reserved values are `aimx-catchall` (for catchall-mailbox
  templates) and `root` (rare, operator-only). The daemon enforces
  `hook.run_as == mailbox.owner OR hook.run_as == "root"` at every
  write (catchall allows `aimx-catchall`), so your agent reads exactly
  the files its matching mailbox owner can read.
- `timeout_secs` is a hard ceiling; SIGTERM at `timeout_secs`, SIGKILL
  at `+5s`.

## Example: "file + reply" hook on an accounts mailbox

A user says: *"When I get an email from my bank, file it and reply
with the current balance."*

1. `hook_list_templates()` → verify `invoke-claude-<username>` (or
   your agent's matching `invoke-<agent>-<username>` template) is
   visible. If not, tell the user to run
   `aimx agents setup <agent>` (no sudo) first.
2. `hook_create(mailbox: "accounts", event: "on_receive", template:
   "invoke-claude-alice", params: {prompt: "You are the accounts agent.
   Read the email on stdin. If it is from the bank, file it via
   email_mark_read and reply with email_reply including the current
   balance from the local ledger. Otherwise mark it read and do
   nothing."})`
3. Echo the `substituted_argv` in your reply to the user so they can
   see exactly what will run.
4. If the user later says *"undo that"*, call `hook_delete(name:
   <effective_name>)` with the name the daemon returned in step 2.

## Example: webhook on outbound mail

A user says: *"Ping https://ops.example.com/log every time I send an
email."*

1. `hook_list_templates()` → verify `webhook` is in the list.
2. `hook_create(mailbox: "agent", event: "after_send", template:
   "webhook", params: {url: "https://ops.example.com/log"})`
3. The template's `stdin = "email_json"` means the remote endpoint
   receives the full message as JSON. Confirm that matches what the
   user wants — if they need HTTP Basic auth or a custom header, the
   `webhook` template does not support it (v1); tell them to ask the
   operator to write a raw-cmd hook instead.

## Troubleshooting

### "My hook does not fire on inbound mail."

The `on_receive` trust gate is the most common cause. MCP-created
hooks **always** honor the gate — they fire iff the email's `trusted`
frontmatter is `"true"`. Check `email_read` output for the target
email:

- `trusted = "none"`: the mailbox's trust policy is `"none"` (the
  default). Ask the operator to set `trust = "verified"` and add a
  `trusted_senders` allowlist in `config.toml`.
- `trusted = "false"`: the sender did not match the allowlist, or
  DKIM did not pass. Hooks do not fire on this mail. Ask the user
  what they want (add sender to allowlist, or accept that this mail
  is untrusted).
- `trusted = "true"` but the hook still didn't fire: the daemon
  logs a structured line per firing. Ask the operator to check
  `aimx logs` or `journalctl -u aimx`.

### "hook_create returned `Unknown template`."

Call `hook_list_templates` again — the owning user may not have run
`aimx agents setup <agent>` yet, or your template name is misspelled
(remember: `invoke-<agent>-<username>`, not the bare `invoke-<agent>`).
The list is the authoritative source; never assume a template exists
from a past install.

### "hook_create returned `Mailbox '…' does not exist`."

Call `mailbox_list` to check spelling. `mailbox_create` first if you
are setting up a new identity.

### "hook_delete returned `origin-protected`."

The target hook was written to `config.toml` by the operator via
`aimx hooks create --cmd "..."`. Only the operator can remove it:
`sudo aimx hooks delete <name>` on the host. Tell the user exactly
that command.

### "I created a hook but `hook_list` does not show the full details."

Check the `origin` field. If it is `"operator"`, the hook was
hand-written; the details are deliberately masked. Your hooks (via
`hook_create`) always show full details because they carry
`origin = "mcp"`.

### "The operator disabled a template I was using."

Existing hooks bound to that template keep firing — the daemon
resolves the template name at fire time, and a disabled template that
still exists in `config.toml` remains callable. But new
`hook_create` calls referencing it will fail with `Unknown template`.
Ask the operator whether they want the template back, or whether the
existing hooks should be removed.

### "I can see an operator-origin hook in hook_list, but I'm told to stay away from it."

Correct. You can see operator-origin hooks so you do not create
duplicates or step on their names, but you cannot inspect, modify, or
delete them. Treat operator hooks as opaque infrastructure.

### "hook_create worked but the child process exits non-zero every time."

Inspect the operator's logs — the daemon captures up to 64 KiB of
stderr per hook fire and logs it at the end of the hook-fire
structured log line. Common causes: the template's `cmd[0]` binary is
not installed on this machine (template points at `/usr/local/bin/…`
and the binary lives at `/usr/bin/…`), or the user / recipe path in a
param value is wrong.

## What you must not do

- **Do not submit raw `cmd` values to `hook_create`.** The tool does
  not expose a `cmd` parameter. Templates are the only path.
- **Do not try to bypass masking on operator-origin hooks.** The
  daemon enforces it server-side.
- **Do not assume `hook_list_templates` is stable across installs.**
  Call it every time — the operator may add or remove templates.
- **Do not `hook_create` every time you reply to a user.** Hooks
  persist across restarts; create them once and reuse by name.
- **Do not set `dangerously_support_untrusted = true`.** It is not a
  valid MCP-side field. Only operators can set it, and only by hand-
  editing `config.toml`.
