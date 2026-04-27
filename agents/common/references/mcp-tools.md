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

Mailbox provisioning (`mailbox_create` / `mailbox_delete`) is **not
exposed via MCP**. It is root-only on the host CLI (`sudo aimx
mailboxes create | delete`) so that the namespace of mailboxes can
never be widened by an agent.

On a single-user box the caller always owns everything and the rules
are invisible. On a multi-user box they give real isolation.

## Mailbox tools

### `mailbox_list`

List mailboxes you own with absolute filesystem paths and message
counts.

**Parameters:** None.

**Returns:** JSON array. One row per visible mailbox with these
fields:

| Field         | Type   | Description                                                                  |
|---------------|--------|------------------------------------------------------------------------------|
| `name`        | string | Mailbox name (the local part).                                               |
| `inbox_path`  | string | Absolute path to the inbox directory (`/var/lib/aimx/inbox/<name>`).         |
| `sent_path`   | string | Absolute path to the sent directory (`/var/lib/aimx/sent/<name>`).           |
| `total`       | number | Total emails in the inbox.                                                   |
| `unread`      | number | Number of inbox emails with `read = false` in the frontmatter.               |
| `sent_count`  | number | Total emails in the sent folder.                                             |
| `registered`  | bool   | `true` for mailboxes in `config.toml`; `false` for stray on-disk dirs only.  |

The empty case returns `[]` (a JSON empty array), never a "no
mailboxes" string. Mailboxes you do not own are simply absent from
the array. To create or delete mailboxes, ask the operator to run
`sudo aimx mailboxes create <name> --owner <user>` or
`sudo aimx mailboxes delete <name>` on the host.

**Example:**
```
mailbox_list()
→ [
    {
      "name": "agent",
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

## Email tools

### `email_list`

List emails in a mailbox you own with optional filters. All filters
AND together.

**Parameters:**
| Name      | Type    | Required | Description |
|-----------|---------|----------|-------------|
| `mailbox` | string  | yes      | Mailbox name (must be owned by you) |
| `folder`  | string  | no       | `"inbox"` (default) or `"sent"` |
| `unread`  | bool    | no       | Filter to only unread emails |
| `from`    | string  | no       | Filter by sender address (substring match) |
| `since`   | string  | no       | Filter to emails since this datetime (RFC 3339, e.g. `2026-01-01T00:00:00Z`) |
| `subject` | string  | no       | Filter by subject (case-insensitive substring match) |

**Returns:** Formatted list of matching emails.

**Example, list unread inbox:**
```
email_list(mailbox: "agent", unread: true)
→ "2026-04-15-143022-meeting-notes | From: alice@company.com | Subject: Meeting Notes | 2026-04-15T14:30:22Z
   2026-04-15-153300-invoice-march | From: billing@vendor.com | Subject: Invoice March | 2026-04-15T15:33:00Z"
```

**Example, list sent mail:**
```
email_list(mailbox: "agent", folder: "sent")
→ "2026-04-15-160145-re-meeting-notes | To: alice@company.com | Subject: Re: Meeting Notes | 2026-04-15T16:01:45Z"
```

**Example, filter by sender since a date:**
```
email_list(mailbox: "agent", from: "alice", since: "2026-04-01T00:00:00Z")
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

**Parameters:**
| Name           | Type     | Required | Description |
|---------------|----------|----------|-------------|
| `from_mailbox` | string   | yes      | Mailbox name to send from (local part only); must be owned by you |
| `to`           | string   | yes      | Recipient email address |
| `subject`      | string   | yes      | Email subject |
| `body`         | string   | yes      | Email body text |
| `attachments`  | string[] | no       | Absolute file paths to attach |
| `reply_to`     | string   | no       | Message-ID of the email being replied to. Sets `In-Reply-To`. When `references` is omitted, `References` is built automatically from this value. Required to enable threading. Without `reply_to`, any `references` value is silently ignored and no threading headers are emitted |
| `references`   | string   | no       | Full `References` header chain (space-separated Message-IDs). **Only applied when `reply_to` is also set.** Supplied alone, it is silently ignored |

For simple replies to a single sender, prefer `email_reply`. It reads the
original email and fills in threading headers and the `Re:` subject
automatically. Use `email_send` with `reply_to` / `references` only when
you need to override the recipient list (e.g. reply-all) or build a
custom threading chain.

**Returns:** Confirmation with Message-ID.

**Example, plain text:**
```
email_send(
  from_mailbox: "agent",
  to: "alice@example.com",
  subject: "Status Update",
  body: "All systems operational."
)
→ "Sent. Message-ID: <abc123@domain.com>"
```

**Example, with attachments:**
```
email_send(
  from_mailbox: "agent",
  to: "bob@example.com",
  subject: "Report",
  body: "Please see attached.",
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
  body: "Looping everyone in.",
  reply_to: "<abc@example.com>",
  references: "<prev@example.com> <abc@example.com>"
)
```

**Errors:**
- Mailbox does not exist or is not owned by you.
- Daemon not running (socket missing).
- Sender domain mismatch.
- Delivery failure (remote MX rejected).

---

### `email_reply`

Reply to an existing email. aimx automatically sets `In-Reply-To`,
`References`, and prepends `Re:` to the subject.

**Parameters:**
| Name      | Type   | Required | Description |
|-----------|--------|----------|-------------|
| `mailbox` | string | yes      | Mailbox containing the email to reply to (must be owned by you) |
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
| `mailbox` | string | yes      | Mailbox name (must be owned by you) |
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
| `mailbox` | string | yes      | Mailbox name (must be owned by you) |
| `id`      | string | yes      | Email ID |
| `folder`  | string | no       | `"inbox"` (default) or `"sent"` |

**Returns:** Confirmation string.

**Example:**
```
email_mark_unread(mailbox: "agent", id: "2026-04-15-143022-meeting-notes")
→ "Marked as unread."
```

## Hook tools

Hooks fire commands on mail events. You create hooks on mailboxes you
own; the hook always executes as the mailbox's owning Linux user, with
the email piped on stdin (or accessible via `$AIMX_FILEPATH` when
stdin is closed). There is no template indirection — your `cmd` is
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
| `stdin`             | string   | no       | `"email"` (default; pipes raw `.md`) or `"none"` (closes stdin; child reads `$AIMX_FILEPATH` instead) |
| `timeout_secs`      | u32      | no       | Per-fire timeout in seconds. Default 60, max 600 |
| `fire_on_untrusted` | bool     | no       | Default `false`. Legal only on `on_receive`; when `true`, fires regardless of `trusted` |

**Returns:** confirmation containing the effective name and the argv
that will run.

**Example:**
```
hook_create(
  mailbox: "support",
  event: "on_receive",
  cmd: ["/usr/local/bin/claude", "-p", "You are the support agent.", "--dangerously-skip-permissions"],
  stdin: "email"
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

List hooks on mailboxes you own (or one when `mailbox` is set).

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
    "stdin":"email","timeout_secs":60,"fire_on_untrusted":false}]
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
