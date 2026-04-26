# aimx recipe for Goose

AIMX (AI Mail Exchange) recipe source tree for Goose. This directory wires
aimx into [Goose](https://goose-docs.ai/). Contents are bundled into the
`aimx` binary at compile time (via `include_dir!`) and installed by
`aimx agents setup goose`.

## What gets installed

- `aimx.yaml`: a Goose recipe dropped into `~/.config/goose/recipes/aimx.yaml`.
  The recipe bundles:
  - `title` + `description`: recipe metadata Goose surfaces in `goose
    recipe list`.
  - `extensions`: a stdio entry for `aimx mcp` so running the recipe
    automatically launches the aimx MCP server.
  - `prompt`: the canonical aimx primer (`agents/common/aimx-primer.md`)
    indented as a YAML block scalar so there is one source of truth for
    agent-facing instructions.

The installer assembles `aimx.yaml` from a YAML header plus the primer at
install time. The raw `.header` file is not shipped.

## Install

```bash
aimx agents setup goose
```

Default recipe destination: `~/.config/goose/recipes/aimx.yaml`.

## Activation

Once the recipe is installed, run it with:

```bash
goose run --recipe aimx
```

Goose auto-discovers recipe files in `~/.config/goose/recipes/` and picks
them up by the filename stem, so `aimx.yaml` is invoked as `--recipe
aimx`.

## Team / org-wide recipe sharing

If you set the `GOOSE_RECIPE_GITHUB_REPO` environment variable to a
GitHub repo path (e.g. `myorg/goose-recipes`), Goose loads recipes from
that repo instead of (or in addition to) your local directory. In that
case, `aimx agents setup goose` still writes the recipe locally, and the
activation hint tells you to commit `~/.config/goose/recipes/aimx.yaml`
into your team repo so every user can invoke it.

## Overriding the data directory

If aimx was set up with a non-default data directory, re-run the
installer with `--data-dir`:

```bash
aimx --data-dir /custom/path agents setup goose
```

The recipe's `extensions[0].args` will include `--data-dir /custom/path`
before `mcp`.

## Channel-trigger recipes

To have aimx invoke `goose run` automatically when an email arrives,
see the [Hook Recipes](../../book/hook-recipes.md#goose) chapter.

## Schema reference

Goose recipe schema is documented at
<https://goose-docs.ai/docs/guides/recipes>. The recipe format requires
`title` and `description` plus at least one of `instructions` or
`prompt`. Extensions follow the `type: stdio` shape with `name`, `cmd`,
and `args`.

## Design choice: recipe-based integration

Goose's native integration shape is different from the skills-based
agents (Claude Code, Codex, OpenCode, Gemini). A recipe bundles both the
MCP extension config AND the agent-facing instructions in one YAML
file, so there is no separate "paste this JSON snippet" step. Running
the recipe starts the aimx MCP server automatically: the installer
writes one file and prints one activation command, so aimx never
mutates an agent's primary config file.
