# Security model

AIMX is a mail server for a single operator running AI agents on a host they control. This page describes the threat model it is built for, the trust boundaries that hold the design together, and — equally important — what AIMX deliberately does not defend against.

If you read only one sentence: **AIMX's external boundary is the root-only DKIM private key; its internal boundary is server-side per-verb authorization keyed on the caller's `SO_PEERCRED` uid, not filesystem ACLs and not the socket permissions**. Everything else on this page is a consequence of that split.

The reasoning is narrow: the only thing that lets an actor credibly impersonate your domain on the public internet is a DKIM signature that verifies against the TXT record you published. Local file ACLs and socket modes only decide who on this host gets to *ask* the daemon to sign. They do not, on their own, produce forgeable output. Letting unprivileged local subjects reach the signing oracle but never the key itself allows the surrounding system to be designed for ergonomics — per-owner mailbox isolation for agent privacy, a world-writable UDS for agent ergonomics with server-side `auth::authorize` on every verb — without weakening the one boundary that matters when the mail leaves the host. This is the single decision every other choice on this page defers to.

## At a glance

- AIMX is a **single-operator, single-host** SMTP server. It assumes one administrator and treats every local user and agent on that host as inside the trust boundary.
- `aimx serve` runs as root and owns the DKIM signing key. All other processes (`aimx send`, `aimx mcp`, hook subprocesses) are unprivileged and cannot forge outbound signatures.
- The Unix socket at `/run/aimx/aimx.sock` is intentionally world-writable (`0666`), but every verb runs `auth::authorize` server-side using the caller's `SO_PEERCRED` uid. It is a signing oracle for the configured mailboxes plus an owner-bound CRUD surface for mailboxes and hooks; it cannot be used to run arbitrary commands on the host.
- Hook subprocesses run as the mailbox's owner (the registering Linux user), never as root. The daemon `setuid`s to `mailbox.owner_uid` before `exec` and `cmd` is `execvp`'d directly — there is no shell wrapper unless the operator spelled `["/bin/sh", "-c", "..."]` themselves.
- DKIM / SPF / DMARC results are recorded in every inbound email's frontmatter but only DKIM gates hook execution. Mail is always stored, regardless of the authentication outcome.
- AIMX does not implement IMAP / POP3, webmail, SMTP AUTH, a retry queue, DSN bounces, spam filtering, or rate limiting. Those are out of scope by design, not on a roadmap.

## Threat model

### Who you are

The operator owns the host, the domain, and every local user on the box. You are the single administrator of a VPS or home server with port 25 open to the internet. You install AIMX to give one or more AI agents their own email addresses on your domain.

### Who the adversaries are

AIMX is built to survive the following:

- **The public internet on port 25.** Unauthenticated senders, spammers, phishers, and automated scanners will connect. AIMX accepts their mail, records the authentication results, and lets the operator (or an agent, via a hook) decide what to do with it.
- **A confused or compromised local agent.** An agent might follow a prompt-injected instruction and try to exfiltrate mail, send spam under your domain, or install a hook that runs arbitrary shell. The design prevents the third outright and limits the first two.
- **A curious local user.** A user on the host with no sudo rights can read every mailbox they own and submit mail through the world-writable socket, but cannot sign mail as a wildcard sender, forge DKIM, mutate or hook a mailbox owned by another uid, or run hooks as anyone other than the mailbox's owner.

### Who the adversaries are not

AIMX does **not** attempt to defend against:

- **Hostile code running as root on the same host.** Root can read the DKIM key, edit `config.toml`, and replace the daemon binary. Nothing below that privilege level can stop it.
- **Multiple mutually-distrustful humans sharing the box.** Each mailbox is `0700` and chowned to its configured Linux owner, so non-owners cannot read another mailbox's contents. But the trust boundary AIMX defends is *the host*, not *each user on the host*: any local uid can submit to the world-writable UDS, and the daemon's owner-binding only prevents that uid from acting on mailboxes it does not own — it does not stop them from creating their own mailbox or sending mail under one they do own. If two users on one host genuinely cannot trust each other to operate the daemon, AIMX is the wrong tool — run [Postfix](https://www.postfix.org/) or [Stalwart](https://stalw.art/) instead.

## Trust boundaries

The clearest view of AIMX is a table of who-can-reach-what:

| Subject            | Runs as               | Holds the DKIM key | Can submit to UDS         | Can edit `config.toml` |
|--------------------|-----------------------|--------------------|---------------------------|------------------------|
| `aimx serve`       | root                  | yes                | n/a (handles the socket)  | yes (via its own UDS)  |
| `aimx send`        | invoking user         | no                 | yes                       | no                     |
| `aimx mcp`         | the agent's user      | no                 | yes (subset of verbs)     | no                     |
| Hook subprocess    | mailbox owner (`setuid` to `mailbox.owner_uid`) | no | no (env-only) | no |
| Any local user     | their login UID       | no                 | yes (socket is `0666`)    | no                     |

The only subject that touches the DKIM key is the daemon. Every other subject that wants to send mail under your domain has to ask the daemon nicely, over the socket — and the daemon, not the caller, decides whether to sign.

### Per-action authorization

Every authorization decision in aimx — CLI, UDS, or MCP — flows through the
single predicate in `src/auth.rs`. Root passes every action unconditionally;
for any other caller, the per-action rules below apply. The same predicate
gates the corresponding host CLI verb, UDS verb, and MCP tool, so the model
is symmetric end-to-end.

| Action            | Root        | Owner-gated (non-root) | Notes |
|-------------------|-------------|------------------------|-------|
| `MailboxRead`     | always      | caller uid must equal mailbox `owner_uid` | Used by `aimx mailboxes show`, `email_*` MCP tools, `email_list`. |
| `MailboxSendAs`   | always      | caller uid must equal mailbox `owner_uid` | Used by `aimx send` and `email_send` / `email_reply`. |
| `MarkReadWrite`   | always      | caller uid must equal mailbox `owner_uid` | Used by `email_mark_read` / `email_mark_unread`. |
| `MailboxCreate`   | always (may pass `--owner` to create a mailbox owned by another uid) | caller may only create a mailbox owned by their own uid; the daemon synthesizes the owner from `SO_PEERCRED` and ignores any client-supplied `Owner:` header from non-root | Used by `aimx mailboxes create`, `MAILBOX-CREATE` UDS verb, and `mailbox_create` MCP tool. |
| `MailboxDelete`   | always      | caller uid must equal mailbox `owner_uid` | Used by `aimx mailboxes delete` (incl. `--force`), `MAILBOX-DELETE` UDS verb, and `mailbox_delete` MCP tool. |
| `HookCrud`        | always      | caller uid must equal mailbox `owner_uid` | Used by `aimx hooks` (create / list / delete), the `HOOK-CREATE` / `HOOK-LIST` / `HOOK-DELETE` UDS verbs, and the `hook_create` / `hook_list` / `hook_delete` MCP tools. Hooks always run as the mailbox owner — see [Hooks](hooks.md#mailbox-ownership--hook-authorization). |
| `SystemCommand`   | always      | rejected | Covers `setup`, `serve`, `uninstall`, `dkim-keygen`, `portcheck`. |

Two consequences of this table are worth naming explicitly. First, mailbox
create and delete are deliberately **owner-gated**, not root-gated: the
single-trust-boundary model treats every local uid on the box as inside
the trust boundary, so requiring `sudo` for an operator to spin up a
mailbox owned by themselves bought zero security and broke the daily
workflow. The privilege-escalation defense is structural — for every
non-root UDS request, the daemon resolves the owner identity from the
kernel-validated `SO_PEERCRED` peer uid and ignores any `Owner:` field a
client might supply. There is no path for a non-root caller to cause a
mailbox to be created with an owner other than their own uid.

Second, two cases stay operator-only by design and are not expressible
through any non-root surface:

- **Cross-uid creates.** Only root may pass `--owner <other-user>` on
  `aimx mailboxes create` and have the daemon honor it. The MCP tool
  `mailbox_create` does not accept an `owner` parameter at all, and the
  UDS handler discards client-supplied `Owner:` headers from non-root
  callers. To create a mailbox owned by a different uid (e.g. for a
  service account), an operator must run the CLI under `sudo`.
- **The catchall.** The catchall mailbox is owned by the reserved
  `aimx-catchall` system user, which has no shell and no resolvable
  login uid. It is provisioned during `aimx setup`, never by a regular
  CLI / MCP call, and is not exposed through the agent surface.

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

Everything AIMX creates on disk has a deliberate permission choice. The surprising ones are called out below the table.

| Path                          | Mode    | Owner             | Purpose                                            |
|-------------------------------|---------|-------------------|----------------------------------------------------|
| `/etc/aimx/`                  | `0755`  | `root:root`       | Config + DKIM directory                            |
| `/etc/aimx/config.toml`       | `0640`  | `root:root`       | Mailboxes, trust policy, hooks                     |
| `/etc/aimx/dkim/private.key`  | `0600`  | `root:root`       | DKIM signing key                                   |
| `/etc/aimx/dkim/public.key`   | `0644`  | `root:root`       | Published in the `_domainkey` TXT record           |
| `/var/lib/aimx/`              | `0755`  | `root:root`       | Storage root (traversable; per-mailbox dirs are `0700`) |
| `/var/lib/aimx/inbox/<mailbox>/` | `0700`   | `<owner>:<owner>` | Inbound mail for the mailbox (owner = the Linux user who owns it) |
| `/var/lib/aimx/sent/<mailbox>/`  | `0700`   | `<owner>:<owner>` | Outbound copies for the mailbox (same owner) |
| `/run/aimx/`                  | `0755`  | `root:root`       | Runtime directory (provided by systemd/OpenRC)     |
| `/run/aimx/aimx.sock`         | `0666`  | `root:root`       | UDS signing oracle + owner-gated CRUD (world-writable, server-side `auth::authorize` per verb) |

Two choices are load-bearing and deserve their own sections below: the **per-owner mailbox isolation** and the **world-writable socket**. Both are deliberate, and both have their escape hatches (run on a dedicated host, don't share the box with untrusted users).

### `/etc/aimx/` and `/var/lib/aimx/`

The two directories exist on purpose to hold opposite kinds of data.

`/etc/aimx/` holds **secrets and policy**: the DKIM private key that signs as your domain, the mailbox list, and the hook config. If any of that is writable or readable by a non-root process, the boundary collapses. So the whole tree is root-owned with the key at `0600`, the config at `0640`, and the daemon is the only process that opens it for read at startup.

`/var/lib/aimx/` holds **the mailboxes**: Markdown files with TOML frontmatter, one per email, plus their attachments as siblings in a Zola-style bundle. Each mailbox is chowned to its configured owner (`<owner>:<owner>`) at mode `0700`, so only that owner and root can read its contents. The daemon enforces this on every write (ingest, send, mark-read). The storage model is deliberately flat text — agents `ls`, `grep`, `head`, index with an LLM pipeline, or walk directly from a shell hook without going through an IMAP server or a mailbox API. `.md` + TOML frontmatter is specifically chosen because every LLM and every RAG tool already knows how to read it; there is no custom schema to learn and no decoder to write. Per-mailbox ownership keeps each agent scoped to its own inbox while preserving the flat-corpus ergonomics inside that inbox.

The boundary between these two directories is why the design holds: secrets never flow outward, mail never flows into the secrets tree.

## Inbound SMTP

`aimx serve` listens on port 25 and accepts plain SMTP. STARTTLS is advertised and supported but **not required** — remote MTAs that speak plain SMTP are accepted, because that is still the norm for inter-MTA traffic. There is no `AUTH` extension, no `SMTP-AUTH`, and no rate limit. Any sender on the public internet can connect, complete an SMTP dialogue, and hand AIMX a message.

### Port 25 is open, but AIMX is not an open relay

Every MTA on the public internet listens on port 25. That is where RFC 5321 says mail arrives. Exposing it is not a security posture; it is a prerequisite for receiving mail at all. The interesting question is *what the listener is willing to do* with what it receives.

An **open relay** is an MTA that accepts mail over SMTP and then forwards it back out to a third-party destination, typically in service of spam. AIMX is not an open relay, by construction:

- **Inbound and outbound paths never cross.** Inbound SMTP writes the message to `inbox/<mailbox>/` on local disk and returns. There is no code path from the SMTP listener to the outbound delivery layer. The outbound path is triggered only by a `SEND` request on the UDS — submitted by a process already on the host, with a validated `From:` that must resolve to a configured local mailbox.
- **Inbound recipients must be yours.** The recipient domain on every `RCPT TO` is compared case-insensitively against `config.domain` (see `recipient_domain_matches` in `src/smtp/session.rs`). Anything that is not an exact match — an unrelated domain, a subdomain of `config.domain`, or an address with no `@` at all — is refused at SMTP time with `550 5.7.1 relay not permitted` and never reaches storage. Subdomains are rejected on purpose: AIMX is one-domain-per-instance, so `sub.yourdomain.com` has to be its own AIMX install with its own DKIM key. The local-part-to-mailbox routing (including the `catchall` fallback for unknown local parts) only runs *after* the domain check passes, so `catchall` only ever covers unknown locals on *your* domain and is never the landing pad for relay attempts.
- **Outbound senders must be yours.** The daemon parses `From:` from the submitted body and refuses to sign anything whose domain is not exactly `config.domain`, and whose local part is not a concrete (non-wildcard) configured mailbox. Even a local user with access to the world-writable UDS cannot cause AIMX to sign mail as `someone-else@someone-elses-domain`.

So: port 25 accepts SMTP from the world, but only to store mail for *your* mailboxes. The daemon will sign outbound only from *your* configured senders. The two together rule out the open-relay class of abuse — an attacker cannot use AIMX to launder mail, and cannot cause AIMX to sign mail they composed under a domain you do not own.

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
- **No bounce / DSN generation**, because delivery failures on AIMX's inbound side (unknown recipient domain, malformed address) are reported synchronously in the SMTP dialogue as a 5xx reply. Standard senders handle that correctly. Generating asynchronous DSNs is how backscatter happens; declining to do it is the safer choice.
- **No retry queue on outbound**, because every outbound send is initiated by a live caller (an agent or the CLI), and that caller can retry with better context than a blind queue. 4xx failures are returned to the caller; 5xx failures are persisted with their reason. The calling agent always knows the outcome.

Each of those is an intentional trade, not a missing feature. The bounds that *do* exist are sized for accidental misuse, not a determined DoS attacker:

- `DEFAULT_MAX_BODY_SIZE` = 25 MB (matches what most SMTP peers advertise).
- `MAX_HEADER_LINE` = 8 KiB (to bound memory on malformed headers).
- `UDS_REQUEST_TIMEOUT` = 30 s per UDS connection.

If your threat model does include hostile volumes of inbound mail, front AIMX with a firewall that understands port 25 or a small MTA that does greylisting.

## Outbound SMTP

Outbound flows through the UDS. Clients submit an unsigned message; the daemon validates the `From:` address, signs the message, and delivers it directly to the recipient's MX.

The `From:` validation is strict:

- The sender domain must be exactly `config.domain` (case-insensitive).
- The sender local part must resolve to a **concrete, non-wildcard** mailbox configured in `config.toml`.
- The catchall (`*@domain`) is **inbound-only** and is explicitly refused as an outbound sender.

This means a local user who submits mail over the socket can sign as any configured mailbox, but cannot invent new senders or hide behind the wildcard. A compromised agent can send under its own mailbox (that is the point) but cannot impersonate another configured mailbox unless your agents share a mailbox — a configuration choice you make, not a design flaw.

Delivery is direct: AIMX resolves the recipient's MX records via hickory-resolver (falling back to A per RFC 5321) and connects to port 25. Opportunistic STARTTLS is attempted on the outbound leg. There is no relay, no submission server, no queued retry. A 4xx transient failure is returned to the caller and is *not* persisted; the calling agent is expected to retry. A 5xx permanent failure is persisted to `sent/<mailbox>/` with `delivery_status = "failed"` and the reason in `delivery_details`. No DSN is ever generated.

This trades reliability for visibility. The calling agent always knows whether its message went out, failed, or deferred — no background queue quietly burns retries.

`aimx send` itself refuses to run as root (`src/send.rs`): it is a thin UDS client and does not need the privilege, so it actively rejects it. The check is small belt-and-suspenders: prevent an accidental `sudo aimx send` from smuggling the root-owned config or DKIM key into something it shouldn't.

## Unix domain socket

`/run/aimx/aimx.sock` is bound by `aimx serve` at mode `0o666`. Any local user can `connect()`.

This is deliberate, and the rationale is a direct consequence of the one-sentence summary at the top of this page: nothing reachable through the socket can produce externally-verifiable fraud that the DKIM boundary wouldn't already block, and every per-mailbox action over the socket is owner-gated server-side (the daemon resolves the caller's uid via `SO_PEERCRED` and runs `auth::authorize` before doing anything). Given those two boundaries, tightening the socket mode itself buys very little extra security and costs real ergonomics.

The common case is "an agent running as a normal user wants to send mail". Agents are launched by humans (MCP clients, terminal sessions, cron jobs) under ordinary unprivileged UIDs. Locking the socket down to root only would force every `aimx send` call through sudo and every MCP client to spawn a privileged helper — a far worse security posture than a mode-`0666` socket that can only do a bounded set of things.

The alternatives were considered and rejected:

- **Socket mode `0660` with a shared group.** Requires every agent's UID to be in the same group as the daemon, which is fragile across reinstalls and user-management flows. Forgetting to add a new user to the group silently breaks their agent.
- **A userland auth handshake over the socket.** A mutual-auth protocol on `AF_UNIX` adds failure modes and still doesn't stop a user from running a daemon of their own. The kernel already hands out peer credentials on UDS, and AIMX consumes those directly via `SO_PEERCRED` to drive `auth::authorize` — a userland handshake on top would be redundant.
- **Requiring sudo for `aimx send`.** Would make the common case (an agent sends mail) gate on root, which defeats the purpose of having an unprivileged agent in the first place.

The combined result: any local process can submit to the socket, but the socket only accepts a narrow, validated verb set, and on every verb the daemon resolves the caller's uid via `SO_PEERCRED` and runs `auth::authorize` server-side before doing anything. A malicious local user on the box can send mail as a mailbox they own (or one root configured under their uid) — which they could also do by logging into that agent's shell session and using the MCP tool, so gating the socket wouldn't have stopped them anyway. What they can't do is forge mail as a domain you don't own, mutate or hook a mailbox owned by a different uid, run hooks as anyone other than the mailbox's owner, run arbitrary commands as root, or read the DKIM key. That is the boundary the design is actually defending.

So the socket is the signing oracle. What it can do is deliberately narrow. The daemon parses a tagged `Request` enum and rejects unknown fields at parse time via `#[serde(deny_unknown_fields)]`. The accepted verbs:

- `SEND` — submit an unsigned RFC 5322 message for DKIM signing and MX delivery.
- `MARK-READ` / `MARK-UNREAD` — rewrite the `read` field in an email's frontmatter under a per-mailbox lock.
- `MAILBOX-CREATE` / `MAILBOX-DELETE` — add or remove a configured mailbox, hot-swapping the in-memory `Arc<Config>`.
- `HOOK-CREATE` — create a hook with a raw argv (`cmd`) on a mailbox the caller owns. The daemon resolves the caller's uid via `SO_PEERCRED`, runs the central `auth::authorize` predicate (gating `Action::HookCrud`), validates the hook (no `dangerously_support_untrusted` over the wire), and stamps `origin = "mcp"` on the persisted hook. The hook always runs as the mailbox owner — there is no per-hook `run_as` knob, full stop.
- `HOOK-DELETE` — remove an existing hook, subject to origin protection (see below).
- `HOOK-LIST` — read-only enumeration of hooks the caller is allowed to see (every hook on a mailbox they own; root sees all). Mirrors `MAILBOX-LIST`'s shape.

The explicit non-list matters as much as the list. There is no verb over the socket that:

- Writes raw shell strings to `config.toml`.
- Runs a subprocess under an arbitrary UID.
- Reads the DKIM key.
- Reloads config from a path chosen by the caller.

Combined with the 30 s per-connection timeout and 25 MB body cap, the socket is small, narrow, and auditable.

## Hooks: ownership = authorization

Hooks are the one piece of AIMX that runs external commands. Every hook is a raw argv (`cmd`) attached to a mailbox; there is no template-hook layer and no per-hook `run_as` override. The trust boundary is the mailbox owner.

### Owner-gated CRUD, owner-uid execution

A hook can be created, listed, or deleted only by the mailbox's `owner` (or by root). The daemon resolves the caller's uid via `SO_PEERCRED` and runs the central `auth::authorize` predicate (gating `Action::HookCrud`) — the same predicate covers the CLI (`aimx hooks create | list | delete`), the UDS (`HOOK-CREATE` / `HOOK-DELETE` / `HOOK-LIST`), and MCP (`hook_create` / `hook_list` / `hook_delete`).

When a hook fires, the daemon spawns the subprocess and `setuid`s to the mailbox's `owner_uid` before `exec`. There is no per-hook `run_as` knob: a hook on `alice`'s mailbox runs as `alice`, full stop. To run a hook as root, an operator hand-edits `mailbox.owner = "root"` in `/etc/aimx/config.toml` — a path that requires root in the first place. Catchall hooks are forbidden at config-load time because the catchall user has no shell.

```toml
[[mailboxes.support.hooks]]
name = "support_notify"
event = "on_receive"
cmd = ["/usr/local/bin/notify", "{}"]
```

`cmd` is `execvp`'d directly. There is no shell wrapper unless the operator spells `cmd = ["/bin/sh", "-c", "..."]` explicitly. The argv is validated at config load: `cmd[0]` must be an absolute path, the array must be non-empty, and no shell-meta byte injection is possible because the argv never round-trips through a string-parser.

### Origin protection

Every hook carries an `origin` tag: `operator` (hand-edited or created via `aimx hooks create` on a `config.toml` direct-write fallback) or `mcp` (created via the UDS). The daemon enforces the asymmetry on `HOOK-DELETE`:

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

- **What MCP can do:** list / read / send / reply to mail, mark read/unread, create and delete mailboxes you own, and create / list / delete hooks on those mailboxes. All mutating operations go through the daemon UDS.
- **What MCP cannot do:** change which uid a hook runs as (hooks always run as the mailbox owner — there is no `run_as` knob anywhere), read the DKIM key, touch `/etc/aimx/`, mutate a mailbox or hook it does not own, or create a hook with a forged `origin` tag (MCP-origin is stamped by the daemon, not the client).
- **MCP is owner-gated, not unrestricted.** Every tool resolves the caller's uid via `SO_PEERCRED` over the daemon socket and the daemon's `auth::authorize` predicate gates every action against the target mailbox's `owner_uid`. An agent running as `alice` cannot list, read, send from, or hook on a mailbox owned by `bob`. Root is the only across-uid identity. If two agents need their own mailbox view on the same host, run them under different Linux users — the same kernel-validated boundary that everything else on this page rests on.

The MCP server is in scope the same way any other local subject is: it runs as the agent's user, it submits to the world-writable socket, and the daemon enforces the same validation regardless of which client is speaking.

## Explicitly out of scope

These are not on a roadmap. They are non-goals.

1. **Multi-user mailbox ACLs beyond owner/root.** Each mailbox is owned by one Linux user at mode `0700`; there is no shared-group readership and no fine-grained per-other-user ACL layer. Use Postfix or Stalwart if you need that.
2. **SMTP AUTH / submission port 587.** aimx is not a submission MTA. Its outbound path is UDS → DKIM-sign → direct MX.
3. **IMAP / POP3 / webmail.** Agents read `.md` files via MCP or the filesystem. There is no mailbox server protocol.
4. **Reverse DNS (PTR).** Configured at your VPS provider, not by `aimx setup`. Optional but improves deliverability.
5. **Socket-mode-based UDS gating.** The socket is `0666` on purpose; per-verb authorization runs server-side on every request via `SO_PEERCRED` + `auth::authorize`. We don't tighten the socket mode itself.
6. **Spam filtering, greylisting, inbound rate limits.** Front aimx with a firewall or small MTA if you need these.
7. **Retry queues, DSN generation.** Failures are agent-visible in real time, not queued behind the scenes.
8. **Detailed audit logging.** Every hook fire emits one structured line via `tracing`. That is the log. There is no separate audit file.

## Hardening the operator can do

The design leaves a few knobs you can tighten beyond the defaults:

- **Firewall :25 inbound** from known-bad netblocks. aimx does not do this itself.
- **Run on a dedicated host** if local users on the box cannot be trusted to sign mail as any configured mailbox.
- **Rotate the DKIM selector periodically.** See [How do I rotate the DKIM key without a delivery gap?](faq.md#how-do-i-rotate-the-dkim-key-without-a-delivery-gap).
- **Keep the per-mailbox hook list minimal.** Every hook is an argv that will run as the mailbox owner the moment the gate fires; review the existing hooks on a mailbox with `aimx hooks list --mailbox <name>` before adding more.
- **Review hook-fire logs** after a new hook lands: `journalctl -u aimx | grep hook_name=<name>`.
- **Switch `trust` to `"verified"`** and populate `trusted_senders` once you know which senders should trigger agents. Default `"none"` is safe but silent.
- **Pick the mailbox `owner` deliberately.** Hooks always run as `mailbox.owner_uid` — there is no per-hook `run_as` override. To run a hook as root, you must set `mailbox.owner = "root"` in `/etc/aimx/config.toml`, which already requires root to write. Pick the owner that matches what the hook needs to touch, not the user that's most convenient.
