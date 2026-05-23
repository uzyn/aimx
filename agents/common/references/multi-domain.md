# aimx multi-domain: full reference

aimx can be configured with one domain or several. From the agent's
perspective, the change is small but load-bearing: mailbox identifiers
become full email addresses (FQDNs), and bare local parts default to
the **default domain** (the first entry in the `domains` array). This
document spells out the rules and the worked patterns.

If you're on a single-domain install, you can skim this — the
default-domain rule preserves all single-domain behavior verbatim.

## The default domain

The operator's `config.toml` carries a `domains` array. The first entry
is the **default**:

```toml
domains = ["a.com", "b.com"]   # a.com is the default
```

You never read `config.toml` (it's `0640 root:root`). The default domain
surfaces to you implicitly:

- `mailbox_list()` returns `address` fields that name the FQDN
  (`agent@a.com`). The substring after `@` is the mailbox's domain.
- `mailbox_create("agent")` returns `agent@<domains[0]>` — the new
  mailbox lives at the default domain.
- `email_send(from_mailbox: "agent", ...)` resolves to `agent@<domains[0]>`
  daemon-side and is DKIM-signed as the default domain.

In other words: when in doubt, use bare local parts and you'll target
the default domain. This is the "single-domain" mental model preserved
for multi-domain installs.

## FQDN disambiguation

When you need to target a non-default domain, **pass the FQDN** in any
mailbox-name parameter:

```
email_send(
  from_mailbox: "agent@b.com",     # FQDN — targets b.com, signs as b.com
  to: "alice@example.com",
  subject: "Hello",
  body: "Hi from b.com"
)
```

The same rule applies to every mailbox-scoped parameter:

| Tool | Parameter | Bare local part | FQDN |
|------|-----------|-----------------|------|
| `email_list` | `mailbox` | default domain | targets named domain |
| `email_read` | `mailbox` | default domain | targets named domain |
| `email_send` | `from_mailbox` | default domain | targets named domain |
| `email_reply` | `mailbox` | default domain | targets named domain |
| `email_mark_read` / `_unread` | `mailbox` | default domain | targets named domain |
| `mailbox_create` | `name` | creates at default | creates at named domain |
| `mailbox_delete` | `name` | targets default | targets named domain |
| `hook_create` | `mailbox` | default domain | targets named domain |
| `hook_list` | `mailbox` (filter) | default domain | targets named domain |

**Important**: on a multi-domain install where both `support@a.com` and
`support@b.com` exist, `mailbox: "support"` silently targets
`support@<domains[0]>`. If you're processing a result from
`mailbox_list()` and feeding the `name` field back into another tool
call, don't strip the `@<domain>` suffix — the FQDN is the unambiguous
identifier.

## Storage layout

Each domain has its own subtree under the data directory:

```
/var/lib/aimx/
├── a.com/
│   ├── inbox/<mailbox>/
│   └── sent/<mailbox>/
└── b.com/
    ├── inbox/<mailbox>/
    └── sent/<mailbox>/
```

`mailbox_list()` returns absolute paths (`inbox_path`, `sent_path`)
that already include the per-domain nesting — use them verbatim with
your filesystem tools instead of reconstructing paths. Each mailbox
directory is still `0700 <owner>:<owner>`; isolation across mailboxes
within and across domains is filesystem-enforced.

## Per-domain config (operator-side)

The operator may set per-domain overrides under
`[domain."<domain>"]` sub-tables:

```toml
domains = ["a.com", "b.com"]

[domain."b.com"]
signature = "Sent from B Corp"
dkim_selector = "s2025"
trust = "verified"
trusted_senders = ["*@trusted-partner.com"]
```

You don't read this directly. The agent-visible effects are:

- **Signature.** Outbound mail from b.com gets the b.com signature
  appended automatically. The `body` you pass to `email_send` /
  `email_reply` does not need to include it.
- **Trust.** The `trusted` frontmatter field on inbound mail to b.com
  is evaluated against b.com's effective trust policy (per-mailbox →
  per-domain → global). You don't need to change behavior based on
  this — the trust gate on `on_receive` hooks is enforced by the
  daemon.
- **DKIM selector.** Affects the DKIM signature only; the signing key
  and selector are picked automatically based on the From: domain.

## Domain management is operator-only

There are **no** MCP tools for domain CRUD. Adding or removing a
domain, generating a DKIM keypair for a new domain, and the upgrade
migration are all operator-driven via `sudo`. The deliberate boundary:

- Agents can `mailbox_create` and `mailbox_delete` mailboxes they
  own, on any **existing** domain (default or otherwise).
- Agents **cannot** add a new domain or remove an existing one. If
  the user asks for that, surface the operator command (`sudo aimx
  domains add <domain>` or `sudo aimx domains remove <domain>`) and
  stop. Don't shell out to `aimx`.

You can infer the list of configured domains by calling `mailbox_list()`
and reading the `@<domain>` suffix of each FQDN. There is no other API
for the domain list.

## Worked examples

### Send from a specific domain

```
mailbox_list()
→ [
    {"name": "agent@a.com", "address": "agent@a.com", "registered": true, ...},
    {"name": "agent@b.com", "address": "agent@b.com", "registered": true, ...}
]

email_send(
  from_mailbox: "agent@b.com",
  to: "alice@example.com",
  subject: "Hello from b.com",
  body: "..."
)
```

The daemon signs with b.com's DKIM key. The sent copy lands at
`/var/lib/aimx/b.com/sent/agent/`.

### List mail across all owned mailboxes (any domain)

```
for mb in mailbox_list():
    rows = email_list(mailbox=mb["name"])  # mb["name"] is the FQDN
    for row in rows:
        # process row
```

Always thread `mb["name"]` (the FQDN) through into the next tool call.

### Reply to mail received on a non-default domain

```
email_list(mailbox: "support@b.com")
→ [{"id": "2026-05-01-090000-question", ...}, ...]

email_reply(
  mailbox: "support@b.com",   # FQDN — sticks the reply to the b.com side
  id: "2026-05-01-090000-question",
  body: "Thanks for reaching out..."
)
```

The reply is sent from `support@b.com` and DKIM-signed as b.com,
matching the original message's domain.

### Create a fresh mailbox at a specific domain

```
mailbox_create("task-42@b.com")     # FQDN — creates at b.com
→ "task-42@b.com"
```

Without the FQDN, `mailbox_create("task-42")` would create
`task-42@<default-domain>`.

## What to tell the user

If a user asks you to "send from a different domain" and you only see
one domain in `mailbox_list()`'s output, the install is single-domain
— tell the user that adding a second domain requires the host
operator to run `sudo aimx domains add <domain>`. Don't shell out.

If a user asks you to "set up a new domain", surface the same
operator command and stop. Domain CRUD is out of the MCP surface
by design.
