# aimx Manual Testing — Results

Execution log for [`docs/manual-test.md`](./manual-test.md). Each test records PASS / FAIL / DEVIATION / BLOCKED per acceptance bullet with evidence.

## Environment

- **Test domain:** `agent.zeroshot.lol`
- **VPS:** `vps-198f7320-vps-ovh-net`, public IPv4 `92.222.243.76`
- **OS:** Linux 6.8.0-107-generic
- **Tester runtime:** tests run as user `ubuntu` with `sudo` rights.
- **aimx binary under test:** `0.1.0 (5f92ee32)` — repo HEAD as of commit `e2f0fa8` (`docs: add manual testing plan`).
- **Execution date:** 2026-04-17
- **Collaboration:** human-in-the-loop via chat for Gmail send / `chua@uzyn.com` receipt confirmations.

## Global deviations from plan

- **Not a fresh VPS.** aimx was already installed and running when this session began, with live config, DKIM keys, DNS records, and a pre-existing `catchall` mailbox (`*@agent.zeroshot.lol`). T1 therefore runs as "verify the existing install meets the plan's acceptance criteria" rather than a clean-room setup. A fresh setup is additionally exercised via T13 (re-entry). Recorded because this changes what T1 proves (idempotent state vs. first-boot).
- **Running as user `ubuntu`, not root.** `sudo` is used where the plan requires privilege. Service unit `User=root`.
- **`[LOCAL]` tests (T6/T7) run on the VPS.** `claude` and `codex` are installed on the VPS itself (not a separate laptop). Recorded as a deviation; does not affect MCP validity.
- **Binary was upgraded before testing.** Previously installed binary was `d8ab17da`; fresh build from HEAD (`5f92ee32`) was copied to `/home/ubuntu/.cargo/bin/aimx` and service was restarted before T0.

---

## T0 — Pre-flight

**Status:** PASS (with deviations).

| Check | Result | Evidence |
|---|---|---|
| Fresh Linux VPS with public IPv4 | PASS | IPv4 `92.222.243.76`, IPv6 `2001:41d0:20a:900::779`. |
| Port 25 unblocked | PASS | `sudo aimx verify` reports **outbound** and **inbound** port 25 reachable. |
| `port 25 not already bound` | DEVIATION | Port 25 **is** bound — by `aimx` (pid 1412283). Expected on a fresh VPS, but this VPS already has the daemon running (global deviation noted above). Real check here is "nothing *other than aimx* is on 25", which holds. |
| DNS editable for test domain | PASS | DNS records for `agent.zeroshot.lol` are already published (A, MX, SPF, DKIM). |
| `chua@uzyn.com` mailbox available | PENDING | Confirmed via user instructions; tested in T4/T6/T7. |
| Gmail account available | PENDING | User will operate Gmail for T3/T5/T8/T9/T12. |
| `claude` + `codex` installed | PASS (deviation) | Installed on the VPS itself: `claude 2.1.112`, `codex-cli 0.121.0`. Plan expects these on a separate `[LOCAL]` laptop. |

**DNS snapshot (verified live):**
```
A     agent.zeroshot.lol              → 92.222.243.76
MX    agent.zeroshot.lol              → 10 agent.zeroshot.lol.
TXT   agent.zeroshot.lol              → "v=spf1 ip4:92.222.243.76 -all"
TXT   dkim._domainkey.agent.zeroshot.lol → v=DKIM1; k=rsa; p=MIIBIjAN...DAQAB (RSA public key present)
```

Note: the plan's suggested SPF (`v=spf1 a mx ~all`) differs from what's published (`ip4:92.222.243.76 -all`). The published form is stricter and valid for a single-IP deployment. Not a failure.

---

## T1 — Fresh setup & service start

**Status:** PASS (as re-entrance / state-verification; see global deviation).

Because aimx was already installed before this session, `sudo aimx setup agent.zeroshot.lol` was NOT re-run in T1 — doing so before T13's dedicated re-entrance test would have polluted the T13 signal. Instead, every acceptance criterion is verified against the current state, which is the post-setup state.

| Acceptance | Result | Evidence |
|---|---|---|
| `systemctl status aimx` → `active (running)` | PASS | `Active: active (running) since Fri 2026-04-17 09:12:50 UTC`, Main PID 1412283, running binary `5f92ee32`. |
| `ss` shows aimx on `:25` | PASS | `LISTEN 0.0.0.0:25 users:(("aimx",pid=1412283,fd=9))`. |
| `/run/aimx/send.sock` exists with mode `srw-rw-rw-` | PASS | `srw-rw-rw- 1 root root 0 Apr 17 09:12 /run/aimx/send.sock`. |
| `aimx verify` reports port 25 reachable | PASS | `Outbound port 25... PASS`, `Inbound port 25... PASS`. |
| `/etc/aimx/config.toml` exists and contains domain | PASS | `domain = "agent.zeroshot.lol"` present; mode `0640 root:root`. |
| `/etc/aimx/dkim/private.key` is `0600 root:root` | PASS | `-rw------- 1 root root 1675 Apr 17 00:04 private.key`. |
| `/var/lib/aimx/README.md` was generated | PASS | 7206 bytes, refreshed `Apr 17 02:51` (matches binary mtime). |
| `dig +short TXT dkim._domainkey.…` returns the key | PASS | `v=DKIM1; k=rsa; p=MIIBIjAN…DAQAB` returned live. |

**Additional observations:**
- TLS cert + key present at `/etc/ssl/aimx/{cert,key}.pem` (mode `0644` / `0600`); STARTTLS is enabled per daemon startup log (`tls: enabled`).
- `aimx status` reports 1 mailbox (`catchall`) with 2 pre-existing messages — leftover from prior work; not related to this test run.
- systemd service uses `User=root`, `RuntimeDirectory=aimx`, `ExecStart=/home/ubuntu/.cargo/bin/aimx serve --data-dir /var/lib/aimx`.

---

## T2 — Create mailboxes

**Status:** PASS.

```
$ sudo aimx mailbox create inbox   → Mailbox 'inbox' created.
$ sudo aimx mailbox create agent   → Mailbox 'agent' created.
$ sudo aimx mailbox create test    → Mailbox 'test' created.
$ sudo aimx mailbox list
  MAILBOX              INBOX    SENT
  agent                0        0
  catchall             2        0   (pre-existing)
  inbox                0        0
  test                 0        0
```

| Acceptance | Result | Evidence |
|---|---|---|
| `/var/lib/aimx/inbox/<name>/` exists for all three | PASS | `ls /var/lib/aimx/inbox/` → `agent catchall inbox test`. |
| `/var/lib/aimx/sent/<name>/` exists for all three | PASS | `ls /var/lib/aimx/sent/` → `agent inbox test`. |

**Observations:**
- `aimx mailbox create` also writes `[mailboxes.<name>]` stanzas into `/etc/aimx/config.toml` with defaults `trust = "none"`, empty `on_receive`, empty `trusted_senders`.
- The pre-existing `catchall` mailbox has no matching `/var/lib/aimx/sent/catchall/` — that directory was never auto-created for it, presumably because `catchall` was created by an earlier aimx version. Minor bookkeeping inconsistency; does not fail any acceptance bullet for T2 but worth noting for future cleanup.

---

## T3 — Inbound email (external send from Gmail)

### Attempt 1 — daemon-stale-config regression discovered

**First send** (Gmail → `inbox@agent.zeroshot.lol`, subj `aimx inbound test 1`, with a PDF attachment): the message was accepted over SMTP but routed into `inbox/catchall/` rather than `inbox/inbox/`, because the running daemon still held the Config it loaded at 09:12:50, i.e. BEFORE T2 created `[mailboxes.inbox]` at 09:14.

**Root cause (code read).** `src/config.rs:170 resolve_mailbox` looks up `local_part` in `self.mailboxes` and falls back to `"catchall"` on miss. In `src/smtp/session.rs:500`, `ingest_email` is called with a `Config` that `SmtpServer::new` cloned once at startup (`src/serve.rs:139`). There is no SIGHUP or inotify reload (`src/serve.rs:544` explicitly forbids `ExecReload`). Creating a new mailbox via `aimx mailbox create` updates `/etc/aimx/config.toml` on disk but the running daemon does not observe it.

**Operational impact (finding to surface):**
- **`aimx mailbox create` silently produces a state where inbound mail is misrouted until the operator restarts the daemon.** The command prints `Mailbox 'xxx' created.` with no hint that a restart is needed. Suggested follow-up: either (a) have the CLI print a "remember to `sudo systemctl restart aimx`" line, or (b) signal the daemon to reload, or (c) watch the config file. This is a minor DX bug worth filing.

Mitigation applied for this test run: `sudo systemctl restart aimx` (09:19 UTC). Retrying with a fresh Gmail batch below.

### Attempt 2 — post-restart (authoritative)

**Status:** PASS (with two findings to log).

All 4 Gmail sends were accepted by aimx. The `inbox@` message landed in `/var/lib/aimx/inbox/inbox/` as a bundle dir with the PDF attachment.

**Inbox message inspected:** `/var/lib/aimx/inbox/inbox/2026-04-17-092111-fwd-aimx-inbound-tes/2026-04-17-092111-fwd-aimx-inbound-tes.md`

Frontmatter extract:
```toml
id = "2026-04-17-092111-fwd-aimx-inbound-tes"
message_id = "CACCt8EVg-yDjNVzW+w+EdtMbjDfx43JabytoVNTskt=Syuz_XQ@mail.gmail.com"
thread_id = "eb482956f2feb4f5"
from = "U-Zyn Chua <chua@uzyn.com>"
to = "inbox@agent.zeroshot.lol"
delivered_to = "inbox@agent.zeroshot.lol"
subject = "Fwd: aimx inbound test 1"
received_at = "2026-04-17T09:21:11.658746505+00:00"
received_from_ip = "209.85.219.44"
size_bytes = 444082
dkim = "pass"
spf = "none"     # see finding below
dmarc = "pass"
trusted = "none"
mailbox = "inbox"
read = false

[[attachments]]
filename = "BYD-H (Maintain Buy, Price_Target_ HKD 106.50_130.00)-en.pdf"
content_type = "application/pdf"
size = 318997
path = "BYD-H (…)-en.pdf"
```

Bundle dir contents: `<stem>.md` + `<pdf>` (318997 bytes). The PDF is a sibling of the `.md`, matching the Zola-style bundle layout.

| Acceptance | Result | Evidence |
|---|---|---|
| File named `YYYY-MM-DD-HHMMSS-aimx-inbound-test-1.{md,dir}` | PASS with naming note | Stem is `2026-04-17-092111-fwd-aimx-inbound-tes`. The `fwd-` prefix comes from the user forwarding the earlier message (gmail prepended `Fwd: `); slug got truncated at the 30-char cap from `src/slug.rs`. Not a failure — matches the actual subject `Fwd: aimx inbound test 1`. |
| TOML frontmatter with `from`, `to`, `subject` | PASS | Exact strings match; `to = "inbox@agent.zeroshot.lol"`. |
| `message_id`, `thread_id`, `received_at`, `size_bytes` | PASS | `thread_id = "eb482956f2feb4f5"` (16 hex chars). |
| `dkim = "pass"`, `spf = "pass"`, `dmarc = "pass"` | **PARTIAL (finding #2)** | DKIM pass ✓, DMARC pass ✓, **SPF = "none"** ✗. See "SPF finding" below. |
| `mailbox = "inbox"`, `read = false` | PASS | Both present. |
| Bundle contains attachment + `[[attachments]]` with `path` | PASS | PDF present; `path = "BYD-H (…)-en.pdf"` matches the file sibling. |
| Body below `+++` is plain text | PASS | Visible body is quoted-forward text; HTML was converted. |

**Finding #2 — SPF reported as "none" for a signed, SPF-covered domain.**
`From:` is `chua@uzyn.com`; `uzyn.com` publishes `v=spf1 a mx ip4:175.41.131.3 include:_spf.google.com ~all`. `_spf.google.com` covers `209.85.128.0/17`, which includes the observed Received-IP `209.85.219.44`. Manual DNS lookups from this VPS return both records correctly. aimx's `build_spf_output` (`src/ingest.rs:318`) uses `mail_auth::MessageAuthenticator::verify_spf` with the `From:`-domain as both helo-domain and mail-from-domain. Expected result: `Pass`. Observed: `None`. This is a potential bug — aimx's SPF evaluator may not be resolving nested `include:` records, or the resolver is mis-configured. All three of the other inbound test emails in this batch showed `spf = "none"` as well, so it's reproducible across messages. Worth filing a separate issue. (DMARC still says `pass` because DMARC aligns on DKIM or SPF — DKIM alone was sufficient.)

**Finding #3 — user forwarded/replied to the originals rather than composing new mail.**
Subjects show `Fwd: …` / `Re: …`, and the frontmatter includes `in_reply_to` / `references` headers chained back to the first (mis-routed) attempt. Not a test failure — it just means the 3 trust/route tests include thread-continuation metadata. Plan has no expectation about compose-new vs. forward, so this is fine.

**Gmail deliverability check.** User did not report a bounce; messages arrived within seconds. No errors in `journalctl -u aimx` during the window.

---

## T5 — Inbound routing

**Status:** PASS.

All three routing messages from the same (post-restart) Gmail batch:

| Destination | File | `mailbox` frontmatter | Result |
|---|---|---|---|
| `agent@agent.zeroshot.lol` | `inbox/agent/2026-04-17-092126-fwd-route-test-agent.md` | `"agent"` | PASS |
| `test@agent.zeroshot.lol` | `inbox/test/2026-04-17-092135-re-route-test-test.md` | `"test"` | PASS |
| `nobody@agent.zeroshot.lol` | `inbox/catchall/2026-04-17-092140-re-route-test-nobody.md` | `"catchall"` | PASS (catch-all) |

**Observed behavior for unknown local-parts.** Routed to `catchall` (no SMTP reject, no bounce). `delivered_to = "nobody@agent.zeroshot.lol"` is preserved in the frontmatter; only the mailbox-selection fell back. This is the "catch-all" branch of the two options offered in the plan (reject-at-SMTP vs. catch-all). The behavior is driven by `Config::resolve_mailbox` (`src/config.rs:170`) which returns `"catchall"` on miss. Current deployment ships with a `catchall` mailbox so all unknown addresses are archived.

| Acceptance | Result | Evidence |
|---|---|---|
| `agent/` contains the message | PASS | `inbox/agent/2026-04-17-092126-fwd-route-test-agent.md`, `mailbox = "agent"`. |
| `test/` contains the message | PASS | `inbox/test/2026-04-17-092135-re-route-test-test.md`, `mailbox = "test"`. |
| `nobody@` → reject OR catch-all, documented | PASS | **Catch-all** (no reject). File lives at `inbox/catchall/2026-04-17-092140-re-route-test-nobody.md` with `to = "nobody@agent.zeroshot.lol"` preserved. |

---

## T4 — Outbound email

**Status:** PASS (with two findings).

### Main send

```
$ aimx send --from inbox@agent.zeroshot.lol --to chua@uzyn.com \
    --subject "aimx outbound test 1" --body "This message was sent by aimx send over MX."

Email sent.
Message-ID: <8d1f257d-b07d-4869-90f9-4fff0db01268@agent.zeroshot.lol>
<8d1f257d-b07d-4869-90f9-4fff0db01268@agent.zeroshot.lol>

exit=0
```

Deviation from plan's expected stdout: plan shows `AIMX/1 OK <message-id>`; actual output is `Email sent.\nMessage-ID: <…>\n<…>`. The daemon-side protocol does return `AIMX/1 OK` on the wire (see `src/send_protocol.rs`), but `aimx send` formats a friendlier human-readable stdout before exit. Exit code matches plan (0).

Sent-copy file: `/var/lib/aimx/sent/inbox/2026-04-17-092509-aimx-outbound-test-1.md`
```toml
outbound = true
delivery_status = "delivered"
delivered_at = "2026-04-17T09:25:09.466819957+00:00"
```
Body includes the DKIM-Signature header (`s=dkim; d=agent.zeroshot.lol`) followed by the raw RFC-5322 message. Gmail-side pass/pass/pass verification pending user paste.

### Finding #4 — `aimx send` requires read access to `/etc/aimx/config.toml` (mode 0640)

Attempting `aimx send` as user `ubuntu` (non-root, not in `root` group) initially failed with:
```
Error: Permission denied (os error 13)
exit=1
```
`strace` showed the failure at `openat("/etc/aimx/config.toml") → EACCES`. The CLI refuses to run as root (`exit 2`), so legitimate non-root users hit this wall unless they're in the `root` group. Workaround applied for this test run: `sudo chmod 0644 /etc/aimx/config.toml`. Recommended follow-up: either (a) create an `aimx` group with read access, (b) make `send` ask the daemon for mailbox resolution rather than reading the config client-side, or (c) document the group-membership requirement in setup.

### Failure-branch tests

| Branch | Expected | Observed | Result |
|---|---|---|---|
| Root refusal | exit 2 | `Error: send is a per-user operation — run without sudo` + exit 2 | PASS |
| Socket missing | exit 2 + guidance | `Error: aimx daemon not running — check 'systemctl status aimx'` + exit 2 | PASS |
| Unknown mailbox (bogus@) | `AIMX/1 ERR MAILBOX …` exit 1 | **exit 0, Email sent via catchall** | DEVIATION |

**Finding #5 — Unknown-mailbox failure branch is masked by the `catchall` wildcard.**
`resolve_from_mailbox` in `src/send.rs:217` matches `bogus@agent.zeroshot.lol` against `[mailboxes.catchall] address = "*@agent.zeroshot.lol"`, so the send succeeds and the sent copy lands in `sent/catchall/2026-04-17-092526-x.md`. To reproduce the plan's intended failure branch you must first delete the catchall (e.g., `sudo aimx mailbox delete catchall` if such exists, or edit `config.toml`). This is arguably a *surprising* behavior: `aimx send` will accept **any** From: address under the operator's domain as long as a wildcard mailbox exists, which means an outbound signing primitive is available to any local process that can reach the UDS. Worth reviewing whether `aimx send` should be stricter than wildcard-match for the outbound path.

### Recipient-side confirmation

User confirmed the message arrived in **Inbox** at `chua@uzyn.com` (not Spam) and pasted the full headers.

```
Authentication-Results: mx.google.com;
   dkim=fail header.i=@agent.zeroshot.lol header.s=dkim header.b=BvZOymZq;
   spf=pass (google.com: domain of inbox@agent.zeroshot.lol designates 92.222.243.76 as permitted sender) smtp.mailfrom=inbox@agent.zeroshot.lol;
   dmarc=pass (p=REJECT sp=REJECT dis=NONE) header.from=agent.zeroshot.lol
Received-SPF: pass (google.com: domain of inbox@agent.zeroshot.lol designates 92.222.243.76 as permitted sender) client-ip=92.222.243.76;
```

| Acceptance | Result |
|---|---|
| Exit 0, message ID printed | PASS |
| `/var/lib/aimx/sent/inbox/` gains `outbound = true` md | PASS |
| DKIM=pass at recipient | **FAIL (finding #6)** — Gmail says `dkim=fail header.b=BvZOymZq`. |
| SPF=pass at recipient | PASS |
| DMARC=pass at recipient | PASS |
| Arrives in Inbox, not Spam | PASS (user-confirmed) |

### Finding #6 — aimx DKIM signatures fail verification at Gmail

The outbound message is DKIM-signed (aimx-side sent-copy frontmatter shows `dkim = "pass"`, which is a self-validation done post-signing), yet Gmail's `Authentication-Results` reports `dkim=fail header.i=@agent.zeroshot.lol header.s=dkim header.b=BvZOymZq`. DMARC still aligns on SPF, so the message is not quarantined — but anyone relying on DKIM-only policies would reject it, and deliverability to strict destinations may suffer over time.

Signed header set in the DKIM-Signature: `Message-ID:Date:Subject:To:From:In-Reply-To:References`. The actual message has no `In-Reply-To` or `References` headers. Per RFC 6376 §3.5, listing a non-existent header in `h=` is legal and the verifier must treat the missing header as empty-value. Possible root causes to investigate (not debugged as part of this test run):
1. Body-hash canonicalization — `c=relaxed/relaxed` with a trailing newline the verifier doesn't see or a CRLF/LF mismatch.
2. Header-value canonicalization — extra whitespace or line-folding on signed headers.
3. Key mismatch — the `s=dkim; d=agent.zeroshot.lol` record returned 2048-bit RSA key matches the private key in `/etc/aimx/dkim/private.key` (sanity check: public/private were generated together by `aimx setup`).
4. Signing the empty `In-Reply-To:References` headers may be triggering a verifier-side quirk; try removing them from the signed-headers list.

This is the highest-priority finding in this test run — outbound DKIM must work for the product's core promise. **Recommend opening a P0 issue.**

---

## T6 — Claude Code agent integration

**Status:** PARTIAL PASS (list and send work; mark-read fails due to permissions; plugin auto-discovery did not expose tools to `claude -p`).

### Plugin install

```
$ aimx agent-setup claude-code --force
Installed /home/ubuntu/.claude/plugins/aimx
Plugin installed. Restart Claude Code to pick it up (it is auto-discovered from ~/.claude/plugins/).
```

Plugin layout on disk:
```
~/.claude/plugins/aimx/
├── .claude-plugin/plugin.json          # mcpServers.aimx = /usr/local/bin/aimx mcp
└── skills/aimx/
    ├── SKILL.md
    └── references/{frontmatter,mcp-tools,troubleshooting,workflows}.md
```

**Deviation from plan's expected `plugin.json + skills/ + references/`:** the `plugin.json` lives inside `.claude-plugin/` (the current Claude-Code plugin layout), and `references/` sits inside `skills/aimx/`, not at the top level. Content-wise everything is present; the layout just differs from the plan's one-liner.

### Finding #7 — `claude -p` does not auto-load the aimx plugin's MCP server

Even after `aimx agent-setup claude-code` (which writes `~/.claude/plugins/aimx/.claude-plugin/plugin.json` with a `mcpServers.aimx` stanza), `claude -p` reports:

> There's no aimx MCP tool available in this environment.

The plugin is on disk but not listed in `~/.claude/plugins/installed_plugins.json`, which is how Claude Code tracks *activated* plugins. Local directories under `~/.claude/plugins/<name>/` are not auto-activated in non-interactive `claude -p` invocations.

**Workaround to exercise the MCP path:** `claude mcp add --scope user aimx /usr/local/bin/aimx mcp`. After that, `claude mcp list` reports `aimx: /usr/local/bin/aimx mcp - ✓ Connected`, and the tools become callable in `claude -p` (still requires `--dangerously-skip-permissions` because `claude -p` has no way to prompt for tool approval).

**Recommended fix / docs change:** the `aimx agent-setup claude-code` command should either (a) also register the MCP server via `claude mcp add`, or (b) print a follow-up line such as `Run: claude mcp add --scope user aimx /usr/local/bin/aimx mcp` so the plugin actually becomes live. Right now users who install only the plugin get silent non-discovery.

### MCP tool exercise (post-workaround)

| Tool | Invocation | Result |
|---|---|---|
| `email_list` | `mailbox='inbox'`, `unread=true` | PASS — Claude returned the single unread mail (Fwd: aimx inbound test 1, from chua@uzyn.com), matching T3's stored file. |
| `email_send` | `from=inbox@agent.zeroshot.lol`, `to=chua@uzyn.com`, subj=`aimx claude MCP test` | PASS — `/var/lib/aimx/sent/inbox/2026-04-17-092959-aimx-claude-mcp-test.md` written with `outbound = true`, `delivery_status = "delivered"`. Message-ID `<0353e369-…@agent.zeroshot.lol>`. |
| `email_read` | Newest mailbox=inbox message | PASS — returned body + attachment metadata. |
| `email_mark_read` | Same message | **FAIL (finding #8)** — `Permission denied (os error 13)`. |

### Finding #8 — `email_mark_read` (and any MCP write op) fails under a non-root user

The daemon (running as `User=root`) writes mailbox files with ownership `root:root 0644`. The aimx MCP server is launched by Claude Code, which runs as the invoking user (`ubuntu`). `ubuntu` has read access but not write, so any MCP tool that rewrites frontmatter (`email_mark_read`, delete, labels, etc.) fails with EACCES.

Options:
1. Run aimx MCP via `sudo` (brittle, requires tty-less sudoers config).
2. Make the MCP tool proxy its write operations through the daemon over the UDS (mirror `AIMX/1 SEND` → add `AIMX/1 MARK-READ`). Most architecturally consistent.
3. Lower daemon umask to `002` and create an `aimx` group containing the operator.

Deliverability-side receipt for `aimx claude MCP test`: user confirmed **arrived in Spam** (not Inbox). Contrast with T4 which arrived in Inbox — the same outbound path (DKIM still fails at Gmail per finding #6) is now tripping Gmail's spam filter on repeated messages. Expected: after a DKIM fix (finding #6), deliverability should stabilize.

| Acceptance | Result |
|---|---|
| `email_list` returns real emails | PASS |
| `email_send` produces a `.md` in `sent/inbox/` | PASS |
| Recipient receives the message DKIM-signed | PASS (signed) — but Gmail-side `dkim=fail` (finding #6) and this one landed in **Spam** |
| `email_mark_read` flips `read = true` in frontmatter | **FAIL (finding #8)** |

---

## T7 — Codex CLI agent integration

**Status:** PARTIAL PASS (list + send work; mark-read fails — same finding #8 as T6).

### Plugin install

```
$ aimx agent-setup codex --force
Installed /home/ubuntu/.codex/skills/aimx
Skill installed at ~/.codex/skills/aimx/. Register the AIMX MCP server with Codex CLI by running this command once:

  codex mcp add aimx -- /usr/local/bin/aimx mcp

Restart Codex CLI after registration so the new server is loaded.
```

Disk layout:
```
~/.codex/skills/aimx/
├── SKILL.md
└── references/…
```

**Deviation from plan.** Plan says `ls -la ~/.codex/plugins/aimx/`, but codex uses `~/.codex/skills/aimx/` (no `plugins/` directory). The install output does print the exact follow-up command, and when run, it populates `~/.codex/config.toml` with `[mcp_servers.aimx]`. Nice: Codex-side instructions are clearer than Claude-Code's (which didn't prompt the user to run `claude mcp add`).

### MCP tool exercise

Codex CLI requires `--sandbox danger-full-access` and `--skip-git-repo-check` for headless MCP testing from `/tmp`.

| Tool | Invocation | Result |
|---|---|---|
| `email_list` | `mailbox='inbox'`, `unread=true` | PASS — returned the same Fwd message. |
| `email_send` | subj `aimx codex MCP test`, body `Sent via Codex MCP.` | PASS — `/var/lib/aimx/sent/inbox/2026-04-17-093343-aimx-codex-mcp-test.md`, Message-ID `<e375f09c-…@agent.zeroshot.lol>`. |
| `email_read` | Newest inbox message | PASS — returned body + attachment metadata. |
| `email_mark_read` | Same message | **FAIL** — `Failed to write email: Permission denied (os error 13)`. Codex automatically retried with an explicit `folder: "inbox"` param — same error. Reproduces finding #8. |

Recipient-side receipt for `aimx codex MCP test`: arrived in **Spam** (user-confirmed). Same deliverability pattern as T6.

| Acceptance | Result |
|---|---|
| List returns real emails | PASS |
| Send produces delivered `.md` in `sent/inbox/` | PASS |
| Recipient receives it DKIM-signed | PASS (signed) — but Gmail-side pattern remains `dkim=fail` (finding #6), message landed in **Spam** |
| mark-read flips `read` field | **FAIL** (finding #8 reproduced) |

---

## T8 — Channel trigger on TRUSTED sender

**Status:** PARTIAL PASS — trigger fires and frontmatter is correct; but the printf line in the trigger command fails due to a shell-injection bug.

### Config applied

Edited `/etc/aimx/config.toml` per plan (trust=verified, trusted_senders=[chua@uzyn.com], on_receive cmd block). Daemon restarted cleanly.

### Send from `chua@uzyn.com` → `test@agent.zeroshot.lol`, subj `trigger-trusted`

Resulting file: `/var/lib/aimx/inbox/test/2026-04-17-113544-trigger-trusted.md`.
Frontmatter: `dkim = "pass"`, `spf = "none"` (finding #2 again), `dmarc = "pass"`, `trusted = "true"`, `mailbox = "test"`. ✓

Trigger artifact: `/tmp/aimx-trigger-2026-04-17-113544-trigger-trusted.flag` (created, empty — `touch` ran).

Trigger log: `/tmp/aimx-trigger.log` **was never created**.

### Finding #9 — Template expansion in `on_receive cmd` triggers shell injection / breakage

The journal shows the literal command aimx constructed:
```
aimx: trigger failed (exit 2): touch /tmp/aimx-trigger-2026-04-17-113544-trigger-trusted.flag
printf 'from=%s\nsubject=%s\nfilepath=%s\n' ''U-Zyn Chua <chua@uzyn.com>'' 'trigger-trusted' '/var/lib/aimx/inbox/test/…md' >> /tmp/aimx-trigger.log
  sh: 2: cannot open chua@uzyn.com: No such file
```
aimx substitutes `{from}` by inlining the raw header value into the pre-existing quotes: `'{from}'` with `from = "U-Zyn Chua <chua@uzyn.com>"` becomes `''U-Zyn Chua <chua@uzyn.com>''` inside the script. `sh -c` then parses `<chua@uzyn.com>` as a redirect → opens file `chua@uzyn.com` → error, exit 2. `touch` on line 1 succeeded (no special characters in `{id}`), so the flag was still created.

**Severity.** High — this is a **shell injection vulnerability**. A sender who controls the `From:` header can craft one that embeds arbitrary shell metacharacters (backticks, `$()`, `;`, pipes, redirects) to execute arbitrary commands on the aimx host whenever a matching trigger fires. The test plan's exact reference recipe is affected out of the box, which also means it will break for essentially every real-world email that uses `Name <addr>` syntax.

**Fix.** Two options:
1. Instead of substituting into a shell-quoted string, pass template variables as **arguments** via `sh -c '<script>' -- "{from}" "{subject}" "{filepath}"` and have the script refer to `$1`, `$2`, `$3`. This escapes automatically.
2. Or, aimx could shell-escape each substituted value with single-quote wrapping and doubling-up of embedded single quotes.

Recommend option 1; simpler and impossible to get wrong.

**Docs impact.** `book/channel-recipes.md` and the test plan use the raw-substitution pattern — both need updating after the fix.

### Acceptance

| Acceptance | Result | Evidence |
|---|---|---|
| `/tmp/aimx-trigger-<id>.flag` created | PASS | `-rw-r--r-- 1 root root 0 Apr 17 11:35 …flag`. |
| `/tmp/aimx-trigger.log` has expected `from=/subject=/filepath=` lines | **FAIL** | Log file never created; the printf line broke on shell redirect. See finding #9. |
| Frontmatter has `trusted = "true"` and `dkim = "pass"` | PASS | Both present in `.md`. |

---

## T9 — Channel trigger does NOT fire on UNTRUSTED sender

**Status:** DEVIATION (documented-subtlety behavior).

### Send from `uzyn@zynesis.com` → `test@agent.zeroshot.lol`, subj `trigger-untrusted`

User sent from `uzyn@zynesis.com` (a distinct Gmail-Workspace address, NOT on `trusted_senders`). Result file: `/var/lib/aimx/inbox/test/2026-04-17-113624-trigger-untrusted.md`.

Frontmatter: `dkim = "pass"`, `spf = "none"`, `dmarc = "none"` (zynesis.com may lack DMARC), **`trusted = "false"`**, `mailbox = "test"`. ✓

**Trigger artifact: `/tmp/aimx-trigger-2026-04-17-113624-trigger-untrusted.flag` exists** — the trigger DID fire.

| Acceptance | Result | Evidence |
|---|---|---|
| No flag file was created | **DEVIATION** (expected per plan's subtlety) | Flag WAS created because `dkim = "pass"`. `src/channel.rs` fires when sender matches `trusted_senders` **OR** DKIM passes. |
| Email IS still stored in mailbox | PASS | file present. |
| `trusted = "false"` in frontmatter | PASS | Exact match. |

**Subtlety confirmed (quoted from plan).**
> The channel-trigger gate is looser than the `trusted` frontmatter field: a trigger fires when the sender is on `trusted_senders` OR DKIM passes. Because Gmail's DKIM passes, a stricter test of "untrusted means no trigger" requires a sender whose DKIM fails (rare from real providers). If the trigger DOES fire despite the sender being off-allowlist, that is expected behavior under current v1 semantics — document what you observe and decide whether to tighten the gate in a follow-up.

Recommendation: tighten the gate so a trigger only fires when the *frontmatter* `trusted = "true"`. The current looser semantics are surprising and risk unintended trigger invocations from any DKIM-valid sender.

---

## T10 — Outbound mail does NOT fire triggers

**Status:** PASS.

Cleared `/tmp/aimx-trigger*` and sent:
```
$ aimx send --from test@agent.zeroshot.lol --to chua@uzyn.com \
    --subject "outbound-should-not-trigger-2" --body "test 2"
Email sent.
Message-ID: <2331fe49-4804-42c1-9057-4c8645290daf@agent.zeroshot.lol>
```

Post-send check:
```
no flag — PASS
no log  — PASS
```

| Acceptance | Result |
|---|---|
| No flag file created for outbound | PASS |
| No log file created for outbound | PASS |

Triggers fire only on inbound (port 25), never on `aimx send`. Matches `src/channel.rs` (`fire_triggers` is only invoked from the inbound ingest path).

Sent copy persisted to `/var/lib/aimx/sent/test/2026-04-17-113747-outbound-should-not.md` (from the first attempt, before the clean retry) and a second copy at `2026-04-17-113…-outbound-should-not.md` from the retry. Both record `outbound = true`, `delivery_status = "delivered"`.

---

## T11 — Trust model: success, failure, definition

**Status:** PASS — all three states observed.

Reference: `src/trust.rs evaluate_trust`.

**Definition confirmed:**
- `trusted = "none"` when mailbox `trust = "none"` (no evaluation).
- `trusted = "true"` when `trust = "verified"` AND sender matches `trusted_senders` AND `dkim = "pass"`.
- `trusted = "false"` when `trust = "verified"` AND either sender is off-allowlist OR DKIM did not pass.

**Observations:**

| State | Evidence |
|---|---|
| `trusted = "none"` | `inbox/inbox/2026-04-17-092111-fwd-aimx-inbound-tes.md` (mailbox `inbox` has `trust = "none"`). |
| `trusted = "true"` | `inbox/test/2026-04-17-113544-trigger-trusted.md` (T8 — `chua@uzyn.com` on allowlist, DKIM pass). |
| `trusted = "false"` | `inbox/test/2026-04-17-113624-trigger-untrusted.md` (T9 — `uzyn@zynesis.com` off-allowlist, even though DKIM passed). |

All three states observed across T8/T9/T11 frontmatter. Definitions in `src/trust.rs` match the plan's contract.

---

## T12 — Reboot survival

**Status:** PASS.

Rebooted at 11:41:04 UTC; VPS back at 11:41:16 UTC.

| Acceptance | Result | Evidence |
|---|---|---|
| Service auto-started on boot (no manual intervention) | PASS | `systemctl status aimx` → `Active: active (running) since 11:41:16 UTC`, Main PID 727. |
| `/run/aimx/send.sock` re-created | PASS | `srw-rw-rw- 1 root root 0 Apr 17 11:41 /run/aimx/send.sock` (systemd `RuntimeDirectory=aimx`). |
| Post-reboot inbound email ingested | PASS | `/var/lib/aimx/inbox/inbox/2026-04-17-114542-post-reboot.md` — `from = "U-Zyn Chua <chua@uzyn.com>"`, `subject = "post-reboot"`, `dkim = "pass"`, `mailbox = "inbox"`. |
| Post-reboot `aimx send` from non-root shell succeeds | PASS | `Message-ID: <91ebd09e-bb11-423f-aea2-13e691b86192@agent.zeroshot.lol>`. |

All four reboot-survival acceptance bullets passed. Config, DKIM keys, TLS cert, systemd unit, and data directory all survived the reboot intact.

---

## T13 — Re-run setup (re-entrance)

**Status:** PASS — plus uncovered the root cause of finding #6.

### Acceptance

| Acceptance | Result | Evidence |
|---|---|---|
| Setup detects existing config/DKIM, does NOT overwrite keys | PASS | Before: `ac8c1d9e…` / `6a1680df…` (private / public sha256). After: identical. Stdout printed `Existing AIMX configuration detected. Skipping install, proceeding to verification.` and `DKIM keypair already exists.` |
| `/var/lib/aimx/README.md` refreshed when binary is newer | PASS (but unconditional) | README mtime moved from pre-setup `…02:51` (old binary) / `07:21` (reboot `refresh_if_outdated`) to `Apr 17 11:46` — setup rewrote it. Minor deviation: the rewrite appears to be unconditional rather than version-gated. No harm; keeps README fresh. |
| `aimx serve` continues running with no interruption | PASS | `systemctl is-active aimx` → `active` throughout. Same PID 727 from post-reboot. Setup does NOT restart the service on re-entry (correct). |

### Finding #10 (CRITICAL) — DKIM keypair on disk does NOT match DNS-published record; explains finding #6

Setup's DNS-verification step reported:
```
DKIM: FAIL - DKIM record found but public key does not match local key
       → Add: TXT dkim._domainkey.agent.zeroshot.lol  v=DKIM1; k=rsa; p=MIIBIjAN…AQEAyjQ9AW6uxv6S7DuPG…DAQAB
```

Comparison:

| Source | `p=` (first ~30 chars after `MIIBIjAN…AQEA`) |
|---|---|
| `/etc/aimx/dkim/public.key` on disk | `yjQ9AW6uxv6S7DuPGmklSSNL7+IqZdKcYfP0Hz…` |
| `dig +short TXT dkim._domainkey.agent.zeroshot.lol` | `011La5tkO7DUxlLEduWsIbrPcK0NAS9SpcW9rf…` |

**These are two entirely different keys.** Every outbound message aimx signs uses the on-disk private key; every DKIM verifier at the receiver fetches the DNS-published public key; the pair does not align, so every signature fails verification. This is the root cause of the Gmail-side `dkim=fail header.b=BvZOymZq` observed in T4/T6/T7 (finding #6).

**Most likely origin.** The DKIM DNS record was published once during the original setup, and then at some later point the keypair was regenerated (either by `aimx dkim-keygen`, a stray `aimx setup`, or a manual rotation) without the DNS TXT record being updated to match. The `/etc/aimx/dkim/private.key` mtime is `Apr 17 00:04` (from the pre-session state snapshot); the original DNS record dates from a prior install.

**Remediation.** One of:
1. Update the DKIM TXT record at the DNS provider to the current on-disk public key (use the value printed in setup's `[DNS]` section). After propagation, Gmail `dkim=pass` will resolve finding #6.
2. OR regenerate a fresh keypair (`sudo aimx dkim-keygen --force`?), publish that to DNS, and restart aimx.

Either path closes findings #6 and #10. Option 1 is the lowest-risk.

**Observation about setup's verification UX.** Setup entered an infinite loop of "Press Enter to verify DNS records, or q to finish and verify later." while DKIM remained FAIL. `q` exits cleanly. That behavior is fine, but the mismatched-DKIM case deserves a louder warning — something like "Your live DNS key does not match your signing key. All outbound DKIM signatures will FAIL verification until you update either the DNS record or the local keypair." Right now the failure line is easy to miss among a block of PASS lines.

---

## Summary checklist

| # | Test | Status |
|---|---|---|
| T0 | Pre-flight | PASS (with deviations) |
| T1 | Setup + service start + DNS live | PASS (as re-entrance) |
| T2 | Mailboxes created | PASS |
| T3 | Inbound from Gmail lands with DKIM/SPF/DMARC=pass | PASS (DKIM+DMARC pass, **SPF=none bug, finding #2**) |
| T4 | Outbound to `chua@uzyn.com` delivered, DKIM-signed, non-spam | **FAIL** at recipient — Gmail `dkim=fail` (finding #6, root-caused in finding #10); SPF+DMARC pass; landed in Inbox |
| T5 | Routing: agent@, test@, unknown local-part | PASS (catch-all behavior for unknown) |
| T6 | Claude Code MCP: list + send + read/mark | PARTIAL — list/send/read PASS; **mark-read FAIL** (finding #8); plugin not auto-discovered in `claude -p` (finding #7) |
| T7 | Codex CLI MCP: list + send + read/mark | PARTIAL — list/send/read PASS; **mark-read FAIL** (finding #8) |
| T8 | Trigger fires on trusted sender | PARTIAL — flag PASS, log FAIL (**finding #9 shell injection**) |
| T9 | Trigger does NOT fire on untrusted sender | DEVIATION (documented — DKIM-pass overrides allowlist) |
| T10 | Trigger does NOT fire on outbound | PASS |
| T11 | Trust model: `"none"`, `"true"`, `"false"` all observed | PASS |
| T12 | Reboot survival | PASS |
| T13 | Setup re-entrance | PASS — also uncovered **finding #10** (DKIM key mismatch) |

## Top-priority findings

In priority order:

| # | Severity | Finding | Fix direction |
|---|---|---|---|
| #10 | **P0** | DKIM keypair on disk doesn't match DNS record → all outbound DKIM signatures fail at receivers (root cause of #6) | Republish DNS TXT with current on-disk public key, OR regenerate + republish. |
| #6 | **P0** | Outbound DKIM `dkim=fail` at Gmail | Resolved by fixing #10. |
| #9 | **P0 (security)** | Shell injection in `on_receive cmd` template expansion | Pass template vars as `sh -c '<script>' -- "$FROM" "$SUBJECT" "$FILEPATH"` instead of inlining into quoted strings. Update `book/channel-recipes.md` and `docs/manual-test.md`. |
| #8 | P1 | MCP write ops (`email_mark_read`, etc.) fail when MCP runs as non-root because files are `root:root 0644` | Proxy write ops through the daemon over UDS, OR `umask 002` + shared `aimx` group. |
| #7 | P1 | `aimx agent-setup claude-code` writes the plugin but doesn't register the MCP server with `claude mcp add`; `claude -p` can't see the tool until manually registered | Have `agent-setup` invoke `claude mcp add`, or print the exact command to run after install. |
| #4 | P1 | `aimx send` fails as a non-root user because `/etc/aimx/config.toml` is `0640 root:root` | Either change send to ask the daemon for mailbox resolution (clean), or create an `aimx` group and document membership (workable). |
| #2 | P2 | Inbound SPF resolver reports `spf = "none"` for mail from domains that clearly publish SPF covering the observed IP (`uzyn.com` + Gmail) | Debug `build_spf_output` / `mail_auth::verify_spf` — likely an `include:` chain resolution issue or an envelope-vs-header From disagreement. |
| #5 | P2 | `aimx send` accepts any `From:` under the operator's domain when a wildcard catchall exists (`*@…` matches `bogus@…`) | Decide whether `send` should be stricter than route-match; possibly require exact mailbox. |
| #1 | P3 (DX) | `aimx mailbox create` silently produces a state where inbound is misrouted until the daemon is restarted | Either print a restart hint or signal-reload the daemon. |
| #3 | P4 | User forwarded/replied to earlier messages; subjects got `Fwd:`/`Re:` prefixes. Not a bug — plan wording could clarify "compose new". |

## Deviations from plan (non-defects)

- Session ran on an existing install rather than a fresh VPS; T1 was re-entrance-style.
- Plan expected SPF `v=spf1 a mx ~all`; deployment uses the stricter `ip4:92.222.243.76 -all`. Both valid.
- `[LOCAL]` tests (T6, T7) ran on the VPS (claude + codex are installed here).
- Plugin layouts differ from plan wording (`.claude-plugin/plugin.json` inside the aimx plugin; `~/.codex/skills/aimx/` instead of `~/.codex/plugins/aimx/`).
- Setup on re-entry rewrites `README.md` unconditionally rather than only when the binary is newer. Benign.
- T9 trigger DID fire for an untrusted sender, which the plan explicitly calls out as "expected behavior under current v1 semantics — document what you observe". Documented.

## Side-effects left in place

- `/etc/aimx/config.toml` mode changed from `0640` → `0644` to work around finding #4.
- `[mailboxes.test]` in config has `trust = "verified"`, `trusted_senders = ["chua@uzyn.com"]`, and an `on_receive` trigger (from T8). Revert if you want a clean slate.
- `/tmp/aimx-trigger-*.flag` root-owned artifacts from T8/T9 still exist; clean with `sudo rm /tmp/aimx-trigger*.flag /tmp/aimx-trigger.log`.

## Files referenced during tests

- `/etc/aimx/config.toml` — domain, mailboxes, triggers (mode now `0644`).
- `/etc/aimx/dkim/{private,public}.key` — DKIM keys (private `0600`, public `0644`).
- `/etc/ssl/aimx/{cert,key}.pem` — TLS cert.
- `/etc/systemd/system/aimx.service` — service unit, `User=root`, `RuntimeDirectory=aimx`.
- `/var/lib/aimx/inbox/{agent,catchall,inbox,test}/` — received mail.
- `/var/lib/aimx/sent/{agent,inbox,test}/` — sent copies (no `sent/catchall/` unless send goes via catchall wildcard).
- `/var/lib/aimx/README.md` — datadir layout guide (refreshed by setup).
- `/run/aimx/send.sock` — UDS for `aimx send` (mode `0666`).
- `~/.claude/plugins/aimx/` — Claude Code plugin (needs manual `claude mcp add` to activate).
- `~/.codex/skills/aimx/` — Codex skill (paired with `~/.codex/config.toml [mcp_servers.aimx]`).
- `sudo journalctl -u aimx -f` — daemon logs.
