# Open-source and self-hosted email options for AI agents (as of 2026-10-16)

Method note: GitHub numbers (stars, forks, created/updated dates, language) come from the GitHub repository search API, pulled on 2026-10-16 through the GitHub MCP tool. For most agent and MCP repos only `updated_at` was returned, and that field also changes when a repo gets new stars or metadata edits, so it is a weak activity signal. For the mail-server repos `pushed_at` (last code push) was available and is reported instead. Several sites could not be fetched from this environment because of DNS failures: stalw.art, forwardemail.net, tailscale.com, developers.google.com and news.ycombinator.com. Claims about those sites rely on search-result summaries or GitHub READMEs and are marked as such.

---

## 1. Self-hostable projects built specifically as "email for AI agents"

### Takeaway
A crowded field of small open-source "agent inbox" projects appeared from February to October 2026. Most have 50–400 stars, and one outlier claims about 2.1k. They split into three architectures. (a) A VPS server with its own SMTP listener that needs inbound port 25, usually sending through a relay such as SES or Resend. (b) Cloudflare Workers with Cloudflare Email Routing for inbound, which avoids port 25 entirely and is the most common pattern. (c) Wrappers that poll an existing IMAP mailbox. Only one other project, Sentio, matches aimx's combination of own SMTP, direct-to-MX DKIM delivery and stdio MCP, and it is a much heavier multi-tenant platform. None of the projects found stores mail as plain Markdown files or uses local shell hooks the way aimx does.

### Cited Findings

**Architecture A: own SMTP listener on a server (needs inbound port 25)**

- **Anil-matcha/open-agent-email**: 2,099 stars, 392 forks, Python, repo created 2023-05-05, updated 2026-10-15. Describes itself as "Open-source email infrastructure for AI agents: secure inboxes, REST and MCP APIs, SMTP, human approval workflows, signed webhooks… Self-host with Docker and PostgreSQL." — [GitHub](https://github.com/Anil-matcha/open-agent-email)
  - License Apache-2.0. Inbound uses its own `aiosmtpd` listener: "your MX record points that domain at your server, which listens for mail on port 25" (default port 2525, mapped to 25). — [README](https://github.com/Anil-matcha/open-agent-email)
  - Outbound goes through a single configured SMTP relay, not direct to MX: "Every inbox sends through one relay you configure once, for example Amazon SES". Resend, SendGrid, Postmark and Gmail with an app password are also supported. — [README](https://github.com/Anil-matcha/open-agent-email)
  - MCP runs over HTTP (`--transport http` against `/mcp/rpc` with a bearer token) and exposes "96 tools". Storage is SQLite by default or PostgreSQL, with attachments kept in the database. Deployment is Docker Compose, with a production example for "one Linux host" behind nginx. The README describes the project as "single-user" and "under active development". — [README](https://github.com/Anil-matcha/open-agent-email)
- **truespar/sentio**: 253 stars, Rust, created 2026-08-23, updated 2026-10-16. Describes itself as an "Email inbox API for AI agents… A complete multi-tenant mail server in Rust: inbound and outbound, DKIM/SPF/DMARC/ARC, MTA-STS, DANE, three-tier anti-spam." — [GitHub](https://github.com/truespar/sentio)
  - Dual-licensed MIT or Apache-2.0. Runs its own SMTP on ports 25, 465 and 587. Outbound resolves MX and delivers directly with DKIM signing ("Outbound is DKIM-signed with the tenant's own key"), and an optional `[delivery.relay]` smarthost is available. The README warns that cloud providers and ISPs often block outbound port 25. — [README](https://github.com/truespar/sentio)
  - `sentio-mcp` is a separate **stdio** MCP server with 6 tools (list, get, send and reply for messages; list and create for mailboxes). It authenticates with a tenant-scoped API key, and binaries ship for Linux x86_64, Linux aarch64 and Windows x86_64. — [README](https://github.com/truespar/sentio)
  - The stack is heavy: PostgreSQL 18+ (a hard floor), Redis/Valkey, NATS JetStream, S3/MinIO, ClamAV and rspamd. Docker images are published for linux/amd64 and linux/arm64, and there is also a systemd unit. Inbound webhooks carry metadata only and are HMAC-SHA256 signed. The REST API has 116 operations. The README says the project is "Built for platforms", meaning multi-tenant use. — [README](https://github.com/truespar/sentio)
- **tokencanopy/e2a**: 194 stars, Go, created 2026-04-25, updated 2026-10-15. "Open-source email API for applications and AI agents. Managed hosting at e2a.dev, or self-host with Docker." — [GitHub](https://github.com/tokencanopy/e2a)
  - Apache-2.0. Inbound runs its own receiver built on `go-smtp`, with the domain's MX pointed at the relay host. Outbound sends agent-to-human mail through an upstream SMTP provider (SES, Resend or SendGrid). MCP is Streamable HTTP, hosted at `api.e2a.dev/mcp` or self-hosted on `:8765`, with OAuth 2.1 or a bearer key. A per-agent WebSocket and REST polling "work from a laptop or behind a firewall". There is a human-in-the-loop approval gate (`pending_review`). Deployment is Docker Compose with Postgres 14+. — [README](https://github.com/tokencanopy/e2a)
  - The Show HN thread for E2a lists gaps such as no DMARC support (SPF and DKIM only) and no HA, and a commenter pointed to a similar project, "nornweave" (search-result summary; HN itself could not be fetched). — [HN search result](https://news.ycombinator.com/item?id=48100227)
- **openagentemail/openagentemail**: 49 stars, TypeScript, Apache-2.0, created 2026-07-26. "Open communication and task handoffs for AI agents — self-hosted email, task threads, approvals, and webhooks over MCP and REST." — [GitHub](https://github.com/openagentemail/openagentemail). A related repo (adity982/openagentemail) calls itself "the open-source alternative to AgentMail. One compose file → unlimited inboxes, OTP extraction, MCP server". According to the search summary, it runs with Docker Compose on a VPS and "needs port 25 open and a domain you control". — [GitHub](https://github.com/adity982/openagentemail)
- **Autopilot** (Show HN): described as a self-hosted, drop-in replacement for AgentMail's SDK with unlimited inboxes (search-result summary only). — [HN](https://news.ycombinator.com/item?id=47785862)

**Architecture B: Cloudflare Workers with Email Routing (no inbound port 25, but depends on Cloudflare)**

- **chekusu/mails**: 368 stars, TypeScript, MIT, created 2026-03-17. Inbound comes "via Cloudflare Email Routing Worker". Outbound tries providers in the order "cloudflare,resend". Comes with a CLI (`npm install -g mails`; `claim`, `send`, `inbox`, `code`). There is a hosted mode (mails.dev, 100 free sends a month) or you can deploy your own Worker. Local storage is SQLite at `~/.mails/mails.db`. The README does not mention MCP. — [GitHub](https://github.com/chekusu/mails)
- **wong2/cf-mailroom**: 336 stars, TypeScript, Apache-2.0 per the README, created 2026-04-17. "Self-hosted email for humans and AI agents, entirely on Cloudflare." Inbound uses Email Routing to a Worker `email()` handler, outbound uses Cloudflare Email Sending, and storage is D1 plus R2. The Worker hosts a remote MCP server at `/mcp`, with OAuth 2.1 and tools `list_inboxes`, `search_conversations`, `get_conversation`, `reply_to_conversation` and `send_email`. It requires a Cloudflare-managed domain and Workers Paid for outbound. Limits: attachments of 3 MB or less, and a default of 100 MCP sends per identity per day. — [GitHub](https://github.com/wong2/cf-mailroom)
- **choyiny/saasmail**: 259 stars, TypeScript, Apache-2.0, created 2026-04-15. "Self-hosted email server for SaaS teams on Cloudflare Workers with WebMCP support". Inbound uses Cloudflare Email Routing only. Outbound can use Cloudflare Email Sending, Resend, Bavimail or Postmark. It offers MCP over OAuth 2.1 and WebMCP with "20 in-page tools" that read and draft but never send. Storage is D1, R2 and Queues. Deployment is done through a Claude Code onboarding skill. — [GitHub](https://github.com/choyiny/saasmail)
- **boringcomputers/goshen-email**: 166 stars, TypeScript, created 2026-09-15. "Email for AI agents, hosted on Cloudflare. Inboxes with an HTTP API, MCP server, SDKs, and a dashboard." The license was not checked. — [GitHub](https://github.com/boringcomputers/goshen-email)
- **agentteamhq/agentteam-email**: 60 stars, MIT, created 2026-06-19. Inbound goes through a Cloudflare Email Routing Worker, and "inbound mail lands in R2". It provides a CLI (`at-email`) plus an agent skill. Deployment is Docker Compose or Helm. The README does not mention MCP. — [GitHub](https://github.com/agentteamhq/agentteam-email)
- **Mailslot**: MIT. Deploys to your own Cloudflare account and domain, with MCP, HTTP API, webhooks and read-once OTP extraction (search-result summary). — [mailslot.dev](https://mailslot.dev/)
- **dearagent.sh**: a single Cloudflare Worker, with D1 for mail and R2 for attachments, plus REST, MCP and webhooks (search-result summary). — [dearagent.sh](https://dearagent.sh/)

**Hybrid that runs on a desktop or laptop and avoids port 25: the closest analog to aimx's planned direction**

- **agenticmail/agenticmail**: 237 stars, TypeScript, MIT, created 2026-02-14. "Email, SMS & phone-call infrastructure for AI agents". — [GitHub](https://github.com/agenticmail/agenticmail)
  - *Relay mode*: "RelayGateway polls Gmail IMAP every 30 seconds". Outbound goes through the user's Gmail or Outlook SMTP as `owner+agentname@gmail.com`. No inbound port is needed. — [README](https://github.com/agenticmail/agenticmail)
  - *Domain mode*: Cloudflare Email Routing hands mail to a Worker, which POSTs to `/api/agenticmail/mail/inbound`. Cloudflare Tunnel provides "secure inbound traffic without exposing ports". It runs a **local Stalwart server in Docker**, and outbound is submitted to Stalwart on port 587, which DKIM-signs and delivers directly to MX, with an optional Gmail SMTP relay for "residential IPs without PTR records". — [README](https://github.com/agenticmail/agenticmail)
  - MCP is **stdio**. The README gives the tool count inconsistently as 80+, 90+ and 100. Install is `npm install -g @agenticmail/cli` or `curl … install.sh | bash`, with Node 22+. Platforms are macOS (brew plus LaunchAgent) and Linux (systemd user service). Windows is not mentioned. — [README](https://github.com/agenticmail/agenticmail)

**Architecture C: wrap an existing IMAP/SMTP mailbox for agents**

- **dominikamann/agent-mail-gateway**: 11 stars, TypeScript, MIT, created 2026-10-02. "give each AI agent its own IMAP/SMTP mailbox with sender/recipient allow lists, HTML-to-Markdown". Streamable HTTP MCP at `/mcp` plus a stdio bridge. One API key per mailbox, per-hour send limits, an optional LLM review of outgoing mail, and an audit log. Ships as a single Docker container with SQLite. — [GitHub](https://github.com/dominikamann/agent-mail-gateway)
- **fathomise/MailFathom**: 15 stars, C#/.NET 10, AGPL-3.0. Syncs IMAP into PostgreSQL and serves it to agents over MCP with semantic retrieval. — [GitHub](https://github.com/fathomise/MailFathom)

**Testing and disposable-inbox niche (overlaps for OTP and verification use cases)**

- **gsd-build/agent-inbox**: 62 stars, JS. "Give any AI agent a disposable email inbox in one tool call. MCP server for email verification, auth flows, and testing." — [GitHub](https://github.com/gsd-build/agent-inbox)
- **soulteary/owlmail**: 75 stars, Go. "self-hosted, AI-native email testing gateway… REST, Webhooks, WebSocket, and MCP." — [GitHub](https://github.com/soulteary/owlmail)
- **axllent/mailpit**: 10,668 stars, MIT, Go, pushed 2026-10-16. "An email and SMTP testing tool with API for developers". MCP support was not checked. — [GitHub](https://github.com/axllent/mailpit)

**Naming collisions and adjacent projects (not SMTP email servers)**

- **Dicklesworthstone/mcp_agent_mail**: 2,209 stars, Python, pushed 2026-10-15. "Asynchronous coordination layer for AI coding agents: identities, inboxes, searchable threads… over FastMCP + Git + SQLite". This is agent-to-agent messaging, not internet email. — [GitHub](https://github.com/Dicklesworthstone/mcp_agent_mail)
- **TryCaspian/caspian-sdk**: 974 stars, created 2026-07-20. A multi-channel agent communication SDK covering email, WhatsApp, Slack and SMS. — [GitHub](https://github.com/TryCaspian/caspian-sdk). **BerriAI/agentchat**: 139 stars, created 2026-09-28. A unified SDK for connecting agents to Slack, Discord, Teams and email. — [GitHub](https://github.com/BerriAI/agentchat)
- **ghostwright/phantom**: 1,483 stars. "An AI co-worker with its own computer… MCP server… email identity", built on the Claude Agent SDK. — [GitHub](https://github.com/ghostwright/phantom)
- **macro-inc/macro**: 4,691 stars, Rust. A unified workspace covering email, chat, docs and agents, with MCP. — [GitHub](https://github.com/macro-inc/macro)
- SDK-only repos for hosted services, which cannot be self-hosted: **KeyID-AI/sdk-js** (357 stars, 2 forks) and **KeyID-AI/sdk-py** (301 stars, 0 forks), both "Free email for AI agents. No signup" — [GitHub](https://github.com/KeyID-AI/sdk-js). **inkbox-ai/inkbox** (100 stars). — [GitHub](https://github.com/inkbox-ai/inkbox)
- Discovery list: **haoruilee/awesome-agent-native-services** (370 stars) catalogs agent-native email, browser and memory services. — [GitHub](https://github.com/haoruilee/awesome-agent-native-services)

### Inferences
- aimx's specific combination is not duplicated by any project found: a single static binary, no database, its own SMTP with direct-to-MX DKIM delivery, Markdown plus TOML files on disk, stdio MCP, and local shell hooks. Sentio is the nearest technical match (Rust, own SMTP, direct MX with DKIM, stdio MCP), but it needs Postgres 18, Redis, NATS, S3, ClamAV and rspamd and is aimed at multi-tenant platforms. aimx's lightweight, single-user, file-native position looks open.
- Every other self-hosted competitor stores mail in SQLite, Postgres or D1 and exposes HTTP APIs and webhooks. Plain Markdown files that agents can read with ordinary file tools, plus local shell hooks, are a real differentiator for local coding agents such as Claude Code or Codex. It is also a niche feature that HTTP-first platforms are not chasing.
- Cloudflare Email Routing to a Worker is the de facto open-source answer to "no port 25". At least 7 projects use it (mails, cf-mailroom, saasmail, goshen, agentteam, Mailslot, dearagent, and agenticmail's domain mode). Any aimx "no port 25" mode will be compared against it. agenticmail already ships the full pattern, a Cloudflare Worker plus Cloudflare Tunnel to a local machine running Stalwart, on macOS and Linux via npm. It is the closest direct competitor to the planned aimx direction.
- Most VPS-style competitors (open-agent-email, e2a) send through SES, Resend or SendGrid rather than directly to MX. Direct-to-MX delivery is a feature but also a liability on residential and cloud IPs. A smarthost option is table stakes (Sentio, mox and agenticmail all offer one).
- The 2,099 stars on open-agent-email conflict with its 2023 repo creation date, which predates the agent-email category, and with its 121 commits. The repo may have been repurposed. The very low fork counts on KeyID's 357- and 301-star SDK repos (2 and 0 forks) are also unusual. Star counts in this category should be treated with caution.

### Gaps
- Could not verify licenses, star counts or architecture for "nornweave", Autopilot, Mailslot or dearagent.sh (no GitHub repo located, or the HN and other pages could not be fetched).
- Could not confirm whether goshen-email or agentteam-email ship an MCP server or what their outbound transports are.
- GitHub repo search was capped at the top 20–30 results per query. Long-tail projects under about 10 stars are under-sampled.
- MCP registry counts (registry.modelcontextprotocol.io, Smithery, Glama, PulseMCP) were not retrieved.

---

## 2. Popular email MCP servers that wrap existing mailboxes

### Takeaway
The "wrap my existing mailbox" category is far larger and more cross-platform than the agent-inbox category. Leaders include taylorwilsdon/google_workspace_mcp (3.3k stars), the archived GongRzhe Gmail server (1.2k), and Wh1isper/mcp-email-server (361, Windows/macOS/Linux via `uvx`). There is a long tail of Apple Mail, Thunderbird, Fastmail/JMAP, Outlook/Graph and Proton Bridge servers. These servers give an agent access to a human's mailbox, not an agent-owned address on its own domain. Gmail access carries OAuth and Google restricted-scope verification burdens (CASA). Google now runs its own Gmail MCP endpoint in Developer Preview, and per third-party reviews it cannot send mail.

### Cited Findings

**Gmail and Google Workspace**
- **taylorwilsdon/google_workspace_mcp**: 3,344 stars, 1,059 forks, Python, updated 2026-10-15. "Control Gmail, Google Calendar, Docs, Sheets… Comprehensive Google Workspace MCP Server & CLI Tool". — [GitHub](https://github.com/taylorwilsdon/google_workspace_mcp)
- **GongRzhe/Gmail-MCP-Server**: 1,168 stars, JavaScript, **archived**. "MCP server for Gmail integration in Claude Desktop with auto authentication support". — [GitHub](https://github.com/GongRzhe/Gmail-MCP-Server)
- **a-bonus/google-docs-mcp**: 674 stars, covering Docs, Sheets, Drive, Gmail and Calendar — [GitHub](https://github.com/a-bonus/google-docs-mcp). **aaronsb/google-workspace-mcp**: 191 stars — [GitHub](https://github.com/aaronsb/google-workspace-mcp). **jasonsum/gmail-mcp-server**: 72 stars — [GitHub](https://github.com/jasonsum/gmail-mcp-server). **shinzo-labs/gmail-mcp**: 62 stars — [GitHub](https://github.com/shinzo-labs/gmail-mcp). **baryhuang/mcp-headless-gmail**: 55 stars, "without local credential or token setup" — [GitHub](https://github.com/baryhuang/mcp-headless-gmail)
- **Google's official Gmail MCP server**: Google-hosted, in Developer Preview, at `https://gmailmcp.googleapis.com/mcp/v1` over HTTP. Setup requires enabling the separate `gmailmcp.googleapis.com` API and using OAuth 2.0. These details come from the search-result summary of Google's reference and configuration pages, which could not be fetched directly. — [Google MCP reference](https://developers.google.com/workspace/gmail/api/reference/mcp?authuser=0), [configure guide](https://developers.google.com/workspace/gmail/api/guides/configure-mcp-server)
  - A third-party review says it launched 2026-04-22 with about 10–11 tools (search, read, labels, drafts) and **no send tool**, but that the connection requests the `gmail.compose` scope, which also covers sending. — [dragapp](https://www.dragapp.com/blog/gmail-mcp-server/), [vorplabs](https://vorplabs.com/agent-tools/gmail-mcp). Reviewers disagree on the exact tool count and launch date.
- **Gmail OAuth friction**: Gmail read, modify and full-access scopes are "restricted". Apps that use them from or through a third-party server need a CASA Tier 2 security assessment, which is revalidated yearly. Vendor estimates put it at roughly $540–$1k a year through self-serve labs, with verification taking 2–8 weeks. — [Unipile](https://www.unipile.com/integrating-google-oauth-2-0-user-authentication-into-your-app/). The compose and insert scopes are also restricted. — [Nylas scopes reference](https://developer.nylas.com/docs/cookbook/use-cases/build/google-oauth-scopes.md). Both are vendor sources, not Google documentation.

**Generic IMAP/SMTP (works with any provider, including Gmail app passwords)**
- **Wh1isper/mcp-email-server**: 361 stars, 134 forks, Python, BSD-3-Clause, updated 2026-10-15. "supports Windows, macOS, and Linux". Install is `uvx mcp-email-server@latest stdio` (stdio transport), with a browser UI for account setup that autofills settings from the email domain. Linux amd64/arm64 Docker images are available. Auth is password-based, and the README does not mention OAuth. — [GitHub](https://github.com/Wh1isper/mcp-email-server)
- **codefuturist/email-mcp**: 128 stars, TypeScript, IMAP and SMTP, tagged protonmail-bridge — [GitHub](https://github.com/codefuturist/email-mcp). **nikolausm/imap-mcp-server**: 111 stars, multi-account, AES-256-encrypted credentials — [GitHub](https://github.com/nikolausm/imap-mcp-server). **tecnologicachile/mail-mcp**: 103 stars, **Rust**, IMAP/SMTP/EWS/Graph with OAuth2 for Microsoft 365, Gmail and Zoho — [GitHub](https://github.com/tecnologicachile/mail-mcp). **non-dirty/imap-mcp**: 58 stars — [GitHub](https://github.com/non-dirty/imap-mcp). **yunfeizhu/mcp-mail-server**: 51 stars, npm/npx — [GitHub](https://github.com/yunfeizhu/mcp-mail-server). **leeguooooo/mail-use**: 45 stars, CLI plus MCP — [GitHub](https://github.com/leeguooooo/mail-use). **n24q02m/email**: 35 stars — [GitHub](https://github.com/n24q02m/email)

**Desktop-client bridges (macOS and Windows friendly, reuse the client's accounts)**
- **TKasperczyk/thunderbird-mcp**: 384 stars, 112 forks, JS, created 2025-12-17. Email, contacts and calendars through Thunderbird — [GitHub](https://github.com/TKasperczyk/thunderbird-mcp). **vitalio-sh/thunderbird-cli**: 58 stars — [GitHub](https://github.com/vitalio-sh/thunderbird-cli)
- **patrickfreyer/apple-mail-mcp**: 230 stars, Python/AppleScript — [GitHub](https://github.com/patrickfreyer/apple-mail-mcp). **s-morgan-jeffries/apple-mail-fast-mcp**: 106 stars — [GitHub](https://github.com/s-morgan-jeffries/apple-mail-fast-mcp). **sweetrb/apple-mail-mcp**: 92 stars — [GitHub](https://github.com/sweetrb/apple-mail-mcp). **imdinu/apple-mail-mcp**: 72 stars, FTS5 search because "AppleScript-based servers timeout" on large mailboxes — [GitHub](https://github.com/imdinu/apple-mail-mcp). **MrGo2/icloud-mcp**: 66 stars — [GitHub](https://github.com/MrGo2/icloud-mcp)

**Fastmail and JMAP**
- **wyattjoh/jmap-mcp**: 176 stars, TypeScript/Deno, generic JMAP — [GitHub](https://github.com/wyattjoh/jmap-mcp). **MadLlama25/fastmail-mcp**: 131 stars, unofficial, includes a Claude Desktop DXT — [GitHub](https://github.com/MadLlama25/fastmail-mcp). **radiosilence/fastmail-cli**: 75 stars, Rust, JMAP + CardDAV — [GitHub](https://github.com/radiosilence/fastmail-cli)

**Outlook and Microsoft Graph**
- **desek/outlook-local-mcp**: 44 stars, Go, Graph API — [GitHub](https://github.com/desek/outlook-local-mcp). Also tecnologicachile/mail-mcp (Graph and EWS, above).

**Proton and Tuta (via local bridges)**
- **googlarz/proton-mail-bridge-client**: 73 stars. "Local-first Proton Mail MCP server and CLI via Proton Bridge… 96 tools, read-only and send-to-self modes" — [GitHub](https://github.com/googlarz/proton-mail-bridge-client). **amotivv/protonmail-mcp**: 49 stars, send-only via Proton SMTP — [GitHub](https://github.com/amotivv/protonmail-mcp). **spartanz51/tutabridge**: 63 stars, a Rust/Tauri IMAP/SMTP bridge for Tuta, tagged mcp — [GitHub](https://github.com/spartanz51/tutabridge)

**Official send-only MCPs from transactional providers (adjacent; SaaS-dependent)**
- **resend/resend-mcp**: 576 stars — [GitHub](https://github.com/resend/resend-mcp). **mailtrap/mailtrap-mcp**: 66 stars — [GitHub](https://github.com/mailtrap/mailtrap-mcp). **ActiveCampaign/postmark-mcp**: 57 stars, 24 tools — [GitHub](https://github.com/ActiveCampaign/postmark-mcp)

### Inferences
- Mailbox-wrapper MCPs already set the bar for easy cross-platform installs (`uvx` or `npx` one-liners, desktop extensions, browser-based setup). An easy-install aimx on macOS and Windows will be compared with these for setup friction, even though they solve a different problem: access to a human's inbox rather than giving the agent its own identity.
- Gmail's restricted-scope and CASA burden, and the official Gmail MCP having no send tool, both push builders toward agent-owned domains or IMAP app passwords. That supports aimx's thesis of a dedicated agent mailbox on your own domain.
- The archived top Gmail MCP (GongRzhe) and the many small forks suggest the wrapper space is fragmented, with few long-lived maintainers.

### Gaps
- Could not fetch Google's official Gmail MCP documentation directly, so the tool count, launch date and scope details rest on third-party reviews.
- No authoritative data was found on Gmail API rate limits specific to MCP usage, or on whether Google restricts agent or automated access beyond the CASA process.
- An Outlook or Graph MCP with more than about 100 stars was not identified. Microsoft's own MCP offerings were not researched.

---

## 3. Self-hosted mail servers and their agent/MCP features

### Takeaway
No major self-hosted mail server ships a first-party MCP server as of October 2026. Stalwart (15.1k stars, Rust) is the only one with built-in LLM features, used for spam filtering and message analysis and historically gated to enterprise users and sponsors. A third-party Stalwart MCP connector exists. All of these servers assume a Linux server with open port 25, except Stalwart, which also documents macOS, FreeBSD and Windows installs. Mox has strong agent-friendly primitives (webapi plus webhooks for incoming mail, and smarthost transports) but runs only on Unix.

### Cited Findings

| Project | Stars (2026-10-16) | Last push | License | Lang | Source |
|---|---|---|---|---|---|
| docker-mailserver | 19,591 | 2026-10-13 | MIT | Shell | [GitHub](https://github.com/docker-mailserver/docker-mailserver) |
| Postal | 16,864 | 2026-10-06 | MIT | Ruby | [GitHub](https://github.com/postalserver/postal) |
| Mail-in-a-Box | 15,440 | 2026-09-26 | CC0-1.0 | Python | [GitHub](https://github.com/mail-in-a-box/mailinabox) |
| Stalwart | 15,120 | 2026-10-15 | AGPL-3.0 / SELv2 (dual) | Rust | [GitHub](https://github.com/stalwartlabs/stalwart) |
| mailcow-dockerized | 13,618 | 2026-10-15 | GPL-3.0 | JS/Shell | [GitHub](https://github.com/mailcow/mailcow-dockerized) |
| Maddy | 6,110 | 2026-10-14 | GPL-3.0 | Go | [GitHub](https://github.com/foxcpp/maddy) |
| Mox | 5,899 | 2026-10-09 | MIT | Go | [GitHub](https://github.com/mjl-/mox) |
| Haraka | 5,618 | 2026-10-11 | MIT | JS | [GitHub](https://github.com/haraka/Haraka) |
| WildDuck | 2,110 | 2026-10-15 | EUPL-1.2 | JS | [GitHub](https://github.com/zone-eu/wildduck) |

- **Stalwart**: dual-licensed AGPL-3.0 and the proprietary Stalwart Enterprise License v2. Install guides cover Linux, macOS, FreeBSD, Windows and Docker. Features include JMAP for Mail, "Webhooks for event-driven automation", and "LLM-driven spam filtering and message analysis". The README does not mention MCP. Sponsors paying "$5 per month or more will automatically receive a Enterprise edition license". The project says it is "feature complete", with 1.0 as the next goal. — [GitHub README](https://github.com/stalwartlabs/stalwart)
  - Stalwart's blog announced LLM support in v0.10.3, initially limited to Enterprise users and sponsors, for spam filtering, threat detection and classification. Its slides list LLM-assisted Sieve scripts as a paid feature and BERT spam classification for Q4 2026 (search-result summary; stalw.art could not be fetched). — [stalw.art blog/ai](https://stalw.art/blog/tags/ai/), [slides](https://stalw.art/slides)
  - A third-party "stalwart-mail-mcp" runs on Cloudflare Workers, speaks JMAP to Stalwart 0.16+, and uses its own MCP OAuth with app passwords. It is not confirmed to be official. — [Glama listing](https://glama.ai/mcp/servers/y5ndajgn3q)
- **Mox**: MIT. "Mox only compiles for and fully works on unix systems". On Windows it "compiles… but 'mox serve' does not yet work". Setup is `./mox quickstart you@example.com`. Outbound can go through a configured "Transport" (SMTP relay, authenticated submission or SOCKS proxy). "Mox webhooks can be used to receive updates about incoming and outgoing deliveries", and the webapi can send messages. No AI or MCP features. — [GitHub](https://github.com/mjl-/mox)
- **Maddy**: GPL-3.0, "one daemon with uniform configuration", and "IMAP storage is 'beta'". The README does not mention smarthost relaying, webhooks, APIs, AI or MCP. — [GitHub](https://github.com/foxcpp/maddy)
- **Mail-in-a-Box**: turns "a fresh Ubuntu 22.04 LTS 64-bit machine into a working mail server" (Postfix, Dovecot, Roundcube, Nextcloud, nsd4, OpenDKIM, SpamAssassin). It has no user-configurable options and offers "An API for all of the actions on the control panel". It warns that other services "might block or spam-filter email sent from your Mail-in-a-Box". No AI or MCP features. — [GitHub](https://github.com/mail-in-a-box/mailinabox)
- **docker-mailserver**: "Production-ready fullstack but simple mail server (SMTP, IMAP, LDAP, Antispam, Antivirus, etc.) running inside a container" — [GitHub](https://github.com/docker-mailserver/docker-mailserver). Its community discusses configuring it to relay both inbound and outbound mail through another host. — [discussion #3965](https://github.com/orgs/docker-mailserver/discussions/3965)
- **Postal**: "A fully featured open source mail delivery platform for incoming & outgoing e-mail" — [GitHub](https://github.com/postalserver/postal). **Haraka**: "A fast, highly extensible, and event driven SMTP server" — [GitHub](https://github.com/haraka/Haraka). **WildDuck**: "Opinionated email server" — [GitHub](https://github.com/zone-eu/wildduck). Agent or MCP features were not found in their repo descriptions.
- Adjacent single-binary Go tools with MCP: **dmarcguardhq/parse-dmarc** (213 stars), "One Go binary, IMAP in, SQLite, Prometheus metrics, MCP server" — [GitHub](https://github.com/dmarcguardhq/parse-dmarc). **herald-email/herald-mail-app** (148 stars), a terminal email client with "MCP tools" — [GitHub](https://github.com/herald-email/herald-mail-app)

### Inferences
- The large incumbent mail servers (13k–20k stars) have not moved into MCP or agent tooling. Integration happens through third-party connectors (Stalwart MCP) or through agent projects that embed a server (agenticmail embeds Stalwart in Docker). aimx is not competing head-on with these servers on full-mailserver features. Its competition is the agent-first layer built on top of them.
- Stalwart is the strongest potential threat or substrate. It is Rust, cross-platform (including macOS and Windows), has JMAP and webhooks, already has LLM features and a growing third-party MCP connector, and is used inside agenticmail. If Stalwart Labs shipped first-party MCP, it would cover much of aimx's server side, though not its Markdown-file or hook model.
- Mox's webhooks for incoming mail, its webapi and its Transports already offer agent-friendly building blocks and smarthost relaying in a single Go binary. Its lack of Windows and macOS server support mirrors aimx's current Linux-only limitation.

### Gaps
- Could not fetch Stalwart's docs (stalw.art DNS failure) to confirm smarthost/relay configuration, current LLM licensing (open-source vs enterprise), or official MCP plans.
- Did not verify install-complexity specifics or smarthost support for mailcow, Postal, Haraka or WildDuck from primary docs.
- Did not find primary sources on which of these servers can receive mail without inbound port 25. All appear to assume a public MX on port 25, except where fronted by a relay.

---

## 4. Open-source forwarding and relay projects relevant to "no port 25"

### Takeaway
The open-source alias forwarders (SimpleLogin 7.1k stars, addy.io 4.9k) and Forward Email (1.7k) are popular. Of the three, only Forward Email visibly advertises inbound email-to-webhook (JSON) delivery, and its "open source" code is actually licensed BUSL-1.1 plus MPL-2.0. In practice, the open-source agent projects solve no-port-25 inbound with Cloudflare Email Routing to a Worker rather than with these forwarders.

### Cited Findings
- **simple-login/app**: 7,059 stars, AGPL-3.0, Python, pushed 2026-10-14. "The SimpleLogin back-end and web app". — [GitHub](https://github.com/simple-login/app)
- **anonaddy/anonaddy (addy.io)**: 4,902 stars, AGPL-3.0, PHP, pushed 2026-10-06. "Anonymous email forwarding". — [GitHub](https://github.com/anonaddy/anonaddy)
- **forwardemail/forwardemail.net**: 1,684 stars, pushed 2026-10-13. The README footer lists the license as "(BUSL-1.1 AND MPL-2.0)" while calling it "the 100% open-source and privacy-focused email service". The repo has `SELFHOSTING.md`, a `self-hosting/` directory and docker-compose files, and runs SMTP, MX, IMAP, POP3, CalDAV, CardDAV and API servers. The README does not mention webhooks or MCP. — [GitHub](https://github.com/forwardemail/forwardemail.net)
  - Forward Email's site lists webhooks ("Inbound mail as JSON events") among its API features (search-result summary; the site could not be fetched). — [forwardemail.net open-startup](https://forwardemail.net/en/open-startup)
- Searches found no evidence that SimpleLogin or addy.io deliver inbound mail to an HTTP webhook. Results instead pointed to SaaS inbound-webhook providers (Mailgun routes, CloudMailin, Webhook Relay). — [Mailgun](https://documentation.mailgun.com/docs/mailgun/user-manual/receive-forward-store/receive-forward-store), [CloudMailin on DigitalOcean](https://docs.digitalocean.com/products/marketplace/catalog/cloudmailin/index.html.md), [Webhook Relay](https://webhookrelay.com/docs/email/)
- **MailLaser** is a self-hostable SMTP-to-webhook forwarder published as a Rust crate (search-result summary). — [docs.rs MailLaser](https://docs.rs/crate/MailLaser/latest)

### Inferences
- An aimx no-port-25 mode has two realistic ingress options. (a) A Cloudflare Email Routing Worker that pushes to the local daemon, which is free and the most-used pattern in this category but ties users to Cloudflare DNS. (b) A forwarder with webhooks, such as Forward Email or Mailgun, or plain forwarding to an IMAP mailbox that aimx polls, which is agenticmail's relay mode. SimpleLogin and addy.io forward to a mailbox, not a URL, so using them would need an IMAP-poll ingest path.
- Forward Email's BUSL licensing weakens any "fully open-source stack" claim for an aimx integration built on it.

### Gaps
- Could not confirm Forward Email's webhook payload format, signing, free-tier availability, or whether self-hosted installs support webhooks (forwardemail.net could not be fetched).
- Did not confirm from primary docs whether SimpleLogin or addy.io expose any inbound webhook or API for message content.

---

## 5. Open-source projects that tunnel inbound SMTP to a home or desktop machine

### Takeaway
No popular, dedicated open-source "inbound SMTP to your laptop" relay project was found. GitHub repos named "smtp tunnel" are DPI-evasion tools. The common approach is DIY: a VPS plus WireGuard or a generic TCP tunnel (frp 110k stars, rathole 14k, inlets), with VPS policy routing so outbound mail also leaves from the VPS IP for SPF and PTR. The only packaged open-source example of no-port-25 inbound to a local machine in the agent space is agenticmail's Cloudflare Email Routing, Worker and Cloudflare Tunnel chain.

### Cited Findings
- GitHub search for "smtp tunnel" (39 results) is led by **x011/smtp-tunnel-proxy** (1,799 stars), "A high-speed covert tunnel that disguises TCP traffic as SMTP email communication to bypass Deep Packet Inspection (DPI) firewalls". It is unrelated to receiving mail. — [GitHub](https://github.com/x011/smtp-tunnel-proxy)
- A GitHub search for "mail server vps wireguard relay inbound smtp home" returned 0 repositories (GitHub search API, 2026-10-16).
- Generic tunnel tools that can forward TCP port 25: **fatedier/frp**, 109,941 stars, "expose a local server behind a NAT or firewall to the internet" — [GitHub](https://github.com/fatedier/frp). **rathole-org/rathole**, 14,340 stars, Rust — [GitHub](https://github.com/rathole-org/rathole). **inlets/inlets-pro**, 573 stars, "Secure HTTP and TCP tunnels" (commercial) — [GitHub](https://github.com/inlets/inlets-pro). **cloudflare/cloudflared**, 16,150 stars, the Cloudflare Tunnel client — [GitHub](https://github.com/cloudflare/cloudflared)
- The DIY WireGuard pattern: the VPS forwards ports 25, 587, 465 and 993 to the home server over WireGuard. Outbound port-25 traffic is policy-routed back through the VPS so SPF passes, and HELO should match the VPS PTR. One setup falls back to the home connection if WireGuard breaks. — [blog.mynacol.xyz](https://blog.mynacol.xyz/en/mail-server-at-home/), [gist](https://gist.github.com/sussycatgirl/4bcd6508dedd21ea21ec494499c92618), [Trisquel forum](https://trisquel.info/en/forum/configuring-mail-server-local-machine-running-trisquel-and-port-forwarding-through-shared-serv)
- Rationale, from the same guides: most ISPs block port 25, residential IPs are commonly blacklisted, and many hosting providers block ports 25, 465 and 587 by default. — [blog.mynacol.xyz](https://blog.mynacol.xyz/en/mail-server-at-home/)
- **agenticmail** domain mode: Cloudflare Email Routing hands mail to a Worker, which POSTs to the local API through Cloudflare Tunnel ("secure inbound traffic without exposing ports"). An optional Gmail SMTP relay handles outbound from residential IPs. — [README](https://github.com/agenticmail/agenticmail)
- **e2a** avoids a public URL for agents by offering per-agent WebSocket and REST polling to its hosted or VPS relay ("work from a laptop or behind a firewall"). — [README](https://github.com/tokencanopy/e2a)

### Inferences
- There is a clear packaging gap: no open-source project turns "VPS or edge relay plus home or desktop mail store" into a one-command product. A relay, either self-hostable (small VPS or Cloudflare Worker) or hosted, that accepts mail on port 25 and pushes or pulls it to a local aimx over an outbound-only connection (WebSocket or HTTP long-poll, like e2a's laptop mode) would be a differentiated piece. Outbound mail would also need relaying through the same edge or a smarthost to keep SPF, DKIM and PTR aligned.
- Generic tunnels (frp, rathole) are popular but require a VPS with port 25 open and do not solve outbound reputation or PTR. They suit power users, not an "easy install".

### Gaps
- Could not fetch Tailscale Funnel docs to confirm port restrictions, which would rule out SMTP (tailscale.com DNS failure). Cloudflare Tunnel's limits on public inbound TCP for SMTP were not verified from primary docs.
- No popularity data (stars or users) was found for any dedicated home-mail-relay project, because none was located.
