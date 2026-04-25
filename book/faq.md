# FAQ

## Deployment

### Why does AIMX need port 25 open for both inbound and outbound?

Inbound: every receiving MTA listens on port 25. It is the SMTP port defined by RFC 5321. Your MX record points at your server and delivering MTAs connect on 25 to hand mail over.

Outbound: AIMX delivers directly to each recipient's MX on port 25. Most VPS providers block outbound 25 by default to contain spam from compromised instances, so check the [compatible provider table](getting-started.md#compatible-vps-providers) before you sign up. Ports 465/587 are submission ports used to hand mail to a relay. AIMX *is* the MTA, so they do not apply.

### Can I run AIMX in Docker or behind NAT?

Docker works if you map port 25 and persist `/etc/aimx`, `/var/lib/aimx`, and `/run/aimx` on the host. Behind NAT you must port-forward 25/tcp both ways and the MX record must resolve to the public IP. AIMX learns the sender IP from the TCP peer, so any proxy in front of port 25 has to be transparent (PROXY protocol is not supported).

### Can I run two AIMX instances on one host?

Only if each binds a different IP on port 25. Two listeners cannot share the same `ip:25`. Point each instance at its own `AIMX_CONFIG_DIR` and `AIMX_DATA_DIR`, run each from its own systemd unit, and give each its own UDS path (the default `/run/aimx/aimx.sock` is hard-coded today. A second instance needs a source patch).

### How do I upgrade the binary without losing mail or breaking in-flight SMTP sessions?

Replace `/usr/local/bin/aimx` and `systemctl restart aimx`. `aimx serve` handles SIGTERM by draining both the SMTP and UDS accept loops, so in-flight sessions finish before the process exits. Mail on disk is format-stable; no migration step is required between patch releases.

### How do I migrate to a new server or change the domain?

Same domain, new server: `rsync -a /etc/aimx/ /var/lib/aimx/` to the new host, install the binary, `sudo aimx setup <domain>` (re-entrant, it reuses the existing DKIM key), then flip the A/MX record. Different domain: run a fresh `aimx setup`. The DKIM selector, SPF, and DMARC records all reference the domain and must be regenerated.

## DNS and deliverability

### What is PTR record? Do I actually need it?

PTR (Pointer Record) is a reverse-DNS record. It maps an IP back to a hostname, the opposite of an A/AAAA record. Setting one improves outbound deliverability and is usually configured at your hosting provider's control panel rather than at your normal DNS registrar. Because AIMX is not meant for bulk sending, **a PTR is optional**. If you are only mailing a handful of targeted recipients (often yourself), having DKIM/SPF/DMARC pass, and if needed whitelisting the sender in your mail client, is usually enough.

### How do I rotate the DKIM key without a delivery gap?

AIMX today supports one active selector at a time. To rotate without bounces:

1. `sudo aimx dkim-keygen --selector aimx2` (generates a new keypair under a second selector).
2. Publish the new TXT record at `aimx2._domainkey.<domain>`, wait for propagation.
3. Flip `dkim_selector = "aimx2"` in `config.toml` and `systemctl restart aimx`.
4. Leave the old DNS record up for a few days so in-flight mail still verifies, then remove it.

### Enabling `enable_ipv6`: what exactly changes?

Outbound delivery starts preferring AAAA records when the recipient publishes them. You need to (a) add an AAAA record for your MX hostname and (b) extend SPF with `ip6:<your /64 or full v6>`. If you leave SPF at the default `ip4:YOUR_IP -all`, every v6-delivered message will SPF-fail.

## Sending

### Can I send from `*@domain` (the catchall)?

No. The catchall is inbound-only. Outbound `From` must resolve to a concrete, non-wildcard mailbox in `config.toml`. The daemon parses the submitted `From:` header itself and rejects catchalls.

### What happens on a deferred or failed MX delivery?

AIMX does not run a retry queue. A transient (4xx) failure returns `Deferred` to the client and is **not** persisted. The client (e.g. `aimx send`, an agent) is expected to retry. A permanent (5xx) failure is persisted to `sent/<mailbox>/` with `delivery_status = "failed"` and the SMTP reason in `delivery_details`. AIMX does not generate DSNs. This keeps the delivery result visible to the calling agent in real time. No send-and-pray.

### Can I send with attachments, a custom Reply-To, or a custom Message-Id?

Attachments: yes, repeat `--attachment <path>`. Custom `Reply-To:` header: not exposed on the CLI (the `--reply-to` flag sets `In-Reply-To` for threading, not the `Reply-To` header). Custom `Message-Id`: not exposed. The daemon generates one per send.

## Storage

### Is the mailbox tree safe to `rsync` or snapshot while `aimx serve` is running?

Yes for reads. `rsync -a` or a filesystem snapshot of `/var/lib/aimx/` will produce a consistent per-file copy. Inbound ingest writes each `.md` atomically (temp file + rename) and mark-read rewrites are serialised under a per-mailbox lock. A snapshot taken mid-ingest may miss the newest message, never a half-written one.

### How is `thread_id` computed, and will threading agree with Gmail?

`thread_id` is `sha256(root)[..8]` in hex, where `root` is the first Message-Id in `In-Reply-To`, else the first in `References`, else the email's own Message-Id. This walks the same header chain Gmail uses, so replies thread correctly in both. Subject-based collapsing (Gmail's fallback) is not replicated. If a conversation loses its `References` chain, the two systems can disagree.

## Hooks

### My `on_receive` hook didn't fire. How do I tell why?

Check in this order:

1. `journalctl -u aimx | grep hook_name=<name>`. Every fire emits one structured line. No line means the hook was gated.
2. The target email's frontmatter: `trusted = "false"` plus `dangerously_support_untrusted` unset is the most common cause. See the [trust gate](hooks.md#trust-gate-on_receive-only).
3. If the line is there with a non-zero `exit_code`, it's your shell command. Test the `cmd` string against the saved `.md` manually.

### Why can my agent create hooks but not run arbitrary shell commands?

This is the core of the hook-templates safety model. A naive design would expose a `hook_create` MCP tool that takes a `cmd` string and writes it to `config.toml`. That's a local RCE waiting to happen: any process that speaks MCP (and the `aimx.sock` UDS is world-writable so hooks can be created without root) could talk the daemon into running arbitrary shell every time mail arrives.

Hook templates split the problem. The **operator** installs a small set of pre-vetted command *shapes* once, during `aimx setup`:

```toml
[[hook_template]]
name = "invoke-claude"
cmd = ["/usr/local/bin/claude", "-p", "{prompt}"]
params = ["prompt"]
```

The **agent** can then only bind to those shapes, filling declared `{placeholder}` slots with string values:

```json
{"template": "invoke-claude", "params": {"prompt": "File this email"}}
```

Two properties make this safe:

1. **No shell invocation.** The daemon builds the argv vector directly from `cmd` + `params` and calls `execvp`. `/bin/sh -c` is never used for template hooks, so there is no string-to-argv parsing step for an attacker to subvert.
2. **Values can't escape their slot.** Substitution happens after the argv is already split. A parameter value like `"; rm -rf /"` lands as one complete argv entry passed verbatim to the target binary; it cannot introduce new argv entries, redirections, pipes, or quote escapes. NUL, `\r`, and other control bytes are rejected outright.

Raw `cmd` submission is physically impossible over the UDS: the `HOOK-CREATE` verb rejects any request body containing a `cmd`, `run_as`, `dangerously_support_untrusted`, `timeout_secs`, or `stdin` field. Those are template properties — not hook properties — so they can't leak through.

The operator keeps the full-power escape hatch: `sudo aimx hooks create --cmd "..."` writes `config.toml` directly. That requires root, bypasses the UDS, and is intentionally not reachable from MCP.

### Env var vs. `{id}`/`{date}` placeholder: when do I use which?

Env vars (`$AIMX_FROM`, `$AIMX_SUBJECT`, …) carry sender-controlled header content. Always expand them inside double quotes. Never splice them into the `cmd` string. Placeholders (`{id}`, `{date}`) are AIMX-generated (slug and ISO-8601 date) and are substituted into `cmd` directly. Use them when you need the value in a filename or path literal, where a shell variable would not expand.

### Can an `after_send` hook distinguish a deferral from a permanent failure?

Yes. `AIMX_SEND_STATUS` is `"delivered"`, `"deferred"`, or `"failed"`. Deferrals do not persist a sent file, so `AIMX_FILEPATH` is empty for them.

## Trust

### What does `trust = "verified"` actually check?

Two conditions: the sender address matches a glob in the effective `trusted_senders` list, AND the inbound DKIM result is `pass`. SPF and DMARC are recorded in frontmatter but are not part of the gate. Missing either of those two conditions yields `trusted = "false"`.

### Per-mailbox `trusted_senders`: does it merge with the global list?

It replaces. Setting `trusted_senders` under a mailbox fully overrides the top-level list for that mailbox. There is no merge, and an empty per-mailbox list means "nobody" for that mailbox.

### When is `dangerously_support_untrusted` actually appropriate?

When the hook's side effect is safe regardless of sender. A logger, a metric counter, a push notification with no email content in the payload. Never use it on a hook that hands the email body to an agent or to any shell command that quotes the body.

## Security model

> The canonical write-up lives at [Security](security.md). The entries below are the common questions; that page has the full model.

### Can I use AIMX in place of Postfix or Stalwart?

No, and that is intentional. AIMX is a single-operator mail server designed for AI agents on a domain you own, not a general-purpose MTA for human users. It has no IMAP/POP3, no webmail, no per-user authentication, no LMTP, no virtual alias tables, and no submission port on 587. Mailboxes are world-readable by design and every hook and MCP tool addresses the whole mailbox tree.

### `aimx.sock` is mode `0666`, why is that fine?

Any local user can submit an outbound message, but the DKIM private key (`/etc/aimx/dkim/private.key`, mode `0600`, root-only) stays inside `aimx serve`. The UDS is a signing oracle for the configured mailboxes and that is the intended authorisation boundary. If local users on this host cannot be trusted to send mail under your domain at all, run AIMX on a dedicated host.

### The mailbox tree is world-readable, why is that fine?

AIMX assumes a single-operator server where every local user and agent is inside the trust boundary. If you need per-user mailbox isolation, AIMX is the wrong tool. Run a general-purpose MTA like Postfix or Stalwart instead (see [Can I use AIMX in place of Postfix or Stalwart?](#can-i-use-aimx-in-place-of-postfix-or-stalwart) above). The trade-off is documented in [Security model](getting-started.md#security-model).

### Who can read the DKIM private key, and what happens if it leaks?

Only root, via `/etc/aimx/dkim/private.key` (mode `0600`). A leak lets anyone sign mail as your domain until you rotate. Rotate with the selector swap described above; publish a DMARC forensic address if you want to detect abuse.

## MCP

### Can two agents share one `aimx mcp` process?

No. `aimx mcp` uses stdio transport. Each MCP client spawns and owns its own process. The filesystem is the shared resource. Concurrent MCP processes coordinate through the daemon (mark-read, mailbox CRUD) or through atomic file writes (ingest, send).

### How do I scope an agent to a single mailbox?

AIMX does not implement MCP-level access control today. Every MCP tool call sees every mailbox. If you need isolation, run a second AIMX instance on a different host (or different IP + config dir) and give each agent its own.

### How do I update the installed agent plugin after upgrading AIMX?

`aimx agent-setup <agent> --force`. The plugin bundle is embedded in the binary at compile time, so the installed plugin is always in sync with the binary version. Re-running with `--force` overwrites whatever is at the destination.

## Operations

### `systemctl status aimx` says `start-limit-hit`. What is it?

The unit caps restarts at `StartLimitBurst=5` within `StartLimitIntervalSec=60`. `sudo systemctl reset-failed aimx` clears the counter. `sudo systemctl start aimx` retries. Investigate the crash in `journalctl -u aimx -e` first. A restart-loop is usually a config error the restart won't fix.

### Where do daemon logs go on OpenRC?

OpenRC does not have journald. aimx writes nothing of its own. `aimx logs` tails `/var/log/aimx/*.log` if the init script redirects there, otherwise it falls back to `/var/log/messages`. On systemd, `aimx logs` shells out to `journalctl -u aimx`.

### How do I run a dry-run send without touching real MX servers?

Set `AIMX_TEST_MAIL_DROP=/path/to/dir` before starting `aimx serve`. Every outbound submission is written to that directory instead of delivered. See [Configuration: Environment variables](configuration.md#environment-variables) for the full set.

## Verifier service

### What is `services/verifier`?

A small companion service that exists purely to answer the question "is port 25 actually reachable from the public internet?". `aimx portcheck` and `aimx setup` call it during setup. Nothing in the mail path depends on it. By default aimx points at the hosted instance at `check.aimx.email`, so you do not need to run your own.

### When would I self-host `services/verifier/`?

When you do not want your setup traffic to hit `check.aimx.email`, or when you are deploying aimx in an air-gapped / regulated environment. The verifier is a small axum service plus a port-25 listener. See `services/verifier/README.md` for the Docker Compose deploy. Point aimx at it with `verify_host` in `config.toml` or `--verify-host` at the command line.
