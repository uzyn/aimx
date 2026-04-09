# aimx

> Your server can receive email. Why are you routing through Gmail?

**SMTP for agents. No middleman.**

One command to give your AI agents their own email addresses — no Gmail, no OAuth, no third-party SaaS. Built for Claude Code, OpenClaw, Codex, and any agentic system that needs an email channel.

```bash
aimx setup agent.mydomain.com
```

Done. Incoming mail is parsed to Markdown. Outbound mail is DKIM-signed. MCP is built in. Channel rules trigger agent actions on incoming mail.

## How it works

```
Inbound:
  Sender → port 25 → OpenSMTPD → aimx ingest → .md file
                                              → channel manager (triggers agent)

Outbound:
  MCP tool call → aimx send → DKIM sign → OpenSMTPD → remote MX
```

- **No daemon.** OpenSMTPD is the only long-running process. `aimx` commands are short-lived.
- **No IMAP/POP3.** Agents read `.md` files via MCP or filesystem.
- **Markdown-first.** Emails stored as Markdown with YAML frontmatter — agents can `cat` and understand immediately.
- **Single binary.** Written in Rust. No runtime dependencies beyond OpenSMTPD.

## Features

- **Setup wizard** — preflight checks, OpenSMTPD config, DKIM keygen, DNS guidance, verification
- **Email delivery** — EML→Markdown with YAML frontmatter, attachment extraction, mailbox routing
- **Email sending** — RFC 5322 composition, DKIM signing (RSA-SHA256), threading support
- **MCP server** — stdio transport for Claude Code and any MCP client: list, read, send, reply, manage mailboxes
- **Channel manager** — trigger shell commands on incoming mail with match filters (from, subject, attachments)
- **Inbound trust** — DKIM/SPF verification, per-mailbox trust policies, trusted sender allowlists

## Requirements

- Linux (Debian/Ubuntu)
- A VPS with port 25 open (inbound + outbound)
- A domain you control

### Compatible VPS providers

| Provider | Port 25 | Notes |
|----------|---------|-------|
| Hetzner Cloud | After unblock request | Request via support after first invoice |
| OVH / Kimsufi | Open by default | |
| Vultr | Unblockable on request | |
| BuyVM | Open by default | |

## Status

**Work in progress.** See [docs/prd.md](docs/prd.md) for the product requirements and [docs/sprint.md](docs/sprint.md) for the sprint plan.

## License

TBD (MIT or Apache-2.0)

## Author

[U-Zyn Chua](https://uzyn.com) <chua@uzyn.com>

