# aimx

> You give your agents an entire server. Why borrow someone else's inbox?

**SMTP for agents. No middleman.**

One command to give your AI agents their own email addresses -- no Gmail, no OAuth, no third-party SaaS. Built for Claude Code, OpenClaw, Codex, and any agentic system that needs an email channel.

```bash
aimx setup agent.mydomain.com
```

Done. Incoming mail is parsed to Markdown. Outbound mail is DKIM-signed. MCP is built in. Channel rules trigger agent actions on incoming mail.

## How it works

```
Inbound:
  Sender -> port 25 -> OpenSMTPD -> aimx ingest -> .md file
                                                 -> channel manager (triggers agent)

Outbound:
  MCP tool call -> aimx send -> DKIM sign -> OpenSMTPD -> remote MX
```

- **No daemon.** OpenSMTPD is the only long-running process. `aimx` commands are short-lived.
- **No IMAP/POP3.** Agents read `.md` files via MCP or filesystem.
- **Markdown-first.** Emails stored as Markdown with TOML frontmatter -- agents can `cat` and understand immediately.
- **Single binary.** Written in Rust. No runtime dependencies beyond OpenSMTPD.

## Features

- **Setup wizard** -- preflight checks, OpenSMTPD config, DKIM keygen, DNS guidance, verification
- **Email delivery** -- EML to Markdown with TOML frontmatter, attachment extraction, mailbox routing
- **Email sending** -- RFC 5322 composition, DKIM signing (RSA-SHA256), threading support, attachments
- **MCP server** -- stdio transport for Claude Code and any MCP client: list, read, send, reply, manage mailboxes
- **Channel manager** -- trigger shell commands on incoming mail with match filters (from, subject, attachments)
- **Inbound trust** -- DKIM/SPF verification, per-mailbox trust policies, trusted sender allowlists
- **Verify service** -- self-hostable port probe and port 25 listener for setup verification

## Requirements

- Linux (Debian/Ubuntu)
- A VPS with port 25 open (inbound and outbound)
- A domain you control
- Rust toolchain (for building from source)

### Compatible VPS providers

| Provider | Port 25 | Notes |
|----------|---------|-------|
| Hetzner Cloud | After unblock request | Request via support after first invoice |
| OVH / Kimsufi | Open by default | |
| Vultr | Unblockable on request | |
| BuyVM (Frantech) | Open by default | |
| Linode / Akamai | On request | Submit support ticket |

## Quick start

```bash
# 1. Build and install
cargo install --path .
sudo cp target/release/aimx /usr/local/bin/

# 2. Run setup (handles everything: preflight, OpenSMTPD, DKIM, DNS)
sudo aimx setup agent.yourdomain.com

# 3. Follow the interactive prompts to add DNS records
# 4. Verify the setup
aimx verify

# 5. Check status
aimx status
```

## Installation

```bash
git clone https://github.com/uzyn/aimx.git
cd aimx
cargo build --release
sudo cp target/release/aimx /usr/local/bin/
```

## Usage

### Setup

```bash
# Full interactive setup (installs OpenSMTPD, generates DKIM keys, guides DNS)
sudo aimx setup agent.yourdomain.com

# Run preflight checks only (port 25, PTR record)
aimx preflight

# Check server status
aimx status

# Check port 25 connectivity
aimx verify
```

### Mailbox management

```bash
# Create a mailbox
aimx mailbox create support

# List mailboxes with message counts
aimx mailbox list

# Delete a mailbox (with confirmation)
aimx mailbox delete support
```

### Sending email

```bash
# Send an email
aimx send --from support@agent.yourdomain.com \
          --to recipient@gmail.com \
          --subject "Hello" \
          --body "Message body"

# Send with attachments
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
```

### MCP server (for AI agents)

```bash
# Start MCP server (stdio mode, launched by MCP client)
aimx mcp
```

Add to Claude Code (`~/.claude/settings.json`):

```json
{
  "mcpServers": {
    "email": {
      "command": "/usr/local/bin/aimx",
      "args": ["mcp"]
    }
  }
}
```

Available MCP tools:
- `mailbox_list` -- list all mailboxes with message counts
- `mailbox_create` -- create a new mailbox
- `mailbox_delete` -- delete a mailbox
- `email_list` -- list emails with optional filters (unread, from, since, subject)
- `email_read` -- read full email content
- `email_send` -- compose and send an email
- `email_reply` -- reply to an email with correct threading
- `email_mark_read` -- mark an email as read
- `email_mark_unread` -- mark an email as unread

### DKIM key management

```bash
# Generate DKIM keypair
aimx dkim-keygen

# Force regenerate (overwrites existing)
aimx dkim-keygen --force

# Custom selector
aimx dkim-keygen --selector mykey
```

## Configuration

Configuration is stored in `config.toml` in the data directory (default: `/var/lib/aimx/`).

### config.toml reference

```toml
# Domain for this email server (required)
domain = "agent.yourdomain.com"

# Data directory (default: /var/lib/aimx)
data_dir = "/var/lib/aimx"

# DKIM selector name (default: dkim)
dkim_selector = "dkim"

# Catchall mailbox (receives all unmatched addresses)
[mailboxes.catchall]
address = "*@agent.yourdomain.com"

# Named mailbox
[mailboxes.support]
address = "support@agent.yourdomain.com"

# Trust policy (default: none)
# none: all triggers fire regardless of DKIM/SPF
# verified: triggers only fire on DKIM-pass emails
trust = "verified"

# Trusted senders bypass DKIM verification (glob patterns)
trusted_senders = ["*@company.com", "boss@gmail.com"]

# Channel rules: trigger commands on incoming email
[[mailboxes.support.on_receive]]
type = "cmd"
command = 'echo "New email from {from}: {subject}" >> /tmp/email.log'

[[mailboxes.support.on_receive]]
type = "cmd"
command = 'ntfy pub my-topic "Email from {from}: {subject}"'

[mailboxes.support.on_receive.match]
from = "*@gmail.com"        # Glob pattern on sender
subject = "urgent"           # Substring match (case-insensitive)
has_attachment = true        # Filter on attachment presence
```

### Channel manager template variables

Available in `on_receive` command templates:

| Variable | Description |
|----------|-------------|
| `{filepath}` | Full path to the saved `.md` file |
| `{from}` | Sender email address |
| `{to}` | Recipient email address |
| `{subject}` | Email subject |
| `{mailbox}` | Mailbox name |
| `{id}` | Email ID (e.g., `2025-01-15-001`) |
| `{date}` | Email date |

### Trust policy

Trust policies control whether channel triggers fire based on sender authentication:

- **`trust: none`** (default) -- triggers fire for all incoming emails
- **`trust: verified`** -- triggers only fire when the sender's DKIM signature passes verification

The `trusted_senders` list allows specific senders to bypass DKIM verification. Supports glob patterns.

Email is always stored regardless of trust result. Trust only gates trigger execution.

## Storage layout

```
/var/lib/aimx/
├── config.toml
├── dkim/
│   ├── private.key
│   └── public.key
├── catchall/
│   ├── 2025-01-15-001.md
│   ├── 2025-01-15-002.md
│   └── attachments/
│       └── document.pdf
└── support/
    └── ...
```

### Email format

Emails are stored as Markdown with TOML frontmatter:

```markdown
+++
id = "2025-01-15-001"
message_id = "<abc123@example.com>"
from = "Alice <alice@example.com>"
to = "support@agent.yourdomain.com"
subject = "Hello"
date = "2025-01-15T10:30:00Z"
in_reply_to = ""
references = ""
attachments = []
mailbox = "support"
read = false
dkim = "pass"
spf = "pass"
+++

Hello, this is the email body in plain text.
```

## Verify service

The verify service (`services/verify/`) is a separate deployable service that provides:

1. **Port probe** at `check.aimx.email` -- performs EHLO handshake back to caller's IP on port 25 to verify inbound SMTP reachability
2. **Port 25 listener** at `check.aimx.email:25` -- accepts TCP connections so aimx clients can test outbound port 25 reachability

No MTA is required on the verify server. The service is open source and self-hostable. See `services/verify/README.md` for deployment instructions.

## DNS records

`aimx setup` will guide you through DNS configuration. The required records are:

| Type | Name | Value |
|------|------|-------|
| A | agent.yourdomain.com | Your server IP |
| MX | agent.yourdomain.com | 10 agent.yourdomain.com. |
| TXT | agent.yourdomain.com | v=spf1 ip4:YOUR_IP -all |
| TXT | dkim._domainkey.agent.yourdomain.com | v=DKIM1; k=rsa; p=... |
| TXT | _dmarc.agent.yourdomain.com | v=DMARC1; p=reject |
| PTR | Your server IP | agent.yourdomain.com. (set at VPS provider) |

## Preventing spam classification

To prevent emails from landing in spam:

1. Ensure all DNS records are correctly set (DKIM, SPF, DMARC)
2. Set a PTR record at your VPS provider
3. In Gmail: Settings > Filters > Create filter for `*@yourdomain.com` > Never send to Spam
4. Alternatively, reply to an email from the domain -- Gmail learns it is not spam

## Data directory override

The default data directory is `/var/lib/aimx`. Override with:

```bash
# CLI flag
aimx --data-dir /custom/path status

# Environment variable
export AIMX_DATA_DIR=/custom/path
aimx status
```

## License

MIT

## Author

[U-Zyn Chua](https://uzyn.com) <chua@uzyn.com>
