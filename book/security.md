# Security model

aimx is a mail server for a single operator running AI agents on a host they control. This page describes the threat model it is built for, the trust boundaries that hold the design together, and — equally important — what aimx deliberately does not defend against.

If you read only one sentence: **the authorisation boundary in aimx is the root-only DKIM private key, not filesystem ACLs and not the socket permissions**. Everything else on this page is a consequence of that choice.

The reasoning is narrow: the only thing that lets an actor credibly impersonate your domain on the public internet is a DKIM signature that verifies against the TXT record you published. Local file ACLs and socket modes only decide who on this host gets to *ask* the daemon to sign. They do not, on their own, produce forgeable output. If we let unprivileged local subjects reach the signing oracle but never the key itself, we can design the surrounding system for ergonomics — world-readable mail for LLM parsing, world-writable UDS for agent ergonomics — without weakening the one boundary that matters when the mail leaves the host. This is the single decision every other choice on this page defers to.

## At a glance

- aimx is a **single-operator, single-host** SMTP server. It assumes one administrator and treats every local user and agent on that host as inside the trust boundary.
- `aimx serve` runs as root and owns the DKIM signing key. All other processes (`aimx send`, `aimx mcp`, hook subprocesses) are unprivileged and cannot forge outbound signatures.
- The Unix socket at `/run/aimx/aimx.sock` is intentionally world-writable (`0666`). It is a signing oracle for the configured mailboxes, nothing more; it cannot be used to run arbitrary commands on the host.
- Hook subprocesses run as the unprivileged `aimx-hook` user by default. Template hooks (the agent-creatable kind) never invoke a shell and cannot escape their declared argv slots.
- DKIM / SPF / DMARC results are recorded in every inbound email's frontmatter but only DKIM gates hook execution. Mail is always stored, regardless of the authentication outcome.
- aimx does not implement per-user mailbox isolation, IMAP / POP3, webmail, SMTP AUTH, a retry queue, DSN bounces, spam filtering, or rate limiting. Those are out of scope by design, not on a roadmap.

## Threat model

### Who you are

The operator owns the host, the domain, and every local user on the box. You are the single administrator of a VPS or home server with port 25 open to the internet. You install aimx to give one or more AI agents their own email addresses on your domain.

### Who the adversaries are

aimx is built to survive the following:

- **The public internet on port 25.** Unauthenticated senders, spammers, phishers, and automated scanners will connect. aimx accepts their mail, records the authentication results, and lets the operator (or an agent, via a hook) decide what to do with it.
- **A confused or compromised local agent.** An agent might follow a prompt-injected instruction and try to exfiltrate mail, send spam under your domain, or install a hook that runs arbitrary shell. The design prevents the third outright and limits the first two.
- **A curious local user.** A user on the host with no sudo rights can read every mailbox and submit mail through the world-writable socket, but cannot sign mail as a wildcard sender, forge DKIM, or escape the hook template sandbox.

### Who the adversaries are not

aimx does **not** attempt to defend against:

- **Hostile code running as root on the same host.** Root can read the DKIM key, edit `config.toml`, and replace the daemon binary. Nothing below that privilege level can stop it.
- **Multiple mutually-distrustful humans sharing the box.** Mailboxes are world-readable. If two users on one host need private inboxes, aimx is the wrong tool — run [Postfix](https://www.postfix.org/) or [Stalwart](https://stalw.art/) instead.

## Trust boundaries

The clearest view of aimx is a table of who-can-reach-what:

| Subject            | Runs as               | Holds the DKIM key | Can submit to UDS         | Can edit `config.toml` |
|--------------------|-----------------------|--------------------|---------------------------|------------------------|
| `aimx serve`       | root                  | yes                | n/a (handles the socket)  | yes (via its own UDS)  |
| `aimx send`        | invoking user         | no                 | yes                       | no                     |
| `aimx mcp`         | the agent's user      | no                 | yes (subset of verbs)     | no                     |
| Hook subprocess    | `aimx-hook`           | no                 | no (env-only)             | no                     |
| Any local user     | their login UID       | no                 | yes (socket is `0666`)    | no                     |

The only subject that touches the DKIM key is the daemon. Every other subject that wants to send mail under your domain has to ask the daemon nicely, over the socket — and the daemon, not the caller, decides whether to sign.

## The DKIM boundary

The DKIM private key is the one thing on disk that unprivileged subjects cannot forge, bypass, or reproduce.

| Path                             | Mode   | Owner       |
|----------------------------------|--------|-------------|
| `/etc/aimx/config.toml`          | `0640` | `root:root` |
| `/etc/aimx/dkim/private.key`     | `0600` | `root:root` |
| `/etc/aimx/dkim/public.key`      | `0644` | `root:root` |

`aimx serve` loads the private key once at startup into an `Arc<DkimKey>` and signs every outbound message in-process. The key is never passed to subprocesses and never written to a descriptor other than its original file. When the key is rotated (new selector), you SIGTERM the daemon, update `dkim_selector` in `config.toml`, and start the daemon again.

If the DKIM key leaks, anyone can sign mail under your domain until you rotate. Treat it like an SSH host key. See [How do I rotate the DKIM key without a delivery gap?](faq.md#how-do-i-rotate-the-dkim-key-without-a-delivery-gap) for the recipe.

## File and socket layout

Everything aimx creates on disk has a deliberate permission choice. The surprising ones are called out below the table.

| Path                          | Mode    | Owner             | Purpose                                            |
|-------------------------------|---------|-------------------|----------------------------------------------------|
| `/etc/aimx/`                  | `0755`  | `root:root`       | Config + DKIM directory                            |
| `/etc/aimx/config.toml`       | `0640`  | `root:root`       | Mailboxes, trust policy, hooks, templates          |
| `/etc/aimx/dkim/private.key`  | `0600`  | `root:root`       | DKIM signing key                                   |
| `/etc/aimx/dkim/public.key`   | `0644`  | `root:root`       | Published in the `_domainkey` TXT record           |
| `/var/lib/aimx/`              | `0755`  | `root:root`       | Storage root (world-readable *by design*)          |
| `/var/lib/aimx/inbox/`        | group-readable | `root:aimx-hook` | Inbound mail (`chmod -R g+rX` so hook stdin works) |
| `/var/lib/aimx/sent/`         | group-readable | `root:aimx-hook` | Outbound copies (`chmod -R g+rX` so hook stdin works) |
| `/run/aimx/`                  | `0755`  | `root:root`       | Runtime directory (provided by systemd/OpenRC)     |
| `/run/aimx/aimx.sock`         | `0666`  | `root:root`       | UDS signing oracle (world-writable *by design*)    |

Two choices are load-bearing and deserve their own sections below: the **world-readable storage tree** and the **world-writable socket**. Both are deliberate, and both have their escape hatches (run on a dedicated host, don't share the box with untrusted users).

### `/etc/aimx/` and `/var/lib/aimx/`

The two directories exist on purpose to hold opposite kinds of data.

`/etc/aimx/` holds **secrets and policy**: the DKIM private key that signs as your domain, the mailbox list, the hook config, the template definitions. If any of that is writable or readable by a non-root process, the boundary collapses. So the whole tree is root-owned with the key at `0600`, the config at `0640`, and the daemon is the only process that opens it for read at startup.

`/var/lib/aimx/` holds **the mailboxes**: Markdown files with TOML frontmatter, one per email, plus their attachments as siblings in a Zola-style bundle. This tree is world-readable on purpose, because the whole point of storing mail this way is to hand agents a flat, parse-friendly corpus they can `ls`, `grep`, `head`, index with an LLM pipeline, or walk directly from a shell hook without going through an IMAP server or a mailbox API. `.md` + TOML frontmatter is specifically chosen because every LLM and every RAG tool already knows how to read it; there is no custom schema to learn and no decoder to write. Making the tree private would either force agents to speak IMAP (a protocol they are notoriously bad at) or to proxy every read through an MCP tool (adding latency and cost). The deliberate trade is: on a single-operator host, local read access to the mailbox tree is a feature, not a leak.

The boundary between these two directories is why the design holds: secrets never flow outward, mail never flows into the secrets tree.

## Inbound SMTP

`aimx serve` listens on port 25 and accepts plain SMTP. STARTTLS is advertised and supported but **not required** — remote MTAs that speak plain SMTP are accepted, because that is still the norm for inter-MTA traffic. There is no `AUTH` extension, no `SMTP-AUTH`, and no rate limit. Any sender on the public internet can connect, complete an SMTP dialogue, and hand aimx a message.

### Port 25 is open, but aimx is not an open relay

Every MTA on the public internet listens on port 25. That is where RFC 5321 says mail arrives. Exposing it is not a security posture; it is a prerequisite for receiving mail at all. The interesting question is *what the listener is willing to do* with what it receives.

An **open relay** is an MTA that accepts mail over SMTP and then forwards it back out to a third-party destination, typically in service of spam. aimx is not an open relay, by construction:

- **Inbound and outbound paths never cross.** Inbound SMTP writes the message to `inbox/<mailbox>/` on local disk and returns. There is no code path from the SMTP listener to the outbound delivery layer. The outbound path is triggered only by a `SEND` request on the UDS — submitted by a process already on the host, with a validated `From:` that must resolve to a configured local mailbox.
- **Inbound recipients must be yours.** The recipient domain on every `RCPT TO` is compared case-insensitively against `config.domain` (see `recipient_domain_matches` in `src/smtp/session.rs`). Anything that is not an exact match — an unrelated domain, a subdomain of `config.domain`, or an address with no `@` at all — is refused at SMTP time with `550 5.7.1 relay not permitted` and never reaches storage. Subdomains are rejected on purpose: aimx is one-domain-per-instance, so `sub.yourdomain.com` has to be its own aimx install with its own DKIM key. The local-part-to-mailbox routing (including the `catchall` fallback for unknown local parts) only runs *after* the domain check passes, so `catchall` only ever covers unknown locals on *your* domain and is never the landing pad for relay attempts.
- **Outbound senders must be yours.** The daemon parses `From:` from the submitted body and refuses to sign anything whose domain is not exactly `config.domain`, and whose local part is not a concrete (non-wildcard) configured mailbox. Even a local user with access to the world-writable UDS cannot cause aimx to sign mail as `someone-else@someone-elses-domain`.

So: port 25 accepts SMTP from the world, but only to store mail for *your* mailboxes. The daemon will sign outbound only from *your* configured senders. The two together rule out the open-relay class of abuse — an attacker cannot use aimx to launder mail, and cannot cause aimx to sign mail they composed under a domain you do not own.

### What the daemon does with an inbound message

When a message arrives, the daemon:

1. Parses the raw `.eml` via `mail-parser` and extracts headers, body, and attachments.
2. Runs DKIM / SPF / DMARC checks and records the result strings in the email's frontmatter (`dkim = "pass" | "fail" | "none"`, etc.). The three fields are always written, never omitted.
3. Writes the Markdown file to `inbox/<mailbox>/` atomically (temp file + rename).
4. Evaluates the **effective trust** for the mailbox (see [Trust evaluation](#trust-evaluation)) and gates `on_receive` hooks accordingly.

### Why the things missing from that list are missing

Notice what is *missing*: a spam filter, a rate limiter, a greylist, a bounce generator, a retry queue. Each of these has been deliberately left out:

- **No spam filter**, because the agent is the spam filter. An LLM reading a mailbox can reason about whether a message is spam, phishing, a cold outreach, or legitimate correspondence, far more accurately than a rule-based scorer. Dropping a message at the MTA layer hides it from the one tenant that can actually evaluate it. Storing it and flagging `trusted = "false"` is the right split.
- **No rate limiter**, because the design assumption is a single-operator host receiving mail at human-agent volumes, not a multi-tenant relay. Rate limiting has real operational costs (false positives from legitimate bursts, greylist ping-pong with senders that never retry) and the threat it addresses — inbound DoS — is better handled at the network edge with a firewall or a cloud WAF, if at all.
- **No greylisting**, because agents are supposed to react to mail in real time. Deferring a first-time sender for ten minutes is exactly the wrong behaviour for a mailbox whose purpose is to trigger an agent.
- **No bounce / DSN generation**, because delivery failures on aimx's inbound side (unknown recipient domain, malformed address) are reported synchronously in the SMTP dialogue as a 5xx reply. Standard senders handle that correctly. Generating asynchronous DSNs is how backscatter happens; declining to do it is the safer choice.
- **No retry queue on outbound**, because every outbound send is initiated by a live caller (an agent or the CLI), and that caller can retry with better context than a blind queue. 4xx failures are returned to the caller; 5xx failures are persisted with their reason. The calling agent always knows the outcome.

Each of those is an intentional trade, not a missing feature. The bounds that *do* exist are sized for accidental misuse, not a determined DoS attacker:

- `DEFAULT_MAX_BODY_SIZE` = 25 MB (matches what most SMTP peers advertise).
- `MAX_HEADER_LINE` = 8 KiB (to bound memory on malformed headers).
- `UDS_REQUEST_TIMEOUT` = 30 s per UDS connection.

If your threat model does include hostile volumes of inbound mail, front aimx with a firewall that understands port 25 or a small MTA that does greylisting.

## Outbound SMTP

Outbound flows through the UDS. Clients submit an unsigned message; the daemon validates the `From:` address, signs the message, and delivers it directly to the recipient's MX.

The `From:` validation is strict:

- The sender domain must be exactly `config.domain` (case-insensitive).
- The sender local part must resolve to a **concrete, non-wildcard** mailbox configured in `config.toml`.
- The catchall (`*@domain`) is **inbound-only** and is explicitly refused as an outbound sender.

This means a local user who submits mail over the socket can sign as any configured mailbox, but cannot invent new senders or hide behind the wildcard. A compromised agent can send under its own mailbox (that is the point) but cannot impersonate another configured mailbox unless your agents share a mailbox — a configuration choice you make, not a design flaw.

Delivery is direct: aimx resolves the recipient's MX records via hickory-resolver (falling back to A per RFC 5321) and connects to port 25. Opportunistic TLS is attempted on the outbound leg. There is no relay, no submission server, no queued retry. A 4xx transient failure is returned to the caller and is *not* persisted; the calling agent is expected to retry. A 5xx permanent failure is persisted to `sent/<mailbox>/` with `delivery_status = "failed"` and the reason in `delivery_details`. No DSN is ever generated.

This trades reliability for visibility. The calling agent always knows whether its message went out, failed, or deferred — no background queue quietly burns retries.

`aimx send` itself refuses to run as root (`src/send.rs`): it is a thin UDS client and does not need the privilege, so it actively rejects it. The check is small belt-and-suspenders: prevent an accidental `sudo aimx send` from smuggling the root-owned config or DKIM key into something it shouldn't.

## Unix domain socket

`/run/aimx/aimx.sock` is bound by `aimx serve` at mode `0o666`. Any local user can `connect()`.

This is deliberate, and the rationale is a direct consequence of the one-sentence summary at the top of this page: the socket is a *signing oracle for the configured mailboxes*, and nothing reachable through it can produce externally-verifiable fraud that the DKIM boundary wouldn't already block. Given that, tightening the socket mode buys us very little security and costs us real ergonomics.

The common case is "an agent running as a normal user wants to send mail". Agents are launched by humans (MCP clients, terminal sessions, cron jobs) under ordinary unprivileged UIDs. If we locked the socket down to root only, every `aimx send` call would need sudo, and every MCP client would need to spawn a privileged helper — a far worse security posture than a mode-`0666` socket that can only do a bounded set of things.

The alternatives were considered and rejected:

- **Socket mode `0660` with a shared group.** Requires every agent's UID to be in the same group as the daemon, which is fragile across reinstalls and user-management flows. Forgetting to add a new user to the group silently breaks their agent.
- **A local auth handshake over the socket.** A mutual-auth protocol on `AF_UNIX` adds failure modes and still doesn't stop a user from running a daemon of their own. The kernel already hands out peer credentials on UDS, but there is nothing aimx wants to *do* with them that the DKIM boundary isn't already doing better.
- **Requiring sudo for `aimx send`.** Would make the common case (an agent sends mail) gate on root, which defeats the purpose of having an unprivileged agent in the first place.

The combined result: any local process can submit to the socket, but the socket only accepts a narrow, validated verb set, and the daemon's own checks on `From:` + template shapes decide what actually happens. A malicious local user on the box can send mail as a mailbox you configured — which they could also do by logging into your agent's shell session and using the MCP tool, so gating the socket wouldn't have stopped them anyway. What they can't do is forge mail as a domain you don't own, run arbitrary commands, or read the DKIM key. That is the boundary the design is actually defending.

So the socket is the signing oracle. What it can do is deliberately narrow. The daemon parses a tagged `Request` enum and rejects unknown fields at parse time via `#[serde(deny_unknown_fields)]`. The accepted verbs:

- `SEND` — submit an unsigned RFC 5322 message for DKIM signing and MX delivery.
- `MARK-READ` / `MARK-UNREAD` — rewrite the `read` field in an email's frontmatter under a per-mailbox lock.
- `MAILBOX-CREATE` / `MAILBOX-DELETE` — add or remove a configured mailbox, hot-swapping the in-memory `Arc<Config>`.
- `HOOK-CREATE` — create a **template-bound** hook. The handler rejects any body that carries `cmd`, `run_as`, `timeout_secs`, `stdin`, or `dangerously_support_untrusted`. These fields are *template* properties — they live on `[[hook_template]]` entries the operator installs — and are unreachable through the socket. Tests (`hook_create_rejects_body_with_cmd`, `…_run_as`, `…_dangerously_support_untrusted`) assert the rejection explicitly.
- `HOOK-DELETE` — remove an existing hook, subject to origin protection (see below).

The explicit non-list matters as much as the list. There is no verb over the socket that:

- Writes raw shell strings to `config.toml`.
- Runs a subprocess under an arbitrary UID.
- Reads the DKIM key.
- Reloads config from a path chosen by the caller.

Combined with the 30 s per-connection timeout and 25 MB body cap, the socket is small, narrow, and auditable.

## Hooks: the two-tier model

Hooks are the one piece of aimx that runs external commands. They are also the one piece where the operator-versus-agent split is visible at the code level.

### Template hooks (agent-safe)

The operator installs a small set of pre-vetted command shapes once — during `aimx setup` or by hand in `config.toml`:

```toml
[[hook_template]]
name = "invoke-claude"
cmd = ["/usr/local/bin/claude", "-p", "{prompt}"]
params = ["prompt"]
run_as = "aimx-hook"
timeout_secs = 60
```

Agents bind to those shapes over MCP, filling declared `{placeholder}` slots with string values:

```json
{"template": "invoke-claude", "params": {"prompt": "File this email"}}
```

What makes this safe is a short list of guarantees the code enforces (from `src/hook_substitute.rs`):

1. **`cmd[0]` is never substituted.** A template whose binary slot contains a placeholder is rejected at config-load. The substitution function also refuses it at call time, as defense in depth.
2. **Values cannot introduce new argv entries.** Substitution is string-level — no shell, no whitespace splitting, no re-parsing. A parameter value of `"; rm -rf /"` lands as a single argv entry passed verbatim to the target binary; it cannot introduce redirections, pipes, or quote escapes.
3. **Values cannot carry NUL or ASCII control bytes.** The exceptions are `\t` and `\n`, to allow the occasional multiline prompt.
4. **Values are capped at `MAX_PARAM_BYTES` (8 KiB).** Large enough for a realistic agent prompt; small enough that nothing can fill the kernel's argv buffer.

The rendered argv vector is handed to `execvp` directly. `/bin/sh -c` is never invoked for template hooks, so there is no string-to-argv parsing step for an attacker to subvert.

Template hooks run as `aimx-hook`, a system user with no login shell and no home directory, created once by `aimx setup`. On systemd hosts the sandbox adds `ProtectSystem=strict`, `PrivateDevices=yes`, `NoNewPrivileges=yes`, and a `MemoryMax=256M` cap via `systemd-run`.

The substitution logic has no I/O and no locks, so it is fuzzed in isolation (`tests/hook_substitute_fuzz.rs`).

### Raw-cmd hooks (operator-only)

The operator keeps a full-power escape hatch. Raw-cmd hooks are shell strings written directly into `config.toml`, either by hand-editing or via `sudo aimx hooks create --cmd "..."`. They:

- Wrap the command in `/bin/sh -c` (so the full shell surface is available).
- May set `run_as = "root"` by hand-edit only. The CLI does not expose the flag.
- May set `dangerously_support_untrusted = true` (on `on_receive` hooks only), bypassing the trust gate described below.
- Are loaded by sending SIGHUP to the daemon.
- Run as `aimx-hook` by default unless `run_as` is overridden.

Crucially, raw-cmd hooks are **never** reachable from the socket, and therefore never from MCP. An agent that wants shell-level automation has to file a ticket with the operator, who decides whether to add a template for it. This is the intended friction.

### Origin protection

Every hook carries an `origin` tag: `operator` (hand-edited or created via `sudo aimx hooks create`) or `mcp` (created via the UDS). The daemon enforces the asymmetry:

- MCP-origin hooks can be deleted via MCP or the CLI.
- Operator-origin hooks can only be deleted via the CLI.

An agent cannot dismantle a policy hook the operator installed, even if it knows the name. See [Hook origin: MCP vs operator](hooks.md#hook-origin-mcp-vs-operator) in the hooks chapter for the full lifecycle.

### Trust gate for `on_receive`

`on_receive` hooks fire iff `email.trusted == "true"` **OR** the hook was configured with `dangerously_support_untrusted = true`. `after_send` hooks have no gate — they always fire (the outbound mail was your own; there is no untrusted input to gate on).

This means agents are only invoked on DKIM-verified mail from senders you listed, unless the operator explicitly opts out. See [Trust evaluation](#trust-evaluation) below for how `trusted` is computed.

## Trust evaluation

Every inbound email gets a three-valued `trusted` field in its frontmatter, computed by `trust::evaluate_trust`:

| Effective `trust` mode | Value             | Meaning                                         |
|------------------------|-------------------|-------------------------------------------------|
| `"none"` (default)     | `TrustedValue::None`  | No evaluation. Hooks do not fire by default. |
| `"verified"`           | `TrustedValue::True`  | Sender matches `trusted_senders` *and* DKIM passed. |
| `"verified"`           | `TrustedValue::False` | `"verified"` was set but the above did not hold. |
| unrecognised value     | `TrustedValue::False` | Fail-closed. A typo in config never grants trust. |

A few consequences worth naming:

- **Storage is unconditional.** Every inbound email is stored regardless of the `trusted` outcome. The field only gates *hook execution*, not delivery to disk.
- **Only DKIM gates trust.** SPF and DMARC are recorded in frontmatter (agents can read them, and hooks can key off them) but they are not part of the gate. SPF in particular is noisy once legitimate forwarding is involved; making it load-bearing would cause real mail to be marked untrusted.
- **Per-mailbox `trusted_senders` replaces the global list.** Setting it on a mailbox fully overrides the top-level default for that mailbox. There is no merge. An empty per-mailbox list means "nobody" for that mailbox.
- **The default is safe.** Fresh installs ship with `trust = "none"` on the global config, which yields `TrustedValue::None` on every email. Hooks that were authored for verified-sender workflows simply don't fire until the operator opts in to `"verified"` mode and populates `trusted_senders`.

## MCP server

`aimx mcp` is a stdio MCP server. It is launched by the agent's MCP client, runs as the agent's own user, and exits with the client. There is no long-running MCP daemon and no MCP-level auth.

This shapes the MCP trust model:

- **What MCP can do:** list / read / send / reply to mail, mark read/unread, create and delete mailboxes, list templates, and create / list / delete template-bound hooks. All mutating operations go through the daemon UDS.
- **What MCP cannot do:** submit raw-cmd hooks, change `run_as`, read the DKIM key, touch `/etc/aimx/`, or create a hook it isn't supposed to own (MCP-origin is stamped by the daemon, not the client).
- **There is no per-mailbox access control.** Every MCP tool call sees every mailbox. If you need two agents on one host with separate mailbox views, run two aimx instances on two hosts (or two IPs, two config dirs, two UDS paths).

The MCP server is in scope the same way any other local subject is: it runs as the agent's user, it submits to the world-writable socket, and the daemon enforces the same validation regardless of which client is speaking.

## Explicitly out of scope

These are not on a roadmap. They are non-goals.

1. **Per-user mailbox isolation.** Mailboxes are world-readable by design. Use Postfix or Stalwart if you need private inboxes per human user.
2. **SMTP AUTH / submission port 587.** aimx is not a submission MTA. Its outbound path is UDS → DKIM-sign → direct MX.
3. **IMAP / POP3 / webmail.** Agents read `.md` files via MCP or the filesystem. There is no mailbox server protocol.
4. **Reverse DNS (PTR).** Configured at your VPS provider, not by `aimx setup`. Optional but improves deliverability.
5. **UDS socket authorisation.** The socket is `0666`; the DKIM key is the boundary instead.
6. **Spam filtering, greylisting, inbound rate limits.** Front aimx with a firewall or small MTA if you need these.
7. **Retry queues, DSN generation.** Failures are agent-visible in real time, not queued behind the scenes.
8. **Detailed audit logging.** Every hook fire emits one structured line via `tracing`. That is the log. There is no separate audit file.

## Hardening the operator can do

The design leaves a few knobs you can tighten beyond the defaults:

- **Firewall :25 inbound** from known-bad netblocks. aimx does not do this itself.
- **Run on a dedicated host** if local users on the box cannot be trusted to sign mail as any configured mailbox.
- **Rotate the DKIM selector periodically.** See [How do I rotate the DKIM key without a delivery gap?](faq.md#how-do-i-rotate-the-dkim-key-without-a-delivery-gap).
- **Keep the template list minimal.** Every installed template is an argv shape you have authorised for agents to invoke.
- **Review hook-fire logs** after a new template lands: `journalctl -u aimx | grep hook_name=<name>`.
- **Switch `trust` to `"verified"`** and populate `trusted_senders` once you know which senders should trigger agents. Default `"none"` is safe but silent.
- **Set `run_as` explicitly** on raw-cmd hooks you hand-edit, even to the default `aimx-hook`. Explicit configs survive refactors better than implicit defaults.
