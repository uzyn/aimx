# AIMX User Guide

AIMX is a self-hosted SMTP server that gives AI agents their own email addresses on a domain you control. Mail is parsed to Markdown with TOML frontmatter and stored on disk; agents read and send via the built-in MCP server, the `aimx` CLI, or directly from the filesystem. `aimx serve` is the SMTP daemon; every other subcommand is a short-lived process.

## How it works

```text
Inbound:
  Sender -> port 25 -> aimx serve -> ingest -> .md file
                                             -> hook manager (fires `on_receive` commands)

Outbound:
  MCP tool call -> aimx send -> UDS (/run/aimx/send.sock) -> aimx serve
                                                          -> DKIM sign
                                                          -> direct SMTP to recipient MX
```

- **Single binary.** Written in Rust. No runtime dependencies.
- **`aimx serve` is the daemon.** Embedded SMTP listener for inbound mail; every other command is short-lived.
- **No IMAP / POP3.** Agents read `.md` files via MCP or the filesystem.
- **Markdown-first.** Mail is stored as Markdown with TOML frontmatter — LLM- and RAG-friendly without a parser.

## Quick start

```bash
# 1. Build and install
git clone https://github.com/uzyn/aimx.git && cd aimx
cargo build --release
sudo cp target/release/aimx /usr/local/bin/

# 2. Run setup (generates service file, DKIM keys, DNS guidance)
sudo aimx setup agent.yourdomain.com

# 3. Verify
sudo aimx portcheck
```

See [Getting Started](getting-started.md) for the full walkthrough.

## Guide contents

| Page | What it covers |
|------|----------------|
| [Getting Started](getting-started.md) | Requirements, installation, first setup |
| [Setup](setup.md) | DNS, verification, DKIM key management, production hardening |
| [Configuration](configuration.md) | `config.toml` field reference, data / config directories, environment variables |
| [Mailboxes & Email](mailboxes.md) | Mailbox CRUD, email frontmatter, attachments, sending, threading |
| [Hooks & Trust](hooks.md) | `on_receive` / `after_send` events, match filters, trust gate |
| [Hook Recipes](hook-recipes.md) | Copy-paste hook snippets per agent (Claude Code, Codex, OpenCode, Gemini, Goose, OpenClaw, Aider) |
| [MCP Server](mcp.md) | The 9 MCP tools — parameters, frontmatter contract, workflow examples |
| [Agent Integration](agent-integration.md) | `aimx agent-setup` installer, per-agent configuration, manual MCP wiring |
| [Troubleshooting](troubleshooting.md) | Diagnostics, common issues, useful commands |
| [FAQ](faq.md) | Deployment, DNS, storage, MCP, and operations questions |
