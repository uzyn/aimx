# Agent Integration

`aimx agents setup <agent>` wires an AI agent into AIMX in one command. It installs the agent's plugin or skill bundle under `$HOME` so the agent can call AIMX's MCP tools and create hooks via MCP. No sudo, no manual config edit. `aimx agents remove <agent>` is the inverse.

For email-triggered workflows after installation, see [Hook Recipes](hook-recipes.md).

## The one-command flow

```bash
aimx agents setup claude-code
```

In order:

1. Refuses root — run it as the user whose agent you are configuring.
2. Writes the embedded skill tree under the agent's per-user destination (e.g. `~/.claude/skills/aimx/`).
3. Auto-registers the MCP server when the agent has a registration CLI (Claude Code → `claude mcp add`, Codex → `codex mcp add`, NanoClaw → merge into `<fork>/.mcp.json`). Falls back to printing the equivalent command if the CLI is not on PATH.
4. Prints an activation hint for snippet-style agents (OpenCode, Gemini CLI, OpenClaw, Hermes) so you can paste the JSON/YAML block into the agent's config.

After this, the agent can call AIMX's MCP tools (including `hook_create`, `hook_list`, `hook_delete`) for any mailbox the calling user owns. The bundled plugin includes a "Wiring yourself up as a mailbox hook" section with the verified `cmd` argv.

## Discovering supported agents

`aimx agents setup` with no argument launches an interactive checkbox TUI listing every supported agent with its detected state (AIMX MCP wired, installed but not wired, or not detected). Arrow keys to move, `Space` to toggle, `Enter` to confirm, `q` to cancel. The TUI pre-checks installed-but-not-wired agents and dims undetected ones.

`Enter` shows a confirmation screen with the right verb per task (`Install AIMX MCP for ...` / `Re-install AIMX MCP for ...`) and asks `Confirm? [Y/n]` before writing any files.

`aimx agents list` and `aimx agents setup --no-interactive` print the same registry as a plain table. Piping to `cat` or `less` also falls back to the plain table.

### Reference: TUI visual

```text
Setting up MCP integration for AI agents for `alice`.
Select which AI agents you want to set up AIMX MCP for:

❯ [ ] Claude Code
  [x] Codex CLI  (AIMX MCP wired)
  [-] Gemini CLI (not detected)
  [ ] OpenClaw
  [-] OpenCode (not detected)
  [-] Hermes (not detected)
  [-] Goose (not detected)

  → Space toggles, Enter confirms, q cancels.
```

- `❯` is the colored caret on the focused row.
- `[x]` / `[ ]` are selected / unselected checkboxes.
- `[-] ... (not detected)` marks agents whose config directory isn't present on this machine — the cursor skips those rows entirely.
- `(AIMX MCP wired)` marks agents whose plugin destination already exists on disk — they're listed but default to unchecked.

## Landing in the TUI from `aimx setup`

When `sudo aimx setup` completes, the wizard drops through to `aimx agents setup` as the invoking user (via `runuser -u $SUDO_USER -- /proc/self/exe agents setup`) so agent wiring is one continuous flow — no second command to type. If `$SUDO_USER` is unset (you logged in directly as root), the wizard prints the guidance message instead and exits cleanly. See [Setup — drop-through to agents-setup](./setup.md) for the wizard-side details.

Under `AIMX_NONINTERACTIVE=1`, the drop-through is skipped (no TTY is assumed).

## Key properties

- **Refuses root.** Run `aimx agents setup` as the user whose agent you are configuring. For single-user root-login VPS setups, pass `--dangerously-allow-root` to wire AIMX into root's home. The flag applies to the TUI, per-agent runs, and `--no-interactive`, and is never passed implicitly by `aimx setup`'s drop-through.
- **Writes only to `$HOME`.** Nothing under `/etc/` or `/var/` is touched by the plugin-install step.
- **Offline.** The plugin tree is embedded at compile time.
- **Idempotent.** `--force` overwrites existing plugin files.
- **Hook authorization is by mailbox ownership.** When the agent later calls `hook_create`, the daemon authorizes via `SO_PEERCRED` and runs hooks as the mailbox's owner uid — see [Security: Per-action authorization](security.md#per-action-authorization).

## Flags

| Flag | Purpose |
|------|---------|
| `--list` | Print the registry (agent name, destination, activation hint). No TUI. |
| `--no-interactive` | Skip the checkbox TUI when no agent is named; print the same plain registry dump as `--list`. Intended for scripting. |
| `--dangerously-allow-root` | Bypass the root-refusal check and wire AIMX into `/root`'s home. Applies to the TUI, per-agent runs, and `--no-interactive`. Prefer `sudo -u <user> aimx agents setup` on any machine with a regular user. |
| `--force` | Overwrite existing destination files without prompting. |
| `--print` | Print plugin contents to stdout instead of writing to disk. Useful for CI and dry runs. |
| `--data-dir <path>` | Global flag. If aimx was set up with a non-default data directory, pass this so the plugin's MCP command is rewritten to include `--data-dir`. |

## Removing an agent: `aimx agents remove`

`aimx agents remove <agent>` is the inverse of `aimx agents setup`. It runs per-user and refuses root. The command removes the plugin files under `$HOME` and prints an agent-specific cleanup hint pointing at any external command you still need to run (for example `claude mcp remove aimx`).

```bash
aimx agents remove claude-code
```

## Supported agents

| Agent | Install command | Destination | Activation | Progressive disclosure |
|-------|-----------------|-------------|------------|------------------------|
| Claude Code | `aimx agents setup claude-code` | `~/.claude/skills/aimx/` | Auto-registered via `claude mcp add` (fallback hint printed if `claude` is not on PATH). Restart Claude Code so the new server is loaded. | Primer as skill + `references/` directory copied as siblings |
| Codex CLI | `aimx agents setup codex` | `~/.codex/skills/aimx/` | Auto-registered via `codex mcp add` (fallback hint printed if `codex` is not on PATH). Restart Codex CLI so the new server is loaded. | Primer as skill + `references/` directory copied as siblings |
| OpenCode | `aimx agents setup opencode` | `~/.config/opencode/skills/aimx/` | Paste the printed JSONC block into `opencode.json`, then restart OpenCode. | Single skill file (primer body). References inlined |
| Gemini CLI | `aimx agents setup gemini` | `~/.gemini/skills/aimx/` | Merge the printed JSON block into `~/.gemini/settings.json`, then restart Gemini CLI. | Single skill file (primer body). References inlined |
| Goose | `aimx agents setup goose` | `~/.config/goose/recipes/aimx.yaml` | Run `goose run --recipe aimx`. The recipe bundles its own MCP extension, so no separate config step. | Single YAML blob (primer as `prompt` block scalar). References inlined |
| OpenClaw | `aimx agents setup openclaw` | `~/.openclaw/skills/aimx/` | Run the printed `openclaw mcp set aimx '...'` command, then restart the OpenClaw gateway. | Primer as skill + `references/` directory copied as siblings |
| Hermes | `aimx agents setup hermes` | `~/.hermes/skills/aimx/` | Paste the printed YAML block under `mcp_servers:` in `~/.hermes/config.yaml`, then run `/reload-mcp` inside Hermes. | Primer as skill + `references/` directory copied as siblings |
| NanoClaw | `aimx agents setup nanoclaw` | `<fork>/skills/aimx/` (default `~/nanoclaw/`, override via `$NANOCLAW_HOME`) | Auto-merged into `<fork>/.mcp.json` under `mcpServers.aimx`. Restart NanoClaw so the new server is loaded. | Primer as skill + `references/` directory copied as siblings |

Every agent receives the canonical AIMX primer (`agents/common/aimx-primer.md`). Multi-file targets (Claude Code, Codex CLI, OpenClaw, Hermes, NanoClaw) also get `agents/common/references/` as siblings for progressive disclosure. Single-file targets (OpenCode, Gemini CLI, Goose) get the primer inline.

NanoClaw is the only supported agent where `aimx agents setup` writes to the agent's MCP config file: NanoClaw has no `mcp add` CLI and ships MCP servers as JSON5 in the per-fork `.mcp.json`. The installer reads that file (if present), merges an `mcpServers.aimx` entry preserving any other servers, and writes back via temp-file + atomic rename. `--print` shows the proposed JSON without touching disk; `--force` overwrites an existing `aimx` entry. Every other agent either has a registration CLI or expects the user to paste a snippet — AIMX does not mutate their config files.

### Per-agent hook recipes

The plugin bundle for each agent ships a "Wiring yourself up as a mailbox hook" section with a copy-paste `cmd` argv. The agent reads its own skill and writes the recipe at hook-creation time — see [Hook Recipes](hook-recipes.md) for the verified invocations.

On a multi-user host, alice and bob each install their own per-`$HOME` copy of the plugin, and each can call `hook_create` only on the mailboxes they own — the daemon enforces ownership via `SO_PEERCRED` on every UDS request.

See [MCP Server § Hook tools](mcp.md#hook-tools) for the full tool reference.

### Claude Code

Claude Code auto-discovers user-scope skills under `~/.claude/skills/`, but the MCP server is not auto-activated — `claude -p` (headless mode, used by hook recipes) needs an explicit `claude mcp add`. The skill ships `SKILL.md` (the AIMX primer) plus a `references/` directory loaded on demand.

```bash
aimx agents setup claude-code
```

The installer auto-runs `claude mcp add --scope user aimx -- /usr/local/bin/aimx mcp`, which updates `~/.claude.json` so both the interactive REPL and `claude -p` see the server. Restart Claude Code after install. If `claude` is not on PATH, the installer prints the equivalent command instead.

Custom data directory:

```bash
aimx --data-dir /custom/path agents setup claude-code
```

The installer threads `--data-dir /custom/path` into the auto-runned
`claude mcp add` invocation (and into the fallback hint when the CLI
is missing).

### Codex CLI

Codex CLI's MCP wiring lives in `~/.codex/config.toml` under `[mcp_servers.<name>]` and is managed via `codex mcp add`. The skill ships at `~/.codex/skills/aimx/` with `SKILL.md` plus a `references/` directory.

```bash
aimx agents setup codex
```

The installer auto-runs `codex mcp add aimx -- /usr/local/bin/aimx mcp`. If `codex` is not on PATH, the equivalent command is printed instead.

Custom data directory:

```bash
aimx --data-dir /custom/path agents setup codex
```

The installer threads `--data-dir /custom/path` into both the
auto-registration command and the fallback hint.

### OpenCode

OpenCode discovers skills from `~/.config/opencode/skills/<name>/` (user) or `<repo>/.opencode/skills/<name>/` (project). MCP servers are configured separately in `opencode.json`, not alongside the skill.

```bash
aimx agents setup opencode
```

The installer writes `~/.config/opencode/skills/aimx/SKILL.md` and prints a JSONC block. Paste it into the `mcp` object in `~/.config/opencode/opencode.json` (or project-level `<repo>/opencode.json`):

```jsonc
{
  "mcp": {
    "aimx": {
      "command": ["/usr/local/bin/aimx", "mcp"]
    }
  }
}
```

For a custom data directory:

```bash
aimx --data-dir /custom/path agents setup opencode
```

The printed JSONC snippet will have `"--data-dir", "/custom/path"`
inserted into the `command` array.

Restart OpenCode (or reload its config) after editing `opencode.json`.
See [`agents/opencode/README.md`](https://github.com/uzyn/aimx/tree/main/agents/opencode)
for the schema reference.

### Gemini CLI

Gemini CLI picks up skills from `~/.gemini/skills/<name>/` and configures MCP servers in `~/.gemini/settings.json`. The installer prints the exact JSON block to merge rather than mutating `settings.json` directly.

```bash
aimx agents setup gemini
```

The installer writes `~/.gemini/skills/aimx/SKILL.md` and prints:

```json
{
  "mcpServers": {
    "aimx": {
      "command": "/usr/local/bin/aimx",
      "args": ["mcp"]
    }
  }
}
```

Merge that block into `~/.gemini/settings.json`. If the file does not
exist, create it with the object above as its full contents. If
`mcpServers` already exists, add the `aimx` key inside it.

For a custom data directory:

```bash
aimx --data-dir /custom/path agents setup gemini
```

The printed `args` array will include `"--data-dir", "/custom/path"`.

Restart Gemini CLI after editing `settings.json`. See
[`agents/gemini/README.md`](https://github.com/uzyn/aimx/tree/main/agents/gemini)
for the schema reference.

### Goose

[Goose](https://goose-docs.ai/) uses YAML recipes — one file bundles the goal, agent-facing prompt, and MCP extensions. The recipe carries both the MCP wiring and the AIMX primer; no separate config edit is needed.

```bash
aimx agents setup goose
```

The installer writes `~/.config/goose/recipes/aimx.yaml`. Run it with:

```bash
goose run --recipe aimx
```

Goose resolves the `--recipe aimx` argument to `aimx.yaml` in the
recipes directory.

For a custom data directory:

```bash
aimx --data-dir /custom/path agents setup goose
```

The recipe's stdio extension args will be rewritten to include
`--data-dir /custom/path` before `mcp`.

**Sharing recipes with a team:** if you set the
`GOOSE_RECIPE_GITHUB_REPO` environment variable, Goose loads recipes
from a GitHub repo. In that case, commit the generated
`~/.config/goose/recipes/aimx.yaml` into your repo so every user can
invoke `goose run --recipe aimx`. The installer detects the env var at
install time and prints a pointer to this workflow.

See [`agents/goose/README.md`](https://github.com/uzyn/aimx/tree/main/agents/goose)
for the schema reference.

### OpenClaw

[OpenClaw](https://docs.openclaw.ai/) uses skill directories like Claude Code, with MCP servers configured in `~/.openclaw/openclaw.json`. The installer uses OpenClaw's `openclaw mcp set` CLI to register the server non-interactively.

```bash
aimx agents setup openclaw
```

The installer writes `~/.openclaw/skills/aimx/SKILL.md` and prints a command like:

```bash
openclaw mcp set aimx '{"command":"/usr/local/bin/aimx","args":["mcp"]}'
```

Run that command (it edits `~/.openclaw/openclaw.json` for you), then
restart the OpenClaw gateway so the new MCP server is loaded.

For a custom data directory:

```bash
aimx --data-dir /custom/path agents setup openclaw
```

The printed `openclaw mcp set` command's JSON will include
`--data-dir /custom/path` in the `args` array.

See [`agents/openclaw/README.md`](https://github.com/uzyn/aimx/tree/main/agents/openclaw)
for the schema reference.

### Hermes

[Hermes Agent](https://hermes-agent.nousresearch.com/) loads skills from `~/.hermes/skills/<name>/SKILL.md` (optionally with `references/` siblings) and reads MCP server definitions from `~/.hermes/config.yaml` under `mcp_servers:`. There is no shell-side CLI for registering external MCP servers in Hermes, so the installer prints a YAML snippet to paste into the config.

```bash
aimx agents setup hermes
```

The installer writes `~/.hermes/skills/aimx/SKILL.md` and the bundled `references/` directory, then prints a YAML block like:

```yaml
mcp_servers:
  aimx:
    command: /usr/local/bin/aimx
    args: [mcp]
    enabled: true
```

Add that block to `~/.hermes/config.yaml` under the top-level
`mcp_servers:` key (create the key if it does not yet exist), save the
file, then run `/reload-mcp` inside Hermes to pick up the new server
without restarting.

For a custom data directory:

```bash
aimx --data-dir /custom/path agents setup hermes
```

The printed YAML's `args` line will become
`args: [--data-dir, /custom/path, mcp]`.

See [`agents/hermes/README.md`](https://github.com/uzyn/aimx/tree/main/agents/hermes)
for the schema reference.

### NanoClaw

[NanoClaw](https://nanoclaw.dev/) is forked per-user from `qwibitai/nanoclaw` and run from the clone, so there is no global `~/.nanoclaw/`. The installer resolves the fork path from `$NANOCLAW_HOME` (default `~/nanoclaw`); set the env var before running setup if your fork lives elsewhere.

NanoClaw exposes no `mcp add` CLI and ships MCP servers as JSON5 in `<fork>/.mcp.json`. The installer reads the file (if present), merges an `mcpServers.aimx` entry, and writes back via temp-file + atomic rename. Other servers in the file are preserved.

Install:

```bash
aimx agents setup nanoclaw
```

(or, with a non-default fork path):

```bash
NANOCLAW_HOME=/opt/my-nanoclaw aimx agents setup nanoclaw
```

The installer requires the fork directory to exist (no auto-`mkdir`) so it never creates a stub a later `git clone` would refuse. Restart NanoClaw after install so it loads the new `.mcp.json` entry and discovers the skill.

For a custom data directory:

```bash
aimx --data-dir /custom/path agents setup nanoclaw
```

The merged `.mcp.json` entry's `args` array will include
`--data-dir /custom/path`.

**Re-running.** Skill files are idempotent (re-run with `--force` to overwrite). On `.mcp.json`, an existing `mcpServers.aimx` entry is left in place unless `--force` is passed; the installer warns and exits cleanly rather than silently shadowing what was there.

**Channel triggers and hooks.** NanoClaw is a long-running Node.js daemon listening on messaging channels inside a container; it does not expose a one-shot CLI suitable for an `on_receive` hook. The natural integration is the other direction — NanoClaw pulls unread mail via MCP on its own scheduled-job cadence. For sub-second reactions to inbound mail, wire a different agent (Claude Code, Codex, or Hermes) as the `on_receive` hook and let NanoClaw consume the resulting state on its next tick.

See [`agents/nanoclaw/README.md`](https://github.com/uzyn/aimx/tree/main/agents/nanoclaw)
for the schema reference.

## Manual MCP wiring

If your agent is not yet supported, wire aimx in manually as a plain MCP
stdio server. Most MCP-compatible agents accept a JSON snippet of this
form:

```json
{
  "mcpServers": {
    "aimx": {
      "command": "/usr/local/bin/aimx",
      "args": ["mcp"]
    }
  }
}
```

For a custom data directory, extend `args`:

```json
"args": ["--data-dir", "/custom/path", "mcp"]
```

The location that JSON goes in is agent-specific. Check your agent's MCP
documentation. The [MCP Server](mcp.md) chapter documents the available
tools.

## Troubleshooting

### Agent binary not found at runtime

When the agent later fires as a hook, the daemon `exec`s the absolute path you wrote into the hook's `cmd[0]`. If that path doesn't exist, the hook log shows `exit_code = -1` with `spawn-failed`. Run `which <agent>` on the host as the mailbox owner to confirm the binary's path, then re-create the hook with the corrected `cmd[0]` (`aimx hooks delete <name>` followed by `aimx hooks create --cmd ...`).

### The agent does not see aimx after `agents setup` runs

- Confirm the destination was written: `aimx agents setup --list` shows the
  destination path; check that it exists and contains the expected files.
- Restart the agent. Most agents only scan their plugin directory at
  startup.
- If the agent requires an explicit install step, re-read the installer
  output. The activation hint tells you exactly which command to run.

### "destination files already exist" error

Re-run with `--force` to overwrite when you want to replace the plugin
files on disk.

### `agents setup` refuses to run as root

It is intentional. Per-user agent configuration lives under `$HOME`; if you
run the installer as root, it would drop files into root's home (or fail in
surprising ways with `sudo -u`). Run it as the user whose agent you are
configuring.

### MCP tools appear but calls fail with "Failed to load config"

The plugin's MCP command defaults to `/var/lib/aimx/` for the aimx data
directory. If you set up aimx with a different path, re-run with
`aimx --data-dir <path> agents setup <agent> --force`.

### OpenCode: skill loads but MCP tools do not appear

OpenCode loads skills from `~/.config/opencode/skills/` but MCP servers
only activate when declared in `opencode.json`. Re-run `aimx agents setup
opencode`, copy the printed JSONC block into the `mcp` object in your
`opencode.json`, and restart OpenCode.

### Gemini: "unknown MCP server aimx"

Gemini CLI requires the `mcpServers.aimx` block in
`~/.gemini/settings.json`. Re-run `aimx agents setup gemini` and merge
the printed JSON block into `settings.json`. If the file did not exist
before you ran the installer, create it with just the printed object as
its contents.

### Goose: `goose run --recipe aimx` says "recipe not found"

Goose resolves `--recipe <name>` to `<name>.yaml` under
`~/.config/goose/recipes/`. Confirm the file is there:

```bash
ls ~/.config/goose/recipes/aimx.yaml
```

If it is missing, re-run `aimx agents setup goose`. If `XDG_CONFIG_HOME` is set to a non-default value, Goose and AIMX both honour it — check under `$XDG_CONFIG_HOME/goose/recipes/` instead.

### OpenClaw: `openclaw mcp set` says "command not found"

The activation step needs the `openclaw` CLI on your `PATH`. If
OpenClaw is installed in a non-standard location, run the printed JSON
through your own OpenClaw binary:

```bash
/path/to/openclaw mcp set aimx '...'
```

Alternatively, hand-edit `~/.openclaw/openclaw.json` and add the
printed object under `mcpServers.aimx`. The JSON5 format accepts
comments and trailing commas but vanilla JSON works too.

### Hermes: aimx tools missing after editing config.yaml

Hermes does not auto-reload MCP servers when `~/.hermes/config.yaml`
changes. You must run the in-app `/reload-mcp` slash command (or
restart Hermes) after pasting the snippet. Confirm the snippet sits
under the top-level `mcp_servers:` key (not nested inside another
section) and that YAML indentation uses spaces, not tabs. If you have
no other MCP servers configured, the block can be the entire
`mcp_servers:` section.
