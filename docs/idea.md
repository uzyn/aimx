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
cargo build --release && sudo cp target/release/aimx /usr/local/bin/
sudo aimx setup agent.mydomain.com
```

Done. Your agent has an email address. Incoming mail is parsed to Markdown. Outbound mail just works. MCP is built in. Channel rules trigger agent actions on incoming mail.

No Gmail. No OAuth. No third parties. No IMAP clients. Just SMTP, directly on your server.


## Architecture

```
Inbound:
  Sender → port 25 → aimx serve → ingest → .md file
                                         → channel manager (triggers agent)

Outbound:
  MCP tool call → aimx send → UDS (/run/aimx/send.sock) → aimx serve
                                                       → DKIM sign
                                                       → direct SMTP to recipient MX

Storage:
  /var/lib/aimx/
  ├── inbox/
  │   ├── schedule/
  │   │   ├── 2026-04-09-103000-meeting.md           # flat (zero attachments)
  │   │   └── 2026-04-09-110000-agenda/              # Zola-style bundle
  │   │       ├── 2026-04-09-110000-agenda.md
  │   │       └── agenda.pdf
  │   ├── family/
  │   ├── accounting/
  │   └── catchall/                                   # default mailbox
  └── sent/
      └── schedule/
          └── ...
```

### Key design decisions

- **One daemon.** `aimx serve` owns port 25 directly — there is no external MTA. It ingests inbound mail in-process and signs + delivers outbound mail over a world-writable UDS (`/run/aimx/send.sock`). The MCP server runs in stdio mode, launched on-demand by the agent framework.
- **No IMAP/POP3/JMAP.** No mail clients will connect to this. Agents read `.md` files via MCP or filesystem. The mail server's only job is SMTP transport.
- **`.md` first.** Emails are stored as Markdown with TOML frontmatter, not raw `.eml`. Agents can `cat` an email and understand it immediately. Attachments become siblings of the `.md` inside a Zola-style bundle directory.
- **DKIM signing in Rust.** `aimx serve` handles DKIM signing natively (via `mail-auth`) before handing the message to `lettre` for MX delivery. The private key is root-only (`0600`) and never leaves the daemon.
- **Mailboxes are directories.** Creating a mailbox = creating a folder + registering an address via UDS to the running daemon. Inbound routing picks up the change live. No OS users, no passwords, no database.


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
  Installing aimx serve unit (systemd/OpenRC)... done
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

Incoming `.eml` is parsed and stored as Markdown with TOML frontmatter:

```markdown
+++
id = "2026-04-09-143200-meeting-next-thursday"
message_id = "<abc123@gmail.com>"
thread_id = "a1b2c3d4e5f6a7b8"
from = "alice@example.com"
to = "schedule@agent.mydomain.com"
delivered_to = "schedule@agent.mydomain.com"
subject = "Meeting next Thursday"
date = "2026-04-09T14:32:00+08:00"
received_at = "2026-04-09T14:32:01Z"
size_bytes = 2048

[[attachments]]
filename = "agenda.pdf"
content_type = "application/pdf"
size = 45230
path = "agenda.pdf"

dkim = "pass"
spf = "pass"
dmarc = "pass"
trusted = "true"
mailbox = "schedule"
read = false
+++

Hi, can we schedule a meeting next Thursday at 2pm?
I've attached the agenda.

Thanks,
Alice
```

When an email has attachments, the stem becomes a directory (a Zola-style bundle): the `.md` sits inside it alongside every attachment as a sibling. Zero-attachment emails are a flat `.md` file. `attachments[].path` is relative to the bundle directory.

This format is designed to be agent-readable without any parsing libraries. An agent can `cat` the file and immediately understand the email.


## Mailboxes

Mailboxes map email addresses to directories. Each mailbox is an independent channel that can have its own trigger rules.

### MCP tools

```
mailbox_create(name: string)
  Creates name@agent.mydomain.com via UDS to aimx serve.
  Daemon updates config.toml atomically and hot-swaps its in-memory view.
  Inbound routing picks up the new mailbox on the next SMTP session.

mailbox_list()
  Returns all mailboxes with inbound + sent message counts.

mailbox_delete(name: string)
  Removes the mailbox. Requires confirmation.

email_list(mailbox: string, filters?: { unread?, from?, since?, subject?, folder? })
  Lists emails in inbox/ (default) or sent/. Returns frontmatter only, not body.

email_read(mailbox: string, id: string, folder?: "inbox" | "sent")
  Returns full .md content of an email.

email_send(from_mailbox: string, to: string, subject: string, body: string, attachments?: string[])
  Composes .eml, submits to aimx serve over UDS; daemon DKIM-signs and
  delivers directly to the recipient's MX via lettre.

email_reply(mailbox: string, id: string, body: string)
  Replies to an email. Sets In-Reply-To and References headers for proper threading.

email_mark_read(mailbox: string, id: string, folder?: "inbox" | "sent")
email_mark_unread(mailbox: string, id: string, folder?: "inbox" | "sent")
```

### On disk

```
/etc/aimx/
├── config.toml               # mailbox definitions + channel rules (mode 0640)
└── dkim/
    ├── private.key           # root-only (mode 0600)
    └── public.key            # advertised via DNS (mode 0644)

/run/aimx/
└── send.sock                 # world-writable UDS (mode 0666)

/var/lib/aimx/
├── inbox/
│   ├── schedule/
│   ├── family/
│   ├── accounting/
│   └── catchall/             # default mailbox for unmatched addresses
└── sent/
    └── schedule/
```

`aimx serve` accepts every address at the domain on port 25 and ingests in-process, writing the `.md` into the correct `inbox/<mailbox>/` directory based on the mailbox config (RCPT TO local part). Unrecognized addresses go to `catchall` when defined, otherwise the session is rejected.


## Channel Manager

The channel manager triggers actions when emails arrive at specific mailboxes. Rules are defined in `config.toml`.

### Configuration

```toml
domain = "agent.mydomain.com"

[mailboxes.schedule]
address = "schedule@agent.mydomain.com"

[[mailboxes.schedule.on_receive]]
type = "cmd"
command = 'claude -p "Handle this scheduling request: $(cat \"$AIMX_FILEPATH\")"'

[mailboxes.accounting]
address = "accounting@agent.mydomain.com"

[[mailboxes.accounting.on_receive]]
type = "cmd"
command = 'claude -p "Process this invoice: $(cat \"$AIMX_FILEPATH\")"'

[mailboxes.accounting.on_receive.match]
has_attachment = true

[[mailboxes.accounting.on_receive]]
type = "cmd"
command = 'claude -p "Handle this accounting email: $(cat \"$AIMX_FILEPATH\")"'

[mailboxes.family]
address = "family@agent.mydomain.com"

[[mailboxes.family.on_receive]]
type = "cmd"
command = 'curl -X POST http://localhost:3000/api/family-inbox -d @"$AIMX_FILEPATH"'

[mailboxes.catchall]
address = "*@agent.mydomain.com"

[[mailboxes.catchall.on_receive]]
type = "cmd"
command = 'ntfy pub agent-mail "New email: $AIMX_SUBJECT from $AIMX_FROM"'
```

### Supported trigger types

- **cmd** — Execute a shell command. User-controlled fields from the sender's headers are exposed as environment variables (`$AIMX_FILEPATH`, `$AIMX_FROM`, `$AIMX_TO`, `$AIMX_SUBJECT`, `$AIMX_MAILBOX`). The aimx-controlled `{id}` and `{date}` are substituted directly into the command string. Quote env-var expansions so arbitrary sender-supplied bytes pass through safely.

### Inbound trust

Channel triggers execute shell commands on incoming mail. Without verification, anyone can email your agent and trigger actions. Trust policies gate trigger execution on sender authenticity.

During `aimx ingest`, verify the sender's DKIM signature and SPF record using the `mail-auth` crate (same library used for outbound signing). Store the result in frontmatter (`dkim = "pass|fail|none"`, `spf = "pass|fail|none"`).

Per-mailbox trust policy in `config.toml`:

```toml
[mailboxes.schedule]
trust = "verified"          # only trigger on DKIM-pass emails
# optional allowlist (always trigger, skip verification)
trusted_senders = ["*@company.com", "alice@gmail.com"]
```

Trust modes:
- `none` (default) — all triggers fire regardless of verification result.
- `verified` — triggers only fire on DKIM-pass emails, unless sender matches `trusted_senders`.

Mail is always stored regardless of trust status. Trust only gates trigger execution.

### Match filters (optional)

Triggers can be conditionally filtered:

```toml
[mailboxes.accounting.on_receive.match]
from = "*@company.com"
subject = "invoice"
has_attachment = true
```

All conditions must match (AND logic). If no `match` is specified, the trigger fires on every email to that mailbox.

### Execution

Triggers run synchronously during mail delivery. The delivery pipeline is:

1. `aimx serve` receives mail on port 25 and dispatches ingest in-process
2. Parse `.eml` → save `.md` + extract attachments (flat or Zola-style bundle)
3. Determine mailbox from RCPT TO, fall back to catchall
4. Run matching channel rules for that mailbox
5. SMTP session completes (250 OK)

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
aimx setup <domain>         Interactive setup wizard (requires root)
aimx serve                  SMTP daemon — binds port 25 + /run/aimx/send.sock
aimx ingest <rcpt>         Inbound MIME→.md ingest (called by aimx serve in-process,
                            also usable via stdin for manual reingest)
aimx send <args>            Compose a message and hand it to aimx serve over UDS
aimx mcp                    Start MCP server in stdio mode
aimx mailbox create <n>     Create a new mailbox (via UDS — daemon picks up live)
aimx mailbox list           List all mailboxes
aimx mailbox delete <n>     Delete a mailbox
aimx status                 Show server status, mailbox counts, recent activity
aimx portcheck              Check port 25 connectivity via the verifier service
aimx dkim-keygen            Regenerate the DKIM keypair (requires root)
aimx agent-setup <agent>    Install the aimx plugin/skill into a supported agent
```


## Tech Stack and Dependencies

Written in Rust. Single binary. Zero runtime dependencies outside the standard OS — no external MTA, no package-manager install.

| Component | Role | License |
|---|---|---|
| **AIMX** (this project) | SMTP listener (inbound), DKIM signer + MX delivery (outbound), .md ingest, MCP, channel manager, CLI | MIT |
| `mail-parser` | MIME parsing | Apache-2.0 / MIT |
| `mail-auth` | DKIM / SPF / DMARC | Apache-2.0 / MIT |
| `lettre` | Outbound SMTP client | MIT / Apache-2.0 |
| `hickory-resolver` | MX / A DNS resolution | MIT / Apache-2.0 |
| `tokio-rustls` | STARTTLS on the inbound listener | MIT / Apache-2.0 |
| `rmcp` | MCP server implementation | MIT / Apache-2.0 |

### Why embedded SMTP (no external MTA)

- One binary to build, install, and manage — no `smtpd.conf` / `main.cf` on top of `config.toml`.
- DKIM key never leaves the daemon; no external process reads the private material.
- Cross-platform: works on any Unix where Rust compiles; not tied to OpenBSD / Debian packaging.
- The daemon is the single writer for mailbox state, so MCP writes (mark-read, mailbox CRUD) can route through UDS without granting non-root processes access to root-owned files.

### Why not alternatives

- **Stalwart** — AGPL + proprietary enterprise license. Multi-tenancy is enterprise-only. Blocks future hosted offering.
- **Maddy** — GPLv3. Copyleft propagation complicates commercial distribution.
- **OpenSMTPD / Postfix** — great mail servers, but layering `aimx ingest` + `aimx send` on top of them means two configs, an external DKIM filter process, and another moving part per-OS. Embedding SMTP directly in `aimx serve` removes the seam.

### DKIM signing

Handled natively in Rust inside `aimx serve` (via `mail-auth`), so the private key lives exclusively in the daemon's address space. Outbound submissions arrive over UDS as unsigned RFC 5322 messages; signing and MX delivery happen inside the daemon.

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

`aimx agent-setup <agent>` installs a plugin/skill bundle into each supported agent's standard config location:

- **Claude Code** — `~/.claude/plugins/aimx/` (MCP + skill + references).
- **Codex CLI** — `~/.codex/plugins/aimx/` (plugin auto-discovered on restart).
- **OpenCode** — `~/.config/opencode/skills/aimx/` (paste the printed JSONC block into `opencode.json`).
- **Gemini CLI** — `~/.gemini/skills/aimx/` (merge the printed JSON into `~/.gemini/settings.json`).
- **Goose** — `~/.config/goose/recipes/aimx.yaml` (single YAML recipe).
- **OpenClaw** — `~/.openclaw/skills/aimx/` (run the printed `openclaw mcp set aimx …` command).
- **Any MCP-compatible client** — wire `aimx mcp` manually as an MCP stdio server.

Channel-trigger recipes (email → agent invocation) are documented per-agent in `book/channel-recipes.md` and also cover Aider.


## Future Considerations

### Multi-tenant hosted offering (out of scope for now)

The license and architecture are designed to not preclude a future hosted service where users get agent mailboxes without managing their own server. The permissive license on all components ensures this path remains open. Multi-tenant would involve running shared `aimx serve` instances with per-user mailbox isolation, domain management, and a web dashboard.

