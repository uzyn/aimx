# MCP Server

> To install AIMX into your agent, see [Agent Integration](agent-integration.md).

AIMX includes a built-in MCP (Model Context Protocol) server that gives AI agents programmatic access to email. Agents can list, read, send, reply to, and manage email through standard MCP tool calls.

## Overview

- **Transport:** stdio (launched on-demand, no daemon)
- **Protocol:** Model Context Protocol (MCP)
- **Compatible with:** Claude Code, OpenClaw, Codex, and any MCP-compatible client

## Running the MCP server

```bash
aimx mcp
```

The server runs in stdio mode -- it reads from stdin and writes to stdout. It is launched on-demand by MCP clients, not run as a background service.

## Agent integration

Install AIMX into your agent with one command: see [Agent Integration](agent-integration.md)
for the full list of supported agents (Claude Code, Codex CLI, OpenCode,
Gemini CLI, Goose, OpenClaw) and one-line install commands. The
per-agent installer wires both the MCP server and the agent-facing
skill/recipe into the correct per-user location — no copy-paste of
JSON snippets from this chapter is required.

If your agent is not in the registry yet, see the "Manual MCP wiring"
section of the agent integration chapter for the generic pattern.

## MCP tools

AIMX exposes 9 MCP tools organized into mailbox management and email operations.

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
| `folder` | string | no | `"inbox"` (default) or `"sent"` — picks which side of the mailbox to list |
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
| `reply_to` | string | no | Message-ID of the email being replied to. Sets the `In-Reply-To` header and (when `references` is omitted) builds the `References` chain automatically |
| `references` | string | no | Full `References` header chain (space-separated Message-IDs). Typically used alongside `reply_to` for reply-all or manually threaded sends |

The MCP server composes the RFC 5322 message and submits it to `aimx serve` over the local `/run/aimx/send.sock` UDS. `aimx serve` DKIM-signs the message and delivers it directly to the recipient's MX server via SMTP.

For replies to a single sender, prefer `email_reply` — it handles threading headers and the `Re:` subject prefix automatically. Use `email_send` with `reply_to` / `references` only when you need to override the recipient list (e.g. reply-all) or build a custom threading chain.

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

Updates `read = true` in the email's frontmatter. The MCP server is non-root so it routes the write through `aimx serve` over the local UDS (`/run/aimx/send.sock`) rather than touching the root-owned mailbox file directly. If `aimx serve` is not running the tool returns an error hint to start the daemon.

---

#### `email_mark_unread`

Mark an email as unread.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `mailbox` | string | yes | Mailbox name |
| `id` | string | yes | Email ID (filename stem, e.g. `2025-01-15-103000-meeting`) |
| `folder` | string | no | `"inbox"` (default) or `"sent"` |

Updates `read = false` in the email's frontmatter. Same daemon-mediated write path as `email_mark_read` — requires a running `aimx serve`.

## Frontmatter reference

Every email stored by AIMX carries a TOML frontmatter block between `+++` delimiters. Inbound emails include:

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
| `trusted` | yes | `none`, `true`, or `false` -- per-mailbox trust evaluation result (see [Configuration](configuration.md)) |
| `mailbox` | yes | Target mailbox name |
| `read` | yes | Read/unread status |
| `delivery_status` | sent only | `delivered`, `failed`, `deferred`, or `pending` |

Outbound (sent) emails additionally carry `outbound = true`, `delivery_status`, and optionally `delivered_at` and `delivery_details`.

## Agent-facing documentation

Two reference documents help agents understand AIMX:

- **`agents/common/aimx-primer.md`** — the canonical primer bundled into every agent plugin. Covers MCP tools, storage layout, frontmatter, trust model, and common workflows.
- **`/var/lib/aimx/README.md`** — the runtime datadir guide written by `aimx setup` and refreshed on `aimx serve` startup. Covers the on-disk layout, file naming, slug algorithm, bundle rules, and the UDS send protocol.

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

1. **Check for new mail** -- call `email_list` with `unread: true`
2. **Read an email** -- call `email_read` with the mailbox and email ID
3. **Process the content** -- agent decides how to respond
4. **Reply** -- call `email_reply` with the response body
5. **Mark as read** -- call `email_mark_read`

For automated processing without MCP, use [hooks](hooks.md) to trigger commands on incoming email.

---

Next: [Hooks & Trust](hooks.md) | [Mailboxes & Email](mailboxes.md) | [Setup](setup.md)
