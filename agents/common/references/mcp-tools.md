# aimx MCP tools: full reference

All tools are served by the `aimx` binary over stdio (MCP transport). They
return strings on success and error strings on failure. The MCP server is
launched on-demand by your MCP client. It is not a long-running process.

## Per-user visibility

The MCP server inherits the uid of the user that launched the client.
All tool calls that touch a mailbox consult the daemon over the world-
writable UDS; the daemon reads `SO_PEERCRED` from the socket and
rejects any request whose caller uid does not own the target mailbox
(root is the only bypass). Effectively:

- `mailbox_list` returns only mailboxes whose `owner` equals the
  caller's username (plus the catchall, which is owned by
  `aimx-catchall`).
- `email_list`, `email_read`, `email_send`, `email_reply`,
  `email_mark_read`, `email_mark_unread` reject `EACCES` for any
  mailbox the caller does not own.
- `hook_list_templates` returns templates whose `run_as` equals the
  caller's username, plus reserved templates (`run_as =
  "aimx-catchall"` or `"root"`).
- `hook_create` / `hook_delete` operate only on mailboxes the caller
  owns. The constraint `hook.run_as == mailbox.owner OR hook.run_as ==
  "root"` (exception: catchall allows `aimx-catchall`) is enforced at
  every write.

On a single-user box the caller always owns everything and the rules
are invisible. On a multi-user box they give real isolation.

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

Delete a mailbox. Removes the `[mailboxes.<name>]` stanza from
`config.toml` and hot-swaps the daemon's in-memory config. The
`inbox/<name>/` and `sent/<name>/` directories must be empty first. The
daemon refuses the delete otherwise. Empty directories are left on disk
for the operator to clean up (e.g. `rmdir`).

**Parameters:**
| Name   | Type   | Required | Description |
|--------|--------|----------|-------------|
| `name` | string | yes      | Mailbox name to delete |

**Returns:** Confirmation string. Mentions the empty directories that
remain on disk so the operator isn't surprised.

**Example:**
```
mailbox_delete(name: "old-project")
→ "Mailbox 'old-project' deleted. Empty `inbox/old-project/` and
   `sent/old-project/` directories remain on disk. Run `rmdir` to tidy
   up if desired."
```

**Errors:**
- Mailbox does not exist.
- Mailbox is non-empty (inbox or sent contains files). The error
  payload spells out the per-directory file counts and points at the
  CLI command that wipes-and-deletes:
  `Cannot delete mailbox 'foo'. inbox: 5 files, sent: 2 files. MCP
  `mailbox_delete` does not wipe mail. Run `sudo aimx mailboxes delete
  --force foo` on the host to wipe and remove.`
  MCP deliberately does **not** expose a force variant. Destructive
  wipes stay on the CLI where the operator sees the prompt and the
  request can't be triggered remotely by an agent.
- Attempt to delete the `catchall` mailbox.

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
| `mailbox` | string | yes      | Mailbox name |
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
- Mailbox does not exist.
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

## Hook tools

Hooks fire shell commands on mail events. Agents create hooks by
referencing pre-vetted **templates**; the template model makes it
impossible to submit arbitrary shell over the world-writable UDS. See
`references/hooks.md` for the full model, worked examples, and
troubleshooting.

### `hook_list_templates`

List hook templates visible to the caller. A template is visible when
its `run_as` equals the caller's username, or when `run_as` is a
reserved sentinel (`aimx-catchall` or `root`). Per-agent templates
follow the naming scheme `invoke-<agent>-<username>` and are created
by `aimx agents setup <agent>` (run without sudo by the owning user).

**Parameters:** None.

**Returns:** JSON array. Fields per entry: `name`, `description`,
`params` (string array of declared parameter names), `allowed_events`
(subset of `["on_receive", "after_send"]`).

**Example, visible to alice after she runs `aimx agents setup claude-code`:**
```
hook_list_templates()
→ [{"name":"invoke-claude-alice","description":"Pipe email into Claude with a prompt.",
    "params":["prompt"],"allowed_events":["on_receive","after_send"]},
   {"name":"webhook","description":"POST the email as JSON to a URL",
    "params":["url"],"allowed_events":["on_receive","after_send"]}]
```

---

### `hook_create`

Attach a template-bound hook to a mailbox. The daemon substitutes the
supplied params into the template's argv, stamps `origin = "mcp"` on
the resulting hook, and writes it to `config.toml`.

**Parameters:**
| Name       | Type              | Required | Description |
|------------|-------------------|----------|-------------|
| `mailbox`  | string            | yes      | Mailbox name (must exist) |
| `event`    | string            | yes      | `"on_receive"` or `"after_send"` |
| `template` | string            | yes      | Template name from `hook_list_templates` |
| `params`   | object(str→str)   | yes      | Must match template's declared `params` exactly |
| `name`     | string            | no       | Optional explicit name; derived from `(event, template, sorted params)` when omitted |

**Returns:** JSON `{effective_name, substituted_argv}`.

**Example:**
```
hook_create(
  mailbox: "agent",
  event: "on_receive",
  template: "invoke-claude-alice",
  params: {"prompt": "You are an assistant."}
)
→ {"effective_name":"a1b2c3d4e5f6",
   "substituted_argv":["/home/alice/.local/bin/claude","-p","You are an assistant."]}
```

**Errors:**
- Unknown template.
- Unknown or missing param.
- Event not allowed by the template.
- Mailbox not found.
- Param validation (NUL / control chars / >8 KiB value).
- Daemon not running.

---

### `hook_list`

List hooks visible to MCP across all mailboxes (or one when `mailbox`
is set). Operator-origin hooks are **masked** to `{name, mailbox,
event, origin}`; MCP-origin hooks include `template` and `params`.

**Parameters:**
| Name      | Type   | Required | Description |
|-----------|--------|----------|-------------|
| `mailbox` | string | no       | Filter to one mailbox |

**Returns:** JSON array. See `references/hooks.md` for the exact
shape of each origin variant.

**Example:**
```
hook_list()
→ [{"name":"daily-report","mailbox":"agent","event":"after_send","origin":"operator"},
   {"name":"mcp_hook","mailbox":"agent","event":"on_receive","origin":"mcp",
    "template":"invoke-claude-alice","params":{"prompt":"…"}}]
```

---

### `hook_delete`

Delete a hook by effective name. Only MCP-origin hooks are deletable
via this tool; operator-origin hooks refuse with `ERR origin-protected`.

**Parameters:**
| Name   | Type   | Required | Description |
|--------|--------|----------|-------------|
| `name` | string | yes      | Effective name from `hook_list` |

**Returns:** Confirmation string.

**Example:**
```
hook_delete(name: "mcp_hook")
→ "Hook 'mcp_hook' deleted."

hook_delete(name: "daily-report")
→ Error: "[VALIDATION] origin-protected: hook 'daily-report' was
   created by the operator — remove via `sudo aimx hooks delete`
   instead"
```
