# AIMX skill for Gemini CLI

This directory is the source tree for the Gemini CLI skill that wires
AIMX into Gemini. Contents are bundled into the `aimx` binary at compile
time (via `include_dir!`) and installed by `aimx agent-setup gemini`.

## What gets installed

- `SKILL.md` — an agent-facing skill dropped into
  `~/.gemini/skills/aimx/SKILL.md`. Its body is the canonical AIMX primer
  (`agents/common/aimx-primer.md`); the installer assembles the final
  `SKILL.md` from a YAML header plus that primer so there is one source
  of truth.

Gemini CLI's MCP configuration lives in `~/.gemini/settings.json`, not
alongside the skill. The installer does **not** mutate that file;
instead it prints the exact JSON block you merge into the `mcpServers`
section of `settings.json` (see Activation below).

## Install

```bash
aimx agent-setup gemini
```

Default skill destination: `~/.gemini/skills/aimx/SKILL.md`. After the skill is
written, merge the printed JSON block into `~/.gemini/settings.json`:

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

If `~/.gemini/settings.json` does not exist yet, create it with the
object above as its full contents. If it already has a `mcpServers` key,
add the `aimx` entry inside the existing object.

Restart Gemini CLI after editing `settings.json`.

## Overriding the data directory

If AIMX was set up with a non-default data directory, re-run the installer
with `--data-dir`:

```bash
aimx --data-dir /custom/path agent-setup gemini
```

The printed JSON block's `args` will include `--data-dir /custom/path`.

## Schema reference

Gemini CLI's skill and MCP conventions are documented at
<https://github.com/google-gemini/gemini-cli>. MCP servers are configured
in `~/.gemini/settings.json` under an object keyed `mcpServers.<name>`
with `command` + `args`, matching Claude Code's manifest shape. The skill
format uses YAML frontmatter with `name` and `description` followed by
the skill body.

## Design choice: print-the-snippet

AIMX follows the "print the activation command" pattern — the installer
writes the skill to disk and prints the exact JSON block for you to
merge into `settings.json`. We intentionally do NOT mutate
`~/.gemini/settings.json` directly because:

1. The file is your config, not ours — we shouldn't silently rewrite it.
2. `settings.json` may already contain other MCP servers or personal
   customisations we would risk disturbing.
3. Making the activation step explicit is self-documenting and audit-safe.
4. It matches FR-49: never mutate an agent's primary config file.
