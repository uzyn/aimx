# Getting Started

Install AIMX (AI Mail Exchange) and run your first setup.

## Requirements

- **OS:** Any Unix where Rust compiles (CI tests Ubuntu, Alpine, Fedora)
- **Server:** A VPS with port 25 open (inbound and outbound)
- **Domain:** A domain you control with access to DNS management
- **Build tools:** Rust toolchain (`rustup`)

### Compatible VPS providers

aimx requires direct SMTP access on port 25. Not all cloud providers allow this.

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
# Install Rust (if not already installed)
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source $HOME/.cargo/env

# Build aimx
git clone https://github.com/uzyn/aimx.git
cd aimx
cargo build --release
sudo cp target/release/aimx /usr/local/bin/
```

Verify the binary is installed:

```bash
aimx --version
```

## Security model

aimx is a **single-operator** mail server designed for AI agents on a domain you own. It stores mail under `/var/lib/aimx/` (world-readable). Any local user or agent can read email files. This is by design: aimx assumes a single-admin server where all agents are trusted to read each other's mail. Configuration and DKIM secrets live under `/etc/aimx/` (root-owned, not readable by non-root).

All mutations (send, reply, mark-read, create/delete mailboxes) go through the `aimx` MCP server or CLI. Never write to the data directory directly. The UDS send socket at `/run/aimx/send.sock` is world-writable. Any local user can submit outbound mail through `aimx send`. The authorisation boundary is the root-only DKIM private key, not filesystem ACLs on the mailbox tree.

**If you need per-user mailbox isolation** (multiple humans with private inboxes, IMAP/POP3, webmail, or a conventional multi-tenant mail setup), aimx is the wrong tool. Use a general-purpose MTA like [Postfix](https://www.postfix.org/) or [Stalwart](https://stalw.art/) instead. See [Can I use aimx in place of Postfix or Stalwart?](faq.md#can-i-use-aimx-in-place-of-postfix-or-stalwart) in the FAQ.

## Setup

Run the interactive setup wizard:

```bash
# With domain argument:
sudo aimx setup agent.yourdomain.com

# Or interactively (will prompt for domain):
sudo aimx setup
```

The wizard will:

1. Generate a self-signed TLS certificate and 2048-bit RSA DKIM keypair
2. Install a systemd (or OpenRC) service file for `aimx serve`
3. Start the embedded SMTP listener and verify port 25 connectivity (inbound and outbound)
4. Display the DNS records you need to add under a **[DNS]** section
5. Let you verify DNS records (press Enter to re-check, or q to defer)
6. Display **[MCP]** configuration for your AI agent
7. Show **[Deliverability Improvement (Optional)]** tips (Gmail filter / whitelist)
8. Create a default `catchall` mailbox

Re-running `sudo aimx setup agent.yourdomain.com` on an existing install skips installation and jumps straight to DNS verification.

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

Install aimx into your agent with one command:

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
