# Getting Started

Install AIMX and run setup.

## Requirements

- **OS:** Linux (x86_64 or aarch64, glibc or musl).
- **Server:** A server (usually a VPS) with port 25 open Some providers block outbound 25 by default — check with yours before signing up, and run the connectivity check below before installing.
- **Domain:** One you control with DNS access. Subdomain works too.

## Optional: Pre-install: check port 25

Install step below starts with the port 25 check, but if you would rather run a standalone port 25 check. You can run the following without installing AIMX:

```bash
curl -fsSL https://aimx.email/portcheck.sh | sh
```

## Install

```bash
curl -fsSL https://aimx.email/install.sh | sh
```

This launches a guided setup with the following steps:

- [ ] Preflight checks on port 25
- [ ] Set up domain and DNS
- [ ] Set up STARTTLS certificate
- [ ] Set up trust policy
- [ ] Install AIMX service
- [ ] Optionally, wire up MCP for agent(s)

The installer auto-detects your platform and installs `aimx` into `/usr/local/bin/`. Verify the binary is installed:

```bash
aimx --version
```

See [Installation](installation.md) for install flags (`--tag`, `--target`, `--to`, `--force`), a skeptical-operator manual verify path (`sha256sum -c` against the published `SHA256SUMS`), `aimx upgrade`, and a source-build recipe for contributors.

## Security model

AIMX is a single-operator server: it assumes one administrator and treats every local user on the host as inside the trust boundary. Mailbox storage is per-owner (`<owner>:<owner> 0700`), config and DKIM secrets stay root-only under `/etc/aimx/`, and every Unix domain socket (UDS) verb is authorized server-side via `SO_PEERCRED`. If multiple humans on the box cannot trust each other to operate the daemon, AIMX is the wrong tool — use [Postfix](https://www.postfix.org/) or [Stalwart](https://stalw.art/).

See [Security](security.md) for the full threat model, trust boundaries, and non-goals.

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

## Connect your AI agent or harness

Install AIMX into your agent with one command:

```bash
aimx agents setup
```

