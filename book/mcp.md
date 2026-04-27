# MCP Server

> To install aimx into your agent, see [Agent Integration](agent-integration.md).

aimx includes a built-in MCP (Model Context Protocol) server that gives AI agents programmatic access to email. Agents can list, read, send, reply to, and manage email through standard MCP tool calls.

## Overview

- **Transport:** stdio (launched on-demand, no daemon)
- **Protocol:** Model Context Protocol (MCP)
- **Compatible clients:** any MCP-compatible client — see [Compatible agent frameworks](#compatible-agent-frameworks) below for the per-agent integration matrix.

## Running the MCP server

```bash
aimx mcp
```

The server runs in stdio mode. It reads from stdin and writes to stdout. It is launched on-demand by MCP clients, not run as a background service.

## Agent integration

See [Agent Integration](agent-integration.md) for one-line `aimx agents setup <agent>` installers and the manual MCP wiring pattern for clients not yet in the registry.

## Per-user authorization

The MCP server inherits the uid of the user that launched the client (stdio transport — there is no server process doing multi-user auth). At startup the server records its euid as the authorization principal; every tool call checks the same predicate the daemon enforces over the UDS:

> Caller is root, or caller's uid equals the target mailbox's `owner_uid`.

What that buys you per tool:

- `mailbox_list` returns only mailboxes whose `owner` resolves to the caller's uid (catchalls are filtered for non-root callers since the catchall owner is `aimx-catchall`).
- `email_list`, `email_read`, `email_mark_read`, `email_mark_unread`, `email_send`, `email_reply` all reject with `EACCES not authorized` when the target mailbox is owned by another user.
- `hook_create`, `hook_list`, `hook_delete` operate only on mailboxes the caller owns. `hook_delete` for a hook the caller does not own collapses to `Hook '<name>' not found` so foreign mailbox names do not leak.

Filesystem enforcement backs this up: every mailbox directory is `0700 <owner>:<owner>`, so even direct `.md` reads only succeed for the mailbox's owner. On a single-user box the rules are invisible (one user owns everything); on a multi-user box they give real isolation between alice and bob.

Root running the MCP server bypasses mailbox-ownership checks (and is logged at info level). Non-root callers see only their own world.

> **Removed in this release.** `mailbox_create`, `mailbox_delete`, and `hook_list_templates` are no longer MCP tools. Mailbox CRUD moves to the root-only host CLI (`sudo aimx mailboxes create | delete`); template hooks have been retired in favor of the unified plain-`cmd` model. Agents that previously called these tools will get a "tool not found" error and should call `hook_create` directly with the `cmd` argv from their bundled plugin recipe.

See [Hooks § UDS authorization (`SO_PEERCRED`)](hooks.md#uds-authorization-so_peercred) for the full per-verb authz table.

## MCP tools

aimx exposes 10 MCP tools organized into mailbox listing, email operations, and hook management.

### Mailbox tools

#### `mailbox_list`

List mailboxes you own.

**Parameters:** none

**Returns:** JSON array. One row per visible mailbox with these fields:

| Field         | Type   | Description                                                                  |
|---------------|--------|------------------------------------------------------------------------------|
| `name`        | string | Mailbox name (the local part).                                               |
| `inbox_path`  | string | Absolute path to the inbox directory (`/var/lib/aimx/inbox/<name>`).         |
| `sent_path`   | string | Absolute path to the sent directory (`/var/lib/aimx/sent/<name>`).           |
| `total`       | number | Total emails in the inbox.                                                   |
| `unread`      | number | Inbox emails with `read = false`.                                            |
| `sent_count`  | number | Total emails in the sent folder.                                             |
| `registered`  | bool   | `true` for mailboxes in `config.toml`; `false` for stray on-disk dirs only.  |

The empty case returns `[]`. Filtered to caller-owned mailboxes for non-root callers; root sees everything. The MCP process resolves the listing through the daemon over `/run/aimx/aimx.sock`, so it works without read access to root-owned `config.toml`.

---

### Email tools

#### `email_list`

List a page of email metadata in a mailbox, sorted descending by
filename (newest first). aimx never scans on the agent's behalf —
agents page through the listing and filter client-side.

| Parameter | Type   | Required | Description |
|-----------|--------|----------|-------------|
| `mailbox` | string | yes      | Mailbox name to list emails from. Must be owned by the caller. |
| `folder`  | string | no       | `"inbox"` (default) or `"sent"`. Picks which side of the mailbox to list |
| `limit`   | u32    | no       | Page size; default 50, hard-capped at 200. Values above 200 silently clamp |
| `offset`  | u32    | no       | Number of newest rows to skip; default 0 |

**Returns:** A JSON array of metadata rows. Inbox rows carry
`{ id, from, to, subject, date, read }`. Sent rows carry
`{ id, from, to, subject, date, delivery_status }` — the `read` field
is intentionally absent from sent rows, since agents do not mark sent
mail. An empty mailbox returns the literal `[]`. Returns
`EACCES not authorized` if the caller does not own the target mailbox.

---

#### `email_read`

Read the full content of an email.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `mailbox` | string | yes | Mailbox name |
| `id` | string | yes | Email ID, i.e. the filename stem (e.g. `2025-01-15-103000-meeting`) |
| `folder` | string | no | `"inbox"` (default) or `"sent"` |

**Returns:** Complete `.md` file content including frontmatter and body. Returns `EACCES not authorized` if the caller does not own the target mailbox.

---

#### `email_send`

Compose and send an email with DKIM signing.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `from_mailbox` | string | yes | Mailbox name to send from. Must be owned by the caller. |
| `to` | string | yes | Recipient email address |
| `subject` | string | yes | Email subject |
| `reply_to` | string | no | Message-ID of the email being replied to. Sets the `In-Reply-To` header and (when `references` is omitted) builds the `References` chain automatically. Required to enable threading. Without `reply_to`, any `references` value is silently ignored and no threading headers are emitted |
| `body` | string | yes | Email body text |
| `attachments` | array of strings | no | File paths to attach |
| `references` | string | no | Full `References` header chain (space-separated Message-IDs). **Only applied when `reply_to` is also set.** Supplied alone, it is silently ignored |

The MCP server composes the RFC 5322 message and submits it to `aimx serve` over the local `/run/aimx/aimx.sock` UDS. `aimx serve` parses `From:` from the body, validates that the caller's uid owns the resolved mailbox, DKIM-signs the message, and delivers it directly to the recipient's MX server via SMTP.

For replies to a single sender, prefer `email_reply`. It handles threading headers and the `Re:` subject prefix automatically. Use `email_send` with `reply_to` / `references` only when you need to override the recipient list (e.g. reply-all) or build a custom threading chain.

---

#### `email_reply`

Reply to an email with correct threading.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `mailbox` | string | yes | Mailbox name containing the email to reply to. Must be owned by the caller. |
| `id` | string | yes | Email ID to reply to (e.g. `2025-01-15-001`) |
| `body` | string | yes | Reply body text |

Automatically sets `In-Reply-To` and `References` headers from the original email for proper thread grouping in the recipient's mail client.

---

#### `email_mark_read`

Mark an inbox email as read. Sent-mail mark has no agent use case and is not supported.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `mailbox` | string | yes | Mailbox name. Must be owned by the caller. |
| `id` | string | yes | Email ID (filename stem, e.g. `2025-01-15-103000-meeting`) |

Updates `read = true` in the email's frontmatter. The MCP server is non-root so it routes the write through `aimx serve` over the local UDS (`/run/aimx/aimx.sock`) rather than touching the root-owned mailbox file directly. If `aimx serve` is not running the tool returns an error hint to start the daemon.

---

#### `email_mark_unread`

Mark an inbox email as unread. Sent-mail mark has no agent use case and is not supported.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `mailbox` | string | yes | Mailbox name. Must be owned by the caller. |
| `id` | string | yes | Email ID (filename stem, e.g. `2025-01-15-103000-meeting`) |

Updates `read = false` in the email's frontmatter. Same daemon-mediated write path as `email_mark_read`. Requires a running `aimx serve`.

---

### Hook tools

Three tools let agents self-configure hooks on mailboxes they own. See [Hooks & Trust](hooks.md) for the model and [Hook Recipes](hook-recipes.md) for verified per-agent `cmd` argv.

#### `hook_create`

Create a new hook on a mailbox you own. The daemon validates the caller's uid against the mailbox's `owner_uid` via `SO_PEERCRED` and rejects with `EACCES not authorized` if the predicate fails.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `mailbox` | string | yes | Target mailbox name. Must be owned by the caller. |
| `event` | string | yes | `"on_receive"` or `"after_send"` |
| `cmd` | array of strings | yes | Argv exec'd directly when the hook fires. `cmd[0]` must be an absolute path. |
| `name` | string | no | Explicit hook name. When omitted, a stable 12-hex-char name is derived from `sha256(event + joined_argv + fire_on_untrusted)`. |
| `timeout_secs` | int | no | Hard subprocess timeout in seconds. Default `60`, range `[1, 600]`. |
| `fire_on_untrusted` | bool | no | `on_receive` only: fire even when `trusted != "true"`. Default `false`. Rejected on `after_send`. |

The raw `.md` (frontmatter + body) is always piped to the hook's stdin and the same path is exposed as `$AIMX_FILEPATH`. If your hook only needs the subject or sender, read `$AIMX_SUBJECT` / `$AIMX_FROM` and ignore stdin — the daemon writes the full email but does not require the child to consume it.

**Returns:** `{effective_name}` — the hook name the daemon wrote.

Example (Claude Code self-wiring):

```json
{"name": "hook_create", "arguments": {
  "mailbox": "accounts",
  "event": "on_receive",
  "cmd": ["/usr/local/bin/claude", "-p", "Read the piped email and act on it via the aimx MCP server.", "--dangerously-skip-permissions"],
  "name": "accounts_claude"
}}
```

**Error examples:**

- `EACCES not authorized` — caller's uid does not own the target mailbox
- `mailbox-not-found: <name>` — mailbox does not exist
- `hook has non-absolute cmd[0]` — `cmd[0]` must be an absolute path
- `fire_on_untrusted is on_receive only` — flag set on an `after_send` hook
- `catchall does not support hooks` — target was a catchall mailbox

---

#### `hook_list`

List hooks on mailboxes you own.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `mailbox` | string | no | Filter to one mailbox (must be owned by the caller); omit to list every owned mailbox |

**Returns:** JSON array. Each entry has `name`, `mailbox`, `event`, `cmd`, `timeout_secs`, and `fire_on_untrusted`.

```json
[
  {"name": "accounts_claude", "mailbox": "accounts", "event": "on_receive", "cmd": ["/usr/local/bin/claude", "-p", "...", "--dangerously-skip-permissions"], "timeout_secs": 60, "fire_on_untrusted": false}
]
```

---

#### `hook_delete`

Delete a hook by name. Caller must own the hook's mailbox.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `name` | string | yes | Effective hook name (explicit or derived) |

Returns `Hook '<name>' not found` for hooks on mailboxes the caller does not own (the lookup is filtered before the existence check, so foreign mailbox names do not leak).

---

## Frontmatter reference

Every email stored by aimx carries a TOML frontmatter block between `+++` delimiters. Inbound emails include:

| Field | Always written | Description |
|-------|----------------|-------------|
| `id` | yes | Filename stem (e.g. `2025-04-15-143022-hello`) |
| `message_id` | yes | RFC 5322 `Message-ID` |
| `thread_id` | yes | 16-hex-char SHA-256 of the resolved thread root |
| `from` | yes | Sender address |
| `to` | yes | Recipient address |
| `delivered_to` | yes | Actual RCPT TO |
| `subject` | yes | Subject line |
| `date` | yes | Sender-claimed datetime (RFC 3339) |
| `received_at` | yes | Server receipt datetime (RFC 3339 UTC) |
| `dkim` | yes | `pass`, `fail`, or `none` |
| `spf` | yes | `pass`, `fail`, or `none` |
| `dmarc` | yes | `pass`, `fail`, or `none` |
| `trusted` | yes | `none`, `true`, or `false`. Per-mailbox trust evaluation result (see [Configuration](configuration.md)) |
| `mailbox` | yes | Target mailbox name |
| `read` | yes | Read/unread status |
| `outbound` | sent only | Always `true` on sent copies |
| `delivery_status` | sent only | `delivered`, `failed`, `deferred`, or `pending` |
| `bcc` | sent only (optional) | Array of BCC recipient addresses |
| `delivered_at` | sent only (optional) | RFC 3339 UTC timestamp of successful MX handoff |
| `delivery_details` | sent only (optional) | SMTP reason string on permanent failure |

See [Mailboxes: Outbound frontmatter](mailboxes.md#outbound-frontmatter) for the full outbound schema.

## Agent-facing documentation

Two reference documents help agents understand aimx:

- **`agents/common/aimx-primer.md`**: the canonical primer bundled into every agent plugin. Covers MCP tools, storage layout, frontmatter, trust model, common workflows, and a "Self-trigger as a mailbox hook" pointer to the agent's own recipe.
- **`/var/lib/aimx/README.md`**: the runtime datadir guide written by `aimx setup` and refreshed on `aimx serve` startup. Covers the on-disk layout, file naming, slug algorithm, bundle rules, and the UDS send protocol.

## Compatible agent frameworks

| Framework | Integration method |
|-----------|-------------------|
| Claude Code | MCP stdio mode via `~/.claude/settings.json` |
| OpenClaw | MCP stdio mode or [hooks](hooks.md) via shell |
| OpenCode | MCP stdio mode |
| Codex | [Hooks](hooks.md) via shell command |
| Any MCP client | Standard MCP stdio transport |

## Example workflow

A typical agent email workflow:

1. **Check for new mail.** Call `email_list` and filter the JSON output to rows where `read == false`
2. **Read an email.** Call `email_read` with the mailbox and email ID, or `Read` `<inbox_path>/<id>.md` directly
3. **Process the content.** Agent decides how to respond
4. **Reply.** Call `email_reply` with the response body
5. **Mark as read.** Call `email_mark_read`

For automated processing without MCP, use [hooks](hooks.md) to trigger commands on incoming email.

---

Next: [Hooks & Trust](hooks.md) | [Mailboxes & Email](mailboxes.md) | [Setup](setup.md)
