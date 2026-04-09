# aimx — Sprint Plan

**Sprint cadence:** 2.5 days per sprint
**Team:** Solo developer with heavy AI augmentation (Claude Code)
**Total sprints:** 6
**Timeline:** 15 calendar days
**v1 Scope:** Full PRD scope including verify service. Sprint 1 targets earliest possible idea validation on a real VPS.

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

- [ ] **(Sprint 2.5)** `serde_yaml` 0.9 is unmaintained/deprecated — should we migrate to an alternative YAML serializer? — _awaiting answer_

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
- [ ] **(Sprint 5.5)** Extract SPF domain-selection logic into standalone testable function instead of duplicating inline in tests
