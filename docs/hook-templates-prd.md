# Hook Templates — Product Requirements Document

## 1. Overview

Hook templates let AI agents configure their own inbound/outbound mail automations through MCP, safely. The operator registers a small set of pre-vetted command shapes (e.g. "invoke Claude with prompt X", "POST to URL Y") once at setup time; agents then pick a template and fill declared parameters via the MCP `hook_create` tool. Raw-command hooks remain available to the operator via CLI, but they can never reach the daemon over a world-writable Unix socket.

This PRD also covers three supporting changes that together form the security story: a dedicated unprivileged service user (`aimx-hook`) that hooks drop privileges to before `exec`, a rename of the runtime socket from `send.sock` to `aimx.sock`, and tighter scoping of the existing `HOOK-CREATE` UDS verb.

**Tagline:** Chat with your agent to wire up mail automations, without handing it a shell.

## 2. Problem Statement

Today an operator who wants an agent to react to mail has two paths, both bad:

1. **Hand-edit `/etc/aimx/config.toml`** — add a `[[mailboxes.accounts.hooks.on_receive]]` block with a `cmd = "..."` field and restart the daemon. Works, but it's friction for a non-trivial command and breaks the "chat with your agent" UX that aimx is meant to deliver.
2. **Expose raw `HOOK-CREATE` over the world-writable UDS** — convenient for MCP, but any local user could then instruct the root-owned daemon to execute arbitrary shell as root on every inbound email. The DKIM-key isolation that protects `SEND` does not protect hooks; hooks are the attack surface.

The impact today: operators either avoid hooks (the most differentiated aimx feature) or accept an unsafe posture. The feature that was supposed to be the headline of agent-native mail is friction to set up and scary to expose.

**Who experiences this:** Agent operators who want self-configuring agents (personal productivity use cases — "file mail into the right folder", "reply with current system state", "route receipts to an accounting agent"), plus framework developers who want to ship agent bundles that assume MCP-level hook control.

**Impact of not solving:** Hooks remain operator-only, set up by hand. The "chat with your AI to wire things up" promise regresses. Alternatively, someone ships MCP hook-create with no authorization story and opens aimx installs to local privilege escalation.

## 3. Goals and Success Metrics

| Goal | Metric | Target |
|------|--------|--------|
| Agents can self-configure hooks via MCP | MCP `hook_create` works without operator intervention after one-time template install | Setting up a "file + reply" hook requires 0 `sudo` calls once templates are installed |
| No local privilege escalation via UDS | Hooks submitted over `aimx.sock` cannot invoke arbitrary binaries | MCP `hook_create` rejects any non-template payload; penetration review finds no bypass |
| Hook execution is sandboxed | Hook processes run as `aimx-hook`, not root | `ps -ef` during hook exec shows `aimx-hook` UID; attempts to read `/etc/aimx/dkim/private.key` fail with EACCES |
| Operator keeps full power | Raw-cmd hooks via CLI continue to work | `aimx hooks create --mailbox x --event on_receive --cmd "..."` still creates an arbitrary-command hook (after `sudo`) |
| Zero-migration rollout | Pre-launch, no backward compat | Config schema changes land in one release; no `v1 → v2` shim needed |

## 4. User Personas

### Agent Operator (primary, root)
- **Description:** Root user on the aimx box. Runs `aimx setup` once, configures domains + DKIM, decides which agents are trusted to self-configure.
- **Needs:** Control over which templates exist. Visibility into what hooks the agent has created on their behalf. Ability to create raw-cmd hooks when templates don't fit.
- **Context:** SSH'd in as root (or via `sudo`). Interacts with aimx via `aimx setup`, `aimx hooks …`, and hand-edits to `/etc/aimx/config.toml` when needed.

### AI Agent (secondary, non-root)
- **Description:** Claude Code / Codex / Goose / etc., running as the operator's unprivileged user, talking to `aimx mcp` over stdio.
- **Needs:** Discover what templates are available. Create hooks wired to those templates. List and delete hooks it owns.
- **Context:** The agent is triggered by a natural-language instruction ("when I get an email from my bank, file it and reply with my balance"). It must be able to wire that up end-to-end without the operator opening a second terminal.

### Agent Framework Developer (tertiary)
- **Description:** Builder shipping an agent plugin/skill that relies on aimx hooks.
- **Needs:** Predictable template names (`invoke-claude`, `webhook`) on any aimx install. Stable parameter schemas.
- **Context:** Writing `SKILL.md` or plugin manifests that instruct the agent to call `hook_create` with specific params.

## 5. User Stories

### P0 — Must Have

- As an **operator**, I want `aimx setup` to ask me (checkboxes) which hook templates to install so that only templates I want are registered.
- As an **operator**, I want the daemon to execute hook commands as a dedicated unprivileged user (`aimx-hook`) so that a buggy or malicious hook cannot read my DKIM key, rewrite `config.toml`, or touch other mailboxes.
- As an **operator**, I want `aimx hooks create --cmd "..."` to keep working so that I can still register arbitrary commands when no template fits.
- As an **agent**, I want a `hook_list_templates` MCP tool so that I can discover what templates are available on this install.
- As an **agent**, I want a `hook_create` MCP tool that accepts `(mailbox, event, template, params)` so that I can wire up an automation without operator intervention.
- As an **agent**, I want `hook_create` to refuse any payload containing a raw `cmd` field so that a prompt-injection attack cannot smuggle arbitrary shell past the template boundary.
- As an **agent**, I want `hook_list` and `hook_delete` MCP tools so that I can introspect and clean up the hooks I have created.
- As an **operator**, I want `aimx agent-setup <agent>` to print a one-line `sudo aimx hooks template-enable <name>` hint for any template that maps to the agent I just installed so that I know exactly how to complete the wiring.

### P1 — Should Have

- As an **operator**, I want `aimx hooks templates` (and `aimx hooks template-list`) CLI commands so that I can inspect which templates are registered without opening `config.toml`.
- As an **operator**, I want hook processes to have a default `timeout_secs` so that a hanging hook never starves the daemon's event loop.
- As an **operator**, I want the daemon to log a single-line structured record for every hook fire (template or raw-cmd, exit code, duration, stderr tail) so that I can debug without `strace`.
- As an **agent**, I want `hook_delete` to be restricted to hooks whose `origin = "mcp"` so that I cannot (accidentally or adversarially) nuke operator-authored hooks.
- As an **operator on a systemd box**, I want hook processes spawned via `systemd-run --uid=aimx-hook --property=ProtectSystem=strict --property=PrivateNetwork=... --property=MemoryMax=...` so that I get OS-level sandboxing for free.

### P2 — Nice to Have

- As an **operator**, I want `aimx hooks template-enable <name>` / `template-disable <name>` CLI commands so that I can toggle templates without hand-editing `config.toml`.
- As an **operator**, I want per-template `allowed_mailboxes` so that I can restrict a sensitive template (e.g. `webhook`) to specific mailboxes only.
- As an **operator**, I want a dry-run mode (`aimx hooks test <name>`) that fires a template with a synthetic email so that I can verify wiring before going live.

## 6. Functional Requirements

### 6.1 Template schema (`config.toml`)

Each template is a `[[hook_template]]` block in `/etc/aimx/config.toml`. The schema:

```toml
[[hook_template]]
name = "invoke-claude"                    # unique across templates; [a-z0-9-]+
description = "Pipe the received/sent email into Claude Code with a custom prompt."
cmd = ["/usr/local/bin/claude", "-p", "{prompt}"]
params = ["prompt"]                        # list of placeholder names that may appear in `cmd`
stdin = "email"                            # "email" | "email_json" | "none"
run_as = "aimx-hook"                       # fixed in v1; kept as a field for future flexibility
timeout_secs = 60                          # default 60, max 600
allowed_events = ["on_receive", "after_send"]  # optional; defaults to both
```

Substitution rules:

1. `{name}` placeholders are replaced **inside argv string values only** — never in `cmd[0]` (the binary path) and never as standalone argv entries (no parameter may introduce new argv entries via whitespace).
2. Every placeholder used in `cmd` must appear in `params`. Every `params` entry must be referenced by at least one placeholder. Validation fails loudly on mismatch.
3. `{event}`, `{mailbox}`, `{message_id}`, `{from}`, `{subject}` are **built-in placeholders** populated by the daemon at fire time; they do not need to be declared in `params`.
4. User-supplied param values are passed verbatim to the substituted argv entry. No shell, no `sh -c`, no string splitting. Placeholder substitution happens **after** the argv is parsed, so quotes and spaces in values cannot escape the value slot.
5. Values are UTF-8 strings. Binary/NUL is rejected.

### 6.2 Default templates (shipped in the binary, installed via `aimx setup` checkboxes)

| Template | Agents | `cmd` shape | `stdin` |
|----------|--------|-------------|---------|
| `invoke-claude` | claude-code | `["/usr/local/bin/claude", "-p", "{prompt}"]` | `email` |
| `invoke-codex` | codex | `["/usr/local/bin/codex", "-p", "{prompt}"]` | `email` |
| `invoke-opencode` | opencode | `["/usr/local/bin/opencode", "run", "{prompt}"]` | `email` |
| `invoke-gemini` | gemini | `["/usr/local/bin/gemini", "-p", "{prompt}"]` | `email` |
| `invoke-goose` | goose | `["/usr/local/bin/goose", "run", "--recipe", "{recipe}"]` | `email` |
| `invoke-openclaw` | openclaw | `["/usr/local/bin/openclaw", "run", "{prompt}"]` | `email` |
| `invoke-hermes` | hermes | `["/usr/local/bin/hermes", "run", "{prompt}"]` | `email` |
| `webhook` | generic | `["/usr/bin/curl", "-sS", "-X", "POST", "-H", "Content-Type: application/json", "--data-binary", "@-", "{url}"]` | `email_json` |

Exact binary paths and argv shapes are finalized during implementation by confirming each agent's current CLI surface.

The binary embeds all template definitions via `include_dir!` or equivalent, so `aimx setup` can write them without a separate asset download.

### 6.3 `aimx setup` interactive install

During `aimx setup`, after DKIM and systemd-unit writing, present a checkbox list:

```
[Hook Templates]
Hook templates let your agents create their own on_receive / after_send
automations via MCP, safely. Each template is a pre-vetted command shape;
agents can only fill declared parameters. Hook commands run as the
unprivileged 'aimx-hook' user, never as root.

Which templates should I enable? (space to toggle, enter to confirm)
 [x] invoke-claude     Pipe email into Claude Code with a prompt
 [ ] invoke-codex      Pipe email into Codex CLI with a prompt
 [ ] invoke-opencode   Pipe email into OpenCode with a prompt
 [ ] invoke-gemini     Pipe email into Gemini CLI with a prompt
 [ ] invoke-goose      Pipe email into a Goose recipe
 [ ] invoke-openclaw   Pipe email into OpenClaw with a prompt
 [ ] invoke-hermes     Pipe email into Hermes with a prompt
 [x] webhook           POST the email as JSON to a URL
```

Defaults: none checked (conservative). Operator explicitly opts in. Re-running `aimx setup` offers the same menu with current selections pre-ticked.

`aimx setup` also creates the `aimx-hook` system user if it does not exist:

```
useradd --system --no-create-home --shell /usr/sbin/nologin aimx-hook
```

(OpenRC boxes use the equivalent `adduser` flags; both paths are idempotent.)

### 6.4 `aimx agent-setup <agent>` template hint

`aimx agent-setup` stays a per-user command (refuses root, writes only to `$HOME`). It does **not** touch `config.toml` directly. It gains a new post-install hint: when the installed agent maps to a known template, print the exact command to activate it:

```
✓ Installed claude-code plugin to /Users/alice/.claude/plugins/aimx

To let Claude Code create its own hooks via MCP, enable the matching template
as root:

    sudo aimx hooks template-enable invoke-claude

(Already enabled if you ticked it during `aimx setup`.)
```

The hint is always printed; `agent-setup` does not try to detect whether the template is already enabled (that would require reading root-owned `/etc/aimx/config.toml`).

### 6.5 UDS protocol and socket rename

**Socket rename:** `/run/aimx/send.sock` → `/run/aimx/aimx.sock` (still mode `0666`, still owned `root:root`, still managed by systemd `RuntimeDirectory=aimx` or OpenRC `checkpath`). This is a breaking change on the wire; acceptable because aimx is pre-launch. All occurrences updated atomically in one release.

**`HOOK-CREATE` verb** (tightened):

```
AIMX/1 HOOK-CREATE\n
Mailbox: accounts\n
Event: on_receive\n
Template: invoke-claude\n
Name: accounts-auto-reply            (optional)
Content-Length: 137\n
\n
{"prompt": "You are the accounts agent. Read the email on stdin, file it into the right folder, and draft a reply with the current balance."}
```

The body is JSON with keys matching the template's declared `params`. The daemon:

1. Rejects any request whose body contains a `cmd`, `run_as`, `dangerously_support_untrusted`, `timeout_secs`, or `stdin` field. These are template properties, not hook properties.
2. Rejects any request that omits `Template:` or names an unknown / disabled template.
3. Rejects any `params` key not declared by the template; rejects missing required params.
4. Tags the resulting hook with `origin = "mcp"` when written to `config.toml`.

**Raw-cmd hook creation** stays in the CLI via `aimx hooks create --cmd "..."`. It does **not** use the UDS: it writes `config.toml` directly (requires `sudo`) and sends SIGHUP to the running `aimx serve` process for hot-reload. If `aimx serve` is not running, it writes the file and prints a restart hint.

Hooks written to `config.toml` carry an `origin` field:
- `origin = "operator"` — authored by root via CLI or hand-edit (default when field is absent)
- `origin = "mcp"` — created by MCP over `aimx.sock`

**`HOOK-DELETE` verb** (tightened): MCP may delete a hook only if its `origin = "mcp"`. Operator-origin hooks can only be removed via CLI (`sudo aimx hooks delete <name>` or `config.toml` edit). The daemon returns `ERR origin-protected` for cross-origin delete attempts on `aimx.sock`.

**Other verbs unchanged:** `SEND`, `MARK-READ`, `MARK-UNREAD`, `MAILBOX-CREATE`, `MAILBOX-DELETE` all continue to work on `aimx.sock` with existing semantics.

### 6.6 MCP tool surface

New `aimx mcp` tools:

1. **`hook_list_templates`** — returns the list of enabled templates. Each entry: `{ name, description, params, allowed_events }`. No auth, no arguments.
2. **`hook_create(mailbox, event, template, params, name?)`** — thin wrapper around the UDS `HOOK-CREATE` verb. Returns the effective hook name and the substituted argv (for confirmation in the agent's UI).
3. **`hook_list(mailbox?)`** — returns hooks visible to MCP: `{ name, mailbox, event, template, params, origin }`. Both operator- and MCP-origin hooks appear in the list so the agent can see the full picture, but `origin` distinguishes them.
4. **`hook_delete(name)`** — thin wrapper around the UDS `HOOK-DELETE` verb. Returns the daemon's error verbatim on `origin-protected` rejection.

Existing 9 MCP tools (`mailbox_*`, `email_*`) are unchanged.

### 6.7 Hook execution flow (daemon-side)

When an event fires for a hook with `origin = "mcp"` or any hook with `run_as = "aimx-hook"`:

1. Daemon resolves the template, substitutes placeholders into argv, and builds the final `Command`.
2. Daemon spawns the command:
   - **systemd box** (detected by `sd_notify` or `/run/systemd/system` presence): `systemd-run --uid=aimx-hook --gid=aimx-hook --property=ProtectSystem=strict --property=PrivateDevices=yes --property=NoNewPrivileges=yes --property=MemoryMax=256M --property=RuntimeMaxSec={timeout_secs} --collect --pipe -- <argv>`.
   - **OpenRC / fallback**: `posix_spawn` / `fork+exec` with manual `setgid(aimx-hook)` + `setuid(aimx-hook)` before `exec`. Wrap in a per-hook timeout (`SIGTERM` at `timeout_secs`, `SIGKILL` at `timeout_secs + 5`).
3. `stdin` policy per template:
   - `email` — pipe the raw `.md` (frontmatter + body) that ingest wrote.
   - `email_json` — pipe a JSON object `{ "frontmatter": {...}, "body": "..." }`.
   - `none` — close stdin immediately.
4. Capture stdout and stderr; truncate at 64 KiB each. Log one structured line at hook completion with `template`, `mailbox`, `event`, `hook_name`, `exit_code`, `duration_ms`, `stderr_tail`.
5. Non-zero exit is logged but does not stop delivery / ingest. After-send hooks with non-zero exit do not retry the send.

Raw-cmd hooks (no template) follow the same exec flow, with `cmd` from the hook itself and `run_as` defaulting to `aimx-hook`. An operator who really wants a raw-cmd hook to run as root can set `run_as = "root"` in `config.toml`; this is intentionally only possible by editing the file as root, not via CLI flag or UDS.

### 6.8 Trust interaction

Template hooks inherit the existing `on_receive` trust gate (`hook.should_fire_on_receive`). `dangerously_support_untrusted` is **not** an MCP-settable field — it can only be set on operator-origin hooks in `config.toml`. MCP-origin hooks always fire only on trusted inbound mail. Rationale: if the agent could opt itself into firing on untrusted mail, the template sandbox is the only thing standing between a spoofed email and `aimx-hook`-level shell.

## 7. Non-Functional Requirements

### 7.1 Security

- **Injection resistance:** placeholder values cannot introduce new argv entries, cannot escape the value slot, cannot appear in `cmd[0]`. No shell interpreter is invoked.
- **Privilege separation:** hook processes run as `aimx-hook` UID. `aimx-hook` has no login shell, no home directory, no group membership beyond its own. `aimx-hook` has read access to `/var/lib/aimx/<folder>/<mailbox>/` (via group `aimx-hook` on those directories) so stdin-piped content is legitimate; it has **no** access to `/etc/aimx/dkim/`, `/etc/aimx/tls/`, or any other mailbox's private data.
- **Socket perms:** `/run/aimx/aimx.sock` stays `0666` (any local user can submit mail — preserved from today). The authorization boundary is the verb surface, not the socket: MCP cannot reach raw-cmd hooks because no verb accepts them.
- **Resource caps:** default `timeout_secs = 60`, default `MemoryMax = 256M` (on systemd). Operator can tighten per template.
- **Audit trail:** every hook fire logs template name, mailbox, event, exit code, duration. `origin` field in `config.toml` lets the operator quickly audit which hooks the agent added.

### 7.2 Reliability

- Placeholder validation happens at **config load**, not at hook fire. A malformed template fails daemon startup (or config reload), not the first inbound email.
- `systemd-run` failure falls back to direct `fork+exec` with the same privilege-drop logic; the hook still fires, just without systemd's sandboxing.
- Hook timeout is enforced even if the template forgets to set one (daemon applies the 60s default).

### 7.3 Observability

- `aimx doctor` gains a "Hook templates" section listing enabled templates and per-template fire counts over the last 24 hours (read from service logs).
- `aimx logs` surfaces the structured hook-fire log line unchanged.

### 7.4 Compatibility

- Pre-launch: no migration path. Operators on pre-release builds must re-run `aimx setup` after upgrading to the release that ships this change; existing raw-cmd hooks remain valid and continue to work.
- No external API changes outside the UDS rename and new `HOOK-CREATE` fields; MCP clients that don't use the new tools are unaffected.

## 8. Technical Considerations

### 8.1 Affected modules

| Module | Change |
|--------|--------|
| `src/config.rs` | Add `HookTemplate` struct, `hook_templates: Vec<HookTemplate>` field on `Config`. Validate `params` ↔ placeholder consistency at load time. Add `origin` field to `Hook`. |
| `src/hook.rs` | Extend `Hook` with optional `template: Option<String>` and `params: BTreeMap<String, String>`. Build final argv from template + params at fire time. |
| `src/hook_handler.rs` | Tighten `HOOK-CREATE` body parsing: reject raw `cmd`, require `Template:`, validate params. Tag `origin = "mcp"`. Add `HOOK-DELETE` origin check. |
| `src/send_protocol.rs` | Update `HOOK-CREATE` `Request` variant to carry `template + params` instead of raw `cmd`. |
| `src/hook_client.rs` | CLI-side helpers: `submit_template_hook_create`. Raw-cmd CLI path bypasses UDS (writes `config.toml` + SIGHUP). |
| `src/hooks.rs` | `aimx hooks create` splits: with `--template`, goes through UDS; with `--cmd`, writes `config.toml` directly (requires root) and signals daemon. Add `aimx hooks templates` subcommand. |
| `src/cli.rs` | New subcommands: `hooks templates`, `hooks template-enable <name>`, `hooks template-disable <name>`. `hooks create` grows `--template` and `--param KEY=VAL` flags. |
| `src/mcp.rs` | New tools: `hook_list_templates`, `hook_create`, `hook_list`, `hook_delete`. |
| `src/setup.rs` | New "Hook Templates" checkbox section in `run_setup`. Creates `aimx-hook` system user. Embeds default template definitions. |
| `src/agent_setup.rs` | Post-install hint prints the `sudo aimx hooks template-enable <name>` line per installed agent. |
| `src/serve.rs` | Socket rename: `send.sock` → `aimx.sock`. Hook executor grows systemd-run path and `setuid` fallback. SIGHUP handler for config reload (for raw-cmd hook hot-reload). |
| `src/platform.rs` | Helper: `spawn_sandboxed(argv, stdin, run_as, timeout)` — picks `systemd-run` vs. `fork+exec` based on platform detection. |

### 8.2 systemd unit changes

The unit file at `/etc/systemd/system/aimx.service` needs two additions:

- `ExecStartPre=/usr/sbin/useradd --system --no-create-home --shell /usr/sbin/nologin aimx-hook` (or equivalent idempotent creation — probably better done in `aimx setup` so the unit stays simple).
- Ensure `RuntimeDirectory=aimx` remains (socket rename doesn't change the dir).

`aimx-hook` needs group-read on `/var/lib/aimx/` so `stdin = "email"` can succeed. Plan: `aimx setup` creates the `aimx-hook` user, then `chown root:aimx-hook /var/lib/aimx/{inbox,sent}` + `chmod g+rX` recursively.

### 8.3 MCP skill / primer updates

`agents/common/aimx-primer.md` and `agents/common/references/*.md` need a new section: "Creating hooks." It should document the four new tools, the template-first discipline ("call `hook_list_templates` first, never assume a template exists"), and example params for each built-in template.

### 8.4 Book documentation updates

Once shipped, these `book/` chapters need revisions (exact wording out of scope for this PRD, but we should know the hit list):

- `book/hooks.md` — new first section explaining the template model; raw-cmd hooks move below templates with a "power user" framing.
- `book/hook-recipes.md` — every recipe rewritten around templates where possible; raw-cmd recipes clearly labeled.
- `book/mcp.md` — new tool entries for `hook_list_templates`, `hook_create`, `hook_list`, `hook_delete`.
- `book/setup.md` — new "Hook Templates" checkbox section in the `aimx setup` walkthrough. `aimx-hook` user creation mentioned.
- `book/agent-integration.md` — per-agent section gains the `sudo aimx hooks template-enable` step.
- `book/configuration.md` — new `[[hook_template]]` schema reference; socket rename note.
- `book/cli.md` — `aimx hooks create` gains `--template` / `--param`; new `aimx hooks templates` / `template-enable` / `template-disable` subcommands; socket path changes.
- `book/troubleshooting.md` — common errors: `aimx-hook` user missing, template disabled, param validation failure, sandbox denied access.
- `book/faq.md` — new Q: "Why can my agent create hooks but not arbitrary shell commands?" (explains the template model).

## 9. Scope and Milestones

### In Scope (v1)

- `[[hook_template]]` schema + load-time validation.
- Default templates embedded in the binary (listed in §6.2).
- `aimx setup` interactive checkbox install; creation of `aimx-hook` user.
- Socket rename `send.sock` → `aimx.sock`.
- UDS `HOOK-CREATE` tightened to template-only; body carries `template + params`.
- UDS `HOOK-DELETE` tightened with `origin` check.
- CLI `aimx hooks create --template ... --param k=v`, plus preserved `--cmd` path (via `config.toml` edit + SIGHUP).
- CLI `aimx hooks templates` (list enabled templates).
- MCP `hook_list_templates`, `hook_create`, `hook_list`, `hook_delete`.
- Daemon hook executor with systemd-run sandbox on systemd; `setuid` fallback on OpenRC.
- Structured hook-fire log line.
- `aimx agent-setup` post-install template-enable hint.
- Trust gate: MCP-origin hooks always trusted-only (no `dangerously_support_untrusted` via MCP).
- Agent-facing primer / reference docs updated in `agents/common/`.

### Out of Scope (future consideration)

- `admin.sock` (root-only socket). Not needed for this feature; can be added later if other privileged verbs appear.
- `aimx hooks template-enable / template-disable` CLI commands (P2). For v1, operators enable templates only during `aimx setup`; toggling later = hand-edit `config.toml` + SIGHUP.
- Per-template `allowed_mailboxes` restriction.
- Dry-run / test mode (`aimx hooks test <name>`).
- Per-user / multi-tenant hook isolation. Single-operator stance stands; multi-user Unix explicitly out of scope.
- Custom operator-authored templates (i.e. operator writes their own `[[hook_template]]` block). v1 only ships the built-in templates; operator-authored templates are mechanically supported (the schema accepts them) but not documented as a supported path until we've hardened validation.
- `email_json` stdin format stabilization — v1 treats it as a best-effort JSON dump of the frontmatter; breaking changes allowed post-v1 if the format proves wrong.

### Milestones

| Milestone | Description | Key Deliverables |
|-----------|-------------|------------------|
| M1 — Schema & validation | `HookTemplate` struct, load-time validation, placeholder substitution | `src/config.rs` + `src/hook.rs` changes; unit tests for substitution edge cases (whitespace injection, placeholder in `cmd[0]`, unknown params) |
| M2 — Sandboxed executor | `spawn_sandboxed` helper; systemd-run path; `setuid` fallback; timeout enforcement; structured log | `src/platform.rs`; integration test on a systemd-less environment via `AIMX_TEST_*` env toggles |
| M3 — UDS & CLI wiring | Socket rename; `HOOK-CREATE` body schema change; `HOOK-DELETE` origin check; CLI `--template` flag; `config.toml` SIGHUP path for raw-cmd | End-to-end test: MCP creates hook → inbound email fires hook → logs show `aimx-hook` UID |
| M4 — Setup & agent-setup integration | Checkbox UI in `aimx setup`; `aimx-hook` user creation; `aimx agent-setup` post-install hint | Fresh-install integration test validates the full flow |
| M5 — MCP tools | `hook_list_templates`, `hook_create`, `hook_list`, `hook_delete`; skill / primer updates | MCP integration test against a live `aimx mcp` process |
| M6 — Docs | `book/` chapters revised per §8.4 | Docs land in the same release as the code |

## 10. Risks and Mitigations

| Risk | Likelihood | Impact | Mitigation |
|------|-----------|--------|------------|
| Placeholder substitution has an injection bug (value escapes its slot, introduces new argv entry) | Medium | Critical (local RCE as `aimx-hook`) | No shell invocation anywhere; argv is parsed before substitution; substitution only fills string slots; fuzz test with whitespace, quotes, NUL, `$(...)`, backticks, `;`, newlines. |
| `systemd-run` is not available (non-systemd box) and the `setuid` fallback is weaker than expected | Medium | High | Document the reduced guarantee; require `aimx-hook` user regardless of init system; apply `RLIMIT_NOFILE`, `RLIMIT_CPU`, `setrlimit` manually in the fork path; add a `chroot`-like fallback later if needed. |
| Operator doesn't know about the `sudo aimx hooks template-enable` step and agent's `hook_create` keeps failing | High | Low (UX only) | `agent-setup` post-install hint; MCP `hook_list_templates` returns empty list with a helpful message; daemon rejection of unknown template includes "run `sudo aimx hooks template-enable <name>`" in the error body. |
| Binary path in a template drifts (user has Claude at `/usr/bin/claude`, not `/usr/local/bin/claude`) | High | Medium (template silently fails) | Ship templates with the canonical distribution path; let operator override via `config.toml`; `aimx doctor` warns if a template's `cmd[0]` is not executable. |
| Agent creates many hooks via MCP and fills `config.toml` with noise | Medium | Low | `hook_list` surfaces all MCP-origin hooks; `hook_delete` works per-hook; future enhancement: rate-limit `HOOK-CREATE` per UDS connection. |
| `aimx-hook` user cannot read `/var/lib/aimx/` (setup fails to chown) | Medium | High (hooks can't see email stdin) | `aimx setup` explicitly chowns and chmods; `aimx doctor` verifies `aimx-hook` read access and surfaces a specific error if missing. |
| Socket rename breaks an already-deployed pre-launch install | Low (pre-launch) | Low | Release notes call out the rename; `aimx doctor` detects the old socket name and prints the migration command. |
| Operator edits `config.toml` while daemon is live and SIGHUP races with an in-flight `MAILBOX-CREATE` | Low | Medium | Existing per-mailbox lock hierarchy in `src/mailbox_locks.rs` already serializes config writes; SIGHUP reload uses the same path. |

## 11. Open Questions

1. **Operator-authored templates — supported or explicitly not?** Technically the schema accepts any `[[hook_template]]` block, so nothing stops an operator from writing their own. Question: do we document this path in `book/configuration.md` for v1, or intentionally leave it undocumented until validation is hardened? Leaning "undocumented in v1, supported in v1.1."
2. **`hook_list` visibility — should MCP see operator-origin hooks at all?** Arguments for: the agent has a clearer picture of the mailbox's behavior. Arguments against: information leakage (operator may have hooks the agent shouldn't know about). Leaning "show name + mailbox + event + origin, but not `cmd` for operator-origin hooks" — enough for the agent to not step on operator hooks, not enough to inspect them.
3. **`invoke-goose` stdin format**: goose recipes take YAML input, not raw email. Does the `email` stdin mode work, or do we need a `goose`-specific `stdin = "email_yaml"` encoding? Needs a real recipe test.
4. **Webhook template authentication**: should the `webhook` template support a header-params slot (e.g. `Authorization: Bearer {token}`)? The concern is that `{token}` in an HTTP header value is a fine substitution slot, but if we don't ship it, operators will reach for raw-cmd hooks just to do webhooks with auth.
5. **`aimx-hook` user on shared hosts**: what if the UID is already taken? Setup should probe with `id aimx-hook` first and skip `useradd` if the user exists; unclear whether we should fail loudly or silently if the existing user has a login shell.
6. **Hot-reload semantics for template changes**: if the operator enables a template via `sudo aimx hooks template-enable`, do we hot-reload the daemon, or require a restart? Hot-reload is user-friendlier; restart is simpler to reason about. Leaning hot-reload via the existing `ConfigHandle` swap.
7. **Do we need a separate `hook_disable(name)` MCP tool?** Or is `hook_delete` sufficient? If an agent wants to "pause" a hook while debugging, deleting and re-creating is stateful (loses the name). Probably defer to v1.1 unless a clear use case shows up.

---

*Status: draft — pending operator review.*
