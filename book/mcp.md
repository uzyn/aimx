# MCP Server

> To install aimx into your agent, see [Agent Integration](agent-integration.md).

aimx includes a built-in MCP (Model Context Protocol) server that gives AI agents programmatic access to email. Agents can list, read, send, reply to, and manage email through standard MCP tool calls.

## Overview

- **Transport:** stdio (launched on-demand, no daemon)
- **Protocol:** Model Context Protocol (MCP)
- **Compatible with:** Claude Code, OpenClaw, Codex, and any MCP-compatible client

## Running the MCP server

```bash
aimx mcp
```

The server runs in stdio mode. It reads from stdin and writes to stdout. It is launched on-demand by MCP clients, not run as a background service.

## Agent integration

See [Agent Integration](agent-integration.md) for one-line `aimx agent-setup <agent>` installers and the manual MCP wiring pattern for clients not yet in the registry.

## MCP tools

aimx exposes 13 MCP tools organized into mailbox management, email operations, and hook templates.

### Mailbox tools

#### `mailbox_list`

List all mailboxes with message counts.

**Parameters:** none

**Returns:** List of mailboxes with addresses, total count, and unread count.

---

#### `mailbox_create`

Create a new mailbox.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `name` | string | yes | Mailbox name (becomes the local part of the email address) |

Creates `<name>@yourdomain.com` and the corresponding directory. No mail server restart required.

---

#### `mailbox_delete`

Delete a mailbox and all its emails.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `name` | string | yes | Mailbox name to delete |

---

### Email tools

#### `email_list`

List emails in a mailbox with optional filters.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `mailbox` | string | yes | Mailbox name to list emails from |
| `folder` | string | no | `"inbox"` (default) or `"sent"`. Picks which side of the mailbox to list |
| `unread` | bool | no | Filter to only unread emails |
| `from` | string | no | Filter by sender address (substring match) |
| `since` | string | no | Filter to emails since this datetime (RFC 3339 format) |
| `subject` | string | no | Filter by subject (substring match, case-insensitive) |

**Returns:** Email metadata (frontmatter only, not body).

---

#### `email_read`

Read the full content of an email.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `mailbox` | string | yes | Mailbox name |
| `id` | string | yes | Email ID, i.e. the filename stem (e.g. `2025-01-15-103000-meeting`) |
| `folder` | string | no | `"inbox"` (default) or `"sent"` |

**Returns:** Complete `.md` file content including frontmatter and body.

---

#### `email_send`

Compose and send an email with DKIM signing.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `from_mailbox` | string | yes | Mailbox name to send from |
| `to` | string | yes | Recipient email address |
| `subject` | string | yes | Email subject |
| `body` | string | yes | Email body text |
| `attachments` | array of strings | no | File paths to attach |
| `reply_to` | string | no | Message-ID of the email being replied to. Sets the `In-Reply-To` header and (when `references` is omitted) builds the `References` chain automatically. Required to enable threading. Without `reply_to`, any `references` value is silently ignored and no threading headers are emitted |
| `references` | string | no | Full `References` header chain (space-separated Message-IDs). **Only applied when `reply_to` is also set.** Supplied alone, it is silently ignored |

The MCP server composes the RFC 5322 message and submits it to `aimx serve` over the local `/run/aimx/aimx.sock` UDS. `aimx serve` DKIM-signs the message and delivers it directly to the recipient's MX server via SMTP.

For replies to a single sender, prefer `email_reply`. It handles threading headers and the `Re:` subject prefix automatically. Use `email_send` with `reply_to` / `references` only when you need to override the recipient list (e.g. reply-all) or build a custom threading chain.

---

#### `email_reply`

Reply to an email with correct threading.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `mailbox` | string | yes | Mailbox name containing the email to reply to |
| `id` | string | yes | Email ID to reply to (e.g. `2025-01-15-001`) |
| `body` | string | yes | Reply body text |

Automatically sets `In-Reply-To` and `References` headers from the original email for proper thread grouping in the recipient's mail client.

---

#### `email_mark_read`

Mark an email as read.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `mailbox` | string | yes | Mailbox name |
| `id` | string | yes | Email ID (filename stem, e.g. `2025-01-15-103000-meeting`) |
| `folder` | string | no | `"inbox"` (default) or `"sent"` |

Updates `read = true` in the email's frontmatter. The MCP server is non-root so it routes the write through `aimx serve` over the local UDS (`/run/aimx/aimx.sock`) rather than touching the root-owned mailbox file directly. If `aimx serve` is not running the tool returns an error hint to start the daemon.

---

#### `email_mark_unread`

Mark an email as unread.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `mailbox` | string | yes | Mailbox name |
| `id` | string | yes | Email ID (filename stem, e.g. `2025-01-15-103000-meeting`) |
| `folder` | string | no | `"inbox"` (default) or `"sent"` |

Updates `read = false` in the email's frontmatter. Same daemon-mediated write path as `email_mark_read`. Requires a running `aimx serve`.

---

### Hook template tools

Four tools let agents safely self-configure hooks without shell access. See [Hooks & Trust § Template hooks](hooks.md#template-hooks-recommended) for the model and why raw `cmd` submission over MCP is rejected by design.

#### `hook_list_templates`

Enumerate hook templates enabled on this install.

**Parameters:** none

**Returns:** JSON array of template descriptors, each with `name`, `description`, `params`, and `allowed_events`. Empty `[]` when no templates are enabled — the operator must install them via `sudo aimx setup`.

Example:

```json
[
  {
    "name": "invoke-claude",
    "description": "Pipe email into Claude Code with a prompt",
    "params": ["prompt"],
    "allowed_events": ["on_receive", "after_send"]
  },
  {
    "name": "webhook",
    "description": "POST the email as JSON to a URL",
    "params": ["url"],
    "allowed_events": ["on_receive", "after_send"]
  }
]
```

---

#### `hook_create`

Bind a template to a mailbox, creating a new hook. The daemon stamps `origin = "mcp"`.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `mailbox` | string | yes | Target mailbox name |
| `event` | string | yes | `"on_receive"` or `"after_send"` (must be in the template's `allowed_events`) |
| `template` | string | yes | Template name from `hook_list_templates` |
| `params` | object | yes | Key/value map for the template's declared `params` |
| `name` | string | no | Explicit hook name. When omitted, a stable 12-hex-char name is derived from `(event, template, params)` |

**Returns:** `{effective_name, substituted_argv}` — the hook name the daemon wrote and the resolved argv the sandboxed executor will run, for confirmation in the agent's UI.

Safety model: the tool refuses any request body that contains a raw `cmd`, `run_as`, `dangerously_support_untrusted`, `timeout_secs`, or `stdin` field. These are template properties, not hook properties. An agent cannot smuggle arbitrary shell past the template boundary.

**Error examples:**

- `Unknown template: foo (run hook_list_templates to see enabled templates)`
- `missing-param: prompt`
- `event-not-allowed: after_send on template webhook_receive_only`
- `mailbox-not-found: accounts`

---

#### `hook_list`

List hooks visible to MCP across all (or one) mailbox.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `mailbox` | string | no | Filter to one mailbox; omit to list all |

**Returns:** JSON array. Each entry has `name`, `mailbox`, `event`, `origin`, and — for `origin = "mcp"` only — `template` + `params`. Operator-origin hooks have `cmd` / `params` **masked** so an agent can avoid duplicates without snooping on operator logic.

```json
[
  {"name": "accounts-auto-reply", "mailbox": "accounts", "event": "on_receive", "origin": "mcp", "template": "invoke-claude", "params": {"prompt": "..."}},
  {"name": "op_audit", "mailbox": "accounts", "event": "on_receive", "origin": "operator"}
]
```

---

#### `hook_delete`

Delete a hook by name.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `name` | string | yes | Effective hook name (explicit or derived) |

MCP can only delete hooks whose `origin = "mcp"`. Attempts to delete an operator-origin hook return `ERR origin-protected`:

```text
ERR origin-protected: hook was created by the operator — remove via `sudo aimx hooks delete` instead
```

The agent should surface this verbatim so the user knows the hook is operator-owned.

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

- **`agents/common/aimx-primer.md`**: the canonical primer bundled into every agent plugin. Covers MCP tools, storage layout, frontmatter, trust model, and common workflows.
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

1. **Check for new mail.** Call `email_list` with `unread: true`
2. **Read an email.** Call `email_read` with the mailbox and email ID
3. **Process the content.** Agent decides how to respond
4. **Reply.** Call `email_reply` with the response body
5. **Mark as read.** Call `email_mark_read`

For automated processing without MCP, use [hooks](hooks.md) to trigger commands on incoming email.

---

Next: [Hooks & Trust](hooks.md) | [Mailboxes & Email](mailboxes.md) | [Setup](setup.md)
