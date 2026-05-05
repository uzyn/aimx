# AIMX — AI Mail Exchange

> [!CAUTION]
> **Under heavy development.** This is a prerelease software. Expect breaking changes and unstable operations.

<p align="center">
    <picture>
        <source media="(prefers-color-scheme: light)" srcset="https://raw.githubusercontent.com/uzyn/aimx/refs/heads/main/etc/aimx-pigeon.svg">
        <img src="https://raw.githubusercontent.com/uzyn/aimx/refs/heads/main/etc/aimx-pigeon.svg" alt="AIMX Pigeon Mascot" width="300">
    </picture>
</p>


<h3 align="center">SMTP for AI agents. No middleman.</h3>

<p align="center"><em>
The internet's oldest protocol, rebuilt for AI agents.<br>
Runs entirely on your box. No third parties.<br>
Your mail, your machine, end to end.<br>
Human-friendly setup. LLM-friendly everything else.
</em></p>


AIMX (AI Mail Exchange) is a self-hosted email server (SMTP) and MCP stdio server that gives AI agents their own email addresses. Plugs into Claude Code, Codex CLI, Gemini CLI, Goose, and any other agent harness. No Gmail, no OAuth, no SaaS. Runs on the same VPS you already provisioned for your agents.

## Features

* **Single binary.** With human-friendly guided set up process.
* **Markdown-based email**, coupled with clean TOML frontmatter. Friendly for your LLMs, RAGs and AI brains.
* **Direct MTA-to-MTA.** Email has become send-and-pray best-effort. AIMX turns it back into direct server-to-server delivery, like an API call.
* **Instant hooks** Inbound mail fires `on_receive` hooks the moment SMTP `DATA` completes. Outbound delivery fires `after_send` hooks when the MX attempt resolves. No cron, no heartbeat.
* **Trust modeling.** Built-in DKIM-based trust model. Widely compatible. Minimizes prompt injection attacks.
* **Built-in MCP server.** Stdio MCP. Efficient. MCP server only runs on demand, kills off when done.
* **No third parties.** Mail lives on your server,. No need to trust and third-party servers with your sensitive data.
* **One-line agent integration.** Integrates directly into your favorite AI agents: OpenClaw, Hermes, NanoClaw, Claude Code, etc.
* **IPv6-ready.** IPv4 by default, but future-proof with IPv6 support.
* **MIT licensed.** Free and open source.

Read the [Book](https://aimx.email/book/) to learn more. See also [Frequently Asked Questions](https://aimx.email/book/faq.html).

Contributions are welcomed!

## Requirements

- A Linux server with port 25 open (inbound and outbound)
- A domain or subdomain you control

## Quick start

```bash
curl -fsSL https://aimx.email/install.sh | sh
```

This will lead you to a guided setup with the following steps:

- [ ] Preflight checks on port 25
- [ ] Set up domain and DNS
- [ ] Set up STARTTLS certificate
- [ ] Set up trust policy
- [ ] Install AIMX service
- [ ] Set up MCP for agent(s)

To upgrade, simply run `sudo aimx upgrade`.

## How to build

```bash
git clone https://github.com/uzyn/aimx.git
cd aimx
cargo build --release
sudo cp target/release/aimx /usr/local/bin/

# Run setup and follow the guided instructions
sudo aimx setup
```

### CLI Commands

```text
Usage: aimx [OPTIONS] <COMMAND>

Operations (as current user):
  send         Send an email
  mailboxes    Manage mailboxes
  hooks        Manage hooks
  agents       Manage AI agent MCP wiring
  mcp          Start the stdio MCP server (for AI agents)

Server administration:
  setup        Run the interactive setup wizard
  serve        Start the SMTP daemon
  doctor       Show server health, DNS, and recent logs
  logs         Tail the aimx service log
  dkim-keygen  Generate a DKIM keypair
  portcheck    Check port 25 connectivity (inbound, outbound)
  uninstall    Uninstall the aimx service (config and data retained)
  upgrade      Fetch the latest release and swap the installed binary

  help         Print help for a subcommand

Options:
      --data-dir <DATA_DIR>  Data directory override (default: /var/lib/aimx) [env: AIMX_DATA_DIR=]
  -V, --version              Print version
  -h, --help                 Print help
```

### MCP server for AI agents

AIMX supports an easy MCP wiring for your AI agents and harnesses. Simply run `aimx agents setup` and follow the interactive prompts. Most harnesses are wired automatically.

  | Agent | MCP | Skill / Recipe |
  |-------|-----|----------------|
  | Claude Code | ✅ Auto-wired | ✅ `~/.claude/skills/aimx/` |
  | Codex CLI | ✅ Auto-wired | ✅ `~/.codex/skills/aimx/` |
  | NanoClaw | ✅ Auto-wired (`<fork>/.mcp.json`; default `~/nanoclaw`, override with `$NANOCLAW_HOME`) | ✅ `<fork>/skills/aimx/` |
  | Goose | ✅ Bundled in the recipe; activate with `goose run --recipe aimx` | ✅ `~/.config/goose/recipes/aimx.yaml` |
  | OpenClaw | ✅ Run the guided `openclaw mcp set aimx '...'` command after setup. | ✅ `~/.openclaw/skills/aimx/` |
  | OpenCode | ✅ Paste the printed JSONC block into `opencode.json` after setup. | ✅ `~/.config/opencode/skills/aimx/` |
  | Gemini CLI | ✅ Merge the printed JSON block into `~/.gemini/settings.json` after setup. | ✅ `~/.gemini/skills/aimx/` |
  | Hermes | ✅ Paste the printed YAML in `~/.hermes/config.yaml` after setup. | ✅ `~/.hermes/skills/aimx/` |

Available MCP tools:
- `mailbox_list`: list all mailboxes with message counts
- `mailbox_create`: create a new mailbox
- `mailbox_delete`: delete a mailbox
- `email_list`: list emails with optional filters (unread, from, since, subject)
- `email_read`: read full email content
- `email_send`: compose and send an email
- `email_reply`: reply to an email with correct threading
- `email_mark_read`: mark an email as read
- `email_mark_unread`: mark an email as unread

For more details, see the [MCP documentation](https://aimx.email/book/mcp.html) and the [agent integration guide](https://aimx.email/book/agent-integration.html#agent-mcp-integration).

## Configuration

aimx reads a single TOML file at `/etc/aimx/config.toml`, written by `aimx setup` with mode `0640 root:root`. Top-level settings cover `domain`, `dkim_selector`, and the defaults for `trust` / `trusted_senders`. Per-mailbox tables attach addresses, override those defaults, and declare hooks.

See [`book/configuration.md`](book/configuration.md) for the full field reference.


## Trust policy

Every inbound email is checked for DKIM, SPF, and DMARC, and the results are written into the TOML frontmatter alongside a summary `trusted` field (`none`, `true`, or `false`). Trust only gates `on_receive` hook execution. Mail is always stored on disk regardless of the outcome, so agents and humans can still read untrusted messages via MCP or the filesystem.

See [`book/hooks.md`](book/hooks.md#trust-gate-on_receive-only) for the gate logic and [`book/configuration.md`](book/configuration.md#inbound-email-verification) for the per-field semantics.


## Hooks

aimx fires commands on two events: `on_receive` (after an inbound email is stored) and `after_send` (after the outbound MX attempt resolves to `delivered` / `failed` / `deferred`). Hooks are declared per mailbox in `config.toml` and fire on every event of their configured type; `on_receive` hooks only fire on trusted mail unless a hook opts in with `fire_on_untrusted = true`. Hooks may carry an optional `name`; when omitted, aimx derives a stable 12-char hex id from `event + cmd + fire_on_untrusted` so `aimx hooks list` / `delete` can still target the hook. Each hook executes as the mailbox's owning Linux user (`setuid` from root before `exec`); there is no per-hook `run_as` override.

```toml
[[mailboxes.support.hooks]]
# name is optional. A stable 12-char hex id is derived from event+cmd+fire_on_untrusted if omitted.
event = "on_receive"
cmd = ["/usr/bin/ntfy", "pub", "agent-mail", "$AIMX_SUBJECT from $AIMX_FROM"]
fire_on_untrusted = true
```

See [`book/hooks.md`](book/hooks.md) for the hook model and [`book/hook-recipes.md`](book/hook-recipes.md) for copy-paste recipes (Claude Code, Codex CLI, OpenCode, Gemini CLI, Goose, OpenClaw, Hermes, Aider). NanoClaw is a long-running daemon and uses a scheduled-job pull model rather than `on_receive` hooks — see [`book/agent-integration.md`](book/agent-integration.md#nanoclaw).


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


## Building from source

Source builds are supported for contributors and air-gapped environments. Everyone else should use the one-line installer above — it is faster and pins a tested release.

```bash
# Prereqs: rustup + a recent stable toolchain
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source $HOME/.cargo/env

git clone https://github.com/uzyn/aimx.git
cd aimx
cargo build --release
sudo install -m 0755 target/release/aimx /usr/local/bin/aimx
aimx --version
```

See [`CLAUDE.md`](CLAUDE.md) for the full developer workflow (lint, format, tests, verifier service).


## License

This project is licensed under the MIT License. See the [LICENSE](LICENSE) file for details.

Copyright (c) 2026 [U-Zyn Chua](https://uzyn.com).
