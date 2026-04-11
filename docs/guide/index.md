# aimx User Guide

> Your server can receive email. Why are you routing through Gmail?

**SMTP for agents. No middleman.**

aimx is a self-hosted email system for AI agents. One command gives your agents their own email addresses on a domain you control -- no Gmail, no OAuth, no third-party SaaS. Built for Claude Code, OpenClaw, Codex, and any agentic system that needs an email channel.

## How it works

```
Inbound:
  Sender -> port 25 -> OpenSMTPD -> aimx ingest -> .md file
                                                 -> channel manager (triggers agent)

Outbound:
  MCP tool call -> aimx send -> DKIM sign -> OpenSMTPD -> remote MX
```

- **No daemon.** OpenSMTPD is the only long-running process. `aimx` commands are short-lived.
- **No IMAP/POP3.** Agents read `.md` files via MCP or the filesystem.
- **Markdown-first.** Emails are stored as Markdown with TOML frontmatter -- agents can `cat` and understand immediately.
- **Single binary.** Written in Rust. No runtime dependencies beyond OpenSMTPD.

## Key capabilities

- **[Setup wizard](setup.md)** -- preflight checks, OpenSMTPD configuration, DKIM key generation, DNS guidance, end-to-end verification
- **[Markdown email](mailboxes.md)** -- incoming email parsed to Markdown with TOML frontmatter, attachment extraction, per-mailbox routing
- **[MCP server](mcp.md)** -- stdio transport for Claude Code and any MCP client: list, read, send, reply, manage mailboxes
- **[Channel rules](channels.md)** -- trigger shell commands on incoming mail with match filters (from, subject, attachments)
- **[DKIM signing](setup.md#dkim-key-management)** -- native RSA-SHA256 signing for outbound mail
- **[Inbound trust](channels.md#trust-policies)** -- DKIM/SPF verification, per-mailbox trust policies, trusted sender allowlists

## Quick start

```bash
# 1. Build and install
git clone https://github.com/uzyn/aimx.git && cd aimx
cargo build --release
sudo cp target/release/aimx /usr/local/bin/

# 2. Run setup (handles OpenSMTPD, DKIM, DNS guidance)
sudo aimx setup agent.yourdomain.com

# 3. Verify
aimx verify
```

See the full [Getting Started](getting-started.md) guide for details.

## Guide contents

| Page | Description |
|------|-------------|
| [Getting Started](getting-started.md) | Requirements, installation, first setup in 5 minutes |
| [Setup](setup.md) | Detailed server setup, DNS, verification, DKIM keys, production hardening |
| [Configuration](configuration.md) | `config.toml` reference, data directory, environment variables |
| [Mailboxes & Email](mailboxes.md) | Mailbox management, email format, sending, attachments, threading |
| [Channel Rules & Trust](channels.md) | Trigger commands on incoming mail, match filters, trust policies |
| [MCP Server](mcp.md) | Agent integration via Model Context Protocol, all 9 MCP tools |
| [Troubleshooting](troubleshooting.md) | Diagnostics, common issues, useful commands |
