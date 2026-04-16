# AIMX workflows — worked examples

Practical task recipes for common email operations. Each workflow shows the
MCP tool calls in order with example parameters.

## 1. Triage inbox

Process all unread mail, categorize by sender, and mark as read:

```
# Get all unread
email_list(mailbox: "agent", unread: true)

# For each email in results:
email_read(mailbox: "agent", id: "<id>")
# → Parse frontmatter: check `from`, `subject`, `trusted`, `auto_submitted`
# → Take appropriate action (reply, forward info, log, ignore)

email_mark_read(mailbox: "agent", id: "<id>")
```

Triage tip: check `auto_submitted` first. If it is `auto-generated` or
`auto-replied`, skip replying to avoid infinite loops. Check `trusted` to
gauge sender authenticity.

## 2. Thread summarization

Reconstruct and summarize an email thread:

```
# List all messages in the mailbox
email_list(mailbox: "agent")

# Read each message
email_read(mailbox: "agent", id: "<id>")

# Group by thread_id from frontmatter
# Sort each group by date (ascending)
# Summarize the conversation flow
```

All messages in a thread share the same `thread_id`, derived from the
earliest Message-ID in the `References`/`In-Reply-To` chain.

## 3. React to auto-submitted mail

Some messages are automated (bounce notices, calendar invites, CI
notifications). Always check before replying:

```
email_read(mailbox: "agent", id: "<id>")
# Check frontmatter:
#   auto_submitted = "auto-generated"  → do NOT reply
#   auto_submitted = "auto-replied"    → do NOT reply
#   auto_submitted is absent           → safe to reply if needed
```

Replying to auto-submitted mail creates infinite loops. Log or process
the information silently instead.

## 4. Handle attachments

Read an email that has attachments:

```
email_read(mailbox: "agent", id: "2026-04-15-153300-invoice-march")
```

The frontmatter will contain:
```toml
[[attachments]]
filename = "invoice.pdf"
content_type = "application/pdf"
size = 45678
path = "invoice.pdf"
```

For bundled emails, attachments are siblings of the `.md` file:
```
/var/lib/aimx/inbox/agent/2026-04-15-153300-invoice-march/
├── 2026-04-15-153300-invoice-march.md
├── invoice.pdf
└── receipt.png
```

Read attachment files directly from the filesystem using the path relative
to the bundle directory.

To send with attachments:
```
email_send(
  from_mailbox: "agent",
  to: "accounting@example.com",
  subject: "Processed invoice",
  body: "See attached processed version.",
  attachments: ["/tmp/processed-invoice.pdf"]
)
```

## 5. Reply-all

AIMX's `email_reply` sends to the original sender. To reply to all
recipients, use `email_send` with explicit addresses:

```
# Read the original to get all recipients
email_read(mailbox: "agent", id: "<id>")
# Parse frontmatter: from, to, cc

# Compose with all original recipients
email_send(
  from_mailbox: "agent",
  to: "<original-from>, <original-to>, <original-cc>",
  subject: "Re: <original-subject>",
  body: "<reply-body>"
)
```

Note: when using `email_send` for reply-all, you must manually set the
subject with `Re:` prefix. Threading headers (`In-Reply-To`, `References`)
are only set automatically by `email_reply`.

## 6. Filter by list-id

Process only mailing list mail:

```
email_list(mailbox: "agent")

# For each email:
email_read(mailbox: "agent", id: "<id>")
# Check frontmatter: if list_id is present, it's from a mailing list
# e.g. list_id = "<dev.lists.example.com>"

# Handle list mail differently:
# - Don't reply to the list unless explicitly needed
# - Extract information without generating outbound mail
```

## 7. Ingest a bounce

Bounce notices arrive as regular inbound mail. Identify them by:

- `auto_submitted = "auto-generated"` or `auto_submitted = "auto-replied"`
- Subject contains "Delivery Status Notification", "Undeliverable", or
  similar
- `from` contains `mailer-daemon` or `postmaster`

Do not reply to bounces. Instead:
1. Read the bounce content to identify the failed recipient.
2. Update your records or alert the operator.
3. Mark as read.

```
email_read(mailbox: "agent", id: "<bounce-id>")
# Parse body for the original recipient and failure reason
email_mark_read(mailbox: "agent", id: "<bounce-id>")
```

## 8. Mark all read

Mark every unread email in a mailbox as read:

```
email_list(mailbox: "agent", unread: true)

# For each email ID in the results:
email_mark_read(mailbox: "agent", id: "<id>")
```

There is no bulk mark-read tool — iterate through each message.

## 9. Check sent mail status

Review delivery status of sent emails:

```
email_list(mailbox: "agent", folder: "sent")

# For each sent email:
email_read(mailbox: "agent", id: "<id>", folder: "sent")
# Check frontmatter:
#   delivery_status = "delivered"  → accepted by remote MX
#   delivery_status = "failed"    → rejected, check delivery_details
#   delivery_status = "deferred"  → temporary failure
```

## 10. Create a mailbox and send first email

Set up a new agent identity and send the first message:

```
# Create the mailbox
mailbox_create(name: "notifications")

# Send the first email
email_send(
  from_mailbox: "notifications",
  to: "admin@example.com",
  subject: "Notifications mailbox active",
  body: "This mailbox is now operational."
)

# Verify delivery
email_list(mailbox: "notifications", folder: "sent")
```

## 11. Process mail from a specific sender since a date

Filter and process a targeted subset of mail:

```
email_list(
  mailbox: "agent",
  from: "alice@company.com",
  since: "2026-04-01T00:00:00Z"
)

# Read and process each matching email
email_read(mailbox: "agent", id: "<id>")
email_mark_read(mailbox: "agent", id: "<id>")
```

## 12. Direct filesystem read (bulk processing)

When MCP tool calls are too slow for bulk operations, read `.md` files
directly from the filesystem:

```
# Scan a mailbox directory
ls /var/lib/aimx/inbox/agent/

# Read a specific email file
cat /var/lib/aimx/inbox/agent/2026-04-15-143022-meeting-notes.md

# Parse the TOML frontmatter between +++ delimiters
# Extract fields like from, subject, trusted, read
```

The filesystem is world-readable and the format is stable. Use direct reads
for scanning, grep-based filtering, or when processing hundreds of messages.
Use MCP tools for mutations (mark read, send, reply, delete).
