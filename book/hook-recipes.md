# Hook Recipes

> **Note on log paths.** The `/var/log/aimx/<agent>.log` paths in the recipes below are user-chosen destinations for hook script output. They are NOT aimx's own logs. aimx itself logs to journald (systemd) or the system logger (OpenRC). See [Troubleshooting: Where are the logs?](troubleshooting.md#where-are-the-logs) for details.

This chapter is the canonical cookbook for wiring aimx hooks to every supported AI agent. Each section shows a copy-paste `config.toml` snippet, the agent-specific CLI flags that matter for non-interactive invocation, and notes on exit codes, logs, and gotchas.

For the underlying mechanics (names, env vars, trust policies, structured logs), see [Hooks & Trust](hooks.md). For installing the aimx plugin/skill into an agent so its MCP tools are discoverable, see [Agent Integration](agent-integration.md).

## Template-first: the easy path

For every agent with a bundled template (`invoke-claude`, `invoke-codex`, `invoke-opencode`, `invoke-gemini`, `invoke-goose`, `invoke-openclaw`, `invoke-hermes`, `webhook`), the recipe reduces to two steps:

1. Operator ticks the template in `aimx setup`. Once per box.
2. Agent or operator calls `hook_create` (MCP) or `aimx hooks create --template <name> --param <k=v>` (CLI). Per mailbox, per prompt.

Example — wire Claude Code to the `schedule` mailbox:

```bash
# One-time: enable the template
sudo aimx setup          # tick invoke-claude in the checkbox UI

# Per mailbox: bind the template
sudo aimx hooks create \
  --mailbox schedule \
  --event on_receive \
  --template invoke-claude \
  --param prompt="Handle this scheduling request."
```

The agent equivalent over MCP looks like:

```json
{"name": "hook_create", "arguments": {
  "mailbox": "schedule",
  "event": "on_receive",
  "template": "invoke-claude",
  "params": {"prompt": "Handle this scheduling request."}
}}
```

Template hooks run sandboxed under the template's `run_as` user (the registering mailbox owner, never root) and can't be abused by prompt injection — see [Hooks & Trust § Template hooks](hooks.md#template-hooks-recommended).

The raw-cmd recipes below are the **power-user path**: use them when you need a shell pipeline, an exit-code check, multiple output sinks, or a flag the template doesn't expose.

## What counts as a hook recipe?

A hook recipe is a `[[mailboxes.<name>.hooks]]` block whose `cmd` invokes an AI agent non-interactively against an incoming email. The recipe pattern is always:

1. Email lands in the mailbox, aimx writes the `.md` file to disk.
2. aimx evaluates the trust gate. A hook fires iff `trusted == "true"` on the email OR the hook sets `dangerously_support_untrusted = true`.
3. aimx fires the shell `cmd`. User-controlled header values are delivered as env vars (`AIMX_HOOK_NAME`, `AIMX_FROM`, `AIMX_SUBJECT`, `AIMX_TO`, `AIMX_MAILBOX`, `AIMX_FILEPATH`). The two aimx-controlled placeholders `{id}` and `{date}` are substituted into the command string.
4. The agent reads the email body (typically by `cat`-ing `"$AIMX_FILEPATH"`) and takes action: replying, filing a ticket, updating a calendar, whatever.
5. Exit code is logged (one structured line per fire) but does not block delivery.

> **Why env vars, not `{from}`/`{subject}` substitution?** User-controlled fields like `From:` and `Subject:` can contain arbitrary bytes, including shell metacharacters (`$()`, backticks, `;`, quotes). Splicing them into the command string, even with shell-escape quoting, is fragile. Delivering them as env vars and expanding with `"$AIMX_FROM"` inside double quotes is safe no matter what the sender puts in the header.

Every recipe below assumes the agent binary (`claude`, `codex`, `opencode`, `gemini`, `goose`, `openclaw`, `aider`) is on the `PATH` of the user running `aimx serve`. Since `aimx serve` runs as a system user under systemd/OpenRC, you will typically either install the agent CLI system-wide or set an explicit absolute path in the command.

## Summary table

| Agent | MCP supported? | Hook CLI | Non-interactive / approval flag | Notes |
|-------|----------------|----------|---------------------------------|-------|
| Claude Code | Yes (`aimx agents setup claude-code`) | `claude -p "<prompt>"` | `--dangerously-skip-permissions` (or `--permission-mode=bypassPermissions`) | `-p` / `--print` runs headless and exits. Pipe the email via `$(cat "$AIMX_FILEPATH")`. |
| Codex CLI | Yes (`aimx agents setup codex`) | `codex exec "<prompt>"` | `--dangerously-bypass-approvals-and-sandbox` (or `--full-auto`) | `exec` is the non-interactive subcommand. `--full-auto` enables auto-approval within sandbox. The bypass flag goes further. |
| OpenCode | Yes (`aimx agents setup opencode`) | `opencode run "<prompt>"` | no confirmation prompts by design | `run` executes a single prompt and exits. Model selection via `-m/--model`. |
| Gemini CLI | Yes (`aimx agents setup gemini`) | `gemini -p "<prompt>"` | `--yolo` (auto-accepts all actions) | `-p/--prompt` is the non-interactive flag. `--yolo` skips confirmations. |
| Goose | Yes (`aimx agents setup goose`) | `goose run -t "<prompt>"` | `--no-session` (optional. Sessions default on) | `-t/--text` takes an inline prompt. `-i/--instructions` reads from a file. |
| OpenClaw | Yes (`aimx agents setup openclaw`) | `openclaw agent --message "<prompt>" --deliver --json` | `--deliver` routes the reply back through OpenClaw. `--json` produces a stable, scriptable output envelope | The `agent` subcommand is non-interactive. See [OpenClaw](#openclaw) for a complete recipe. |
| Hermes | Yes (`aimx agents setup hermes`) | *(no headless CLI)* | n/a | Hermes has no shell-side invocation for dispatching a one-shot prompt. Integrate via MCP instead. See [Hermes](#hermes) for the recommended pattern. |
| Aider | No (no MCP server) | `aider --message "<prompt>"` | `--yes-always` | Aider is a code-editing agent. Recipes below pattern it as "take email, apply patch, commit." |

Every agent with an `aimx agents setup <agent>` installer can ALSO be wired as a hook. MCP support and hook support are orthogonal. MCP gives the agent a way to read/send mail on demand. A hook is aimx pushing an email into the agent when it arrives.

> **Flag drift warning.** CLI flags for every agent below were verified against each project's current `--help` output and public docs at the time of writing. Agent CLIs evolve fast. Always run `<agent> --help` yourself before deploying a recipe to a production mailbox, and check the linked docs for current flag names.

## Claude Code

- Docs: <https://docs.claude.com/en/docs/claude-code/cli-reference>
- Non-interactive: `claude -p` (aliases: `--print`).
- Bypass permissions: `--dangerously-skip-permissions` or `--permission-mode=bypassPermissions`.
- Output: stdout by default. `--output-format text|json|stream-json` available.

### config.toml snippet

```toml
[mailboxes.inbox]
address = "inbox@agent.yourdomain.com"
trust = "verified"
trusted_senders = ["*@yourcompany.com"]

[[mailboxes.inbox.hooks]]
name = "claudeinbox1"
event = "on_receive"
cmd = '''
claude -p "You received a new email. Read it and file a summary ticket.

Email path: $AIMX_FILEPATH
Subject: $AIMX_SUBJECT
From: $AIMX_FROM

Use the Read tool to open \"$AIMX_FILEPATH\", then call the appropriate MCP tool." \
  --permission-mode=bypassPermissions \
  >> /var/log/aimx/claude.log 2>&1
'''
```

- `"$AIMX_FILEPATH"` is always safe. The shell quotes the value, so paths with spaces or unusual characters are handled correctly.
- `claude -p` exits when the turn completes. The hook finishes quickly.
- Redirect stdout and stderr to a log file because the hook runs detached under `aimx serve`. There is no TTY to print to.
- Pair with `trust = "verified"` so only DKIM-passing allowlisted senders can steer Claude.

## Codex CLI

- Docs: <https://github.com/openai/codex> (see `codex exec --help`).
- Non-interactive: `codex exec "<prompt>"`.
- Bypass approvals: `--dangerously-bypass-approvals-and-sandbox` or `--full-auto` (auto-approve within the sandbox).


### config.toml snippet

```toml
[mailboxes.triage]
address = "triage@agent.yourdomain.com"
trust = "verified"

[[mailboxes.triage.hooks]]
name = "codextriage1"
event = "on_receive"
cmd = '''
codex exec "Triage this email and update the issue tracker.

Email file: $AIMX_FILEPATH
From: $AIMX_FROM
Subject: $AIMX_SUBJECT" \
  --full-auto \
  >> /var/log/aimx/codex.log 2>&1
'''
```

- `--full-auto` is the recommended default for hooks: Codex auto-approves tool use but stays inside its sandbox.

## OpenCode

- Docs: <https://opencode.ai/docs/cli/>
- Non-interactive: `opencode run "<prompt>"`.
- Approval: OpenCode's `run` mode does not prompt for confirmation on tool use. Safe for hooks without a dedicated bypass flag.

### config.toml snippet

```toml
[mailboxes.research]
address = "research@agent.yourdomain.com"
trust = "verified"

[[mailboxes.research.hooks]]
name = "ocresearch01"
event = "on_receive"
cmd = '''
opencode run "A new research email arrived. Read \"$AIMX_FILEPATH\" and append a digest entry to docs/digest.md.

From: $AIMX_FROM
Subject: $AIMX_SUBJECT" \
  >> /var/log/aimx/opencode.log 2>&1
'''
```

## Gemini CLI

- Docs: <https://github.com/google-gemini/gemini-cli> (see `gemini --help`).
- Non-interactive: `gemini -p "<prompt>"` (alias `--prompt`).
- Auto-approve: `--yolo` (accepts all tool-use confirmations).

### config.toml snippet

```toml
[mailboxes.notes]
address = "notes@agent.yourdomain.com"
trust = "verified"

[[mailboxes.notes.hooks]]
name = "geminines01"
event = "on_receive"
cmd = '''
gemini -p "A new note arrived by email. Read \"$AIMX_FILEPATH\" and file it into my notes." \
  --yolo \
  >> /var/log/aimx/gemini.log 2>&1
'''
```

## Goose

- Docs: <https://block.github.io/goose/docs/guides/headless-goose>
- Non-interactive: `goose run -t "<prompt>"`.

### config.toml snippet

```toml
[mailboxes.ops]
address = "ops@agent.yourdomain.com"
trust = "verified"

[[mailboxes.ops.hooks]]
name = "gooseops001"
event = "on_receive"
cmd = '''
goose run -t "Ops email arrived. Read \"$AIMX_FILEPATH\", check if action is required, and page on-call if so.

Subject: $AIMX_SUBJECT
From: $AIMX_FROM" \
  >> /var/log/aimx/goose.log 2>&1
'''
```

## OpenClaw

- Docs: <https://docs.openclaw.ai/>
- MCP server: `aimx agents setup openclaw` emits an `openclaw mcp set aimx '<json>'` command you paste once to register aimx's MCP tools with the OpenClaw gateway.
- Non-interactive agent invocation: `openclaw agent --message "<prompt>" --deliver --json`.

### config.toml snippet

```toml
[mailboxes.inbox]
address = "inbox@agent.yourdomain.com"
trust = "verified"
trusted_senders = ["*@yourcompany.com"]

[[mailboxes.inbox.hooks]]
name = "openclaw0001"
event = "on_receive"
cmd = '''
openclaw agent \
    --message "An email arrived at $(basename \"$AIMX_FILEPATH\"). Read it at \"$AIMX_FILEPATH\", summarize the sender's request, and respond via the aimx MCP tools if a reply is appropriate." \
    --deliver \
    --json \
    >> /var/log/aimx/openclaw.log 2>&1
'''
```

## Hermes

- Docs: <https://hermes-agent.nousresearch.com/>
- Shell-side invocation: Hermes does not currently expose a headless CLI for dispatching a one-shot prompt (the `hermes mcp serve` subcommand runs Hermes *as* an MCP server, the opposite direction). Hook-driven agent dispatch therefore uses MCP on the inbound side.

### Recommended pattern

Register aimx as an MCP server inside Hermes (`aimx agents setup hermes` plus the YAML snippet it prints. See [Agent Integration: Hermes](agent-integration.md#hermes)). Inside Hermes, use the aimx skill's `email_list` / `email_read` tools to inspect the inbox on demand.

For shell-side notifications on new mail (so Hermes operators know there is mail to look at), wire a simple non-agent `on_receive` hook:

```toml
[mailboxes.hermes]
address = "hermes@agent.yourdomain.com"
trust = "verified"

[[mailboxes.hermes.hooks]]
# name is optional — a stable 12-char hex id is derived from event+cmd if omitted
event = "on_receive"
cmd = 'ntfy pub hermes-mail "New email: $AIMX_SUBJECT from $AIMX_FROM"'
dangerously_support_untrusted = true
```

When Hermes grows a headless `--message` / `exec`-style CLI, add it here mirroring the Claude Code or Codex recipe.

## Aider

- Docs: <https://aider.chat/docs/usage.html>
- Non-interactive: `aider --message "<prompt>"` (runs once and exits).
- Auto-approve: `--yes-always` (skips all confirmation prompts).


### config.toml snippet

```toml
[mailboxes.bugs]
address = "bugs@agent.yourdomain.com"
trust = "verified"
trusted_senders = ["*@yourcompany.com"]

[[mailboxes.bugs.hooks]]
name = "aiderbugs01"
event = "on_receive"
cmd = '''
cd /srv/repos/myapp && \
aider --yes-always \
      --message "Bug report arrived by email. Read \"$AIMX_FILEPATH\", reproduce the issue if possible, apply a fix, and commit." \
      >> /var/log/aimx/aider.log 2>&1
'''
```

## `after_send` recipes

Send-side hooks run after aimx resolves the MX delivery attempt. They cannot affect the send result (hooks are observability-only) but are ideal for audit logs, outbound notifications, or post-send bookkeeping.

### Append to an audit log

```toml
[[mailboxes.alice.hooks]]
name = "auditlog0001"
event = "after_send"
cmd = 'printf "%s %s %s %s\n" "$AIMX_SEND_STATUS" "$AIMX_TO" "$AIMX_SUBJECT" "$AIMX_HOOK_NAME" >> /var/log/aimx/alice-sent.log'
```

### Page on failed sends

```toml
[[mailboxes.alerts.hooks]]
name = "failedpage01"
event = "after_send"
cmd = '''
if [ "$AIMX_SEND_STATUS" != "delivered" ]; then
  ntfy pub on-call "aimx send to $AIMX_TO $AIMX_SEND_STATUS: $AIMX_SUBJECT"
fi
'''
```

### Notify only on matching recipients (shell guard)

Filter fields on hooks have been removed — do recipient/subject matching in the `cmd` itself with a shell guard.

```toml
[[mailboxes.marketing.hooks]]
name = "marknotify01"
event = "after_send"
cmd = '''
case "$AIMX_TO" in
  *@customer-co.com)
    curl -fsS -X POST https://hooks.internal/marketing-sent \
         -d "to=$AIMX_TO&status=$AIMX_SEND_STATUS"
    ;;
esac
'''
```

## Operational tips

### Logging

Every recipe above redirects stdout and stderr to a log file because `aimx serve` runs detached. Without the redirect, the agent's output is lost. aimx itself emits one structured log line per hook fire to journald:

```text
hook_name=<name> event=<on_receive|after_send> mailbox=<m> (email_id=<id>|message_id=<id>) exit_code=<n> duration_ms=<n>
```

Grep by `hook_name=<name>` (`journalctl -u aimx | grep hook_name=claudeinbox1`) to trace every fire of a specific hook.

### Exit codes

A non-zero exit from the hook command is logged to `aimx serve`'s stderr at `warn` but does NOT block delivery or the send. Email is always stored as a `.md` file. This is intentional: you do not want flaky agent CLIs to stall your mailbox.

### Concurrent hooks

If two emails arrive in rapid succession, `aimx serve` fires two hook shells in parallel. Agent CLIs that lock a resource (e.g. an Aider-managed git repo) can collide. Serialise with `flock`:

```bash
flock /tmp/aider-myapp.lock aider --yes-always --message "..." ...
```

### Testing a recipe locally

Use the integration test fixture path to simulate a real delivery without a live SMTP exchange:

```bash
aimx --data-dir /tmp/aimx-test ingest catchall@agent.example.com \
     < tests/fixtures/plain.eml
```

Check your log file and confirm the agent ran with the expected template values.

---

Next: [Hooks & Trust](hooks.md) | [Agent Integration](agent-integration.md) | [MCP Server](mcp.md)
