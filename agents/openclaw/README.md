# AIMX skill for OpenClaw

This directory is the source tree for the OpenClaw skill that wires
AIMX into [OpenClaw](https://docs.openclaw.ai/). Contents are bundled
into the `aimx` binary at compile time (via `include_dir!`) and
installed by `aimx agent-setup openclaw`.

## What gets installed

- `skills/aimx/SKILL.md` — an agent-facing skill dropped into
  `~/.openclaw/skills/aimx/SKILL.md`. Its body is the canonical AIMX
  primer (`agents/common/aimx-primer.md`); the installer assembles the
  final `SKILL.md` from a YAML header plus that primer so there is one
  source of truth.
- `skills/aimx/references/` — detailed reference docs (MCP tool signatures,
  frontmatter schema, workflows, troubleshooting) copied from
  `agents/common/references/`.

OpenClaw's MCP configuration lives in `~/.openclaw/openclaw.json` (a
JSON5 file) under the `mcpServers` key. The installer does **not**
mutate that file; instead it prints an `openclaw mcp set` command you
paste into your shell to register AIMX's MCP server with one step.

## Install

```bash
aimx agent-setup openclaw
```

Default skill destination: `~/.openclaw/skills/aimx/SKILL.md`.

## Activation

After the skill is installed, register AIMX's MCP server with OpenClaw's
built-in CLI:

```bash
openclaw mcp set aimx '{"command":"/usr/local/bin/aimx","args":["mcp"]}'
```

That command edits `~/.openclaw/openclaw.json` for you — no config-file
hand-editing. Restart the OpenClaw gateway after registration so the
new server is loaded.

## Overriding the data directory

If AIMX was set up with a non-default data directory, re-run the
installer with `--data-dir`:

```bash
aimx --data-dir /custom/path agent-setup openclaw
```

The printed `openclaw mcp set` command's JSON will include
`--data-dir /custom/path` in the `args` array.

## Channel-trigger recipes

OpenClaw does not currently ship a non-interactive `run` / `exec` CLI
suitable for channel triggers. See the
[Channel Recipes](../../book/channel-recipes.md#openclaw) chapter for
the current recommendation (wire a different agent as the trigger and
let OpenClaw consume the result via its own pipeline).

## Schema reference

OpenClaw's skill format is documented at
<https://docs.openclaw.ai/tools/skills>. Skills use YAML frontmatter
with `name` + `description` (required), optional `version`, and
optional `metadata.openclaw` declarations for runtime requirements.
The body is agent-facing instructions.

OpenClaw's MCP server configuration lives in
`~/.openclaw/openclaw.json` under `mcpServers.<name>` with `command` +
`args` (stdio transport). The CLI reference for `openclaw mcp set` is
documented at <https://docs.openclaw.ai/cli/mcp>.

## Design choice: CLI-based activation

OpenClaw provides a first-class `openclaw mcp set <name> <json>` CLI
that registers an MCP server non-interactively. AIMX uses that command
as the activation step rather than asking users to hand-edit
`~/.openclaw/openclaw.json` (a JSON5 file AIMX does not want to parse
and rewrite). This matches FR-49: the installer writes the skill to
disk and prints one exact command the user runs.
