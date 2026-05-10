# Mailboxes & Email

A mailbox maps an email address to a directory on disk. `aimx mailbox` is a clap alias for `aimx mailboxes` and works identically.

## Concepts

- **Mailboxes are directories.** Creating a mailbox creates two folders (one under `inbox/`, one under `sent/`) and registers an address. No passwords, no database.
- **Per-mailbox owner.** Every mailbox has a single Linux `owner` in `config.toml`. Storage is chowned `<owner>:<owner> 0700` at create and kept consistent through every write. Only the owner and root can read it; the MCP server and UDS both authorize on `SO_PEERCRED` matching the owner uid. See [Security: Per-action authorization](security.md#per-action-authorization).
- **Catchall.** The `catchall` mailbox catches mail for unrecognized addresses at your domain. It is inbound-only (no `sent/catchall/`), owned by the reserved `aimx-catchall` system user.
- **No sudo for the mailboxes you own.** `aimx mailboxes create / delete` route through the daemon's UDS, so the daemon synthesizes the owner from `SO_PEERCRED` and atomically rewrites `config.toml`. Root may still pass `--owner <user>` to provision a mailbox for another uid.
- **Hot-reload.** When `aimx serve` is running, create and delete take effect on the next SMTP session — no restart needed.
- **Delete is file-safe.** Non-empty mailboxes are refused with `ERR NONEMPTY` and a file count. Archive or remove the files first. The directories are left on disk after delete so an operator can `rmdir` them at leisure.
- **Force-delete is CLI-only.** `aimx mailboxes delete --force <name>` recursively wipes `inbox/<name>/` and `sent/<name>/` first. It always prompts unless `--yes` is passed. The MCP `mailbox_delete` tool deliberately has no force variant — destructive wipes stay where the operator sees prompts. `catchall` is refused with or without `--force`.

### On-disk layout

```text
/var/lib/aimx/
├── inbox/              # inbound mail lives here
│   ├── catchall/
│   └── support/
└── sent/               # outbound sent copies
    └── support/
```

Each email is stored as either a flat `YYYY-MM-DD-HHMMSS-<slug>.md` file
when it has zero attachments, or as a bundle directory
`YYYY-MM-DD-HHMMSS-<slug>/` containing `<stem>.md` plus every attachment
as a sibling file when attachments are present.

### Routing logic

When an email arrives, AIMX matches the local part of the recipient address (the part before `@`) against mailbox names in the config. If a mailbox with that exact name exists, the email is delivered there. Otherwise it falls through to the `catchall` mailbox.

RCPT TO addresses whose domain is not the configured `domain` (case-insensitive exact match) are rejected at SMTP time with `550 5.7.1 relay not permitted` and never reach storage. AIMX is not an open relay: `catchall` only covers unrecognized local parts *at your configured domain*, not unrelated domains or subdomains.

For example, with mailboxes `support` and `catchall` configured:
- `support@agent.yourdomain.com` -> delivered to the `support` mailbox
- `billing@agent.yourdomain.com` -> delivered to the `catchall` mailbox (no `billing` mailbox exists)
- `anything@agent.yourdomain.com` -> delivered to the `catchall` mailbox
- `anything@some-other-domain.com` -> rejected at RCPT TO with `550 5.7.1 relay not permitted`
- `anything@sub.agent.yourdomain.com` -> rejected at RCPT TO with `550 5.7.1 relay not permitted`

## Managing mailboxes

### Create a mailbox

```bash
# As yourself: create a mailbox owned by your own uid.
aimx mailboxes create support
```

This creates `support@agent.yourdomain.com` and both directories:
`/var/lib/aimx/inbox/support/` (for incoming mail) and
`/var/lib/aimx/sent/support/` (for outbound copies). Storage is chowned to
your uid at mode `0700`. Deletion removes both; `catchall` cannot be
deleted.

**Owner-binding rule.** Non-root callers create and delete only mailboxes they own — the daemon synthesizes the owner from `SO_PEERCRED` and ignores any client-supplied owner. Root passes unconditionally and may use `--owner <user>` to provision a mailbox owned by another Linux user. Passing `--owner <other>` from a non-root shell prints a soft warning to stderr and submits the request with the synthesized owner anyway.

**Cross-uid create (root only).** Provision a shared mailbox owned by a service account:

```bash
# create the Linux user first
sudo useradd --system --shell /usr/sbin/nologin support-agent

# operator creates the mailbox owned by that user (cross-uid → sudo)
sudo aimx mailboxes create support --owner support-agent

# verify ownership landed where expected
ls -la /var/lib/aimx/inbox/support/    # drwx------  support-agent support-agent
ls -la /var/lib/aimx/sent/support/     # drwx------  support-agent support-agent
```

Any agent running under uid `support-agent` can now read `/var/lib/aimx/inbox/support/` and use the MCP tools against the `support` mailbox. Other users cannot read it — isolation is filesystem-enforced.

For the day-to-day case, drop the `--owner` flag and skip `sudo`:

```bash
# As yourself, no sudo:
aimx mailboxes create agent-1
```

The daemon must be running for non-root mailbox CRUD; if it is stopped, the CLI exits with a precise error naming both remediations. See [Troubleshooting](troubleshooting.md#aimx-mailboxes-create--delete-exits-with-daemon-must-be-running-for-non-root-mailbox-crud).

### Agents can self-serve via MCP

Agents call [`mailbox_create`](mcp.md#mailbox_create) and [`mailbox_delete`](mcp.md#mailbox_delete) over MCP. Neither accepts an `owner` parameter — the daemon synthesizes the owner from the MCP process's uid via `SO_PEERCRED`. An agent provisions an inbox for a transient task, sends and receives on it, then calls `mailbox_delete("task-42", force: true)` when done. No operator intervention required.

### List mailboxes

```bash
aimx mailboxes list
```

Shows all mailboxes with their addresses and message counts (total and unread).

### Inspect a single mailbox

```bash
aimx mailboxes show support
```

Prints the mailbox's address, effective trust policy, full `trusted_senders` list, configured hooks grouped by event (`on_receive` / `after_send`. Each entry shows the hook id, `cmd` truncated to 60 chars with a `…` suffix when longer, filters in compact form, and the `dangerously_support_untrusted=true` flag where set), and inbox + sent + unread message counts. Example output:

```text
Mailbox: support
  Address: support@agent.yourdomain.com
  Trust:   verified
  Trusted senders:
    - *@company.com
    - boss@example.com

Hooks
  on_receive
    - aaaabbbbcccc  cmd: curl -fsS https://hooks.example.com/notify   [from=*@gmail.com subject=urgent]
  after_send
    - ddddeeeeffff  cmd: /usr/local/bin/notify "$AIMX_TO"             [to=*@client.com]

Messages
  inbox: 12 (3 unread)
  sent:  5
```

### Delete a mailbox

```bash
aimx mailboxes delete support
```

Prompts for confirmation. Use `--yes` to skip the prompt. When the daemon is
running, the request routes through its UDS socket and the daemon refuses
to delete a mailbox that still contains files (error `NONEMPTY`). Archive
or remove them first, then retry. When the daemon is stopped, delete goes
through the direct-edit fallback which removes the directory tree and
prints a restart-hint banner.

### Force-delete a non-empty mailbox

`--force` permanently removes every email under `inbox/<name>/` and `sent/<name>/` before unregistering the mailbox. There is no undo.

```bash
# Interactive: shows file counts and prompts before wiping
aimx mailboxes delete --force support

# Scripted: skip the confirmation prompt
aimx mailboxes delete --force --yes support
```

Without `--force`, a non-empty mailbox is refused with `ERR NONEMPTY`. `catchall` is refused even with `--force`. Force is CLI-only — the MCP `mailbox_delete` tool returns a hint pointing here on NONEMPTY rather than gaining its own force variant.

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

The format is agent-readable without a MIME parser: an agent can `cat` the file and act on it directly.

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
| `trusted` | string | Effective trust evaluation for the email's mailbox (per-mailbox override if set, otherwise the top-level default): `none`, `true`, or `false` |
| `mailbox` | string | Mailbox name this email was routed to |
| `read` | bool | Read status (`false` on ingest) |
| `read_at` | datetime | RFC 3339 UTC timestamp set when the email is marked read. Removed on mark-unread. Reflects the most recent read, not the first. Optional, omitted when absent |

### Outbound frontmatter

Emails under `sent/<mailbox>/` carry every inbound field plus an outbound block appended at the end:

| Field | Type | Always written | Description |
|-------|------|----------------|-------------|
| `outbound` | bool | yes | Always `true` on sent copies. Distinguishes outbound files from inbound. |
| `delivery_status` | string | yes | One of `"delivered"`, `"failed"`, `"deferred"`, `"pending"`. |
| `bcc` | array of strings | no | BCC recipients. Optional, omitted when empty. |
| `delivered_at` | string | no | RFC 3339 UTC timestamp of the successful MX handoff. Optional, present only when `delivery_status = "delivered"`. |
| `delivery_details` | string | no | SMTP reason string on permanent failure (e.g. `"550 no such user"`). Optional. |

Deferred (4xx) sends are not persisted. The submitting client is expected to retry. Permanent (5xx) failures are persisted with `delivery_status = "failed"` and the SMTP reason in `delivery_details`. On outbound files the inbound `received_at` and `received_from_ip` fields are omitted when empty.

### Body extraction

- **Text/plain** is preferred when available
- Falls back to **text/html** converted to plaintext (via `html2text`)
- Stored as Markdown content after the frontmatter

## Attachments

When an email carries one or more attachments, AIMX writes a bundle
directory whose name matches the `.md` file's stem:

```text
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
| `path` | Path relative to the bundle directory |

## Read/unread tracking

Emails are marked `read = false` on ingest. Use MCP tools or update the frontmatter directly:

- **MCP:** `email_mark_read` and `email_mark_unread` (see [MCP Server](mcp.md#email-tools))
- **CLI/filesystem:** Edit the `read` field in the `.md` file's frontmatter

The `email_list` MCP tool returns the `read` flag on every inbox row. Agents page through the listing and filter client-side to `read == false`; AIMX itself does not scan on the agent's behalf.

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

# Advanced: supply the full References header for deep threading
aimx send --from support@agent.yourdomain.com \
          --to recipient@gmail.com \
          --subject "Re: Hello" \
          --body "Reply body" \
          --reply-to "<parent@example.com>" \
          --references "<root@example.com> <parent@example.com>"
```

`--reply-to` sets the `In-Reply-To` header (single Message-ID). `--references` sets the `References` chain and is only needed for multi-step threads where `In-Reply-To` alone is not enough. Most users can omit it. For interactive agent use, prefer the `email_reply` MCP tool. It reads the original message and fills both headers automatically.

### Via MCP

Agents send email using the `email_send` and `email_reply` MCP tools. See [MCP Server](mcp.md#email-tools) for details.

### Send pipeline

1. `aimx send` composes an RFC 5322 message and submits it over `/run/aimx/aimx.sock`. The client does not read `config.toml`.
2. `aimx serve` parses `From:` from the body, verifies the domain matches `config.domain` and the local part resolves to a configured non-wildcard mailbox, DKIM-signs the message with RSA-SHA256, and delivers it directly to the recipient's MX over SMTP. The catchall (`*@domain`) is never accepted as an outbound sender.
3. `aimx send` exits as soon as the daemon returns a status. Signing, mailbox resolution, and delivery happen entirely in the daemon — the client does not need root, does not read the DKIM key, and does not read `config.toml`.

### Reply threading

Replies set `In-Reply-To` and `References` so the thread lands correctly in the recipient's mail client. Pass `--reply-to` with the original message's `Message-ID` value.

The `email_reply` MCP tool handles threading automatically by reading the original email and setting the headers.

## Email ID format

Each email's `id` field is the filename stem `YYYY-MM-DD-HHMMSS-<slug>` in UTC. The slug is derived from the subject: lowercase, non-alphanumeric runs collapsed to `-`, trimmed, capped at 20 characters, falling back to `no-subject` when empty. Two emails with the same subject in the same UTC second get `-2`, `-3`, … appended.
