# Getting Started

This guide walks you through installing aimx and setting up your first agent email address.

## Requirements

- **OS:** Linux (Debian/Ubuntu)
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

## Setup

Run the interactive setup wizard:

```bash
sudo aimx setup agent.yourdomain.com
```

The wizard will:

1. Check port 25 reachability (inbound and outbound)
2. Install and configure OpenSMTPD
3. Generate a 2048-bit RSA DKIM keypair
4. Display the DNS records you need to add
5. Verify DNS configuration
6. Create a default `catchall` mailbox

Follow the on-screen prompts to add the required DNS records at your domain registrar. See [Setup -- DNS Configuration](setup.md#dns-configuration) for details on each record.

## Verify

After DNS records propagate, verify the setup:

```bash
# Check port 25 connectivity
aimx verify

# Check server status and mailbox counts
aimx status
```

## Send a test email

```bash
aimx send --from catchall@agent.yourdomain.com \
          --to your-personal@gmail.com \
          --subject "Hello from aimx" \
          --body "My agent can send email now."
```

## Connect your AI agent

Add aimx as an MCP server in Claude Code (`~/.claude/settings.json`):

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

Your agent can now list, read, send, and reply to email. See the [MCP Server](mcp.md) guide for all available tools.

## Next steps

- **[Setup](setup.md)** -- detailed walkthrough of every setup step, DNS records, DKIM management, and production hardening
- **[Configuration](configuration.md)** -- full `config.toml` reference for mailboxes, channel rules, and trust policies
- **[Channel Rules](channels.md)** -- trigger agent actions automatically when email arrives
- **[MCP Server](mcp.md)** -- integrate with Claude Code, OpenClaw, or any MCP client
- **[Troubleshooting](troubleshooting.md)** -- common issues and diagnostic commands
