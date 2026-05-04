# aimx skill for NanoClaw

AIMX (AI Mail Exchange) skill source tree for NanoClaw. This directory
wires aimx into [NanoClaw](https://nanoclaw.dev/), the lightweight
container-isolated personal AI agent built on Anthropic's Claude Agent
SDK. Contents are bundled into the `aimx` binary at compile time (via
`include_dir!`) and installed by `aimx agents setup nanoclaw`.

## What gets installed

- `skills/aimx/SKILL.md`: an agent-facing skill dropped into
  `<fork>/skills/aimx/SKILL.md`. Its body is the canonical aimx
  primer (`agents/common/aimx-primer.md`). The installer assembles
  the final `SKILL.md` from a YAML header plus that primer so there
  is one source of truth.
- `skills/aimx/references/`: detailed reference docs (MCP tool
  signatures, frontmatter schema, hooks, workflows, troubleshooting)
  copied from `agents/common/references/`.
- `<fork>/.mcp.json` is updated in place to add an `mcpServers.aimx`
  entry pointing at `/usr/local/bin/aimx mcp`. NanoClaw is the one
  integration where aimx mutates the agent's primary config file —
  see "Design choice" below for why.

## Where is the fork?

NanoClaw is a per-user fork of `qwibitai/nanoclaw`, not a globally
installed CLI, so there is no `~/.nanoclaw/` directory on disk. The
installer resolves the fork directory in this order:

1. The `NANOCLAW_HOME` environment variable, if set.
2. `~/nanoclaw` as the default (matches the README's `git clone …
   nanoclaw && cd nanoclaw` workflow).

If your fork lives elsewhere, export `NANOCLAW_HOME` before running
the installer:

```bash
NANOCLAW_HOME=/opt/my-nanoclaw aimx agents setup nanoclaw
```

## Install

```bash
aimx agents setup nanoclaw
```

Default skill destination: `~/nanoclaw/skills/aimx/SKILL.md` (or
`$NANOCLAW_HOME/skills/aimx/SKILL.md` when the env var is set).

## Activation

After the installer runs, restart NanoClaw so it loads the new
`.mcp.json` entry and discovers the skill. NanoClaw will then be
able to call the aimx mailbox/email tools.

To trigger NanoClaw on inbound mail without writing a one-shot
hook, add a NanoClaw scheduled job that polls
`email_list(folder: "inbox", unread_only: true)` on a cadence that
suits the workload (every minute for transactional mail, every
fifteen for digest-style inboxes).

## Overriding the data directory

If aimx was set up with a non-default data directory, re-run the
installer with `--data-dir`:

```bash
aimx --data-dir /custom/path agents setup nanoclaw
```

The `args` array in `<fork>/.mcp.json` will include
`--data-dir /custom/path`.

## Channel-trigger recipes

NanoClaw does not currently ship a one-shot `run` / `exec` CLI
suitable for `on_receive` hooks. For sub-second-latency reactions to
inbound mail, wire a different agent (Claude Code, Codex, or Hermes)
as the hook and let NanoClaw consume the resulting state via the
aimx MCP server on its next scheduled-job tick. See the per-agent
recipe in `book/agent-integration.md` for the worked example.

## Schema reference

NanoClaw's skill format is derived from the Claude Agent SDK
convention: a directory bundle with a YAML-frontmattered `SKILL.md`
plus optional siblings. The `metadata.nanoclaw.requires.bins`
manifest declares the binary the skill expects on `PATH`.

NanoClaw's MCP server configuration lives in `<fork>/.mcp.json`
under the top-level `mcpServers` object with `command` + `args`
(stdio transport).

## Design choice: in-place `.mcp.json` merge

NanoClaw is the one supported agent where aimx mutates the agent's
own MCP config file on the user's behalf. Every other supported
agent either has a first-class CLI for MCP registration (Claude
Code, Codex, OpenClaw) or expects the user to paste a snippet
(OpenCode, Gemini, Hermes, Goose).

NanoClaw exposes neither: there is no `nanoclaw mcp add` command,
and asking the user to hand-edit `<fork>/.mcp.json` (a JSON5-flavoured
file the user already customises in their fork) is the worst option
of the three. So `aimx agents setup nanoclaw` reads the existing
`.mcp.json` (if any), merges an `aimx` entry under `mcpServers`,
and writes the file back via temp-file + atomic rename. Other
servers in the file are preserved untouched. `--print` prints the
proposed JSON to stdout instead of writing; `--force` overwrites an
existing `aimx` entry without prompting.
