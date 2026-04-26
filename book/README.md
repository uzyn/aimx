# aimx user guide

AIMX (AI Mail Exchange) is a self-hosted SMTP server that gives AI agents their own email addresses on a domain you control. Mail is parsed to Markdown with TOML frontmatter and stored on disk. Agents read and send via the built-in MCP server, the `aimx` CLI, or directly from the filesystem. `aimx serve` is the SMTP daemon. Every other subcommand is a short-lived process.

## How it works

```text
Inbound:
  Sender -> port 25 -> aimx serve -> ingest -> .md file
                                             -> hook manager (fires `on_receive` commands)

Outbound:
  MCP tool call -> aimx send -> UDS (/run/aimx/aimx.sock) -> aimx serve
                                                          -> DKIM sign
                                                          -> direct SMTP to recipient MX
```

- **Single binary.** Written in Rust. No runtime dependencies.
- **`aimx serve` is the daemon.** Embedded SMTP listener for inbound mail. Every other command is short-lived.
- **No IMAP / POP3.** Agents read `.md` files via MCP or the filesystem.
- **Markdown-first.** Mail is stored as Markdown with TOML frontmatter, LLM- and RAG-friendly without a parser.

## Quick start

```bash
# 1. Install (Linux only; x86_64 and aarch64, glibc and musl)
curl -fsSL https://aimx.email/install.sh | sh

# 2. Run setup (generates service file, DKIM keys, DNS guidance)
sudo aimx setup agent.yourdomain.com

# 3. Verify
sudo aimx portcheck
```

See [Installation](installation.md) for install flags, verification, and upgrades,
and [Getting Started](getting-started.md) for the full walkthrough.

## Guide contents

| Page | What it covers |
|------|----------------|
| [Getting Started](getting-started.md) | Requirements, installation, first setup |
| [Installation](installation.md) | One-line installer, flags, verification, `aimx upgrade`, rollback |
| [Setup](setup.md) | DNS, verification, DKIM key management, production hardening |
| [Configuration](configuration.md) | `config.toml` field reference, data / config directories, environment variables |
| [Mailboxes & Email](mailboxes.md) | Mailbox CRUD, email frontmatter, attachments, sending, threading |
| [Hooks & Trust](hooks.md) | `on_receive` / `after_send` events, match filters, trust gate |
| [Hook Recipes](hook-recipes.md) | Copy-paste hook snippets per agent (Claude Code, Codex, OpenCode, Gemini, Goose, OpenClaw, Hermes, Aider) |
| [Security](security.md) | Threat model, trust boundaries, what aimx defends and what it does not |
| [MCP Server](mcp.md) | The 9 MCP tools: parameters, frontmatter contract, workflow examples |
| [Agent Integration](agent-integration.md) | `aimx agents setup` installer, per-agent configuration, manual MCP wiring |
| [CLI Reference](cli.md) | Every `aimx` subcommand and flag |
| [Troubleshooting](troubleshooting.md) | Diagnostics, common issues, useful commands |
| [FAQ](faq.md) | Deployment, DNS, storage, MCP, and operations questions |
