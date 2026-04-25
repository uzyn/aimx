# Getting Started

Install AIMX (AI Mail Exchange) and run your first setup.

## Requirements

- **OS:** Linux (x86_64 or aarch64, glibc or musl). Release tarballs ship for all four targets; CI covers Ubuntu, Alpine, Fedora.
- **Server:** A VPS with port 25 open (inbound and outbound)
- **Domain:** A domain you control with access to DNS management

### Compatible VPS providers

AIMX requires direct SMTP access on port 25. Not all cloud providers allow this.

| Provider | Port 25 | Notes |
|----------|---------|-------|
| Hetzner Cloud | After unblock request | Request via support after first invoice |
| OVH / Kimsufi | Open by default | |
| Vultr | Unblockable on request | |
| BuyVM (Frantech) | Open by default | |
| Linode / Akamai | On request | Submit support ticket |

Providers that **block** port 25 permanently (not compatible): DigitalOcean, AWS EC2, Azure VMs, GCP.

## Install

```bash
curl -fsSL https://aimx.email/install.sh | sh
```

The installer auto-detects your platform and installs `aimx` into `/usr/local/bin/`. Verify the binary is installed:

```bash
aimx --version
```

See [Installation](installation.md) for install flags (`--tag`, `--target`, `--to`, `--force`), a skeptical-operator manual verify path (`sha256sum -c` against the published `SHA256SUMS`), `aimx upgrade`, and a source-build recipe for contributors.

## Security model

AIMX is a **single-operator** mail server designed for AI agents on a domain you own. It stores mail under `/var/lib/aimx/` (world-readable). Any local user or agent can read email files. This is by design: AIMX assumes a single-admin server where all agents are trusted to read each other's mail. Configuration and DKIM secrets live under `/etc/aimx/` (root-owned, not readable by non-root).

All mutations (send, reply, mark-read, create/delete mailboxes) go through the `aimx` MCP server or CLI. Never write to the data directory directly. The UDS send socket at `/run/aimx/aimx.sock` is world-writable. Any local user can submit outbound mail through `aimx send`. The authorisation boundary is the root-only DKIM private key, not filesystem ACLs on the mailbox tree.

**If you need per-user mailbox isolation** (multiple humans with private inboxes, IMAP/POP3, webmail, or a conventional multi-tenant mail setup), AIMX is the wrong tool. Use a general-purpose MTA like [Postfix](https://www.postfix.org/) or [Stalwart](https://stalw.art/) instead. See [Can I use AIMX in place of Postfix or Stalwart?](faq.md#can-i-use-aimx-in-place-of-postfix-or-stalwart) in the FAQ.

See [Security](security.md) for the full threat model, trust boundaries, and non-goals.

## Setup

Run the interactive setup wizard:

```bash
# With domain argument:
sudo aimx setup agent.yourdomain.com

# Or interactively (will prompt for domain):
sudo aimx setup
```

The wizard will:

1. Run a port 25 preflight (outbound + inbound)
2. Prompt for the domain (if not passed as an argument) and the trusted-sender list
3. Generate a self-signed TLS certificate, a 2048-bit RSA DKIM keypair, and `/etc/aimx/config.toml`
4. Create the unprivileged `aimx-catchall` service user and the default `catchall` mailbox
5. Print the DNS records you need to add, then re-verify on Enter (press `q` to skip and run `aimx doctor` later)
6. Install and start `aimx.service`, waiting for port 25 to come up
7. Print a single-line `aimx is running for <domain>.` banner and a short `[MCP]` summary
8. Drop through to `aimx agent-setup` as your regular user (via `runuser -u $SUDO_USER`) so you can tick the AI agents to wire into AIMX

Re-running `sudo aimx setup agent.yourdomain.com` on an existing install skips the TLS / DKIM / config-write steps, re-verifies DNS, and drops through to `aimx agent-setup` again so you can wire additional agents.

Follow the on-screen prompts to add the required DNS records at your domain registrar. See [Setup: DNS Configuration](setup.md#dns-configuration) for per-record details.

## Verify

After DNS records propagate, verify the setup:

```bash
# Check port 25 connectivity (requires root)
sudo aimx portcheck

# Check server health, mailbox counts, and DNS verification
aimx doctor
```

## Send a test email

```bash
aimx send --from catchall@agent.yourdomain.com \
          --to your-personal@gmail.com \
          --subject "Hello from aimx" \
          --body "My agent can send email now."
```

## Connect your AI agent

Install AIMX into your agent with one command:

```bash
aimx agent-setup claude-code    # or codex / opencode / gemini / goose / openclaw / hermes
```

Run `aimx agent-setup --list` to see every supported agent and its
destination path, and see the [Agent Integration](agent-integration.md)
chapter for per-agent activation steps.

The agent can now list, read, send, and reply to email via MCP. See [MCP Server](mcp.md) for the full tool set.

## Next steps

- **[Setup](setup.md)**: detailed walkthrough of every setup step, DNS records, DKIM management, and production hardening
- **[Configuration](configuration.md)**: full `config.toml` reference for mailboxes, hooks, and trust policies
- **[Hooks & Trust](hooks.md)**: fire agent actions automatically on inbound/outbound events
- **[MCP Server](mcp.md)**: integrate with Claude Code, OpenClaw, or any MCP client
- **[Troubleshooting](troubleshooting.md)**: common issues and diagnostic commands
