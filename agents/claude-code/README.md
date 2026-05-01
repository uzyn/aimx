# aimx skill for Claude Code

AIMX (AI Mail Exchange) skill source tree for Claude Code. This directory
wires aimx into Claude Code. Contents are bundled into the `aimx` binary
at compile time (via `include_dir!`) and installed by
`aimx agents setup claude-code`.

## What gets installed

- `SKILL.md`: an agent-facing skill. Its body is the canonical aimx primer
  (`agents/common/aimx-primer.md`); the installer assembles the final
  `SKILL.md` from a YAML header plus that primer so there is one source
  of truth.
- `references/`: detailed reference docs (MCP tool signatures, frontmatter
  schema, workflows, troubleshooting) copied from
  `agents/common/references/`. Progressive disclosure: Claude Code loads
  the primer first and reads references on demand.

## Install

```bash
aimx agents setup claude-code
```

Default destination: `~/.claude/skills/aimx/`. Claude Code auto-discovers
user-scope skills under `~/.claude/skills/`. The installer also registers
the aimx MCP server with Claude Code by shelling out to `claude mcp add
--scope user aimx -- /usr/local/bin/aimx mcp`. If `claude` is not on
`PATH`, the installer prints the equivalent command for the user to run
manually. Restart Claude Code after install so the new MCP server is
loaded.

## Overriding the data directory

If aimx was set up with a non-default data directory, re-run the installer
with `--data-dir`; the override is threaded into the `claude mcp add`
invocation:

```bash
aimx --data-dir /custom/path agents setup claude-code
```

## Channel-trigger recipes

Installing the skill gives Claude Code MCP access to aimx. To wire it the
other way, have aimx invoke `claude -p` automatically on inbound email.
See the [Hook Recipes](../../book/hook-recipes.md#claude-code) chapter,
which has a copy-paste `config.toml` snippet and flag references.

## Schema reference

The skill format follows the Claude Code skill schema (YAML frontmatter
with `name` and `description`, followed by the skill body), documented at
<https://docs.claude.com/en/docs/claude-code/skills>.
