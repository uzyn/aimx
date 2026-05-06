# CLI Reference

Every `aimx` subcommand and its flags. `aimx <command> --help` is authoritative; this page summarises.

## Global flags

Accepted on every subcommand.

| Flag | Env var | Description |
|------|---------|-------------|
| `--data-dir <path>` | `AIMX_DATA_DIR` | Override the mailbox data directory (default `/var/lib/aimx`). The flag wins when both are set. |

For the full set of environment variables (`AIMX_DATA_DIR`, `AIMX_CONFIG_DIR`, `AIMX_TEST_MAIL_DROP`, `NO_COLOR`), see [Configuration: Environment variables](configuration.md#environment-variables).

## Daemon and setup

### `aimx serve`

Start the embedded SMTP listener daemon. Managed by systemd / OpenRC in normal operation.

| Flag | Default | Description |
|------|---------|-------------|
| `--bind <addr>` | `0.0.0.0:25` | Bind address for the SMTP listener. |
| `--tls-cert <path>` | *(from setup)* | PEM file for the STARTTLS certificate. |
| `--tls-key <path>` | *(from setup)* | PEM file for the STARTTLS private key. |

See [Setup](setup.md) for service installation and [Configuration](configuration.md) for config file details.

### `aimx setup [domain]`

Interactive setup wizard. Requires root. Generates STARTTLS cert and DKIM keys, writes `/etc/aimx/config.toml`, installs a systemd (or OpenRC) unit for `aimx serve`, and drives DNS verification. Re-entrant: running it on an existing install skips install and jumps to DNS verification.

| Flag | Description |
|------|-------------|
| `<domain>` (positional) | Domain to configure (e.g. `agent.yourdomain.com`). Prompted if omitted. |
| `--verify-host <url>` | Override the verifier service host for this invocation. |

See [Setup](setup.md) for the full walkthrough.

### `aimx uninstall`

Stop the daemon, remove the init-system service file, and delete the installed `aimx` binary itself so a subsequent `install.sh` run starts from a clean slate. Leaves `/etc/aimx/` and `/var/lib/aimx/` intact — wipe them manually with `sudo rm -rf /etc/aimx /var/lib/aimx` if you want a full purge.

| Flag | Description |
|------|-------------|
| `-y`, `--yes` | Skip the confirmation prompt. |

### `aimx portcheck`

Check port 25 connectivity (outbound EHLO + inbound EHLO probe). Requires root.

| Flag | Description |
|------|-------------|
| `--verify-host <url>` | Override the verifier service host for this invocation. |

See [Setup: End-to-end verification](setup.md#end-to-end-verification).

## Diagnostics

### `aimx doctor`

Print server health: config path, per-mailbox totals and unread counts, ownership status, DKIM key presence, SMTP service state, DNS record verification, and a pointer to `aimx logs`. Exits non-zero when any mailbox has an unresolvable owner so monitoring can detect orphans.

The Service section also prints `Client version:` (the CLI binary you just invoked) and `Server version:` (probed from the running daemon). When they differ, the on-disk binary is newer than the daemon — restart the service so it picks up the new build. See [Troubleshooting: Version drift](troubleshooting.md#version-drift-between-client-and-daemon).

No flags.

### `aimx logs`

Tail or follow the `aimx serve` service log. Wraps `journalctl -u aimx` on systemd and `/var/log/aimx/*.log` / `/var/log/messages` on OpenRC.

| Flag | Default | Description |
|------|---------|-------------|
| `--lines <N>` | 50 | Number of trailing lines to show. |
| `-f`, `--follow` | off | Stream new lines as they arrive (like `journalctl -f`). |

## Mail operations

### `aimx send`

Compose an RFC 5322 message and submit it to `aimx serve` via `/run/aimx/aimx.sock`. Refuses root. The daemon handles DKIM signing and MX delivery.

The caller's euid must own the mailbox resolved from the `From:` local part; sends from another owner's mailbox are rejected with `not authorized: <local_part>@<domain>`. The catchall (`*@domain`) is inbound-only and is never accepted as an outbound sender. See [Security: Per-action authorization](security.md#per-action-authorization).

| Flag | Description |
|------|-------------|
| `--from <addr>` | Sender address. Must resolve to an explicitly configured (non-wildcard) mailbox owned by the caller. |
| `--to <addr>` | Recipient address. |
| `--subject <text>` | Subject line. |
| `--body <text>` | Plain-text body. |
| `--reply-to <msg-id>` | Sets the `In-Reply-To` header for threading. |
| `--references <chain>` | Sets the full `References` header. Needed only for multi-step threads where `In-Reply-To` alone is insufficient. |
| `--attachment <path>` | Attach a file. Repeatable for multiple attachments. |

See [Mailboxes: Sending email](mailboxes.md#sending-email).

### `aimx ingest <rcpt>`

Read a raw `.eml` message from stdin, parse it, and write the Markdown frontmatter file to the mailbox that routes for `<rcpt>`. Called in-process by `aimx serve`; available as a CLI for manual ingestion and testing.

```bash
aimx ingest catchall@agent.yourdomain.com < message.eml
```

## Mailbox management

Alias: `aimx mailbox` works identically to `aimx mailboxes`.

### `aimx mailboxes create <name>`

Register `<name>@<domain>` and create `inbox/<name>/` and `sent/<name>/` chowned `<owner>:<owner> 0700`. Owner-gated, not root-gated: non-root callers create mailboxes owned by their own uid, root may pass `--owner <user>` to create one owned by another uid. The reserved literals `catchall` and `aimx-catchall` are rejected.

When `aimx serve` is running, the change hot-reloads with no restart. When the daemon is stopped, root falls back to a direct `config.toml` edit; non-root exits with code 2. See [Troubleshooting: daemon must be running](troubleshooting.md#aimx-mailboxes-create--delete-exits-with-daemon-must-be-running-for-non-root-mailbox-crud).

| Flag | Description |
|------|-------------|
| `--owner <user>` | Linux user that should own the mailbox's storage and run hooks. Honored only when run as root. Non-root callers passing `--owner <other>` get a soft warning to stderr (`--owner ignored for non-root callers; mailbox will be owned by <caller>`) and the daemon synthesizes the correct owner from `SO_PEERCRED`. Under root, the CLI prompts when omitted (default `<name>` if such a user exists). The user must resolve via `getpwnam(3)` on this host. |

### `aimx mailboxes list`

List mailboxes you own. Prints addresses, total count, and unread count. Non-root callers see only mailboxes whose owner uid matches their euid; the catchall is filtered out unless the caller is root or `aimx-catchall`.

| Flag | Description |
|------|-------------|
| `--all` | Root only. List every mailbox regardless of owner. Non-root callers passing `--all` get `--all requires root`. |

### `aimx mailboxes show <name>`

Print a mailbox's address, owner, effective trust policy, `trusted_senders`, configured hooks grouped by event, and inbox / sent / unread counts. Non-root callers may only inspect mailboxes they own.

### `aimx mailboxes delete <name>`

Delete a mailbox. Owner-gated: non-root callers may only delete mailboxes they own. Refuses non-empty mailboxes with `ERR NONEMPTY` unless `--force` is passed. `catchall` cannot be deleted with or without `--force`.

| Flag | Description |
|------|-------------|
| `-y`, `--yes` | Skip the confirmation prompt. |
| `--force` | Recursively wipe `inbox/<name>/` and `sent/<name>/` before deleting. Daemon-side wipe runs under per-mailbox lock + `CONFIG_WRITE_LOCK`, so the wipe and the config rewrite are atomic together. Prompts before wiping unless paired with `--yes`. Refuses `catchall`. |

See [Mailboxes: Managing mailboxes](mailboxes.md#managing-mailboxes).

## Hook management

Alias: `aimx hook` works identically to `aimx hooks`. Authorization: caller must own the target mailbox, or be root. When `aimx serve` is running, hook CRUD hot-swaps into the live config with no restart. See [Security: Per-action authorization](security.md#per-action-authorization).

### `aimx hooks list`

List hooks. Non-root callers see only hooks on mailboxes they own. Prints a table of `NAME`, `MAILBOX`, `EVENT`, `CMD`. Anonymous hooks (those without an explicit `name =`) appear under their derived 12-char hex name.

| Flag | Description |
|------|-------------|
| `--mailbox <name>` | Filter to one mailbox you own. |
| `--all` | Root only. List hooks on every mailbox. |

### `aimx hooks create`

Create a hook. `--cmd` takes the argv as a JSON array string; `cmd[0]` must be an absolute path.

| Flag | Description |
|------|-------------|
| `--mailbox <name>` | Owning mailbox. Must already exist and be owned by the caller (or caller is root). |
| `--event <event>` | `on_receive` or `after_send`. |
| `--cmd <json-array>` | Argv exec'd directly when the hook fires. Required. JSON array string with `cmd[0]` as an absolute path. No shell wrapping — wrap in `["/bin/sh", "-c", "..."]` explicitly when you need shell expansion. |
| `--timeout-secs <N>` | Hard subprocess timeout. Default `60`, range `[1, 600]`. SIGTERM at the limit, SIGKILL 5s later. |
| `--fire-on-untrusted` | Fire even when `trusted != "true"`. Only valid on `--event on_receive`. Rejected on `--event after_send`. |
| `--name <name>` | Optional. Matches `^[a-zA-Z0-9_][a-zA-Z0-9_.-]{0,127}$`. Must be globally unique across all mailboxes. When omitted, a derived 12-char hex name is used. |

The raw `.md` (frontmatter + body) is always piped to the hook's stdin and the same path is also exposed as `$AIMX_FILEPATH`. If your hook only needs the subject or sender, read `$AIMX_SUBJECT` / `$AIMX_FROM` and ignore stdin — the daemon writes the full email but does not require the child to consume it.

Example (as the mailbox owner — no sudo needed when the daemon is running):

```bash
aimx hooks create \
  --mailbox accounts \
  --event on_receive \
  --cmd '["/usr/local/bin/claude", "-p", "Read the piped email and act on it.", "--dangerously-skip-permissions"]' \
  --name accounts_claude
```

See [Hook Recipes](hook-recipes.md) for verified per-agent invocations.

### `aimx hooks delete <name>`

Delete a hook by name. Works for both explicit and derived names (as shown in `aimx hooks list`). Authorization is the same as `create`: caller must own the hook's mailbox, or be root.

| Flag | Description |
|------|-------------|
| `-y`, `--yes` | Skip the confirmation prompt. |

See [Hooks & Trust](hooks.md).

## Agent integration

### `aimx mcp`

Start the MCP server in stdio mode. Launched on-demand by MCP clients, not a background service.

No flags. See [MCP Server](mcp.md).

### `aimx agents setup [agent]`

Install the aimx skill for a supported agent into the current user's config directory and (for Claude Code and Codex CLI) auto-register the aimx MCP server via `claude mcp add` / `codex mcp add`. Refuses to run as root. Run with no arguments to launch the interactive checkbox TUI; pass `--list` (or call `aimx agents list`) to print the supported-agent registry and exit without installing.

The skill bundle teaches the agent how to call aimx's MCP tools and includes a "Wiring yourself up as a mailbox hook" section with the verified `cmd` argv to use with [`aimx hooks create`](#aimx-hooks-create).

When installing for `claude-code`, the installer also removes any pre-existing `~/.claude/plugins/aimx/` from the older plugin layout so the new skills install isn't shadowed.

| Flag | Description |
|------|-------------|
| `<agent>` (positional) | Short name (e.g. `claude-code`, `codex`, `opencode`, `gemini`, `goose`, `openclaw`, `hermes`). Omit for the interactive TUI. |
| `--list` | Print the registry: agent name, destination path, activation hint. Same output as `aimx agents list`. |
| `--no-interactive` | Skip the TUI when no agent is named; print the same plain registry dump. Intended for scripting. |
| `--dangerously-allow-root` | Bypass the root-refusal check and wire AIMX into `/root`'s home. Prefer `sudo -u <user> aimx agents setup` on any machine with a regular user. |
| `--force` | Overwrite existing destination files without prompting. |
| `--print` | Print the skill contents and activation hint to stdout instead of writing to disk or invoking any MCP CLI. |

See [Agent Integration](agent-integration.md) for per-agent activation steps.

### `aimx agents list`

Print the supported-agent registry as a plain table (agent name, destination path, activation hint).

### `aimx agents remove <agent>`

Inverse of `aimx agents setup`. Removes the skill files under `$HOME` and prints an agent-specific cleanup hint pointing at any external command you still need to run (for example `claude mcp remove aimx`). Refuses to run as root.

| Flag | Description |
|------|-------------|
| `<agent>` (positional) | Short name; must match the agent previously passed to `aimx agents setup`. |
| `--dangerously-allow-root` | Bypass the root-refusal check. |

## Utilities

### `aimx dkim-keygen`

Generate a 2048-bit RSA DKIM keypair under `/etc/aimx/dkim/` (private `0600`, public `0644`). Normally run automatically by `aimx setup`; use directly for key rotation.

| Flag | Default | Description |
|------|---------|-------------|
| `--selector <name>` | `aimx` | DKIM selector name (controls the DNS record `<selector>._domainkey.<domain>`). |
| `--force` | off | Overwrite existing keys. |

## UDS protocol verbs

`aimx serve` exposes a small `AIMX/1` request set on `/run/aimx/aimx.sock` (mode `0666`). The CLI subcommands above and the [`aimx mcp`](mcp.md) tools all submit these verbs; the daemon resolves the caller's uid via `SO_PEERCRED` and runs `auth::authorize` server-side. Operators do not normally speak the wire format directly.

| Verb | Direction | Authorization | Used by |
|------|-----------|---------------|---------|
| `SEND` | request + body (RFC 5322 message) | caller uid must own the mailbox resolved from `From:` | `aimx send`, `email_send` / `email_reply` MCP tools |
| `MARK-READ` / `MARK-UNREAD` | header (mailbox + path) | caller uid must own the mailbox | `email_mark_read` / `email_mark_unread` MCP tools |
| `MAILBOX-CREATE` | header + JSON body | caller uid synthesized as owner from `SO_PEERCRED` (root may pass `Owner:` for cross-uid creates) | `aimx mailboxes create`, `mailbox_create` MCP tool |
| `MAILBOX-DELETE` | header (mailbox) | caller uid must own the mailbox | `aimx mailboxes delete` (incl. `--force`), `mailbox_delete` MCP tool |
| `MAILBOX-LIST` | request only | none server-side; the response is filtered to caller-owned rows (root sees all) | `aimx mailboxes list`, `mailbox_list` MCP tool, every other MCP tool's resolution pre-flight |
| `HOOK-CREATE` | header + JSON body | caller uid must own the hook's mailbox | `aimx hooks create` (UDS path), `hook_create` MCP tool |
| `HOOK-DELETE` | header (hook name) | caller uid must own the hook's mailbox; operator-origin hooks are CLI-only | `aimx hooks delete` (UDS path), `hook_delete` MCP tool |
| `HOOK-LIST` | request only | none server-side; the response is filtered to hooks on caller-owned mailboxes (root sees all) | `hook_list` MCP tool |
| `VERSION` | request only | none — payload is daemon build metadata only | `aimx doctor`'s `Server version:` line |
