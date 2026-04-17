# AIMX plugin for Claude Code

This directory is the source tree for the Claude Code plugin that wires AIMX
into Claude Code. Contents are bundled into the `aimx` binary at compile
time (via `include_dir!`) and installed by `aimx agent-setup claude-code`.

## What gets installed

- `.claude-plugin/plugin.json` — Claude Code plugin manifest, including an
  `mcpServers.aimx` entry pointing at `/usr/local/bin/aimx mcp`.
- `skills/aimx/SKILL.md` — an agent-facing skill. Its body is the canonical
  AIMX primer (`agents/common/aimx-primer.md`); the installer assembles the
  final `SKILL.md` from a YAML header plus that primer so there is one
  source of truth.
- `skills/aimx/references/` — detailed reference docs (MCP tool signatures,
  frontmatter schema, workflows, troubleshooting) copied from
  `agents/common/references/`. Progressive disclosure: Claude Code loads the
  primer first and reads references on demand.

## Install

```bash
aimx agent-setup claude-code
claude mcp add --scope user aimx /usr/local/bin/aimx mcp
```

Default destination: `~/.claude/plugins/aimx/`. The plugin itself is
auto-discovered from that directory, but the MCP server must be registered
with `claude mcp add` so both the interactive REPL and `claude -p`
headless invocations (used by channel-trigger recipes) can see it.
Restart Claude Code after both commands complete.

## Overriding the data directory

If AIMX was set up with a non-default data directory, re-run the installer
and the MCP registration with `--data-dir`:

```bash
aimx --data-dir /custom/path agent-setup claude-code
claude mcp add --scope user aimx /usr/local/bin/aimx --data-dir /custom/path mcp
```

The installer rewrites the plugin's `mcpServers.aimx.args` to include
`--data-dir /custom/path` before writing `plugin.json` to disk, and the
`claude mcp add` command threads the same override into the user-scope
MCP registry.

## Channel-trigger recipes

Installing the plugin gives Claude Code MCP access to AIMX. To wire it
the other way — have AIMX invoke `claude -p` automatically on inbound
email — see the
[Channel Recipes](../../book/channel-recipes.md#claude-code) chapter,
which has a copy-paste `config.toml` snippet and flag references.

## Schema reference

The plugin manifest follows the Claude Code plugin schema documented at
<https://docs.claude.com/en/docs/claude-code/plugins>. The skill format
follows the Claude Code skill schema (YAML frontmatter with `name` and
`description`, followed by the skill body).
