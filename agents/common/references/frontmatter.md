# AIMX frontmatter schema — full reference

Every email file begins with a TOML frontmatter block between `+++`
delimiters. Fields are ordered by section: Identity, Parties, Content,
Threading, Auth, Storage, and (for sent copies) an Outbound block.

## Inbound frontmatter

### Identity

| Field        | Type   | Required | Notes |
|-------------|--------|----------|-------|
| `id`        | string | always   | Filename stem, e.g. `2026-04-15-143022-meeting-notes` |
| `message_id`| string | always   | RFC 5322 Message-ID without angle brackets |
| `thread_id` | string | always   | First 16 hex chars of SHA-256 over the resolved thread root Message-ID. Walk `In-Reply-To` first, then `References` (leftmost), fall back to own Message-ID |

### Parties

| Field         | Type    | Required | Notes |
|--------------|---------|----------|-------|
| `from`       | string  | always   | Sender address, may include display name (e.g. `Alice <alice@example.com>`) |
| `to`         | string  | always   | Recipient address |
| `cc`         | string  | optional | Carbon copy recipients. Omitted when empty |
| `reply_to`   | string  | optional | Reply-To header value. Omitted when empty |
| `delivered_to`| string | always   | Actual RCPT TO address. Disambiguates list mail from direct mail |

### Content

| Field             | Type      | Required | Notes |
|------------------|-----------|----------|-------|
| `subject`        | string    | always   | Email subject, or `(no subject)` when absent |
| `date`           | string    | always   | Sender-claimed RFC 3339 timestamp |
| `received_at`    | string    | always   | Server-side RFC 3339 UTC timestamp when AIMX ingested the message |
| `received_from_ip`| string   | optional | SMTP client IP address. Omitted when unavailable (e.g. piped ingest) |
| `size_bytes`     | integer   | always   | Raw message size in bytes |
| `attachments`    | table[]   | optional | Array of attachment metadata tables. Omitted when empty |

Each entry in the `attachments` array:

| Field          | Type    | Notes |
|---------------|---------|-------|
| `filename`    | string  | Original filename |
| `content_type`| string  | MIME type (e.g. `application/pdf`) |
| `size`        | integer | Attachment size in bytes |
| `path`        | string  | Relative path within the bundle directory |

### Threading

| Field            | Type    | Required | Notes |
|-----------------|---------|----------|-------|
| `in_reply_to`   | string  | optional | Parent Message-ID. Omitted when not a reply |
| `references`    | string  | optional | Space-separated chain of Message-IDs for threading. Omitted when empty |
| `list_id`       | string  | optional | `List-Id` header value. Identifies mailing list mail. Omitted when absent |
| `auto_submitted`| string  | optional | `Auto-Submitted` header value (`auto-generated`, `auto-replied`, etc.). Omitted when absent. Check this before replying to avoid infinite loops |

### Auth

All auth fields are always written. They are never omitted or null so
"not evaluated" cannot be confused with "absent."

| Field     | Type   | Required | Values | Notes |
|----------|--------|----------|--------|-------|
| `dkim`   | string | always   | `"pass"`, `"fail"`, `"none"` | DKIM signature verification result |
| `spf`    | string | always   | `"pass"`, `"fail"`, `"softfail"`, `"neutral"`, `"none"` | SPF record verification result |
| `dmarc`  | string | always   | `"pass"`, `"fail"`, `"none"` | DMARC alignment check result |
| `trusted`| string | always   | `"none"`, `"true"`, `"false"` | Effective trust evaluation outcome (per-mailbox override if set, else top-level default) |

#### `trusted` field details

The `trusted` field surfaces the effective trust evaluation for the email's
mailbox. The effective policy is the mailbox's own `trust` /
`trusted_senders` if set, otherwise the top-level defaults in
`config.toml`:

- **`"none"`** — effective `trust` is `"none"` (the default). No trust
  evaluation was performed.
- **`"true"`** — effective `trust` is `"verified"`, the sender matches
  one of the effective `trusted_senders` glob patterns, AND DKIM passed.
  This is equivalent to "the email passed the trigger gate."
- **`"false"`** — effective `trust` is `"verified"`, but one or both
  conditions were not met: the sender was not in the allowlist, or DKIM did
  not pass (or both).

Trust config in `config.toml` — global defaults at the top, optional
per-mailbox overrides:
```toml
# Global defaults applied to every mailbox unless overridden
trust = "verified"
trusted_senders = ["*@company.com"]

[mailboxes.support]
address = "support@domain.com"
# Per-mailbox override fully replaces the global list (no merging)
trusted_senders = ["*@company.com", "alice@gmail.com"]
```

### Storage

| Field    | Type     | Required | Notes |
|---------|----------|----------|-------|
| `mailbox`| string  | always   | Name of the mailbox this email was routed to |
| `read`   | bool    | always   | `false` on ingest. Updated by `email_mark_read`/`email_mark_unread` |
| `labels` | string[]| optional | Empty by default. Omitted when empty |

---

## Outbound frontmatter

Sent copies under `sent/<mailbox>/` use the same field ordering as inbound
(Identity → Parties → Content → Threading → Auth → Storage) plus an
additional Outbound block at the end.

### Outbound block

| Field              | Type     | Required | Notes |
|-------------------|----------|----------|-------|
| `outbound`        | bool     | always   | Always `true` for sent copies |
| `delivery_status` | string   | always   | `"delivered"`, `"deferred"`, `"failed"`, or `"pending"` |
| `bcc`             | string[] | optional | BCC recipients. Only present on sent copies. Omitted when empty |
| `delivered_at`    | string   | optional | RFC 3339 UTC timestamp when remote MX accepted the message |
| `delivery_details`| string   | optional | Last remote SMTP response (e.g. `250 OK`) |

### Field-omission rule

Prefer omitting empty optional fields over writing `null`. Exceptions that
are always written (so "not evaluated" cannot be confused with "absent"):
`dkim`, `spf`, `dmarc`, `trusted`, `read`, `delivery_status`.

---

## Example: inbound email

```toml
+++
id = "2026-04-15-143022-meeting-notes"
message_id = "abc123@example.com"
thread_id = "a1b2c3d4e5f6a7b8"
from = "Alice <alice@company.com>"
to = "agent@yourdomain.com"
delivered_to = "agent@yourdomain.com"
subject = "Meeting Notes"
date = "2026-04-15T14:30:22Z"
received_at = "2026-04-15T14:30:23Z"
received_from_ip = "203.0.113.42"
size_bytes = 2048
dkim = "pass"
spf = "pass"
dmarc = "pass"
trusted = "true"
mailbox = "agent"
read = false
+++

Hi, here are the notes from today's meeting...
```

## Example: sent email

```toml
+++
id = "2026-04-15-160145-re-meeting-notes"
message_id = "def456@yourdomain.com"
thread_id = "a1b2c3d4e5f6a7b8"
from = "agent@yourdomain.com"
to = "alice@company.com"
delivered_to = "alice@company.com"
subject = "Re: Meeting Notes"
date = "2026-04-15T16:01:45Z"
received_at = ""
size_bytes = 1024
dkim = "pass"
spf = "none"
dmarc = "none"
trusted = "none"
mailbox = "agent"
read = false
outbound = true
delivery_status = "delivered"
delivered_at = "2026-04-15T16:01:47Z"
delivery_details = "250 OK"
+++

DKIM-Signature: ...
From: agent@yourdomain.com
To: alice@company.com
...
```
