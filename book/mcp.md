# MCP Server

aimx includes a built-in MCP (Model Context Protocol) server that gives AI agents programmatic access to email. Agents can list, read, send, reply to, and manage email through standard MCP tool calls.

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

Add aimx to any MCP-compatible AI agent (Claude Code, OpenClaw, Codex, OpenCode, etc.). The configuration snippet is the same for all agents that support MCP stdio transport:

```json
{
  "mcpServers": {
    "email": {
      "command": "/usr/local/bin/aimx",
      "args": ["mcp"]
    }
  }
}
```

For Claude Code, add this to `~/.claude/settings.json`. Other agents may use different config file locations -- check your agent's MCP documentation.

### Custom data directory

If you use a non-default data directory, pass it via `--data-dir`:

```json
{
  "mcpServers": {
    "email": {
      "command": "/usr/local/bin/aimx",
      "args": ["--data-dir", "/custom/path", "mcp"]
    }
  }
}
```

## MCP tools

aimx exposes 9 MCP tools organized into mailbox management and email operations.

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
| `id` | string | yes | Email ID (e.g. `2025-01-15-001`) |

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

The email is composed as RFC 5322, DKIM-signed, and handed to OpenSMTPD for delivery.

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
| `id` | string | yes | Email ID (e.g. `2025-01-15-001`) |

Updates `read = true` in the email's frontmatter.

---

#### `email_mark_unread`

Mark an email as unread.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `mailbox` | string | yes | Mailbox name |
| `id` | string | yes | Email ID (e.g. `2025-01-15-001`) |

Updates `read = false` in the email's frontmatter.

## Compatible agent frameworks

| Framework | Integration method |
|-----------|-------------------|
| Claude Code | MCP stdio mode via `~/.claude/settings.json` |
| OpenClaw | MCP stdio mode or [channel rules](channels.md) via shell |
| OpenCode | MCP stdio mode |
| Codex | [Channel rules](channels.md) via shell command |
| Any MCP client | Standard MCP stdio transport |

## Example workflow

A typical agent email workflow:

1. **Check for new mail** -- call `email_list` with `unread: true`
2. **Read an email** -- call `email_read` with the mailbox and email ID
3. **Process the content** -- agent decides how to respond
4. **Reply** -- call `email_reply` with the response body
5. **Mark as read** -- call `email_mark_read`

For automated processing without MCP, use [channel rules](channels.md) to trigger commands on incoming email.

---

Next: [Channel Rules](channels.md) | [Mailboxes & Email](mailboxes.md) | [Setup](setup.md)
