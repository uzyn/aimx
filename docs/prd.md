# aimx — Product Requirements Document

## 1. Overview

aimx is a self-hosted email system for AI agents. It gives agents their own email addresses on a domain the user controls, with mail stored as Markdown files, MCP integration for agent access, and channel rules to trigger actions on incoming mail. One binary, one setup command, no third parties.

**Tagline:** SMTP for agents. No middleman.

## 2. Problem Statement

Giving an AI agent an email address today is unreasonably difficult:

- **Gmail/OAuth route** requires Google Cloud Console setup, OAuth credential management, refresh token handling, SSH tunnels for headless auth — and risks account bans for bot behavior.
- **SaaS route** (AgentMail, etc.) routes all agent email through third-party infrastructure. Data lives on their servers. Service discontinuation kills agent communication.
- **DIY route** means selecting and configuring a mail server, writing MIME parsers, building delivery pipelines, and gluing together unrelated tools.

All routes expose sensitive communications to third parties, which is absurd when the user already dedicates a server to their agentic system — a server perfectly capable of handling SMTP directly.

**Who experiences this:** Developers running AI agent systems (Claude Code, OpenClaw, Codex) on dedicated VPS/servers who need email as a communication channel for their agents.

**Impact of not solving:** Developers either waste hours on fragile Gmail/OAuth setups, depend on third-party SaaS for critical agent communication, or avoid email as an agent channel entirely.

## 3. Goals and Success Metrics

| Goal | Metric | Target |
|------|--------|--------|
| Zero-to-working-email in one session | Time from `aimx setup` to verified send/receive | < 15 minutes (excluding DNS propagation) |
| Agent-native email access | MCP tool coverage | All core email operations (list, read, send, reply) available via MCP |
| Reliable delivery | DKIM/SPF/DMARC pass rate on outbound mail | 100% for correctly configured domains |
| Minimal operational burden | Long-running processes required beyond OpenSMTPD | 0 (no aimx daemon) |
| Developer adoption | GitHub stars / installs | Establish initial user base; exact target TBD |

## 4. User Personas

### Agent Operator (primary)
- **Description:** Developer who runs one or more AI agents on a dedicated VPS. Comfortable with Linux, DNS, and CLI tools. Already uses Claude Code or similar agentic frameworks.
- **Needs:** Give agents email addresses quickly. Read/send email programmatically. Trigger agent actions on incoming mail. Keep data on their own server.
- **Context:** SSH'd into a VPS. Has a domain they control. Wants to set up email in one sitting and never think about it again.

### Agent Framework Developer (secondary)
- **Description:** Developer building tools/frameworks for AI agents who wants to integrate email as a channel.
- **Needs:** Standard MCP interface for email. Predictable file-based storage that's easy to read programmatically. No daemon to manage.
- **Context:** Integrating aimx into a larger agent system. Cares about the MCP API surface and file format stability.

## 5. User Stories

### P0 — Must Have
- As an agent operator, I want to run a single setup command so that my agent gets a working email address with proper DKIM/SPF/DMARC.
- As an agent operator, I want incoming emails stored as Markdown files so that my agent can read them without parsing libraries.
- As an agent operator, I want to send emails via MCP tool calls so that my agent can compose and send messages programmatically.
- As an agent operator, I want to create multiple mailboxes so that different agents or functions have dedicated email addresses.
- As an agent operator, I want channel rules that execute commands on incoming mail so that my agent can react to emails automatically.
- As an agent operator, I want DKIM signing handled natively so that outbound mail passes authentication checks without external tools.
- As an agent operator, I want setup to verify port 25 reachability (inbound and outbound) before proceeding so that I don't waste time configuring a server that can't deliver mail.
- As an agent operator, I want read/unread tracking so that agents can process only new emails.
- As an agent operator, I want inbound DKIM/SPF verification so that channel triggers only fire on authenticated emails when I enable trust policies.

### P1 — Should Have
- As an agent operator, I want to filter channel triggers by sender, subject, or attachment presence so that agents only act on relevant emails.
- As an agent operator, I want email threading support (In-Reply-To, References) so that replies are grouped correctly in recipients' mail clients.
- As an agent operator, I want to check server status and mailbox counts with a single command so that I can verify the system is working.
- As an agent operator, I want email attachments extracted to the filesystem so that agents can access attached files directly.

### P2 — Nice to Have
- As an agent operator, I want end-to-end verification against a public verify service so that I can confirm the full pipeline works after setup.

## 6. Functional Requirements

### 6.1 Setup Wizard (`aimx setup <domain>`)
- FR-1: Require root. Exit with clear message if not running as root.
- FR-1b: Detect MTA conflict on port 25. If non-OpenSMTPD MTA is running, exit with process name and instructions. If OpenSMTPD is already running, prompt user to confirm smtpd.conf overwrite and create .bak backup.
- FR-2: Install and configure OpenSMTPD with TLS and MDA delivery to `aimx ingest`.
- FR-3: Run port 25 checks after OpenSMTPD is installed: outbound (connect to check service port 25), inbound (check service performs EHLO handshake back to caller).
- FR-4: Stop with clear error message if port 25 is blocked. List compatible VPS providers.
- FR-5: Check reverse DNS (PTR) and warn (non-blocking) if not set.
- FR-6: Generate 2048-bit RSA DKIM keypair.
- FR-7: Display required DNS records (MX, A, SPF, DKIM, DMARC, PTR) and wait for user confirmation.
- FR-8: Verify DNS records are correctly set.
- FR-9: Create default `catchall` mailbox.
- FR-10: Display MCP configuration snippet for Claude Code.

### 6.2 Email Delivery (`aimx ingest <rcpt>`)
- FR-11: Accept raw `.eml` from OpenSMTPD via stdin.
- FR-12: Parse MIME message: extract headers, body (prefer text/plain, fall back to text/html→plaintext), and attachments.
- FR-13: Generate Markdown file with TOML frontmatter (id, message_id, from, to, subject, date, in_reply_to, references, attachments list, mailbox, read). Set `read: false` on ingest.
- FR-14: Extract attachments to `attachments/` subdirectory within the mailbox folder.
- FR-15: Route to correct mailbox directory based on RCPT TO local part. Unrecognized addresses go to `catchall`.
- FR-16: Execute matching channel rules after saving. Trigger failures are logged but do not block delivery.

### 6.3 Email Sending (`aimx send`)
- FR-17: Compose RFC 5322 compliant email from provided parameters (from, to, subject, body, attachments).
- FR-18: Sign message with DKIM (RSA-SHA256) using the domain's private key.
- FR-19: Hand signed message to OpenSMTPD for delivery.
- FR-20: Support file attachments by path.
- FR-21: Set proper In-Reply-To and References headers when replying.

### 6.4 MCP Server (`aimx mcp`)
- FR-22: Run in stdio mode (launched on-demand by MCP client, no long-running process).
- FR-23: Implement tools: `mailbox_create`, `mailbox_list`, `mailbox_delete`.
- FR-24: Implement tools: `email_list` (with optional filters: unread, from, since, subject), `email_read`, `email_send`, `email_reply`.
- FR-25: Implement tools: `email_mark_read`, `email_mark_unread`.

### 6.5 Mailbox Management
- FR-26: Create mailbox: create directory under `/var/lib/aimx/`, register address in `config.toml`. No mail server restart required.
- FR-27: List mailboxes with message counts.
- FR-28: Delete mailbox with confirmation.

### 6.6 Channel Manager
- FR-29: Read trigger rules from `config.toml` per mailbox.
- FR-30: Support `cmd` trigger type: execute shell command with template variables (`{filepath}`, `{from}`, `{to}`, `{subject}`, `{mailbox}`, `{id}`, `{date}`).
- FR-31: Support optional match filters: `from` (glob), `subject` (substring), `has_attachment` (bool). All conditions AND.
- FR-32: Execute triggers synchronously during delivery. Log failures, never block delivery.

### 6.7 Inbound Trust
- FR-33: During `aimx ingest`, verify sender's DKIM signature and SPF record using `mail-auth` crate.
- FR-34: Store verification results in frontmatter (`dkim: pass|fail|none`, `spf: pass|fail|none`).
- FR-35: Support per-mailbox `trust` config: `none` (default, all triggers fire) or `verified` (triggers only fire on DKIM-pass).
- FR-36: Support optional `trusted_senders` allowlist per mailbox (glob patterns). Allowlisted senders always trigger, skip verification.
- FR-37: Mail is always stored regardless of trust result. Trust only gates trigger execution.

### 6.8 Verify Service
- FR-38: Hosted probe service at `check.aimx.email` that performs SMTP EHLO handshake with caller's IP on port 25, confirming a real SMTP server is responding.
- FR-39: ~~Hosted email endpoint at `verify@aimx.email` that receives test email and sends reply.~~ _Removed: email echo eliminated to avoid backscatter risk and MTA dependency on the verify server. DKIM/SPF verification is handled by DNS record checks during setup instead._
- FR-39b: Port 25 listener on the verify service that accepts TCP connections, allowing aimx clients to test outbound port 25 reachability.
- FR-40: Verify service is open source and self-hostable. No MTA required on the verify server.

### 6.9 CLI Commands
- FR-41: `aimx setup <domain>` — interactive setup wizard.
- FR-42: `aimx preflight` — run port/DNS checks without installing.
- FR-43: `aimx ingest <rcpt>` — delivery command (called by OpenSMTPD).
- FR-44: `aimx send` — compose, sign, and send email.
- FR-45: `aimx mcp` — start MCP server in stdio mode.
- FR-46: `aimx mailbox create|list|delete <name>` — mailbox management.
- FR-47: `aimx status` — show server status, mailbox counts, recent activity.
- FR-48: `aimx verify` — check port 25 connectivity (outbound via check service port 25, inbound via EHLO probe, PTR record).

## 7. Non-Functional Requirements

- **NFR-1: Single binary.** The entire aimx tool compiles to one static binary. No runtime dependencies beyond OpenSMTPD.
- **NFR-2: No daemon.** aimx never runs as a long-lived process (except MCP stdio sessions). OpenSMTPD is the only daemon.
- **NFR-3: Permissive licensing.** All aimx code and dependencies must use MIT, Apache-2.0, ISC, or BSD licenses. No GPL/AGPL.
- **NFR-4: Linux only.** Target Debian/Ubuntu initially. Other distributions are best-effort.
- **NFR-5: Minimal resource usage.** aimx ingest must complete in < 1 second for typical emails (< 10MB).
- **NFR-6: Secure defaults.** Self-signed TLS cert for STARTTLS (generated during setup, no Let's Encrypt needed), DKIM signing on all outbound, DMARC reject policy, SPF strict.
- **NFR-7: Filesystem-based storage.** No database. Mailboxes are directories. Configuration is TOML.

## 8. Technical Considerations

### Language and Stack
- **Rust** — single binary, no runtime, strong ecosystem for email/crypto.
- Key crates: `mail-parser` (MIME parsing), `mail-auth` (DKIM signing), `rmcp` (MCP SDK), `clap` (CLI), `serde`+`toml` (config).

### System Dependencies
- **OpenSMTPD** — SMTP transport. Installed via `apt`. Configured by `aimx setup` to pipe all incoming mail to `aimx ingest`.
- **TLS certificates** — Self-signed cert generated during `aimx setup`. Sufficient for STARTTLS on port 25 (MTAs don't validate certs for opportunistic encryption). No Let's Encrypt or certbot needed.

### Storage Layout
```
/var/lib/aimx/
├── config.toml
├── dkim/
│   ├── private.key
│   └── public.key
├── <mailbox>/
│   ├── YYYY-MM-DD-NNN.md
│   └── attachments/
│       └── <filename>
└── catchall/
    └── ...
```

### Architecture
- No custom networking code. OpenSMTPD handles all SMTP.
- `aimx ingest` is a short-lived process invoked per message by OpenSMTPD's MDA.
- `aimx mcp` is a stdio process launched per MCP session.
- `aimx send` is a one-shot CLI invocation.

### Integration Points
- OpenSMTPD MDA pipe (`/usr/local/bin/aimx ingest %{rcpt}`)
- MCP stdio transport (Claude Code, OpenClaw, any MCP client)
- Channel manager triggers (arbitrary shell commands)

## 9. Scope and Milestones

### In Scope (v1)
- Setup wizard with preflight, DNS guidance, OpenSMTPD config, DKIM keygen
- Email delivery pipeline (EML→Markdown with attachments)
- Email sending with DKIM signing
- MCP server with full email/mailbox tool set
- Channel manager with `cmd` triggers and match filters
- Inbound trust: DKIM/SPF verification, per-mailbox trust policy, trusted_senders allowlist
- Verify service (probe + end-to-end test)
- CLI for all operations
- Linux (Debian/Ubuntu) only
- Build from source (cargo install)

### Out of Scope (future consideration)
- Package manager distribution (apt/brew/nix)
- `webhook` trigger type
- Multi-tenant / hosted offering
- Web dashboard
- Non-Linux platforms
- IMAP/POP3/JMAP access
- Email encryption (PGP/S/MIME)
- Rate limiting / spam filtering (rely on OpenSMTPD + DMARC)

### Milestones

| Milestone | Description | Key Deliverables |
|-----------|-------------|-----------------|
| M1: Core Pipeline | Receive and store email as Markdown | `aimx ingest`, EML→MD parser, attachment extraction, mailbox routing |
| M2: Outbound | Send email with DKIM | `aimx send`, DKIM key generation, DKIM signing, RFC 5322 composition |
| M3: MCP Server | Agent access via MCP | `aimx mcp` with all email/mailbox tools, stdio transport |
| M4: Channel Manager | Trigger actions on incoming mail | `cmd` triggers, match filters, config.toml rules |
| M5: Inbound Trust | Gate triggers on sender verification | DKIM/SPF verification during ingest, per-mailbox trust policy, trusted_senders allowlist |
| M6: Setup Wizard | One-command setup | `aimx setup`, preflight checks, OpenSMTPD config, DNS guidance, verification |
| M7: Verify Service | Hosted verification endpoint | `check.aimx.email` probe, `verify@aimx.email` endpoint, self-hostable |
| M8: Polish | CLI completeness and docs | `aimx status`, `aimx preflight`, `aimx verify`, README, usage docs |

## 10. Risks and Mitigations

| Risk | Likelihood | Impact | Mitigation |
|------|-----------|--------|------------|
| Port 25 blocked on many cloud providers | High | Limits addressable market | Clear documentation of compatible providers. Preflight check catches this early. |
| Outbound mail flagged as spam by Gmail/Outlook | Medium | Agent emails don't reach recipients | Proper DKIM/SPF/DMARC. PTR record guidance. Gmail filter whitelist instructions. |
| OpenSMTPD configuration complexity | Low | Setup fails or is fragile | aimx generates the config automatically. Tested against Debian/Ubuntu packages. |
| MIME parsing edge cases | Medium | Some emails render poorly as Markdown | Use battle-tested `mail-parser` crate. Accept that HTML-heavy emails may lose formatting. |
| `mail-auth` crate license compatibility | Low | Can't use for DKIM signing | Verify license before development. Fallback: `mini-mail-auth` or implement RSA-SHA256 signing directly. |
| Verify service availability | Low | Setup can't run end-to-end test | Make verify service optional (warn, don't block). Provide self-hosted alternative. |

## 11. Resolved Decisions

1. **TLS** — Self-signed cert generated during setup. Sufficient for STARTTLS on port 25 (MTAs don't validate certs). No Let's Encrypt needed.
2. **Email size limits** — Use OpenSMTPD defaults. No separate aimx limit. Trusted environment.
3. **Mailbox deletion** — Deletes the directory and all emails. No archiving.
4. **Config hot-reload** — Not needed. `aimx ingest` is short-lived and reads config on each invocation.
5. **Read/unread tracking** — `read = false` field in TOML frontmatter, set on ingest. `email_mark_read` updates to `read = true`. Self-contained, grepable, no extra files.

