# AIMX — Product Requirements Document

## 1. Overview

AIMX is a self-hosted email system for AI agents. It gives agents their own email addresses on a domain the user controls, with mail stored as Markdown files, MCP integration for agent access, and channel rules to trigger actions on incoming mail. One binary, one setup command, no third parties.

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
| Minimal operational burden | Long-running processes managed by AIMX | 1 (`aimx serve` — the only daemon, managed via systemd/OpenRC) |
| Developer adoption | GitHub stars / installs | Establish initial user base; exact target TBD |

## 4. User Personas

### Agent Operator (primary)
- **Description:** Developer who runs one or more AI agents on a dedicated VPS. Comfortable with Linux, DNS, and CLI tools. Already uses Claude Code or similar agentic frameworks.
- **Needs:** Give agents email addresses quickly. Read/send email programmatically. Trigger agent actions on incoming mail. Keep data on their own server.
- **Context:** SSH'd into a VPS. Has a domain they control. Wants to set up email in one sitting and never think about it again.

### Agent Framework Developer (secondary)
- **Description:** Developer building tools/frameworks for AI agents who wants to integrate email as a channel.
- **Needs:** Standard MCP interface for email. Predictable file-based storage that's easy to read programmatically. Minimal operational overhead.
- **Context:** Integrating AIMX into a larger agent system. Cares about the MCP API surface and file format stability.

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
- As an agent operator, I want to run `aimx agent-setup <agent>` so that my chosen agent is configured to use AIMX — including MCP wiring and agent-facing instructions — in one command, without hand-editing the agent's config file.

### P1 — Should Have
- As an agent operator, I want to filter channel triggers by sender, subject, or attachment presence so that agents only act on relevant emails.
- As an agent operator, I want email threading support (In-Reply-To, References) so that replies are grouped correctly in recipients' mail clients.
- As an agent operator, I want to check server status and mailbox counts with a single command so that I can verify the system is working.
- As an agent operator, I want email attachments extracted to the filesystem so that agents can access attached files directly.

### P2 — Nice to Have
- As an agent operator, I want end-to-end verification against a public verifier service so that I can confirm the full pipeline works after setup.

## 6. Functional Requirements

### 6.1 Setup Wizard (`aimx setup [domain]`)
- FR-1: Require root. Exit with clear message if not running as root.
- FR-1b: Detect port 25 conflict. If another process is listening on port 25, exit with process name and instructions to stop it.
- FR-2: Generate systemd unit file (or OpenRC init script on Alpine), enable and start `aimx serve` as the SMTP listener with TLS. (v0.2) Unit declares `RuntimeDirectory=aimx` so `/run/aimx/` exists at start with `root:root 0755`.
- FR-3: Run port 25 checks after `aimx serve` is started: outbound (connect to check service port 25), inbound (check service performs EHLO handshake back to caller).
- FR-4: Stop with clear error message if port 25 is blocked. List compatible VPS providers.
- FR-6: Generate 2048-bit RSA DKIM keypair. (v0.2) Private key written to `/etc/aimx/dkim/private.key` with mode `600` and owner `root:root` — never readable by non-root. Public key written to `/etc/aimx/dkim/public.key` with mode `644`.
- FR-7: Display required DNS records (MX, A, SPF, DKIM, DMARC) and wait for user confirmation. Reverse DNS (PTR) is the operator's responsibility (configured with the VPS provider) and is out of scope for `aimx setup`. When `enable_ipv6 = true` is set in `config.toml`, also include the AAAA record and add `ip6:<server_ipv6>` to the SPF mechanism, and verify both alongside the IPv4 records. When the flag is unset or `false`, IPv6 records are neither advertised nor verified — existing AAAA records in DNS are left unchanged.
- FR-8: Verify DNS records are correctly set.
- FR-9: Create default `catchall` inbox under `/var/lib/aimx/inbox/catchall/`. (v0.2) Catchall is inbox-only — no `sent/catchall/` is created (the catchall is a routing target for unknown local parts, not an identity that sends).
- FR-10: Recommend `aimx agent-setup <agent>` for one-command per-agent integration. The wizard lists supported agents and their install commands rather than emitting a generic, copy-paste JSON snippet.
- FR-1c: (v0.2) Write config to `/etc/aimx/config.toml` with mode `640`, owner `root:root`. `AIMX_CONFIG_DIR` env var overrides the location for tests and non-standard installs (alongside the existing `AIMX_DATA_DIR`).
- FR-1e: (v0.2) Write `/var/lib/aimx/README.md` (agent-facing layout guide; see FR-50b) on initial setup. Re-running setup refreshes the README if the baked-in version string differs.

### 6.2 Email Delivery (`aimx ingest <rcpt>`)
- FR-11: Accept raw email from the embedded SMTP listener (`aimx serve`) in-process, or from stdin for manual/pipe usage via `aimx ingest`.
- FR-12: Parse MIME message: extract headers, body (prefer text/plain, fall back to text/html→plaintext), and attachments.
- FR-13: (v0.2) Generate Markdown file with TOML frontmatter following the section ordering **Identity → Parties → Content → Threading → Auth → Storage → Outbound block (sent only)**. Required inbound fields: `id`, `message_id`, `thread_id`, `from`, `to`, `delivered_to`, `subject`, `date`, `received_at`, `received_from_ip`, `size_bytes`, `dkim`, `spf`, `dmarc`, `trusted`, `mailbox`, `read`. Optional inbound fields (omitted when empty rather than written as `null`): `cc`, `reply_to`, `attachments`, `in_reply_to`, `references`, `list_id`, `auto_submitted`, `labels`, `read_at` (RFC 3339 UTC; set by MARK-READ, removed by MARK-UNREAD — reflects "most recent read", not "first read"). Set `read = false` on ingest. `thread_id` is the first 16 hex chars of SHA-256 over the resolved thread root `Message-ID` (walk `In-Reply-To`/`References` backward; fall back to the message's own `Message-ID` when unresolvable). `received_at` is RFC 3339 UTC; `date` is sender-claimed RFC 3339. `received_from_ip` is the SMTP client IP. `delivered_to` is the actual RCPT TO (disambiguates list mail). `size_bytes` is the raw message size.
- FR-13b: (v0.2) Inbound files use the filename format `YYYY-MM-DD-HHMMSS-<slug>.md` (UTC). Slug algorithm: MIME-decode `Subject:` → lowercase → replace every non-alphanumeric character with `-` → collapse runs of `-` → trim leading/trailing `-` → truncate to 20 characters → empty result becomes `no-subject`. On filename collision, append `-2`, `-3`, … to the slug segment until free.
- FR-14: (v0.2) Attachment bundling — zero attachments produce a flat `<stem>.md`; one or more attachments produce a directory `<stem>/` containing `<stem>.md` plus all attachment files as siblings (Zola-style bundle). The previous mailbox-level `attachments/` directory is removed.
- FR-15: (v0.2) Route to `/var/lib/aimx/inbox/<mailbox>/` based on RCPT TO local part. Unrecognized local parts go to `inbox/catchall/`.
- FR-16: Execute matching channel rules after saving. Trigger failures are logged but do not block delivery.

### 6.3 Email Sending (`aimx send`)
- FR-17: Compose RFC 5322 compliant email from provided parameters (from, to, subject, body, attachments).
- FR-18: (v0.2) `aimx send` is a thin client. It composes the unsigned RFC 5322 message, opens the Unix domain socket at `/run/aimx/send.sock`, writes a length-prefixed `AIMX/1 SEND` request, reads the response, and exits with the corresponding status. The DKIM private key is **not** read by the client — `aimx serve` owns signing. If the socket is absent, `aimx send` exits non-zero with `aimx daemon not running — check 'systemctl status aimx'`.
- FR-18b: (v0.2) `aimx serve` binds `/run/aimx/send.sock` (`root:root 0666`, world-writable) alongside its SMTP listener. Each accept reads `SO_PEERCRED` and logs `peer_uid` / `peer_pid` to journald for diagnostics only — they are not used for authorization. The DKIM key is loaded once at startup into `Arc<DkimKey>` and shared across per-connection tasks. There is no send queue — concurrent sends are independent tokio tasks. **Authorization is explicitly out of scope in v0.2:** any local user on the host can submit mail through the socket. Operators needing isolation should restrict host access by other means (OS user policy, container boundaries).
- FR-18c: (v0.2) Wire protocol — length-prefixed and binary-safe.
  ```
  Client → Server:
    AIMX/1 SEND\n
    From-Mailbox: <name>\n
    Content-Length: <n>\n
    \n
    <n bytes of RFC 5322 message, unsigned>

  Server → Client:
    AIMX/1 OK <message-id>\n
  or
    AIMX/1 ERR <code> <reason>\n
      codes: MAILBOX | DOMAIN | SIGN | DELIVERY | TEMP | MALFORMED
  ```
- FR-18d: Domain and mailbox validation — `aimx serve` parses the `From:` header from the submitted message. The sender domain (case-insensitive) must equal the primary domain in `/etc/aimx/config.toml`; domain mismatch returns `ERR DOMAIN sender domain does not match aimx domain`. The sender local part must additionally resolve to an explicitly configured non-wildcard mailbox; the catchall wildcard (`*@domain`) matches **inbound routing only** and is never accepted as an outbound sender. Local-part miss returns `ERR MAILBOX no mailbox matches From: <addr>`. This closes a privilege gap where any local UDS client could sign mail as `anything@domain` by falling through to the catchall.
- FR-18e: `aimx serve` exposes additional state-mutation verbs on the same UDS socket (`/run/aimx/send.sock`): `MARK-READ`, `MARK-UNREAD`, `MAILBOX-CREATE`, `MAILBOX-DELETE`. These let the MCP server and the `aimx mailboxes` CLI mutate mailbox metadata and per-message state without writing files or config directly — the daemon is the single writer, which also avoids the daemon's in-memory Config drifting from disk after `aimx mailboxes create`. Each verb uses the same length-prefixed framing as `SEND` (with `Content-Length: 0` for bodyless verbs); success returns `AIMX/1 OK`, failure returns `AIMX/1 ERR <code> <reason>`. Clients (MCP server, `aimx mailboxes` CLI) fall back to direct on-disk edits when the daemon isn't running (first-time setup, teardown). Socket authorization remains out of scope per FR-18b — any local process can invoke these verbs.
- FR-19: (v0.2) `aimx serve` DKIM-signs the message (RSA-SHA256, relaxed/relaxed canonicalization) using the root-only `/etc/aimx/dkim/private.key`, then delivers the signed message directly to the recipient's MX server via SMTP (MX resolution + lettre transport). Default to IPv4-only outbound to match the SPF record generated by `aimx setup`. Support IPv6 as an opt-in via the `enable_ipv6` config flag — when enabled, the OS chooses the address family and the user is responsible for adding AAAA + `ip6:` SPF records. Return delivery errors immediately to the UDS client — no background queue.
- FR-19b: (v0.2) On successful delivery, `aimx serve` writes the **signed** message to `/var/lib/aimx/sent/<from-mailbox>/<stem>.md` (or a bundle folder when attachments are present), using the same filename format and slug algorithm as inbound (FR-13b). A single `Mutex<()>` guards the "allocate filename + create file" critical section to avoid stem collisions across concurrent sends.
- FR-19c: (v0.2) Sent-file frontmatter follows the same ordering as inbound (FR-13) and adds an outbound block: `outbound = true` (always), `delivery_status` (`"delivered"` | `"deferred"` | `"failed"` | `"pending"`, always written), and the optional fields `bcc` (only meaningful on the sent copy), `delivered_at` (RFC 3339 UTC when remote MX accepted), and `delivery_details` (last remote SMTP response).
- FR-19d: (v0.2) Field-omission rule — prefer omitting empty optional fields over writing `null`. Exceptions that are always written so "not evaluated" cannot be confused with "absent": `dkim`, `spf`, `dmarc`, `trusted`, `read`, `delivery_status`.
- FR-20: Support file attachments by path.
- FR-21: Set proper In-Reply-To and References headers when replying.

### 6.4 MCP Server (`aimx mcp`)
- FR-22: Run in stdio mode (launched on-demand by MCP client, no long-running process).
- FR-23: Implement tools: `mailbox_create`, `mailbox_list`, `mailbox_delete`.
- FR-24: Implement tools: `email_list` (with optional filters: unread, from, since, subject), `email_read`, `email_send`, `email_reply`.
- FR-25: Implement tools: `email_mark_read`, `email_mark_unread`.

### 6.5 Mailbox Management
- FR-26: (v0.2) Create mailbox: create both `/var/lib/aimx/inbox/<name>/` and `/var/lib/aimx/sent/<name>/`, register address in `/etc/aimx/config.toml`. The mailbox is a full identity (inbox + sent), not just an inbox. No mail server restart required.
- FR-27: (v0.2) List mailboxes by scanning `inbox/*/` (excluding `catchall`, or surfacing it as a distinct special entry), with message counts.
- FR-28: (v0.2) Delete mailbox — refuses with `ERR NONEMPTY` when either `inbox/<name>/` or `sent/<name>/` contains files. `aimx mailboxes delete --force <name>` wipes both directories' contents, then removes the config stanza; interactive `[y/N]` prompt shows per-directory file counts unless `--yes` is passed. CLI-only — MCP `mailbox_delete` does not gain a force variant; on `ERR NONEMPTY` it returns a hint pointing at the CLI command. Still refuses to delete the `catchall` mailbox.

### 6.6 Hook Manager
- FR-29: Read hook rules from `config.toml` per mailbox — `[[mailboxes.<name>.hooks]]` arrays-of-tables, one entry per hook. Required fields: `id` (12-char `[a-z0-9]`, auto-generated by `aimx hooks create`, format-validated on load, globally unique across all mailboxes), `event` (`on_receive` | `after_send`), `cmd`. Legacy `[[mailboxes.<name>.on_receive]]` schema refuses to load with a migration error (pre-launch; no compat shim).
- FR-30: Support `cmd` execution: the daemon spawns `sh -c <cmd>` with aimx-controlled template variables `{id}` and `{date}` substituted into the command string. All user-controlled fields are exposed as `AIMX_`-prefixed environment variables to avoid shell-injection via header values. `on_receive` env vars: `AIMX_FROM`, `AIMX_TO`, `AIMX_SUBJECT`, `AIMX_MAILBOX`, `AIMX_FILEPATH`, `AIMX_HOOK_ID`. `after_send` env vars: `AIMX_FROM`, `AIMX_TO`, `AIMX_SUBJECT`, `AIMX_MAILBOX`, `AIMX_HOOK_ID`, `AIMX_FILEPATH` (path to sent-copy `.md`), `AIMX_SEND_STATUS` (`delivered` | `failed` | `deferred`). Subprocess is spawned with `.env_clear()` + selective re-add of `PATH`, `HOME`, and the `AIMX_*` set (defense-in-depth per S47-2).
- FR-31: Support optional match filters per hook. `on_receive` accepts `from` (glob), `subject` (substring), `has_attachment` (bool). `after_send` accepts `to` (glob), `subject` (substring), `has_attachment` (bool). All conditions AND. Invalid event × filter combinations are rejected at config load (e.g. `from` on `after_send`, `to` on `on_receive`).
- FR-32: Execute hooks synchronously — `on_receive` fires during delivery (after the mail is stored, per FR-16); `after_send` fires immediately after the outbound MX attempt resolves (success, failure, or deferred). The daemon awaits each subprocess to completion for predictable timing but discards the exit code — hooks cannot abort delivery or the send. Failures logged at `warn` with the hook ID. Subprocess runtime > 5 s logged at `warn` for operator visibility into slow hooks.
- FR-32b: Every hook fire emits one `info`-level journald log line with the stable format `hook_id=<id> event=<e> mailbox=<m> (email_id=<id>|message_id=<id>) exit_code=<n> duration_ms=<n>`. This is the operator-level trace surface — there is no `hooks` field in email frontmatter.
- FR-32c: `aimx hooks create | list | delete` CLI manages hooks. `create` is flag-based (`--mailbox`, `--event`, `--cmd`, optional filters, optional `--dangerously-support-untrusted`), auto-generates the hook ID, prints it on success. `list` prints a table (global view; `--mailbox <name>` filters). `delete <id>` prompts interactively (`[y/N]`) showing the hook details unless `--yes`. No `update` verb — delete and recreate. `aimx hook` is retained as a clap alias.
- FR-32d: Hook CRUD routes through the daemon's UDS socket via new `HOOK-CREATE` / `HOOK-DELETE` verbs on the existing `AIMX/1` codec; the daemon atomically rewrites `config.toml` (write-temp-then-rename) and hot-swaps the in-memory `Arc<Config>` so newly-created hooks fire on the very next event. Clients fall back to direct `config.toml` edit + restart hint when the daemon is not running (same pattern as FR-18e MAILBOX-*).

### 6.7 Inbound Trust
- FR-33: (v0.2) During `aimx ingest`, verify sender's DKIM signature, SPF record, and DMARC alignment using the `mail-auth` crate.
- FR-34: (v0.2) Store verification results in frontmatter as always-written fields: `dkim` (`"pass"` | `"fail"` | `"none"`), `spf` (`"pass"` | `"fail"` | `"softfail"` | `"neutral"` | `"none"`), `dmarc` (`"pass"` | `"fail"` | `"none"`).
- FR-35: Support per-mailbox `trust` config in `/etc/aimx/config.toml`: `none` (default) or `verified`. `trust` drives computation of the `trusted` frontmatter field via FR-37b; it no longer directly gates hook firing.
- FR-36: Support optional `trusted_senders` allowlist per mailbox (glob patterns). When a sender matches, `trusted` is set to `"true"` without requiring DKIM evaluation.
- FR-37: Mail is always stored regardless of trust result. `on_receive` hooks fire iff the email's `trusted` frontmatter value is `"true"` OR the hook has `dangerously_support_untrusted = true` (per-hook opt-in, `on_receive` only — rejected at config load on other events). Behavioral consequence: a mailbox with `trust: none` (no trust evaluation, so `trusted == "none"`) fires *no* `on_receive` hooks unless the hook explicitly opts into untrusted mail. This is a deliberate inversion of the v1 default (where `trust: none` fired all triggers) — see §11 Resolved Decisions item 12.
- FR-37b: (v0.2) Surface the trust evaluation as an always-written `trusted` frontmatter field on every inbound email, so agents and operators can see the outcome without having to re-derive it. Values:
    - `"none"` — mailbox `trust` is `none` (default). No trust evaluation performed.
    - `"true"` — mailbox `trust` is `verified`, sender matches `trusted_senders`, AND DKIM passed.
    - `"false"` — mailbox `trust` is `verified`, any other outcome (sender not in `trusted_senders`, OR DKIM failed/absent, OR both).
  The frontmatter field is `trusted` (distinct from the mailbox config field `trust`) to reflect that the value is the result of the evaluation, not the policy itself. `trusted == "true"` is equivalent to "this email passed the trigger gate for this mailbox," so the channel manager's gating logic (FR-35/36/37) can be expressed as `if mailbox.trust == "verified" then require trusted == "true"`.

### 6.8 Verifier Service
- FR-38: Hosted verifier service at `check.aimx.email` exposing an HTTP `/probe` endpoint that identifies the caller via a Caddy-injected `X-AIMX-Client-IP` header and performs a full SMTP EHLO handshake against the caller's IP on port 25 (used by `aimx setup` and `aimx portcheck` to confirm `aimx serve` is responding after setup). The endpoint applies a target guard that rejects loopback, unspecified, link-local, and RFC 1918 / RFC 4193 ranges so the service cannot be used as a port-scanner proxy. `/probe` (EHLO handshake) is the single endpoint.
- FR-39: ~~Hosted email endpoint at `verify@aimx.email` that receives test email and sends reply.~~ _Removed: email echo eliminated to avoid backscatter risk and MTA dependency on the verify server. DKIM/SPF verification is handled by DNS record checks during setup instead._
- FR-39b: Port 25 listener on the verifier service that accepts SMTP connections (responds to EHLO), allowing `aimx` clients to test outbound port 25 connectivity via EHLO handshake. _Note: outbound check now performs EHLO handshake directly from the client, not via `/reach`._
- FR-40: Verifier service is open source and self-hostable. No MTA required on the verifier server.

### 6.10 Agent Integrations (`aimx agent-setup <agent>`)
- FR-49: `aimx agent-setup <agent>` installs the AIMX plugin/skill/recipe for the named agent into that agent's standard per-user location (under `$HOME` / `$XDG_CONFIG_HOME`). Runs as the current user. Never mutates the agent's own primary config file. On success, prints the exact activation command (e.g., `claude plugin install ...`, `openclaw mcp set ...`) the user should run next.
- FR-50: Supported agents in v1: Claude Code, Codex CLI, OpenCode (anomalyco), Gemini CLI, Goose, OpenClaw. Each shipped package contains both the MCP wiring and an instructions payload (`SKILL.md`, recipe `prompt`, or equivalent) describing AIMX's MCP tools, storage layout, frontmatter fields, and read/unread semantics so the agent can interact with AIMX without further prompting. (v0.2) Each shipped package's author metadata is `U-Zyn Chua <chua@uzyn.com>`; a repo-wide grep verifies no `"AIMX"` author strings or other placeholders remain across `plugin.json`, `aimx.yaml.header`, and any future Gemini/OpenCode/OpenClaw manifests.
- FR-50a: (v0.2) The shared agent-facing primer is restructured as a **progressive-disclosure skill bundle** under `agents/common/`, mirroring the [anthropics/skills](https://github.com/anthropics/skills) pattern:
    ```
    agents/common/
    ├── aimx-primer.md                # main SKILL body (target 300–500 lines)
    └── references/
        ├── mcp-tools.md              # full MCP tool reference with examples
        ├── frontmatter.md            # full frontmatter schema
        ├── workflows.md              # worked examples for common tasks
        └── troubleshooting.md        # error codes and recovery steps
    ```
  The main `aimx-primer.md` covers identity/purpose, the two access surfaces (MCP for writes/sends, direct filesystem for reads), quick-reference summaries of the 9+ MCP tools, the frontmatter fields agents most often check (`trusted`, `thread_id`, `list_id`, `auto_submitted`, `read`, `labels`), the 4–5 most common workflows inline (check inbox, send, reply, summarize a thread, handle auto-submitted mail), a short trust-model overview, pointers to `references/*.md` for depth, a pointer to `/var/lib/aimx/README.md` as the runtime authoritative layout reference, and a "what you must not do" safety list.
- FR-50b: (v0.2) `aimx serve` maintains an always-present `/var/lib/aimx/README.md` agent-readable guide to the datadir. The README is baked into the binary via `include_str!`, written on `aimx setup`, and refreshed by `aimx serve` on startup if the baked-in version string (`<!-- aimx-readme-version: N -->` at top of file; exact string match, not semver) differs from the on-disk version. The file is deterministic and idempotent (same bytes for a given AIMX version), states at the top that user edits will be overwritten, and covers: directory purpose, read vs write access model, directory layout, file naming rules, slug algorithm, bundle rule, frontmatter reference, trust/DKIM/SPF/DMARC explanation, thread grouping, handling auto-submitted/list mail, attachments, and a pointer to the `aimx` MCP server for all mutations. Refresh on `aimx setup` re-run is also performed; out-of-band refresh (e.g., on every command) is out of scope for v0.2.
- FR-50c: (v0.2) Storage-layout exposure policy — the agent primer **documents the datadir layout explicitly**. (Reverses an earlier draft that proposed redacting layout references for "security.") Rationale: `/var/lib/aimx/` is world-readable, agents already have filesystem access, and concealing the layout makes them less effective without making anything safer. The real security boundary is enforced elsewhere — writes require root + UDS, and DKIM keys live at `/etc/aimx/` root-only.
- FR-51: (v0.2) Plugin sources live under `agents/<agent>/` and `agents/common/` in the AIMX repo and are bundled into the binary at compile time (e.g., via `include_dir!`) so `aimx agent-setup` works offline and is version-locked to the installed binary. Install-time concatenation in `src/agent_setup.rs` extends to support `<platform-prefix>.header → SKILL.md top`, `common primer body → SKILL.md middle`, `<platform-suffix>.footer → SKILL.md bottom` (suffix optional, new in v0.2), and `references/*.md → copied alongside SKILL.md, untouched`. Platforms that support progressive disclosure (Claude Code, Codex) get the full bundle including `references/`. Platforms that take a single blob (Goose recipes, Gemini prompts) receive the main primer only; references are inlined selectively per platform at install time if size permits, or omitted with a pointer to the repo.
- FR-52: `aimx agent-setup --list` prints the registry of supported agents, their destination paths, and their activation commands. `--force` overwrites an existing destination without prompting; `--print` writes plugin contents to stdout instead of disk for dry-run / CI use.

### 6.9 CLI Commands
- FR-41: `aimx setup [domain]` — interactive setup wizard. When domain is omitted, prompt interactively for domain and confirm DNS access.
- FR-41b: ~~Pre-seed OpenSMTPD debconf answers.~~ _Removed: OpenSMTPD replaced by embedded SMTP server. Setup now generates a systemd/OpenRC service file for `aimx serve`._
- FR-42: ~~`aimx preflight` — run port 25 reachability checks (outbound and inbound) without installing.~~ _Removed: preflight functionality merged into `aimx portcheck`._
- FR-42b: `aimx serve` — start the embedded SMTP listener daemon. Options: `--bind` (default `0.0.0.0:25`), `--tls-cert`, `--tls-key`. Runs until SIGTERM/SIGINT.
- FR-43: `aimx ingest <rcpt>` — delivery command for manual/pipe usage (reads raw email from stdin). Also called in-process by `aimx serve`.
- FR-44: `aimx send` — compose, sign, and send email.
- FR-45: `aimx mcp` — start MCP server in stdio mode.
- FR-46: `aimx mailboxes create | list | delete | show <name>` — mailbox management. `aimx mailboxes delete --force <name>` wipes contents of `inbox/<name>/` and `sent/<name>/` before removing the config stanza, with interactive confirmation unless `--yes`. `aimx mailboxes show <name>` prints a per-mailbox deep dive (trust config, full `trusted_senders` list, hooks grouped by event, inbox + sent + unread message counts). `aimx mailbox` is retained as a clap alias.
- FR-47: `aimx doctor` — show server status, config file path (respecting `AIMX_CONFIG_DIR`), per-mailbox trust + hooks summary, mailbox counts, recent activity, and the last 10 journald log lines (always included). No `status` alias — clean rename from the v1 command. `aimx logs [--lines N] [--follow]` is a dedicated subcommand for the full log stream, wrapping `journalctl -u aimx` on systemd with an OpenRC fallback.
- FR-48: `aimx portcheck` — check port 25 connectivity. Requires root. If `aimx serve` is running: outbound EHLO + inbound EHLO probe. If port 25 is free: spawn temp SMTP listener and run checks. If port 25 is occupied by another process: report process name and exit.
- FR-48b: `aimx agent-setup <agent> [--list] [--force] [--print]` — install per-agent plugin/skill/recipe (see §6.10). Runs as the current user.
- FR-48c: `aimx hooks create | list | delete <id>` — hook management (see §6.6 FR-32c/FR-32d). Routes through the daemon's UDS socket for live hot-swap when running; falls back to direct `config.toml` edit + restart hint otherwise. `aimx hook` is retained as a clap alias.
- FR-48d: `aimx completion <shell>` — print a shell-completion script to stdout for the requested shell. Supports bash, zsh, fish, and elvish. Generated from the clap command tree via `clap_complete`.

## 7. Non-Functional Requirements

- **NFR-1: Single binary, no runtime dependencies.** The entire AIMX tool compiles to one binary. No external packages, no system users, no package manager interaction. The binary is fully self-contained.
- **NFR-2: `aimx serve` is the daemon.** `aimx serve` runs as a long-lived SMTP listener process, managed by systemd or OpenRC. All other commands (`ingest`, `send`, `mcp`, `setup`, etc.) remain short-lived.
- **NFR-3: Permissive licensing.** All AIMX code and dependencies must use MIT, Apache-2.0, ISC, or BSD licenses. No GPL/AGPL.
- **NFR-4: Cross-platform Unix.** Any Unix where Rust compiles and port 25 is available. CI tests Ubuntu, Alpine Linux (musl), and Fedora.
- **NFR-5: Minimal resource usage.** `aimx ingest` must complete in < 1 second for typical emails (< 10MB).
- **NFR-6: Secure defaults.** Self-signed TLS cert for STARTTLS (generated during setup, no Let's Encrypt needed), DKIM signing on all outbound, DMARC reject policy, SPF strict. (v0.2) DKIM private key lives at `/etc/aimx/dkim/private.key` (`root:root 600`) and is never readable by non-root processes; `aimx send` reaches the key only indirectly by speaking the UDS protocol to `aimx serve`. Authorization of local clients on `/run/aimx/send.sock` is explicitly out of scope in v0.2 — the socket is world-writable (`0o666`) on the assumption of a single-admin host (see FR-18b).
- **NFR-7: Filesystem-based storage.** No database. Mailboxes are directories. Configuration is TOML. (v0.2) Configuration and secrets live under `/etc/aimx/` (root-owned); mail data lives under `/var/lib/aimx/` (world-readable). AIMX targets single-admin servers — on shared multi-tenant hosts mail is visible to any local user, and this is documented in `book/getting-started.md`. The runtime socket directory `/run/aimx/` is provided by systemd via `RuntimeDirectory=aimx`.

## 8. Technical Considerations

### Language and Stack
- **Rust** — single binary, no runtime, strong ecosystem for email/crypto.
- Key crates: `mail-parser` (MIME parsing), `mail-auth` (DKIM signing/verification), `rmcp` (MCP SDK), `clap` (CLI), `serde`+`toml` (config), `lettre` (outbound SMTP transport), `hickory-resolver` (MX DNS resolution), `tokio-rustls` (TLS for inbound SMTP).

### SMTP Transport
- **Inbound:** Hand-rolled tokio-based SMTP listener embedded in `aimx serve`. Implements receive-only SMTP (EHLO, MAIL FROM, RCPT TO, DATA, QUIT, RSET, NOOP) with STARTTLS. Calls `ingest_email()` in-process on received mail. No external MTA.
- **Outbound:** Direct SMTP delivery via `lettre`. Resolves recipient's MX records via `hickory-resolver`, connects to MX servers in priority order, negotiates STARTTLS, delivers DKIM-signed message. Synchronous delivery — errors returned immediately to caller (no background queue).
- **TLS certificates** — Self-signed cert generated during `aimx setup`. Sufficient for STARTTLS on port 25 (MTAs don't validate certs for opportunistic encryption). No Let's Encrypt or certbot needed.

### Storage Layout (v0.2)
```
/etc/aimx/                              # root-owned, 755
├── config.toml                         # root:root 640 — main config
└── dkim/
    ├── private.key                     # root:root 600 — never readable by non-root
    └── public.key                      # root:root 644 — publishable

/var/lib/aimx/                          # root:root 755 — world-readable datadir
├── README.md                           # agent-facing layout guide, written by setup, refreshed on serve
├── inbox/
│   ├── <mailbox>/
│   │   ├── 2026-04-15-143022-meeting-notes.md
│   │   └── 2026-04-15-153300-invoice-march/        # Zola-style attachment bundle
│   │       ├── 2026-04-15-153300-invoice-march.md
│   │       ├── invoice.pdf
│   │       └── receipt.png
│   └── catchall/                       # unknown local parts
│       └── ...
└── sent/
    └── <mailbox>/                      # one subdir per mailbox; no catchall
        └── 2026-04-15-160145-re-meeting-notes.md

/run/aimx/                              # root:root 0755 — systemd RuntimeDirectory=aimx
└── send.sock                           # root:root 0666 — world-writable UDS; any local user can submit
```

### Architecture
- `aimx serve` is the long-running SMTP daemon. It listens on port 25, accepts inbound email, and calls `ingest_email()` in-process. (v0.2) It also binds `/run/aimx/send.sock` and handles privileged outbound: parses the `From:` header, validates the sender domain, DKIM-signs with the root-only key, delivers to the recipient's MX, and persists the signed copy under `sent/<from-mailbox>/`.
- `aimx ingest` remains available as a CLI command for manual/pipe usage (reads raw email from stdin).
- (v0.2) `aimx send` is a thin UDS client. It composes the unsigned RFC 5322 message and submits it to `aimx serve` over `/run/aimx/send.sock`. The DKIM key never leaves the root-owned daemon process.
- `aimx mcp` is a stdio process launched per MCP session.
- All commands except `aimx serve` are short-lived processes.

### Integration Points
- `aimx serve` SMTP listener (replaces OpenSMTPD MDA pipe)
- (v0.2) `aimx serve` UDS send endpoint at `/run/aimx/send.sock`
- MCP stdio transport (Claude Code, OpenClaw, any MCP client)
- Channel manager triggers (arbitrary shell commands)
- systemd/OpenRC service management for `aimx serve` (v0.2: systemd unit declares `RuntimeDirectory=aimx`)

## 9. Scope and Milestones

### In Scope (v1)
- Setup wizard with DNS guidance, service file generation, DKIM keygen
- Embedded SMTP receiver (`aimx serve`) with STARTTLS and connection hardening
- Direct outbound SMTP delivery with MX resolution (no external MTA)
- Email delivery pipeline (EML→Markdown with attachments)
- Email sending with DKIM signing
- MCP server with full email/mailbox tool set
- Per-agent plugin/skill/recipe packages (Claude Code, Codex CLI, OpenCode, Gemini CLI, Goose, OpenClaw) plus `aimx agent-setup <agent>` installer
- Channel-trigger cookbook documenting email→agent invocation patterns per supported agent
- Channel manager with `cmd` triggers and match filters
- Inbound trust: DKIM/SPF verification, per-mailbox trust policy, trusted_senders allowlist
- Verifier service (probe + reach endpoints)
- CLI for all operations
- Cross-platform Unix (CI: Ubuntu, Alpine, Fedora)
- Build from source (cargo install)
- Prebuilt binary tarballs (Linux x86_64/aarch64, glibc + musl) attached to GitHub Releases on tag push; workflow-artifact builds on every main merge

### Added in v0.2 (pre-launch reshape)
- Privilege separation: DKIM private key root-only at `/etc/aimx/dkim/`, `aimx send` is a UDS client to `aimx serve`
- Filesystem split: config and secrets at `/etc/aimx/`, data at `/var/lib/aimx/` (world-readable)
- Datadir reshape: `inbox/<mailbox>/` and `sent/<mailbox>/`, Zola-style attachment bundles, `YYYY-MM-DD-HHMMSS-<slug>.md` filenames
- Expanded inbound frontmatter (`thread_id`, `received_at`, `received_from_ip`, `size_bytes`, `delivered_to`, `list_id`, `auto_submitted`, `dmarc`, `trusted`, `labels`) and outbound block (`outbound`, `bcc`, `delivered_at`, `delivery_status`, `delivery_details`)
- `trusted` frontmatter field surfacing per-mailbox trust evaluation as an always-written value
- DMARC verification stored alongside DKIM/SPF
- Versioned agent-facing `/var/lib/aimx/README.md` baked into the binary, refreshed on `aimx serve` startup and `aimx setup` re-run
- Common primer restructured as a progressive-disclosure skill bundle (`agents/common/aimx-primer.md` + `references/`)
- Plugin/extension author metadata standardized to `U-Zyn Chua <chua@uzyn.com>`

### Out of Scope (future consideration)
- (v0.2) Per-user mailbox ACLs on send — UDS socket is world-writable; any local user can submit
- (v0.2) Multi-domain or subdomain aliasing in `aimx serve`'s send-side domain validation
- (v0.2) Wildcard entries in `trusted_senders` beyond the existing glob pattern semantics
- (v0.2) Archiving/starring/flagging beyond the existing `read` field and the new `labels` array
- (v0.2) Write-back of triggered channel names onto email frontmatter
- (v0.2) Automatic refresh of `/var/lib/aimx/README.md` outside of `aimx serve` startup and `aimx setup` re-run
- (v0.2) Queueing, retry, or durable delivery state for outbound mail — current behavior (sync SMTP at send time) is preserved
- Package manager distribution (apt/brew/nix)
- `webhook` trigger type
- Multi-tenant / hosted offering
- Web dashboard
- IMAP/POP3/JMAP access
- Email encryption (PGP/S/MIME)
- Rate limiting / spam filtering (rely on DMARC policy for v1)
- Outbound mail queue with retry (v1 uses synchronous delivery with immediate error feedback)
- Auto-merging plugin/MCP entries into the agent's primary config file (AIMX writes plugin packages to standard per-user dirs and prints the activation command; the user runs it)
- Runtime plugin marketplace / plugin hot-reload for agent integrations
- Aider integration (no MCP support in Aider as of v1)

### Milestones

| Milestone | Description | Key Deliverables |
|-----------|-------------|-----------------|
| M1: Core Pipeline | Receive and store email as Markdown | `aimx ingest`, EML→MD parser, attachment extraction, mailbox routing |
| M2: Outbound | Send email with DKIM | `aimx send`, DKIM key generation, DKIM signing, RFC 5322 composition |
| M3: MCP Server | Agent access via MCP | `aimx mcp` with all email/mailbox tools, stdio transport |
| M4: Channel Manager | Trigger actions on incoming mail | `cmd` triggers, match filters, config.toml rules |
| M5: Inbound Trust | Gate triggers on sender verification | DKIM/SPF verification during ingest, per-mailbox trust policy, trusted_senders allowlist |
| M6: Setup Wizard | One-command setup | `aimx setup`, preflight checks, service file generation, DNS guidance, verification |
| M7: Verifier Service | Hosted verification endpoint | `check.aimx.email` probe + reach endpoints, self-hostable |
| M8: Polish | CLI completeness and docs | `aimx doctor`, `aimx portcheck`, README, usage docs |
| M9: Embedded SMTP | Replace OpenSMTPD with built-in SMTP | `aimx serve` daemon, hand-rolled tokio SMTP receiver, lettre outbound, systemd/OpenRC service |

## 10. Risks and Mitigations

| Risk | Likelihood | Impact | Mitigation |
|------|-----------|--------|------------|
| Port 25 blocked on many cloud providers | High | Limits addressable market | Clear documentation of compatible providers. Preflight check catches this early. |
| Outbound mail flagged as spam by Gmail/Outlook | Medium | Agent emails don't reach recipients | Proper DKIM/SPF/DMARC. Gmail filter whitelist instructions. (Reverse DNS / PTR is the operator's responsibility — out of scope for aimx.) |
| Embedded SMTP listener edge cases | Medium | Malformed input or protocol violations crash the listener | Connection hardening (timeouts, size limits, command limits), extensive unit tests, RFC 5321 compliance. Graceful error handling — ingest failures return 451 (retry), never crash the daemon. |
| MIME parsing edge cases | Medium | Some emails render poorly as Markdown | Use battle-tested `mail-parser` crate. Accept that HTML-heavy emails may lose formatting. |
| `mail-auth` crate license compatibility | Low | Can't use for DKIM signing | Verify license before development. Fallback: `mini-mail-auth` or implement RSA-SHA256 signing directly. |
| Verifier service availability | Low | Setup can't run end-to-end test | Make verifier service optional (warn, don't block). Provide self-hosted alternative. |

## 11. Resolved Decisions

1. **TLS** — Self-signed cert generated during setup. Sufficient for STARTTLS on port 25 (MTAs don't validate certs). No Let's Encrypt needed.
2. **Email size limits** — 25 MB default, configurable via config.toml. Enforced by the embedded SMTP listener.
3. **Mailbox deletion** — Deletes the directory and all emails. No archiving.
4. **Config hot-reload** — Not needed for v1. `aimx serve` loads config on startup. Restart the service to apply config changes.
5. **No outbound mail queue** — Synchronous delivery with immediate error feedback. Agents get clear errors on failure and can decide whether to retry. Silent background queuing with delayed bounce notifications is worse for AI agents than immediate failure feedback.
6. **OpenSMTPD removal** — Replaced by embedded SMTP (`aimx serve` for inbound, direct delivery via lettre for outbound). Eliminates apt/debconf/systemd dependency chain. `aimx ingest` remains as a CLI command for manual/pipe usage.
7. **Read/unread tracking** — `read = false` field in TOML frontmatter, set on ingest. `email_mark_read` updates to `read = true`. Self-contained, grepable, no extra files.
8. **IPv4-only outbound by default** — `aimx send` defaults to IPv4-only delivery. IPv6 is opt-in via `enable_ipv6 = true` in `config.toml` (a hidden/advanced flag, not exposed via `aimx setup`). Rationale: most single-VPS deployments only set the `ip4:` SPF mechanism, and letting the OS pick IPv6 when AAAA/SPF aren't set up breaks SPF at major providers like Gmail. Opt-in keeps the default reliable and gives power users a single-line switch.
9. **(v0.2) Privileged send via Unix domain socket** — `aimx send` no longer signs locally. The DKIM private key is root-owned (`/etc/aimx/dkim/private.key`, mode `600`) and never readable by non-root processes. `aimx send` writes a length-prefixed `AIMX/1 SEND` request to `/run/aimx/send.sock` (`root:root 0666`, world-writable); `aimx serve` parses the `From:`, validates the sender domain against the configured primary domain, signs, delivers to MX, and persists the signed copy to `sent/<from-mailbox>/`. **Authorization is explicitly out of scope in v0.2** — any local user can submit mail through the socket. `SO_PEERCRED` is logged for diagnostics but not enforced. Rationale: shrinks the trust boundary so any non-root process that can read the datadir cannot also forge DKIM-signed mail; per-user authorization on top of that is deferred until a real demand surfaces.
10. **(v0.2) Filesystem split — `/etc/aimx/` for config + secrets, `/var/lib/aimx/` for data** — Configuration and DKIM keys move to `/etc/aimx/` (root-owned). Data stays under `/var/lib/aimx/` and is world-readable. Rationale: matches Unix conventions, isolates secrets from agents that need filesystem read access to mail, and lets the datadir stay world-readable without leaking the DKIM key. AIMX targets single-admin servers — on shared multi-tenant hosts mail is visible to any local user, and this is documented in `book/getting-started.md`.
11. **(v0.2) `trusted` frontmatter field, per-mailbox trust model preserved** — The v1 per-mailbox `trust: none|verified` + `trusted_senders` model and trigger-gating semantics are preserved unchanged through v0.2. v0.2 adds an always-written `trusted` field to inbound frontmatter (`"none"` | `"true"` | `"false"`) so agents and operators can read the trust outcome directly per email instead of inferring it from "did a trigger fire." `trusted == "true"` is exactly the condition under which a `trust: verified` mailbox would fire its triggers. Rationale: the agent-facing payoff of trust evaluation should be visible at the email level, not buried in trigger behavior. _Note: the trigger-gating portion of this decision is superseded by item 12 (post-v1) — the `trusted` field itself is unchanged._
12. **(Post-v1) Hooks rename + trust-gate inversion + `after_send`-only send-side hook** — "Channels" is renamed to "hooks" across config/code/docs (Sprint 50). Each hook carries a 12-char auto-generated ID for tracing, log correlation, and CLI addressability. Two events: `on_receive` (existing channel behavior) and `after_send` (new, post-delivery observability — exit code discarded, cannot abort the send). `before_send` was considered and dropped to protect outbound reliability — a flaky `before_send` hook could break mail delivery for the operator. The trust gate is inverted: `on_receive` hooks fire iff `trusted == "true"` on the email OR the hook sets `dangerously_support_untrusted = true`. This means a mailbox with `trust: none` no longer fires any `on_receive` hooks by default — the per-hook opt-in is the escape hatch. Rationale: the v1 mailbox-level trust gate forced all-or-nothing per mailbox; the per-hook opt-in lets the same mailbox run trusted-only alerting plus a permissive log hook without conflict. Hook-fire traceability is provided via structured journald logs (`hook_id=... event=... exit_code=... duration_ms=...`) rather than a frontmatter `hooks` field — the frontmatter approach would have required either a second write per email or a pre-fire-only snapshot, neither of which reflected what actually ran.

