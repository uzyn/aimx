# aimx Manual Testing Plan

## Context

aimx is a self-hosted email server for AI agents. This plan manually verifies an end-to-end install on a fresh VPS: setup, inbound/outbound mail, agent integrations (Claude Code + Codex CLI), channel triggers, and the trust model. Every test below is copy-pasteable — run top-to-bottom on the VPS as root unless noted. Tests that need external help (sending from Gmail, receiving at `chua@uzyn.com`) call that out explicitly and include a prompt to hand off to a human collaborator.

**Test domain assumption.** Replace `mail.example.com` with your real test domain everywhere below. The plan uses `inbox`, `agent`, and `test` as mailbox names.

**Notation.** `[VPS]` = run on the aimx VPS. `[LOCAL]` = run on your laptop (where `claude` / `codex` are installed). `[GMAIL]` = user action in Gmail web UI.

---

## T0 — Pre-flight

1. Fresh Linux VPS with public IPv4, port 25 unblocked by the provider.
2. DNS zone editable for the test domain.
3. `chua@uzyn.com` mailbox available on your laptop (to observe outbound).
4. A Gmail account available (to observe inbound external deliverability).
5. `claude` and `codex` CLIs installed and logged in on your laptop.

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

**Interactive steps to answer.**
- Confirm domain when prompted.
- Accept default verify host (`https://check.aimx.email`).
- When asked to install the service, choose yes.
- Capture the printed DKIM TXT record (the `dkim._domainkey.mail.example.com` line).

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
sudo aimx status                 # summary output
sudo aimx verify                 # port 25 reachability probe
```

**Acceptance criteria.**
- `systemctl status aimx` → `active (running)`.
- `ss` shows aimx on `:25`.
- `/run/aimx/send.sock` exists with mode `srw-rw-rw-`.
- `aimx verify` reports port 25 reachable.
- `/etc/aimx/config.toml` exists and contains your domain.
- `/etc/aimx/dkim/private.key` is `0600 root:root`.
- `/var/lib/aimx/README.md` was generated.
- DKIM DNS lookup returns the published key: `dig +short TXT dkim._domainkey.mail.example.com`.

---

## T2 — Create mailboxes

```bash
# [VPS]
sudo aimx mailbox list
sudo aimx mailbox create inbox
sudo aimx mailbox create agent
sudo aimx mailbox create test
sudo aimx mailbox list
ls /var/lib/aimx/inbox/
```

**Acceptance.** Three mailbox directories exist under `/var/lib/aimx/inbox/` and `/var/lib/aimx/sent/`.

---

## T3 — Inbound email (external send from Gmail)

**Goal.** Real inbound mail over SMTP lands as a Markdown file with correct frontmatter and passes DKIM/SPF/DMARC.

**Handoff to human.**

> Please send a test email from your Gmail account:
> - **To:** `inbox@mail.example.com`
> - **Subject:** `aimx inbound test 1`
> - **Body:** anything; include 1 PDF or image attachment to exercise the bundle path.

```bash
# [VPS] watch the inbox directory while the email is in flight
watch -n 1 'ls -la /var/lib/aimx/inbox/inbox/'

# After it arrives:
ls /var/lib/aimx/inbox/inbox/
# Read the newest file (flat .md or bundle dir)
sudo find /var/lib/aimx/inbox/inbox/ -name '*.md' -newer /tmp -print
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
```

---

## T5 — Inbound routing into specific mailboxes

**Goal.** Different local parts route to their own mailboxes; catch-all or rejection behavior is predictable.

**Handoff to human.** Send three emails from Gmail in sequence:
1. To `agent@mail.example.com`, subject `route-test-agent`.
2. To `test@mail.example.com`, subject `route-test-test`.
3. To `nobody@mail.example.com`, subject `route-test-nobody` (unknown mailbox).

```bash
# [VPS]
ls /var/lib/aimx/inbox/agent/
ls /var/lib/aimx/inbox/test/
sudo journalctl -u aimx -n 200 --no-pager | grep -i 'nobody\|reject\|550'
```

**Acceptance.**
- `agent/` and `test/` each contain the respective message.
- The `nobody@` message is either (a) rejected at SMTP (check Gmail for a bounce), or (b) routed to a configured catch-all. Document which behavior you observe so the expectation is explicit.

---

## T6 — Claude Code agent integration

**Goal.** The Claude Code skill is installed, the MCP server starts, and Claude can list and send email.

```bash
# [LOCAL]
aimx agent-setup claude-code
ls -la ~/.claude/plugins/aimx/
# expect: plugin.json + skills/ + references/
```

Restart Claude Code (exit and relaunch), then test:

```bash
# [LOCAL] list inbox via MCP
claude -p "Use the aimx MCP tools. Call email_list with mailbox='inbox' and unread=true. Summarize what's there."
```

**Acceptance — list.** Claude responds with a summary matching the emails delivered in T3/T5.

```bash
# [LOCAL] send via MCP
claude -p "Use the aimx MCP tool email_send to send from inbox@mail.example.com to chua@uzyn.com. Subject: 'aimx claude MCP test'. Body: 'Sent via Claude MCP.'"
```

**Handoff.** Confirm receipt at `chua@uzyn.com`.

**Acceptance — send.**
- Claude reports a successful send with a message ID.
- `/var/lib/aimx/sent/inbox/` gains a new `.md` with `outbound = true`.
- Recipient receives the mail with DKIM=pass.

```bash
# [LOCAL] read + mark_read
claude -p "List unread mail in 'inbox'. Read the most recent one, summarize in one sentence, then mark it read."
```

**Acceptance — read/mark.** The read-flag updates in the mailbox file: `grep '^read' /var/lib/aimx/inbox/inbox/<file>.md` shows `read = true`.

---

## T7 — Codex CLI agent integration

```bash
# [LOCAL]
aimx agent-setup codex
ls -la ~/.codex/plugins/aimx/
```

Follow any printed `codex mcp add ...` step verbatim, then restart Codex.

```bash
# [LOCAL]
codex exec "Use the aimx MCP tools. Call email_list with mailbox='inbox' and unread=true. Summarize."

codex exec "Use the aimx MCP tool email_send to send from inbox@mail.example.com to chua@uzyn.com. Subject: 'aimx codex MCP test'. Body: 'Sent via Codex MCP.'"

codex exec "List unread mail in mailbox 'inbox'. Read the most recent one, summarize it, then mark it read."
```

**Acceptance.** Same as T6: list returns real emails, send produces a delivered `.md` in `sent/inbox/`, recipient receives it DKIM-signed, mark-read flips the `read` field.

---

## T8 — Channel trigger fires on TRUSTED sender

**Goal.** A configured `on_receive` shell command runs when a trusted sender emails, and receives the expanded template variables.

Edit `/etc/aimx/config.toml` to add:

```toml
[mailboxes.test]
address = "test@mail.example.com"
trust = "verified"
trusted_senders = ["chua@uzyn.com"]

[[mailboxes.test.on_receive]]
type = "cmd"
command = '''
touch /tmp/aimx-trigger-{id}.flag
printf 'from=%s\nsubject=%s\nfilepath=%s\n' '{from}' '{subject}' '{filepath}' >> /tmp/aimx-trigger.log
'''
```

```bash
# [VPS]
sudo systemctl restart aimx
rm -f /tmp/aimx-trigger*.flag /tmp/aimx-trigger.log
```

**Handoff.** Send an email from `chua@uzyn.com` (from your laptop — trusted sender) to `test@mail.example.com` with subject `trigger-trusted`.

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

## T9 — Channel trigger does NOT fire on UNTRUSTED sender

Keep the T8 config. Clear state:

```bash
# [VPS]
rm -f /tmp/aimx-trigger*.flag /tmp/aimx-trigger.log
```

**Handoff.** Send from a Gmail address NOT in `trusted_senders` (e.g. the Gmail used in T3) to `test@mail.example.com` with subject `trigger-untrusted`.

```bash
# [VPS]
ls /tmp/aimx-trigger-*.flag 2>/dev/null || echo "no flag — PASS"
sudo ls /var/lib/aimx/inbox/test/ | grep trigger-untrusted
sudo grep '^trusted' /var/lib/aimx/inbox/test/*trigger-untrusted*.md
```

**Acceptance.**
- No flag file was created.
- The email IS still stored in the mailbox (delivery is independent of trigger).
- Frontmatter `trusted = "false"` (mailbox is `verified` but sender not on allowlist, even if DKIM passed).

> **Subtlety to note.** The channel-trigger gate is looser than the `trusted` frontmatter field: a trigger fires when the sender is on `trusted_senders` OR DKIM passes. Because Gmail's DKIM passes, a stricter test of "untrusted means no trigger" requires a sender whose DKIM fails (rare from real providers). If the trigger DOES fire despite the sender being off-allowlist, that is expected behavior under current v1 semantics — document what you observe and decide whether to tighten the gate in a follow-up.

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

Wait 60s, reconnect, then:

```bash
# [VPS]
systemctl status aimx --no-pager   # active (running)
ss -tlnp sport :25                 # bound
ls /run/aimx/send.sock             # present
aimx status
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
sudo systemctl disable --now aimx
sudo rm -rf /etc/aimx /var/lib/aimx /run/aimx /etc/ssl/aimx
sudo rm -f /etc/systemd/system/aimx.service
sudo systemctl daemon-reload
rm -rf ~/.claude/plugins/aimx ~/.codex/plugins/aimx
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
- [ ] T9 — Trigger does NOT fire on untrusted sender
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
- `~/.claude/plugins/aimx/` — Claude Code plugin
- `~/.codex/plugins/aimx/` — Codex plugin
- `sudo journalctl -u aimx -f` — daemon logs
