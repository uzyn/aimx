# FAQ

Short answers to questions that come up often. See the linked pages for depth.

## Deployment

### My VPS blocks port 25 — which providers don't?

See the [compatible provider table](getting-started.md#compatible-vps-providers). Hetzner, OVH, Vultr, BuyVM, and Linode all permit port 25 (some after a support ticket). DigitalOcean, AWS EC2, Azure VMs, and GCP block it permanently.

### Can I run AIMX in Docker or behind NAT?

Docker works if you map port 25 and persist `/etc/aimx`, `/var/lib/aimx`, and `/run/aimx` on the host. Behind NAT you must port-forward 25/tcp both ways and the MX record must resolve to the public IP. AIMX learns the sender IP from the TCP peer, so any proxy in front of port 25 has to be transparent (PROXY protocol is not supported).

### Can I run two AIMX instances on one host?

Only if each binds a different IP on port 25 — two listeners cannot share the same `ip:25`. Point each instance at its own `AIMX_CONFIG_DIR` and `AIMX_DATA_DIR`, run each from its own systemd unit, and give each its own UDS path (the default `/run/aimx/send.sock` is hard-coded today; a second instance needs a source patch).

### How do I upgrade the binary without losing mail or breaking in-flight SMTP sessions?

Replace `/usr/local/bin/aimx` and `systemctl restart aimx`. `aimx serve` handles SIGTERM by draining both the SMTP and UDS accept loops, so in-flight sessions finish before the process exits. Mail on disk is format-stable; no migration step is required between patch releases.

### How do I migrate to a new server or change the domain?

Same domain, new server: `rsync -a /etc/aimx/ /var/lib/aimx/` to the new host, install the binary, `sudo aimx setup <domain>` (re-entrant — it will reuse the existing DKIM key), then flip the A/MX record. Different domain: it's a fresh `aimx setup` — the DKIM selector, SPF, and DMARC records all reference the domain and must be regenerated.

## DNS and deliverability

### Do I actually need a PTR record?

Gmail and Outlook both penalise mail from IPs without matching forward-confirmed reverse DNS. Set a PTR at your VPS control panel pointing to the same hostname as your MX. `aimx setup` intentionally does not touch this — it's provider-specific.

### Gmail still marks mail as spam after DKIM/SPF/DMARC pass — what now?

The usual fix is (a) set PTR to your MX hostname, (b) have a recipient reply once or add a Gmail filter `from:*@yourdomain.com → Never send to Spam`, (c) stop sending terse one-line bodies from fresh IPs. New IPs need a short warm-up regardless of auth.

### How do I rotate the DKIM key without a delivery gap?

AIMX today supports one active selector at a time. To rotate without bounces:

1. `sudo aimx dkim-keygen --selector aimx2` (generates a new keypair under a second selector).
2. Publish the new TXT record at `aimx2._domainkey.<domain>`, wait for propagation.
3. Flip `dkim_selector = "aimx2"` in `config.toml` and `systemctl restart aimx`.
4. Leave the old DNS record up for a few days so in-flight mail still verifies, then remove it.

### Enabling `enable_ipv6` — what exactly changes?

Outbound delivery starts preferring AAAA records when the recipient publishes them. You need to (a) add an AAAA record for your MX hostname and (b) extend SPF with `ip6:<your /64 or full v6>`. If you leave SPF at the default `ip4:YOUR_IP -all`, every v6-delivered message will SPF-fail.

## Sending

### Why does `aimx send` refuse to run as root?

`aimx send` is a thin UDS client — it does not read `config.toml` or the DKIM key. All privileged work happens inside `aimx serve`. Refusing root nudges operators to invoke sends from their normal user or an agent account, which is the intended path.

### Can I send from `*@domain` (the catchall)?

No. The catchall is inbound-only. Outbound `From` must resolve to a concrete, non-wildcard mailbox in `config.toml`; the daemon parses the submitted `From:` header itself and rejects catchalls.

### What happens on a deferred or failed MX delivery?

AIMX does not run a retry queue. A transient (4xx) failure returns `Deferred` to the client and is **not** persisted — the client (e.g. `aimx send`, an agent) is expected to retry. A permanent (5xx) failure is persisted to `sent/<mailbox>/` with `delivery_status = "failed"` and the SMTP reason in `delivery_details`. AIMX does not generate DSNs.

### Can I send with attachments, a custom Reply-To, or a custom Message-Id?

Attachments: yes, repeat `--attachment <path>`. Custom `Reply-To:` header: not exposed on the CLI (the `--reply-to` flag sets `In-Reply-To` for threading, not the `Reply-To` header). Custom `Message-Id`: not exposed — the daemon generates one per send.

## Storage

### Is the mailbox tree safe to `rsync` or snapshot while `aimx serve` is running?

Yes for reads. `rsync -a` or a filesystem snapshot of `/var/lib/aimx/` will produce a consistent per-file copy — inbound ingest writes each `.md` atomically (temp file + rename) and mark-read rewrites are serialised under a per-mailbox lock. A snapshot taken mid-ingest may miss the newest message, never a half-written one.

### How is `thread_id` computed — will threading agree with Gmail?

`thread_id` is `sha256(root)[..8]` in hex, where `root` is the first Message-Id in `In-Reply-To`, else the first in `References`, else the email's own Message-Id. This walks the same header chain Gmail uses, so replies thread correctly in both. Subject-based collapsing (Gmail's fallback) is not replicated — if a conversation loses its `References` chain, the two systems can disagree.

### Can I hand-edit an email's frontmatter, or will the daemon fight me?

You can edit at rest, but concurrent writes are not arbitrated. A `MARK-READ`/`MARK-UNREAD` rewrite takes a per-mailbox lock, does a read-modify-write, and atomically renames the result back into place. A concurrent hand-edit will lose to whichever write lands last. Stop the daemon, edit, restart — or do it through MCP.

## Hooks

### My `on_receive` hook didn't fire — how do I tell why?

Check in this order:

1. `journalctl -u aimx | grep hook_id=<id>` — every fire emits one structured line. No line means the hook was filtered out or gated.
2. The target email's frontmatter: `trusted = "false"` plus `dangerously_support_untrusted` unset is the most common cause. See the [trust gate](hooks.md#trust-gate-on_receive-only).
3. Match filters: `from`, `subject`, `has_attachment` are AND-combined; one mismatch skips silently.
4. If the line is there with a non-zero `exit_code`, it's your shell command — test the `cmd` string against the saved `.md` manually.

### Does a slow hook block the SMTP `DATA` response?

Yes. Hooks run synchronously under `sh -c` and the daemon awaits the subprocess. The SMTP peer does not see a `250 OK` on DATA until the hook returns. Fan out to a queue or run the heavy work in `cmd &` if the hook body is more than a second or two.

### Env var vs. `{id}`/`{date}` placeholder — when do I use which?

Env vars (`$AIMX_FROM`, `$AIMX_SUBJECT`, …) carry sender-controlled header content. Always expand them inside double quotes; never splice them into the `cmd` string. Placeholders (`{id}`, `{date}`) are aimx-generated (slug and ISO-8601 date) and are substituted into `cmd` directly — use them when you need the value in a filename or path literal, where a shell variable would not expand.

### Can an `after_send` hook distinguish a deferral from a permanent failure?

Yes. `AIMX_SEND_STATUS` is `"delivered"`, `"deferred"`, or `"failed"`. Deferrals do not persist a sent file, so `AIMX_FILEPATH` is empty for them.

## Trust

### What does `trust = "verified"` actually check?

Two conditions: the sender address matches a glob in the effective `trusted_senders` list, AND the inbound DKIM result is `pass`. SPF and DMARC are recorded in frontmatter but are not part of the gate. Missing either of those two conditions yields `trusted = "false"`.

### Per-mailbox `trusted_senders` — does it merge with the global list?

It replaces. Setting `trusted_senders` under a mailbox fully overrides the top-level list for that mailbox — there is no merge, and an empty per-mailbox list means "nobody" for that mailbox.

### When is `dangerously_support_untrusted` actually appropriate?

When the hook's side effect is safe regardless of sender — a logger, a metric counter, a `ntfy` notification with no email content in the payload. Never use it on a hook that hands the email body to an agent or to any shell command that quotes the body.

## Security model

### `send.sock` is mode `0666` — why is that fine?

Any local user can submit an outbound message, but the DKIM private key (`/etc/aimx/dkim/private.key`, mode `0600`, root-only) stays inside `aimx serve`. The UDS is a signing oracle for the configured mailboxes and that is the intended authorisation boundary. If local users on this host cannot be trusted to send mail under your domain at all, run AIMX on a dedicated host.

### The mailbox tree is world-readable — why is that fine?

AIMX assumes a single-operator server where every local user/agent is in the trust boundary. If you need per-user mailbox isolation, tighten permissions yourself with `chmod`/ACLs, or run one AIMX instance per tenant on its own host. The documentation is explicit about this trade-off in [Security model](getting-started.md#security-model).

### Who can read the DKIM private key, and what happens if it leaks?

Only root, via `/etc/aimx/dkim/private.key` (mode `0600`). A leak lets anyone sign mail as your domain until you rotate. Rotate with the selector swap described above; publish a DMARC forensic address if you want to detect abuse.

## MCP

### Can two agents share one `aimx mcp` process?

No. `aimx mcp` uses stdio transport; each MCP client spawns and owns its own process. The filesystem is the shared resource — concurrent MCP processes coordinate through the daemon (mark-read, mailbox CRUD) or through atomic file writes (ingest, send).

### How do I scope an agent to a single mailbox?

AIMX does not implement MCP-level access control today. Every MCP tool call sees every mailbox. If you need isolation, run a second AIMX instance on a different host (or different IP + config dir) and give each agent its own.

### How do I update the installed agent plugin after upgrading AIMX?

`aimx agent-setup <agent> --force`. The plugin bundle is embedded in the binary at compile time, so the installed plugin is always in sync with the binary version — re-running with `--force` overwrites whatever is at the destination.

## Operations

### `systemctl status aimx` says `start-limit-hit` — what is it?

The unit caps restarts at `StartLimitBurst=5` within `StartLimitIntervalSec=60`. `sudo systemctl reset-failed aimx` clears the counter; `sudo systemctl start aimx` retries. Investigate the crash in `journalctl -u aimx -e` first — a restart-loop is usually a config error the restart won't fix.

### Where do daemon logs go on OpenRC?

OpenRC does not have journald. AIMX writes nothing of its own — `aimx logs` tails `/var/log/aimx/*.log` if the init script redirects there, otherwise it falls back to `/var/log/messages`. On systemd, `aimx logs` shells out to `journalctl -u aimx`.

### How do I run a dry-run send without touching real MX servers?

Set `AIMX_TEST_MAIL_DROP=/path/to/dir` before starting `aimx serve`. Every outbound submission is written to that directory instead of being delivered; lettre is not invoked. The daemon logs a startup warning so you cannot leave this on in production by accident. Unset the env var and restart to go live.

## Verifier service

### When would I self-host `services/verifier/`?

When you do not want your setup traffic to hit `check.aimx.email`, or when you are deploying AIMX in an air-gapped / regulated environment. The verifier is a small axum service plus a port-25 listener; see `services/verifier/README.md` for the Docker Compose deploy. Point AIMX at it with `verify_host` in `config.toml` or `--verify-host` at the command line.
