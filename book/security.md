# Security model

AIMX is a mail server for a single operator running AI agents on a host they control. This page describes the threat model, the trust boundaries that hold the design together, and what AIMX does not defend against.

AIMX has two boundaries: an **external boundary** at the root-only DKIM private key, and an **internal boundary** at server-side per-verb authorization keyed on the caller's `SO_PEERCRED` uid. The DKIM key is what lets a sender credibly impersonate your domain on the public internet — local file ACLs and socket modes only decide who on this host gets to ask the daemon to sign. Letting unprivileged subjects reach the signing oracle but never the key itself is what makes the rest of the design ergonomic.

## At a glance

- Single-operator, single-host. Every local user and agent on the box is inside the trust boundary.
- `aimx serve` runs as root and owns the DKIM key. Every other process (`aimx send`, `aimx mcp`, hook subprocesses) is unprivileged and cannot forge outbound signatures.
- `/run/aimx/aimx.sock` is `0666`, but every verb runs `auth::authorize` server-side using the caller's `SO_PEERCRED` uid. It is a signing oracle for the configured mailboxes plus an owner-bound CRUD surface; it cannot run arbitrary commands.
- Hook subprocesses always run as the mailbox owner. The daemon `setuid`s to `mailbox.owner_uid` before `exec` and `cmd` is `execvp`'d directly — no shell wrapper unless the operator spelled `["/bin/sh", "-c", "..."]`.
- DKIM, SPF, and DMARC results are recorded on every inbound email; only DKIM gates hook execution. Mail is always stored regardless of the result.
- No IMAP, POP3, webmail, SMTP AUTH, retry queue, DSN bounces, spam filtering, or rate limiting. Out of scope by design.

## Threat model

### Who you are

The operator: a single administrator of a VPS or home server with port 25 open to the internet. You own the host, the domain, and every local user on it. You install AIMX to give AI agents their own addresses on your domain.

### Who the adversaries are

AIMX is built to survive:

- **The public internet on port 25.** Unauthenticated senders, spammers, phishers, scanners. AIMX records the authentication results and lets the operator (or an agent via a hook) decide what to do with the mail.
- **A confused or compromised local agent.** A prompt-injected agent might try to exfiltrate mail, send spam under your domain, or install a shell-running hook. The design prevents the third outright and bounds the first two.
- **A curious local user.** Reads every mailbox they own and submits mail through the socket, but cannot sign as a wildcard, forge DKIM, mutate or hook another uid's mailbox, or run hooks as anyone but the owner.

### Who the adversaries are not

AIMX does not attempt to defend against:

- **Hostile code running as root.** Root can read the DKIM key, edit `config.toml`, and replace the daemon binary. Nothing below that privilege level can stop it.
- **Multiple mutually-distrustful humans on one host.** Mailbox isolation (`0700`, per-owner) prevents reads across uids, but any local user can submit to the UDS and act on mailboxes they own. If two users on one box cannot trust each other to operate the daemon, AIMX is the wrong tool — run [Postfix](https://www.postfix.org/) or [Stalwart](https://stalw.art/).

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

Every authorization decision in AIMX — CLI, UDS, or MCP — flows through the predicate in `src/auth.rs`. Root passes unconditionally; non-root callers are bound by the per-action rules below. The same predicate gates the host CLI verb, the UDS verb, and the MCP tool, so the model is symmetric end-to-end.

| Action            | Root        | Owner-gated (non-root) | Notes |
|-------------------|-------------|------------------------|-------|
| `MailboxRead`     | always      | caller uid must equal mailbox `owner_uid` | `aimx mailboxes show`, `email_*` MCP tools, `email_list`. |
| `MailboxSendAs`   | always      | caller uid must equal mailbox `owner_uid` | `aimx send` and `email_send` / `email_reply`. |
| `MarkReadWrite`   | always      | caller uid must equal mailbox `owner_uid` | `email_mark_read` / `email_mark_unread`. |
| `MailboxCreate`   | always (may pass `--owner` to create cross-uid) | caller may only create a mailbox owned by their own uid; the daemon synthesizes the owner from `SO_PEERCRED` and ignores any client-supplied `Owner:` header from non-root | `aimx mailboxes create`, `MAILBOX-CREATE` UDS, `mailbox_create` MCP. |
| `MailboxDelete`   | always      | caller uid must equal mailbox `owner_uid` | `aimx mailboxes delete` (incl. `--force`), `MAILBOX-DELETE` UDS, `mailbox_delete` MCP. |
| `HookCrud`        | always      | caller uid must equal mailbox `owner_uid` | `aimx hooks` create / list / delete, `HOOK-*` UDS verbs, `hook_*` MCP tools. Hooks always run as the mailbox owner. |
| `SystemCommand`   | always      | rejected | `setup`, `serve`, `uninstall`, `dkim-keygen`, `portcheck`. |

Mailbox create and delete are owner-gated, not root-gated: every local uid on the box is already inside the trust boundary, so `sudo` for spinning up a mailbox owned by yourself bought zero security and broke the daily workflow. The privilege-escalation defense is structural — for every non-root UDS request, the daemon resolves the owner identity from the kernel-validated `SO_PEERCRED` peer uid and ignores any client-supplied `Owner:` field.

Two cases stay operator-only:

- **Cross-uid creates.** Only root may pass `--owner <other-user>`. `mailbox_create` has no `owner` parameter, and the UDS handler discards `Owner:` headers from non-root callers.
- **The catchall.** Owned by the reserved `aimx-catchall` user (no shell, no resolvable login uid). Provisioned only by `aimx setup` and not exposed through any agent surface.

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

| Path                          | Mode    | Owner             | Purpose                                            |
|-------------------------------|---------|-------------------|----------------------------------------------------|
| `/etc/aimx/`                  | `0755`  | `root:root`       | Config + DKIM directory                            |
| `/etc/aimx/config.toml`       | `0640`  | `root:root`       | Mailboxes, trust policy, hooks                     |
| `/etc/aimx/dkim/private.key`  | `0600`  | `root:root`       | DKIM signing key                                   |
| `/etc/aimx/dkim/public.key`   | `0644`  | `root:root`       | Published in the `_domainkey` TXT record           |
| `/var/lib/aimx/`              | `0755`  | `root:root`       | Storage root (traversable; per-mailbox dirs are `0700`) |
| `/var/lib/aimx/inbox/<mailbox>/` | `0700`   | `<owner>:<owner>` | Inbound mail (owner = the configured Linux user) |
| `/var/lib/aimx/sent/<mailbox>/`  | `0700`   | `<owner>:<owner>` | Outbound copies (same owner) |
| `/run/aimx/`                  | `0755`  | `root:root`       | Runtime directory (provided by systemd/OpenRC)     |
| `/run/aimx/aimx.sock`         | `0666`  | `root:root`       | UDS signing oracle + owner-gated CRUD (world-writable, server-side `auth::authorize` per verb) |

Two choices are load-bearing: per-owner mailbox isolation, and the world-writable socket. Both are deliberate.

### `/etc/aimx/` and `/var/lib/aimx/`

`/etc/aimx/` holds secrets and policy: the DKIM private key, the mailbox list, the hook config. The whole tree is root-owned, the key is `0600`, the config is `0640`, and the daemon is the only process that reads it.

`/var/lib/aimx/` holds the mailboxes: Markdown files with TOML frontmatter, plus attachments as siblings in Zola-style bundles. Each mailbox is `<owner>:<owner> 0700`. The daemon enforces this on every write. Storage is deliberately flat text so agents can `ls`, `grep`, RAG-index, or read from a shell hook without an IMAP layer. Per-mailbox ownership scopes each agent to its own inbox while preserving flat-corpus ergonomics inside.

Secrets never flow outward, mail never flows into the secrets tree.

## Inbound SMTP

`aimx serve` listens on port 25 and accepts plain SMTP. STARTTLS is advertised and supported but **not required** — remote MTAs that speak plain SMTP are accepted, because that is still the norm for inter-MTA traffic. There is no `AUTH` extension, no `SMTP-AUTH`, and no rate limit. Any sender on the public internet can connect, complete an SMTP dialogue, and hand AIMX a message.

### Port 25 is open, but AIMX is not an open relay

Every MTA on the public internet listens on port 25 — RFC 5321 says that is where mail arrives. Exposing it is not a security posture, it is a prerequisite for receiving mail. What matters is what the listener does with what it receives.

An open relay accepts mail over SMTP and forwards it back out to a third-party destination, typically for spam. AIMX is not an open relay, by construction:

- **Inbound and outbound paths never cross.** Inbound writes the message to `inbox/<mailbox>/` and returns. The outbound path is triggered only by a `SEND` request on the UDS, submitted by a process already on the host with a validated `From:` that resolves to a configured local mailbox.
- **Inbound recipients must be yours.** Every `RCPT TO` is compared case-insensitively against `config.domain`. Unrelated domains, subdomains of `config.domain`, and malformed addresses are refused with `550 5.7.1 relay not permitted` before any storage. The catchall only ever covers unknown locals on *your* domain.
- **Outbound senders must be yours.** The daemon parses `From:` from the submitted body and refuses to sign anything whose domain is not exactly `config.domain` or whose local part does not resolve to a concrete (non-wildcard) configured mailbox. Even a local user with UDS access cannot cause AIMX to sign mail as `someone-else@someone-elses-domain`.

### What the daemon does with an inbound message

When a message arrives, the daemon:

1. Parses the raw `.eml` via `mail-parser` and extracts headers, body, and attachments.
2. Runs DKIM / SPF / DMARC checks and records the result strings in the email's frontmatter (`dkim = "pass" | "fail" | "none"`, etc.). The three fields are always written, never omitted.
3. Writes the Markdown file to `inbox/<mailbox>/` atomically (temp file + rename).
4. Evaluates the **effective trust** for the mailbox (see [Trust evaluation](#trust-evaluation)) and gates `on_receive` hooks accordingly.

### Inbound non-goals

AIMX has no spam filter, rate limiter, greylist, bounce generator, or retry queue:

- **No spam filter.** The agent is the spam filter. An LLM reading a mailbox can classify a message far more accurately than a rule-based scorer. Storing the mail and flagging `trusted = "false"` is the right split.
- **No rate limiter.** The design assumes a single-operator host at human-agent volumes, not a multi-tenant relay. Inbound DoS is better handled at the network edge.
- **No greylisting.** Agents are supposed to react in real time. Deferring a first-time sender ten minutes defeats the purpose of an agent mailbox.
- **No bounce / DSN generation.** Inbound failures (unknown recipient domain, malformed address) are reported synchronously as a 5xx in the SMTP dialogue. Async DSNs are how backscatter happens.
- **No outbound retry queue.** Every send is initiated by a live caller, who can retry with better context than a blind queue. 4xx is returned to the caller; 5xx is persisted with the reason.

Bounds that do exist are sized for accidental misuse, not a determined DoS attacker:

- `DEFAULT_MAX_BODY_SIZE` = 25 MB.
- `MAX_HEADER_LINE` = 8 KiB.
- `UDS_REQUEST_TIMEOUT` = 30 s per connection.

If your threat model includes hostile volumes of inbound mail, front AIMX with a firewall or a small greylisting MTA.

## Outbound SMTP

Outbound flows through the UDS. Clients submit an unsigned message; the daemon validates `From:`, signs, and delivers directly to the recipient's MX.

`From:` validation is strict:

- Domain must be exactly `config.domain` (case-insensitive).
- Local part must resolve to a concrete, non-wildcard mailbox in `config.toml`.
- The catchall is inbound-only and is explicitly refused as a sender.

A local user with socket access can sign as any mailbox they own but cannot invent new senders or hide behind the wildcard. A compromised agent can send under its own mailbox (that is the point) but cannot impersonate another configured mailbox unless they share one.

Delivery is direct: AIMX resolves the recipient's MX via hickory-resolver (falling back to A per RFC 5321) and connects to port 25. Opportunistic STARTTLS is attempted. There is no relay, no submission server, no queue. 4xx is returned to the caller and not persisted; the caller retries. 5xx is persisted to `sent/<mailbox>/` with `delivery_status = "failed"` and the reason in `delivery_details`. No DSN is generated. The trade is reliability for visibility — the calling agent always knows the outcome.

`aimx send` refuses root: it is a thin UDS client that doesn't need privilege, so it rejects it as belt-and-suspenders against an accidental `sudo aimx send`.

## Unix domain socket

`/run/aimx/aimx.sock` is bound at mode `0666`. Any local user can `connect()`.

This is deliberate. The DKIM key never leaves the daemon, and every per-mailbox action is owner-gated server-side — the daemon resolves the caller's uid via `SO_PEERCRED` and runs `auth::authorize` before any state work. Given those two boundaries, tightening the socket mode buys very little and costs real ergonomics: agents launched by humans run under unprivileged uids, and locking the socket to root would force every `aimx send` through `sudo` and every MCP client to spawn a privileged helper.

Alternatives considered and rejected:

- **Socket mode `0660` with a shared group.** Fragile across reinstalls and user-management flows. Forgetting to add a new user to the group silently breaks their agent.
- **A userland auth handshake.** Adds failure modes; the kernel already supplies peer credentials via `SO_PEERCRED`. A userland handshake on top would be redundant.
- **`sudo` for `aimx send`.** Gates the common case (an agent sends mail) on root, defeating the unprivileged agent.

A malicious local user can send mail as a mailbox they own — which they could also do via the agent's shell session, so gating the socket wouldn't have stopped them anyway. What they cannot do: forge mail under a domain you don't own, mutate or hook a mailbox owned by another uid, run hooks as anyone but the owner, run arbitrary commands as root, or read the DKIM key. That is the boundary the design defends.

The accepted verbs:

- `SEND` — submit an unsigned RFC 5322 message for DKIM signing and MX delivery.
- `MARK-READ` / `MARK-UNREAD` — rewrite the `read` field under a per-mailbox lock.
- `MAILBOX-CREATE` / `MAILBOX-DELETE` — add or remove a configured mailbox; hot-swap `Arc<Config>`.
- `HOOK-CREATE` — create a hook with a raw argv on a mailbox the caller owns. Stamped `origin = "mcp"`.
- `HOOK-DELETE` — remove an existing hook, subject to origin protection.
- `HOOK-LIST` / `MAILBOX-LIST` — read-only enumeration filtered to owned mailboxes / hooks; root sees all.
- `VERSION` — daemon build metadata.

The daemon parses a tagged `Request` enum with `#[serde(deny_unknown_fields)]`. There is no verb that writes raw shell strings to `config.toml`, runs subprocesses under arbitrary UIDs, reads the DKIM key, or reloads config from a caller-chosen path. Combined with the 30 s per-connection timeout and 25 MB body cap, the socket is small and auditable.

## Hooks and MCP

Hooks are the one piece of AIMX that runs external commands. Every hook is a raw argv attached to a mailbox; there is no template layer and no per-hook `run_as`. The trust boundary is the mailbox owner. See [Hooks & Trust](hooks.md) for the full model.

Hooks always exec as the mailbox owner (the daemon `setuid`s before `exec`); `cmd[0]` must be an absolute path; argv is `execvp`'d directly. The trust gate fires `on_receive` hooks only when `email.trusted == "true"` or the hook sets `fire_on_untrusted = true`. `after_send` hooks have no gate.

Every hook carries an `origin` tag: `operator` (hand-edited or via CLI direct-write fallback) or `mcp` (via the UDS). MCP-origin hooks can be deleted via MCP or CLI; operator-origin hooks can only be deleted via CLI. An agent cannot dismantle a policy hook the operator installed.

`aimx mcp` is a stdio MCP server launched by the client, running as the agent's own user. Every tool routes through the daemon UDS, which authorizes via `SO_PEERCRED` against the target mailbox's `owner_uid`. The MCP process never reads `/etc/aimx/`, never touches the DKIM key, and cannot mutate a mailbox it does not own. It cannot change which uid a hook runs as (no `run_as` knob exists) or forge an `origin` tag (the daemon stamps `mcp` itself). See [MCP Server: Per-user authorization](mcp.md#per-user-authorization).

## Explicitly out of scope

These are not on a roadmap. They are non-goals.

1. **Multi-user mailbox ACLs beyond owner/root.** Each mailbox is owned by one Linux user at mode `0700`; there is no shared-group readership and no fine-grained per-other-user ACL layer. Use Postfix or Stalwart if you need that.
2. **SMTP AUTH / submission port 587.** AIMX is not a submission MTA. Its outbound path is UDS → DKIM-sign → direct MX.
3. **IMAP / POP3 / webmail.** Agents read `.md` files via MCP or the filesystem. There is no mailbox server protocol.
4. **Reverse DNS (PTR).** Configured at your VPS provider, not by `aimx setup`. Optional but improves deliverability.
5. **Socket-mode-based UDS gating.** The socket is `0666` on purpose; per-verb authorization runs server-side on every request via `SO_PEERCRED` + `auth::authorize`. We don't tighten the socket mode itself.
6. **Spam filtering, greylisting, inbound rate limits.** Front aimx with a firewall or small MTA if you need these.
7. **Retry queues, DSN generation.** Failures are agent-visible in real time, not queued behind the scenes.
8. **Detailed audit logging.** Every hook fire emits one structured line via `tracing`. That is the log. There is no separate audit file.

## Hardening

Knobs you can tighten beyond the defaults:

- **Firewall port 25 inbound** from known-bad netblocks. AIMX does not do this itself.
- **Run on a dedicated host** if local users cannot be trusted to sign mail as any configured mailbox.
- **Rotate the DKIM selector periodically.** See [How do I rotate the DKIM key without a delivery gap?](faq.md#how-do-i-rotate-the-dkim-key-without-a-delivery-gap).
- **Keep the per-mailbox hook list minimal.** Review with `aimx hooks list --mailbox <name>` before adding more.
- **Review hook-fire logs** after a new hook lands: `journalctl -u aimx | grep hook_name=<name>`.
- **Switch `trust = "verified"`** and populate `trusted_senders` once you know which senders should trigger agents. Default `"none"` is safe but silent.
- **Pick the mailbox `owner` deliberately.** Hooks always run as `mailbox.owner_uid`. Pick the owner that matches what the hook needs to touch, not the user that's most convenient.
