# Mailboxes & Email

Mailboxes are the core organizational unit in aimx. Each mailbox maps an email address to a directory on disk.

## Concepts

- **Mailboxes are directories.** Creating a mailbox creates a folder and registers an address. No OS users, no passwords, no database.
- **Catchall.** The `catchall` mailbox (created by default during setup) receives email for any unrecognized address at your domain.
- **No restart required.** Mailbox changes take effect immediately -- `aimx ingest` reads the config on each invocation.

### Routing logic

When an email arrives, aimx matches the local part of the recipient address (the part before `@`) against mailbox names in the config. If a mailbox with that exact name exists, the email is delivered there. Otherwise it falls through to the `catchall` mailbox.

For example, with mailboxes `support` and `catchall` configured:
- `support@agent.yourdomain.com` -> delivered to the `support` mailbox
- `billing@agent.yourdomain.com` -> delivered to the `catchall` mailbox (no `billing` mailbox exists)
- `anything@agent.yourdomain.com` -> delivered to the `catchall` mailbox

## Managing mailboxes

### Create a mailbox

```bash
aimx mailbox create support
```

This creates `support@agent.yourdomain.com` and the directory `/var/lib/aimx/support/`.

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
id = "2025-01-15-001"
message_id = "<abc123@example.com>"
from = "Alice <alice@example.com>"
to = "support@agent.yourdomain.com"
subject = "Hello"
date = "2025-01-15T10:30:00Z"
in_reply_to = ""
references = ""
attachments = []
mailbox = "support"
read = false
dkim = "pass"
spf = "pass"
+++

Hello, this is the email body in plain text.
```

This format is designed to be agent-readable without parsing libraries. An agent can `cat` the file and understand it immediately.

### Frontmatter fields

| Field | Type | Description |
|-------|------|-------------|
| `id` | string | Unique email ID within the mailbox (e.g. `2025-01-15-001`) |
| `message_id` | string | RFC 5322 Message-ID header |
| `from` | string | Sender address with optional display name |
| `to` | string | Recipient address |
| `subject` | string | Email subject line |
| `date` | string | Email date in RFC 3339 format |
| `in_reply_to` | string | Message-ID of the email being replied to |
| `references` | string | Full threading chain of Message-IDs |
| `attachments` | array | Attachment metadata (see below) |
| `mailbox` | string | Mailbox name this email was routed to |
| `read` | bool | Read status (`false` on ingest) |
| `dkim` | string | DKIM verification result: `pass`, `fail`, or `none` |
| `spf` | string | SPF verification result: `pass`, `fail`, or `none` |

### Body extraction

- **Text/plain** is preferred when available
- Falls back to **text/html** converted to plaintext (via `html2text`)
- Stored as Markdown content after the frontmatter

## Attachments

Attachments are extracted to an `attachments/` subdirectory within the mailbox folder:

```
/var/lib/aimx/support/
├── 2025-01-15-001.md
└── attachments/
    ├── report.pdf
    └── image.png
```

Attachment metadata is stored in the email frontmatter:

```toml
[[attachments]]
filename = "report.pdf"
content_type = "application/pdf"
size = 45230
path = "attachments/report.pdf"
```

| Field | Description |
|-------|-------------|
| `filename` | Original filename |
| `content_type` | MIME type |
| `size` | File size in bytes |
| `path` | Relative path from the mailbox directory |

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

1. aimx composes an RFC 5322 compliant message
2. Signs the message with DKIM (RSA-SHA256) using your domain's private key
3. Hands the signed message to OpenSMTPD via `sendmail -t` for delivery

### Reply threading

When replying to an email, aimx sets the `In-Reply-To` and `References` headers so the reply is threaded correctly in the recipient's mail client. Use `--reply-to` with the original message's `Message-ID` value.

The `email_reply` MCP tool handles threading automatically -- it reads the original email and sets the correct headers.

## Email ID format

Each email receives a unique ID within its mailbox in the format `YYYY-MM-DD-NNN` (e.g. `2025-01-15-001`). The counter increments atomically per mailbox to prevent collisions.

---

Next: [Channel Rules](channels.md) | [MCP Server](mcp.md) | [Configuration](configuration.md)
