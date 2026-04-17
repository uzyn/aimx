# Mailboxes & Email

Mailboxes are the core organizational unit in AIMX. Each mailbox maps an email address to a directory on disk.

## Concepts

- **Mailboxes are directories.** Creating a mailbox creates two folders (one under `inbox/` and one under `sent/`) and registers an address. No OS users, no passwords, no database.
- **Catchall.** The `catchall` mailbox (created by default during setup) receives email for any unrecognized address at your domain. Catchall is inbox-only — no `sent/catchall/` directory is created.
- **Restart `aimx` after `create` / `delete`.** `aimx serve` reads `config.toml` once at startup, so a freshly-created `[mailboxes.<name>]` entry doesn't reach the running daemon until you restart it (`sudo systemctl restart aimx`, or `sudo rc-service aimx restart` on OpenRC). Inbound mail addressed to an unrestarted mailbox silently falls through to `catchall`. The `aimx mailbox create` / `delete` commands print a follow-up hint reminding you of this. (Sprint 46 will remove the requirement by routing mailbox CRUD through the daemon over UDS.)

### On-disk layout

```
/var/lib/aimx/
├── inbox/              # inbound mail lives here
│   ├── catchall/
│   └── support/
└── sent/               # outbound sent copies
    └── support/
```

Each email is stored as either a flat `YYYY-MM-DD-HHMMSS-<slug>.md` file
when it has zero attachments, or as a Zola-style bundle directory
`YYYY-MM-DD-HHMMSS-<slug>/` containing `<stem>.md` plus every attachment
as a sibling file when attachments are present.

### Routing logic

When an email arrives, AIMX matches the local part of the recipient address (the part before `@`) against mailbox names in the config. If a mailbox with that exact name exists, the email is delivered there. Otherwise it falls through to the `catchall` mailbox.

For example, with mailboxes `support` and `catchall` configured:
- `support@agent.yourdomain.com` -> delivered to the `support` mailbox
- `billing@agent.yourdomain.com` -> delivered to the `catchall` mailbox (no `billing` mailbox exists)
- `anything@agent.yourdomain.com` -> delivered to the `catchall` mailbox

## Managing mailboxes

### Create a mailbox

```bash
aimx mailbox create support
```

This creates `support@agent.yourdomain.com` and both directories:
`/var/lib/aimx/inbox/support/` (for incoming mail) and
`/var/lib/aimx/sent/support/` (for outbound copies). Deletion removes
both; `catchall` cannot be deleted.

### List mailboxes

```bash
aimx mailbox list
```

Shows all mailboxes with their addresses and message counts (total and unread).

### Delete a mailbox

```bash
aimx mailbox delete support
```

Deletes the mailbox directory and all its emails. Prompts for confirmation. Use `--yes` to skip the prompt.

Mailboxes can also be managed via [MCP tools](mcp.md#mailbox-tools) (`mailbox_list`, `mailbox_create`, `mailbox_delete`).

## Email format

Incoming emails are parsed from raw MIME (`.eml`) and stored as Markdown with TOML frontmatter:

```markdown
+++
id = "2025-04-15-143022-hello"
message_id = "abc123@example.com"
thread_id = "a1b2c3d4e5f6a7b8"
from = "Alice <alice@example.com>"
to = "support@agent.yourdomain.com"
delivered_to = "support@agent.yourdomain.com"
subject = "Hello"
date = "2025-04-15T14:30:22Z"
received_at = "2025-04-15T14:30:23Z"
size_bytes = 1024
dkim = "pass"
spf = "pass"
dmarc = "pass"
trusted = "true"
mailbox = "support"
read = false
+++

Hello, this is the email body in plain text.
```

This format is designed to be agent-readable without parsing libraries. An agent can `cat` the file and understand it immediately.

### Frontmatter fields

| Field | Type | Description |
|-------|------|-------------|
| `id` | string | Filename stem (e.g. `2025-04-15-143022-hello`) |
| `message_id` | string | RFC 5322 Message-ID |
| `thread_id` | string | 16-hex-char SHA-256 of the thread root Message-ID |
| `from` | string | Sender address with optional display name |
| `to` | string | Recipient address |
| `delivered_to` | string | Actual RCPT TO address |
| `subject` | string | Email subject line |
| `date` | string | Sender-claimed date in RFC 3339 format |
| `received_at` | string | Server-side receipt datetime (RFC 3339 UTC) |
| `size_bytes` | integer | Raw message size in bytes |
| `attachments` | array | Attachment metadata (see below) |
| `in_reply_to` | string | Message-ID of the email being replied to (optional, omitted when empty) |
| `references` | string | Full threading chain of Message-IDs (optional, omitted when empty) |
| `dkim` | string | DKIM verification result: `pass`, `fail`, or `none` |
| `spf` | string | SPF verification result: `pass`, `fail`, `softfail`, `neutral`, or `none` |
| `dmarc` | string | DMARC alignment result: `pass`, `fail`, or `none` |
| `trusted` | string | Per-mailbox trust evaluation: `none`, `true`, or `false` |
| `mailbox` | string | Mailbox name this email was routed to |
| `read` | bool | Read status (`false` on ingest) |

### Body extraction

- **Text/plain** is preferred when available
- Falls back to **text/html** converted to plaintext (via `html2text`)
- Stored as Markdown content after the frontmatter

## Attachments

When an email carries one or more attachments, AIMX writes a Zola-style
bundle directory whose name matches the `.md` file's stem:

```
/var/lib/aimx/inbox/support/
├── 2025-01-15-103000-status-update.md         # flat: no attachments
└── 2025-01-15-104500-quarterly-report/        # bundle: one or more attachments
    ├── 2025-01-15-104500-quarterly-report.md
    ├── report.pdf
    └── image.png
```

Attachment metadata is stored in the email frontmatter:

```toml
[[attachments]]
filename = "report.pdf"
content_type = "application/pdf"
size = 45230
path = "report.pdf"
```

| Field | Description |
|-------|-------------|
| `filename` | Original filename (path components stripped, duplicates suffixed `-1`, `-2`, …) |
| `content_type` | MIME type |
| `size` | File size in bytes |
| `path` | Path relative to the bundle directory (no `attachments/` prefix in v0.2) |

## Read/unread tracking

Emails are marked `read = false` on ingest. Use MCP tools or update the frontmatter directly:

- **MCP:** `email_mark_read` and `email_mark_unread` (see [MCP Server](mcp.md#email-tools))
- **CLI/filesystem:** Edit the `read` field in the `.md` file's frontmatter

The `email_list` MCP tool supports an `unread` filter to list only unread emails.

## Sending email

### Via CLI

```bash
# Basic send
aimx send --from support@agent.yourdomain.com \
          --to recipient@gmail.com \
          --subject "Hello" \
          --body "Message body"

# With attachments
aimx send --from support@agent.yourdomain.com \
          --to recipient@gmail.com \
          --subject "Report" \
          --body "See attached." \
          --attachment /path/to/report.pdf

# Reply to a message (preserves threading)
aimx send --from support@agent.yourdomain.com \
          --to recipient@gmail.com \
          --subject "Re: Hello" \
          --body "Reply body" \
          --reply-to "<original-message-id@example.com>"
```

### Via MCP

Agents send email using the `email_send` and `email_reply` MCP tools. See [MCP Server](mcp.md#email-tools) for details.

### How sending works

1. `aimx send` composes an RFC 5322 compliant message and submits it to `aimx serve` over the local `/run/aimx/send.sock` UDS.
2. `aimx serve` validates the sender mailbox + domain, DKIM-signs the message (RSA-SHA256) with the domain's private key it loaded at startup, and delivers the signed message directly to the recipient's MX server via SMTP.
3. `aimx send` exits as soon as the daemon returns a status — signing and delivery never run inside the client, so it does not need to read the DKIM key and does not need to run as root.

### Reply threading

When replying to an email, AIMX sets the `In-Reply-To` and `References` headers so the reply is threaded correctly in the recipient's mail client. Use `--reply-to` with the original message's `Message-ID` value.

The `email_reply` MCP tool handles threading automatically -- it reads the original email and sets the correct headers.

## Email ID format

Each email's `id` field is the filename stem
`YYYY-MM-DD-HHMMSS-<slug>` in UTC. The slug is derived from the subject:
lowercase, non-alphanumeric runs collapsed to `-`, trimmed, capped at 20
characters, with `no-subject` as a fallback for empty results. Two emails
with the same subject in the same UTC second have `-2`, `-3`, … appended
to disambiguate.

---

Next: [Channel Rules](channels.md) | [MCP Server](mcp.md) | [Configuration](configuration.md)
