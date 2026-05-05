# Getting Started

Install AIMX and run setup.

## Requirements

- **OS:** Linux (x86_64 or aarch64, glibc or musl). CI covers Ubuntu, Alpine, Fedora.
- **Server:** A VPS with port 25 open both ways.
- **Domain:** One you control with DNS access.

### Compatible VPS providers

AIMX needs direct SMTP on port 25; not every cloud provider allows it.

| Provider | Port 25 | Notes |
|----------|---------|-------|
| Hetzner Cloud | After unblock request | Request via support after first invoice |
| OVH / Kimsufi | Open by default | |
| Vultr | Unblockable on request | |
| BuyVM (Frantech) | Open by default | |
| Linode / Akamai | On request | Submit support ticket |

Providers that **block** port 25 permanently (not compatible): DigitalOcean, AWS EC2, Azure VMs, GCP.

## Pre-install: check port 25

Run the connectivity check before installing:

```bash
curl -fsSL https://aimx.email/portcheck.sh | sh
```

`portcheck.sh` is an alias for `install.sh --port-check-only`. It runs the same outbound and inbound EHLO probes as `aimx portcheck` and exits without installing. Run it under `sudo` to include the inbound check; otherwise inbound is reported as `[skip]`. Exits `0` on pass, `1` on fail, `2` if a required tool is missing. Override the verifier with `--verify-host <URL>` (or `AIMX_VERIFY_HOST`).

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

AIMX is a single-operator server: it assumes one administrator and treats every local user on the host as inside the trust boundary. Mailbox storage is per-owner (`<owner>:<owner> 0700`), config and DKIM secrets stay root-only under `/etc/aimx/`, and every UDS verb is authorized server-side via `SO_PEERCRED`. If multiple humans on the box cannot trust each other to operate the daemon, AIMX is the wrong tool — use [Postfix](https://www.postfix.org/) or [Stalwart](https://stalw.art/).

See [Security](security.md) for the full threat model, trust boundaries, and non-goals.

## Setup

Run the interactive setup wizard:

```bash
# With domain argument:
sudo aimx setup agent.yourdomain.com

# Or interactively (will prompt for domain):
sudo aimx setup
```

The wizard runs a six-step checklist:

1. Port 25 preflight (outbound + inbound).
2. Prompt for domain and trusted-sender list.
3. Generate STARTTLS cert, DKIM keypair, and `/etc/aimx/config.toml`.
4. Create the `aimx-catchall` system user and the default catchall mailbox.
5. Print DNS records and re-verify on Enter (press `q` to skip and run `aimx doctor` later).
6. Install and start `aimx.service`, then re-exec `aimx agents setup` as `$SUDO_USER` so you can pick which AI agents to wire in.

Re-running `sudo aimx setup` on an existing install skips the cert/key/config writes, re-verifies DNS, and re-runs the agents picker. See [Setup: DNS Configuration](setup.md#dns-configuration).

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
aimx agents setup claude-code    # or codex / opencode / gemini / goose / openclaw / hermes
```

Run `aimx agents list` to see every supported agent and destination path; [Agent Integration](agent-integration.md) covers per-agent activation. The agent can now list, read, send, and reply to email via MCP — see [MCP Server](mcp.md) for the full tool set.
