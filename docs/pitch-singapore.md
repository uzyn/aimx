# AIMX — Pitch for AI Engineer Singapore

> Venue: [ai.engineer/singapore](https://www.ai.engineer/singapore)
> Speaker: U-Zyn Chua
> Format: 5-minute lightning / 15-minute talk (scalable)

---

## One-liner

**AIMX is self-hosted email for AI agents. One binary. One setup command. No middleman.**

---

## The 30-second pitch

Your agents have a server. They have a domain. They have MCP. But when they need to send or receive email, you hand them over to Gmail, OAuth, and a third-party SaaS that reads every message they write.

AIMX fixes that. `aimx setup agent.yourdomain.com` — and your agent has its own SMTP server, its own DKIM keys, its own mailbox, and an MCP interface. Incoming mail lands as Markdown files. Outbound mail is DKIM-signed and delivered directly to the recipient's MX. No OAuth tokens. No API keys. No vendor.

It's the most boring infrastructure in the world, finally built the way agents actually use it.

---

## Talk structure (15 min)

### 1. The hook — "Why does your agent borrow someone else's inbox?" (1 min)

Open on the absurdity:

> You rent a VPS for your agent. You hand-tune its prompts. You wire up MCP servers, tool calls, custom retrieval. And when it's time to send an email, you spend three hours in Google Cloud Console setting up OAuth 2.0 refresh tokens.
>
> Why? The server already has port 25. SMTP has been around since 1982. Your agent is more capable than most humans who ran mail servers in the 90s. Why is it asking Gmail for permission?

### 2. The problem — three bad options (2 min)

Every agent builder in this room has hit this wall:

- **Gmail / OAuth route** → Cloud Console, consent screens, refresh tokens, SSH tunnels for headless auth, and the looming risk of a `Suspicious activity detected` ban the moment your agent looks like a bot. Which it is.
- **SaaS route** (AgentMail and friends) → Every email your agent sends and receives passes through someone else's servers. Your data. Their infrastructure. Their pricing page. Their sunset announcement.
- **DIY route** → Postfix. Dovecot. OpenDKIM. A weekend of YAML. A second weekend of debugging why Gmail thinks you're spam.

All three are absurd, because **the agent operator already owns a perfectly capable Unix box.**

### 3. The insight (1 min)

An AI agent is not a human mail user. It doesn't need:

- IMAP. It doesn't have a GUI client.
- A webmail interface. It reads files.
- A folder hierarchy designed for 1995. It has a filesystem.
- OAuth. It already has root.

An agent needs: **an address that receives mail, a function that sends mail, and a format it can `cat` and understand.** That's it.

### 4. The solution — AIMX (3 min)

```bash
sudo aimx setup agent.yourdomain.com
```

One command:

- Preflight-checks port 25 (inbound + outbound).
- Generates a 2048-bit DKIM keypair.
- Prints the exact DNS records you need (A, MX, SPF, DKIM, DMARC, PTR).
- Installs a systemd unit for the embedded SMTP listener.
- Writes a config file and an agent-facing `README.md` so your LLM understands the mailbox layout the moment you point it at `/var/lib/aimx/`.

Then:

- **Inbound** — mail hits port 25, is parsed, and written as Markdown with TOML frontmatter. One file per email. `cat` it and you understand it. No MIME parser required on the agent side.
- **Outbound** — `aimx send` composes RFC 5322, DKIM-signs it, resolves the recipient's MX, and delivers directly. No relay. No API.
- **MCP** — `aimx mcp` exposes `mailbox_list`, `email_read`, `email_send`, `email_reply`, `email_mark_read` over stdio. Claude Code, Codex CLI, Gemini CLI, OpenCode, Goose, OpenClaw — one command each: `aimx agent-setup claude-code`.
- **Channel triggers** — `config.toml` lets you fire a shell command on incoming mail, with match filters on sender, subject, or attachments. Your agent gets invoked when the email arrives, not when it remembers to poll.

All in one Rust binary. No runtime. No daemons except the SMTP listener itself.

### 5. Live demo (3 min)

Three moments on stage:

1. `sudo aimx setup demo.aimx.email` — show the preflight checks, the DNS card, the systemd unit landing.
2. Send an email to `claude@demo.aimx.email` from a phone.
3. Show the Markdown file landing in `/var/lib/aimx/inbox/claude/`, then show Claude Code reading it via MCP and replying — all on screen. The reply lands in the sender's inbox with DKIM pass, in real time.

> The whole loop — human sends email, agent reads, agent replies, human receives — runs on one VPS and zero third-party services.

### 6. Why this matters for agent builders (2 min)

Three arguments, in order of how much the room will care:

1. **Sovereignty.** Agent communication is conversation data. If you wouldn't paste your prompts into a third-party SaaS, don't paste your agent's inbox into one either.
2. **Composability.** Email is the oldest, best-supported interop channel in computing. Every service, every human, every other agent already speaks it. Give your agent a real address and it plugs into everything without a single new integration.
3. **Event-driven agents.** Channel triggers mean your agent doesn't need a polling loop or a scheduler. An email arrives; your agent wakes up; your agent acts. This is the cheapest trigger primitive you will ever add to an agent system, and it works today.

### 7. What's next (1 min)

- v0.2 ships hardened setup (split `/etc/aimx/` config, root-only DKIM key, `aimx` system group gating `aimx send` via Unix domain socket, signed-sent-copy archiving).
- Inbound trust policies: per-mailbox `verified` mode gates triggers on DKIM-pass, so your agent only acts on authenticated senders.
- A hosted verifier service (`check.aimx.email`) so anyone can confirm port 25 reachability without setting up a second server.

Everything MIT-licensed. Everything on your box.

### 8. Call to action (30 sec)

```bash
cargo install aimx
sudo aimx setup agent.yourdomain.com
```

Give your agent an email address before you leave Marina Bay Sands.

- GitHub: `github.com/uzyn/aimx`
- Docs: the `book/` directory in the repo
- Me: [@uzyn](https://uzyn.com)

Questions I want from this room:
- What's the first thing you'd wire an agent mailbox to?
- What integrations break without a real SMTP identity?
- Who wants to co-host a verifier node in APAC?

---

## Slide / beat list (for a 5-minute lightning cut)

1. **Title** — "SMTP for agents. No middleman."
2. **Problem** — three bad options: Gmail/OAuth, SaaS, DIY. (One slide, three columns, all red.)
3. **Insight** — agents don't need IMAP; they need files.
4. **Solution** — one `aimx setup` command. Diagram: Sender → port 25 → `aimx serve` → `.md` file → MCP → agent.
5. **Demo** — phone sends email, file appears, agent replies. Live.
6. **Stack** — one Rust binary, Markdown + TOML storage, MCP native, channel triggers, DKIM signing built-in.
7. **Why now** — every agent framework just added MCP; email is the missing channel.
8. **CTA** — `cargo install aimx`. Repo URL. Stage exit.

---

## Tone notes

- Keep it builder-to-builder. The audience is AI engineers, not CIOs. No "enterprise-grade" language.
- Lean on concrete commands and file paths. Show the filesystem. Show the frontmatter. Developers trust what they can `ls`.
- Resist feature-listing. Pick three things (setup, Markdown, MCP) and hammer them.
- End on sovereignty, not features. The lasting thought should be: *my agent's inbox should live where my agent lives.*
