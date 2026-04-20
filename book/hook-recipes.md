# Hook Recipes

> **Note on log paths.** The `/var/log/aimx/<agent>.log` paths in the recipes below are user-chosen destinations for hook script output — they are NOT AIMX's own logs. AIMX itself logs to journald (systemd) or the system logger (OpenRC); see [Troubleshooting — Where are the logs?](troubleshooting.md#where-are-the-logs) for details.

This chapter is the canonical cookbook for wiring AIMX hooks to every supported AI agent. Each section shows a copy-paste `config.toml` snippet, the agent-specific CLI flags that matter for non-interactive invocation, and notes on exit codes, logs, and gotchas.

For the underlying mechanics (match filters, template variables, trust policies, structured logs), see [Hooks & Trust](hooks.md). For installing the AIMX plugin/skill into an agent so its MCP tools are discoverable, see [Agent Integration](agent-integration.md).

## What counts as a hook recipe?

A hook recipe is a `[[mailboxes.<name>.hooks]]` block whose `cmd` invokes an AI agent non-interactively against an incoming email. The recipe pattern is always:

1. Email lands in the mailbox, AIMX writes the `.md` file to disk.
2. AIMX evaluates the trust gate. A hook fires iff `trusted == "true"` on the email OR the hook sets `dangerously_support_untrusted = true`.
3. AIMX fires the shell `cmd`. User-controlled header values are delivered as env vars (`AIMX_HOOK_ID`, `AIMX_FROM`, `AIMX_SUBJECT`, `AIMX_TO`, `AIMX_MAILBOX`, `AIMX_FILEPATH`); the two aimx-controlled placeholders `{id}` and `{date}` are substituted into the command string.
4. The agent reads the email body (typically by `cat`-ing `"$AIMX_FILEPATH"`) and takes action — replying, filing a ticket, updating a calendar, whatever.
5. Exit code is logged (one structured line per fire) but does not block delivery.

> **Why env vars, not `{from}`/`{subject}` substitution?** User-controlled fields like `From:` and `Subject:` can contain arbitrary bytes, including shell metacharacters (`$()`, backticks, `;`, quotes). Splicing them into the command string — even with shell-escape quoting — is fragile. Delivering them as env vars and expanding with `"$AIMX_FROM"` inside double quotes is safe no matter what the sender puts in the header.

Every recipe below assumes the agent binary (`claude`, `codex`, `opencode`, `gemini`, `goose`, `openclaw`, `aider`) is on the `PATH` of the user running `aimx serve`. Since `aimx serve` runs as a system user under systemd/OpenRC, you will typically either install the agent CLI system-wide or set an explicit absolute path in the command.

## Summary table

| Agent | MCP supported? | Hook CLI | Non-interactive / approval flag | Notes |
|-------|----------------|----------|---------------------------------|-------|
| Claude Code | Yes (`aimx agent-setup claude-code`) | `claude -p "<prompt>"` | `--dangerously-skip-permissions` (or `--permission-mode=bypassPermissions`) | `-p` / `--print` runs headless and exits; pipe the email via `$(cat "$AIMX_FILEPATH")`. |
| Codex CLI | Yes (`aimx agent-setup codex`) | `codex exec "<prompt>"` | `--dangerously-bypass-approvals-and-sandbox` (or `--full-auto`) | `exec` is the non-interactive subcommand. `--full-auto` enables auto-approval within sandbox; the bypass flag goes further. |
| OpenCode | Yes (`aimx agent-setup opencode`) | `opencode run "<prompt>"` | no confirmation prompts by design | `run` executes a single prompt and exits. Model selection via `-m/--model`. |
| Gemini CLI | Yes (`aimx agent-setup gemini`) | `gemini -p "<prompt>"` | `--yolo` (auto-accepts all actions) | `-p/--prompt` is the non-interactive flag. `--yolo` skips confirmations. |
| Goose | Yes (`aimx agent-setup goose`) | `goose run -t "<prompt>"` | `--no-session` (optional; sessions default on) | `-t/--text` takes an inline prompt; `-i/--instructions` reads from a file. |
| OpenClaw | Yes (`aimx agent-setup openclaw`) | `openclaw agent --message "<prompt>" --deliver --json` | `--deliver` routes the reply back through OpenClaw; `--json` produces a stable, scriptable output envelope | The `agent` subcommand is non-interactive. See [OpenClaw](#openclaw) for a complete recipe. |
| Aider | No (no MCP server) | `aider --message "<prompt>"` | `--yes-always` | Aider is a code-editing agent; recipes below pattern it as "take email, apply patch, commit." |

Every agent with an `aimx agent-setup <agent>` installer can ALSO be wired as a hook — MCP support and hook support are orthogonal. MCP gives the agent a way to read/send mail on demand; a hook is AIMX pushing an email into the agent when it arrives.

> **Flag drift warning.** CLI flags for every agent below were verified against each project's current `--help` output and public docs at the time of writing. Agent CLIs evolve fast — always run `<agent> --help` yourself before deploying a recipe to a production mailbox, and check the linked docs for current flag names.

## Claude Code

- Docs: <https://docs.claude.com/en/docs/claude-code/cli-reference>
- Non-interactive: `claude -p` (aliases: `--print`).
- Bypass permissions: `--dangerously-skip-permissions` or `--permission-mode=bypassPermissions`.
- Output: stdout by default; `--output-format text|json|stream-json` available.

### config.toml snippet

```toml
[mailboxes.inbox]
address = "inbox@agent.yourdomain.com"
trust = "verified"
trusted_senders = ["*@yourcompany.com"]

[[mailboxes.inbox.hooks]]
id = "claudeinbox1"
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

- `"$AIMX_FILEPATH"` is always safe — the shell quotes the value, so paths with spaces or unusual characters are handled correctly.
- `claude -p` exits when the turn completes; the hook finishes quickly.
- Redirect stdout and stderr to a log file because the hook runs detached under `aimx serve` — there is no TTY to print to.
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
id = "codextriage1"
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
- Approval: OpenCode's `run` mode does not prompt for confirmation on tool use — safe for hooks without a dedicated bypass flag.

### config.toml snippet

```toml
[mailboxes.research]
address = "research@agent.yourdomain.com"
trust = "verified"

[[mailboxes.research.hooks]]
id = "ocresearch01"
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
id = "geminines01"
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
id = "gooseops001"
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
- MCP server: `aimx agent-setup openclaw` emits an `openclaw mcp set aimx '<json>'` command you paste once to register AIMX's MCP tools with the OpenClaw gateway.
- Non-interactive agent invocation: `openclaw agent --message "<prompt>" --deliver --json`.

### config.toml snippet

```toml
[mailboxes.inbox]
address = "inbox@agent.yourdomain.com"
trust = "verified"
trusted_senders = ["*@yourcompany.com"]

[[mailboxes.inbox.hooks]]
id = "openclaw0001"
event = "on_receive"
cmd = '''
openclaw agent \
    --message "An email arrived at $(basename \"$AIMX_FILEPATH\"). Read it at \"$AIMX_FILEPATH\", summarize the sender's request, and respond via the aimx MCP tools if a reply is appropriate." \
    --deliver \
    --json \
    >> /var/log/aimx/openclaw.log 2>&1
'''
```

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
id = "aiderbugs01"
event = "on_receive"
cmd = '''
cd /srv/repos/myapp && \
aider --yes-always \
      --message "Bug report arrived by email. Read \"$AIMX_FILEPATH\", reproduce the issue if possible, apply a fix, and commit." \
      >> /var/log/aimx/aider.log 2>&1
'''
```

## `after_send` recipes

Send-side hooks run after AIMX resolves the MX delivery attempt. They cannot affect the send result — hooks are observability-only — but are ideal for audit logs, outbound notifications, or post-send bookkeeping.

### Append to an audit log

```toml
[[mailboxes.alice.hooks]]
id = "auditlog0001"
event = "after_send"
cmd = 'printf "%s %s %s %s\n" "$AIMX_SEND_STATUS" "$AIMX_TO" "$AIMX_SUBJECT" "$AIMX_HOOK_ID" >> /var/log/aimx/alice-sent.log'
```

### Page on failed sends

```toml
[[mailboxes.alerts.hooks]]
id = "failedpage01"
event = "after_send"
cmd = '''
if [ "$AIMX_SEND_STATUS" != "delivered" ]; then
  ntfy pub on-call "AIMX send to $AIMX_TO $AIMX_SEND_STATUS: $AIMX_SUBJECT"
fi
'''
```

### Notify on delivered marketing mail only

```toml
[[mailboxes.marketing.hooks]]
id = "marknotify01"
event = "after_send"
cmd = 'curl -fsS -X POST https://hooks.internal/marketing-sent -d "to=$AIMX_TO&status=$AIMX_SEND_STATUS"'
to = "*@customer-co.com"
```

## Operational tips

### Logging

Every recipe above redirects stdout and stderr to a log file because `aimx serve` runs detached. Without the redirect, the agent's output is lost. AIMX itself emits one structured log line per hook fire to journald:

```text
hook_id=<id> event=<on_receive|after_send> mailbox=<m> (email_id=<id>|message_id=<id>) exit_code=<n> duration_ms=<n>
```

Grep by `hook_id=<id>` (`journalctl -u aimx | grep hook_id=claudeinbox1`) to trace every fire of a specific hook.

### Exit codes

A non-zero exit from the hook command is logged to `aimx serve`'s stderr at `warn` but does NOT block delivery or the send. Email is always stored as a `.md` file. This is intentional: you do not want flaky agent CLIs to stall your mailbox.

### Concurrent hooks

If two emails arrive in rapid succession, `aimx serve` fires two hook shells in parallel. Agent CLIs that lock a resource (e.g. an Aider-managed git repo) can collide — serialise with `flock`:

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
