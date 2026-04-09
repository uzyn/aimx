# Idea

> Your server can receive email. Why are you routing through Gmail?

SMTP for agents. No middleman.

One command to give your agents their own email addresses — no Gmail, no OAuth, no third-party SaaS.

Built for Claude Code, OpenClaw, Codex, and any agentic system that needs an email channel.


## The Problem

Today, giving an AI agent an email address is absurdly complicated:

- **Gmail route:** Create a dedicated Gmail account, set up Google Cloud Console with billing, configure OAuth credentials, manage refresh tokens, install `gogcli`, set up SSH tunnels for browser-based auth from a headless server — and risk getting banned because Gmail detects bot behavior.
- **AgentMail / SaaS route:** Send your agent's email through a YC startup's infrastructure. Your data lives on their servers. They fold, your agent loses its inbox.
- **DIY route:** Pick a mail server, pick an MCP wrapper, parse MIME yourself, write your own delivery pipeline, glue it all together.

All the above involve having your email communications flow through external 3rd-party services, exposing sensitive information to 3rd parties when all you want to do is to have secure trusted email communication channel with your AI agents. Not mentioning you might potentially even violate some ToS of these services.

All of this sounds even more absurd when you're dedicating an entire server for your agentic system that is perfectly capable of sending and receiving email.


## The Solution

```bash
apt install aimx
aimx setup agent.mydomain.com
```

Done. Your agent has an email address. Incoming mail is parsed to Markdown. Outbound mail just works. MCP is built in. Channel rules trigger agent actions on incoming mail.

No Gmail. No OAuth. No third parties. No IMAP clients. Just SMTP, directly on your server.


## Architecture

```
Inbound:
  Sender → port 25 → OpenSMTPD → aimx ingest → .md file
                                                        → channel manager (triggers agent)
s
Outbound:
  MCP tool call → aimx send → DKIM sign → OpenSMTPD → remote MX

Storage:
  /var/lib/aimx/
  ├── schedule/
  │   ├── 2026-04-09-001.md
  │   └── 2026-04-09-002.md
  ├── family/
  │   └── 2026-04-08-001.md
  ├── accounting/
  │   ├── 2026-04-07-001.md
  │   └── attachments/
  │       └── invoice-march.pdf
  └── catchall/
      └── ...
```

### Key design decisions

- **No daemon.** OpenSMTPD is the only long-running process. It calls `aimx ingest` on each incoming message. The MCP server runs in stdio mode, launched on-demand by Claude Code. Nothing else needs to run.
- **No IMAP/POP3/JMAP.** No mail clients will connect to this. Agents read `.md` files via MCP or filesystem. The mail server's only job is SMTP transport.
- **`.md` first.** Emails are stored as Markdown with YAML frontmatter, not raw `.eml`. Agents can `cat` an email and understand it immediately. Attachments are extracted to a sibling directory.
- **DKIM signing in Rust.** Rather than depending on `opensmtpd-filter-dkimsign`, `aimx send` handles DKIM signing natively before handing the message to OpenSMTPD for delivery. Keeps the dependency tree minimal.
- **Mailboxes are directories.** Creating a mailbox = creating a folder + registering an address. No OS users, no passwords, no database.


## Setup Flow

```
$ aimx setup agent.mydomain.com

Step 1/6: Preflight checks
  Outbound port 25... (connecting to gmail MX)                ✓ open
  Inbound port 25...  (asking check.aimx.email)         ✓ reachable
  Reverse DNS (PTR)... ⚠ not set
    Warning: Some providers (Gmail, Outlook) may flag outbound
    mail as spam. Set PTR to mail.agent.mydomain.com in your
    VPS provider's control panel.
    Continue anyway? [Y/n]

Step 2/6: Configuring mail server
  Configuring OpenSMTPD... done
  Generating DKIM keys (2048-bit RSA)... done

Step 3/6: DNS records
  Add these records to your DNS provider:

  MX    agent.mydomain.com  →  mail.agent.mydomain.com  (priority 10)
  A     mail.agent.mydomain.com  →  203.0.113.5
  TXT   agent.mydomain.com  →  "v=spf1 ip4:203.0.113.5 -all"
  TXT   dkim._domainkey.agent.mydomain.com  →  "v=DKIM1; k=rsa; p=MIIBIj..."
  TXT   _dmarc.agent.mydomain.com  →  "v=DMARC1; p=reject"
  PTR   203.0.113.5  →  mail.agent.mydomain.com  (set via VPS provider panel)

  Press Enter when DNS records are added...

Step 4/6: Verifying DNS
  MX record...   ✓ found
  A record...    ✓ resolves to 203.0.113.5
  SPF record...  ✓ valid
  DKIM record... ✓ valid
  DMARC record.. ✓ valid

Step 5/6: End-to-end verification
  Sending test to verify@aimx.email... ✓ delivered, DKIM pass
  Receiving test from verify@aimx.email... ✓ received, saved to .md

Step 6/6: Creating default mailbox
  Created: catchall@agent.mydomain.com

  ✓ agent@agent.mydomain.com is ready!

  Whitelist your agent in Gmail (recommended):
    1. Open Gmail → Settings → Filters
    2. Create filter: From @agent.mydomain.com
    3. Check "Never send to Spam"

  MCP config for Claude Code (~/.claude/settings.json):
  {
    "mcpServers": {
      "email": {
        "command": "aimx",
        "args": ["mcp"]
      }
    }
  }
```

### Preflight: Port 25 check

Outbound is checked locally by connecting to a well-known MX (e.g., `gmail-smtp-in.l.google.com:25`).

Inbound cannot be verified from inside the server. The `aimx` project runs a lightweight probe service at `check.aimx.email` that connects back to the user's IP on port 25 during preflight. The probe service code is open source and self-hostable.

If port 25 is blocked (inbound or outbound), setup stops with a clear message:

```
✗ Port 25 is blocked by your provider.

aimx requires direct SMTP access (port 25 inbound + outbound).
Most dedicated servers and established VPS accounts support this.

Common providers:
  Hetzner Cloud  — request unblock after first paid invoice
  OVH / Kimsufi — open by default
  Vultr          — unblock on request
  BuyVM          — open by default
```

No relay mode. No fallbacks. Fix the infrastructure or use a compatible provider.

Also potentially link it to the website if user wants with a referral code.


## Email Format (.md)

Incoming `.eml` is parsed and stored as Markdown with YAML frontmatter:

```markdown
---
id: "msg-2026-04-09-001"
message_id: "<abc123@gmail.com>"
from: "alice@example.com"
to: "schedule@agent.mydomain.com"
subject: "Meeting next Thursday"
date: "2026-04-09T14:32:00+08:00"
in_reply_to: null
references: []
attachments:
  - filename: "agenda.pdf"
    content_type: "application/pdf"
    size: 45230
    path: "attachments/agenda.pdf"
mailbox: "schedule"
read: false
---

Hi, can we schedule a meeting next Thursday at 2pm?
I've attached the agenda.

Thanks,
Alice
```

Attachments are extracted to a sibling `attachments/` directory within the mailbox folder. The `.md` file references them by relative path.

This format is designed to be agent-readable without any parsing libraries. An agent can `cat` the file and immediately understand the email.


## Mailboxes

Mailboxes map email addresses to directories. Each mailbox is an independent channel that can have its own trigger rules.

### MCP tools

```
mailbox_create(name: string)
  Creates name@agent.mydomain.com
  Creates /var/lib/aimx/name/
  No mail server restart required.

mailbox_list()
  Returns all mailboxes with message counts.

mailbox_delete(name: string)
  Removes the mailbox. Requires confirmation.

email_list(mailbox: string, filters?: { unread?, from?, since?, subject? })
  Lists emails in a mailbox. Returns frontmatter only, not body.

email_read(mailbox: string, id: string)
  Returns full .md content of an email.

email_send(from_mailbox: string, to: string, subject: string, body: string, attachments?: string[])
  Composes .eml, signs with DKIM, hands to OpenSMTPD for delivery.

email_reply(mailbox: string, id: string, body: string)
  Replies to an email. Sets In-Reply-To and References headers for proper threading.

email_mark_read(mailbox: string, id: string)
email_mark_unread(mailbox: string, id: string)
```

### On disk

```
/var/lib/aimx/
├── config.yaml               # mailbox definitions + channel rules
├── schedule/
│   ├── 2026-04-09-001.md
│   ├── 2026-04-09-002.md
│   └── attachments/
├── family/
│   └── ...
├── accounting/
│   └── ...
└── catchall/                  # default mailbox for unmatched addresses
    └── ...
```

OpenSMTPD routes all addresses at the domain to a single delivery command. The delivery script reads the RCPT TO local part and drops the `.md` into the correct mailbox directory. Unrecognized addresses go to `catchall`.

```
# /etc/smtpd.conf (auto-generated by aimx setup)
pki mail.agent.mydomain.com cert "/etc/ssl/aimx/cert.pem"
pki mail.agent.mydomain.com key "/etc/ssl/aimx/key.pem"

listen on 0.0.0.0 tls pki mail.agent.mydomain.com
action "deliver" mda "/usr/local/bin/aimx ingest %{rcpt}"
match from any for domain "agent.mydomain.com" action "deliver"
```


## Channel Manager

The channel manager triggers actions when emails arrive at specific mailboxes. Rules are defined in `config.yaml`.

### Configuration

```yaml
domain: agent.mydomain.com

mailboxes:
  schedule:
    address: schedule@agent.mydomain.com
    on_receive:
      - type: cmd
        command: 'claude -p "Handle this scheduling request: $(cat {filepath})"'
  accounting:
    address: accounting@agent.mydomain.com
    on_receive:
      - type: cmd
        command: 'claude -p "Process this invoice: $(cat {filepath})"'
        match:
          has_attachment: true
      - type: cmd
        command: 'claude -p "Handle this accounting email: $(cat {filepath})"'

  family:
    address: family@agent.mydomain.com
    on_receive:
      - type: cmd
        command: 'curl -X POST http://localhost:3000/api/family-inbox -d @{filepath}'

  catchall:
    address: "*@agent.mydomain.com"
    on_receive:
      - type: cmd
        command: 'ntfy pub agent-mail "New email: {subject} from {from}"'
```

### Supported trigger types

- **cmd** — Execute a shell command. Template variables: `{filepath}`, `{from}`, `{to}`, `{subject}`, `{mailbox}`, `{id}`, `{date}`.

### Inbound trust

Channel triggers execute shell commands on incoming mail. Without verification, anyone can email your agent and trigger actions. Trust policies gate trigger execution on sender authenticity.

During `aimx ingest`, verify the sender's DKIM signature and SPF record using the `mail-auth` crate (same library used for outbound signing). Store the result in frontmatter (`dkim: pass|fail|none`, `spf: pass|fail|none`).

Per-mailbox trust policy in `config.yaml`:

```yaml
schedule:
  trust: verified          # only trigger on DKIM-pass emails
  trusted_senders:         # optional allowlist (always trigger, skip verification)
    - "*@company.com"
    - "alice@gmail.com"
```

Trust modes:
- `none` (default) — all triggers fire regardless of verification result.
- `verified` — triggers only fire on DKIM-pass emails, unless sender matches `trusted_senders`.

Mail is always stored regardless of trust status. Trust only gates trigger execution.

### Match filters (optional)

Triggers can be conditionally filtered:

```yaml
match:
  from: "*@company.com"
  subject: "invoice"
  has_attachment: true
```

All conditions must match (AND logic). If no `match` is specified, the trigger fires on every email to that mailbox.

### Execution

Triggers run synchronously during mail delivery. The delivery pipeline is:

1. OpenSMTPD receives mail, calls `aimx ingest`
2. Parse `.eml` → save `.md` + extract attachments
3. Determine mailbox from RCPT TO
4. Run matching channel rules for that mailbox
5. Exit (OpenSMTPD marks delivery complete)

Trigger failures are logged but do not block delivery. The `.md` is always saved regardless of whether triggers succeed.


## Verify Service

`aimx` includes a hosted verification service at `verify@aimx.email` that provides:

1. **Preflight port check** — During `aimx setup`, the CLI asks `check.aimx.email` to probe the user's IP on port 25 to confirm inbound reachability.
2. **End-to-end delivery test** — After setup, the CLI sends a test email to `verify@aimx.email` and waits for a reply. This confirms outbound delivery, DKIM signing, and inbound reception all work.

The verify service is:
- A lightweight Cloudflare Worker (or equivalent) hosted by the project
- Open source and self-hostable for users who prefer not to use the public instance
- Used only during setup, never during normal operation


## CLI Reference

```
aimx setup <domain>       Interactive setup wizard
aimx preflight            Run port/DNS checks without installing
aimx ingest <rcpt>       Delivery command (called by OpenSMTPD, not user-facing)
aimx send <args>          Compose, DKIM-sign, and send an email
aimx mcp                  Start MCP server in stdio mode (for Claude Code)
aimx mailbox create <n>   Create a new mailbox
aimx mailbox list         List all mailboxes
aimx mailbox delete <n>   Delete a mailbox
aimx status               Show server status, mailbox counts, recent activity
aimx verify               Run end-to-end verification against verify service
```


## Tech Stack and Dependencies

Written in Rust. Single binary. Minimal dependencies.

| Component | Role | License |
|---|---|---|
| **OpenSMTPD** | SMTP transport (send/receive) | ISC (MIT-equivalent) |
| **aimx** (this project) | Delivery, .md parsing, MCP, channel manager, DKIM signing, CLI | TBD (permissive) |

### Why OpenSMTPD

- ISC license — no copyleft, no restrictions on commercial or multi-tenant use
- `apt install opensmtpd` — already in Debian/Ubuntu repos
- Does exactly one thing: SMTP transport
- Native `mda` action pipes mail directly to our delivery command
- Battle-tested (OpenBSD project, ~10 years in production)

### Why not alternatives

- **Stalwart** — AGPL + proprietary enterprise license. Multi-tenancy is enterprise-only. Blocks future hosted offering.
- **Maddy** — GPLv3. Copyleft propagation complicates commercial distribution.
- **Postfix + Dovecot** — Two packages, more complex config, IMAP not needed.

### DKIM signing

Handled natively in Rust within `aimx send`, rather than depending on `opensmtpd-filter-dkimsign` (which has a less permissive license and adds an external process). The Rust ecosystem has mature DKIM libraries.

### License

The project license must be fully permissive (MIT or Apache-2.0) to allow:
- Self-hosted use without restrictions
- Commercial hosted offerings
- Multi-tenant deployments
- Redistribution and modification

All dependencies must also carry permissive licenses (MIT, ISC, Apache-2.0, BSD). No AGPL, GPL, or copyleft dependencies.


## Compatibility

### Supported VPS providers

Any provider that allows inbound and outbound traffic on port 25.

| Provider | Port 25 | Notes |
|---|---|---|
| Hetzner Cloud | After unblock request | Wait 1 month + first invoice, then request via support |
| Hetzner Dedicated | Open by default | |
| OVH / Kimsufi | Open by default | Has outbound anti-spam filter |
| Vultr | Unblockable on request | |
| BuyVM | Open by default | Mail-friendly provider |
| Any dedicated/bare metal | Generally open | |

### Not supported

| Provider | Reason |
|---|---|
| DigitalOcean | Permanently blocks SMTP, recommends against self-hosted mail |
| AWS EC2 | Permanently blocks port 25 since 2020 |
| Azure VMs | Blocked on VMs created after Nov 2017 |
| GCP | Blocked by default, rare exceptions |

### Supported agent frameworks

- **Claude Code** — MCP stdio mode. Add to `~/.claude/settings.json`.
- **OpenClaw** — MCP integration or channel manager triggers via shell.
- **Codex** — Channel manager triggers via shell command.
- **Any MCP-compatible client** — Standard MCP stdio transport.


## Future Considerations

### Multi-tenant hosted offering (out of scope for now)

The license and architecture are designed to not preclude a future hosted service where users get agent mailboxes without managing their own server. The permissive license on all components ensures this path remains open. Multi-tenant would involve running shared OpenSMTPD instances with per-user mailbox isolation, domain management, and a web dashboard.

