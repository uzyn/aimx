# AIMX – AI Mail Exchange

> You give your agents an entire server. Why borrow someone else's inbox?

**SMTP for agents. No middleman.**

> [!CAUTION]
> **Under heavy development.** This is pre-v1 alpha release. Expect breaking changes in v0 releases. Pin to an exact version if you use it on stable systems.


One command gives your AI agents their own email addresses. No Gmail, no OAuth, no SaaS. Fully self-hosted means full sovereignty.

Mail in as Markdown. Mail out DKIM-signed. MCP built in. Works with any MCP-capable agent -- Claude Code, Codex CLI, OpenCode, Gemini CLI, Goose, OpenClaw, Hermes.


- **Single binary.** One binary, no other dependencies. 
- **Direct MTA-to-MTA.** Email has become send-and-pray best-effort. AIMX turns it back into direct server-to-server delivery -- closer to an API call.
- **Push, not poll.** Inbound mail fires `on_receive` hooks the moment SMTP `DATA` completes. Outbound delivery fires `after_send` hooks when the MX attempt resolves. No cron, no heartbeat.
- **Trust modeling.** Signatures verified on ingest, stamped into the frontmatter. Configurable globally or per mailbox.
- **IPv6-ready.** One flag opts in. IPv4 by default keeps your SPF simple.
- **Markdown-first storage.** No `.eml`, no database. Just Markdown with TOML frontmatter -- LLM and RAG friendly. Your agent can `cat` the mailbox. Your inbox becomes your knowledge base.
- **You own the inbox.** Mail lives on your disk, under your domain. Nothing phones home.
- **Hot-swappable mailboxes.** Agents (or you) create and manage mailboxes. Changes take effect live.
- **Built-in MCP server.** Stdio tools: list, read, send, reply, mark read/unread, mailbox CRUD.
- **One-line agent integration.** `aimx agent-setup` wires AIMX into any supported agent above.
- **MIT licensed.** No license server, no telemetry, no account.

Check [Frequently Asked Questions](book/faq.md) for more.

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

# 3. Check health
aimx doctor
```

### CLI Commands

```text
$ aimx
SMTP for agents. No middleman.

Usage: aimx [OPTIONS] <COMMAND>

Commands:
  ingest       Ingest an email from stdin (called by aimx serve or via stdin)
  send         Compose and send an email
  mailboxes    Manage mailboxes
  hooks        Manage hooks
  mcp          Start MCP server in stdio mode
  setup        Run interactive setup wizard
  uninstall    Uninstall the aimx daemon service (config and data are retained)
  doctor       Show server health, mailbox counts, configuration, DNS verification, and recent logs
  logs         Tail or follow the aimx service log
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

Install AIMX into your agent with one command:

| Agent | Install command | Activation |
|-------|-----------------|------------|
| Claude Code | `aimx agent-setup claude-code` | Restart Claude Code (auto-discovered from `~/.claude/plugins/`). |
| Codex CLI | `aimx agent-setup codex` | Restart Codex CLI (auto-discovered from `~/.codex/plugins/`). |
| OpenCode | `aimx agent-setup opencode` | Paste the printed JSONC block into `opencode.json`, then restart. |
| Gemini CLI | `aimx agent-setup gemini` | Merge the printed JSON block into `~/.gemini/settings.json`, then restart. |
| Goose | `aimx agent-setup goose` | Run `goose run --recipe aimx`. |
| OpenClaw | `aimx agent-setup openclaw` | Run the printed `openclaw mcp set aimx '...'` command, then restart the gateway. |
| Hermes | `aimx agent-setup hermes` | Paste the printed YAML block under `mcp_servers:` in `~/.hermes/config.yaml`, then run `/reload-mcp` inside Hermes. |

Run `aimx agent-setup` (no args) or `aimx agent-setup --list` to print the supported-agent registry. See [`book/agent-integration.md`](book/agent-integration.md) for per-agent activation steps and manual MCP wiring, and [`book/hook-recipes.md`](book/hook-recipes.md) for copy-paste hook recipes covering every supported agent plus Aider.

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



## Configuration

AIMX reads a single TOML file at `/etc/aimx/config.toml`, written by `aimx setup` with mode `0640 root:root`. Top-level settings cover `domain`, `dkim_selector`, and the defaults for `trust` / `trusted_senders`; per-mailbox tables attach addresses, override those defaults, and declare hooks.

See [`book/configuration.md`](book/configuration.md) for the full field reference.


## Trust policy

Every inbound email is checked for DKIM, SPF, and DMARC, and the results are written into the TOML frontmatter alongside a summary `trusted` field (`none`, `true`, or `false`). Trust only gates `on_receive` hook execution — mail is always stored on disk regardless of the outcome, so agents and humans can still read untrusted messages via MCP or the filesystem.

See [`book/hooks.md`](book/hooks.md#trust-gate-on_receive-only) for the gate logic and [`book/configuration.md`](book/configuration.md#inbound-email-verification) for the per-field semantics.


## Hooks

AIMX fires shell commands on two events: `on_receive` (after an inbound email is stored) and `after_send` (after the outbound MX attempt resolves to `delivered` / `failed` / `deferred`). Hooks are declared per mailbox in `config.toml` and fire on every event of their configured type; `on_receive` hooks only fire on trusted mail unless a hook opts in with `dangerously_support_untrusted = true`. Hooks may carry an optional `name`; when omitted, AIMX derives a stable 12-char hex id from `event + cmd` so `aimx hooks list` / `delete` can still target the hook.

```toml
[[mailboxes.support.hooks]]
# name is optional — a stable 12-char hex id is derived from event+cmd if omitted
event = "on_receive"
cmd = 'ntfy pub agent-mail "$AIMX_SUBJECT from $AIMX_FROM"'
dangerously_support_untrusted = true
```

See [`book/hooks.md`](book/hooks.md) for the hook model and [`book/hook-recipes.md`](book/hook-recipes.md) for copy-paste recipes (Claude Code, Codex CLI, OpenCode, Gemini CLI, Goose, OpenClaw, Hermes, Aider).


## Email format

Inbound emails are stored as Markdown with TOML frontmatter. A minimal file looks like:

```markdown
+++
id = "2025-04-15-143022-hello"
from = "Alice <alice@example.com>"
to = "support@agent.yourdomain.com"
subject = "Hello"
date = "2025-04-15T14:30:22Z"
dkim = "pass"
spf = "pass"
trusted = "true"
mailbox = "support"
read = false
+++

Hello, this is the email body in plain text.
```

See [`book/mailboxes.md`](book/mailboxes.md#email-format) for the full field schema, attachment bundles, and outbound sent-copy fields.


## DNS records

`aimx setup` will guide you through DNS configuration. The required records are:

| Type | Name | Value |
|------|------|-------|
| A | agent.yourdomain.com | Your server IP |
| MX | agent.yourdomain.com | 10 agent.yourdomain.com. |
| TXT | agent.yourdomain.com | v=spf1 ip4:YOUR_IP -all |
| TXT | aimx._domainkey.agent.yourdomain.com | v=DKIM1; k=rsa; p=... |
| TXT | _dmarc.agent.yourdomain.com | v=DMARC1; p=reject |


## License

This project is licensed under the MIT License. See the [LICENSE](LICENSE) file for details.

Copyright (c) [U-Zyn Chua](https://uzyn.com).
