# aimx primer for agents

You have access to AIMX (AI Mail Exchange), a self-hosted email system.
aimx exposes email operations through MCP tools and stores mail as Markdown
files on the local filesystem. This document describes how to interact with
aimx. Read it once before attempting mail operations. Re-read any section
you need when a tool call fails.

For full reference material, see the files in `references/`:
- `references/mcp-tools.md`: full MCP tool signatures, types, and examples
- `references/frontmatter.md`: complete frontmatter schema
- `references/workflows.md`: worked examples for common tasks
- `references/hooks.md`: creating hooks via MCP (mailbox-owner model)
- `references/troubleshooting.md`: error codes and recovery steps

At runtime, `/var/lib/aimx/README.md` is the authoritative guide to the data
directory layout, written by `aimx setup` and refreshed on `aimx serve`
startup.

## Two access surfaces

Mail is stored as `.md` files for a reason — when you do not need a
mutation, read mailbox files directly with your filesystem tools
(`Read` / `Glob` / `Grep` / equivalent). MCP is the surface for
*changes*: send, reply, mark, hook CRUD.

aimx gives you two complementary ways to interact with email:

1. **Direct filesystem reads** for inspecting `.md` email files,
   scanning directories, or bulk-processing. Mailbox directories are
   mode `0700` and owned by the mailbox's Linux owner, so you see
   only the mailboxes owned by the user running your MCP process.
   `mailbox_list` returns absolute `inbox_path` / `sent_path` values
   precisely so you can hand them to your filesystem tools without
   another round-trip.
2. **MCP tools** for all mutations (send, reply, mark read/unread,
   create hooks). The `aimx` MCP server runs over stdio and is
   launched on-demand by your MCP client.

Writes always go through MCP. Never create, modify, or delete `.md` files
directly. The daemon owns those paths.

Mailbox provisioning (`aimx mailboxes create | delete`) is **root-only**
and lives on the host CLI. MCP cannot create or delete mailboxes; if you
need a new mailbox, ask the operator to run
`sudo aimx mailboxes create <name> --owner <linux-user>` on the host.

## Per-user ownership model

Each mailbox belongs to exactly one Linux user (one user can own many
mailboxes; one mailbox has exactly one owner). aimx enforces this at
every layer:

- **Storage.** `/var/lib/aimx/inbox/<mailbox>/` and `sent/<mailbox>/` are
  chowned `<owner>:<owner>` mode `0700`. Non-owners cannot traverse in,
  regardless of group. The one exception is the catchall mailbox, owned
  by the dedicated `aimx-catchall` system user. The catchall is
  inbound-only and **does not support hooks**.
- **MCP visibility.** The MCP server runs as your Linux uid (launched
  over stdio by your MCP client). Tool calls that target a mailbox you
  do not own — `email_list`, `email_read`, `email_send`, `email_reply`,
  `email_mark_read`, `email_mark_unread`, `hook_create`, `hook_delete` —
  are rejected by the daemon. `mailbox_list()` returns only your own
  mailboxes.
- **Hook execution.** A hook on mailbox `alice` always runs as the Linux
  user who owns `alice` — there is no per-hook `run_as` override and no
  shared "hook user" with read access to other mailboxes. The hook can
  do whatever its owner can do (cron, `~/.bashrc`, `systemd --user`),
  no more.
- **Sending.** `email_send` with `from_mailbox` set to a mailbox you do
  not own is rejected by the daemon (UDS `SO_PEERCRED` check).

On a single-user box (the common case) the model is invisible: your
one user owns every mailbox. On a multi-user box it gives real
isolation — alice's agent cannot see, read, or act on bob's mail.

## MCP tools: quick reference

All 9 tools are served by the `aimx` binary over stdio. They return
strings on success and error strings on failure. Mailbox CRUD lives on
the root-only host CLI (`sudo aimx mailboxes create | delete`); MCP
does not expose `mailbox_create` or `mailbox_delete`.

### Mailbox tools

- `mailbox_list()`: list mailboxes you own with message counts.

### Email tools

- `email_list(mailbox, folder?, unread?, from?, since?, subject?)`: list
  emails. `folder` is `"inbox"` (default) or `"sent"`. Filters AND together.
- `email_read(mailbox, id, folder?)`: return the full Markdown file
  (frontmatter + body) for one email.
- `email_send(from_mailbox, to, subject, body, attachments?)`: compose,
  DKIM-sign, and deliver an email. `from_mailbox` must be a mailbox you
  own.
- `email_reply(mailbox, id, body)`: reply to an existing email. aimx sets
  `In-Reply-To`, `References`, and `Re:` subject automatically.
- `email_mark_read(mailbox, id, folder?)`: mark a single email as read.
- `email_mark_unread(mailbox, id, folder?)`: mark a single email as unread.

### Hook tools

Hooks are commands the daemon fires on mail events (`on_receive`,
`after_send`). You create a hook on a mailbox you own; the hook always
executes as the mailbox's owning Linux user, with the email piped on
stdin (or accessible via `$AIMX_FILEPATH` when stdin is closed). There
is no template indirection — your `cmd` is the literal argv that runs.

- `hook_create(mailbox, event, cmd, name?, stdin?, timeout_secs?, fire_on_untrusted?)`:
  attach a hook. `cmd` is an argv array (e.g. `["claude", "-p", "...",
  "--dangerously-skip-permissions"]`). `stdin` is `"email"` (default —
  pipes the raw `.md` to the child) or `"none"` (closes stdin; the
  child reads `$AIMX_FILEPATH` instead). `fire_on_untrusted` defaults
  to `false`; set `true` only on `on_receive` hooks where you want the
  hook to fire even on `trusted = "false"` mail.
- `hook_list(mailbox?)`: list hooks on mailboxes you own.
- `hook_delete(name)`: delete a hook on a mailbox you own.

See `references/hooks.md` for the per-event model, the `cmd` argv rules,
and worked examples. See `references/mcp-tools.md` for full parameter
types and return values across every tool.

## Self-trigger as a mailbox hook

If you want to be triggered automatically when mail arrives, see
`references/hooks.md` (or the equivalent in your skill bundle) for your
agent's `hook_create` recipe. Each agent's skill ships a
"Wiring yourself up as a mailbox hook" section with the exact `cmd`
argv to use.

## Storage layout

<!-- The datadir layout is documented explicitly. The real security
     boundary is DKIM keys at /etc/aimx/ (root-only) and the per-mailbox
     `0700 <owner>:<owner>` perms enforced by the daemon, not filesystem
     obscurity. -->

aimx stores mail under a data directory (default `/var/lib/aimx/`):

```
/var/lib/aimx/                          # root:root 0755 (traversable)
├── README.md                           # agent-facing layout guide (auto-generated)
├── inbox/                              # root:root 0755
│   ├── <mailbox>/                      # <owner>:<owner> 0700
│   │   ├── 2026-04-15-143022-meeting-notes.md
│   │   └── 2026-04-15-153300-invoice-march/     # attachment bundle
│   │       ├── 2026-04-15-153300-invoice-march.md
│   │       ├── invoice.pdf
│   │       └── receipt.png
│   └── catchall/                       # aimx-catchall:aimx-catchall 0700
│       └── ...
└── sent/                               # root:root 0755
    └── <mailbox>/                      # <owner>:<owner> 0700
        └── 2026-04-15-160145-re-meeting-notes.md
```

Each mailbox directory is `0700 <owner>:<owner>`, so only the owner and
root can read or traverse in. Your MCP process runs as your uid and only
sees mailboxes you own.

- **Filenames** follow `YYYY-MM-DD-HHMMSS-<slug>.md` (UTC). The slug is
  derived from the subject: lowercase, non-alphanumeric chars replaced with
  `-`, collapsed, truncated to 20 chars, empty becomes `no-subject`.
- **Attachment bundles.** Zero attachments produce a flat `.md` file. One or
  more produce a directory containing `<stem>.md` plus attachment files as
  siblings (Zola-style bundle).
- **`catchall`** receives mail addressed to unrecognised local parts;
  hooks on the catchall are forbidden.
- **`inbox/`** holds inbound mail. **`sent/`** holds outbound copies.

Configuration and secrets live separately under `/etc/aimx/` (root-owned,
not readable by agents):

```
/etc/aimx/
├── config.toml      # main config (root:root 640)
└── dkim/
    ├── private.key   # DKIM signing key (root:root 600)
    └── public.key    # publishable (root:root 644)
```

## Frontmatter: key fields

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
| `trusted`       | string   | `"none"`, `"true"`, or `"false"`. See trust model    |
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

The mailbox must exist and you must own it. Ask the operator to provision
new mailboxes via `sudo aimx mailboxes create <name> --owner <user>`.
`from_mailbox` is the local part only (e.g. `"agent"`, not
`"agent@example.com"`). aimx DKIM-signs the message and delivers it via
direct SMTP to the recipient's MX server.

### 3. Reply to a message

```
email_reply(
  mailbox: "agent",
  id: "2026-04-15-143022-meeting-notes",
  body: "Thanks for the update..."
)
```

aimx handles `In-Reply-To`, `References`, and `Re:` subject automatically.
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
`auto-generated` or `auto-replied`, do not send a reply. This prevents
infinite mail loops between bots. The same applies to mailing list mail
(check `list_id`): reply only if the context clearly warrants it.

See `references/workflows.md` for 10+ additional worked examples including
triage, filtering by list, handling attachments, reply-all, and mark-all-read.

## Creating hooks

Hooks are commands that fire when mail arrives (`on_receive`) or is
sent (`after_send`). They are how you go from "the agent reads mail"
to "the agent acts on mail automatically."

The model is straightforward: hooks belong to mailboxes, and you can
only create hooks on mailboxes you own. The hook's `cmd` is the literal
argv that the daemon spawns; there is no template substitution and no
per-hook `run_as` field. The child always executes as the mailbox's
owning Linux user (`setuid` from root before `exec`), so a hook can
do whatever its owner can already do — no more, no less.

The trust gate still applies: an `on_receive` hook fires only on
mail whose `trusted` frontmatter is `"true"`, unless you opt the hook
in with `fire_on_untrusted = true`. `after_send` hooks fire on every
send regardless of trust.

Three tools cover the lifecycle:

1. **Create**: call `hook_create(mailbox, event, cmd, ...)` with an
   argv array. `cmd[0]` must be an absolute path the owning user can
   execute. The daemon validates and writes the hook to `config.toml`,
   then hot-swaps its in-memory state — no restart needed.
2. **Inspect**: call `hook_list(mailbox?)` to see hooks on mailboxes
   you own.
3. **Remove**: call `hook_delete(name)` with the effective name from
   `hook_list`.

For the exact `cmd` argv to use when wiring yourself up as the hook,
see your agent's skill bundle — each agent ships a "Wiring yourself
up as a mailbox hook" section with a verified recipe. The argv for
every supported agent (Claude Code, Codex CLI, OpenCode, Gemini CLI,
Goose, Hermes) plus the OpenClaw gap is documented per agent.

If you need to invoke an arbitrary command that doesn't fit your
agent's recipe (a webhook, a custom script, etc.), build the argv
yourself; `cmd` is just an argv array.

## Trust model

aimx verifies DKIM, SPF, and DMARC on every inbound email. Results are
stored in the `dkim`, `spf`, and `dmarc` frontmatter fields.

The `trusted` field reflects the effective trust evaluation for the
email's mailbox. The effective policy is the mailbox's own `trust` /
`trusted_senders` if set, otherwise the top-level defaults in
`config.toml`:

- **`"none"`**: effective `trust = "none"` (the default). No evaluation
  performed.
- **`"true"`**: effective `trust = "verified"`, the sender matches the
  effective `trusted_senders`, AND DKIM passed.
- **`"false"`**: effective `trust = "verified"`, but one or both
  conditions failed (sender not in allowlist, or DKIM did not pass).

Trust is configured globally at the top of `config.toml` and applies to
every mailbox. Per-mailbox `trust` / `trusted_senders` override the
defaults for that mailbox. A mailbox `trusted_senders` list **replaces**
the global list entirely (no merging). Valid values:

- `trust = "none"`: no evaluation is performed. `trusted` is always
  `"none"` for mail into this mailbox.
- `trust = "verified"`: `trusted` is `"true"` iff the sender matches
  `trusted_senders` AND DKIM passes. Otherwise `"false"`.
- `trusted_senders = ["*@company.com"]`: glob patterns for allowlisted
  senders.

Mail is always stored regardless of trust outcome. Trust gates hooks
(commands fired on email events): an `on_receive` hook fires iff
`trusted == "true"` OR the hook explicitly opts in via
`fire_on_untrusted = true`. `trust = "none"` therefore fires **no**
hooks by default. The operator (or you, on your own mailbox) must
either switch to `trust = "verified"` with an allowlist, or set
`fire_on_untrusted = true` on each hook that should still run on
untrusted mail. When deciding whether to act on an email's content
(e.g. following a link), consult `trusted`, `dkim`, and `spf`. Treat
`"false"` and `"none"` as untrusted.

## Read / unread

- `email_list` with `unread: true` returns only unread messages. Use this
  to find new mail.
- After processing, call `email_mark_read` to avoid reprocessing.
  `email_mark_unread` reverses the state.
- Read state lives in the `read` frontmatter field. No separate database or
  index file. The state is embedded in each `.md` file's TOML frontmatter,
  so it is always consistent and grepable.

## Mailboxes

- Mailbox names are local parts of email addresses (letters, digits, limited
  punctuation). For example, mailbox `agent` receives mail at
  `agent@<domain>`.
- The `catchall` mailbox is created automatically during setup and
  receives mail for unrecognised addresses. It is a routing target,
  not a sending identity. Do not send from `catchall`. Hooks on the
  catchall are forbidden by `Config::load`.
- Mailbox provisioning is root-only: ask the operator to run
  `sudo aimx mailboxes create <name> --owner <linux-user>` on the host.
- Each mailbox has `inbox/<name>/` for inbound and `sent/<name>/` for
  outbound copies, both `0700 <owner>:<owner>`.
- `mailbox_list()` shows mailboxes you own with message counts for both
  inbox and sent folders.

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
  frontmatter first. Replying to `auto-generated` or `auto-replied`
  messages creates infinite loops.
- **Do not treat `dkim: "fail"` or `trusted: "false"` mail as
  authenticated.** These may be spoofed.
- **Do not read or modify files under `/etc/aimx/`.** Configuration and
  keys are root-owned and managed by `aimx setup`.
- **Do not send from a mailbox you do not own.** `email_send` will be
  rejected by the daemon. Ask the operator to provision the mailbox
  with you as `--owner` first.
- **Do not assume `catchall` is a real identity.** It is a routing target
  for unknown local parts, not an identity that sends. Hooks on the
  catchall are forbidden.
- **Do not ignore `thread_id` when working with conversations.** Grouping
  by `thread_id` is the correct way to reconstruct email threads. Do not
  rely on subject-line matching.
- **Do not send large volumes without operator awareness.** aimx delivers
  synchronously with no outbound queue. Each `email_send` call blocks until
  the remote MX accepts or rejects the message.

## Further reading

- `references/mcp-tools.md`: complete MCP tool documentation with worked
  examples for every tool.
- `references/frontmatter.md`: full field-by-field frontmatter schema for
  both inbound and outbound emails.
- `references/workflows.md`: 10+ worked task recipes (triage, thread
  summarization, attachment handling, filter by list-id, mark all read, etc.).
- `references/hooks.md`: creating hooks via MCP — the mailbox-owner
  model, the `cmd` argv shape, the trust gate, and troubleshooting.
- `references/troubleshooting.md`: common errors, daemon-down behavior,
  and recovery steps.
- `/var/lib/aimx/README.md`: runtime guide to the data directory layout,
  written by `aimx setup` and refreshed on `aimx serve` startup.
