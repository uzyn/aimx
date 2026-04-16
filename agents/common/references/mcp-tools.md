# AIMX MCP tools — full reference

All tools are served by the `aimx` binary over stdio (MCP transport). They
return strings on success and error strings on failure. The MCP server is
launched on-demand by your MCP client — it is not a long-running process.

## Mailbox tools

### `mailbox_create`

Create a new mailbox. This creates both `inbox/<name>/` and `sent/<name>/`
directories and registers the address in `config.toml`.

**Parameters:**
| Name   | Type   | Required | Description |
|--------|--------|----------|-------------|
| `name` | string | yes      | Local part of the email address (e.g. `agent` for `agent@domain`) |

**Returns:** Confirmation string.

**Example:**
```
mailbox_create(name: "reports")
→ "Mailbox 'reports' created."
```

**Errors:**
- Mailbox already exists.
- Invalid name (must be valid email local part).

---

### `mailbox_list`

List all mailboxes with total and unread message counts.

**Parameters:** None.

**Returns:** Formatted list of mailboxes with counts.

**Example:**
```
mailbox_list()
→ "agent (inbox: 12 total, 3 unread; sent: 5 total)
   reports (inbox: 0 total, 0 unread; sent: 0 total)"
```

---

### `mailbox_delete`

Delete a mailbox and all its mail. This removes both `inbox/<name>/` and
`sent/<name>/` directories. Irreversible.

**Parameters:**
| Name   | Type   | Required | Description |
|--------|--------|----------|-------------|
| `name` | string | yes      | Mailbox name to delete |

**Returns:** Confirmation string.

**Example:**
```
mailbox_delete(name: "old-project")
→ "Mailbox 'old-project' deleted."
```

**Errors:**
- Mailbox does not exist.

---

## Email tools

### `email_list`

List emails in a mailbox with optional filters. All filters AND together.

**Parameters:**
| Name      | Type    | Required | Description |
|-----------|---------|----------|-------------|
| `mailbox` | string  | yes      | Mailbox name |
| `folder`  | string  | no       | `"inbox"` (default) or `"sent"` |
| `unread`  | bool    | no       | Filter to only unread emails |
| `from`    | string  | no       | Filter by sender address (substring match) |
| `since`   | string  | no       | Filter to emails since this datetime (RFC 3339, e.g. `2026-01-01T00:00:00Z`) |
| `subject` | string  | no       | Filter by subject (case-insensitive substring match) |

**Returns:** Formatted list of matching emails.

**Example — list unread inbox:**
```
email_list(mailbox: "agent", unread: true)
→ "2026-04-15-143022-meeting-notes | From: alice@company.com | Subject: Meeting Notes | 2026-04-15T14:30:22Z
   2026-04-15-153300-invoice-march | From: billing@vendor.com | Subject: Invoice March | 2026-04-15T15:33:00Z"
```

**Example — list sent mail:**
```
email_list(mailbox: "agent", folder: "sent")
→ "2026-04-15-160145-re-meeting-notes | To: alice@company.com | Subject: Re: Meeting Notes | 2026-04-15T16:01:45Z"
```

**Example — filter by sender since a date:**
```
email_list(mailbox: "agent", from: "alice", since: "2026-04-01T00:00:00Z")
```

---

### `email_read`

Return the full Markdown file (TOML frontmatter + body) for a single email.

**Parameters:**
| Name      | Type   | Required | Description |
|-----------|--------|----------|-------------|
| `mailbox` | string | yes      | Mailbox name |
| `id`      | string | yes      | Email ID — the filename stem (e.g. `2026-04-15-143022-meeting-notes`) |
| `folder`  | string | no       | `"inbox"` (default) or `"sent"` |

**Returns:** Full file content as a string (frontmatter + body).

**Example:**
```
email_read(mailbox: "agent", id: "2026-04-15-143022-meeting-notes")
→ "+++
   id = \"2026-04-15-143022-meeting-notes\"
   message_id = \"<abc@example.com>\"
   ...
   +++

   Hello, here are the meeting notes..."
```

**Errors:**
- Email not found.
- Invalid ID format.

---

### `email_send`

Compose, DKIM-sign, and deliver an email. The message is submitted to
`aimx serve` via UDS, which signs it with the server's DKIM key and delivers
directly to the recipient's MX server.

**Parameters:**
| Name           | Type     | Required | Description |
|---------------|----------|----------|-------------|
| `from_mailbox` | string   | yes      | Mailbox name to send from (local part only) |
| `to`           | string   | yes      | Recipient email address |
| `subject`      | string   | yes      | Email subject |
| `body`         | string   | yes      | Email body text |
| `attachments`  | string[] | no       | Absolute file paths to attach |

**Returns:** Confirmation with Message-ID.

**Example — plain text:**
```
email_send(
  from_mailbox: "agent",
  to: "alice@example.com",
  subject: "Status Update",
  body: "All systems operational."
)
→ "Sent. Message-ID: <abc123@domain.com>"
```

**Example — with attachments:**
```
email_send(
  from_mailbox: "agent",
  to: "bob@example.com",
  subject: "Report",
  body: "Please see attached.",
  attachments: ["/tmp/report.csv", "/tmp/chart.png"]
)
```

**Errors:**
- Mailbox does not exist.
- Daemon not running (socket missing).
- Sender domain mismatch.
- Delivery failure (remote MX rejected).

---

### `email_reply`

Reply to an existing email. AIMX automatically sets `In-Reply-To`,
`References`, and prepends `Re:` to the subject.

**Parameters:**
| Name      | Type   | Required | Description |
|-----------|--------|----------|-------------|
| `mailbox` | string | yes      | Mailbox containing the email to reply to |
| `id`      | string | yes      | Email ID to reply to |
| `body`    | string | yes      | Reply body text |

**Returns:** Confirmation with Message-ID.

**Example:**
```
email_reply(
  mailbox: "agent",
  id: "2026-04-15-143022-meeting-notes",
  body: "Thanks, I'll review the notes."
)
→ "Sent. Message-ID: <def456@domain.com>"
```

---

### `email_mark_read`

Mark a single email as read (sets `read = true` in frontmatter).

**Parameters:**
| Name      | Type   | Required | Description |
|-----------|--------|----------|-------------|
| `mailbox` | string | yes      | Mailbox name |
| `id`      | string | yes      | Email ID |
| `folder`  | string | no       | `"inbox"` (default) or `"sent"` |

**Returns:** Confirmation string.

**Example:**
```
email_mark_read(mailbox: "agent", id: "2026-04-15-143022-meeting-notes")
→ "Marked as read."
```

---

### `email_mark_unread`

Mark a single email as unread (sets `read = false` in frontmatter).

**Parameters:**
| Name      | Type   | Required | Description |
|-----------|--------|----------|-------------|
| `mailbox` | string | yes      | Mailbox name |
| `id`      | string | yes      | Email ID |
| `folder`  | string | no       | `"inbox"` (default) or `"sent"` |

**Returns:** Confirmation string.

**Example:**
```
email_mark_unread(mailbox: "agent", id: "2026-04-15-143022-meeting-notes")
→ "Marked as unread."
```
