# Agent Integration

`aimx agent-setup <agent>` installs AIMX's plugin/skill package into the agent's per-user config directory so the agent discovers AIMX as both an MCP server and an agent-facing primer. For email-triggered workflows after installation, see [Hook Recipes](hook-recipes.md).

## Guided setup

`aimx agent-setup` with no argument on an interactive terminal prints a numbered menu of every supported agent plus an **MCP (General)** option that outputs a generic MCP stdio JSON snippet for clients not yet in the registry. The menu respects `--data-dir`, so the printed snippet includes any override. Non-interactive callers (scripts, CI) must pass the positional `<agent>` argument or `--list`.

## What `aimx agent-setup` does

`aimx agent-setup <agent>`:

1. Looks up the named agent in the built-in registry.
2. Expands `$HOME` / `$XDG_CONFIG_HOME` to compute the destination.
3. Writes the plugin tree embedded in the `aimx` binary to that destination
   with file mode `0o644` and directory mode `0o755`.
4. Prints an activation hint — usually "restart the agent" when the agent
   auto-discovers plugins from a known directory, or the exact install
   command when the agent needs an explicit step.

Key properties:

- **Runs as the current user.** Never requires root. `sudo aimx agent-setup
  ...` is rejected with a clear error.
- **Writes only to `$HOME`.** Nothing under `/etc` or `/var` is touched.
- **Never edits the agent's primary config file.** If the agent needs
  additional wiring (e.g. a `mcp add` call), the installer prints the
  command and the user runs it.
- **Offline.** The plugin tree is embedded at compile time; no network
  access is required at install time.

## Flags

| Flag | Purpose |
|------|---------|
| `--list` | Print the registry (agent name, destination, activation hint). |
| `--force` | Overwrite existing destination files without prompting. |
| `--print` | Print plugin contents to stdout instead of writing to disk. Useful for CI and dry runs. |
| `--data-dir <path>` | Global flag. If AIMX was set up with a non-default data directory, pass this so the plugin's MCP command is rewritten to include `--data-dir`. |

## Supported agents

| Agent | Install command | Destination | Activation | Progressive disclosure |
|-------|-----------------|-------------|------------|------------------------|
| Claude Code | `aimx agent-setup claude-code` | `~/.claude/plugins/aimx/` | Run the printed `claude mcp add --scope user aimx …` command, then restart Claude Code. | Primer as skill + `references/` directory copied as siblings |
| Codex CLI | `aimx agent-setup codex` | `~/.codex/plugins/aimx/` | Restart Codex CLI; the plugin is auto-discovered from `~/.codex/plugins/`. | Primer as skill + `references/` directory copied as siblings |
| OpenCode | `aimx agent-setup opencode` | `~/.config/opencode/skills/aimx/` | Paste the printed JSONC block into `opencode.json`, then restart OpenCode. | Single skill file (primer body); references inlined |
| Gemini CLI | `aimx agent-setup gemini` | `~/.gemini/skills/aimx/` | Merge the printed JSON block into `~/.gemini/settings.json`, then restart Gemini CLI. | Single skill file (primer body); references inlined |
| Goose | `aimx agent-setup goose` | `~/.config/goose/recipes/aimx.yaml` | Run `goose run --recipe aimx`. The recipe bundles its own MCP extension, so no separate config step. | Single YAML blob (primer as `prompt` block scalar); references inlined |
| OpenClaw | `aimx agent-setup openclaw` | `~/.openclaw/skills/aimx/` | Run the printed `openclaw mcp set aimx '...'` command, then restart the OpenClaw gateway. | Primer as skill + `references/` directory copied as siblings |
| Hermes | `aimx agent-setup hermes` | `~/.hermes/skills/aimx/` | Paste the printed YAML block under `mcp_servers:` in `~/.hermes/config.yaml`, then run `/reload-mcp` inside Hermes. | Primer as skill + `references/` directory copied as siblings |

> **Progressive disclosure.** Every agent receives the same canonical AIMX primer (`agents/common/aimx-primer.md`). Agents with multi-file skill directories (Claude Code, Codex CLI, OpenClaw, Hermes) also receive `agents/common/references/` as siblings so detailed material loads on demand without bloating the initial context. Single-file agents (OpenCode, Gemini CLI, Goose) receive the primer inline; the `references/` content remains available in the AIMX source tree and at `/var/lib/aimx/README.md`.

### Claude Code

Claude Code discovers plugins by scanning `~/.claude/plugins/`, but the MCP
server bundled inside a plugin is **not** auto-activated for every
invocation — in particular `claude -p` (headless mode, used by hook
recipes) needs an explicit `claude mcp add` so the server is registered in
its MCP registry. The AIMX plugin ships two pieces:

- `.claude-plugin/plugin.json` — manifest declaring the plugin and the
  `mcpServers.aimx` entry. The plugin itself auto-activates for interactive
  `claude` sessions.
- `skills/aimx/SKILL.md` — a skill Claude Code loads when the conversation
  touches email, inboxes, or AIMX. The skill body is the canonical AIMX
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

Codex CLI discovers plugins by scanning `~/.codex/plugins/`. The AIMX
plugin ships two pieces:

- `.codex-plugin/plugin.json` — manifest declaring the plugin and
  registering `aimx mcp` as an MCP server.
- `skills/aimx/SKILL.md` — the agent-facing skill (body = canonical AIMX
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
CLI documentation before relying on this in production — agent plugin
formats drift between releases. See the per-agent
[README](https://github.com/uzyn/aimx/tree/main/agents/codex) for the
documentation link.

### OpenCode

OpenCode discovers skills from `~/.config/opencode/skills/<name>/` (user)
or `<repo>/.opencode/skills/<name>/` (project). The AIMX package is
skill-only — MCP servers in OpenCode are configured in `opencode.json`,
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
MCP servers in `~/.gemini/settings.json`. AIMX does not mutate
`settings.json` directly (FR-49); instead the installer prints the exact
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
`extensions` that run alongside the agent — so one file carries both the
MCP wiring AND the AIMX primer. No separate config-file edit is needed.

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
having you hand-edit the JSON5 file, AIMX uses OpenClaw's first-class
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

Run that command — it edits `~/.openclaw/openclaw.json` for you — then
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
(`hermes mcp serve` runs Hermes as an MCP server — the opposite
direction), so AIMX prints a YAML snippet for you to paste into the
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

If your agent is not yet supported, wire AIMX in manually as a plain MCP
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

The location that JSON goes in is agent-specific — check your agent's MCP
documentation. AIMX's [MCP Server](mcp.md) chapter documents the available
tools.

## Troubleshooting

### The agent does not see AIMX after `agent-setup` runs

- Confirm the destination was written: `aimx agent-setup --list` shows the
  destination path; check that it exists and contains the expected files.
- Restart the agent. Most agents only scan their plugin directory at
  startup.
- If the agent requires an explicit install step, re-read the installer
  output — the activation hint tells you exactly which command to run.

### "destination files already exist" error

Re-run with `--force` to overwrite when you want to replace the plugin
files on disk.

### `agent-setup` refuses to run as root

It is intentional. Per-user agent configuration lives under `$HOME`; if you
run the installer as root, it would drop files into root's home (or fail in
surprising ways with `sudo -u`). Run it as the user whose agent you are
configuring.

### MCP tools appear but calls fail with "Failed to load config"

The plugin's MCP command defaults to `/var/lib/aimx/` for AIMX's data
directory. If you set up AIMX with a different path, re-run with
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
is set to a non-default value, Goose and AIMX will both honour it —
check under `$XDG_CONFIG_HOME/goose/recipes/` instead.

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

### Hermes: AIMX tools missing after editing config.yaml

Hermes does not auto-reload MCP servers when `~/.hermes/config.yaml`
changes — you must run the in-app `/reload-mcp` slash command (or
restart Hermes) after pasting the snippet. Confirm the snippet sits
under the top-level `mcp_servers:` key (not nested inside another
section) and that YAML indentation uses spaces, not tabs. If you have
no other MCP servers configured, the block can be the entire
`mcp_servers:` section.
