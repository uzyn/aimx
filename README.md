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

A single TOML file at `/etc/aimx/config.toml` is everything. `aimx setup` writes the initial file for you; edit it directly to add mailboxes, override defaults, or attach hooks.

### Top-level

| Setting | Type | Default | Description |
|---------|------|---------|-------------|
| `domain` | string | *(required)* | The email domain (e.g. `agent.yourdomain.com`). |
| `data_dir` | string | `/var/lib/aimx` | Where mailboxes are stored on disk. |
| `dkim_selector` | string | `aimx` | Selector name used in your DKIM TXT record. |
| `trust` | string | `none` | Default trust policy: `none` or `verified`. |
| `trusted_senders` | array | `[]` | Default sender allowlist (globs, e.g. `*@yourcompany.com`). |
| `enable_ipv6` | bool | `false` | Opt into IPv6 outbound delivery. |
| `verify_host` | string | `https://check.aimx.email` | Preflight port 25 checking service used by `aimx portcheck` and `aimx setup`. |

### Mailbox `[mailboxes.<name>]`

| Setting | Type | Default | Description |
|---------|------|---------|-------------|
| `address` | string | *(required)* | Email address pattern (e.g. `support@…` or `*@…` for catchall). |
| `owner` | string | *(required)* | Linux user that owns the mailbox storage and runs hooks. |
| `trust` | string | *(inherited)* | Override the global default. |
| `trusted_senders` | array | *(inherited)* | Override the global allowlist. Replaces, no merge. |
| `hooks` | array | `[]` | `on_receive` / `after_send` commands — see [Hooks](#hooks) below. |

### Example

```toml
domain = "agent.yourdomain.com"
trust = "verified"
trusted_senders = ["*@yourcompany.com"]

[mailboxes.support]
address = "support@agent.yourdomain.com"
owner = "support-bot"
```

See [`book/configuration`](https://aimx.email/book/configuration.html) for the full field reference (hook fields, IPv6, env-var overrides).


## Trust & security

**Phishing-aware by default.** Every inbound message is checked for DKIM, SPF, and DMARC. Untrusted mail is still stored on disk so you can read it, but it never fires `on_receive` hooks. Your agent *sees* the message; it doesn't *act on* it. Untrusted emails are also clearly marked as `trusted: false` in the frontmatter for agent's reference.

* **DKIM-gated hooks.** Only mail that verifies (and matches your `trusted_senders` list) can trigger automation.
* **Per-mailbox owner.** One user per mailbox. Each mailbox runs as its own system user that owns the directory and executes hooks. Run a finance agent on one, family mail on another. They can't read each other's mail or interfere with each other's processes — enforced by the operating system, not by aimx.

Learn more about [`book/security`](https://aimx.email/book/security.html) for the threat model and [`book/hooks`](https://aimx.email/book/hooks.html#trust-gate-on_receive-only) for the trust gate.


## Hooks

**Mail is an event** Inbound mail fires `on_receive` the instant SMTP `DATA` completes. Outbound delivery fires `after_send` when the MX attempt resolves to `delivered` / `failed` / `deferred`. Hooks are declared per mailbox and `exec`'d directly — no shell, no cron, no heartbeat.

```toml
[[mailboxes.support.hooks]]
event = "on_receive"
cmd = ["/usr/bin/ntfy", "pub", "agent-mail", "$AIMX_SUBJECT from $AIMX_FROM"]
```

Each hook runs as the mailbox's owning Linux user and only on trusted emails that pass DKIM and match the `trusted_senders` allowlist. This makes it safe to plug directly into your agents without worrying about prompt injection or malicious emails.

See [`book/hooks`](https://aimx.email/book/hooks.html) for the hook model and [`book/hook-recipes`](https://aimx.email/book/hook-recipes.html) for copy-paste recipes (Claude Code, Codex CLI, OpenCode, Gemini CLI, Goose, OpenClaw, Hermes, Aider).


## Email format

**No `.eml`. No database.** Mail lands as plain Markdown your agent can `cat`, `grep`, or stream straight into a context window. Frontmatter is TOML between `+++` delimiters. Attachments live as sibling files inside a Zola-style bundle directory — no MIME parser required.

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

See [`book/mailboxes`](https://aimx.email/book/mailboxes.html#email-format) for the full schema and outbound sent-copy fields.

## DNS records

`aimx setup` prints these for you to copy-paste. Listed here for reference.

| Type | Name | Value |
|------|------|-------|
| A | agent.yourdomain.com | Your server IP |
| MX | agent.yourdomain.com | 10 agent.yourdomain.com. |
| TXT | agent.yourdomain.com | v=spf1 ip4:YOUR_IP -all |
| TXT | aimx._domainkey.agent.yourdomain.com | v=DKIM1; k=rsa; p=... |
| TXT | _dmarc.agent.yourdomain.com | v=DMARC1; p=reject |


## Why AIMX?

| | Gmail / Outlook | SaaS relays<br>(SendGrid / Postmark) | AgentMail / LobsterMail | Postfix / Stalwart | **AIMX** |
|---|:---:|:---:|:---:|:---:|:---:|
| Built for AI agents | ❌ | ❌ | ✅ | ❌ | ✅ |
| MCP support | via 3rd-party | via 3rd-party | ✅ (ext server) | ❌ | ✅ (local stdio) |
| Self-sovereign | ❌ | ❌ | ❌ | ✅ | ✅ |
| Direct delivery | ❌ | ❌ | ❌ | ✅ | ✅ |
| Markdown emails | ❌ | ❌ | ❌ | ❌ | ✅ |
| Free & open source | ❌ | ❌ | ❌ | ✅ | ✅ |

### Use cases

1. **Finance agent.** Forward invoices, receipts, and statements to `finance@…`. The agent extracts amounts and vendors, files attachments, and reconciles against your ledger.
1. **Privacy-sensitive workflows.** Bank statements, medical records, legal correspondence — mail that shouldn't sit in someone else's datacenter. No SaaS relay, no third-party MCP wrapper, no Gmail scanning the contents.
1. **Morning briefing.** A cron job kicks the agent at 7am. It reads overnight mails, research news and emails you your daily briefing.
1. **Support triage.** `support@…` drafts replies via MCP, tags by severity, and escalates when confidence is low. Threading is preserved, so customers see a normal Gmail conversation.
1. **Monitoring & alerts.** Datadog, GitHub Actions, and cron jobs page `alerts@…`. The `on_receive` hook decides whether to wake you, file a ticket, or self-remediate.
1. **Newsletter digest.** `news@…` collects Substack, arXiv, and HN digests. A nightly job ships one summary instead of 40 unread threads.
1. **Travel concierge.** Forward bookings to `travel@…`. The agent builds a live itinerary, pushes calendar events, and pings you on gate or check-in changes.
1. **Personal CRM.** `contacts@…` ingests intros and follow-ups. The agent extracts who, where, and why, and reminds you before the connection goes cold.
1. **Cross-agent message bus.** Two agents on different boxes use email as durable, DKIM-signed transport. Markdown is a payload format both already speak.
1. **Knowledge-base ingest.** Forward anything to `kb@…`. Markdown plus YAML frontmatter is RAG-ready the moment it hits disk.

### Design philosophy

AIMX is a mail server for AI agents, not a Postfix replacement.

1. AIMX is designed for a single operator (you) and a single domain (youragent.yourdomain.com). No multi-user, multi-domain, or shared hosting use cases. If you need those, run Postfix or Stalwart.
2. AIMX is designed for direct MTA-to-MTA delivery.
3. AIMX mails (data) are stored as Markdown for LLM-friendliness, not as `.eml` or in a database. Attachments are stored as files in a bundle directory, not as MIME parts, also for LLM-friendliness and ease of access.

As such, currently, by design:
* No IMAP or POP3
* No Webmail
* No SMTP AUTH
* No retry queues. Send is an synchronous operation that either delivers or fails immediately so your agent can react to the result in real time.
* No bounces. AIMX doesn't generate or process DSNs.
* No mail indexing or search. Bring your own RAG, indexing system or AI brains.

Subject to change, but for now AIMX is intentionally single-operator and single-domain. If you need multi-user, multi-domain, or IMAP access, run [Postfix](https://www.postfix.org/) or [Stalwart](https://stalw.art/).


## Contributing

Issues, PRs, and new hook recipes are very welcomed.

## License

MIT. See [LICENSE](LICENSE).

Copyright (c) 2026 [U-Zyn Chua](https://uzyn.com).
