# aimx skill for Hermes Agent

AIMX (AI Mail Exchange) skill source tree for Hermes Agent. This directory
wires aimx into [Hermes Agent](https://hermes-agent.nousresearch.com/) by
Nous Research. Contents are bundled into the `aimx` binary at compile time
(via `include_dir!`) and installed by `aimx agent-setup hermes`.

## What gets installed

- `skills/aimx/SKILL.md`: an agent-facing skill dropped into
  `~/.hermes/skills/aimx/SKILL.md`. Its body is the canonical aimx
  primer (`agents/common/aimx-primer.md`). The installer assembles the
  final `SKILL.md` from a YAML header plus that primer so there is one
  source of truth.
- `skills/aimx/references/`: detailed reference docs (MCP tool signatures,
  frontmatter schema, workflows, troubleshooting) copied from
  `agents/common/references/`.

Hermes' MCP configuration lives in `~/.hermes/config.yaml` (a YAML file)
under the top-level `mcp_servers:` key. The installer does **not** mutate
that file. Instead it prints a YAML snippet you paste in, then refresh
the running agent with the in-app `/reload-mcp` slash command.

## Install

```bash
aimx agent-setup hermes
```

Default skill destination: `~/.hermes/skills/aimx/SKILL.md`.

## Activation

After the skill is installed, register the aimx MCP server by adding the
following block to `~/.hermes/config.yaml` under the top-level
`mcp_servers:` key (create the key if it does not yet exist):

```yaml
mcp_servers:
  aimx:
    command: /usr/local/bin/aimx
    args: [mcp]
    enabled: true
```

Save the file, then run `/reload-mcp` inside Hermes to pick up the new
server without restarting the agent. Hermes will discover the aimx
mailbox/email tools automatically.

## Overriding the data directory

If aimx was set up with a non-default data directory, re-run the
installer with `--data-dir`:

```bash
aimx --data-dir /custom/path agent-setup hermes
```

The printed YAML snippet's `args` array will become
`[--data-dir, /custom/path, mcp]`.

## Schema reference

Hermes' skill format is documented at
<https://hermes-agent.nousresearch.com/docs/developer-guide/creating-skills>.
Skills use YAML frontmatter with `name`, `description`, `version`,
`author`, `license` (all required) plus an optional `metadata.hermes`
block declaring tags and required toolsets. The body is agent-facing
instructions.

Hermes' MCP server configuration lives in `~/.hermes/config.yaml` under
`mcp_servers.<name>` with `command` + `args` + `enabled` (stdio
transport). The MCP reference is at
<https://hermes-agent.nousresearch.com/docs/user-guide/features/mcp/>.

## Design choice: print-snippet activation

Hermes does not currently expose a shell-side CLI for registering
external MCP servers. `hermes mcp serve` runs Hermes itself as an MCP
server (the opposite direction), and the canonical registration path per
the official docs is editing `~/.hermes/config.yaml` directly. aimx
keeps to the same pattern: the installer writes the skill to disk and
prints the exact YAML block you paste into your config, mirroring the
Gemini CLI / OpenCode integrations rather than the OpenClaw `mcp set`
pattern.
