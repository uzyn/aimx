# Channel Recipes

> **Note on log paths.** The `/var/log/aimx/<agent>.log` paths in the recipes below are user-chosen destinations for trigger script output — they are NOT AIMX's own logs. AIMX itself logs to journald (systemd) or the system logger (OpenRC); see [Troubleshooting — Where are the logs?](troubleshooting.md#where-are-the-logs) for details.

This chapter is the canonical cookbook for wiring AIMX's channel triggers to every supported AI agent. Each section shows a copy-paste `config.toml` snippet, the agent-specific CLI flags that matter for non-interactive invocation, and notes on exit codes, logs, and gotchas.

For the underlying mechanics (match filters, template variables, trust policies), see [Channel Rules & Trust](channels.md). For installing the AIMX plugin/skill into an agent so its MCP tools are discoverable, see [Agent Integration](agent-integration.md).

## What counts as a channel-trigger recipe?

A channel-trigger recipe is a `[[mailboxes.<name>.on_receive]]` block whose `command` invokes an AI agent non-interactively against an incoming email. The recipe pattern is always:

1. Email lands in the mailbox, AIMX writes the `.md` file to disk.
2. AIMX fires the shell `command`. User-controlled header values are delivered as env vars (`AIMX_FROM`, `AIMX_SUBJECT`, `AIMX_TO`, `AIMX_MAILBOX`, `AIMX_FILEPATH`); the two aimx-controlled placeholders `{id}` and `{date}` are substituted into the command string.
3. The agent reads the email body (typically by `cat`-ing `"$AIMX_FILEPATH"`) and takes action — replying, filing a ticket, updating a calendar, whatever.
4. Exit code is logged; a non-zero exit does not block delivery.

> **Why env vars, not `{from}`/`{subject}` substitution?** User-controlled fields like `From:` and `Subject:` can contain arbitrary bytes, including shell metacharacters (`$()`, backticks, `;`, quotes). Splicing them into the command string — even with shell-escape quoting — is fragile. Delivering them as env vars and expanding with `"$AIMX_FROM"` inside double quotes is safe no matter what the sender puts in the header. The legacy `{from}`, `{subject}`, `{to}`, `{mailbox}`, `{filepath}` placeholders are rejected at config-load time with a migration hint.

Every recipe below assumes the agent binary (`claude`, `codex`, `opencode`, `gemini`, `goose`, `openclaw`, `aider`) is on the `PATH` of the user running `aimx serve`. Since `aimx serve` runs as a system user under systemd/OpenRC, you will typically either install the agent CLI system-wide or set an explicit absolute path in the command.

## Summary table

| Agent | MCP supported? | Channel-trigger CLI | Non-interactive / approval flag | Notes |
|-------|----------------|---------------------|---------------------------------|-------|
| Claude Code | Yes (`aimx agent-setup claude-code`) | `claude -p "<prompt>"` | `--dangerously-skip-permissions` (or `--permission-mode=bypassPermissions`) | `-p` / `--print` runs headless and exits; pipe the email via `$(cat {filepath})`. |
| Codex CLI | Yes (`aimx agent-setup codex`) | `codex exec "<prompt>"` | `--dangerously-bypass-approvals-and-sandbox` (or `--full-auto`) | `exec` is the non-interactive subcommand. `--full-auto` enables auto-approval within sandbox; the bypass flag goes further. |
| OpenCode | Yes (`aimx agent-setup opencode`) | `opencode run "<prompt>"` | no confirmation prompts by design | `run` executes a single prompt and exits. Model selection via `-m/--model`. |
| Gemini CLI | Yes (`aimx agent-setup gemini`) | `gemini -p "<prompt>"` | `--yolo` (auto-accepts all actions) | `-p/--prompt` is the non-interactive flag. `--yolo` skips confirmations. |
| Goose | Yes (`aimx agent-setup goose`) | `goose run -t "<prompt>"` | `--no-session` (optional; sessions default on) | `-t/--text` takes an inline prompt; `-i/--instructions` reads from a file. |
| OpenClaw | Yes (`aimx agent-setup openclaw`) | `openclaw agent --message "<prompt>" --deliver --json` | `--deliver` routes the reply back through OpenClaw; `--json` produces a stable, scriptable output envelope | The `agent` subcommand is non-interactive. See [OpenClaw](#openclaw) for a complete recipe. |
| Aider | No (no MCP server) | `aider --message "<prompt>"` | `--yes-always` | Aider is a code-editing agent; recipes below pattern it as "take email, apply patch, commit." |

All six agents with an `aimx agent-setup <agent>` installer can ALSO be wired as channel triggers — MCP support and channel-trigger support are orthogonal. MCP gives the agent a way to read/send mail on demand; a channel trigger is AIMX pushing an email into the agent when it arrives.

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

[[mailboxes.inbox.on_receive]]
type = "cmd"
command = '''
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
- `claude -p` exits when the turn completes; the trigger finishes quickly.
- Redirect stdout and stderr to a log file because the trigger runs detached under `aimx serve` — there is no TTY to print to.
- Pair with `trust = "verified"` so only DKIM-passing senders can steer Claude.

## Codex CLI

- Docs: <https://github.com/openai/codex> (see `codex exec --help`).
- Non-interactive: `codex exec "<prompt>"`.
- Bypass approvals: `--dangerously-bypass-approvals-and-sandbox` or `--full-auto` (auto-approve within the sandbox).
- Output: stdout.

### config.toml snippet

```toml
[mailboxes.triage]
address = "triage@agent.yourdomain.com"
trust = "verified"

[[mailboxes.triage.on_receive]]
type = "cmd"
command = '''
codex exec "Triage this email and update the issue tracker.

Email file: $AIMX_FILEPATH
From: $AIMX_FROM
Subject: $AIMX_SUBJECT" \
  --full-auto \
  >> /var/log/aimx/codex.log 2>&1
'''
```

- `--full-auto` is the recommended default for triggers: Codex auto-approves tool use but stays inside its sandbox. Use `--dangerously-bypass-approvals-and-sandbox` only if you need unrestricted shell access and you trust the sender policy.
- `codex exec` prints a session-scoped summary to stdout and exits. Trigger exit code propagates from Codex.

## OpenCode

- Docs: <https://opencode.ai/docs/cli/>
- Non-interactive: `opencode run "<prompt>"`.
- Approval: OpenCode's `run` mode does not prompt for confirmation on tool use — safe for triggers without a dedicated bypass flag.
- Model selection: `-m <provider/model>` (e.g. `-m anthropic/claude-sonnet-4-5`).
- Output: stdout.

### config.toml snippet

```toml
[mailboxes.research]
address = "research@agent.yourdomain.com"
trust = "verified"

[[mailboxes.research.on_receive]]
type = "cmd"
command = '''
opencode run "A new research email arrived. Read \"$AIMX_FILEPATH\" and append a digest entry to docs/digest.md.

From: $AIMX_FROM
Subject: $AIMX_SUBJECT" \
  >> /var/log/aimx/opencode.log 2>&1
'''
```

- `opencode run` terminates after the single prompt resolves.
- If you run OpenCode with a project-scoped config (`opencode.json` in the repo), either `cd` into the project inside the trigger command or set the working directory with a shell wrapper.

## Gemini CLI

- Docs: <https://github.com/google-gemini/gemini-cli> (see `gemini --help`).
- Non-interactive: `gemini -p "<prompt>"` (alias `--prompt`).
- Auto-approve: `--yolo` (accepts all tool-use confirmations).
- Output: stdout.

### config.toml snippet

```toml
[mailboxes.notes]
address = "notes@agent.yourdomain.com"
trust = "verified"

[[mailboxes.notes.on_receive]]
type = "cmd"
command = '''
gemini -p "A new note arrived by email. Read \"$AIMX_FILEPATH\" and file it into my notes." \
  --yolo \
  >> /var/log/aimx/gemini.log 2>&1
'''
```

- `--yolo` is required for non-interactive triggers; without it Gemini prompts for every tool use, which will hang the shell command.
- Gemini exits non-zero if its turn fails (e.g. model error); the failure is logged but does not block email delivery.

## Goose

- Docs: <https://block.github.io/goose/docs/guides/headless-goose>
- Non-interactive: `goose run -t "<prompt>"` (alias `--text`), or `goose run -i <file>` to read a prompt from a file.
- Recipe mode: `goose run --recipe <name>` runs a pre-registered recipe non-interactively.
- Output: stdout.

### config.toml snippet

```toml
[mailboxes.ops]
address = "ops@agent.yourdomain.com"
trust = "verified"

[[mailboxes.ops.on_receive]]
type = "cmd"
command = '''
goose run -t "Ops email arrived. Read \"$AIMX_FILEPATH\", check if action is required, and page on-call if so.

Subject: $AIMX_SUBJECT
From: $AIMX_FROM" \
  >> /var/log/aimx/goose.log 2>&1
'''
```

- If you installed the AIMX recipe (`aimx agent-setup goose`), you can instead run `goose run --recipe aimx` and pass the email details in a follow-up prompt — but for triggers, the inline `-t` form is shorter.
- Goose sessions are persisted by default; pass `--no-session` if you prefer each trigger to start fresh.

## OpenClaw

- Docs: <https://docs.openclaw.ai/>
- MCP server: `aimx agent-setup openclaw` emits an `openclaw mcp set aimx '<json>'` command you paste once to register AIMX's MCP tools with the OpenClaw gateway.
- Non-interactive agent invocation: `openclaw agent --message "<prompt>" --deliver --json`.
- Output: structured JSON when invoked with `--json`. `--deliver` routes the agent's reply back through OpenClaw's configured destination (useful when you want the reply to land in a channel the user already watches); omit it to just capture the reply locally.

### config.toml snippet

```toml
[mailboxes.inbox]
address = "inbox@agent.yourdomain.com"
trust = "verified"
trusted_senders = ["*@yourcompany.com"]

[[mailboxes.inbox.on_receive]]
type = "cmd"
command = '''
openclaw agent \
    --message "An email arrived at $(basename \"$AIMX_FILEPATH\"). Read it at \"$AIMX_FILEPATH\", summarize the sender's request, and respond via the aimx MCP tools if a reply is appropriate." \
    --deliver \
    --json \
    >> /var/log/aimx/openclaw.log 2>&1
'''
```

### Notes

- OpenClaw's `agent` subcommand is the non-interactive entrypoint. Pair it with `--json` so the output envelope is stable for scripts/log analysis.
- If you do not want OpenClaw to auto-send a reply, drop `--deliver` and inspect the JSON output yourself — the `outputs` field carries the agent's response.
- `openclaw agent --local` bypasses the OpenClaw gateway and runs an embedded agent directly; useful on a server where the gateway is not running.
- When a reply is appropriate, have the agent use AIMX's `email_reply` MCP tool (registered by `aimx agent-setup openclaw`) rather than shelling out to `aimx send` — the MCP tool handles threading automatically.

## Aider

- Docs: <https://aider.chat/docs/usage.html>
- Non-interactive: `aider --message "<prompt>"` (runs once and exits).
- Auto-approve: `--yes-always` (skips all confirmation prompts).
- Repo scope: `--file <path>` or positional file args.

### config.toml snippet

Aider is a code-editing agent — a natural recipe is "take an email describing a bug, apply a patch, commit." Aider has no MCP server, so the trigger is the only integration path.

```toml
[mailboxes.bugs]
address = "bugs@agent.yourdomain.com"
trust = "verified"
trusted_senders = ["*@yourcompany.com"]

[[mailboxes.bugs.on_receive]]
type = "cmd"
command = '''
cd /srv/repos/myapp && \
aider --yes-always \
      --message "Bug report arrived by email. Read \"$AIMX_FILEPATH\", reproduce the issue if possible, apply a fix, and commit." \
      >> /var/log/aimx/aider.log 2>&1
'''
```

- Aider auto-commits by default — pair the recipe with `trust = "verified"` plus an explicit `trusted_senders` allowlist so a spoofed sender cannot push a commit.
- `cd /srv/repos/myapp` scopes Aider to the right repository. Without it, Aider will prompt for a git repo, which will hang.
- If the model cost matters, set `--model <cheap-model>` for triage recipes and reserve expensive models for explicit human-invoked edits.

## Operational tips

### Logging

Every recipe above redirects stdout and stderr to a log file because `aimx serve` runs detached. Without the redirect, the agent's output is lost. Recommended convention:

```bash
/var/log/aimx/<agent>.log
```

Rotate with `logrotate` (or the equivalent) — these files grow.

### Exit codes

A non-zero exit from the trigger command is logged to `aimx serve`'s stderr but does NOT block delivery. The email is always stored as a `.md` file. This is intentional: you do not want flaky agent CLIs to stall your mailbox.

### Concurrent triggers

If two emails arrive in rapid succession, `aimx serve` fires two trigger shells in parallel. Agent CLIs that lock a resource (e.g. an Aider-managed git repo) can collide — serialise with `flock`:

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

Next: [Channels](channels.md) | [Agent Integration](agent-integration.md) | [MCP Server](mcp.md)
