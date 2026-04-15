# Agent Integration

AIMX ships plugin/skill packages for popular AI agents and a one-command
installer, `aimx agent-setup <agent>`, that drops the right files into the
right per-user directory. After install, the agent discovers AIMX as both an
MCP server (so tool calls work) and an agent-facing primer (so the agent
knows when and how to use those tools).

This page covers what the installer does, the list of supported agents, and
how to wire AIMX in manually if your agent is not yet in the registry.

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

The matrix below tracks what is available in the current `aimx` binary. It
grows as more agents are landed in subsequent sprints.

| Agent | Install command | Destination | Activation |
|-------|-----------------|-------------|------------|
| Claude Code | `aimx agent-setup claude-code` | `~/.claude/plugins/aimx/` | Restart Claude Code; the plugin is auto-discovered from `~/.claude/plugins/`. |
| Codex CLI | `aimx agent-setup codex` | `~/.codex/plugins/aimx/` | Restart Codex CLI; the plugin is auto-discovered from `~/.codex/plugins/`. |
| OpenCode | `aimx agent-setup opencode` | `~/.config/opencode/skills/aimx/` | Paste the printed JSONC block into `opencode.json`, then restart OpenCode. |
| Gemini CLI | `aimx agent-setup gemini` | `~/.gemini/skills/aimx/` | Merge the printed JSON block into `~/.gemini/settings.json`, then restart Gemini CLI. |

More agents — Goose and OpenClaw — land in Sprint 30.

### Claude Code

Claude Code discovers plugins by scanning `~/.claude/plugins/`. The AIMX
plugin ships two pieces:

- `.claude-plugin/plugin.json` — manifest declaring the plugin and
  registering `aimx mcp` as an MCP server.
- `skills/aimx/SKILL.md` — a skill Claude Code loads when the conversation
  touches email, inboxes, or AIMX. The skill body is the canonical AIMX
  primer: MCP tool names and parameters, the on-disk storage layout, the
  frontmatter format, read/unread semantics, and the DKIM/SPF trust model.

Install:

```bash
aimx agent-setup claude-code
```

Custom data directory:

```bash
aimx --data-dir /custom/path agent-setup claude-code
```

The installer rewrites `mcpServers.aimx.args` to include
`--data-dir /custom/path` before writing `plugin.json` to disk.

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

Re-run with `--force` to overwrite. This is the expected behaviour when
you are upgrading AIMX and want the new plugin version on disk.

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
