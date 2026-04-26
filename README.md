# AIMX — AI Mail Exchange

> [!CAUTION]
> **Under heavy development.** This is a pre-v1 alpha release. Expect breaking changes and unstable operations in v0 releases.

<p align="center">
    <picture>
        <source media="(prefers-color-scheme: light)" srcset="https://raw.githubusercontent.com/uzyn/aimx/refs/heads/main/etc/aimx-pigeon.svg">
        <img src="https://raw.githubusercontent.com/uzyn/aimx/refs/heads/main/etc/aimx-pigeon.svg" alt="aimx Pigeon Mascot" width="300">
    </picture>
</p>


<h3 align="center">SMTP for AI agents. No middleman.</h3>

<p align="center"><em>
The internet's oldest protocol, rebuilt for AI agents.<br>
Runs entirely on your box. No third parties.<br>
Your mail, your machine, end to end.<br>
Human-friendly setup. LLM-friendly everything else.
</em></p>


AIMX (AI Mail Exchange) is a self-hosted mail server for AI agents. One command gives your agents their own email addresses. No Gmail, no OAuth, no SaaS. Self-hosted means full sovereignty.

Mail in as Markdown. Mail out DKIM-signed. MCP built in. Works with any MCP-capable agent: Claude Code, Codex CLI, OpenCode, Gemini CLI, Goose, OpenClaw, Hermes.


- **Single binary.** One binary, no other dependencies.
- **Direct MTA-to-MTA.** Email has become send-and-pray best-effort. AIMX turns it back into direct server-to-server delivery. Feels like an API call.
- **Push, not poll.** Inbound mail fires `on_receive` hooks the moment SMTP `DATA` completes. Outbound delivery fires `after_send` hooks when the MX attempt resolves. No cron, no heartbeat.
- **Markdown emails.** No `.eml`, no database. Just Markdown with TOML frontmatter, LLM and RAG friendly. Your agent can `cat` the mailbox. Your inbox becomes your knowledge base.
- **Trust modeling.** Built-in DKIM-based trust model. Widely compatible. Minimizes prompt injection attacks.
- **You own the inbox.** Mail lives on your disk, under your domain. Nothing phones home.
- **Hot-swappable mailboxes.** Agents (or you) create and manage mailboxes. Changes take effect live.
- **Built-in MCP server.** Stdio MCP. Efficient. Create address, send mail, receive mail and more.
- **One-line agent integration.** Integrates directly into your favorite AI agents: OpenClaw, Claude Code, etc.
- **IPv6-ready.** One flag opts in. IPv4 by default keeps your SPF simple.
- **MIT licensed.** No license server, no telemetry, no account.

Read the [Book](book/) to learn more. See also [Frequently Asked Questions](book/faq.md).

## Requirements

- A Linux server (VPS) with port 25 open (inbound and outbound)
- A domain or subdomain you control

## Quick start

aimx ships as a single prebuilt binary for Linux (x86_64 and aarch64, glibc and musl). You need `sudo` rights — port 25 is a [privileged port](https://www.w3.org/Daemon/User/Installation/PrivilegedPorts.html).

```bash

# 1. Install the latest release into /usr/local/bin
# !! DO NOT use installer script yet. Compile aimx in the mean time if you are installing !!
# curl -fsSL https://aimx.email/install.sh | sh

# or compile and install it from source
git clone https://github.com/uzyn/aimx.git
cd aimx
cargo build --release
sudo cp target/release/aimx /usr/local/bin/

# 2. Run setup and follow the guided instructions
sudo aimx setup

# 3. Check health
aimx doctor
```

Later, upgrade in place:

```bash
sudo aimx upgrade
```

No wizard re-run, no DNS re-verify; the binary is swapped atomically and the service is restarted. See [`book/installation.md`](book/installation.md) for install flags, upgrade semantics, and the manual rollback path (`/usr/local/bin/aimx.prev`).

### Verification (optional)

If you would rather not `curl | sh`, every release ships a `.sha256` per tarball and a release-wide `SHA256SUMS`. The trust anchor in v1 is HTTPS on the GitHub Releases domain — signed releases are deferred to v2. Verify manually:

```bash
# Tags are bare SemVer (no `v` prefix). Tarball filenames drop the
# `-unknown-` vendor field; the canonical target triple is still what
# `aimx --version` prints in its target slot.
TAG=0.1.0
TARBALL_TARGET=x86_64-linux-gnu
TARBALL=aimx-${TAG}-${TARBALL_TARGET}.tar.gz

curl -fL -O "https://github.com/uzyn/aimx/releases/download/${TAG}/${TARBALL}"
curl -fL -O "https://github.com/uzyn/aimx/releases/download/${TAG}/${TARBALL}.sha256"
sha256sum -c "${TARBALL}.sha256"

tar -xzf "${TARBALL}"
sudo install -m 0755 "aimx-${TAG}-${TARBALL_TARGET}/aimx" /usr/local/bin/aimx
aimx --version
```

Or inspect the installer itself before running it: `curl -fsSL https://aimx.email/install.sh | less`.

### CLI Commands

```text
$ aimx
SMTP for AI agents. No middleman.

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
  agents       Manage AI agent MCP wiring (setup / remove / list)
  dkim-keygen  Generate DKIM keypair for email signing
  help         Print this message or the help of the given subcommand(s)

Options:
      --data-dir <DATA_DIR>  Data directory override (default: /var/lib/aimx) [env: AIMX_DATA_DIR=]
  -h, --help                 Print help (see more with '--help')
  -V, --version              Print version
```

### MCP server (for AI agents)

Install aimx into your agent with one command:

| Agent | Install command | Activation |
|-------|-----------------|------------|
| Claude Code | `aimx agents setup claude-code` | Restart Claude Code (auto-discovered from `~/.claude/plugins/`). |
| Codex CLI | `aimx agents setup codex` | Restart Codex CLI (auto-discovered from `~/.codex/plugins/`). |
| OpenCode | `aimx agents setup opencode` | Paste the printed JSONC block into `opencode.json`, then restart. |
| Gemini CLI | `aimx agents setup gemini` | Merge the printed JSON block into `~/.gemini/settings.json`, then restart. |
| Goose | `aimx agents setup goose` | Run `goose run --recipe aimx`. |
| OpenClaw | `aimx agents setup openclaw` | Run the printed `openclaw mcp set aimx '...'` command, then restart the gateway. |
| Hermes | `aimx agents setup hermes` | Paste the printed YAML block under `mcp_servers:` in `~/.hermes/config.yaml`, then run `/reload-mcp` inside Hermes. |

Run `aimx agents setup` (no args, launches the interactive picker) or `aimx agents list` to print the supported-agent registry. See [`book/agent-integration.md`](book/agent-integration.md) for per-agent activation steps and manual MCP wiring, and [`book/hook-recipes.md`](book/hook-recipes.md) for copy-paste hook recipes covering every supported agent plus Aider.

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
