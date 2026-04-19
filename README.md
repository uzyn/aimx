# AIMX – AI Mail Exchange

> You give your agents an entire server. Why borrow someone else's inbox?

**SMTP for agents. No middleman.**

One command gives your AI agents their own email addresses. No Gmail, no OAuth, no SaaS. Fully self-hosted means full sovereignty.

Mail in as Markdown. Mail out DKIM-signed. MCP built in. Works with any MCP-capable agent -- Claude Code, Codex CLI, OpenCode, Gemini CLI, Goose, OpenClaw.


- **Single binary.** One binary, no other dependencies. 
- **Direct MTA-to-MTA.** Email has become send-and-pray best-effort. AIMX turns it back into direct server-to-server delivery -- closer to an API call.
- **Push, not poll.** Inbound mail fires channel triggers the moment SMTP `DATA` completes. No cron, no heartbeat.
- **Trust modeling.** Signatures verified on ingest, stamped into the frontmatter. Configurable globally or per mailbox.
- **IPv6-ready.** One flag opts in. IPv4 by default keeps your SPF simple.
- **Markdown-first storage.** No `.eml`, no database. Just Markdown with TOML frontmatter -- LLM and RAG friendly. Your agent can `cat` the mailbox. Your inbox becomes your knowledge base.
- **You own the inbox.** Mail lives on your disk, under your domain. Nothing phones home.
- **Hot-swappable mailboxes.** Agents (or you) create and manage mailboxes. Changes take effect live.
- **Built-in MCP server.** Stdio tools: list, read, send, reply, mark read/unread, mailbox CRUD.
- **One-line agent integration.** `aimx agent-setup` wires AIMX into any supported agent above.
- **MIT licensed.** No license server, no telemetry, no account.

## Requirements

- A Linux server (VPS) with port 25 open (inbound and outbound)
- A domain or subdomain you control

## Quick start

You need `sudo` rights as port 25 is a [privileged port](https://www.w3.org/Daemon/User/Installation/PrivilegedPorts.html).

```bash
# 1. Build and install the binary into /usr/local/bin
git clone https://github.com/uzyn/aimx.git
cd aimx
cargo build --release
sudo cp target/release/aimx /usr/local/bin/

# 2. Run setup and follow the guided instructions
sudo aimx setup

# 3. Check status
aimx status
```

### CLI Commands

```
$ aimx
SMTP for agents. No middleman.

Usage: aimx [OPTIONS] <COMMAND>

Commands:
  ingest       Ingest an email from stdin (called by aimx serve or via stdin)
  send         Compose and send an email
  mailbox      Manage mailboxes
  mcp          Start MCP server in stdio mode
  setup        Run interactive setup wizard
  uninstall    Uninstall the aimx daemon service (config and data are retained)
  status       Show server status, mailbox counts, configuration, and DNS record verification
  serve        Start the embedded SMTP listener daemon
  portcheck    Check port 25 connectivity (outbound, inbound)
  agent-setup  Install AIMX plugin/skill for an AI agent into the current user's config
  dkim-keygen  Generate DKIM keypair for email signing
  help         Print this message or the help of the given subcommand(s)

Options:
      --data-dir <DATA_DIR>  Data directory override (default: /var/lib/aimx) [env: AIMX_DATA_DIR=]
  -h, --help                 Print help (see more with '--help')
  -V, --version              Print version
```

### MCP server (for AI agents)

```bash
# Start MCP server (stdio mode, launched by MCP client)
aimx mcp
```

Install AIMX into your agent with one command:

| Agent | Install command | Activation |
|-------|-----------------|------------|
| Claude Code | `aimx agent-setup claude-code` | Restart Claude Code (auto-discovered from `~/.claude/plugins/`). |
| Codex CLI | `aimx agent-setup codex` | Restart Codex CLI (auto-discovered from `~/.codex/plugins/`). |
| OpenCode | `aimx agent-setup opencode` | Paste the printed JSONC block into `opencode.json`, then restart. |
| Gemini CLI | `aimx agent-setup gemini` | Merge the printed JSON block into `~/.gemini/settings.json`, then restart. |
| Goose | `aimx agent-setup goose` | Run `goose run --recipe aimx`. |
| OpenClaw | `aimx agent-setup openclaw` | Run the printed `openclaw mcp set aimx '...'` command, then restart the gateway. |

Run `aimx agent-setup --list` to see every supported agent and its
destination path. See [`book/agent-integration.md`](book/agent-integration.md)
for per-agent activation steps and manual MCP wiring, and
[`book/channel-recipes.md`](book/channel-recipes.md) for copy-paste
channel-trigger recipes (email-driven agent invocation) covering every
supported agent plus Aider.

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

DKIM keys live at `/etc/aimx/dkim/{private,public}.key`. The private key is `0600` (root-only); the public key is `0644` (advertised via DNS). `aimx dkim-keygen` writes to that directory, so it must be invoked with `sudo`:

```bash
# Generate DKIM keypair (requires root)
sudo aimx dkim-keygen

# Force regenerate (overwrites existing)
sudo aimx dkim-keygen --force

# Custom selector
sudo aimx dkim-keygen --selector mykey
```

For tests or dev loops that need to run without root, set `AIMX_CONFIG_DIR` to a writable location first (e.g. `AIMX_CONFIG_DIR=/tmp/aimx-dev aimx dkim-keygen`).

## Configuration

Configuration lives at `/etc/aimx/config.toml` (mode `0640`, owner `root:root`). It is created by `aimx setup` and is read by every `aimx` command. The DKIM keypair sits beside it under `/etc/aimx/dkim/`. The **data directory** (`/var/lib/aimx/` by default) holds only mailbox storage.

Two environment overrides exist, and they are independent:

- `--data-dir <PATH>` / `AIMX_DATA_DIR=<PATH>` — relocate the storage directory (`/var/lib/aimx/`). Useful for unusual deployments or for running multiple instances side-by-side.
- `AIMX_CONFIG_DIR=<PATH>` — relocate the config directory (`/etc/aimx/`, which contains `config.toml` and `dkim/`). Intended for tests and dev loops that need to run without root.

Under a normal install you don't need either — `aimx setup` writes to `/etc/aimx/` and every command picks it up from there.

### config.toml reference

```toml
# Domain for this email server (required)
domain = "agent.yourdomain.com"

# Storage directory for mailboxes (default: /var/lib/aimx).
# Config and DKIM keys live under /etc/aimx/ separately and are NOT
# governed by this setting — see the Configuration section above.
data_dir = "/var/lib/aimx"

# DKIM selector name (default: dkim)
dkim_selector = "dkim"

# Verifier service base URL (default: https://check.aimx.email)
# Used by `aimx portcheck` and `aimx setup`. Set this only if
# you are self-hosting the verifier service (see `services/verifier/`). aimx appends
# `/probe` to this base URL internally.
# verify_host = "https://verify.yourdomain.com"

# Advanced: opt into IPv6 outbound delivery. Default false — outbound goes
# over IPv4 only, matching the default SPF record. See book/configuration.md
# for the extra AAAA + `ip6:` SPF records you need when enabling this.
# enable_ipv6 = true

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
command = 'echo "New email from $AIMX_FROM: $AIMX_SUBJECT" >> /tmp/email.log'

[[mailboxes.support.on_receive]]
type = "cmd"
command = 'ntfy pub my-topic "Email from $AIMX_FROM: $AIMX_SUBJECT"'

[mailboxes.support.on_receive.match]
from = "*@gmail.com"        # Glob pattern on sender
subject = "urgent"           # Substring match (case-insensitive)
has_attachment = true        # Filter on attachment presence
```

### Channel manager variables

User-controlled fields (from the sender's headers) are exposed as **environment variables** so they're safe to quote inside the command string. Always expand them inside double quotes (`"$AIMX_SUBJECT"`) — `sh -c` preserves arbitrary bytes under env-var expansion.

| Environment variable | Description |
|----------------------|-------------|
| `AIMX_FILEPATH` | Full path to the saved `.md` file (e.g., `/var/lib/aimx/inbox/support/2025-04-15-103000-hello.md`) |
| `AIMX_FROM` | Sender email address (may include display name) |
| `AIMX_TO` | Recipient email address |
| `AIMX_SUBJECT` | Email subject |
| `AIMX_MAILBOX` | Mailbox name |

Two aimx-controlled fields are also substituted as **template placeholders** directly into the command string (safe because aimx controls the content):

| Placeholder | Description |
|-------------|-------------|
| `{id}` | Email ID / filename stem (e.g., `2025-04-15-103000-hello`) |
| `{date}` | Email date |

### Trust policy

Trust policies control whether channel triggers fire based on sender authentication:

- **`trust: none`** (default) -- triggers fire for all incoming emails
- **`trust: verified`** -- triggers only fire when the sender's DKIM signature passes verification

The `trusted_senders` list allows specific senders to bypass DKIM verification. Supports glob patterns.

Every inbound email records the evaluated result in a `trusted` frontmatter field so agents can act on it without re-reading config:

| Value | Meaning |
|-------|---------|
| `"none"` | Mailbox `trust` is `none` (default) -- no evaluation performed. |
| `"true"` | Mailbox `trust` is `verified`, sender matches `trusted_senders`, AND DKIM passed. |
| `"false"` | Mailbox `trust` is `verified`, any other outcome. |

Email is always stored regardless of trust result. Trust only gates trigger execution.

## Storage layout

```
/etc/aimx/                       # Config + secrets (root-owned, mode 0755)
├── config.toml                  # Configuration (mode 0640, root:root)
└── dkim/
    ├── private.key              # RSA private key (mode 0600, root-only)
    └── public.key               # RSA public key (mode 0644, advertised via DNS)

/run/aimx/                       # Runtime dir (mode 0755, root:root)
└── send.sock                    # World-writable UDS for aimx send (mode 0666)

/var/lib/aimx/                   # Mailbox storage (world-readable by design)
├── inbox/                       # inbound mail
│   ├── catchall/                # default mailbox
│   │   ├── 2025-04-15-143022-hello.md                  # flat: zero attachments
│   │   └── 2025-04-15-153300-invoice-march/            # Zola-style bundle
│   │       ├── 2025-04-15-153300-invoice-march.md
│   │       ├── invoice.pdf
│   │       └── receipt.png
│   └── support/
│       └── ...
└── sent/                        # outbound sent copies
    └── support/
        └── ...
```

Filenames use `YYYY-MM-DD-HHMMSS-<slug>.md` in UTC. Zero attachments produce a flat `.md` file; one or more attachments produce a bundle directory of the same stem with the `.md` and every attachment as siblings (the old `attachments/` subdirectory is gone).

The DKIM private key is `0600` and readable only by root (`aimx serve` loads it at startup and signs in-process). The public key and the datadir are world-readable; AIMX treats DKIM secret isolation as the security boundary, not filesystem ACLs on the mailbox tree.

### Email format

Inbound emails are stored as Markdown with TOML frontmatter:

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
received_from_ip = "203.0.113.10"
size_bytes = 1024
dkim = "pass"
spf = "pass"
dmarc = "pass"
trusted = "true"
mailbox = "support"
read = false
labels = []
+++

Hello, this is the email body in plain text.
```

Optional fields (`cc`, `reply_to`, `in_reply_to`, `references`, `list_id`, `auto_submitted`, `received_from_ip`) are omitted when empty rather than written as empty strings. Sent copies (under `/var/lib/aimx/sent/<mailbox>/`) add an outbound block with `outbound`, `delivery_status`, `bcc`, `delivered_at`, and `delivery_details`. See [`book/mailboxes.md`](book/mailboxes.md) for the full field reference and outbound schema.

## Verifier service

The verifier service (`services/verifier/`) is a separate deployable service that provides:

1. **Port probe** at `check.aimx.email` -- performs EHLO handshake back to caller's IP on port 25 to verify inbound SMTP reachability
2. **Port 25 listener** at `check.aimx.email:25` -- accepts TCP connections so AIMX clients can test outbound port 25 reachability

No MTA is required on the verifier server. The service is open source and self-hostable. See `services/verifier/README.md` for deployment instructions.

To point AIMX at a self-hosted instance, set `verify_host` in `config.toml`:

```toml
verify_host = "https://verify.yourdomain.com"
```

Or override it per-invocation with the `--verify-host` flag, which is accepted by `aimx portcheck` and `aimx setup`:

```bash
sudo aimx portcheck --verify-host https://verify.yourdomain.com
```

Precedence is **CLI flag > config > default** (`https://check.aimx.email`).

## DNS records

`aimx setup` will guide you through DNS configuration. The required records are:

| Type | Name | Value |
|------|------|-------|
| A | agent.yourdomain.com | Your server IP |
| MX | agent.yourdomain.com | 10 agent.yourdomain.com. |
| TXT | agent.yourdomain.com | v=spf1 ip4:YOUR_IP -all |
| TXT | dkim._domainkey.agent.yourdomain.com | v=DKIM1; k=rsa; p=... |
| TXT | _dmarc.agent.yourdomain.com | v=DMARC1; p=reject |

Reverse DNS (PTR) is configured at your VPS provider's control panel. Setting a correct PTR record improves deliverability but is the operator's responsibility and is out of scope for `aimx setup`.

## Preventing spam classification

To prevent emails from landing in spam:

1. Ensure all DNS records are correctly set (DKIM, SPF, DMARC).
2. (Optional but recommended) Configure a PTR / reverse-DNS record at your VPS provider pointing to your domain.
3. In Gmail: Settings > Filters > Create filter for `*@yourdomain.com` > Never send to Spam.
4. Alternatively, reply to an email from the domain -- Gmail learns it is not spam.

## Directory overrides

AIMX splits its filesystem footprint across two roots, each with its own override:

- **Storage** (`/var/lib/aimx/`) — mailboxes, inbound and sent. Override with `--data-dir <PATH>` or `AIMX_DATA_DIR=<PATH>`.
- **Config + DKIM** (`/etc/aimx/`) — `config.toml` plus the DKIM keypair. Override with `AIMX_CONFIG_DIR=<PATH>`.

```bash
# Storage override (CLI flag — wins over env var)
aimx --data-dir /custom/storage status

# Storage override (env var)
export AIMX_DATA_DIR=/custom/storage
aimx status

# Config + DKIM override (tests and dev loops that can't run as root)
export AIMX_CONFIG_DIR=/tmp/aimx-dev
aimx dkim-keygen
```

Under a normal install you won't need either — `aimx setup` writes to the canonical locations and everything picks them up from there.

## License

This project is licensed under the MIT License. See the [LICENSE](LICENSE) file for details.

Copyright (c) [U-Zyn Chua](https://uzyn.com).
