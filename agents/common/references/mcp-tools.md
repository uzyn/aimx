# aimx MCP tools: full reference

All tools are served by the `aimx` binary over stdio (MCP transport). They
return strings on success and error strings on failure. The MCP server is
launched on-demand by your MCP client. It is not a long-running process.

## Per-user visibility

The MCP server inherits the uid of the user that launched the client.
Mutations that touch a mailbox consult the daemon over the world-
writable UDS; the daemon reads `SO_PEERCRED` from the socket and
rejects any request whose caller uid does not own the target mailbox
(root is the only bypass). Reads are scoped by filesystem permissions
(`/var/lib/aimx/inbox/<mailbox>/` is `0700 <owner>:<owner>`).

Effectively:

- `mailbox_list` returns only mailboxes whose `owner` equals the
  caller's username.
- `email_list`, `email_read`, `email_send`, `email_reply`,
  `email_mark_read`, `email_mark_unread` reject for any mailbox the
  caller does not own.
- `hook_create` / `hook_list` / `hook_delete` operate only on
  mailboxes the caller owns. Hooks always execute as the mailbox's
  owning Linux user — there is no per-hook `run_as` field.

Mailbox provisioning (`mailbox_create` / `mailbox_delete`) is
**owner-gated**: the daemon synthesizes the new mailbox's owner from
`SO_PEERCRED` (your uid), so an agent can only create or remove
mailboxes owned by itself. Cross-uid creates (a mailbox owned by a
different Linux user) remain operator-only via the host CLI's
`sudo aimx mailboxes create <name> --owner <other-user>` flag.

On a single-user box the caller always owns everything and the rules
are invisible. On a multi-user box they give real isolation.

## Mailbox tools

### `mailbox_list`

List mailboxes you own with absolute filesystem paths and message
counts.

**Parameters:** None.

**Returns:** JSON array. One row per visible mailbox with these
fields:

| Field         | Type            | Description                                                                  |
|---------------|-----------------|------------------------------------------------------------------------------|
| `name`        | string          | Mailbox name (the local part).                                               |
| `address`     | string \| null  | Full address `<name>@<domain>` for registered mailboxes; `null` for unregistered stray on-disk dirs. **The substring after `@` is the daemon's primary domain — there is no other API for this.** |
| `inbox_path`  | string          | Absolute path to the inbox directory (`/var/lib/aimx/inbox/<name>`).         |
| `sent_path`   | string          | Absolute path to the sent directory (`/var/lib/aimx/sent/<name>`).           |
| `total`       | number          | Total emails in the inbox.                                                   |
| `unread`      | number          | Number of inbox emails with `read = false` in the frontmatter.               |
| `sent_count`  | number          | Total emails in the sent folder.                                             |
| `registered`  | bool            | `true` for mailboxes in `config.toml`; `false` for stray on-disk dirs only.  |

The empty case returns `[]` (a JSON empty array), never a "no
mailboxes" string. Mailboxes you do not own are simply absent from
the array. To create or delete mailboxes you own, use
`mailbox_create` / `mailbox_delete` (below). To create a mailbox
owned by a different Linux user, ask the operator to run
`sudo aimx mailboxes create <name> --owner <user>` on the host.

**Example:**
```
mailbox_list()
→ [
    {
      "name": "agent",
      "address": "agent@your.tld",
      "inbox_path": "/var/lib/aimx/inbox/agent",
      "sent_path": "/var/lib/aimx/sent/agent",
      "total": 12,
      "unread": 3,
      "sent_count": 5,
      "registered": true
    }
  ]
```

**Next step:** read messages directly from disk. Take an `id` from
`email_list(mailbox: "agent")` (or list the inbox dir) and `Read`
`<inbox_path>/<id>.md` — no need for `email_read` unless you need
the daemon to enforce path canonicalisation for you.

---

### `mailbox_create`

Provision a new mailbox owned by your Linux uid. The daemon resolves
the owner from `SO_PEERCRED` over the UDS socket — there is no `owner`
parameter, so by construction an agent cannot create mailboxes owned
by another user. Validation (regex, reserved-names list) and
idempotent semantics live on the daemon side; the tool surfaces
daemon errors verbatim.

**Parameters:**
| Name   | Type   | Required | Description |
|--------|--------|----------|-------------|
| `name` | string | yes      | Mailbox name (the local part). Must match `[a-z0-9._-]+` with no leading/trailing dot and no `..`. The reserved names `catchall` and `aimx-catchall` are rejected by the daemon. |

**Returns:** the new mailbox's full address string (`<name>@<domain>`)
on success; an error string on failure (e.g. `[VALIDATION] Mailbox
name 'foo!' contains invalid character ...`).

**Idempotency.** Re-running `mailbox_create` on a mailbox you already
own succeeds without modification (matches the existing CLI behavior
of `aimx mailboxes create`). A name collision with a mailbox owned by
another user surfaces as a daemon-side validation error.

**Example:**
```
mailbox_create(name: "task-42")
→ "task-42@your.tld"
```

**Next step:** call `email_send(from_mailbox: "task-42", ...)` or
attach a hook with `hook_create(mailbox: "task-42", ...)`. The
mailbox is registered in `config.toml` and the daemon's in-memory
routing table is hot-swapped — inbound mail to
`task-42@your.tld` lands immediately.

---

### `mailbox_delete`

Tear down a mailbox you own. The daemon enforces ownership via
`SO_PEERCRED`; an attempt to delete a mailbox owned by another uid
surfaces as a not-authorized error. With `force: true`, the tool first
wipes `inbox/<name>/` and `sent/<name>/` before submitting the delete
(mirroring the CLI's `--force` flag); without `force`, the daemon
refuses to delete any mailbox whose storage directories aren't empty.

**Parameters:**
| Name    | Type   | Required | Description |
|---------|--------|----------|-------------|
| `name`  | string | yes      | Mailbox name (must be owned by you). The catchall mailbox cannot be deleted. |
| `force` | bool   | no       | Default `false`. When `true`, wipe `inbox/<name>/` and `sent/<name>/` contents before submitting the delete; when `false`, the daemon refuses non-empty mailboxes with a `[NONEMPTY]` error. |

**Returns:** a one-line success message, or an error string on
failure (`[EACCES]` for not-owner, `[NONEMPTY]` for non-empty without
`force`, `[ENOENT]` for an unknown mailbox).

**Example, clean up after a transient task:**
```
mailbox_delete(name: "task-42", force: true)
→ "Mailbox 'task-42' deleted."
```

**Example, refuse to nuke a mailbox you don't own:**
```
mailbox_delete(name: "alice", force: true)
→ "not authorized: mailbox 'alice' not found"
```

(The error never distinguishes "exists but you don't own it" from
"doesn't exist" — NFR2 forbids that information leak.)

---

## Email tools

### `email_list`

List a page of emails in a mailbox you own. Rows are sorted descending
by filename (newest first). aimx never scans on your behalf — pass
`offset` to page deeper, and filter client-side on the JSON output.

**Parameters:**
| Name      | Type   | Required | Description |
|-----------|--------|----------|-------------|
| `mailbox` | string | yes      | Mailbox name (must be owned by you) |
| `folder`  | string | no       | `"inbox"` (default) or `"sent"` |
| `limit`   | u32    | no       | Page size; default 50, hard-capped at 200 (values above silently clamp) |
| `offset`  | u32    | no       | Number of newest rows to skip; default 0 |

**Returns:** A JSON array. Inbox rows: `{ id, from, to, subject, date, read }`.
Sent rows: `{ id, from, to, subject, date, delivery_status }` — `read`
is intentionally absent from sent rows. Empty mailbox → `"[]"`.

**Example, list newest inbox page (default 50):**
```
email_list(mailbox: "agent")
→ '[{"id":"2026-04-15-153300-invoice-march","from":"billing@vendor.com","to":"agent@your.tld","subject":"Invoice March","date":"2026-04-15T15:33:00Z","read":false},
    {"id":"2026-04-15-143022-meeting-notes","from":"alice@company.com","to":"agent@your.tld","subject":"Meeting Notes","date":"2026-04-15T14:30:22Z","read":true}]'
```

**Next step:** read messages directly. Take the row's `id` and `Read`
`<inbox_path>/<id>.md` (the `inbox_path` you got from `mailbox_list`).
No need to call `email_read` unless you want the daemon to re-canonicalise
the path for you.

**Example, polling for unread:**
```
# List a page, then filter client-side.
rows = JSON.parse(email_list(mailbox: "agent"))
unread = rows.filter(r => r.read === false)
# For each unread row, Read its .md or call email_read, then mark it read.
```

**Example, paging deeper:**
```
email_list(mailbox: "agent", limit: 50, offset: 50)
# Newest 50 already seen; this returns rows 51..100.
```

**Example, list sent mail:**
```
email_list(mailbox: "agent", folder: "sent")
→ '[{"id":"2026-04-15-160145-re-meeting-notes","from":"agent@your.tld","to":"alice@company.com","subject":"Re: Meeting Notes","date":"2026-04-15T16:01:45Z","delivery_status":"delivered"}]'
```

---

### `email_read`

Return the full Markdown file (TOML frontmatter + body) for a single email.

**Parameters:**
| Name      | Type   | Required | Description |
|-----------|--------|----------|-------------|
| `mailbox` | string | yes      | Mailbox name (must be owned by you) |
| `id`      | string | yes      | Email ID. The filename stem (e.g. `2026-04-15-143022-meeting-notes`) |
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
- Mailbox not owned by caller.

---

### `email_send`

Compose, DKIM-sign, and deliver an email. The message is submitted to
`aimx serve` via UDS, which signs it with the server's DKIM key and delivers
directly to the recipient's MX server.

`body` is interpreted as **Markdown by default** (CommonMark + GFM
extensions: tables, strikethrough, autolinks, task lists, footnotes).
AIMX renders the Markdown to HTML with an inlined stylesheet and ships
a `multipart/alternative` — recipients on rich-capable clients see
styled HTML; recipients on text-only clients see the Markdown source.

Two escape hatches cover the edge cases:

- `text_only: true` — ship `body` verbatim as `text/plain`. No
  rendering. Use for OTPs, transactional one-liners, and existing
  scripts that must not change shape.
- `html_body: "<custom html>"` — supply a custom HTML template that
  AIMX uses verbatim as the `text/html` part. `body` is the
  `text/plain` fallback. Mutually exclusive with `text_only`.

**Parameters:**
| Name           | Type     | Required | Description |
|---------------|----------|----------|-------------|
| `from_mailbox` | string   | yes      | Mailbox name to send from (local part only); must be owned by you |
| `to`           | string   | yes      | Recipient email address |
| `subject`      | string   | yes      | Email subject |
| `reply_to`     | string   | no       | Message-ID of the email being replied to. Sets `In-Reply-To`. When `references` is omitted, `References` is built automatically from this value. Required to enable threading. Without `reply_to`, any `references` value is silently ignored and no threading headers are emitted |
| `body`         | string   | yes      | Email body. Interpreted as Markdown by default (CommonMark + GFM). Used verbatim as `text/plain` when `text_only: true`; used as the `text/plain` fallback when `html_body` is set |
| `attachments`  | string[] | no       | Absolute file paths to attach |
| `references`   | string   | no       | Full `References` header chain (space-separated Message-IDs). **Only applied when `reply_to` is also set.** Supplied alone, it is silently ignored |
| `text_only`    | boolean  | no       | When `true`, ship `body` verbatim as `text/plain` with no Markdown rendering and no HTML alternative part. Mutually exclusive with `html_body` |
| `html_body`    | string   | no       | Custom HTML for the `text/html` part. Operator-supplied; bypasses the renderer. Mutually exclusive with `text_only` |

For simple replies to a single sender, prefer `email_reply`. It reads the
original email and fills in threading headers and the `Re:` subject
automatically. Use `email_send` with `reply_to` / `references` only when
you need to override the recipient list (e.g. reply-all) or build a
custom threading chain.

**Returns:** Confirmation with Message-ID.

**Example, default Markdown:**
```
# `from_mailbox` is the local part only. The daemon derives the
# full From address from mailbox_list().address — you do not
# need to know the configured domain.
email_send(
  from_mailbox: "agent",
  to: "alice@example.com",
  subject: "Status Update",
  body: "# Status\n\n- All systems operational.\n- See [dashboard](https://example.com/dash) for details."
)
→ "Sent. Message-ID: <abc123@domain.com>"
```

**Example, plain-text only (OTP):**
```
email_send(
  from_mailbox: "agent",
  to: "alice@example.com",
  subject: "Verification code",
  body: "Your code: 184293",
  text_only: true
)
```

**Example, custom HTML layout:**
```
email_send(
  from_mailbox: "agent",
  to: "bob@example.com",
  subject: "Newsletter",
  body: "Plain-text fallback for text-only clients.",
  html_body: "<!DOCTYPE html><html><body><h1>Newsletter</h1>...</body></html>"
)
```

**Example, with attachments:**
```
email_send(
  from_mailbox: "agent",
  to: "bob@example.com",
  subject: "Report",
  body: "# Report\n\nSee the attached files.",
  attachments: ["/tmp/report.csv", "/tmp/chart.png"]
)
```

**Example, threaded reply-all:**
```
# First, read the original to get recipients and the Message-ID:
email_read(mailbox: "agent", id: "2025-06-01-001")
# Frontmatter yields: message_id = "<abc@example.com>",
#                     references = "<prev@example.com>",
#                     from / to / cc.

email_send(
  from_mailbox: "agent",
  to: "alice@example.com, bob@example.com, carol@example.com",
  subject: "Re: Status Update",
  reply_to: "<abc@example.com>",
  body: "Looping everyone in.",
  references: "<prev@example.com> <abc@example.com>"
)
```

**Errors:**
- Mailbox does not exist or is not owned by you.
- Daemon not running (socket missing).
- Sender domain mismatch.
- Delivery failure (remote MX rejected).
- `text_only` and `html_body` both set: `AIMX/1 SEND: --text-only and --html-body are mutually exclusive`.
- Markdown body exceeds 5 MiB: `markdown body exceeds 5 MiB; use --html-body for pre-rendered large documents or --attachment for sending the document as a file`.

---

### `email_reply`

Reply to an existing email. AIMX automatically sets `In-Reply-To`,
`References`, and prepends `Re:` to the subject.

`body` is interpreted as **Markdown by default** with the same
`text_only` / `html_body` escape hatches as `email_send`.

**Parameters:**
| Name        | Type    | Required | Description |
|-------------|---------|----------|-------------|
| `mailbox`   | string  | yes      | Mailbox containing the email to reply to (must be owned by you) |
| `id`        | string  | yes      | Email ID to reply to |
| `body`      | string  | yes      | Reply body. Interpreted as Markdown by default. With `text_only: true`, shipped verbatim as `text/plain`. With `html_body`, used as the `text/plain` fallback |
| `text_only` | boolean | no       | When `true`, ship `body` verbatim as `text/plain` with no Markdown rendering. Mutually exclusive with `html_body` |
| `html_body` | string  | no       | Custom HTML for the `text/html` part. Operator-supplied; bypasses the renderer. Mutually exclusive with `text_only` |

**Returns:** Confirmation with Message-ID.

**Example, default Markdown reply:**
```
email_reply(
  mailbox: "agent",
  id: "2026-04-15-143022-meeting-notes",
  body: "Thanks — I'll review the notes.\n\n- Quick follow-up: meet again Friday?"
)
→ "Sent. Message-ID: <def456@domain.com>"
```

**Example, plain-text reply:**
```
email_reply(
  mailbox: "agent",
  id: "2026-04-15-143022-meeting-notes",
  body: "Acknowledged.",
  text_only: true
)
```

---

### `email_mark_read`

Mark a single inbox email as read (sets `read = true` in
frontmatter). Sent-mail mark has no agent use case and is not
supported.

**Parameters:**
| Name      | Type   | Required | Description |
|-----------|--------|----------|-------------|
| `mailbox` | string | yes      | Mailbox name (must be owned by you) |
| `id`      | string | yes      | Email ID |

**Returns:** Confirmation string.

**Example:**
```
email_mark_read(mailbox: "agent", id: "2026-04-15-143022-meeting-notes")
→ "Marked as read."
```

---

### `email_mark_unread`

Mark a single inbox email as unread (sets `read = false` in
frontmatter). Sent-mail mark has no agent use case and is not
supported.

**Parameters:**
| Name      | Type   | Required | Description |
|-----------|--------|----------|-------------|
| `mailbox` | string | yes      | Mailbox name (must be owned by you) |
| `id`      | string | yes      | Email ID |

**Returns:** Confirmation string.

**Example:**
```
email_mark_unread(mailbox: "agent", id: "2026-04-15-143022-meeting-notes")
→ "Marked as unread."
```

## Hook tools

Hooks fire commands on mail events. You create hooks on mailboxes you
own; the hook always executes as the mailbox's owning Linux user. The
raw `.md` (frontmatter + body) is always piped on stdin and the same
path is exposed as `$AIMX_FILEPATH`. If your hook only needs the
subject or sender, read `$AIMX_SUBJECT` / `$AIMX_FROM` and ignore
stdin — the daemon writes the full email but does not require the
child to consume it. There is no template indirection — your `cmd` is
the literal argv that runs. See `references/hooks.md` for the full
model, worked examples, and troubleshooting.

### `hook_create`

Attach a hook to a mailbox you own.

**Parameters:**
| Name                | Type     | Required | Description |
|---------------------|----------|----------|-------------|
| `mailbox`           | string   | yes      | Mailbox name (must be owned by you) |
| `event`             | string   | yes      | `"on_receive"` or `"after_send"` |
| `cmd`               | string[] | yes      | argv array. `cmd[0]` must be an absolute path the owning user can execute |
| `name`              | string   | no       | Optional explicit name; derived from `(event, cmd, fire_on_untrusted)` when omitted |
| `timeout_secs`      | u32      | no       | Per-fire timeout in seconds. Default 60, max 600 |
| `fire_on_untrusted` | bool     | no       | Default `false`. Legal only on `on_receive`; when `true`, fires regardless of `trusted` |

**Returns:** confirmation containing the effective name and the argv
that will run.

**Example:**
```
hook_create(
  mailbox: "support",
  event: "on_receive",
  cmd: ["/usr/local/bin/claude", "-p", "You are the support agent.", "--dangerously-skip-permissions"]
)
→ "Hook 'support-replier' created on mailbox 'support'. argv=['/usr/local/bin/claude', '-p', 'You are the support agent.', '--dangerously-skip-permissions']"
```

**Errors:**
- Not authorized (mailbox not owned by you).
- Mailbox not found.
- `cmd[0]` not an absolute path.
- `fire_on_untrusted` set on an `after_send` hook.
- Name conflict with an existing hook.
- Daemon not running.

---

### `hook_list`

List hooks on mailboxes you own (or one when `mailbox` is set). Backed by the `HOOK-LIST` UDS verb — the daemon filters to hooks on mailboxes the caller's uid owns server-side, so the MCP process never reads `/etc/aimx/config.toml` directly.

**Parameters:**
| Name      | Type   | Required | Description |
|-----------|--------|----------|-------------|
| `mailbox` | string | no       | Filter to one mailbox (must be owned by you) |

**Returns:** JSON array. See `references/hooks.md` for the row shape.

**Example:**
```
hook_list()
→ [{"name":"support-replier","mailbox":"support","event":"on_receive",
    "cmd":["/usr/local/bin/claude","-p","...","--dangerously-skip-permissions"],
    "timeout_secs":60,"fire_on_untrusted":false}]
```

---

### `hook_delete`

Delete a hook by effective name on a mailbox you own.

**Parameters:**
| Name   | Type   | Required | Description |
|--------|--------|----------|-------------|
| `name` | string | yes      | Effective name from `hook_list` |

**Returns:** Confirmation string.

**Example:**
```
hook_delete(name: "support-replier")
→ "Hook 'support-replier' deleted."

hook_delete(name: "someone-elses-hook")
→ Error: "Hook 'someone-elses-hook' not found"
   (the daemon collapses "exists but unauthorized" into not-found so
   foreign mailbox names do not leak)
```
