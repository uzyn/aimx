# Agent Integration

`aimx agent-setup <agent>` is the one command that wires an AI agent into aimx. It installs the agent's plugin/skill package under the caller's `$HOME`, probes `$PATH` for the agent binary, and registers a matching hook template (`invoke-<agent>-<username>`) over the daemon's UDS socket so the agent can immediately create `on_receive` / `after_send` hooks via MCP. No sudo, no manual config edit, no second `aimx setup` run. When you are done, `aimx agent-cleanup <agent>` removes what `agent-setup` installed.

For email-triggered workflows after installation, see [Hook Recipes](hook-recipes.md).

## The one-command flow

```bash
aimx agent-setup claude-code
```

What this does, in order:

1. **Refuse root.** `sudo aimx agent-setup ...` is rejected. Run it as the user whose agent you are configuring.
2. **Install plugin files.** The plugin tree embedded in the `aimx` binary is written under the agent's per-user destination (e.g. `~/.claude/plugins/aimx/`). File mode `0o644`, directory mode `0o755`.
3. **Probe `$PATH`.** aimx walks `$PATH` in order looking for the agent's canonical binary name (for example `claude` for `claude-code`). First match wins.
4. **Register the template.** aimx connects to `/run/aimx/aimx.sock` and submits a `TEMPLATE-CREATE` verb. The template is named `invoke-<agent>-<username>` (e.g. `invoke-claude-alice`), carries `cmd = [<found_path>, ...<spec args>]`, and sets `run_as` to the caller's username. The daemon hot-swaps its in-memory config so the template is live immediately — no SIGHUP, no restart.
5. **Print confirmation.** The installer prints a single line confirming the template registration (plus any activation hint the agent requires, such as a `claude mcp add` command).

Example output for alice running `aimx agent-setup claude-code`:

```text
Installed plugin files to /home/alice/.claude/plugins/aimx/
Template invoke-claude-alice registered
  cmd:     /home/alice/.local/bin/claude -p {prompt}
  run_as:  alice
```

After this, alice can create hooks via MCP by referencing `invoke-claude-alice` — no operator intervention required.

## Discovering supported agents

`aimx agent-setup` with no argument launches an interactive checkbox TUI listing every supported agent with its detected install state (already wired, installed but not wired, or not detected). Use the arrow keys to move, `Space` to toggle, `Enter` to confirm, or `q` to cancel. The TUI defaults the right boxes for you: installed-but-not-wired agents are pre-checked; already-wired agents are listed but unchecked; not-detected agents are dimmed and skipped by the cursor.

For scripting, `aimx agent-setup --list` prints the same registry as a plain table with no prompt, and `aimx agent-setup --no-interactive` prints the same table when invoked with no agent argument. Piping the output to `cat` / `less` also falls back to the plain table automatically.

### Reference: TUI visual

```text
Wire aimx into your AI agents
  → Space toggles, Enter confirms, q cancels.

❯ [ ] Claude Code
  [x] Codex CLI  (already wired)
  [-] Gemini CLI (not detected)
  [ ] OpenClaw
  [-] OpenCode (not detected)
  [-] Hermes (not detected)
  [-] Goose (not detected)
```

- `❯` is the colored caret on the focused row.
- `[x]` / `[ ]` are selected / unselected checkboxes.
- `[-] ... (not detected)` marks agents whose config directory isn't present on this machine — the cursor skips those rows entirely.
- `(already wired)` marks agents whose plugin destination already exists on disk — they're listed but default to unchecked.

## Landing in the TUI from `aimx setup`

When `sudo aimx setup` completes, the wizard drops through to `aimx agent-setup` as the invoking user (via `runuser -u $SUDO_USER -- /proc/self/exe agent-setup`) so agent wiring is one continuous flow — no second command to type. If `$SUDO_USER` is unset (you logged in directly as root), the wizard prints the guidance message instead and exits cleanly. See [Setup — drop-through to agent-setup](./setup.md) for the wizard-side details.

Under `AIMX_NONINTERACTIVE=1`, the drop-through is skipped (no TTY is assumed).

## Key properties

- **Runs as the current user.** `aimx agent-setup` refuses to run as root by default.
- **`--dangerously-allow-root` escape hatch.** For single-user root-login VPS setups that have no separate operator account, pass `--dangerously-allow-root` to wire aimx into `/root`'s home. The flag applies uniformly to the TUI, per-agent runs, and `--no-interactive`. It is **never** passed implicitly by the `aimx setup` drop-through — you must opt in by hand. On any machine with a regular user, prefer `sudo -u <user> aimx agent-setup` instead.
- **Writes only to `$HOME`.** Nothing under `/etc` or `/var` is touched by the plugin-install step.
- **Template registration uses UDS `SO_PEERCRED`.** The daemon reads the caller's uid directly from the socket; the `run_as` of the registered template must equal the caller's username. This is how isolation between users is enforced — alice cannot register a template that runs as bob.
- **Offline.** The plugin tree is embedded at compile time. No network access is required.
- **Idempotent re-runs.** Re-running `aimx agent-setup <agent>` with `--force` overwrites existing plugin files. Use `--redetect` to re-probe `$PATH` and refresh `cmd[0]` if your agent binary moved. Use `--no-template` to skip the template-registration step entirely (plugin-install only).

## Flags

| Flag | Purpose |
|------|---------|
| `--list` | Print the registry (agent name, destination, activation hint). No TUI. |
| `--no-interactive` | Skip the checkbox TUI when no agent is named; print the same plain registry dump as `--list`. Intended for scripting. |
| `--dangerously-allow-root` | Footgun. Bypass the root-refusal check and wire aimx into `/root`'s home. Applies uniformly across the TUI, per-agent runs, and `--no-interactive`. See Key properties above. |
| `--force` | Overwrite existing destination files without prompting. |
| `--print` | Print plugin contents to stdout instead of writing to disk. Useful for CI and dry runs. Also prints the template that would be registered. |
| `--no-template` | Skip the `$PATH` probe and `TEMPLATE-CREATE` step. Plugin-install only. |
| `--redetect` | Re-probe `$PATH` and refresh an existing `invoke-<agent>-<username>` template's `cmd[0]` if the binary moved. |
| `--data-dir <path>` | Global flag. If aimx was set up with a non-default data directory, pass this so the plugin's MCP command is rewritten to include `--data-dir`. |

## Removing an agent: `aimx agent-cleanup`

`aimx agent-cleanup <agent>` is the inverse of `agent-setup`. It runs per-user and refuses root. By default it submits a `TEMPLATE-DELETE` for `invoke-<agent>-<caller_username>`. Pass `--full` to also remove the plugin files under `$HOME`.

```bash
aimx agent-cleanup claude-code           # template only
aimx agent-cleanup claude-code --full    # template + plugin files
```

If the daemon is down (`/run/aimx/aimx.sock` is missing), `--full` still removes plugin files and prints: "daemon unreachable; run `sudo aimx hooks prune --orphans` after restarting to clean up templates." The command exits `2`.

## Supported agents

| Agent | Install command | Destination | Activation | Progressive disclosure |
|-------|-----------------|-------------|------------|------------------------|
| Claude Code | `aimx agent-setup claude-code` | `~/.claude/plugins/aimx/` | Run the printed `claude mcp add --scope user aimx …` command, then restart Claude Code. | Primer as skill + `references/` directory copied as siblings |
| Codex CLI | `aimx agent-setup codex` | `~/.codex/plugins/aimx/` | Restart Codex CLI; the plugin is auto-discovered from `~/.codex/plugins/`. | Primer as skill + `references/` directory copied as siblings |
| OpenCode | `aimx agent-setup opencode` | `~/.config/opencode/skills/aimx/` | Paste the printed JSONC block into `opencode.json`, then restart OpenCode. | Single skill file (primer body). References inlined |
| Gemini CLI | `aimx agent-setup gemini` | `~/.gemini/skills/aimx/` | Merge the printed JSON block into `~/.gemini/settings.json`, then restart Gemini CLI. | Single skill file (primer body). References inlined |
| Goose | `aimx agent-setup goose` | `~/.config/goose/recipes/aimx.yaml` | Run `goose run --recipe aimx`. The recipe bundles its own MCP extension, so no separate config step. | Single YAML blob (primer as `prompt` block scalar). References inlined |
| OpenClaw | `aimx agent-setup openclaw` | `~/.openclaw/skills/aimx/` | Run the printed `openclaw mcp set aimx '...'` command, then restart the OpenClaw gateway. | Primer as skill + `references/` directory copied as siblings |
| Hermes | `aimx agent-setup hermes` | `~/.hermes/skills/aimx/` | Paste the printed YAML block under `mcp_servers:` in `~/.hermes/config.yaml`, then run `/reload-mcp` inside Hermes. | Primer as skill + `references/` directory copied as siblings |

> **Progressive disclosure.** Every agent receives the same canonical aimx primer (`agents/common/aimx-primer.md`). Agents with multi-file skill directories (Claude Code, Codex CLI, OpenClaw, Hermes) also receive `agents/common/references/` as siblings so detailed material loads on demand without bloating the initial context. Single-file agents (OpenCode, Gemini CLI, Goose) receive the primer inline. The `references/` content remains available in the aimx source tree and at `/var/lib/aimx/README.md`.

### Template naming and mapping

Every template registered by `aimx agent-setup` follows the pattern `invoke-<agent>-<username>`. Mapping of agent → template prefix:

| Agent | Template name |
|-------|---------------|
| `claude-code` | `invoke-claude-<username>` |
| `codex` | `invoke-codex-<username>` |
| `opencode` | `invoke-opencode-<username>` |
| `gemini` | `invoke-gemini-<username>` |
| `goose` | `invoke-goose-<username>` |
| `openclaw` | `invoke-openclaw-<username>` |
| `hermes` | `invoke-hermes-<username>` |

On a host where alice and bob each run `aimx agent-setup claude-code`, the daemon carries both `invoke-claude-alice` and `invoke-claude-bob`. `hook_list_templates` returns only the templates whose `run_as` equals the caller's username, so alice's MCP sees `invoke-claude-alice` and bob's sees `invoke-claude-bob` — never each other's.

See [MCP Server § Hook template tools](mcp.md#hook-template-tools) for the full tool reference.

### Claude Code

Claude Code discovers plugins by scanning `~/.claude/plugins/`, but the MCP
server bundled inside a plugin is **not** auto-activated for every
invocation. In particular `claude -p` (headless mode, used by hook
recipes) needs an explicit `claude mcp add` so the server is registered in
its MCP registry. The aimx plugin ships two pieces:

- `.claude-plugin/plugin.json`: manifest declaring the plugin and the
  `mcpServers.aimx` entry. The plugin itself auto-activates for interactive
  `claude` sessions.
- `skills/aimx/SKILL.md`: a skill Claude Code loads when the conversation
  touches email, inboxes, or aimx. The skill body is the canonical aimx
  primer: MCP tool names and parameters, the on-disk storage layout, the
  frontmatter format, read/unread semantics, and the DKIM/SPF trust model.

Install:

```bash
aimx agent-setup claude-code
```

Then register the MCP server with Claude Code:

```bash
claude mcp add --scope user aimx /usr/local/bin/aimx mcp
```

This updates `~/.claude.json` (the user-scope MCP registry) so both the
interactive REPL and `claude -p` headless invocations see the `aimx`
server. Restart Claude Code after registration.

Custom data directory:

```bash
aimx --data-dir /custom/path agent-setup claude-code
claude mcp add --scope user aimx /usr/local/bin/aimx --data-dir /custom/path mcp
```

The `aimx agent-setup` installer rewrites `mcpServers.aimx.args` in the
plugin's `plugin.json` and prints a `claude mcp add` command that includes
the same `--data-dir` override.

### Codex CLI

Codex CLI discovers plugins by scanning `~/.codex/plugins/`. The aimx
plugin ships two pieces:

- `.codex-plugin/plugin.json`: manifest declaring the plugin and
  registering `aimx mcp` as an MCP server.
- `skills/aimx/SKILL.md`: the agent-facing skill (body = canonical aimx
  primer).

Install:

```bash
aimx agent-setup codex
```

Custom data directory:

```bash
aimx --data-dir /custom/path agent-setup codex
```

Like Claude Code, the installer rewrites `mcpServers.aimx.args` in
`plugin.json` to include `--data-dir /custom/path`.

Verify the plugin format and destination path against the current Codex
CLI documentation before relying on this in production. Agent plugin
formats drift between releases. See the per-agent
[README](https://github.com/uzyn/aimx/tree/main/agents/codex) for the
documentation link.

### OpenCode

OpenCode discovers skills from `~/.config/opencode/skills/<name>/` (user)
or `<repo>/.opencode/skills/<name>/` (project). The aimx package is
skill-only. MCP servers in OpenCode are configured in `opencode.json`,
not alongside the skill.

Install:

```bash
aimx agent-setup opencode
```

The installer writes `~/.config/opencode/skills/aimx/SKILL.md` and prints
a JSONC block. Paste that block into the `mcp` object in your
`opencode.json` (user-level at `~/.config/opencode/opencode.json` or
project-level at `<repo>/opencode.json`):

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
aimx --data-dir /custom/path agent-setup opencode
```

The printed JSONC snippet will have `"--data-dir", "/custom/path"`
inserted into the `command` array.

Restart OpenCode (or reload its config) after editing `opencode.json`.
See [`agents/opencode/README.md`](https://github.com/uzyn/aimx/tree/main/agents/opencode)
for the schema reference.

### Gemini CLI

Gemini CLI picks up skills from `~/.gemini/skills/<name>/` and configures
MCP servers in `~/.gemini/settings.json`. aimx does not mutate
`settings.json` directly (FR-49). Instead the installer prints the exact
JSON block to merge.

Install:

```bash
aimx agent-setup gemini
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
aimx --data-dir /custom/path agent-setup gemini
```

The printed `args` array will include `"--data-dir", "/custom/path"`.

Restart Gemini CLI after editing `settings.json`. See
[`agents/gemini/README.md`](https://github.com/uzyn/aimx/tree/main/agents/gemini)
for the schema reference.

### Goose

[Goose](https://goose-docs.ai/) uses YAML "recipes" rather than plugins or
skills. A recipe bundles a goal, an agent-facing `prompt`, and the MCP
`extensions` that run alongside the agent. One file carries both the
MCP wiring AND the aimx primer. No separate config-file edit is needed.

Install:

```bash
aimx agent-setup goose
```

The installer writes `~/.config/goose/recipes/aimx.yaml`. Run the recipe
with:

```bash
goose run --recipe aimx
```

Goose resolves the `--recipe aimx` argument to `aimx.yaml` in the
recipes directory.

For a custom data directory:

```bash
aimx --data-dir /custom/path agent-setup goose
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

[OpenClaw](https://docs.openclaw.ai/) uses skill directories (similar
to Claude Code) for agent-facing instructions and a separate JSON5
config file for MCP servers at `~/.openclaw/openclaw.json`. Rather than
having you hand-edit the JSON5 file, aimx uses OpenClaw's first-class
`openclaw mcp set` CLI to register the MCP server non-interactively.

Install:

```bash
aimx agent-setup openclaw
```

The installer writes `~/.openclaw/skills/aimx/SKILL.md` and prints a
command like:

```bash
openclaw mcp set aimx '{"command":"/usr/local/bin/aimx","args":["mcp"]}'
```

Run that command (it edits `~/.openclaw/openclaw.json` for you), then
restart the OpenClaw gateway so the new MCP server is loaded.

For a custom data directory:

```bash
aimx --data-dir /custom/path agent-setup openclaw
```

The printed `openclaw mcp set` command's JSON will include
`--data-dir /custom/path` in the `args` array.

See [`agents/openclaw/README.md`](https://github.com/uzyn/aimx/tree/main/agents/openclaw)
for the schema reference.

### Hermes

[Hermes Agent](https://hermes-agent.nousresearch.com/) (by Nous Research)
loads skills from `~/.hermes/skills/<name>/SKILL.md` (with optional
`references/` siblings) and reads MCP server definitions from
`~/.hermes/config.yaml` under the top-level `mcp_servers:` key. There is
no shell-side CLI for registering external MCP servers in Hermes today
(`hermes mcp serve` runs Hermes as an MCP server, the opposite
direction), so aimx prints a YAML snippet for you to paste into the
config file, mirroring the Gemini CLI / OpenCode flow.

Install:

```bash
aimx agent-setup hermes
```

The installer writes `~/.hermes/skills/aimx/SKILL.md` and the bundled
`references/` directory, then prints a YAML block like:

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
aimx --data-dir /custom/path agent-setup hermes
```

The printed YAML's `args` line will become
`args: [--data-dir, /custom/path, mcp]`.

See [`agents/hermes/README.md`](https://github.com/uzyn/aimx/tree/main/agents/hermes)
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

### `Could not find <binary> in $PATH`

The `$PATH` probe did not find the agent's canonical binary. Install the agent CLI first (for example `npm install -g @anthropic-ai/claude-code` for Claude Code), confirm it lands in a directory on your `$PATH`, then re-run `aimx agent-setup <agent>`. If the binary is installed but the probe still misses it (shell alias, non-`$PATH` location), open a fresh shell so the login `$PATH` is in effect, or temporarily `export PATH="$HOME/.local/bin:$PATH"` before re-running.

### `aimx serve is not running; start it and re-run aimx agent-setup <agent>`

The UDS socket at `/run/aimx/aimx.sock` is missing. Start the daemon with `sudo systemctl start aimx` (or `sudo rc-service aimx start` on Alpine) and re-run `aimx agent-setup <agent>`. The template-registration step is all-or-nothing for v1: if the daemon is down, the plugin files are still written but no template is registered.

### `template-already-exists: invoke-<agent>-<username>`

You already ran `aimx agent-setup <agent>` on this host. There are three paths forward:

- **Change nothing.** The existing template is still registered and working; nothing to do.
- **Refresh `cmd[0]`** (your agent binary moved): run `aimx agent-setup <agent> --redetect`. This re-probes `$PATH` and updates the existing template's `cmd[0]` to the current path.
- **Replace entirely.** Run `aimx agent-cleanup <agent>` first to drop the existing template, then `aimx agent-setup <agent>` again to register a fresh one.

### The agent does not see aimx after `agent-setup` runs

- Confirm the destination was written: `aimx agent-setup --list` shows the
  destination path; check that it exists and contains the expected files.
- Restart the agent. Most agents only scan their plugin directory at
  startup.
- If the agent requires an explicit install step, re-read the installer
  output. The activation hint tells you exactly which command to run.

### "destination files already exist" error

Re-run with `--force` to overwrite when you want to replace the plugin
files on disk.

### `agent-setup` refuses to run as root

It is intentional. Per-user agent configuration lives under `$HOME`; if you
run the installer as root, it would drop files into root's home (or fail in
surprising ways with `sudo -u`). Run it as the user whose agent you are
configuring.

### MCP tools appear but calls fail with "Failed to load config"

The plugin's MCP command defaults to `/var/lib/aimx/` for the aimx data
directory. If you set up aimx with a different path, re-run with
`aimx --data-dir <path> agent-setup <agent> --force`.

### OpenCode: skill loads but MCP tools do not appear

OpenCode loads skills from `~/.config/opencode/skills/` but MCP servers
only activate when declared in `opencode.json`. Re-run `aimx agent-setup
opencode`, copy the printed JSONC block into the `mcp` object in your
`opencode.json`, and restart OpenCode.

### Gemini: "unknown MCP server aimx"

Gemini CLI requires the `mcpServers.aimx` block in
`~/.gemini/settings.json`. Re-run `aimx agent-setup gemini` and merge
the printed JSON block into `settings.json`. If the file did not exist
before you ran the installer, create it with just the printed object as
its contents.

### Goose: `goose run --recipe aimx` says "recipe not found"

Goose resolves `--recipe <name>` to `<name>.yaml` under
`~/.config/goose/recipes/`. Confirm the file is there:

```bash
ls ~/.config/goose/recipes/aimx.yaml
```

If it is missing, re-run `aimx agent-setup goose`. If `XDG_CONFIG_HOME`
is set to a non-default value, Goose and aimx will both honour it.
Check under `$XDG_CONFIG_HOME/goose/recipes/` instead.

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
