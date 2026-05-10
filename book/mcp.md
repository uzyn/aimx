# MCP Server

AIMX includes a Model Context Protocol server that gives AI agents programmatic access to email. Agents can list, read, send, reply to, and manage email through standard MCP tool calls.

## Overview

- **Transport:** stdio (launched on-demand by the MCP client; no background daemon).
- **Protocol:** Model Context Protocol.
- **Compatible clients:** any MCP-compatible client. See [Agent Integration § Supported agents](agent-integration.md#supported-agents) for the per-agent matrix.

## Running the MCP server

```bash
aimx mcp
```

The server reads from stdin and writes to stdout. To install it into a supported agent, see [Agent Integration](agent-integration.md).

## Per-user authorization

The MCP server inherits the uid of the user that launched the client. Every tool call routes through the daemon UDS, which authorizes against the caller's uid via `SO_PEERCRED`: caller is root, or caller's uid equals the target mailbox's `owner_uid`. Tools acting on mailboxes the caller does not own return `EACCES not authorized`; `hook_delete` for an unowned hook collapses to `Hook '<name>' not found` so foreign mailbox names do not leak.

Root running the MCP server bypasses mailbox-ownership checks (and is logged at info level). See [Security: Per-action authorization](security.md#per-action-authorization).

## MCP tools

AIMX exposes 12 MCP tools organized into mailbox lifecycle, email operations, and hook management.

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

#### `mailbox_create`

Provision a new mailbox owned by the calling agent's uid.

| Parameter | Type   | Required | Description |
|-----------|--------|----------|-------------|
| `name`    | string | yes      | Mailbox name (the local part of the resulting address). Must match `[a-z0-9._-]+`, must not be the reserved literals `catchall` / `aimx-catchall`. |

There is no `owner` parameter, by construction. The daemon synthesizes the owner from the MCP process's uid via `SO_PEERCRED` — agents can only create mailboxes owned by themselves. To provision a mailbox owned by another uid (e.g. a service account), an operator must run `sudo aimx mailboxes create <name> --owner <user>` from the host CLI.

**Returns:** the new mailbox's full address (`<name>@<domain>`) on success. Idempotent: re-running `mailbox_create("foo")` on a mailbox you already own returns the existing address with no side effects.

**Errors:** surfaces the daemon's `ErrCode` + reason verbatim. Common cases:

- `Validation: reserved` — `name` was `catchall` or `aimx-catchall`.
- `Validation: ...` — name matched a structural rule violation (empty, contains `..`, leading/trailing `.`, invalid character, etc.).
- `daemon must be running for non-root mailbox CRUD` — daemon is offline; agents cannot fall back to a direct config edit.

---

#### `mailbox_delete`

Remove a mailbox the calling agent owns.

| Parameter | Type    | Required | Description |
|-----------|---------|----------|-------------|
| `name`    | string  | yes      | Mailbox name to delete. Caller's uid must equal the mailbox's `owner_uid`. |
| `force`   | bool    | no       | Default `false`. When `true`, the daemon wipes `inbox/<name>/` and `sent/<name>/` under per-mailbox lock + `CONFIG_WRITE_LOCK` before unregistering the mailbox. |

Without `force`, the daemon refuses non-empty mailboxes with `ERR NONEMPTY` and the tool surfaces a hint pointing at the CLI's interactive `--force` prompt. The catchall mailbox is refused with or without `force`.

**Errors:** surfaces the daemon's `ErrCode` + reason verbatim. Common cases:

- `EACCES not authorized` — caller's uid does not own the target mailbox.
- `NONEMPTY: inbox=N sent=M` — mailbox has files; pass `force: true` (or use the CLI's interactive `--force` prompt) to wipe them.
- `daemon must be running for non-root mailbox CRUD` — daemon is offline.

```json
{"name": "mailbox_create", "arguments": {"name": "task-42"}}
{"name": "mailbox_delete", "arguments": {"name": "task-42", "force": true}}
```

---

### Email tools

#### `email_list`

List a page of email metadata in a mailbox, sorted descending by filename (newest first). AIMX never scans on the agent's behalf — agents page through the listing and filter client-side.

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
| `body` | string | yes | Email body. Interpreted as Markdown by default — rendered to HTML and shipped as `multipart/alternative` with the Markdown source as the text part. |
| `text_only` | bool | no | When `true`, ship the body verbatim as `text/plain`. No Markdown rendering, no HTML alternative. Mutually exclusive with `html_body`. |
| `html_body` | string | no | Operator-supplied HTML used verbatim as the `text/html` part; `body` becomes the `text/plain` fallback. Mutually exclusive with `text_only`. |
| `attachments` | array of strings | no | File paths to attach |
| `reply_to` | string | no | Message-ID of the email being replied to. Sets the `In-Reply-To` header and (when `references` is omitted) builds the `References` chain automatically. Required to enable threading. Without `reply_to`, any `references` value is silently ignored and no threading headers are emitted |
| `references` | string | no | Full `References` header chain (space-separated Message-IDs). **Only applied when `reply_to` is also set.** Supplied alone, it is silently ignored |

The MCP server composes the RFC 5322 message and submits it to `aimx serve` over the local `/run/aimx/aimx.sock` UDS. `aimx serve` parses `From:` from the body, validates that the caller's uid owns the resolved mailbox, DKIM-signs the message, and delivers it directly to the recipient's MX server via SMTP. See [Markdown Email](markdown-email.md) for the rendering pipeline and the `--text-only` / `--html-body` semantics that mirror these MCP parameters.

For replies to a single sender, prefer `email_reply`. It handles threading headers and the `Re:` subject prefix automatically. Use `email_send` with `reply_to` / `references` only when you need to override the recipient list (e.g. reply-all) or build a custom threading chain.

---

#### `email_reply`

Reply to an email with correct threading.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `mailbox` | string | yes | Mailbox name containing the email to reply to. Must be owned by the caller. |
| `id` | string | yes | Email ID to reply to (e.g. `2025-01-15-001`) |
| `body` | string | yes | Reply body. Interpreted as Markdown by default — same semantics as `email_send`'s `body`. |
| `text_only` | bool | no | When `true`, ship the body verbatim as `text/plain`. Mutually exclusive with `html_body`. |
| `html_body` | string | no | Operator-supplied HTML used verbatim as the `text/html` part; `body` becomes the `text/plain` fallback. Mutually exclusive with `text_only`. |

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

Every email carries a TOML frontmatter block between `+++` delimiters. See [Mailboxes: Frontmatter fields](mailboxes.md#frontmatter-fields) for the full inbound schema and [Mailboxes: Outbound frontmatter](mailboxes.md#outbound-frontmatter) for the outbound additions.

## Agent-facing documentation

Two reference documents help agents understand AIMX:

- `agents/common/aimx-primer.md` — the canonical primer bundled into every agent plugin. Covers MCP tools, storage layout, frontmatter, trust model, workflows.
- `/var/lib/aimx/README.md` — the runtime datadir guide written by `aimx setup` and refreshed on `aimx serve` startup. Covers on-disk layout, file naming, slug algorithm, bundle rules.

## Example workflow

1. Call `email_list` and filter rows where `read == false`.
2. Call `email_read` with the mailbox and email ID, or read `<inbox_path>/<id>.md` from the filesystem.
3. Process the content.
4. Call `email_reply` with the response body.
5. Call `email_mark_read`.

For automated processing without MCP, use [hooks](hooks.md).
