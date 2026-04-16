# AIMX skill for Codex CLI

This directory is the source tree for the Codex CLI skill that wires AIMX
into Codex. Contents are bundled into the `aimx` binary at compile time
(via `include_dir!`) and installed by `aimx agent-setup codex`.

## What gets installed

- `SKILL.md` at `~/.codex/skills/aimx/SKILL.md` — an agent-facing skill.
  Its body is the canonical AIMX primer (`agents/common/aimx-primer.md`);
  the installer assembles the final `SKILL.md` from a YAML header plus
  that primer so there is one source of truth.
- `references/` at `~/.codex/skills/aimx/references/` — detailed reference
  docs (MCP tool signatures, frontmatter schema, workflows, troubleshooting)
  copied from `agents/common/references/`.

## Install

```bash
aimx agent-setup codex
```

After install, the installer prints a `codex mcp add aimx -- /usr/local/bin/aimx mcp`
command that registers AIMX's MCP server with Codex CLI. Run it once,
then restart Codex CLI — the MCP server is now available.

## Overriding the data directory

If AIMX was set up with a non-default data directory, re-run the installer
with `--data-dir`:

```bash
aimx --data-dir /custom/path agent-setup codex
```

The printed `codex mcp add` command then includes `--data-dir /custom/path`
in the server command.

## Channel-trigger recipes

To have AIMX invoke `codex exec` automatically when an email arrives, see
the [Channel Recipes](../../book/channel-recipes.md#codex-cli) chapter.

## Why a skill, not a plugin directory

Earlier revisions of this installer shipped a `.codex-plugin/plugin.json`
that mirrored the Claude Code plugin shape. Validation against Codex CLI
0.117.0 confirmed that Codex CLI does **not** scan `~/.codex/plugins/` for
MCP servers; its MCP configuration lives exclusively in
`~/.codex/config.toml` (managed via `codex mcp add`). The installer now
ships only the skill and asks the user to run the canonical registration
command — no plugin manifest is written.
