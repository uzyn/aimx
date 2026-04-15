# AIMX plugin for Codex CLI

This directory is the source tree for the Codex CLI plugin that wires AIMX
into Codex. Contents are bundled into the `aimx` binary at compile time
(via `include_dir!`) and installed by `aimx agent-setup codex`.

## What gets installed

- `.codex-plugin/plugin.json` — Codex plugin manifest, including an
  `mcpServers.aimx` entry pointing at `/usr/local/bin/aimx mcp`.
- `skills/aimx/SKILL.md` — an agent-facing skill. Its body is the canonical
  AIMX primer (`agents/common/aimx-primer.md`); the installer assembles the
  final `SKILL.md` from a YAML header plus that primer so there is one
  source of truth.

## Install

```bash
aimx agent-setup codex
```

Default destination: `~/.codex/plugins/aimx/`. After install, restart
Codex CLI — the plugin is auto-discovered from that directory.

## Overriding the data directory

If AIMX was set up with a non-default data directory, re-run the installer
with `--data-dir`:

```bash
aimx --data-dir /custom/path agent-setup codex
```

The installer rewrites the plugin's `mcpServers.aimx.args` to include
`--data-dir /custom/path` before writing `plugin.json` to disk.

## Schema reference

The plugin manifest follows the Codex CLI plugin schema documented at
<https://github.com/openai/codex>. Codex CLI's MCP wiring primarily lives
in `~/.codex/config.toml`; plugin-managed MCP servers follow the same
shape under the plugin's `plugin.json`. The skill format mirrors Claude
Code's `SKILL.md` layout (YAML frontmatter with `name` and `description`,
followed by the skill body) and is portable across CLI agents that adopt
the same skill convention.

## Verification

Verify the plugin format and destination path against the current Codex
CLI documentation before relying on this install layout in production —
agent plugin formats drift between CLI releases.
