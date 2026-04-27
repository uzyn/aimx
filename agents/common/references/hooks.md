# aimx hooks: creating and managing automations via MCP

Hooks are commands the aimx daemon fires on mail events:

- `on_receive`: an inbound email has been ingested. Fires only on
  **trusted** mail (`trusted == "true"` in frontmatter) unless the
  hook itself sets `fire_on_untrusted = true`.
- `after_send`: an outbound email has resolved (delivered, deferred,
  or failed). Fires on every send regardless of trust.
  `fire_on_untrusted` is rejected on `after_send` hooks at config-load
  time — there is no untrusted gate to bypass on outbound.

Hooks belong to mailboxes you own. Use `hook_create` with the right
`cmd` for your runtime — see your agent's skill for the exact argv.
There is no template indirection, no per-hook `run_as` field, and no
shared "hook user" with read access to other mailboxes. The child
process always executes as the mailbox's owning Linux user, so a hook
on mailbox `alice` can do exactly what `alice` can do — read her own
inbox, run her own MCP server — and no more.

## The three MCP hook tools

### `hook_create(mailbox, event, cmd, name?, timeout_secs?, fire_on_untrusted?)`

Attach a hook to a mailbox you own.

**Parameters:**

| Name                | Type              | Required | Description |
|---------------------|-------------------|----------|-------------|
| `mailbox`           | string            | yes      | Mailbox name (e.g. `"alice"`) — must exist and be owned by you |
| `event`             | string            | yes      | `"on_receive"` or `"after_send"` |
| `cmd`               | string[]          | yes      | argv array. `cmd[0]` must be an absolute path the owning user can execute |
| `name`              | string            | no       | Explicit hook name. Omitted → daemon derives a 12-hex-char name from `(event, cmd, fire_on_untrusted)` |
| `timeout_secs`      | u32               | no       | Per-fire timeout in seconds. Default 60, max 600. SIGTERM at expiry, SIGKILL +5s |
| `fire_on_untrusted` | bool              | no       | Default `false`. Legal only on `on_receive`. When `true`, the hook fires on any inbound mail regardless of `trusted` |

The raw `.md` (frontmatter + body) is always piped on stdin and the
same path is also exposed as `$AIMX_FILEPATH`. If your hook only needs
the subject or sender, read `$AIMX_SUBJECT` / `$AIMX_FROM` and ignore
stdin — the daemon writes the full email but does not require the
child to consume it.

**Returns:** confirmation containing the effective name and the argv
that will run.

**Errors:**

- `not authorized`: the target mailbox is not owned by your uid.
  `mailbox_list()` shows what you can target.
- `Mailbox '…' does not exist.`: ask the operator to provision it via
  `sudo aimx mailboxes create <name> --owner <user>`.
- `name-conflict: hook name '…' already exists`: pick a different
  explicit name, or omit `name` to get a derived one.
- `daemon not running`: the `aimx serve` process is down. Hooks
  cannot be created until it restarts.
- `fire_on_untrusted is on_receive only`: drop the flag, or change
  the event to `on_receive`.
- `cmd[0] must be an absolute path`: the daemon refuses to spawn a
  bare command name; supply the full path.

### `hook_list(mailbox?)`

List hooks on mailboxes you own.

**Parameters:**

| Name      | Type              | Required | Description |
|-----------|-------------------|----------|-------------|
| `mailbox` | string (optional) | no       | Filter to one mailbox (must be owned by you) |

**Returns:** JSON array. Each row carries the full hook definition:

```json
{
  "name": "support-replier",
  "mailbox": "support",
  "event": "on_receive",
  "cmd": ["/usr/local/bin/claude", "-p", "You are the support agent...", "--dangerously-skip-permissions"],
  "timeout_secs": 60,
  "fire_on_untrusted": false
}
```

Hooks on mailboxes you do not own do not appear at all.

### `hook_delete(name)`

Delete a hook by effective name on a mailbox you own.

**Parameters:**

| Name   | Type   | Required | Description |
|--------|--------|----------|-------------|
| `name` | string | yes      | Effective name from `hook_list` |

**Returns:** confirmation string.

**Errors:**

- `Hook '…' not found`: no hook with that name exists in mailboxes
  you own. (The daemon collapses "exists but you don't own it" into
  the same not-found response so foreign mailbox names do not leak.)
- `daemon not running`: the `aimx serve` process is down. Hooks
  cannot be deleted until it restarts.

## The `cmd` argv shape

`cmd` is the literal argv passed to `posix_spawn`. There is no shell
wrapping, no string form, and no placeholder substitution. The only
runtime context the daemon adds is via environment variables (see
below).

- `cmd[0]` must be an absolute path. The daemon refuses bare command
  names so the binary you mean is the binary that runs.
- argv elements pass verbatim. Spaces, quotes, shell metacharacters,
  none of them are special — each array element becomes one `argv[N]`
  in the child.
- The child runs with `setuid(<mailbox owner uid>)` from root before
  `exec`. There is no `run_as` knob; ownership is the policy.
- The daemon clears the environment and restores only `PATH`, `HOME`,
  and the `AIMX_*` variables listed below.

### Environment variables set on every fire

- `AIMX_FILEPATH`: absolute path to the email's `.md` file (the
  bundle's main file, when attachments are present).
- `AIMX_MAILBOX`: mailbox name.
- `AIMX_EVENT`: `on_receive` or `after_send`.
- `AIMX_MESSAGE_ID`: RFC 5322 Message-ID without angle brackets.
- `AIMX_FROM`: sender address (header `From:`).
- `AIMX_SUBJECT`: subject line.

The raw `.md` (frontmatter + body) is always piped to the child's
stdin. The same path is exposed as `$AIMX_FILEPATH` so a hook that
only needs select fields (or that uses an agent which does not read
stdin in headless mode) can ignore stdin and act on env vars only.

### Per-agent recipes

The exact `cmd` argv to use when wiring up an agent as the hook
worker lives in the agent's skill bundle, not here, because the right
flags depend on the agent's own headless-run contract. Look for the
"Wiring yourself up as a mailbox hook" section in your agent's
`SKILL.md`. As of Apr 2026 the supported set is:

| Agent        | Notes |
|--------------|-------|
| Claude Code  | `claude -p <instruction> --dangerously-skip-permissions` (reads piped email on stdin) |
| Codex CLI    | `codex exec --skip-git-repo-check --full-auto -` (trailing `-` reads stdin as the prompt) |
| Gemini CLI   | `gemini -p <instruction> --yolo` (reads piped email on stdin) |
| Goose        | `goose run --recipe <path>` (preferred) or `goose run --instructions - --quiet` |
| OpenCode     | `opencode run --dangerously-skip-permissions <inline-prompt>` (reads `$AIMX_FILEPATH`; ignores stdin) |
| Hermes       | `hermes chat -q <inline-prompt> --yolo` (reads `$AIMX_FILEPATH`; ignores stdin) |
| OpenClaw     | No documented headless CLI as of Apr 2026 — see the OpenClaw skill |

## Example: "file + reply" hook on an accounts mailbox

A user (alice) says: *"When I get an email from my bank, file it and
reply with the current balance."*

```
hook_create(
  mailbox: "accounts",
  event: "on_receive",
  cmd: [
    "/usr/local/bin/claude", "-p",
    "You are the accounts agent. Read the email on stdin. If it is from the bank, mark it read via email_mark_read and reply via email_reply with the current balance from the local ledger. Otherwise mark it read and do nothing.",
    "--dangerously-skip-permissions"
  ]
)
```

If alice later says *"undo that"*, call `hook_delete(name:
<effective_name>)` with the name returned by `hook_create` (or look
it up via `hook_list(mailbox: "accounts")`).

## Example: webhook on outbound mail

A user says: *"Ping `https://ops.example.com/log` every time I send
an email from this mailbox."*

```
hook_create(
  mailbox: "agent",
  event: "after_send",
  cmd: [
    "/usr/bin/curl", "-sS", "-X", "POST",
    "-H", "Content-Type: text/markdown",
    "--data-binary", "@-",
    "https://ops.example.com/log"
  ]
)
```

The remote endpoint receives the raw `.md` (frontmatter + body) on
the request body. If the user needs JSON specifically, ask them to
pre-process the file from a small wrapper script — the daemon always
pipes the raw email to the hook's stdin and there is no JSON-mode
variant.

## Trust gate

`on_receive` hooks fire iff the email's `trusted` frontmatter is
`"true"`, OR the hook sets `fire_on_untrusted = true`. The flag is
the owner's choice — the relevant defense is mailbox isolation
(your hook runs as you, on your mailbox), not template gating.

If your hook does not fire on inbound mail, check `email_read`
output for the message:

- `trusted = "none"`: the mailbox's effective trust policy is
  `"none"` (no evaluation). Either switch the mailbox to
  `trust = "verified"` with a `trusted_senders` allowlist, or set
  `fire_on_untrusted = true` on the hook.
- `trusted = "false"`: the sender did not match the allowlist, or
  DKIM did not pass. Same options as above.
- `trusted = "true"` and the hook still didn't fire: ask the
  operator to check `aimx logs` or `journalctl -u aimx`. Each fire
  emits a structured line of the form
  `hook_name=<n> event=<e> mailbox=<m> owner=<u> exit_code=<n>
  duration_ms=<n> timed_out=<bool> stderr_tail=<…>`.

## Daemon-down behavior

When the `aimx serve` process is not running, MCP `hook_create` /
`hook_delete` return `daemon not running` immediately — there is no
fallback path that doesn't require config write, and you (as a
non-root user) cannot edit the root-owned `config.toml`. Tell the
user to bring the daemon back up (`sudo systemctl start aimx` on
systemd, `sudo rc-service aimx start` on OpenRC) and retry.

## Troubleshooting

### "My hook does not fire on inbound mail."

Most common cause: the trust gate. See the "Trust gate" section above
for the diagnosis flow.

### "hook_create returned `not authorized`."

Check `mailbox_list()`. The mailbox must be owned by your Linux uid.
Ask the operator to run
`sudo aimx mailboxes create <name> --owner <your-username>` (or
re-`--owner` an existing one) on the host.

### "hook_create returned `Mailbox '…' does not exist`."

Call `mailbox_list` to check spelling. Mailbox provisioning is
root-only on the host CLI; you cannot create mailboxes from MCP.

### "hook_delete returned `Hook '…' not found`."

Either the hook name is wrong, or the hook lives on a mailbox you do
not own (the daemon collapses "exists but unauthorized" into
not-found so foreign mailbox names don't leak). Re-run
`hook_list()` to see your hooks.

### "hook_create worked but the child process exits non-zero every time."

Inspect the operator's logs — the daemon captures up to 64 KiB of
stderr per hook fire and logs it at the end of the structured
hook-fire log line. Common causes: `cmd[0]` is not installed for
the owning user, the user is missing an agent skill (`aimx agents
setup <agent>` was not re-run as that user), or a path in a `cmd`
argument is wrong.

## What you must not do

- **Do not target mailboxes you do not own.** The daemon rejects
  every cross-owner write; do not waste tool calls.
- **Do not assume `cmd[0]` resolves via `$PATH`.** It must be an
  absolute path. The daemon refuses bare names.
- **Do not embed shell quoting in argv elements.** argv is not
  shell-expanded; each element is one `argv[N]`.
- **Do not set `fire_on_untrusted = true` on `after_send` hooks.**
  Config-load rejects it. There is no untrusted gate on outbound.
- **Do not `hook_create` every time you reply to a user.** Hooks
  persist across restarts; create once, reuse by name.
