# AIMX skill for OpenCode

This directory is the source tree for the OpenCode skill that wires AIMX
into OpenCode. Contents are bundled into the `aimx` binary at compile time
(via `include_dir!`) and installed by `aimx agent-setup opencode`.

## What gets installed

- `SKILL.md` — an agent-facing skill dropped into
  `~/.config/opencode/skills/aimx/SKILL.md`. Its body is the canonical
  AIMX primer (`agents/common/aimx-primer.md`); the installer assembles
  the final `SKILL.md` from a YAML header plus that primer so there is
  one source of truth.

OpenCode's MCP configuration lives in `opencode.json` / `opencode.jsonc`,
not alongside the skill. The installer does **not** mutate that file;
instead it prints the exact JSONC snippet you paste into the `mcp` section
of `opencode.json` (see Activation below).

## Install

```bash
aimx agent-setup opencode
```

Default skill destination: `~/.config/opencode/skills/aimx/SKILL.md`. After the
skill is written, copy the printed JSONC snippet into your
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

Restart OpenCode (or reload its config) after editing `opencode.json`.

## Overriding the data directory

If AIMX was set up with a non-default data directory, re-run the installer
with `--data-dir`:

```bash
aimx --data-dir /custom/path agent-setup opencode
```

The printed JSONC snippet will include `--data-dir /custom/path` in the
`command` array.

## Channel-trigger recipes

To have AIMX invoke `opencode run` automatically when an email arrives,
see the [Channel Recipes](../../book/channel-recipes.md#opencode)
chapter.

## Schema reference

OpenCode's skill and MCP conventions are documented at
<https://opencode.ai/docs>. OpenCode's skill format is compatible with
Claude Code's `SKILL.md` (YAML frontmatter with `name` and `description`,
followed by the skill body). MCP servers are configured under the root
`mcp.<name>` key with `command` as a single array combining binary + args.

## Design choice: print-the-snippet

AIMX follows the "print the activation command" pattern — the installer
writes the skill to disk and prints the exact JSONC block for you to
paste. We intentionally do NOT mutate `opencode.json` directly because:

1. The file is your config, not ours — we shouldn't silently rewrite it.
2. `opencode.json` may already contain other MCP servers or project
   customisations we would risk disturbing.
3. Making the activation step explicit is self-documenting and audit-safe.
