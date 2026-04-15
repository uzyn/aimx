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

## Install

```bash
aimx agent-setup claude-code
```

Default destination: `~/.claude/plugins/aimx/`. After install, restart
Claude Code — the plugin is auto-discovered from that directory.

## Overriding the data directory

If AIMX was set up with a non-default data directory, re-run the installer
with `--data-dir`:

```bash
aimx --data-dir /custom/path agent-setup claude-code
```

The installer rewrites the plugin's `mcpServers.aimx.args` to include
`--data-dir /custom/path` before writing `plugin.json` to disk.

## Schema reference

The plugin manifest follows the Claude Code plugin schema documented at
<https://docs.claude.com/en/docs/claude-code/plugins>. The skill format
follows the Claude Code skill schema (YAML frontmatter with `name` and
`description`, followed by the skill body).
