# Hook Recipes

Copy-paste `aimx hooks create` invocations for every supported AI agent. For the underlying mechanics, see [Hooks & Trust](hooks.md); for installing an agent's plugin so its MCP tools are discoverable, see [Agent Integration](agent-integration.md).

The `/var/log/aimx/<agent>.log` paths below are user-chosen destinations for hook script output. They are not AIMX's own logs — AIMX logs to journald (systemd) or the system logger (OpenRC). See [Troubleshooting: Logs](troubleshooting.md#logs).

## What counts as a hook recipe?

A hook recipe is a `[[mailboxes.<name>.hooks]]` block (or equivalent `aimx hooks create` invocation) whose `cmd` invokes an AI agent or HTTP endpoint non-interactively against an incoming email. AIMX writes the `.md` file, evaluates the trust gate, then `execvp`s the argv as the mailbox owner with the raw `.md` piped to stdin and `$AIMX_FILEPATH` set. The agent reads the email and takes action (reply, file a ticket, update a calendar). Exit code is logged but never blocks delivery.

`cmd` is `execvp`'d directly: there is no `/bin/sh` between AIMX and the agent. User-controlled header fields like `From:` and `Subject:` can contain shell metacharacters (`$()`, backticks, `;`, quotes), so AIMX delivers them as env vars and avoids the shell parser entirely. When you need shell expansion, spell out `["/bin/sh", "-c", "..."]` so it is visible at the call site.

## Two consumption shapes

The daemon always pipes the `.md` to stdin, but recipes split into two camps based on whether the agent reads stdin in headless mode:

- **Native stdin** (Claude Code, Codex, Gemini, Goose, webhook): the agent reads the piped email off stdin; the prompt only tells it what to do.
- **Filepath only** (OpenCode, Hermes): the agent does not read stdin in headless mode, so the prompt instructs it to read `$AIMX_FILEPATH` via its filesystem tool. argv is not shell-expanded — the literal `$AIMX_FILEPATH` token reaches the agent, which expands it at run time.

Hooks that only need the subject or sender can read `$AIMX_SUBJECT` / `$AIMX_FROM` and ignore stdin.

## Absolute paths

Every recipe uses `/usr/local/bin/<agent>` for `cmd[0]`. `Config::load` rejects relative paths (`hook has non-absolute cmd[0]`). Adjust the path to match your install — `which <agent>` shows where each binary lives. The `cmd[0]` literal in your `config.toml` (or in the `--cmd` argv array passed to `aimx hooks create`) must point at the binary that exists on the box.

## Self-loop reminder

The headless agent process runs as the mailbox owner's uid. That user must have the agent binary installed at the `cmd[0]` path AND the AIMX skill installed (run `aimx agents setup <agent>` as that user). Without both, the agent starts without the skill loaded and does not know what to do with the email.

## Claude Code

- Docs: <https://docs.claude.com/en/docs/claude-code/cli-reference>
- Stdin: native (`-p` reads from stdin when `cmd[0]` is fed via pipe).
- Bypass permissions: `--dangerously-skip-permissions` (alias `--permission-mode bypassPermissions`) is required for autonomous tool calls.

```bash
aimx hooks create \
  --mailbox accounts \
  --event on_receive \
  --cmd '["/usr/local/bin/claude", "-p", "Read the piped email and act on it via the aimx MCP server.", "--dangerously-skip-permissions"]' \
  --name accounts_claude
```

Optional: `--mcp-config /path` + `--strict-mcp-config` to pin which MCP config Claude reads, and `--model sonnet` (or `--model opus`) to tune the routing model.

## Codex CLI

- Docs: <https://github.com/openai/codex> (run `codex exec --help`).
- Stdin: native (the trailing `-` argument tells Codex to read stdin as the prompt — the email itself becomes Codex's task).
- Bypass: `--full-auto` is the safe sandboxed default; `--dangerously-bypass-approvals-and-sandbox` only inside a contained host.
- Required: `--skip-git-repo-check` because hooks fire from `/var/lib/aimx/inbox/<mailbox>/`, which is not a git repo.

```bash
aimx hooks create \
  --mailbox triage \
  --event on_receive \
  --cmd '["/usr/local/bin/codex", "exec", "--skip-git-repo-check", "--full-auto", "-"]' \
  --name triage_codex
```

## OpenCode

- Docs: <https://opencode.ai/docs/cli/>
- Stdin: inline-only (`opencode run` does not read stdin in headless mode).
- Bypass: `--dangerously-skip-permissions`.

```bash
aimx hooks create \
  --mailbox research \
  --event on_receive \
  --cmd '["/usr/local/bin/opencode", "run", "--dangerously-skip-permissions", "Read the aimx email at the path in env var AIMX_FILEPATH and act on it via the aimx MCP server (e.g. email_reply)."]' \
  --name research_opencode
```

The literal token `$AIMX_FILEPATH` is **not** shell-expanded (argv is not shell-parsed). The agent sees the literal string `AIMX_FILEPATH` (or `$AIMX_FILEPATH` if you write it that way) inside its prompt and uses its Bash/Read tool to read the env var at run time. Optional: `--format json` for a structured run log.

## Gemini CLI

- Docs: <https://github.com/google-gemini/gemini-cli>
- Stdin: native (`-p` reads from stdin when piped).
- Bypass: `--yolo` auto-approves tool calls; required for unattended operation.

```bash
aimx hooks create \
  --mailbox notes \
  --event on_receive \
  --cmd '["/usr/local/bin/gemini", "-p", "Read the piped email and file it into my notes via the aimx MCP server.", "--yolo"]' \
  --name notes_gemini
```

Optional: `-m gemini-2.5-flash` to pin the routing model.

## Goose

- Docs: <https://block.github.io/goose/docs/guides/headless-goose>
- Stdin: native via `--instructions -` (reads instructions from stdin; the recipe form is preferred for production).
- Two recipes shown: ad-hoc (instructions piped via stdin) and recipe-based (the inner recipe pre-binds the `aimx mcp` extension).

**Ad-hoc:**

```bash
aimx hooks create \
  --mailbox ops \
  --event on_receive \
  --cmd '["/usr/local/bin/goose", "run", "--instructions", "-", "--quiet"]' \
  --name ops_goose
```

**Recipe-based (preferred for production):**

```bash
aimx hooks create \
  --mailbox ops \
  --event on_receive \
  --cmd '["/usr/local/bin/goose", "run", "--recipe", "/etc/aimx/goose-aimx-hook.yaml"]' \
  --name ops_goose_recipe
```

The referenced recipe pre-binds the `aimx mcp` extension and parameterizes the inbound email path. See Goose's recipe documentation for the inner-recipe shape.

## OpenClaw

OpenClaw does not document a one-shot prompt mode as of Apr 2026. Its surfaces are interactive (chat-channel, Control-UI dashboard) plus admin subcommands; there is no `--prompt`-style entry point that fits the headless hook pattern.

To use OpenClaw with AIMX, treat AIMX as a read source via the MCP server and trigger the agent through OpenClaw's documented interactive entry points. For shell-side notifications on new mail (so OpenClaw operators know there is mail to look at), wire a simple non-agent `on_receive` hook:

```bash
aimx hooks create \
  --mailbox openclaw \
  --event on_receive \
  --cmd '["/bin/sh", "-c", "ntfy pub openclaw-mail \"New email: $AIMX_SUBJECT from $AIMX_FROM\""]' \
  --fire-on-untrusted \
  --name openclaw_notify
```

When OpenClaw later publishes a one-shot CLI, this section will be updated; track upstream at <https://docs.openclaw.ai/>.

## Hermes

- Docs: <https://hermes-agent.nousresearch.com/>
- Stdin: inline-only (stdin handling for `hermes chat -q` is undocumented as of Apr 2026; this recipe ships the inline-only form).
- Bypass: `--yolo` skips dangerous-command approval; `--ignore-user-config --ignore-rules` for fully isolated runs.

```bash
aimx hooks create \
  --mailbox hermes \
  --event on_receive \
  --cmd '["/usr/local/bin/hermes", "chat", "-q", "Read the aimx email at the path in env var AIMX_FILEPATH and act on it via the aimx MCP server.", "--yolo"]' \
  --name hermes_chat
```

If a future Hermes release confirms that `chat -q` accepts piped stdin, shorten the inline prompt to "Read the piped email and act on it via the AIMX MCP server." — the daemon already pipes the email regardless.

## Webhook (POST to URL)

A pure-curl recipe that POSTs the raw `.md` to an HTTPS endpoint. No agent required.

```bash
aimx hooks create \
  --mailbox alerts \
  --event on_receive \
  --cmd '["/usr/bin/curl", "-sS", "-X", "POST", "-H", "Content-Type: application/json", "--data-binary", "@-", "https://hooks.example.com/aimx"]' \
  --name alerts_webhook
```

`--data-binary @-` tells curl to POST whatever lands on its stdin verbatim. Since the daemon always pipes the email to the hook, the receiver gets the raw `.md` (TOML frontmatter + body) as the request body. Use `Content-Type: text/markdown` instead if your receiver expects that explicitly.

## `after_send` recipes

Send-side hooks run after AIMX resolves the MX delivery attempt. They cannot affect the send result (hooks are observability-only) but are ideal for audit logs, outbound notifications, or post-send bookkeeping.

### Append to an audit log

```bash
aimx hooks create \
  --mailbox alice \
  --event after_send \
  --cmd '["/bin/sh", "-c", "printf \"%s %s %s %s\\n\" \"$AIMX_SEND_STATUS\" \"$AIMX_TO\" \"$AIMX_SUBJECT\" \"$AIMX_HOOK_NAME\" >> /var/log/aimx/alice-sent.log"]' \
  --name alice_audit
```

### Page on failed sends

```bash
aimx hooks create \
  --mailbox alerts \
  --event after_send \
  --cmd '["/bin/sh", "-c", "if [ \"$AIMX_SEND_STATUS\" != \"delivered\" ]; then ntfy pub on-call \"aimx send to $AIMX_TO $AIMX_SEND_STATUS: $AIMX_SUBJECT\"; fi"]' \
  --name alerts_failed_page
```

### Recipient-based filtering (shell guard)

`after_send` hooks have no built-in filter fields — do recipient/subject matching in the `cmd` itself with a shell guard.

```bash
aimx hooks create \
  --mailbox marketing \
  --event after_send \
  --cmd '["/bin/sh", "-c", "case \"$AIMX_TO\" in *@customer-co.com) curl -fsS -X POST https://hooks.internal/marketing-sent -d \"to=$AIMX_TO&status=$AIMX_SEND_STATUS\" ;; esac"]' \
  --name marketing_customer_notify
```

## Operational tips

### Logging

AIMX itself emits one structured log line per hook fire to journald:

```text
hook_name=<name> event=<on_receive|after_send> mailbox=<m> owner=<u> exit_code=<n> duration_ms=<n>
```

Grep by `hook_name=<name>` (`journalctl -u aimx | grep hook_name=accounts_claude`) to trace every fire of a specific hook. Hook stdout / stderr are captured by the daemon (truncated at 64 KiB each) and surfaced as `stderr_tail=...` on the structured line.

If you want a separate per-agent log file too, wrap the agent invocation in `["/bin/sh", "-c", "<agent> ... >> /var/log/aimx/<agent>.log 2>&1"]`. Most operators find the journald line sufficient.

### Exit codes

A non-zero exit from the hook command is logged at `warn` but does **not** block delivery or the send. Email is always stored as a `.md` file. This is intentional: you do not want flaky agent CLIs to stall your mailbox.

### Concurrent hooks

If two emails arrive in rapid succession, `aimx serve` fires two hook subprocesses in parallel. Agent CLIs that lock a resource (e.g. a per-repo Aider session) can collide. Serialise inside your own wrapper using `flock`:

```bash
aimx hooks create \
  --mailbox bugs \
  --event on_receive \
  --cmd '["/usr/bin/flock", "/tmp/myapp.lock", "/usr/local/bin/claude", "-p", "Reproduce the reported bug.", "--dangerously-skip-permissions"]' \
  --name bugs_serialised
```

### Testing a recipe locally

Use the `aimx ingest` CLI to simulate a real delivery without a live SMTP exchange:

```bash
sudo aimx --data-dir /tmp/aimx-test ingest catchall@agent.example.com \
     < tests/fixtures/plain.eml
```

Watch journald (`aimx logs --follow`) and confirm the agent ran with the expected env vars.
