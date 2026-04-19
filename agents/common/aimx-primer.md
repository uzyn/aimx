# AIMX primer for agents

You have access to AIMX, a self-hosted email system. AIMX exposes email
operations through MCP tools and stores mail as Markdown files on the local
filesystem. This document describes how to interact with AIMX. Read it once
before attempting mail operations; re-read any section you need when a tool
call fails.

For full reference material, see the files in `references/`:
- `references/mcp-tools.md` — full MCP tool signatures, types, and examples
- `references/frontmatter.md` — complete frontmatter schema
- `references/workflows.md` — worked examples for common tasks
- `references/troubleshooting.md` — error codes and recovery steps

At runtime, `/var/lib/aimx/README.md` is the authoritative guide to the data
directory layout, written by `aimx setup` and refreshed on `aimx serve`
startup.

## Two access surfaces

AIMX gives you two complementary ways to interact with email:

1. **MCP tools** — for all mutations (send, reply, create mailboxes, mark
   read/unread). The `aimx` MCP server runs over stdio and is launched
   on-demand by your MCP client.
2. **Direct filesystem reads** — for reading `.md` email files, scanning
   directories, or bulk-processing. The data directory is world-readable and
   its format is stable.

Writes always go through MCP. Never create, modify, or delete `.md` files
directly — the daemon owns those paths.

## MCP tools — quick reference

All 9 tools are served by the `aimx` binary over stdio. They return strings
on success and error strings on failure.

### Mailbox tools

- `mailbox_create(name)` — create a new mailbox identity (inbox + sent).
- `mailbox_list()` — list all mailboxes with message counts.
- `mailbox_delete(name)` — delete an empty mailbox. The daemon refuses
  when `inbox/<name>/` or `sent/<name>/` still contains files; the MCP
  error spells out the per-directory file counts and tells you to run
  `sudo aimx mailboxes delete --force <name>` on the host. MCP does not
  wipe mail — that stays on the CLI where the operator sees the prompt.

### Email tools

- `email_list(mailbox, folder?, unread?, from?, since?, subject?)` — list
  emails. `folder` is `"inbox"` (default) or `"sent"`. Filters AND together.
- `email_read(mailbox, id, folder?)` — return the full Markdown file
  (frontmatter + body) for one email.
- `email_send(from_mailbox, to, subject, body, attachments?)` — compose,
  DKIM-sign, and deliver an email. `from_mailbox` must be a real mailbox.
- `email_reply(mailbox, id, body)` — reply to an existing email. AIMX sets
  `In-Reply-To`, `References`, and `Re:` subject automatically.
- `email_mark_read(mailbox, id, folder?)` — mark a single email as read.
- `email_mark_unread(mailbox, id, folder?)` — mark a single email as unread.

See `references/mcp-tools.md` for full parameter types, return values, and
worked examples.

## Storage layout

<!-- FR-50c: the datadir layout is documented explicitly. The real security
     boundary is DKIM keys at /etc/aimx/ (root-only) and the UDS socket at
     /run/aimx/send.sock — not filesystem obscurity. -->

AIMX stores mail under a data directory (default `/var/lib/aimx/`):

```
/var/lib/aimx/                          # world-readable datadir
├── README.md                           # agent-facing layout guide (auto-generated)
├── inbox/
│   ├── <mailbox>/
│   │   ├── 2026-04-15-143022-meeting-notes.md
│   │   └── 2026-04-15-153300-invoice-march/     # attachment bundle
│   │       ├── 2026-04-15-153300-invoice-march.md
│   │       ├── invoice.pdf
│   │       └── receipt.png
│   └── catchall/                       # unknown local parts
│       └── ...
└── sent/
    └── <mailbox>/
        └── 2026-04-15-160145-re-meeting-notes.md
```

- **Filenames** follow `YYYY-MM-DD-HHMMSS-<slug>.md` (UTC). The slug is
  derived from the subject: lowercase, non-alphanumeric chars replaced with
  `-`, collapsed, truncated to 20 chars, empty becomes `no-subject`.
- **Attachment bundles** — zero attachments produce a flat `.md` file; one or
  more produce a directory containing `<stem>.md` plus attachment files as
  siblings (Zola-style bundle).
- **`catchall`** receives mail addressed to unrecognised local parts.
- **`inbox/`** holds inbound mail; **`sent/`** holds outbound copies.

Configuration and secrets live separately under `/etc/aimx/` (root-owned,
not readable by agents):

```
/etc/aimx/
├── config.toml      # main config (root:root 640)
└── dkim/
    ├── private.key   # DKIM signing key (root:root 600)
    └── public.key    # publishable (root:root 644)
```

## Frontmatter — key fields

Each email file has TOML frontmatter between `+++` delimiters. The fields
agents most commonly need:

| Field            | Type     | Notes                                              |
|-----------------|----------|-----------------------------------------------------|
| `id`            | string   | Matches the filename stem                            |
| `message_id`    | string   | RFC 5322 Message-ID, without angle brackets          |
| `thread_id`     | string   | 16-hex-char SHA-256 of the thread root Message-ID    |
| `from`          | string   | Sender address (may include display name)            |
| `to`            | string   | Recipient address                                    |
| `subject`       | string   | Email subject, or `(no subject)` when absent         |
| `date`          | string   | Sender-claimed RFC 3339 timestamp                    |
| `received_at`   | string   | Server-side RFC 3339 UTC timestamp                   |
| `trusted`       | string   | `"none"`, `"true"`, or `"false"` — see trust model   |
| `read`          | bool     | `false` on ingest                                    |
| `list_id`       | string?  | Mailing list ID header, if present                   |
| `auto_submitted`| string?  | `auto-generated`, `auto-replied`, etc., if present   |
| `labels`        | string[] | Empty by default                                     |
| `dkim`          | string   | `"pass"`, `"fail"`, or `"none"`                      |
| `spf`           | string   | `"pass"`, `"fail"`, `"softfail"`, `"neutral"`, or `"none"` |
| `dmarc`         | string   | `"pass"`, `"fail"`, or `"none"`                      |

Sent copies carry an additional outbound block: `outbound = true`,
`delivery_status` (`"delivered"`, `"deferred"`, `"failed"`, `"pending"`),
and optional `delivered_at` and `delivery_details`.

See `references/frontmatter.md` for the complete schema with every field,
types, required/optional, and the outbound block.

## Common workflows

### 1. Check inbox for new mail

```
email_list(mailbox: "agent", unread: true)
```

Loop through results. For each, call `email_read` to get the content, then
`email_mark_read` when done processing. This is the standard polling pattern.

### 2. Send an email

```
email_send(
  from_mailbox: "agent",
  to: "alice@example.com",
  subject: "Weekly report",
  body: "Here is the summary..."
)
```

The mailbox must exist — create it first with `mailbox_create` if needed.
`from_mailbox` is the local part only (e.g. `"agent"`, not
`"agent@example.com"`). AIMX DKIM-signs the message and delivers it via
direct SMTP to the recipient's MX server.

### 3. Reply to a message

```
email_reply(
  mailbox: "agent",
  id: "2026-04-15-143022-meeting-notes",
  body: "Thanks for the update..."
)
```

AIMX handles `In-Reply-To`, `References`, and `Re:` subject automatically.
The `id` is the filename stem (visible in `email_list` output or in
frontmatter).

### 4. Summarize a thread

Use `thread_id` from frontmatter to group messages:

1. `email_list(mailbox: "agent")` to get all messages.
2. Read each via `email_read` and group by `thread_id`.
3. Sort by `date` within each group.

All messages in a thread share the same `thread_id`, which is derived from
the earliest Message-ID in the `References` chain.

### 5. Handle auto-submitted mail

Check `auto_submitted` in frontmatter before replying. If it is
`auto-generated` or `auto-replied`, do not send a reply — this prevents
infinite mail loops between bots. The same applies to mailing list mail
(check `list_id`): reply only if the context clearly warrants it.

See `references/workflows.md` for 10+ additional worked examples including
triage, filtering by list, handling attachments, reply-all, and mark-all-read.

## Trust model

AIMX verifies DKIM, SPF, and DMARC on every inbound email. Results are
stored in the `dkim`, `spf`, and `dmarc` frontmatter fields.

The `trusted` field reflects the effective trust evaluation for the
email's mailbox. The effective policy is the mailbox's own `trust` /
`trusted_senders` if set, otherwise the top-level defaults in
`config.toml`:

- **`"none"`** — effective `trust = "none"` (the default). No evaluation
  performed.
- **`"true"`** — effective `trust = "verified"`, the sender matches the
  effective `trusted_senders`, AND DKIM passed.
- **`"false"`** — effective `trust = "verified"`, but one or both
  conditions failed (sender not in allowlist, or DKIM did not pass).

Trust is configured globally at the top of `config.toml` and applies to
every mailbox. Per-mailbox `trust` / `trusted_senders` override the
defaults for that mailbox — a mailbox `trusted_senders` list **replaces**
the global list entirely (no merging). Valid values:

- `trust = "none"` — all mail is accepted, triggers fire freely.
- `trust = "verified"` — triggers only fire when `trusted == "true"` or
  the sender is in the effective `trusted_senders` allowlist.
- `trusted_senders = ["*@company.com"]` — glob patterns for allowlisted
  senders.

Mail is always stored regardless of trust outcome. Trust only gates channel
triggers (shell commands fired on inbound mail). When deciding whether to
act on an email's content (e.g. following a link), consult `trusted`, `dkim`,
and `spf`. Treat `"false"` and `"none"` as untrusted.

## Read / unread

- `email_list` with `unread: true` returns only unread messages — use this
  to find new mail.
- After processing, call `email_mark_read` to avoid reprocessing.
  `email_mark_unread` reverses the state.
- Read state lives in the `read` frontmatter field. No separate database or
  index file — the state is embedded in each `.md` file's TOML frontmatter,
  so it is always consistent and grepable.

## Mailboxes

- Mailbox names are local parts of email addresses (letters, digits, limited
  punctuation). For example, mailbox `agent` receives mail at
  `agent@<domain>`.
- The `catchall` mailbox is created automatically during setup and receives
  mail for unrecognised addresses. It is a routing target, not a sending
  identity — do not send from `catchall`.
- Create additional mailboxes with `mailbox_create` before sending from them.
- Each mailbox has `inbox/<name>/` for inbound and `sent/<name>/` for
  outbound copies.
- `mailbox_list()` shows all mailboxes with message counts for both inbox
  and sent folders.
- `mailbox_delete(name)` only succeeds when both `inbox/<name>/` and
  `sent/<name>/` are empty. On non-empty the MCP error tells you the
  file counts and points at the host CLI command that wipes-and-deletes
  (`sudo aimx mailboxes delete --force <name>`); MCP itself never wipes
  mail.

## Attachments

- Inbound attachments are extracted into the Zola-style bundle directory
  alongside the `.md` file. The frontmatter `attachments` array lists each
  attachment with `filename`, `content_type`, `size`, and `path` fields.
- To send with attachments, pass absolute file paths to `email_send`:
  ```
  email_send(
    from_mailbox: "agent",
    to: "bob@example.com",
    subject: "Report with data",
    body: "Attached.",
    attachments: ["/tmp/report.csv"]
  )
  ```
- Attachment paths in frontmatter are relative to the bundle directory.

## Sent mail

- Every successfully sent email is persisted under `sent/<mailbox>/`.
- Use `email_list(mailbox: "agent", folder: "sent")` to browse sent mail.
- Sent copies include the full DKIM-signed message and an outbound block
  in frontmatter with `delivery_status`, `delivered_at`, and
  `delivery_details`.

## What you must not do

- **Do not write to the data directory.** All mutations go through MCP tools.
  Creating, modifying, or deleting `.md` files directly will corrupt state.
- **Do not reply to auto-submitted mail.** Check `auto_submitted` in
  frontmatter first — replying to `auto-generated` or `auto-replied`
  messages creates infinite loops.
- **Do not treat `dkim: "fail"` or `trusted: "false"` mail as
  authenticated.** These may be spoofed.
- **Do not read or modify files under `/etc/aimx/`.** Configuration and
  keys are root-owned and managed by `aimx setup`.
- **Do not send from a mailbox that does not exist.** `email_send` will
  fail. Create it first with `mailbox_create`.
- **Do not assume `catchall` is a real identity.** It is a routing target
  for unknown local parts, not an identity that sends.
- **Do not ignore `thread_id` when working with conversations.** Grouping
  by `thread_id` is the correct way to reconstruct email threads; do not
  rely on subject-line matching.
- **Do not send large volumes without operator awareness.** AIMX delivers
  synchronously with no outbound queue. Each `email_send` call blocks until
  the remote MX accepts or rejects the message.

## Further reading

- `references/mcp-tools.md` — complete MCP tool documentation with worked
  examples for every tool.
- `references/frontmatter.md` — full field-by-field frontmatter schema for
  both inbound and outbound emails.
- `references/workflows.md` — 10+ worked task recipes (triage, thread
  summarization, attachment handling, filter by list-id, mark all read, etc.).
- `references/troubleshooting.md` — UDS protocol error codes, common
  misconfigurations, and recovery steps.
- `/var/lib/aimx/README.md` — runtime guide to the data directory layout,
  written by `aimx setup` and refreshed on `aimx serve` startup.
