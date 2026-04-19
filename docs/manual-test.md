# aimx Manual Testing Plan

## Context

aimx is a self-hosted email server for AI agents. This plan manually verifies an end-to-end install on a fresh VPS: setup, inbound/outbound mail, agent integrations (Claude Code + Codex CLI), channel triggers, and the trust model. Every test below is copy-pasteable — run top-to-bottom on the VPS as root unless noted. Tests that need external help (sending from Gmail, receiving at `chua@uzyn.com`) call that out explicitly and include a prompt to hand off to a human collaborator.

**Test domain assumption.** Replace `mail.example.com` with your real test domain everywhere below. The plan uses `inbox`, `agent`, and `test` as mailbox names.

**Notation.** `[VPS]` = run on the aimx VPS (the same machine where `claude` / `codex` are installed — aimx is designed for a host dedicated to AI agents). `[GMAIL]` = user action in Gmail web UI. Commands run as root unless the step says otherwise; `aimx send` and `aimx agent-setup` refuse to run as root, so run those as a non-root user on the VPS.

---

## T0 — Pre-flight

1. Fresh Linux VPS with public IPv4, port 25 unblocked by the provider.
2. DNS zone editable for the test domain.
3. Access to the `chua@uzyn.com` mailbox from anywhere (to observe outbound).
4. A Gmail account (to observe inbound external deliverability).
5. `claude` and `codex` CLIs installed and logged in **on the VPS**, under a non-root user (the same user that will run `aimx send` and `aimx agent-setup`).

```bash
# [VPS] sanity — port 25 not already bound
sudo ss -tlnp sport :25
# expect: no rows
```

---

## T1 — Fresh setup & service start

**Goal.** `aimx setup` produces a working config, DKIM key, TLS cert, and enabled service.

```bash
# [VPS]
sudo aimx setup mail.example.com
```

**What setup prints (no interactive prompts when the domain is passed as an argument).**
- Passing the domain on the command line skips the domain prompt. Verify host and service installation are non-interactive (CLI flags / unconditional).
- Capture the printed DKIM TXT record (the `dkim._domainkey.mail.example.com` line) — you will publish this to DNS below.
- Note the `[DNS]`, `[MCP]`, and `[Deliverability]` sections; keep the terminal scrollback for reference.

**Then publish DNS.** In your DNS provider, add:
- `A mail.example.com → <VPS ipv4>`
- `MX mail.example.com → mail.example.com. (priority 10)`
- `TXT dkim._domainkey.mail.example.com` → the printed DKIM value
- `TXT mail.example.com → "v=spf1 a mx ~all"`

**Start the service and verify.**

```bash
# [VPS]
sudo systemctl daemon-reload
sudo systemctl enable --now aimx
sudo systemctl status aimx --no-pager

sudo ss -tlnp sport :25          # aimx listening on :25
ls -la /run/aimx/send.sock       # UDS present, mode 0666
sudo aimx doctor                 # summary output
sudo aimx portcheck              # port 25 reachability probe
```

**Acceptance criteria.**
- `systemctl status aimx` → `active (running)`.
- `ss` shows aimx on `:25`.
- `/run/aimx/send.sock` exists with mode `srw-rw-rw-`.
- `aimx portcheck` reports port 25 reachable.
- `/etc/aimx/config.toml` exists and contains your domain.
- `/etc/aimx/dkim/private.key` is `0600 root:root`.
- `/var/lib/aimx/README.md` was generated.
- DKIM DNS lookup returns the published key: `dig +short TXT dkim._domainkey.mail.example.com`.

---

## T2 — Create mailboxes

```bash
# [VPS]
sudo aimx mailboxes list
sudo aimx mailboxes create inbox
sudo aimx mailboxes create agent
sudo aimx mailboxes create test
sudo aimx mailboxes list
ls /var/lib/aimx/inbox/
```

**Acceptance.** Four mailbox directories exist under `/var/lib/aimx/inbox/` and `/var/lib/aimx/sent/`: `catchall/` (created by `aimx setup` as the default fallback — see `src/setup.rs`), plus `inbox/`, `agent/`, and `test/` just created. The `catchall` mailbox also appears in `aimx mailboxes list` and in `/etc/aimx/config.toml`.

---

## T3 — Inbound email (external send from Gmail)

**Goal.** Real inbound mail over SMTP lands as a Markdown file with correct frontmatter and passes DKIM/SPF/DMARC.

**Handoff to human.**

> Please **compose a new email** (do NOT forward or reply to an existing thread — forwarded/replied messages pick up `Fwd:`/`Re:` subject prefixes and carry `In-Reply-To`/`References` headers that add noise to the frontmatter) from your Gmail account:
> - **To:** `inbox@mail.example.com`
> - **Subject:** `aimx inbound test 1`
> - **Body:** anything; include 1 PDF or image attachment to exercise the bundle path.

```bash
# [VPS] Drop a mark file BEFORE asking the human to hit send, so we can
# use it as a stable "-newer" reference. /tmp's mtime is not stable.
touch /tmp/.aimx-t3-mark

# [VPS] watch the inbox directory while the email is in flight
watch -n 1 'ls -la /var/lib/aimx/inbox/inbox/'

# After it arrives:
ls /var/lib/aimx/inbox/inbox/
# Read the newest file (flat .md or bundle dir)
sudo find /var/lib/aimx/inbox/inbox/ -name '*.md' -newer /tmp/.aimx-t3-mark -print
sudo cat /var/lib/aimx/inbox/inbox/<newest>
```

**Acceptance criteria.**
- File named `YYYY-MM-DD-HHMMSS-aimx-inbound-test-1.md` (or bundle dir of same name with `.md` inside).
- TOML frontmatter between `+++` delimiters contains:
  - `from = "<your-gmail>"`, `to = "inbox@mail.example.com"`, `subject = "aimx inbound test 1"`
  - `message_id`, `thread_id` (16-hex), `received_at`, `size_bytes`
  - `dkim = "pass"`, `spf = "pass"`, `dmarc = "pass"` (Gmail signs everything)
  - `mailbox = "inbox"`, `read = false`
- If you attached a file: bundle directory contains the attachment, and frontmatter lists it under `[[attachments]]` with a correct `path`.
- Body below `+++` is plain text (HTML was converted).

**Gmail deliverability check.** In the Gmail "sent" view, open the message header and confirm no bounce. If delivery silently failed, check `sudo journalctl -u aimx -n 200 --no-pager`.

---

## T4 — Outbound email (send to chua@uzyn.com)

**Goal.** `aimx send` delivers via MX, gets DKIM-signed, lands in `sent/`, and arrives in the recipient's inbox (not spam).

```bash
# [VPS] run as a non-root user (aimx send refuses root)
aimx send \
  --from inbox@mail.example.com \
  --to chua@uzyn.com \
  --subject "aimx outbound test 1" \
  --body "This message was sent by aimx send over MX."
```

**Expected stdout.** `AIMX/1 OK <message-id>` and exit code `0`.

**Handoff to human.**

> Please check `chua@uzyn.com` for a message titled `aimx outbound test 1`. Paste the full headers back so we can confirm: (a) it landed in Inbox not Spam, (b) DKIM=pass, SPF=pass, DMARC=pass, (c) `Authentication-Results` shows `mail.example.com`.

**Acceptance criteria.**
- Exit code `0`; message ID printed.
- `/var/lib/aimx/sent/inbox/` contains a new `.md` with outbound frontmatter: `outbound = true`, `delivery_status = "delivered"`, `delivered_at`.
- Recipient-side headers show DKIM=pass, SPF=pass, DMARC=pass.
- Message appears in Inbox, not Spam.

**Failure branches to test.**

```bash
# Root refusal (exit 2)
sudo aimx send --from inbox@mail.example.com --to chua@uzyn.com --subject x --body x
# expect: error about running as root, exit 2

# Socket missing (exit 2) — simulate by stopping the daemon briefly
sudo systemctl stop aimx
aimx send --from inbox@mail.example.com --to chua@uzyn.com --subject x --body x
# expect: "daemon not running" guidance, exit 2
sudo systemctl start aimx

# Unknown mailbox (daemon ERR, exit 1)
aimx send --from bogus@mail.example.com --to chua@uzyn.com --subject x --body x
# expect: AIMX/1 ERR MAILBOX ..., exit 1
#
# Note: if you instead see `AIMX/1 ERR DOMAIN ...`, your From: domain does
# not match the configured aimx domain. Check the `domain =` line in
# /etc/aimx/config.toml and re-try with a local-part under that domain.
```

---

## T5 — Inbound routing into specific mailboxes

**Goal.** Different local parts route to their own mailboxes; unknown local-parts fall through to the `catchall` mailbox (no SMTP reject in v1).

**Handoff to human.** **Compose three new emails** from Gmail in sequence (do NOT forward or reply to any prior thread — the resulting `Fwd:`/`Re:` prefixes and `In-Reply-To`/`References` headers add noise to the frontmatter this test is inspecting):
1. To `agent@mail.example.com`, subject `route-test-agent`.
2. To `test@mail.example.com`, subject `route-test-test`.
3. To `nobody@mail.example.com`, subject `route-test-nobody` (unknown mailbox).

```bash
# [VPS]
ls /var/lib/aimx/inbox/agent/
ls /var/lib/aimx/inbox/test/
# Unknown local-parts route silently to the `catchall` mailbox created by
# `aimx setup` (see `Config::resolve_mailbox` in src/config.rs). There is
# no SMTP-level 550 for an unknown local-part in v1.
ls /var/lib/aimx/inbox/catchall/ | grep route-test-nobody
sudo cat /var/lib/aimx/inbox/catchall/*route-test-nobody*.md | head -40
```

**Acceptance.**
- `agent/` and `test/` each contain the respective message.
- The `nobody@` message appears under `/var/lib/aimx/inbox/catchall/` with correct frontmatter (this is the v1 catch-all behavior — there is no SMTP reject for unknown local-parts).

---

## T6 — Claude Code agent integration

**Goal.** The Claude Code skill is installed, the MCP server starts, and Claude can list and send email.

```bash
# [VPS] run as the non-root user that owns the claude install
aimx agent-setup claude-code
ls -la ~/.claude/plugins/aimx/
# expect (dot-directory is only visible with -la):
#   .claude-plugin/plugin.json
#   skills/aimx/SKILL.md
#   skills/aimx/references/*.md
ls -la ~/.claude/plugins/aimx/.claude-plugin/
ls    ~/.claude/plugins/aimx/skills/aimx/
```

Restart Claude Code (exit and relaunch), then test:

```bash
# [VPS] list inbox via MCP
claude -p "Use the aimx MCP tools. Call email_list with mailbox='inbox' and unread=true. Summarize what's there."
```

**Acceptance — list.** Claude responds with a summary matching the emails delivered in T3/T5.

```bash
# [VPS] send via MCP
claude -p "Use the aimx MCP tool email_send to send from inbox@mail.example.com to chua@uzyn.com. Subject: 'aimx claude MCP test'. Body: 'Sent via Claude MCP.'"
```

**Handoff.** Confirm receipt at `chua@uzyn.com`.

**Acceptance — send.**
- Claude reports a successful send with a message ID.
- `/var/lib/aimx/sent/inbox/` gains a new `.md` with `outbound = true`.
- Recipient receives the mail with DKIM=pass.

```bash
# [VPS] read + mark_read
claude -p "List unread mail in 'inbox'. Read the most recent one, summarize in one sentence, then mark it read."
```

**Acceptance — read/mark.** The read-flag updates in the mailbox file: `grep '^read' /var/lib/aimx/inbox/inbox/<file>.md` shows `read = true`.

---

## T7 — Codex CLI agent integration

```bash
# [VPS] run as the non-root user that owns the codex install
aimx agent-setup codex
ls -la ~/.codex/skills/aimx/
# expect: SKILL.md + references/*.md
#
# Note: Codex CLI does NOT auto-discover plugins under ~/.codex/plugins/
# (validated against Codex CLI 0.117.0). MCP wiring lives in
# ~/.codex/config.toml and is populated by the `codex mcp add` step below.
```

Follow the printed `codex mcp add aimx -- /usr/local/bin/aimx mcp` step verbatim, then restart Codex. Confirm registration with:

```bash
grep -A2 '\[mcp_servers.aimx\]' ~/.codex/config.toml
# expect: command = "/usr/local/bin/aimx" and args = ["mcp"]
```

```bash
# [VPS]
codex exec "Use the aimx MCP tools. Call email_list with mailbox='inbox' and unread=true. Summarize."

codex exec "Use the aimx MCP tool email_send to send from inbox@mail.example.com to chua@uzyn.com. Subject: 'aimx codex MCP test'. Body: 'Sent via Codex MCP.'"

codex exec "List unread mail in mailbox 'inbox'. Read the most recent one, summarize it, then mark it read."
```

**Acceptance.** Same as T6: list returns real emails, send produces a delivered `.md` in `sent/inbox/`, recipient receives it DKIM-signed, mark-read flips the `read` field.

---

## T8 — Channel trigger fires on TRUSTED sender

**Goal.** A configured `on_receive` shell command runs when a trusted sender emails, and receives the expanded template variables.

Edit `/etc/aimx/config.toml` to **update the existing `[mailboxes.test]` section** (created by `aimx mailboxes create test` in T2). Set `trust` and `trusted_senders` on the existing table, and append the `on_receive` sub-table under it:

```toml
[mailboxes.test]
address = "test@mail.example.com"
trust = "verified"
trusted_senders = ["chua@uzyn.com"]

[[mailboxes.test.on_receive]]
type = "cmd"
command = '''
touch /tmp/aimx-trigger-{id}.flag
printf 'from=%s\nsubject=%s\nfilepath=%s\n' "$AIMX_FROM" "$AIMX_SUBJECT" "$AIMX_FILEPATH" >> /tmp/aimx-trigger.log
'''
```

> Do NOT paste this as a second block. TOML does not allow two `[mailboxes.test]` tables in the same file — having both the T2-created block and a new block produces undefined parse behavior. Replace the fields on the existing table in-place.

```bash
# [VPS]
sudo systemctl restart aimx
rm -f /tmp/aimx-trigger*.flag /tmp/aimx-trigger.log
```

**Handoff.** **Compose a new email** from `chua@uzyn.com` (from your laptop — trusted sender) to `test@mail.example.com` with subject `trigger-trusted`. Do NOT forward or reply to an existing thread — forwarded/replied messages arrive with `Fwd:`/`Re:` subjects and `In-Reply-To`/`References` headers that pollute the test's frontmatter assertions.

```bash
# [VPS]
ls /tmp/aimx-trigger-*.flag
cat /tmp/aimx-trigger.log
sudo grep '^trusted' /var/lib/aimx/inbox/test/*trigger-trusted*.md
```

**Acceptance.**
- A `/tmp/aimx-trigger-<id>.flag` file was created.
- `/tmp/aimx-trigger.log` shows the expected `from=`, `subject=`, `filepath=` lines.
- The stored `.md` frontmatter has `trusted = "true"` and `dkim = "pass"`.

---

## T9 — Off-allowlist sender: trigger gate vs. `trusted` field

Keep the T8 config. Clear state:

```bash
# [VPS]
rm -f /tmp/aimx-trigger*.flag /tmp/aimx-trigger.log
```

**Handoff.** **Compose a new email** from a Gmail address NOT in `trusted_senders` (e.g. the Gmail used in T3) to `test@mail.example.com` with subject `trigger-untrusted`. Do NOT forward or reply to any prior thread — we want a clean frontmatter with no `In-Reply-To` / `References` headers.

```bash
# [VPS]
ls /tmp/aimx-trigger-*.flag 2>/dev/null
sudo ls /var/lib/aimx/inbox/test/ | grep trigger-untrusted
sudo grep '^trusted' /var/lib/aimx/inbox/test/*trigger-untrusted*.md
```

**Acceptance (v1 semantics — trigger gate is wider than the `trusted` field).**
- A `/tmp/aimx-trigger-<id>.flag` file **is** created, because Gmail's DKIM passes and the v1 trigger gate fires on "allowlisted OR DKIM-pass" (see `src/channel.rs::should_execute_triggers` and the parity test in `src/trust.rs`).
- The email is stored in the mailbox with frontmatter `trusted = "false"` — the `trusted` field is strictly stronger (requires allowlisted AND DKIM-pass), so an off-allowlist Gmail sender yields `trusted = "false"` even though the trigger fired.
- The `.md` contains `dkim = "pass"` but `trusted = "false"`.

> **Subtlety confirmed.** This test demonstrates the documented asymmetry: the channel-trigger gate is deliberately wider than the `trusted` frontmatter field. To observe "untrusted → no trigger fires", you would need a sender whose DKIM fails, which is rare from real providers. If you want that stricter behavior, that is a future tightening (tracked as a v1.x follow-up). Today, this output is **expected**, not a regression.

---

## T10 — Outbound mail does NOT fire triggers

```bash
# [VPS]
rm -f /tmp/aimx-trigger*.flag /tmp/aimx-trigger.log

# [VPS, as non-root]
aimx send --from test@mail.example.com --to chua@uzyn.com \
  --subject "outbound-should-not-trigger" --body "test"

ls /tmp/aimx-trigger-*.flag 2>/dev/null || echo "no flag — PASS"
```

**Acceptance.** No flag file. Triggers fire only on **inbound** (port 25), never on `aimx send`.

---

## T11 — Trust model: success, failure, and definition

This test formalizes what "trusted" means. Reference: `src/trust.rs` (`evaluate_trust`).

**Definition (record this in the test log).**
- `trusted = "none"` when the mailbox has `trust = "none"` (no evaluation performed).
- `trusted = "true"` when `trust = "verified"` AND sender matches a `trusted_senders` glob AND DKIM verifies (`dkim = "pass"`).
- `trusted = "false"` when `trust = "verified"` AND either the sender is not on the allowlist OR DKIM did not pass.

**Success case (already covered by T8).** `/var/lib/aimx/inbox/test/*trigger-trusted*.md` → `trusted = "true"`.

**Failure case — sender off allowlist.** Covered by T9 → `trusted = "false"`.

**Failure case — trust=verified with DKIM pass but wrong sender.** Also covered by T9.

**Policy=none baseline.** Use the `inbox` mailbox from T2 (which has default `trust` behavior — confirm it's `none` by checking `/etc/aimx/config.toml`). Re-read any message delivered in T3:

```bash
sudo grep '^trusted' /var/lib/aimx/inbox/inbox/*.md
# expect: trusted = "none"
```

**Acceptance.** All three states (`none`, `"true"`, `"false"`) observed across T8/T9/T11 frontmatter.

---

## T12 — Reboot survival

```bash
# [VPS]
sudo reboot
```

Wait until SSH reconnects (typically ~30-90s depending on provider), then:

```bash
# [VPS]
systemctl status aimx --no-pager   # active (running)
ss -tlnp sport :25                 # bound
ls /run/aimx/send.sock             # present
aimx doctor
```

**Handoff.** Send one more email from Gmail to `inbox@mail.example.com` with subject `post-reboot`.

```bash
ls /var/lib/aimx/inbox/inbox/ | grep post-reboot
```

**Acceptance.**
- Service auto-started on boot (no manual intervention).
- `/run/aimx/send.sock` was re-created (provided by `RuntimeDirectory=aimx`).
- A post-reboot inbound email is successfully ingested.
- A post-reboot `aimx send` from a non-root shell also succeeds.

---

## T13 — Re-run setup (re-entrance)

```bash
# [VPS]
sudo aimx setup mail.example.com
```

**Acceptance.**
- Setup detects existing config/DKIM and does NOT overwrite keys without consent.
- `/var/lib/aimx/README.md` was refreshed if the binary is newer.
- `aimx serve` continues running with no interruption (or restarts cleanly).

---

## Teardown (optional)

```bash
# systemd hosts:
sudo systemctl disable --now aimx
sudo rm -f /etc/systemd/system/aimx.service
sudo systemctl daemon-reload

# OpenRC hosts (substitute for the systemd block above):
#   sudo rc-service aimx stop
#   sudo rc-update del aimx
#   sudo rm -f /etc/init.d/aimx

# Shared cleanup (both init systems):
sudo rm -rf /etc/aimx /var/lib/aimx /run/aimx /etc/ssl/aimx
rm -rf ~/.claude/plugins/aimx ~/.codex/skills/aimx

# Codex MCP registration lives in ~/.codex/config.toml — remove it with:
#   codex mcp remove aimx
# (or hand-edit ~/.codex/config.toml to drop the [mcp_servers.aimx] table).
```

---

## Summary checklist

- [ ] T1 — Setup + service start + DNS live
- [ ] T2 — Mailboxes created
- [ ] T3 — Inbound from Gmail lands with DKIM/SPF/DMARC=pass
- [ ] T4 — Outbound to `chua@uzyn.com` delivered, DKIM-signed, non-spam
- [ ] T5 — Routing: `agent@`, `test@`, unknown local-part behavior observed
- [ ] T6 — Claude Code: list + send + read/mark via MCP
- [ ] T7 — Codex CLI: list + send + read/mark via MCP
- [ ] T8 — Trigger fires on trusted sender
- [ ] T9 — Off-allowlist sender: trigger fires via DKIM-pass, `trusted = "false"`
- [ ] T10 — Trigger does NOT fire on outbound
- [ ] T11 — Trust model: `none`, `"true"`, `"false"` all observed
- [ ] T12 — Reboot survival
- [ ] T13 — Setup re-entrance

## Files referenced during tests

- `/etc/aimx/config.toml` — domain, mailboxes, triggers
- `/etc/aimx/dkim/{private,public}.key` — DKIM keys
- `/etc/ssl/aimx/{cert,key}.pem` — TLS cert
- `/etc/systemd/system/aimx.service` — service unit
- `/var/lib/aimx/inbox/<mailbox>/` — received mail
- `/var/lib/aimx/sent/<mailbox>/` — sent copies
- `/var/lib/aimx/README.md` — datadir layout guide
- `/run/aimx/send.sock` — UDS for `aimx send`
- `~/.claude/plugins/aimx/` — Claude Code plugin (manifest at `.claude-plugin/plugin.json`, skill at `skills/aimx/SKILL.md`)
- `~/.codex/skills/aimx/` — Codex skill (Codex CLI does NOT auto-discover `~/.codex/plugins/`; MCP wiring is in `~/.codex/config.toml` via `codex mcp add`)
- `sudo journalctl -u aimx -f` — daemon logs
