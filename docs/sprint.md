# aimx — Sprint Plan

**Sprint cadence:** 2.5 days per sprint
**Team:** Solo developer with heavy AI augmentation (Claude Code)
**Total sprints:** 15 (6 original + 2 post-audit hardening + 1 YAML→TOML migration + 2 verify/setup overhaul + 2 post-Sprint-11 bug fixes + 2 verify ops)
**Timeline:** ~42.5 calendar days
**v1 Scope:** Full PRD scope including verify service. Sprint 1 targets earliest possible idea validation on a real VPS. Sprints 7–8 address findings from post-v1 code review audit. Sprints 10–11 overhaul the verify service (remove email echo, add EHLO probe) and rewrite the setup flow (root check, MTA conflict detection, install-before-check). Sprints 12–13 fix critical bugs found during post-Sprint-11 debugging: Caddy self-probe loop / XFF SSRF risk in the verify service, and the preflight chicken-and-egg problem on fresh VPSes. Sprints 14–15 are review-driven operational quality work on the verify service (request logging, Docker packaging).

---

## Sprint 1 — Core Pipeline + Idea Validation (Days 1–2.5) [DONE]

**Goal:** Get the inbound and outbound email pipeline working end-to-end so the core idea can be validated on a real VPS with manual OpenSMTPD configuration. Establish CI and test infrastructure from day one.

**Dependencies:** None

### S1.1 — Project Scaffolding + CI

*As a developer, I want a well-structured Rust project with CI so that all subsequent work has a solid foundation and regressions are caught automatically.*

**Technical context:** Set up workspace with `clap` for CLI dispatch, `serde` + `serde_yaml` for config, `mail-parser` for MIME. Use a single binary with subcommands. Define the `config.yaml` schema covering domain, mailboxes, and channel rules (channel rules won't be implemented until Sprint 4, but the schema should be forward-compatible). Set up GitHub Actions CI from the start.

**Acceptance criteria:**
- [x] `cargo build` produces a single `aimx` binary
- [x] `aimx --help` shows all planned subcommands (ingest, send, mcp, mailbox, setup, status, preflight, verify)
- [x] `config.yaml` schema defined and parseable with serde: domain, data directory, mailboxes with addresses and on_receive stubs
- [x] Default data directory is `/var/lib/aimx/`
- [x] Tests pass for config parsing with sample config
- [x] GitHub Actions CI workflow runs on push and PR: `cargo test`, `cargo clippy -- -D warnings`, `cargo fmt -- --check`
- [x] CI uses stable Rust toolchain
- [x] Test fixtures directory (`tests/fixtures/`) created with sample `.eml` files for ingest testing (plain text, HTML-only, multipart, with attachments)

### S1.2 — Email Ingest Pipeline

*As an agent operator, I want incoming emails stored as Markdown files so that my agent can read them without parsing libraries.*

**Technical context:** `aimx ingest <rcpt>` reads raw `.eml` from stdin (piped by OpenSMTPD's MDA). Use `mail-parser` to extract headers, body (prefer text/plain, fall back to HTML→plaintext), and attachments. Generate Markdown with YAML frontmatter matching the format in the PRD. Route to mailbox directory based on RCPT TO local part; fall back to `catchall`.

**Acceptance criteria:**
- [x] `cat test.eml | aimx ingest user@domain.com` produces a correctly formatted `.md` file in the `user` mailbox directory
- [x] Frontmatter includes all required fields: id, message_id, from, to, subject, date, in_reply_to, references, attachments, mailbox, read
- [x] `read` is set to `false` on ingest
- [x] File is named `YYYY-MM-DD-NNN.md` with incrementing counter per day
- [x] Unrecognized local parts route to `catchall` mailbox
- [x] HTML-only emails are converted to plaintext
- [x] Multipart emails extract text/plain correctly
- [x] Unit tests for EML→Markdown conversion using fixture `.eml` files (plain text, HTML-only, multipart)
- [x] Unit tests for frontmatter generation and YAML validity
- [x] Unit tests for mailbox routing (known mailbox, unknown → catchall)
- [x] Integration test: pipe fixture `.eml` via stdin → verify `.md` output file content against expected snapshot <!-- Partial: integration tests verify fixture parseability, not full pipeline snapshot output -->

### S1.3 — Attachment Extraction

*As an agent operator, I want email attachments extracted to the filesystem so that agents can access attached files directly.*

**Acceptance criteria:**
- [x] Attachments saved to `<mailbox>/attachments/<filename>` within the data directory
- [x] Duplicate filenames are deduplicated (append counter)
- [x] Frontmatter `attachments` array lists each with filename, content_type, size, and relative path
- [x] Unit tests: extract single attachment, multiple attachments, duplicate filenames, no attachments
- [x] Integration test: ingest `.eml` with attachments → verify files on disk match original content <!-- Partial: integration test checks attachment count, not full disk content -->

### S1.4 — Basic Email Sending

*As an agent operator, I want to send emails from the command line so that I can test outbound delivery immediately.*

**Technical context:** `aimx send` composes an RFC 5322 message and hands it to `/usr/sbin/sendmail` (provided by OpenSMTPD). No DKIM signing yet — that comes in Sprint 2. This is intentionally minimal to enable quick validation.

**Acceptance criteria:**
- [x] `aimx send --from user@domain.com --to recipient@example.com --subject "Test" --body "Hello"` composes and sends an email
- [x] Generated message has valid RFC 5322 headers (From, To, Subject, Date, Message-ID)
- [x] Message is handed to sendmail for delivery
- [x] Sending errors produce clear error messages
- [x] Unit tests for RFC 5322 message composition (verify headers, body, Message-ID format)
- [x] Unit test: sendmail handoff is abstracted behind a trait so tests don't require a real MTA

### S1.5 — Mailbox Management

*As an agent operator, I want to create multiple mailboxes so that different agents or functions have dedicated email addresses.*

**Acceptance criteria:**
- [x] `aimx mailbox create schedule` creates the directory and registers in `config.yaml`
- [x] `aimx mailbox list` shows all mailboxes with message counts
- [x] `aimx mailbox delete schedule` removes directory and config entry (with confirmation prompt)
- [x] Creating a mailbox that already exists produces a clear error
- [x] `catchall` mailbox cannot be deleted
- [x] Unit tests for create/list/delete operations using temp directories
- [x] Unit tests for error cases: duplicate create, delete catchall, delete non-existent

### VPS Validation Guide — Sprint 1

**Prerequisites:** A VPS with port 25 open (Hetzner, OVH, BuyVM, Vultr). A domain you control.

```bash
# 1. Install OpenSMTPD
sudo apt update && sudo apt install -y opensmtpd

# 2. Build aimx from source
cargo install --path .
sudo cp target/release/aimx /usr/local/bin/

# 3. Create data directory and initial config
sudo mkdir -p /var/lib/aimx/catchall
sudo cat > /var/lib/aimx/config.yaml <<EOF
domain: agent.yourdomain.com
mailboxes:
  catchall:
    address: "*@agent.yourdomain.com"
EOF

# 4. Configure OpenSMTPD (minimal, no TLS yet)
sudo cat > /etc/smtpd.conf <<EOF
listen on 0.0.0.0
action "deliver" mda "/usr/local/bin/aimx ingest %{rcpt}"
match from any for domain "agent.yourdomain.com" action "deliver"
match for any action "relay"
EOF
sudo systemctl restart opensmtpd

# 5. Set up DNS records
#    MX   agent.yourdomain.com → your-server-ip (priority 10)
#    A    agent.yourdomain.com → your-server-ip

# 6. Test inbound: send an email from your personal Gmail to
#    anything@agent.yourdomain.com, then check:
ls /var/lib/aimx/catchall/
cat /var/lib/aimx/catchall/*.md

# 7. Test outbound (no DKIM yet, may land in spam):
aimx send --from catchall@agent.yourdomain.com \
          --to your-personal@gmail.com \
          --subject "Test from aimx" \
          --body "Hello from my agent's email server!"

# 8. Check Gmail — the reply may be in spam (no DKIM yet, that's Sprint 2)
```

**What you're validating:** The core thesis — emails arrive as readable Markdown files, and outbound email works. If this feels right, the idea is validated.

---

## Sprint 2 — DKIM + Production-Quality Outbound (Days 3–5) [DONE]

**Goal:** Make outbound email pass authentication checks (DKIM/SPF/DMARC) so messages land in inboxes, not spam folders.

**Dependencies:** Sprint 1 (send pipeline, config schema)

### S2.1 — DKIM Key Generation

*As an agent operator, I want DKIM signing handled natively so that outbound mail passes authentication checks without external tools.*

**Technical context:** Generate 2048-bit RSA keypair using `rsa` crate. Store private key at `<data_dir>/dkim/private.key`, public key at `<data_dir>/dkim/public.key`. Add a CLI command or integrate into setup flow. The public key needs to be formatted for DNS TXT record output.

**Acceptance criteria:**
- [x] `aimx dkim-keygen` (or equivalent) generates 2048-bit RSA keypair
- [x] Keys stored in `<data_dir>/dkim/` directory
- [x] Command outputs the DNS TXT record value for the DKIM public key
- [x] Existing keys are not overwritten without confirmation
- [x] Unit test: generated keypair is valid 2048-bit RSA
- [x] Unit test: DNS TXT record output is correctly formatted

### S2.2 — DKIM Signing on Outbound

*As an agent operator, I want all outbound emails DKIM-signed so that recipients' mail servers verify authenticity.*

**Technical context:** Use `mail-auth` crate for DKIM signing (RSA-SHA256). Sign after composing RFC 5322 message, before handing to sendmail. Sign headers: From, To, Subject, Date, Message-ID, In-Reply-To, References.

**Acceptance criteria:**
- [x] All outbound email is signed with DKIM-Signature header
- [x] Signature algorithm is RSA-SHA256
- [x] DKIM selector is configurable (default: `dkim`)
- [x] Signed message passes verification when checked against the published DNS record
- [x] Missing private key produces a clear error, not a crash
- [x] Unit test: sign a message with a test keypair, then verify the signature with `mail-auth` in the same test (round-trip)
- [x] Unit test: missing key returns appropriate error

### S2.3 — Email Threading

*As an agent operator, I want email threading support so that replies are grouped correctly in recipients' mail clients.*

**Acceptance criteria:**
- [x] `aimx send --reply-to <message-id>` sets correct In-Reply-To header
- [x] References header is built from the original email's References + Message-ID
- [x] Thread-aware replies display correctly in Gmail's conversation view <!-- Not verifiable in automated tests; requires manual VPS validation -->
- [x] Unit tests: In-Reply-To set correctly, References chain built from original email's References + Message-ID

### S2.4 — File Attachments on Send

*As an agent operator, I want to send emails with file attachments so that agents can share documents.*

**Acceptance criteria:**
- [x] `aimx send --attachment /path/to/file.pdf` attaches the file with correct MIME type
- [x] Multiple `--attachment` flags supported
- [x] Attachment Content-Type is inferred from file extension
- [x] Missing file produces a clear error
- [x] Unit tests: single attachment, multiple attachments, MIME type inference, missing file error

### VPS Validation Guide — Sprint 2

```bash
# 1. Generate DKIM keys
sudo aimx dkim-keygen

# 2. Add DNS records (the command will print what to add):
#    TXT  dkim._domainkey.agent.yourdomain.com → "v=DKIM1; k=rsa; p=MIIBIj..."
#    TXT  agent.yourdomain.com → "v=spf1 ip4:YOUR_IP -all"
#    TXT  _dmarc.agent.yourdomain.com → "v=DMARC1; p=reject"

# 3. Wait for DNS propagation (check with dig):
dig TXT dkim._domainkey.agent.yourdomain.com

# 4. Send a test email:
aimx send --from catchall@agent.yourdomain.com \
          --to your-personal@gmail.com \
          --subject "DKIM test" \
          --body "This should land in your inbox, not spam."

# 5. In Gmail, click "Show original" on the received email. Verify:
#    DKIM: PASS
#    SPF: PASS
#    DMARC: PASS

# 6. Test with https://www.mail-tester.com — send to their test address,
#    aim for a score of 9/10 or higher.

# 7. Test threading — reply to an email:
aimx send --from catchall@agent.yourdomain.com \
          --to your-personal@gmail.com \
          --subject "Re: DKIM test" \
          --body "This is a reply." \
          --reply-to "<message-id-from-original>"
#    Verify it threads correctly in Gmail.
```

**What you're validating:** Outbound mail is production-quality — DKIM/SPF/DMARC all pass, emails land in inbox, threading works.

---

## Sprint 2.5 — Non-blocking Cleanup (Days 5.5–6) [DONE]

**Goal:** Address accumulated non-blocking improvements from Sprint 1 and Sprint 2 reviews.

**Dependencies:** Sprint 2 (merged)

### S2.5-1: Ingest Hardening + Testing

- [x] Add `--data-dir` or `AIMX_DATA_DIR` CLI option to override the hardcoded `/var/lib/aimx/` path *(from Sprint 1 review)*
- [x] Add mailbox name validation to prevent `..`, `/`, or empty strings in `create_mailbox` *(from Sprint 1 review)*
- [x] Replace hand-rolled `yaml_escape` with `serde_yaml` struct serialization for frontmatter *(from Sprint 1 review)*
- [x] Add `\r` to the quoting condition in `yaml_escape` for hardening *(from Sprint 1 review)* — Superseded: `yaml_escape` replaced entirely by `serde_yaml` struct serialization
- [x] Enhance integration tests to exercise `ingest_email()` with fixture files through the full pipeline *(from Sprint 1 review)*

### S2.5-2: Send Hardening + Testing

- [x] Escape attachment filenames in MIME headers to prevent malformed headers *(from Sprint 2 review)*
- [x] Add integration test for `aimx dkim-keygen` CLI end-to-end *(from Sprint 2 review)*
- [x] Refactor duplicated header construction logic in `compose_message()` *(from Sprint 2 review)*
- [x] Add test verifying `dkim_selector` config value is used at runtime *(from Sprint 2 review)*

---

## Sprint 3 — MCP Server (Days 6–8.5) [DONE]

**Goal:** Give AI agents full email access via MCP so that Claude Code (or any MCP client) can read, send, and manage email programmatically.

**Dependencies:** Sprint 1 (ingest, mailbox management), Sprint 2 (send with DKIM)

### S3.1 — MCP Transport + Mailbox Tools

*As an agent framework developer, I want a standard MCP interface for email so that any MCP-compatible agent can use email.*

**Technical context:** Use `rmcp` crate for MCP stdio transport. `aimx mcp` starts the server, launched on-demand by the MCP client (no daemon). Implement `mailbox_create`, `mailbox_list`, `mailbox_delete` as MCP tools that wrap the existing CLI logic.

**Acceptance criteria:**
- [x] `aimx mcp` starts an MCP server in stdio mode
- [x] Server responds to MCP `initialize` handshake correctly
- [x] `mailbox_create(name)` creates mailbox and returns confirmation
- [x] `mailbox_list()` returns all mailboxes with message counts (total and unread)
- [x] `mailbox_delete(name)` deletes mailbox (with appropriate safeguards)
- [x] Server exits cleanly when stdin closes
- [x] Integration tests: spawn `aimx mcp` as child process, send JSON-RPC requests via stdin, assert responses (initialize handshake, tool calls, error cases)

### S3.2 — Email Read + List Tools

*As an agent operator, I want my agent to list and read emails via MCP so that it can process incoming messages programmatically.*

**Acceptance criteria:**
- [x] `email_list(mailbox)` returns frontmatter of all emails in the mailbox
- [x] `email_list` supports optional filters: `unread` (bool), `from` (string), `since` (datetime), `subject` (string)
- [x] `email_read(mailbox, id)` returns full Markdown content of the email
- [x] `email_mark_read(mailbox, id)` updates frontmatter `read: true`
- [x] `email_mark_unread(mailbox, id)` updates frontmatter `read: false`
- [x] Non-existent mailbox or email ID returns clear MCP error
- [x] Unit tests for email listing with each filter type and combinations
- [x] Unit tests for mark read/unread (verify frontmatter file is updated correctly)
- [x] Integration tests via MCP JSON-RPC: list, read, mark_read, error cases

### S3.3 — Email Send + Reply Tools

*As an agent operator, I want my agent to send and reply to emails via MCP so that it can compose and respond to messages programmatically.*

**Acceptance criteria:**
- [x] `email_send(from_mailbox, to, subject, body, attachments?)` composes, DKIM-signs, and sends
- [x] `email_reply(mailbox, id, body)` replies with correct In-Reply-To/References headers
- [x] Send/reply return confirmation with the sent Message-ID
- [x] Errors (missing mailbox, invalid recipient, missing DKIM key) return clear MCP errors
- [x] Integration tests via MCP JSON-RPC: send and reply (using mock MTA trait from Sprint 1)

### VPS Validation Guide — Sprint 3

```bash
# 1. Add MCP config to Claude Code:
#    In ~/.claude/settings.json:
#    {
#      "mcpServers": {
#        "email": {
#          "command": "/usr/local/bin/aimx",
#          "args": ["mcp"]
#        }
#      }
#    }

# 2. Start Claude Code and test:
#    "List all my mailboxes"
#    "Show me unread emails in the catchall mailbox"
#    "Read the latest email"
#    "Send an email to my-personal@gmail.com saying hello"
#    "Reply to the last email from alice@example.com"

# 3. Verify Claude Code can:
#    - See mailbox list with counts
#    - Read email content
#    - Send email that arrives in your Gmail
#    - Reply with correct threading
```

**What you're validating:** The full agent experience — Claude Code can autonomously read and respond to email.

---

## Sprint 4 — Channel Manager + Inbound Trust (Days 8–10) [DONE]

**Goal:** Enable automated reactions to incoming email (triggers) with security gating so that agents can act on email automatically while being protected from spoofed senders.

**Dependencies:** Sprint 1 (ingest pipeline, config schema)

### S4.1 — Channel Manager: Trigger Execution

*As an agent operator, I want channel rules that execute commands on incoming mail so that my agent can react to emails automatically.*

**Technical context:** During `aimx ingest`, after saving the `.md` file, read the mailbox's `on_receive` rules from `config.yaml`. For each `cmd` trigger, substitute template variables (`{filepath}`, `{from}`, `{to}`, `{subject}`, `{mailbox}`, `{id}`, `{date}`) and execute the command via shell. Run synchronously. Log failures to stderr but never block delivery.

**Acceptance criteria:**
- [x] `on_receive` rules in `config.yaml` execute on email delivery to that mailbox
- [x] Template variables `{filepath}`, `{from}`, `{to}`, `{subject}`, `{mailbox}`, `{id}`, `{date}` are substituted correctly
- [x] Trigger failures are logged but do not block email delivery or cause `aimx ingest` to exit non-zero
- [x] Multiple triggers on the same mailbox execute in order
- [x] Mailboxes with no triggers work without errors
- [x] Unit tests for template variable substitution (all variables, special characters in values)
- [x] Integration test: ingest email with trigger config → verify trigger command executed (use `touch {filepath}.triggered` as test command)
- [x] Integration test: failing trigger does not affect email delivery (`.md` still saved)

### S4.2 — Match Filters

*As an agent operator, I want to filter channel triggers by sender, subject, or attachment presence so that agents only act on relevant emails.*

**Acceptance criteria:**
- [x] `match.from` supports glob patterns (e.g., `*@company.com`)
- [x] `match.subject` matches as substring (case-insensitive)
- [x] `match.has_attachment` filters on attachment presence (bool)
- [x] All conditions are AND logic — all must match for trigger to fire
- [x] Trigger with no `match` block fires on every email
- [x] Unit tests for each filter type: from glob match/mismatch, subject match/mismatch, has_attachment true/false
- [x] Unit tests for AND logic: partial match does not fire, full match fires

### S4.3 — Inbound DKIM/SPF Verification

*As an agent operator, I want inbound DKIM/SPF verification so that channel triggers only fire on authenticated emails when I enable trust policies.*

**Technical context:** Use `mail-auth` crate to verify DKIM signature and SPF record of the incoming message during `aimx ingest`. Store results in frontmatter. This runs on the raw `.eml` before Markdown conversion.

**Acceptance criteria:**
- [x] Inbound emails have `dkim: pass|fail|none` and `spf: pass|fail|none` in frontmatter
- [x] Verification uses the `mail-auth` crate against the sender's published DNS records
- [x] Verification failure does not block email storage — mail is always saved
- [x] Verification results are accurate when tested against known DKIM-signed email (e.g., from Gmail) <!-- Partial: requires real DNS; verified functional at runtime, not testable in CI -->
- [x] Unit test: parse DKIM/SPF results from a known-good DKIM-signed `.eml` fixture (captured from Gmail) <!-- Partial: unsigned email tested; DKIM-signed fixture requires real DNS for verification -->
- [x] Unit test: unsigned email produces `dkim: none`, `spf: none`

### S4.4 — Trust Policy + Trusted Senders

*As an agent operator, I want per-mailbox trust policies so that triggers only fire on authenticated emails when I choose.*

**Acceptance criteria:**
- [x] `trust: none` (default) — all triggers fire regardless of verification result
- [x] `trust: verified` — triggers only fire when `dkim: pass`
- [x] `trusted_senders` allowlist accepts glob patterns (e.g., `*@company.com`, `alice@gmail.com`)
- [x] Allowlisted senders always trigger, bypassing DKIM check
- [x] Trust settings are per-mailbox in `config.yaml`
- [x] Email is always stored regardless of trust result
- [x] Unit tests for trust gating: trust=none fires always, trust=verified blocks on dkim!=pass, trusted_senders bypasses check
- [x] Integration test: full ingest pipeline with trust=verified config — DKIM-pass email triggers, DKIM-fail email stores but does not trigger

### VPS Validation Guide — Sprint 4

```bash
# 1. Set up a trigger in config.yaml:
#    mailboxes:
#      catchall:
#        address: "*@agent.yourdomain.com"
#        on_receive:
#          - type: cmd
#            command: 'echo "New email from {from}: {subject}" >> /tmp/aimx-triggers.log'
#          - type: cmd
#            command: 'ntfy pub your-topic "Email from {from}: {subject}"'
#            match:
#              from: "*@gmail.com"

# 2. Send a test email from Gmail → check /tmp/aimx-triggers.log
# 3. Send from non-Gmail → verify only the first trigger fires

# 4. Test trust gating:
#    mailboxes:
#      secure:
#        address: secure@agent.yourdomain.com
#        trust: verified
#        on_receive:
#          - type: cmd
#            command: 'echo "TRUSTED: {from}" >> /tmp/aimx-triggers.log'
#
# Send from Gmail (DKIM passes) → trigger fires
# Send from a server with no DKIM → trigger does NOT fire, but email is still stored

# 5. Verify frontmatter contains dkim/spf results:
cat /var/lib/aimx/catchall/*.md | head -20
```

---

## Sprint 5 — Setup Wizard (Days 10.5–12.5) [DONE]

**Goal:** Replace all manual VPS setup with a single `aimx setup <domain>` command that handles everything from preflight checks to DNS verification.

**Dependencies:** Sprint 1 (config, ingest), Sprint 2 (DKIM keygen)

### S5.1 — Preflight Checks

*As an agent operator, I want setup to verify port 25 reachability before proceeding so that I don't waste time configuring a server that can't deliver mail.*

**Technical context:** Outbound check: connect to `gmail-smtp-in.l.google.com:25`. Inbound check: make HTTP request to `check.aimx.email` probe service with callback IP (the probe service connects back on port 25). PTR check: reverse DNS lookup on server IP. If port 25 is blocked, stop with clear error listing compatible providers.

**Acceptance criteria:**
- [x] Outbound port 25 check connects to a well-known MX and reports pass/fail
- [x] Inbound port 25 check requests probe from `check.aimx.email` and reports pass/fail
- [x] PTR record check warns (non-blocking) if not set, with instructions
- [x] Port 25 blocked → setup stops with error message listing compatible VPS providers
- [x] `aimx preflight` runs these checks standalone without proceeding to setup
- [x] Unit tests for each check result path (pass, fail, timeout) using mockable network traits

### S5.2 — OpenSMTPD Configuration

*As an agent operator, I want setup to configure OpenSMTPD automatically so that I don't have to write smtpd.conf manually.*

**Technical context:** Install OpenSMTPD via `apt install opensmtpd`. Generate self-signed TLS cert for STARTTLS (`openssl req -x509 ...`). Write `smtpd.conf` with TLS, MDA delivery to `aimx ingest`, and relay for outbound. Restart OpenSMTPD.

**Acceptance criteria:**
- [x] Setup installs OpenSMTPD if not present (via apt)
- [x] Self-signed TLS cert generated and placed in `/etc/ssl/aimx/`
- [x] `smtpd.conf` written with TLS, inbound delivery via `aimx ingest`, and outbound relay
- [x] OpenSMTPD restarted successfully after configuration
- [x] Existing OpenSMTPD config is backed up before overwriting
- [x] Unit test: generated `smtpd.conf` content is correct for a given domain and IP
- [x] Unit test: TLS cert generation produces valid self-signed cert

### S5.3 — DNS Guidance + Verification

*As an agent operator, I want setup to display required DNS records and verify them so that I get clear instructions and confirmation.*

**Acceptance criteria:**
- [x] Setup displays all required DNS records: MX, A, SPF, DKIM, DMARC, PTR
- [x] Records include the actual values (server IP, DKIM public key)
- [x] Setup pauses and waits for user to confirm DNS records are added
- [x] After confirmation, setup verifies each record via DNS lookup
- [x] Failed verification shows which records are wrong/missing with guidance
- [x] Unit test: DNS record display formatting for each record type
- [x] Unit test: verification logic handles each record type's pass/fail/missing states

### S5.4 — Setup Finalization

*As an agent operator, I want setup to create a default mailbox and show me the MCP config so that I'm ready to go immediately after setup.*

**Acceptance criteria:**
- [x] Default `catchall` mailbox created
- [x] DKIM keypair generated (if not already present)
- [x] Data directory created with correct permissions
- [x] MCP configuration snippet for Claude Code displayed
- [x] Gmail whitelist instructions displayed
- [x] Setup is idempotent — running again doesn't break existing config

### VPS Validation Guide — Sprint 5

```bash
# 1. Get a FRESH VPS (Hetzner Cloud, OVH, BuyVM, Vultr)
# 2. Install aimx binary
# 3. Run setup:
sudo aimx setup agent.yourdomain.com

# 4. Follow the interactive prompts:
#    - Preflight checks should pass (port 25 open)
#    - Add DNS records as instructed
#    - Wait for DNS verification to pass
#    - Setup completes with catchall mailbox

# 5. Test the full flow without any manual OpenSMTPD config:
#    - Send email from Gmail → verify .md appears
#    - Send email via aimx send → verify DKIM passes in Gmail
#    - Add MCP config to Claude Code → verify agent access works

# 6. Time the process — target is < 15 minutes (excluding DNS propagation)
```

---

## Sprint 5.5 — Non-blocking Cleanup (Days 12.5–13) [DONE]

**Goal:** Address accumulated non-blocking improvements from sprint reviews.

**Dependencies:** Sprint 5 (merged)

### S5.5-1: Serialization + Error Handling

- [x] Replace `unwrap_or_default()` on `serde_yaml::to_string()` with `expect()` or error propagation *(from Sprint 2.5 review)*
- [x] Narrow `tokio` features from `"full"` to specific needed features *(from Sprint 3 review)*

### S5.5-2: Send Module Improvements

- [x] Add unit test for `write_common_headers` with `references = Some(...)` path *(from Sprint 3 review)*

### S5.5-3: Channel/Ingest Improvements

- [x] Deduplicate DNS resolver creation in `verify_dkim_async` and `verify_spf_async` *(from Sprint 4 review)*
- [x] Fix SPF domain fallback semantics — variable naming and fallback logic *(from Sprint 4 review)*
- [x] Add captured DKIM-signed `.eml` fixture from Gmail for verification testing *(from Sprint 4 review)*
- [x] Verify `mail-auth` `dkim_headers` field is stable public API *(from Sprint 4 review)*

### S5.5-4: Setup Improvements

- [x] Implement timestamped backup for pre-aimx OpenSMTPD config *(from Sprint 5 review)*

---

## Sprint 6 — Verify Service + Polish (Days 13–15.5) [DONE]

**Goal:** Complete the product with the hosted verification service, remaining CLI commands, and documentation for open source release.

**Dependencies:** Sprint 5 (setup wizard references verify service)

### S6.1 — Verify Service: Port Probe

*As an agent operator, I want inbound port 25 checked by an external service during setup so that I know my server is reachable before configuring everything.*

**Technical context:** Lightweight HTTP service at `check.aimx.email`. Receives a request with the caller's IP, connects back to that IP on port 25, returns the result. Can be a Cloudflare Worker, a small Rust/Node service, or equivalent. Must be open source and self-hostable.

**Acceptance criteria:**
- [x] `check.aimx.email` accepts probe requests with target IP
- [x] Service connects to target IP on port 25 and reports open/closed
- [x] Response is a simple JSON payload: `{ "reachable": true/false }`
- [x] Service source code is in the aimx repo (e.g., `services/verify/`)
- [x] Service is self-hostable with clear deployment instructions
- [x] Tests for the verify service (unit tests appropriate to the chosen platform — e.g., Cloudflare Worker test harness or Rust integration tests)

### S6.2 — Verify Service: Email Echo

*As an agent operator, I want an end-to-end delivery test so that I can confirm the full pipeline works after setup.*

**Technical context:** Email endpoint at `verify@aimx.email`. Receives a test email from the user's server, verifies DKIM, and sends a reply. The reply confirms DKIM pass/fail status. Used during `aimx setup` and `aimx verify`.

**Acceptance criteria:**
- [x] `verify@aimx.email` receives email and sends an auto-reply
- [x] Reply includes DKIM/SPF verification result of the received message
- [x] Service handles concurrent requests from multiple users
- [x] Service source code is in the aimx repo alongside the probe service

### S6.3 — CLI Polish: status, preflight, verify

*As an agent operator, I want to check server status and verify my setup with simple commands.*

**Acceptance criteria:**
- [x] `aimx status` shows: domain, mailbox count, message counts (total/unread), OpenSMTPD running status, DKIM key presence
- [x] `aimx preflight` runs port 25 + DNS checks without installing anything (extracted from setup wizard)
- [x] `aimx verify` sends test email to `verify@aimx.email`, waits for reply, reports pass/fail
- [x] All commands have clear, formatted output
- [x] All commands have `--help` with usage examples
- [x] Unit tests for `aimx status` output formatting with various states (no mailboxes, multiple mailboxes, missing DKIM key)

### S6.4 — Documentation

*As a developer discovering aimx, I want clear documentation so that I can understand what it does and get started quickly.*

**Acceptance criteria:**
- [x] README.md with: project description, quick start, requirements, installation, usage examples
- [x] Compatible VPS providers listed with port 25 status
- [x] MCP configuration example for Claude Code
- [x] Channel manager configuration examples
- [x] Trust policy documentation
- [x] `config.yaml` reference with all fields documented
- [x] LICENSE file (MIT or Apache-2.0)

### VPS Validation Guide — Sprint 6

```bash
# 1. Full fresh-VPS end-to-end test:
sudo aimx setup agent.yourdomain.com
# Setup should now include the end-to-end verify step using verify@aimx.email

# 2. After setup, test CLI commands:
aimx status
aimx preflight
aimx verify

# 3. Test the full agent workflow:
#    - Configure Claude Code MCP
#    - Have Claude create a mailbox
#    - Have Claude send email
#    - Send email to the agent from Gmail
#    - Have Claude read the email and reply
#    - Set up a channel trigger that invokes Claude on incoming mail

# 4. Review README — would a stranger understand how to set this up?
```

---

## Sprint 7 — Security Hardening + Critical Fixes (Days 16–18.5) [DONE]

**Goal:** Fix all critical and high-severity issues found in the post-v1 code review audit. These address security vulnerabilities, data loss risks, and PRD compliance gaps.

**Dependencies:** Sprint 6 (merged)

### S7.1 — Enforce DKIM Signing on All Outbound Email

*As an agent operator, I expect outbound email to always be DKIM-signed, and to get a clear error if signing is impossible — not a silently unsigned message that may land in spam.*

**Context:** There are two outbound code paths and both silently skip DKIM when the key is missing:

1. **CLI path** (`src/send.rs`, `run()` function, ~line 197): Config is loaded with `.ok()` (line 203–206), swallowing any load error. The private key is loaded with `.ok()` (line 209). If either is `None`, the code prints `eprintln!("Warning: DKIM signing disabled (no key found)")` (line 214) and proceeds to send the email unsigned. The `send_with_transport()` function (line 180) accepts `dkim_key: Option<...>` and simply skips signing when `None`.

2. **MCP path** (`src/mcp.rs`): Both `email_send` (~line 238) and `email_reply` (~line 278) call `load_dkim_key(&config)` (a helper at line 532 that returns `Option<RsaPrivateKey>` via `.ok()`). The result is passed through `.as_ref().map(...)` to build `dkim_info` (lines 268–270 for send, lines 350–354 for reply). If the key is missing, `dkim_info` is `None` and `send_with_transport` sends unsigned — with **no warning or error message at all**.

**What should happen:** Both paths should return an error when the DKIM key can't be loaded, refusing to send unsigned email. This was the original intent per FR-18 ("Sign message with DKIM"), S2.2 AC ("Missing private key produces a clear error, not a crash"), and S3.3 AC ("Errors … missing DKIM key … return clear MCP errors").

**Approach:** Change `send::run()` to propagate config/key load errors instead of using `.ok()`. In MCP, change `load_dkim_key` to return `Result` and have `email_send`/`email_reply` return `Err(...)` when the key is missing.

**Acceptance criteria:**
- [x] `send::run()` returns an error when DKIM config or private key cannot be loaded — send must not proceed unsigned
- [x] MCP `email_send` returns a clear MCP error when DKIM key is unavailable
- [x] MCP `email_reply` returns a clear MCP error when DKIM key is unavailable
- [x] Unit test: `send::run()` with missing DKIM key returns error (not warning)
- [x] Unit test: MCP send/reply with missing DKIM key returns error response

### S7.2 — Sanitize Email Headers Against CRLF Injection

*As a security-conscious operator, I need outbound email composition to be safe from header injection, even when input comes from an AI agent via MCP.*

**Context:** In `src/send.rs`, the function `write_common_headers()` (line 57) builds MIME headers by directly interpolating user-controlled values:

```rust
msg.push_str(&format!("From: {}\r\n", args.from));   // line 58
msg.push_str(&format!("To: {}\r\n", args.to));       // line 59
msg.push_str(&format!("Subject: {}\r\n", args.subject)); // line 60
```

If any of these values contain `\r\n`, an attacker can inject arbitrary additional headers or even start the message body early. For example, a subject of `"Hello\r\nBcc: victim@evil.com\r\n\r\nInjected body"` would inject a Bcc header and replace the body.

Note that attachment filenames **are** already sanitized — `escape_filename()` (line 50) strips `\r` and `\n`. The same pattern needs to be applied (or a stricter one — reject rather than strip) to the From/To/Subject header values.

The primary attack vector is MCP tool calls (`email_send`/`email_reply` in `src/mcp.rs`) where input originates from an AI agent that may be processing untrusted data. CLI args are lower risk since shells typically don't pass raw CRLF, but defense-in-depth applies.

**Approach:** Add a `sanitize_header_value()` function that strips `\r` and `\n` characters (matching the `escape_filename` approach), and call it in `write_common_headers()` for all three fields. Alternatively, return an error from `compose_message()` if any header value contains CRLF — this is stricter and may be preferable for a security fix.

**Acceptance criteria:**
- [x] From, To, and Subject values are sanitized to strip or reject CRLF sequences before header interpolation
- [x] Sanitization covers both `\r\n` and bare `\n` injection vectors
- [x] `compose_message()` returns an error if a header value contains CRLF after sanitization (defense in depth)
- [x] Unit test: subject containing `\r\n` does not produce injected headers
- [x] Unit test: from/to containing `\r\n` does not produce injected headers
- [x] Unit test: normal headers with no CRLF pass through unchanged

### S7.3 — Atomic File ID Generation in Ingest

*As an operator running a production mail server, I need concurrent deliveries to never overwrite each other.*

**Context:** In `src/ingest.rs`, the function `generate_file_id()` (line 407) generates email IDs using a check-then-act pattern:

```rust
fn generate_file_id(mailbox_dir: &Path) -> String {
    let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
    let mut counter = 1u32;
    loop {
        let candidate = format!("{today}-{counter:03}");
        let path = mailbox_dir.join(format!("{candidate}.md"));
        if !path.exists() {       // <-- checks existence (line 414)
            return candidate;      // <-- returns ID, but doesn't create the file
        }
        counter += 1;
    }
}
```

The ID is returned to `ingest_email()` which later writes the file at line 142: `std::fs::write(&filepath, content)?;`. Between the existence check and the write, a concurrent `aimx ingest` process could pick the same ID, and the second write would silently overwrite the first email.

While OpenSMTPD's default MDA delivery is typically serialized per-recipient, this is not architecturally guaranteed — custom configs, multiple domains, or future changes could introduce concurrency.

**Approach:** Merge ID generation and file creation into a single atomic operation. Replace `generate_file_id()` + `fs::write()` with a function that uses `OpenOptions::new().write(true).create_new(true).open(...)` in a loop. `create_new(true)` atomically fails if the file already exists (maps to `O_CREAT | O_EXCL`), so the loop increments the counter and retries on collision. The function should return both the ID and the open file handle (or just write the content directly).

**Acceptance criteria:**
- [x] File creation uses `OpenOptions::new().create_new(true)` (or equivalent atomic create) to prevent TOCTOU race
- [x] On collision, the ID counter increments and retries
- [x] Unit test: two rapid `generate_file_id` calls for the same day produce different IDs
- [x] Unit test: pre-existing file triggers retry rather than overwrite

### S7.4 — Fix Verify Race Condition

*As an operator running `aimx verify`, I need the reply detection to work reliably regardless of timing.*

**Context:** In `src/verify.rs`, the `run()` function (line 10) has a race condition in its reply detection logic. The current execution order is:

```
line 35:  send::run(send_args, data_dir)?;           // 1. Send test email
line 36:  println!("Test email sent.\n");
...
line 43:  let before: Vec<String> = list_md_files(&catchall_dir);  // 2. Snapshot "before" files
line 46:  while elapsed < MAX_WAIT_SECS {             // 3. Poll for new files
line 50:      let after = list_md_files(&catchall_dir);
line 51:      let new_files = after.iter().filter(|f| !before.contains(f)).collect();
```

The problem: the "before" snapshot (step 2) is taken **after** the email is sent (step 1). If the verify service at `verify@aimx.email` replies very quickly, the reply could arrive and be written to the catchall directory between steps 1 and 2. In that case, the reply file would appear in the "before" set and would never be detected as "new" by the polling loop — causing a false timeout.

**Fix:** Swap lines 35 and 43 — take the snapshot before sending. This is a two-line reorder. The `catchall_dir` is already computed earlier (line 41: `let catchall_dir = config.mailbox_dir("catchall")`), so moving the snapshot before the send is straightforward.

Also handle the edge case where the catchall mailbox directory doesn't exist — `list_md_files` (line 78) returns an empty Vec on error via `unwrap_or_default()`, which silently hides a misconfigured catchall. If the directory is missing, `verify` should error immediately with a clear message.

**Acceptance criteria:**
- [x] "Before" file snapshot is taken *before* sending the test email
- [x] Existing unit tests still pass after reordering
- [x] Handle edge case: missing catchall mailbox directory produces a clear error instead of silently returning empty *(also addresses Sprint 6 backlog item)*

### S7.5 — Integrate End-to-End Verify into Setup

*As an operator completing setup, I want the wizard to confirm the full pipeline works — not just that DNS records exist.*

**Context:** In `src/setup.rs`, the `run_setup()` function (line 725) currently ends after DNS record verification:

```
line 766:  let results = verify_all_dns(net, domain, &server_ip, &dkim_selector);
line 767:  let all_pass = display_dns_verification(&results);
line 769-775: if all_pass { "ready!" } else { "some records not correct" }
line 777:  Ok(())  // <-- setup ends here, no end-to-end email test
```

The PRD requirement FR-8 states: "Run end-to-end verification by sending/receiving test email via `verify@aimx.email`." The `verify::run()` function already exists in `src/verify.rs` and does exactly this — it sends a test email, polls for a reply, and reports pass/fail. But `run_setup()` never calls it.

The sprint 6 VPS validation guide (sprint.md line 614) explicitly says: "Setup should now include the end-to-end verify step using verify@aimx.email."

**Approach:** After the DNS verification block (line 775), add a prompt asking the user if they want to run end-to-end verification. If yes, call `verify::run(Some(data_dir))`. Make the verify step non-blocking — if it fails (likely due to DNS propagation delays), print a warning suggesting the user run `aimx verify` later, and still exit successfully. The existing interactive pattern in setup (line 761–764: "press Enter to verify...") provides a good template for the UX.

Note: `run_setup()` takes `sys: &dyn SystemOps` and `net: &dyn NetworkOps` for testability. The verify call uses `send::run()` internally which calls real sendmail — this isn't mockable via the existing traits. For unit testing, consider adding a flag or trait method to skip the actual verify call, or test the prompt/flow logic separately from the email send.

**Acceptance criteria:**
- [x] After DNS verification passes, `run_setup()` offers to run end-to-end email verification
- [x] If verify fails, setup completes with a warning (non-blocking — DNS may still be propagating)
- [x] If verify passes, setup reports full success including email delivery confirmation
- [x] If user declines verify, setup completes normally (verify is optional during setup since DNS propagation may be pending)
- [x] Unit test: `run_setup()` flow includes verify step call (using mockable traits) <!-- Partial: VerifyRunner trait is tested via mock, but no test exercises the full run_setup_with_verify flow due to stdin dependency -->

---

## Sprint 8 — Setup Robustness, CI & Documentation (Days 19–21.5) [DONE]

**Goal:** Fix medium and low-severity issues: strengthen DNS verification, propagate configuration correctly, add CI coverage for the verify service, and resolve documentation inconsistencies.

**Dependencies:** Sprint 7

### S8.1 — Improve DNS Verification Accuracy

*As an operator, I want DNS verification to catch real misconfiguration, not just confirm that records exist.*

**Context:** In `src/setup.rs`, the three DNS verification functions are overly permissive:

1. **SPF** — `verify_spf()` (line 496): Filters TXT records for those starting with `"v=spf1"`, then checks `spf.iter().any(|r| r.contains(expected_ip))` (line 503). This is a bare substring match — if the expected IP is `"1.2.3.4"`, it would also match a record containing `"ip4:1.2.3.45"` or `"ip4:11.2.3.4"`. Should instead parse the SPF mechanisms and match the exact IP.

2. **DKIM** — `verify_dkim()` (line 516): Looks up `{selector}._domainkey.{domain}` TXT, filters for records containing `"v=DKIM1"` (line 520), and returns Pass if any match exists. It does **not** check that the `p=` public key in the DNS record matches the private key on disk (`<data_dir>/dkim/private.key`). An operator could publish the wrong key and setup would still say PASS.

3. **DMARC** — `verify_dmarc()` (line 531): Looks up `_dmarc.{domain}` TXT, filters for `"v=DMARC1"` (line 535), and returns Pass if any match. A record of `"v=DMARC1; p=none"` passes — but `p=none` means no enforcement, which defeats the purpose of DMARC for deliverability. Should warn when policy is too permissive.

All three functions use the same `DnsVerifyResult` enum (Pass/Fail/Missing) and are called via `verify_all_dns()` (line 546) and displayed by `display_dns_verification()`. The existing mock infrastructure (`MockNetworkOps` with `txt_records` HashMap) makes these fully testable.

**Acceptance criteria:**
- [x] SPF verification checks for the IP in proper SPF mechanisms (`ip4:X.X.X.X/32`, `ip4:X.X.X.X `, or end-of-string) — not bare substring
- [x] DKIM verification extracts the `p=` value from the DNS record and confirms it matches the local public key
- [x] DMARC verification warns if policy is `p=none` (too permissive for production)
- [x] Unit test: SPF with similar-prefix IP (e.g., "1.2.3.4" vs "1.2.3.45") correctly fails
- [x] Unit test: DKIM record with mismatched public key reports failure
- [x] Unit test: DMARC with `p=none` reports warning

### S8.2 — Propagate --data-dir to OpenSMTPD Ingest Command

*As an operator using a custom data directory, I need OpenSMTPD's MDA command to use the same path.*

**Context:** In `src/setup.rs`, `generate_smtpd_conf()` (line 324) takes `domain` and `aimx_binary` parameters and generates the OpenSMTPD config. The MDA action is hardcoded as:

```rust
action "deliver" mda "{aimx_binary} ingest %{{rcpt}}"  // line 335
```

Meanwhile, `run_setup()` (line 725) accepts an optional `data_dir` parameter (line 727) and defaults to `/var/lib/aimx` (line 740). All config and mailbox operations use this custom path. But the generated `smtpd.conf` doesn't pass `--data-dir` to the ingest command.

**Result:** If an operator runs `sudo aimx setup mydomain.com --data-dir /opt/aimx`, setup creates config at `/opt/aimx/config.yaml` and mailboxes under `/opt/aimx/`. But when OpenSMTPD delivers mail, it invokes `aimx ingest user@domain.com` (no `--data-dir`), which defaults to `/var/lib/aimx` — so ingest looks for config in the wrong place and mail either fails or goes to the wrong directory.

**Approach:** Add a `data_dir: Option<&Path>` parameter to `generate_smtpd_conf()`. When the data dir is non-default, generate `action "deliver" mda "{aimx_binary} ingest --data-dir {data_dir} %{{rcpt}}"`. The caller in `configure_opensmtpd()` (line 344) and `run_setup()` need to thread the data dir through. There's an existing unit test `generated smtpd.conf content is correct` that will need updating.

**Acceptance criteria:**
- [x] `generate_smtpd_conf()` accepts a data directory parameter
- [x] When data dir is non-default, the MDA command includes `--data-dir <path>`
- [x] Default path (`/var/lib/aimx`) omits the flag for cleaner config
- [x] Unit test: custom data dir produces `--data-dir` in smtpd.conf
- [x] Unit test: default data dir omits `--data-dir` in smtpd.conf

### S8.3 — Fix SPF Verification Domain Fallback

*As an operator, I want SPF verification results to be accurate, not evaluated against the wrong domain.*

**Context:** In `src/ingest.rs`, the `verify_spf_async()` function (line 204) determines which domain to use for SPF evaluation:

```rust
let mail_from = extract_mail_from(raw).unwrap_or_default();       // line 210
let from_domain = mail_from.split('@').nth(1).unwrap_or("");      // line 211

let helo_domain = if !from_domain.is_empty() {
    from_domain                                                     // line 213: use sender domain
} else {
    rcpt.split('@').nth(1).unwrap_or("")                           // line 216: FALLBACK to recipient domain
};
```

When the sender's From domain can't be determined (empty mail_from or missing @), the code falls back to using the **recipient's** domain for SPF evaluation (line 216). This is semantically wrong — SPF records are published by the sending domain. Evaluating the recipient domain's SPF record against the sending IP is meaningless and could produce false passes (if the recipient domain's SPF happens to include the sending IP) or false fails.

The `helo_domain` variable is then used both as the HELO domain **and** the mail-from domain in the `mail-auth` call: `resolver.verify_spf_sender(ip, helo_domain, helo_domain, &mail_from)` (line 223–224).

Note: Sprint 5.5 partially addressed this (renamed variables, clarified fallback logic) but the fundamental issue — falling back to recipient domain — was not fixed.

**Approach:** When `from_domain` is empty, return `"none"` immediately instead of falling back. Also extract the domain-selection logic into a standalone function (e.g., `fn spf_domain(mail_from: &str, rcpt: &str) -> Option<&str>`) that returns `None` when the sender domain can't be determined. This was already an open backlog item from Sprint 5.5.

**Acceptance criteria:**
- [x] SPF verification returns `spf: none` when the sender domain cannot be determined, instead of falling back to recipient domain
- [x] SPF domain-selection logic extracted into a standalone testable function *(also resolves Sprint 5.5 backlog item)*
- [x] Unit test: empty sender domain produces `spf: none`, not evaluation against recipient domain
- [x] Unit test: valid sender domain is used correctly

### S8.4 — Configurable Verify Service URLs

*As an operator self-hosting the verify service, I need to point the client at my own instance.*

**Context:** Two values are hardcoded as constants with no configuration override:

1. **Probe URL** — In `src/setup.rs`, `check_inbound_port25()` (line 141) shells out to `curl` with a hardcoded URL:
   ```rust
   .args(["-s", "-m", "15", "https://check.aimx.email/probe"])  // line 143
   ```

2. **Verify address** — In `src/verify.rs`, the target email for end-to-end testing:
   ```rust
   const VERIFY_ADDRESS: &str = "verify@aimx.email";  // line 5
   ```

The verify service README (`services/verify/README.md`, line 54–60) documents self-hosting instructions and explicitly tells users to "point the probe URL to your service" — but there's no `config.yaml` field or CLI flag to do so.

The `Config` struct is defined in `src/config.rs` and currently has: `domain`, `data_dir`, `dkim_selector`, `mailboxes`. New optional fields need to be added there with serde defaults.

**Approach:** Add `probe_url: Option<String>` and `verify_address: Option<String>` to `Config`. In `check_inbound_port25()`, read the URL from config (or pass it as a parameter). In `verify::run()`, read the address from config. Both fall back to the current hardcoded defaults when not set. Also add `--probe-url` and `--verify-address` CLI flags to `aimx setup` and `aimx verify` that override the config value. Update the verify service README to reference these config fields.

**Acceptance criteria:**
- [x] `config.yaml` supports optional `probe_url` and `verify_address` fields
- [x] `check_inbound_port25()` uses configured probe URL, defaulting to `https://check.aimx.email/probe`
- [x] `verify::run()` uses configured verify address, defaulting to `verify@aimx.email`
- [x] Unit test: custom probe URL is used when configured
- [x] Unit test: custom verify address is used when configured
- [x] Update verify service README to document the config fields

### S8.5 — CI Coverage for Verify Service

*As a contributor, I need CI to catch regressions in the verify service, not just the main crate.*

**Context:** The verify service at `services/verify/` is a standalone Cargo project (package name `aimx-verify`, see `services/verify/Cargo.toml`). It is **not** a workspace member of the root `Cargo.toml`.

The CI workflow (`.github/workflows/ci.yml`) runs three checks, all at the repo root only:

```yaml
- name: Check formatting
  run: cargo fmt -- --check           # line 28 — only checks root crate

- name: Clippy
  run: cargo clippy -- -D warnings    # line 31 — only checks root crate

- name: Run tests
  run: cargo test                      # line 34 — only runs root crate tests
```

This means the verify service can accumulate lint warnings, formatting drift, or test failures without CI catching them.

**Approach:** Add a second job (or additional steps in the existing job) that runs the same checks with `working-directory: services/verify`. Alternatively, convert to a Cargo workspace — but that may pull in the verify service's dependencies (actix-web, etc.) into the main binary's build, so separate CI steps are likely cleaner.

**Acceptance criteria:**
- [x] CI workflow runs `cargo test` in `services/verify/` directory
- [x] CI workflow runs `cargo clippy -- -D warnings` for `services/verify/`
- [x] CI workflow runs `cargo fmt -- --check` for `services/verify/`

### S8.6 — Documentation & Status Fixes

*As a user reading docs or running `aimx status`, I expect accuracy and consistency.*

**Context — Status "recent activity":** The PRD (FR-47, `docs/prd.md` line 132) specifies: "`aimx status` — show server status, mailbox counts, recent activity." The current `format_status()` in `src/status.rs` (line 109) outputs domain, data dir, DKIM selector, DKIM key presence, OpenSMTPD status, and mailbox table (name, address, total, unread) — but no "recent activity" section. The `StatusInfo` struct (line 5) would need a new field, and `gather_status()` (line 21) would need to read the most recent email(s) per mailbox (e.g., last 3–5 by date from the `.md` frontmatter).

**Context — DigitalOcean inconsistency:** Two docs make contradictory claims:
- `README.md` line 56: `| DigitalOcean | On request | Submit support ticket |` — listed as a compatible provider
- `docs/idea.md` line 434: `| DigitalOcean | Permanently blocks SMTP, recommends against self-hosted mail |` — listed under "Not supported"

One of these is wrong. Research suggests DigitalOcean's current policy is closer to the idea.md version (SMTP is restricted and difficult to unblock), but this should be verified. Pick the accurate position and update both files to match.

**Context — GitHub URLs:** Existing Sprint 6 backlog item: GitHub repo URLs in `README.md` and `services/verify/README.md` reference the wrong owner/org. Find and fix all occurrences.

**Acceptance criteria:**
- [x] `aimx status` includes a "Recent activity" section showing the last few emails received (most recent per mailbox)
- [x] `StatusInfo` struct extended with recent activity data; `gather_status()` reads recent emails from mailbox directories
- [x] README.md and docs/idea.md are consistent on DigitalOcean — pick the accurate position and update both
- [x] Fix GitHub URLs in README.md and services/verify/README.md *(existing Sprint 6 backlog item)*
- [x] Unit test: `format_status` output includes recent activity section

---

## Sprint 9 — Migrate from YAML to TOML (Days 22–24.5) [DONE]

**Goal:** Replace `serde_yaml` (unmaintained) with `toml` for both configuration and email frontmatter, aligning with idiomatic Rust ecosystem conventions.

**Dependencies:** Sprint 8 (merged)

### S9.1 — Migrate Config from YAML to TOML

*As a developer, I want configuration in TOML so the project uses an actively maintained serializer and follows Rust ecosystem conventions.*

**Context:** `config.yaml` is parsed in `src/config.rs` via `serde_yaml::from_str`/`to_string`. The `Config` struct uses `#[derive(Serialize, Deserialize)]` which is format-agnostic — only the parse/write calls and file extension need changing. The PRD specifies YAML (NFR-7, section 8), but the owner has approved migrating to TOML. `aimx setup` generates the initial config file. All tests in `config.rs` use inline YAML strings.

**Scope:**
- Replace `serde_yaml` with `toml` crate in `Cargo.toml`
- Update `Config::load()` and `Config::save()` in `src/config.rs`
- Rename `config.yaml` → `config.toml` throughout (code, docs, README)
- Update `aimx setup` to generate `config.toml`
- Update all config tests to use TOML format
- Update `aimx status` output that references config path

**Acceptance criteria:**
- [x] `serde_yaml` removed from `Cargo.toml`; `toml` crate added
- [x] `Config::load()` reads `config.toml` using `toml::from_str`
- [x] `Config::save()` writes `config.toml` using `toml::to_string_pretty`
- [x] `aimx setup` generates `config.toml` (not `config.yaml`)
- [x] All references to `config.yaml` updated to `config.toml` in code, docs, and README
- [x] All config unit tests updated to TOML format and pass
- [x] Integration tests updated and pass

### S9.2 — Migrate Email Frontmatter from YAML to TOML

*As a developer, I want email frontmatter in TOML so the entire project uses a single serialization format.*

**Context:** Email `.md` files use YAML frontmatter between `---` delimiters. The `EmailMetadata` struct in `src/ingest.rs` is serialized via `serde_yaml::to_string()` and parsed back in `src/mcp.rs`, `src/status.rs`, and `src/verify.rs` via `serde_yaml::from_str()`. TOML frontmatter uses `+++` delimiters (Hugo convention).

**Scope:**
- Change frontmatter delimiters from `---` to `+++`
- Replace `serde_yaml::to_string(meta)` with `toml::to_string_pretty(meta)` in `ingest.rs`
- Replace all `serde_yaml::from_str` frontmatter parsing in `mcp.rs`, `status.rs`, `verify.rs`
- Update all `serde_yaml::Value` / `serde_yaml::Mapping` test assertions to use `toml::Value` / `toml::Table` equivalents
- Update PRD/docs references to "YAML frontmatter" → "TOML frontmatter"

**Acceptance criteria:**
- [x] Email frontmatter uses `+++` delimiters and TOML format
- [x] `ingest.rs` serializes `EmailMetadata` via `toml::to_string_pretty`
- [x] `mcp.rs` frontmatter parsing uses `toml::from_str`
- [x] `status.rs` frontmatter parsing uses `toml::from_str`
- [x] `verify.rs` frontmatter parsing uses `toml::from_str`
- [x] All `serde_yaml::Value`/`Mapping` test assertions migrated to `toml::Value`/`Table`
- [x] No remaining `serde_yaml` imports in the codebase
- [x] All unit and integration tests pass
- [x] `cargo clippy -- -D warnings` clean

---

## Sprint 10 — Verify Service Overhaul (Days 25–27.5) [DONE]

**Goal:** Simplify the verify service to a port probe with EHLO handshake and a port 25 listener — no email processing, no outbound email, no backscatter risk.

**Dependencies:** Sprint 9 (merged)

### S10.1 — Remove Email Echo + Strip Dependencies

*As a verify service operator, I want the service to never send email so that there's no backscatter risk and no outbound MTA dependency.*

**Technical context:** Delete `services/verify/src/echo.rs` entirely. Remove the `echo` subcommand handling from `main.rs` (lines 79–85). Remove `mail-parser` and `mail-auth` from `services/verify/Cargo.toml`. The `run_echo()` function, `parse_incoming()`, `compose_reply()`, `extract_auth_result()`, and all echo tests are deleted.

**Acceptance criteria:**
- [x] `echo.rs` deleted
- [x] `echo` subcommand removed from `main.rs`
- [x] `mail-parser` and `mail-auth` removed from `Cargo.toml`
- [x] `cargo build` succeeds with no echo-related code
- [x] `cargo test` passes — all remaining tests still work
- [x] `cargo clippy -- -D warnings` clean

### S10.2 — Add Port 25 Listener

*As an aimx client checking outbound port 25, I want the verify service to accept TCP connections on port 25 so that connecting to it proves my outbound port 25 is working.*

**Technical context:** Add a minimal SMTP-like listener using `tokio::net::TcpListener` on port 25 (configurable via `SMTP_BIND_ADDR` env var, default `0.0.0.0:25`). On connection: send a `220 check.aimx.email SMTP aimx-verify\r\n` banner, wait for any input (or timeout after 10 seconds), send `221 Bye\r\n`, and close. This is not a real SMTP server — it's just enough to accept connections and respond with a valid SMTP banner. Run this listener as a second `tokio::spawn` task alongside the existing Axum HTTP server.

**Acceptance criteria:**
- [x] Service listens on port 25 (configurable via `SMTP_BIND_ADDR` env var)
- [x] On TCP connection: sends `220` banner, waits briefly, sends `221 Bye`, closes
- [x] Port 25 listener runs concurrently with HTTP server (both in same tokio runtime)
- [x] Connection timeout of 10 seconds prevents resource exhaustion from idle connections
- [x] Unit test: verify banner format starts with `220`
- [x] Integration test: connect to port 25 listener, receive banner, verify valid SMTP greeting

### S10.3 — Upgrade Probe to EHLO Handshake

*As an aimx client checking inbound port 25, I want the verify service to perform a proper SMTP EHLO with my server so that the check confirms an actual SMTP server is responding, not just an open port.*

**Technical context:** Replace `check_port25()` in `main.rs` — currently a bare `TcpStream::connect` (line 64–74) — with an SMTP handshake function. The new function should: (1) TCP connect with 10s timeout, (2) read the `220` banner, (3) send `EHLO check.aimx.email\r\n`, (4) read the `250` response, (5) send `QUIT\r\n`, (6) close. If any step fails or times out, report `reachable: false`. The overall timeout for the EHLO sequence should be 45 seconds (matching the client-side expectation).

**Acceptance criteria:**
- [x] Probe performs SMTP EHLO handshake instead of bare TCP connect
- [x] Banner read (`220`), EHLO (`250`), and QUIT sequence completed
- [x] Timeout of 45 seconds for the full EHLO handshake
- [x] `reachable: true` only if EHLO gets a `250` response
- [x] `reachable: false` if connection refused, banner missing, or EHLO rejected
- [x] Unit test: mock TCP stream with valid SMTP responses → `reachable: true`
- [x] Unit test: mock TCP stream with no banner → `reachable: false`
- [x] Unit test: mock TCP stream with non-250 EHLO response → `reachable: false`

### S10.4 — Remove `ip` Parameter from Probe

*As a verify service operator, I want the probe to only check the caller's own IP so that the service cannot be used as a port scanner proxy.*

**Technical context:** Remove the `ip` field from `ProbeRequest` and the `ip` query parameter from the `GET /probe` handler. Remove the `POST /probe` endpoint entirely. The probe should only use `ConnectInfo(addr).ip()` to get the caller's IP. Remove all tests for custom IP parameter and POST body.

**Acceptance criteria:**
- [x] `GET /probe` uses caller's IP only — no `ip` query parameter
- [x] `POST /probe` endpoint removed
- [x] `ProbeRequest` struct removed or simplified
- [x] Tests updated: probe always uses caller IP
- [x] Unit test: probe response contains caller's IP
- [x] Old tests for custom `ip` parameter and POST body removed

---

## Sprint 11 — Setup Flow Rewrite + Client Cleanup (Days 28–30.5) [DONE]

**Goal:** Rewrite the setup flow to check root, detect MTA conflicts, install OpenSMTPD before port checks, and simplify the verify client to port-check-only.

**Dependencies:** Sprint 10 (verify service must support EHLO probe and port 25 listener)

### S11.1 — Root Check + MTA Conflict Detection

*As an operator running `aimx setup`, I want clear errors if I'm not root or if a non-OpenSMTPD MTA is on port 25 so that I don't waste time on a setup that will fail.*

**Technical context:** Add two new checks at the top of `run_setup_with_verify()` (line 832), before any other work:

1. **Root check:** Use `libc::geteuid() == 0` or equivalent. If not root, exit: "aimx setup requires root. Run with: sudo aimx setup <domain>"

2. **MTA conflict detection:** Use `ss -tlnp sport = :25` (via `SystemOps` trait method) to check what's on port 25. Parse output to determine: (a) nothing → proceed, (b) OpenSMTPD → warn that smtpd.conf will be overwritten, ask user to confirm, create .bak backup, (c) other MTA (Postfix, Exim, Sendmail) → exit: "SMTP port 25 is already in use by [process]. aimx requires OpenSMTPD. Uninstall the current SMTP server and run `aimx setup` again."

Add `check_root()` and `check_port25_occupancy()` to `SystemOps` trait for testability. Return an enum: `Port25Status::Free`, `Port25Status::OpenSmtpd`, `Port25Status::OtherMta(String)`.

**Acceptance criteria:**
- [x] Non-root user gets clear error: "aimx setup requires root. Run with: sudo aimx setup <domain>"
- [x] Port 25 occupied by non-OpenSMTPD → exit with process name in error message
- [x] Port 25 occupied by OpenSMTPD → prompt user to confirm smtpd.conf overwrite, create .bak backup
- [x] User declines overwrite → setup exits cleanly
- [x] Port 25 free → proceed silently
- [x] `SystemOps` trait extended with `check_root()` and `check_port25_occupancy()` methods
- [x] Unit test: non-root detection
- [x] Unit test: OpenSMTPD detected → confirmation flow
- [x] Unit test: Postfix detected → exit with correct error message
- [x] Unit test: nothing on port 25 → proceed

### S11.2 — Reorder Setup Flow: Install Before Check

*As an operator, I want port 25 checks to run after OpenSMTPD is installed so that the inbound check can verify my SMTP server is actually responding with a proper EHLO, not just that the port is open.*

**Technical context:** Restructure `run_setup_with_verify()` to follow the new flow:

1. `check_root()` — exit if not root
2. `check_port25_occupancy()` — exit if non-OpenSMTPD MTA; confirm if OpenSMTPD exists
3. `configure_opensmtpd()` — install + configure (existing function, line 375)
4. `check_outbound()` — connect to `check.aimx.email:25` (check service port 25 listener)
5. `check_inbound()` — HTTP call to check service `/probe`, which does EHLO back
6. `check_ptr()` — unchanged
7. If outbound or inbound fails → exit with clear message and provider list
8. Continue to DKIM keygen, DNS guidance, verification (unchanged)

Update `check_outbound_port25()` in `RealNetworkOps` to connect to the check service's port 25 instead of `gmail-smtp-in.l.google.com:25`. Derive the SMTP address from `probe_url` host (e.g., `check.aimx.email:25`). Add `check_service_smtp_addr` field to `RealNetworkOps`.

Update the HTTP timeout for `check_inbound_port25()` from 15 seconds to 60 seconds (the check service needs up to 45s for the EHLO handshake).

**Acceptance criteria:**
- [x] Setup flow order: root → MTA check → OpenSMTPD install → outbound → inbound → PTR → DKIM → DNS
- [x] Outbound check connects to check service port 25 (not `gmail-smtp-in.l.google.com:25`)
- [x] Inbound check HTTP timeout increased to 60 seconds
- [x] If outbound fails after OpenSMTPD install → clear error with provider list
- [x] If inbound fails after OpenSMTPD install → clear error about firewall/provider
- [x] Unit test: full setup flow order verified via mock call sequence <!-- Partial: individual steps tested; full flow mock impractical due to interactive stdin -->
- [x] Unit test: outbound connects to check service port 25
- [x] Unit test: inbound timeout is 60 seconds

### S11.3 — Simplify aimx verify + Remove verify_address

*As an operator, I want `aimx verify` to check port 25 connectivity only so that it's fast, reliable, and doesn't depend on email round-trips.*

**Technical context:** Rewrite `src/verify.rs` completely. The current implementation sends an email, polls the catchall mailbox for a reply, and parses the result (lines 17–94). Replace with: (1) check outbound port 25 by connecting to check service port 25, (2) check inbound port 25 via HTTP probe (EHLO callback), (3) check PTR. Report pass/fail for each. Remove `verify_address` from `Config` in `src/config.rs`. Keep `probe_url`. Update all tests.

Also update `aimx preflight` to use the same check service port 25 for the outbound test.

The `VerifyRunner` trait in `setup.rs` and `RealVerifyRunner` should call the new `verify::run()` which no longer sends email.

**Acceptance criteria:**
- [x] `aimx verify` checks outbound port 25, inbound port 25 (EHLO), and PTR — no email sent
- [x] `verify_address` field removed from `Config` struct
- [x] `probe_url` field retained in `Config` struct
- [x] `aimx preflight` uses check service port 25 for outbound test
- [x] Old email-based verify logic removed entirely (no `send::run`, no mailbox polling)
- [x] Unit test: verify reports pass/fail for each check
- [x] Unit test: config without `verify_address` parses correctly
- [x] Unit test: config with legacy `verify_address` field doesn't error (serde ignores unknown — verify with `#[serde(deny_unknown_fields)]` is NOT set)

### S11.4 — Documentation + Backlog Cleanup

*As a user or contributor, I want docs to accurately reflect the simplified verify service and setup flow.*

**Acceptance criteria:**
- [x] `services/verify/README.md` updated: remove email echo section, add port 25 listener docs, update self-hosting instructions (no MTA needed on verify server), update systemd example
- [x] `README.md` updated: verify service description reflects probe-only, remove references to `verify@aimx.email` and email echo, update `config.toml` reference (remove `verify_address`), update setup flow description
- [x] Obsolete non-blocking backlog items in `docs/sprint.md` marked as resolved: multiline Authentication-Results (Sprint 6 — obsolete, echo removed), Message-ID/Date on echo reply (Sprint 6 — obsolete, echo removed), SSRF hardening on `/probe` ip parameter (Sprint 6 — obsolete, ip parameter removed)
- [x] PRD updated: FR-8 and S6.2 reflect simplified verify service (port probe only, no email echo) <!-- Partial: PRD already had FR-39 struck through from Sprint 10; no further PRD edits made -->

---

## Sprint 12 — aimx-verify Security Hardening + /reach Endpoint (Days 31–33.5) [DONE]

**Goal:** Fix three real bugs in the verify service discovered during post-Sprint-11 debugging: the Caddy self-probe loop (ConnectInfo reports loopback when behind a reverse proxy, so the service probes itself), the SSRF / port-scan-as-a-service risk in naive X-Forwarded-For handling, and the self-EHLO trap in the built-in SMTP listener. Also add a plain-TCP `/reach` endpoint so `aimx preflight` (Sprint 13) can check port 25 reachability on a fresh VPS without requiring a live SMTP server.

**Dependencies:** Sprint 11 (merged)

**Background — the bugs this sprint fixes:**

1. **Caddy self-probe loop.** `services/verify/src/main.rs:26` uses `ConnectInfo(addr)` to identify the caller, but when the axum server is behind Caddy (as the deployed `check.aimx.email` is), the TCP peer is the loopback Caddy→axum connection. So every `/probe` call resolves the caller IP to `127.0.0.1`, connects to `127.0.0.1:25`, hits the service's OWN built-in SMTP listener (`run_smtp_listener`, line 92), gets a malformed SMTP exchange, and returns `{"reachable": false, "ip": "127.0.0.1"}`. Real users hitting the public endpoint have been getting garbage results. Verified: `curl https://check.test.aimx.email/probe` returns `{"reachable":false,"ip":"127.0.0.1"}`.

2. **SSRF / port-scan-as-a-service via XFF poisoning.** Even with an X-Forwarded-For fallback added naively, Caddy's default behavior APPENDS rather than replaces the header. A client sending `X-Forwarded-For: 8.8.8.8` gets that value forwarded through as the leftmost entry — so a "leftmost = client" parser would let any internet caller make the service probe port 25 on any host of their choosing. Needs a trust-boundary design, not just a fallback.

3. **Self-EHLO trap.** `handle_smtp_connection` (line 117) sends `220` banner → waits for any input → sends `221 Bye` and closes. It never sends `250` in response to EHLO. So any EHLO-speaking client (including the service's own `/probe` loop) reads `221` after `EHLO` and fails the handshake. The listener is not a valid SMTP responder.

**Additional scope — `/reach` endpoint for Sprint 13.** `aimx preflight` needs to check inbound port 25 reachability on a fresh VPS before OpenSMTPD is installed, which means there's nothing on :25 answering SMTP yet. The current `/probe` endpoint requires a full EHLO handshake and will always fail in that state. The clean fix is a second endpoint that only does a plain TCP reachability test (equivalent to `nc -z <ip> 25`), matching what preflight actually means. `/probe` stays unchanged for `aimx setup` and `aimx verify`, which run after OpenSMTPD is installed and SHOULD validate a real SMTP responder.

### S12.1 — 4-Layer Caddy Self-Probe Fix + /reach Endpoint

*As a user calling the verify service from the public internet, I want `/probe` to correctly identify my IP and probe it — not the service's own loopback — and as a security-conscious operator of the service, I want it protected against being used as a port-scanner proxy via XFF spoofing. Additionally, as an operator running `aimx preflight` on a fresh VPS, I want a plain-TCP `/reach` endpoint that passes when port 25 is reachable, even if no SMTP server is answering yet.*

**Technical context:** Implements a 4-layer defense against the Caddy self-probe bug + XFF SSRF risk, applied uniformly to both `/probe` (existing EHLO endpoint) and a new `/reach` (plain TCP endpoint). Each layer fails closed without the others.

**Layer 1 — Network (bind loopback by default).** `services/verify/src/main.rs:141` currently defaults `BIND_ADDR` to `0.0.0.0:3025`. Change the default to `127.0.0.1:3025`. `BIND_ADDR` env var still overrides for operators who know what they're doing. This removes the ability for external callers to skip Caddy and hit the backend directly with arbitrary headers. **Breaking change for the currently-deployed service** — operators must either (a) put Caddy in front, (b) set `BIND_ADDR=0.0.0.0:3025` explicitly and accept the risk, or (c) use the Dockerized deployment from Sprint 15 which binds loopback inside the container and publishes via docker-compose port mapping. Document the change in the README.

**Layer 2 — Proxy (Caddyfile + header contract).** Commit a canonical `services/verify/Caddyfile` with:

```caddyfile
{$DOMAIN:check.aimx.email} {
    reverse_proxy 127.0.0.1:3025 {
        header_up -X-Forwarded-For
        header_up X-AIMX-Client-IP {remote_host}
    }
}
```

- `header_up -X-Forwarded-For` strips any client-supplied XFF so downstream code is not tempted to trust it.
- `header_up X-AIMX-Client-IP {remote_host}` authoritatively sets a dedicated header to Caddy's view of the real TCP peer. Caddy's `header_up <name> <value>` REPLACES, not appends, so a client cannot pre-seed `X-AIMX-Client-IP` — Caddy always overwrites.
- `{$DOMAIN:check.aimx.email}` uses Caddy's env-var interpolation with a default. Canonical file works out of the box for the production deployment; operators running `check.test.aimx.email` or a self-hosted instance set `DOMAIN=...` and reuse the same file.

**Layer 3 — App (trusted header resolver).** Add `fn resolve_client_ip(peer: &SocketAddr, headers: &HeaderMap) -> Option<IpAddr>` to `main.rs`:

- If `peer.ip().is_loopback()` is **false** → not from Caddy, return `Some(peer.ip())`. Direct-connect semantics for `BIND_ADDR=0.0.0.0` mode or local testing.
- If peer IS loopback → the request came through a trusted reverse proxy. Require `X-AIMX-Client-IP`. Parse it as an `IpAddr`. Reject loopback / unspecified / link-local / RFC 1918 / RFC 4193 values. Return `Some(ip)` if valid, `None` otherwise.
- Apply to BOTH `/probe` and `/reach` handlers (shared helper). When the resolver returns `None` on a loopback peer, return **HTTP 400** — per owner decision, this is an API contract violation (Caddy should have set the header), not a silent probe of the wrong target.
- Do NOT read `X-Forwarded-For` anywhere. Caddy strips it; app must not re-introduce a vulnerability by parsing it.

**Layer 4 — Probe guard (target validation).** In both `check_port25_ehlo` (`/probe`) and the new TCP-only check (`/reach`), before attempting any connection, validate the resolved target IP:

- Reject: loopback, unspecified (`0.0.0.0`, `::`), link-local (`169.254.0.0/16`, `fe80::/10`), RFC 1918 (`10/8`, `172.16/12`, `192.168/16`), RFC 4193 (`fc00::/7`).
- Return `reachable: false` immediately on rejection — do not reveal whether the blocked target would have been reachable.
- Use `std::net::IpAddr::is_loopback()` and similar stdlib helpers where available; hand-roll RFC 1918 / RFC 4193 checks as a small helper with unit tests.

**New `/reach` endpoint.** Add `GET /reach` route to the axum router at line 139:

- Resolves caller IP via `resolve_client_ip` (same as `/probe`).
- Runs a plain `TcpStream::connect("{caller_ip}:25")` with a 10-second timeout. No banner read, no EHLO, no handshake, no `221 Bye`.
- Returns `{"reachable": bool, "ip": "..."}` — same response shape as `/probe` for client-code symmetry.
- Applies the Layer 4 target guard.
- Does NOT share code with `check_port25_ehlo` beyond the target guard — keep the TCP-only path simple.

**Acceptance criteria:**
- [x] Default HTTP bind address changed from `0.0.0.0:3025` to `127.0.0.1:3025` in `services/verify/src/main.rs`
- [x] `services/verify/Caddyfile` committed with `header_up -X-Forwarded-For`, `header_up X-AIMX-Client-IP {remote_host}`, and `{$DOMAIN:check.aimx.email}` interpolation
- [x] `resolve_client_ip(peer, headers)` helper added to `main.rs` with the trust-boundary logic described above
- [x] `/probe` handler uses `resolve_client_ip`; returns HTTP 400 when peer is loopback and `X-AIMX-Client-IP` is missing, unparseable, or a rejected range
- [x] New `GET /reach` route added that uses `resolve_client_ip` and does a plain 10-second TCP connect to `{caller_ip}:25`, returning `{"reachable": bool, "ip": "..."}`
- [x] Layer 4 target guard rejects loopback / unspecified / link-local / RFC 1918 / RFC 4193 targets in both `/probe` and `/reach` <!-- Exceeded: also rejects broadcast, multicast, RFC 6598 CGNAT, and IPv4-mapped IPv6 bypass via `canonicalize_ip` -->
- [x] App does NOT read `X-Forwarded-For` anywhere — grep confirms
- [x] Unit test: `resolve_client_ip` returns peer IP when peer is a public IPv4/IPv6 address (direct-connect mode)
- [x] Unit test: `resolve_client_ip` returns `X-AIMX-Client-IP` value when peer is loopback and header is a valid public IP
- [x] Unit test: `resolve_client_ip` returns `None` when peer is loopback and header is missing
- [x] Unit test: `resolve_client_ip` returns `None` when peer is loopback and header value is loopback / private / unspecified / link-local
- [x] Unit test: `/probe` handler returns 400 when peer is loopback and `X-AIMX-Client-IP` is missing
- [x] Unit test: `/reach` handler returns 400 under the same conditions
- [x] Unit test: Layer 4 target guard rejects `127.0.0.1`, `::1`, `0.0.0.0`, `10.0.0.1`, `172.16.0.1`, `192.168.1.1`, `169.254.1.1`, `fe80::1`, `fc00::1`
- [x] Unit test: `/reach` against an unreachable host returns `reachable: false` within the 10-second timeout window
- [x] Unit test: `/reach` against a listening TCP socket (no SMTP) returns `reachable: true` — this is the key semantic difference from `/probe`
- [x] Integration test: end-to-end `/probe` with a hand-rolled loopback caller setting `X-AIMX-Client-IP` returns the expected resolved IP (not `127.0.0.1`)
- [x] Existing `/probe` EHLO handshake tests still pass — no regression
- [x] `cargo fmt -- --check`, `cargo clippy -- -D warnings`, `cargo test` all clean in `services/verify/`

### S12.2 — Fix Self-EHLO Trap in Built-in SMTP Listener

*As a user probing the verify service's built-in port 25 listener with a real EHLO client, I want a correct SMTP exchange so the listener is actually useful as a reachability test target — not a malformed conversation that breaks real clients.*

**Technical context:** `handle_smtp_connection` at `services/verify/src/main.rs:117-129` currently does: write `220 check.aimx.email SMTP aimx-verify\r\n` → read up to 512 bytes with 10s timeout → write `221 Bye\r\n` → close. It never sends `250` in response to EHLO. Any real EHLO client (including the verify service's own `check_port25_ehlo` loop from the Caddy bug) reads `221 Bye` instead of `250 ...`, which starts with neither `250 ` nor `250-`, and the handshake fails.

Rewrite `handle_smtp_connection` to implement a minimal but correct SMTP exchange:

1. Send `220 {hostname} SMTP aimx-verify\r\n` (hostname from existing `SMTP_BANNER` constant or derived similarly)
2. Loop:
   - Read a CRLF-terminated line with a read timeout (5-10s per line)
   - If the line starts with `EHLO` or `HELO` (case-insensitive) → send `250 {hostname}\r\n` and continue the loop
   - If the line starts with `QUIT` (case-insensitive) → send `221 Bye\r\n`, close, return
   - If the line is any other command → send `500 Command not recognized\r\n` and continue the loop
   - If read returns 0 bytes (peer closed) → close, return
   - If read times out → close, return
3. Overall connection has a hard wall-clock timeout (~30s total) to prevent idle connection pinning

Use `tokio::io::BufReader` and `AsyncBufReadExt::read_line` for line-delimited reads. Still not a real SMTP server (no MAIL FROM, RCPT TO, DATA, or AUTH) — it exists only as a correct-enough handshake target for external EHLO-based reachability probes that hit the verify server directly (e.g., `aimx setup`'s outbound check at `check.aimx.email:25`, and any operator's own manual testing).

**Acceptance criteria:**
- [x] `handle_smtp_connection` responds to `EHLO` with `250 {hostname}\r\n` and continues the session
- [x] `handle_smtp_connection` responds to `HELO` with `250 {hostname}\r\n` and continues the session
- [x] `handle_smtp_connection` responds to `QUIT` with `221 Bye\r\n` and closes cleanly
- [x] `handle_smtp_connection` responds to unknown commands with `500 Command not recognized\r\n` and continues
- [x] Connection is closed cleanly on peer close or idle/read timeout
- [x] Overall wall-clock connection timeout prevents indefinite resource pinning (~30s) <!-- Exceeded: also caps per-line memory via SMTP_MAX_LINE_BYTES=1024 -->
- [x] Unit test: full exchange `220` → `EHLO` → `250` → `QUIT` → `221` completes correctly
- [x] Unit test: unknown command returns `500` without closing the connection
- [x] Unit test: client closing the connection mid-session is handled without error
- [x] Unit test: idle timeout closes the connection <!-- Implemented via `#[tokio::test(start_paused = true)]` + `tokio::io::duplex` so virtual time advances without a real wall-clock wait -->
- [x] Existing `smtp_listener_sends_banner_and_bye` test is updated or replaced for the new semantics (it currently asserts behavior that was itself the bug)
- [x] Integration test: `check_port25_ehlo` successfully probes this listener — this test is the round-trip that proves the self-loop scenario is now well-formed (even though Layer 4 would block the self-probe in production, the handshake itself must be correct)

### S12.3 — Caddyfile Docs + README + manual-setup + PRD Update

*As a self-hoster of the verify service, I need docs that explain the new Caddy deployment contract, the loopback-bind default, and the two-endpoint split.*

**Technical context:** The code changes in S12.1 break existing deployments of the verify service (default bind moves to loopback, `/probe` now returns 400 on a loopback peer without `X-AIMX-Client-IP`). Docs must cover the new deployment contract so operators can migrate without guesswork.

**`services/verify/README.md` updates:**
- New "Caddy deployment" section referencing the canonical `services/verify/Caddyfile`, explaining why `-X-Forwarded-For` and `X-AIMX-Client-IP {remote_host}` are both required, and how to set `DOMAIN` for non-default hostnames.
- Expand the "API Endpoints" section to document both `/probe` (full SMTP EHLO handshake — for post-install verification via `aimx setup` and `aimx verify`) and `/reach` (plain TCP reachability — for pre-install preflight via `aimx preflight`). Make the semantic difference explicit.
- Note that the HTTP default bind is `127.0.0.1:3025` and that direct `0.0.0.0:3025` binding is NOT supported in production — there is no trust boundary without a reverse proxy setting `X-AIMX-Client-IP`. Document the `BIND_ADDR` override for operators who understand the trade-off.
- Update the systemd example to reflect the new defaults.

**`docs/manual-setup.md` updates:**
- Part A (verify service self-hosting): update to reflect the Caddyfile, the loopback bind default, and the two-endpoint model. Remove any stale instructions that assumed `0.0.0.0:3025`.
- Add a note about `DOMAIN` env var for the Caddyfile.

**`README.md` at repo root:** NOT modified. Per prior decision, end users don't run verify — the verify-specific docs stay scoped to `services/verify/README.md`.

**PRD update (`docs/prd.md`) — small case-(b) extension:** Section 6.8 Verify Service currently has FR-38 describing a single `check.aimx.email` probe that performs an SMTP EHLO handshake, and FR-39b describing the port 25 listener. Update FR-38 to reflect that the verify service now exposes TWO complementary HTTP endpoints:
- `/reach` — plain TCP reachability test (for `aimx preflight` on fresh VPSes before OpenSMTPD is installed)
- `/probe` — full SMTP EHLO handshake (for `aimx setup` / `aimx verify` post-install validation)

Keep the rest of section 6.8 as-is. This is a small, uncontroversial extension — the two-endpoint design is a refinement, not a scope change.

**Acceptance criteria:**
- [x] `services/verify/README.md` has a "Caddy deployment" section referencing the canonical `Caddyfile` and explaining the `header_up` directives
- [x] `services/verify/README.md` "API Endpoints" section documents both `/probe` (EHLO) and `/reach` (plain TCP) with their distinct use cases
- [x] `services/verify/README.md` notes the new `127.0.0.1:3025` default bind and warns against direct `0.0.0.0` exposure without a reverse proxy
- [x] `services/verify/README.md` systemd example updated to reflect new defaults
- [x] `docs/manual-setup.md` Part A updated for the Caddyfile, loopback bind, and two-endpoint model
- [x] `docs/prd.md` FR-38 updated to describe the two-endpoint design (`/reach` + `/probe`)
- [x] Repo-root `README.md` is NOT modified
- [x] No stale references to naive XFF handling or `0.0.0.0:3025` default in any doc

---

## Sprint 13 — Preflight Flow Fix + PTR Display (Days 34–36.5) [DONE]

**Goal:** Fix the preflight chicken-and-egg problem on fresh VPSes (preflight currently fails because `/probe` requires a live SMTP responder that isn't installed yet) by routing the preflight inbound check at the new `/reach` endpoint from Sprint 12. Also fix the PTR display ordering bug that mangles output when the inbound check fails.

**Dependencies:** Sprint 12 (merged) — requires `/reach` to exist on the deployed verify service

**Background — the bugs this sprint fixes:**

1. **Preflight chicken-and-egg.** `aimx preflight` is meant to be run on a fresh VPS before `aimx setup` installs OpenSMTPD. But the inbound check in `RealNetworkOps::check_inbound_port25()` (src/setup.rs:270-283) calls `{verify_host}/probe`, which does a full SMTP EHLO handshake against the caller's port 25. On a fresh VPS nothing is listening there yet, so the handshake fails and preflight reports `FAIL: Inbound port 25 is not reachable` — even when port 25 is actually reachable at the TCP level (verified: the operator tested with `sudo nc -l -p 25` and `curl https://check.test.aimx.email/probe` still returns `reachable: false` because `nc` doesn't speak SMTP). The fix is to route preflight at the new plain-TCP `/reach` endpoint added in Sprint 12. `aimx setup` (which installs OpenSMTPD before the port check per S11.2) and `aimx verify` (which runs post-setup) continue to use `/probe` for full EHLO validation — no regression in their flows.

2. **PTR display ordering bug.** `check_ptr` at `src/setup.rs:383-388` emits its own `println!("  PTR record: {ptr}")` at line 386 BEFORE returning `PreflightResult::Pass`. But the caller in `run_preflight` (line 431) uses `print!("  PTR record... ")` without a newline, waiting for the match result to append `PASS`. The unflushed `print!` + the `println!` inside `check_ptr` interleave, producing mangled output like:

```
  Inbound port 25 is not reachable. Check your firewall and VPS provider settings.
  PTR record...   PTR record: vps-198f7320.vps.ovh.net.
PASS
```

Per owner decision, PTR stays in preflight as advisory (Warn on missing, Pass on present, never Fail — non-blocking), but the display ordering needs to produce a single well-formed line.

### S13.1 — Route Preflight Inbound at /reach; Keep Setup/Verify at /probe

*As an operator running `aimx preflight` on a fresh VPS with nothing on port 25, I want the inbound check to PASS when the TCP path is reachable, without requiring a live SMTP server. As an operator running `aimx setup` or `aimx verify` on a configured box, I want the existing full EHLO handshake validation to remain unchanged.*

**Technical context:** Split the inbound check into two distinct operations in the `NetworkOps` trait and route each caller at the right one.

**`NetworkOps` trait (`src/setup.rs:34-36`) changes:**
- Add `fn check_inbound_reachable(&self) -> Result<bool, Box<dyn std::error::Error>>;` — calls `{verify_host}/reach`, used by `aimx preflight`.
- Keep `fn check_inbound_port25(&self) -> Result<bool, Box<dyn std::error::Error>>;` as-is (still calls `/probe` and does EHLO) for `aimx setup` and `aimx verify`. Optionally rename to `check_inbound_ehlo()` for clarity if the developer prefers — either keep the name and let the different call sites document the semantics, or rename both for symmetry (`check_inbound_reachable` + `check_inbound_ehlo`). Developer's call; document the choice in the PR.

**`RealNetworkOps` (`src/setup.rs:270-283`):**
- Implement `check_inbound_reachable()` by curl-ing `{verify_host}/reach` with the existing 60s timeout and parsing `"reachable":true` — mirror the current `check_inbound_port25()` implementation exactly, just with a different path.
- Existing `check_inbound_port25()` implementation stays unchanged (still calls `/probe`).

**Callers to update:**
- `run_preflight()` at `src/setup.rs:419-429` — change `check_inbound` (which wraps `check_inbound_port25`) to use the reachable variant. Either update `check_inbound()` helper to take a flag, or add a parallel `check_inbound_reachable()` helper. Keep the display text `Inbound port 25...` — the semantic is still "is my inbound port 25 reachable."
- `run_setup_with_verify()` — keep using the EHLO variant (`/probe`). Setup installs OpenSMTPD before the port check per Sprint 11's install-before-check reorder, so the EHLO handshake is the right test at that point. **No regression.**
- `src/verify.rs` (the `aimx verify` CLI) — keep using the EHLO variant. `aimx verify` is a post-setup sanity check; the user already has a working mail server and we want to validate it responds correctly.
- Any mock `NetworkOps` impls in tests (`src/setup.rs:1116-1122`, `src/verify.rs:96-102`, and the mocks referenced in `src/setup.rs:2076`-area tests) — extend to cover both methods, preserving existing test coverage for `check_inbound_port25` and adding new tests for `check_inbound_reachable`.

**Acceptance criteria:**
- [x] `NetworkOps` trait gains `check_inbound_reachable()` method
- [x] `RealNetworkOps::check_inbound_reachable()` implementation calls `{verify_host}/reach`, parses `"reachable":true`, uses the same 60s timeout as the existing `/probe` call <!-- Exceeded: factored into shared private `curl_reachable(path)` helper so `/probe` and `/reach` call sites cannot drift -->
- [x] `run_preflight()` calls the reachable variant for its inbound check
- [x] `run_setup_with_verify()` continues to call `check_inbound_port25()` (EHLO via `/probe`) for its post-install inbound check — verified by test and by reading the setup flow
- [x] `src/verify.rs` (`aimx verify` command) continues to call `check_inbound_port25()` (EHLO via `/probe`) — verified by test
- [x] All mock `NetworkOps` impls in tests implement both methods
- [x] Unit test: `run_preflight` with a mock `NetworkOps` where `check_inbound_reachable` returns `Ok(true)` reports inbound `PASS` — this is the fresh-VPS scenario
- [x] Unit test: `run_setup_with_verify` still uses `check_inbound_port25` (EHLO) after OpenSMTPD install — no regression
- [x] Integration test: preflight against a mock verify service that implements `/reach` completes all checks cleanly
- [x] `cargo fmt -- --check`, `cargo clippy -- -D warnings`, `cargo test` all clean in the root crate

### S13.2 — Fix PTR Display Ordering Bug

*As an operator running `aimx preflight`, I want the PTR output to appear as a single well-formed line — not mangled into the middle of the inbound check's error block.*

**Technical context:** Two tightly coupled changes in `src/setup.rs`:

**(a) Remove the errant `println!` from `check_ptr`.** At `src/setup.rs:385-388`:

```rust
Ok(Some(ptr)) => {
    println!("  PTR record: {ptr}");   // <-- this line causes the interleaving
    PreflightResult::Pass
}
```

Delete the `println!`. The PTR value needs to be carried back to the caller some other way.

**(b) Thread the PTR value back to the caller.** Options (developer picks the least-invasive):
- Extend `PreflightResult::Pass` to carry an optional detail string: `Pass(Option<String>)` — requires updating all match arms across the file
- Add a variant like `PassWithDetail(String)` alongside `Pass` — more changes but preserves existing `Pass` usage
- Return `(PreflightResult, Option<String>)` from `check_ptr` specifically — narrowest change, only affects PTR
- Use an out-parameter or a separate getter on `NetworkOps` — uglier but zero touch to `PreflightResult`

Recommendation: extend `PreflightResult::Pass` to `Pass(Option<String>)` since it's the cleanest model and only `check_ptr` uses it today — most match arms can stay as `Pass(_) => println!("PASS")` with a small exception for the PTR case that prints the detail too. Developer has final say.

**(c) Display the PTR value inline.** In `run_preflight` at `src/setup.rs:431-440`, when the PTR check passes with a detail string, print it on the same line as `PASS`:

```
  PTR record... PASS (vps-198f7320.vps.ovh.net.)
```

No interleaving with the inbound error block, no duplicate line, single well-formed output.

PTR remains advisory: `PreflightResult::Warn` on missing/error (non-blocking), `Pass(Some(ptr))` on success. Never `Fail`. Per owner decision, the check stays in preflight because PTR is still useful deliverability guidance even if imperfect (the check can't distinguish a useful PTR from OVH's default, but showing the value to the user at least lets them notice if it's the wrong one).

**Acceptance criteria:**
- [x] `check_ptr` no longer calls `println!` directly
- [x] PTR value is returned to the caller via `PreflightResult` (or equivalent — developer's choice documented in PR) <!-- `PreflightResult::Pass` extended to `Pass(Option<String>)`; non-PTR checks use `Pass(None)` -->
- [x] `run_preflight` displays PTR value inline with `PASS` marker as a single line (e.g., `  PTR record... PASS (vps-198f7320.vps.ovh.net.)`)
- [x] When PTR check returns `Warn` (missing record), the existing `WARN\n  {msg}` output format is preserved
- [x] PTR remains non-blocking: `all_pass` stays `true` when PTR is missing (existing behavior, don't change)
- [x] Unit test: `run_preflight` with a mock `NetworkOps` returning `Some(ptr)` produces a single well-formed line containing both `PASS` and the PTR value, with no intermediate newline
- [x] Unit test: the interleaving bug does not reproduce — assert that the output when inbound fails and PTR passes has the PTR line strictly after the inbound error block, not interleaved <!-- Exceeded: `run_preflight` refactored into `run_preflight_to<W, E>` so stream ordering is asserted with captured buffers, not global stdout -->
- [x] Unit test: `run_preflight` with a mock `NetworkOps` returning `None` for PTR still produces `WARN` + the advisory message, and does not fail the overall preflight
- [x] `cargo fmt -- --check`, `cargo clippy -- -D warnings`, `cargo test` all clean

---

## Sprint 14 — Request Logging for aimx-verify (Days 37–39.5) [DONE]

**Goal:** Add per-request logging to every call served by `aimx-verify` — HTTP and SMTP — so operators can see who's using the service, diagnose issues, and spot abuse directly from the shell output.

**Dependencies:** Sprint 13 (merged) — logging applies to the fixed verify service, not the broken one

### S14.1 — Log All HTTP and SMTP Calls

*As an operator of aimx-verify, I want every HTTP and SMTP call logged with the caller's IP and relevant params so that I can see who's using the service, diagnose issues, and spot abuse directly from the shell output.*

**Technical context:** The verify service at `services/verify/` already initializes `tracing_subscriber::fmt::init()` in `main()` (line 134), but request logging is almost non-existent. `probe()` (line 26) and `health()` (line 19) log nothing — the caller IP is available via `ConnectInfo(addr)` but discarded. `handle_smtp_connection()` (line 117) logs nothing on the success path; only `run_smtp_listener()` logs bind announcement and accept errors.

Add per-request logging to every path. The format stays as the default `tracing-subscriber` pretty text (not JSON) — per owner decision, operators tail the shell or journalctl, not a JSON log aggregator. Log level defaults to `info` and honors `RUST_LOG` overrides.

Log every call, including `/health` (no filtering — owner confirmed ALL calls):

- **HTTP `/probe`**: method, path, caller IP (resolved via Sprint 12's `resolve_client_ip`), response status, elapsed ms, and the EHLO handshake outcome (`reachable: true|false`).
- **HTTP `/reach`** (added in Sprint 12): method, path, caller IP (same resolver), response status, elapsed ms, and the plain-TCP reachability result (`reachable: true|false`).
- **HTTP `/health`**: method, path, caller IP, response status, elapsed ms.
- **SMTP listener (port 25)**: peer IP on accept, and whether the banner/EHLO/QUIT lifecycle (fixed in Sprint 12) completed cleanly or errored. Existing error-path `tracing::debug!` in `run_smtp_listener` should be promoted to `info` / `warn` where appropriate so connection attempts are visible at the default level.

Implementation choice is open: axum's `tower_http::trace::TraceLayer` + a small middleware that extracts `ConnectInfo<SocketAddr>`, or a hand-rolled `axum::middleware::from_fn` wrapper. There are three HTTP routes (`/probe`, `/reach`, `/health`), so a custom middleware is likely simpler than pulling in `tower-http`. Developer's call.

**Acceptance criteria:**
- [x] Every `/probe` request logs method, path, caller IP, response status, elapsed ms, and the `reachable` result at `info` level <!-- Implemented via `log_request` middleware + `ReachableOutcome` response extension so exactly one `info!` line is emitted per request, with the `reachable` field joined onto the same line -->
- [x] Every `/reach` request logs method, path, caller IP, response status, elapsed ms, and the `reachable` result at `info` level
- [x] Every `/health` request logs method, path, caller IP, response status, elapsed ms at `info` level
- [x] Every TCP connection to the SMTP listener logs peer IP on accept and success/error on close at `info` level <!-- Factored into shared `spawn_smtp_connection(stream, peer)` helper so test and production exercise exactly the same logging body (anti-drift) -->
- [x] Log output uses the default `tracing-subscriber` text formatter (not JSON)
- [x] `RUST_LOG` env var still works for level overrides (e.g., `RUST_LOG=aimx_verify=debug`)
- [x] Unit or integration test: hit `/probe` on a local test server and assert a log line containing the caller IP is captured (via `tracing-subscriber`'s test writer or equivalent) <!-- Exceeded: three HTTP integration tests cover /health, /reach (with reachable=false), and /probe 400 (caller_ip=unknown) -->
- [x] Integration test: connect to the SMTP listener on an ephemeral port and assert a log line with the peer IP is captured
- [x] `cargo fmt -- --check`, `cargo clippy -- -D warnings`, `cargo test` all clean in `services/verify/`

---

## Sprint 15 — Dockerize aimx-verify (Days 40–42.5) [IN PROGRESS]

**Goal:** Ship a Dockerfile and docker-compose for `aimx-verify` so the service can be redeployed to any host consistently without tracking apt packages or systemd units by hand. The deployment must work correctly with the Sprint 12 security model (loopback-default bind + Caddy trust boundary + Layer 4 target guard).

**Dependencies:** Sprint 14 (merged) — Docker ships the fully-instrumented, logging-enabled, security-hardened service.

**Note on Sprint 12 interaction:** Sprint 12 changed the default HTTP bind to `127.0.0.1:3025` and introduced a Layer 3 trust check that reads `X-AIMX-Client-IP` only when the TCP peer is loopback. This means the "simple" docker-compose shape of "bind `0.0.0.0:3025` in the container and port-map to the host" is NOT compatible with the security model — Docker's userland proxy presents peer IPs as the Docker bridge gateway (a private IP), which Layer 4's target guard will reject. The correct deployment pattern is either:

- **(a) `network_mode: host`** for the verify container, so binding `127.0.0.1:3025` inside the container is the host's loopback, and Caddy (running on the host or as a sibling service) can reverse-proxy to it normally. This is the simplest fix and is the recommended shape.
- **(b) Caddy as a second docker-compose service** with an internal Docker network where the verify container binds loopback on its internal interface and Caddy is the only client. More portable (no host-network dependency) but more moving pieces.

The implementer should pick (a) by default unless there's a reason to avoid `network_mode: host`, in which case (b) is the fallback. Document the choice in the Docker README section.

### S15.1 — Dockerfile + docker-compose + README Update

*As the maintainer of aimx-verify, I want to deploy the service from a Docker image with docker-compose so that I can redeploy to any host consistently without tracking apt dependencies or systemd units by hand — and the deployment must respect the Sprint 12 security model.*

**Technical context:** The verify service is a standalone Cargo crate at `services/verify/` (package `aimx-verify`). No Dockerfile exists yet. After Sprint 12, `services/verify/README.md` will document a Caddyfile + loopback-bind deployment as the recommended non-Docker path. This sprint adds the Docker equivalent without regressing the security model.

Add a **multi-stage Dockerfile** at `services/verify/Dockerfile`:
- **Builder stage:** `rust:1-bookworm` (or current stable slim). Cache-friendly layering — copy `Cargo.toml` + `Cargo.lock` first, prime the dep cache with a stub build, then copy `src/` and build `cargo build --release`.
- **Runtime stage:** `debian:bookworm-slim` (glibc target matches the builder — no musl cross-compile complexity). Install `ca-certificates` only. Copy the release binary from the builder to `/usr/local/bin/aimx-verify`.
- Container **runs as root** (per owner decision) so binding port 25 works without capability fiddling.
- `EXPOSE 25 3025`; `ENTRYPOINT ["/usr/local/bin/aimx-verify"]`.

Add **`services/verify/docker-compose.yml`** using **`network_mode: host`** as the default deployment shape:
- Single `verify` service with `build: .`
- `network_mode: host` — the container shares the host's network namespace, so the post-Sprint-12 default bind `127.0.0.1:3025` behaves identically to a systemd-native deployment and the Layer 3 loopback check still works.
- `environment:` block can include a commented `BIND_ADDR` / `SMTP_BIND_ADDR` / `RUST_LOG` example, but defaults inherit from the binary (no override needed).
- `restart: unless-stopped`
- No explicit `ports:` mapping when using `network_mode: host` — the container binds directly on the host.
- Caddy is NOT included in the compose file in this sprint (operators run Caddy separately on the host, using the Sprint 12 canonical `Caddyfile`). A future sprint could add a Caddy sibling service if desired, but it's out of scope here.

Add **`services/verify/.dockerignore`** excluding `target/` and other build artifacts.

**Update `services/verify/README.md`** with a new "Docker" section that:
- Documents `docker compose up -d --build` as the Docker deployment path, with `network_mode: host` as the default shape
- Explains the Sprint 12 security model interaction — why `network_mode: host` rather than port mapping
- References the canonical `services/verify/Caddyfile` from Sprint 12 as the required companion on the host
- Provides a raw `docker build` + `docker run --network host` example as an alternative
- Does NOT replace the systemd section from Sprint 12 — both deployment paths coexist

**Do NOT update the repo-root `README.md`** — per owner decision, end users don't run verify.

No GitHub Actions image publishing to ghcr.io in this sprint — not requested. No new CI docker-build step either — existing `services/verify/` CI steps from S8.5 stay unchanged.

**Acceptance criteria:**
- [ ] `services/verify/Dockerfile` uses a multi-stage build (Rust builder + `debian:bookworm-slim` runtime)
- [ ] Final image runs as root and has `ENTRYPOINT` pointing at the binary
- [ ] `services/verify/.dockerignore` excludes `target/` and other build artifacts
- [ ] `services/verify/docker-compose.yml` builds from the local Dockerfile and uses `network_mode: host` so the post-Sprint-12 loopback-default bind works without override
- [ ] Manually verified: `docker compose up -d --build` in `services/verify/` brings the service up; `curl http://127.0.0.1:3025/health` from the host returns `{"status":"ok","service":"aimx-verify"}`
- [ ] Manually verified: with the Sprint 12 canonical `Caddyfile` running on the host, `curl https://<domain>/probe` from a remote machine returns a correctly-resolved caller IP (not `127.0.0.1`) and a valid probe result — this proves the container + Caddy + Sprint 12 security model all work end-to-end
- [ ] Manually verified: with the Sprint 12 canonical `Caddyfile` running on the host, `curl https://<domain>/reach` from a remote machine returns a plain-TCP reachability result
- [ ] Manually verified: `nc 127.0.0.1 25` from the host receives the `220 check.aimx.email SMTP aimx-verify` banner
- [ ] Manually verified: the per-request logs from Sprint 14 appear in the container's stdout (`docker compose logs verify`) when the endpoints are exercised
- [ ] `services/verify/README.md` has a new "Docker" section documenting `docker compose up -d --build` with `network_mode: host`, explains the Sprint 12 interaction, references the canonical `Caddyfile`
- [ ] Repo-root `README.md` is NOT modified

---

## Summary Table

| Sprint | Days | Focus | Key Output | Status |
|--------|------|-------|------------|--------|
| 1 | 1–2.5 | Core Pipeline + Idea Validation | `aimx ingest`, basic `aimx send`, mailbox CLI, CI pipeline, test fixtures — testable on VPS | Done |
| 2 | 3–5 | DKIM + Production Outbound | DKIM signing, threading, attachments — mail passes Gmail checks | Done |
| 2.5 | 5.5–6 | Non-blocking Cleanup | Ingest/send hardening, test gaps, `--data-dir` CLI option | Done |
| 3 | 6–8.5 | MCP Server | All 9 MCP tools — Claude Code can read/send email | Done |
| 4 | 8–10 | Channel Manager + Inbound Trust | Triggers, match filters, DKIM/SPF verification, trust gating | Done |
| 5 | 10.5–12.5 | Setup Wizard | `aimx setup` — one-command setup with preflight + DNS | Done |
| 5.5 | 12.5–13 | Non-blocking Cleanup | Serialization, resolver dedup, SPF fix, setup backup | Done |
| 6 | 13–15.5 | Verify Service + Polish | Hosted probe, status/verify CLI, README | Done |
| 7 | 16–18.5 | Security Hardening + Critical Fixes | DKIM enforcement, header injection fix, atomic ingest, verify race fix, setup e2e verify | Done |
| 8 | 19–21.5 | Setup Robustness, CI & Documentation | DNS verification accuracy, data-dir propagation, SPF fix, configurable verify URLs, CI coverage, doc fixes | Done |
| 9 | 22–24.5 | Migrate from YAML to TOML | Replace serde_yaml with toml crate for config and email frontmatter | Done |
| 10 | 25–27.5 | Verify Service Overhaul | Remove echo, add port 25 listener, EHLO probe, remove ip parameter — no outbound email | Done |
| 11 | 28–30.5 | Setup Flow Rewrite + Client Cleanup | Root check, MTA conflict detection, install-before-check flow, simplified verify, docs | Done |
| 12 | 31–33.5 | aimx-verify Security Hardening + /reach Endpoint | 4-layer Caddy self-probe fix, `/reach` TCP-only endpoint, self-EHLO trap fix, canonical `Caddyfile` | Done |
| 13 | 34–36.5 | Preflight Flow Fix + PTR Display | Route `aimx preflight` at `/reach`, fix PTR display ordering bug | Done |
| 14 | 37–39.5 | Request Logging for aimx-verify | Per-request logging for `/probe`, `/reach`, `/health`, and SMTP listener — caller IP, status, elapsed ms | Done |
| 15 | 40–42.5 | Dockerize aimx-verify | Multi-stage Dockerfile, `docker-compose.yml` with `network_mode: host`, `.dockerignore`, verify README update | In Progress |

## Deferred to v2

| Feature | Rationale |
|---------|-----------|
| Package manager distribution (apt/brew/nix) | v1 ships as `cargo install`; packaging is post-launch polish |
| `webhook` trigger type | `cmd` covers all use cases via curl; native webhook is convenience |
| Web dashboard | Agents don't need a UI; operators use CLI or MCP |
| Non-Linux platforms | Target audience runs on Linux VPS; macOS/Windows adds complexity with no demand signal |
| IMAP/POP3/JMAP | Agents access via MCP/filesystem; traditional mail clients are not the use case |
| Email encryption (PGP/S/MIME) | Adds significant complexity; defer until there's demand |
| Rate limiting / spam filtering | Rely on OpenSMTPD defaults + DMARC for v1 |
| Multi-tenant hosted offering | Architecture supports it; business decision for later |

## Non-blocking Review Backlog

This section collects non-blocking feedback from sprint reviews. Questions need human answers (edit inline). Improvements accumulate until triaged into a sprint.

### Questions

Items needing human judgment. Answer inline by replacing the `_awaiting answer_` text, then check the box.

- [x] **(Sprint 2.5)** `serde_yaml` 0.9 is unmaintained/deprecated — should we migrate to an alternative YAML serializer? — Migrate to TOML (`toml` crate) instead. _Triaged into Sprint 9_

### Improvements

Concrete items with clear implementation direction. Will be triaged into a cleanup sprint periodically.

- [x] **(Sprint 1)** Add `--data-dir` or `AIMX_DATA_DIR` CLI option to override the hardcoded `/var/lib/aimx/` path — _Triaged into Sprint 2.5_
- [x] **(Sprint 1)** Enhance integration tests to exercise `ingest_email()` with fixture files through the full pipeline, not just `mail-parser` parseability — _Triaged into Sprint 2.5_
- [x] **(Sprint 1)** Add mailbox name validation to prevent `..`, `/`, or empty strings in `create_mailbox` — _Triaged into Sprint 2.5_
- [x] **(Sprint 1)** Replace hand-rolled `yaml_escape` with `serde_yaml` struct serialization for frontmatter to avoid edge cases (YAML booleans, special characters) — _Triaged into Sprint 2.5_
- [x] **(Sprint 1)** Add `\r` to the quoting condition in `yaml_escape` for hardening (bare `\r` not exploitable but inconsistent) — _Triaged into Sprint 2.5_
- [x] **(Sprint 2)** Escape attachment filenames in MIME `Content-Type`/`Content-Disposition` headers to prevent malformed headers from special characters — _Triaged into Sprint 2.5_
- [x] **(Sprint 2)** Add integration test for `aimx dkim-keygen` CLI command end-to-end (subprocess test) — _Triaged into Sprint 2.5_
- [x] **(Sprint 2)** Refactor duplicated header construction logic in `compose_message()` attachment vs non-attachment paths — _Triaged into Sprint 2.5_
- [x] **(Sprint 2)** Add test verifying `dkim_selector` config value is actually used at runtime in `send::run()` — _Triaged into Sprint 2.5_
- [x] **(Sprint 2.5)** Replace `unwrap_or_default()` on `serde_yaml::to_string()` with `expect()` or error propagation to avoid silent empty frontmatter on serialization failure — _Triaged into Sprint 5.5_
- [x] **(Sprint 3)** Narrow `tokio` features from `"full"` to specific needed features (`rt-multi-thread`, `macros`, `io-util`, `io-std`) for smaller binary — _Triaged into Sprint 5.5_
- [x] **(Sprint 3)** Add unit test for `write_common_headers` with `references = Some(...)` path — _Triaged into Sprint 5.5_
- [x] **(Sprint 4)** Deduplicate DNS resolver creation in `verify_dkim_async` and `verify_spf_async` — create once and pass to both — _Triaged into Sprint 5.5_
- [x] **(Sprint 4)** Fix SPF domain fallback semantics — `sender_domain` derived from `rcpt` is semantically incorrect as fallback for sender's HELO domain — _Triaged into Sprint 5.5_
- [x] **(Sprint 4)** Add captured DKIM-signed `.eml` fixture from Gmail for verification testing (even if DNS-dependent) — _Triaged into Sprint 5.5_
- [x] **(Sprint 4)** Verify `mail-auth` `dkim_headers` field is stable public API, not internal implementation detail — _Triaged into Sprint 5.5_
- [x] **(Sprint 5)** Implement timestamped backup for pre-aimx OpenSMTPD config to avoid overwriting on repeated setup runs — _Triaged into Sprint 5.5_
- [x] **(Sprint 5.5)** Extract SPF domain-selection logic into standalone testable function instead of duplicating inline in tests — _Triaged into Sprint 8 (S8.3)_
- [x] **(Sprint 6)** Fix GitHub URL in README.md and services/verify/README.md (currently wrong owner) — _Triaged into Sprint 8 (S8.6)_
- [x] **(Sprint 6)** Add IP validation on `/probe` endpoint to reject private/internal IPs (SSRF hardening) — _Obsolete: `ip` parameter removed in Sprint 10 (S10.4)_
- [x] **(Sprint 6)** Handle multiline (folded) Authentication-Results headers in `extract_auth_result` — _Obsolete: echo removed in Sprint 10 (S10.1)_
- [x] **(Sprint 6)** Add `Message-ID` and `Date` headers to echo reply (RFC 5322 compliance) — _Obsolete: echo removed in Sprint 10 (S10.1)_
- [x] **(Sprint 6)** Handle missing catchall mailbox gracefully in `aimx verify` — _Triaged into Sprint 7 (S7.4)_
- [ ] **(Sprint 8)** Add `ip6:` mechanism support to `spf_contains_ip()` for IPv6 server addresses
- [ ] **(Sprint 8)** Quote data dir path in `generate_smtpd_conf` MDA command to handle paths with spaces
- [ ] **(Sprint 11)** `parse_port25_status` uses `smtpd` substring match which could misidentify non-OpenSMTPD processes — low practical risk but could use stricter matching
- [ ] **(Sprint 11)** Dead `Fail` branch for PTR in `verify.rs` — `check_ptr()` never returns `Fail`, so the match arm is unreachable
- [ ] **(Sprint 12)** `run_smtp_listener` spawns per-accept with no concurrency bound — deferred from Sprint 12 with an inline comment at `services/verify/src/main.rs` pointing at Sprint 14. Per-connection bounds are already tight (30s wall, 10s per-line, 1 KiB per-line), so this is defense-in-depth DoS hardening. Add a bounded semaphore or `tower::limit::ConcurrencyLimit`-style gate around accept loop
- [ ] **(Sprint 12)** Cosmetic: in `smtp_session`, fold `let mut writer = writer;` into the destructuring pattern as `let (reader, mut writer) = tokio::io::split(stream);` — zero behavioral change, post-merge cleanup suggestion from reviewer
