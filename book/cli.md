# CLI Reference

Every `aimx` subcommand with its flags. `aimx <command> --help` is authoritative; this page summarises.

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

Interactive setup wizard. Requires root. Generates TLS cert and DKIM keys, writes `/etc/aimx/config.toml`, installs a systemd (or OpenRC) unit for `aimx serve`, and drives DNS verification. Re-entrant: running it on an existing install skips install and jumps to DNS verification.

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

Print server health: configuration path, mailbox counts and unread counts, a per-mailbox table (Mailbox, Address, Total, Unread, Trust, Senders, Hooks), an Ownership section listing each mailbox's `owner` and whether the owner uid resolves on the host, DKIM key presence, SMTP service state, DNS record verification, and a pointer to `aimx logs` for recent service logs. Exits non-zero when any mailbox has an unresolvable owner so monitoring can detect orphans.

No flags.

See [Troubleshooting](troubleshooting.md).

### `aimx logs`

Tail or follow the `aimx serve` service log. Wraps `journalctl -u aimx` on systemd and `/var/log/aimx/*.log` / `/var/log/messages` on OpenRC.

| Flag | Default | Description |
|------|---------|-------------|
| `--lines <N>` | 50 | Number of trailing lines to show. |
| `-f`, `--follow` | off | Stream new lines as they arrive (like `journalctl -f`). |

## Mail operations

### `aimx send`

Compose an RFC 5322 message and submit it to `aimx serve` via `/run/aimx/aimx.sock`. Refuses root. The daemon handles DKIM signing and MX delivery.

The caller's euid must own the mailbox resolved from the `From:` local part. The daemon parses `From:` from the submitted body itself (no client-supplied `From-Mailbox:` header) and rejects sends whose mailbox owner does not match the caller with `not authorized: <local_part>@<domain>` (exit code 1). The catchall (`*@domain`) is inbound-only and is never accepted as an outbound sender.

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

Create a mailbox registering `<name>@<domain>` and directories under `inbox/<name>/` and `sent/<name>/` chowned `<owner>:<owner> 0700`. **Requires root** — non-root callers are rejected with exit code 2. The daemon hot-reloads the new mailbox; no restart needed. The `MAILBOX-CREATE` UDS verb is also root-only (per-verb `SO_PEERCRED` check). The reserved literals `catchall` and `aimx-catchall` are rejected with `Validation: reserved`.

| Flag | Description |
|------|-------------|
| `--owner <user>` | Linux user that owns the mailbox's storage and runs hooks. Required (the CLI prompts when omitted, defaulting to `<name>` if a user with that name already exists). Under `AIMX_NONINTERACTIVE=1` the default is accepted when available, otherwise the command errors hard so scripted installs fail fast. The user must resolve via `getpwnam(3)` on this host. |

### `aimx mailboxes list`

List mailboxes you own. Prints addresses, total count, and unread count. Non-root callers see only mailboxes whose owner uid matches their euid; the catchall is filtered out unless the caller is root or `aimx-catchall`.

| Flag | Description |
|------|-------------|
| `--all` | Root only. List every mailbox regardless of owner. Non-root callers passing `--all` get `--all requires root`. |

### `aimx mailboxes show <name>`

Print a mailbox's address, owner, effective trust policy, `trusted_senders`, configured hooks grouped by event, and inbox / sent / unread counts. Non-root callers may only inspect mailboxes they own.

### `aimx mailboxes delete <name>`

Delete a mailbox. **Requires root** (matching `MAILBOX-DELETE` UDS authorization). Refuses non-empty mailboxes with `ERR NONEMPTY` unless `--force` is passed. `catchall` cannot be deleted.

| Flag | Description |
|------|-------------|
| `-y`, `--yes` | Skip the confirmation prompt. |
| `--force` | Recursively wipe `inbox/<name>/` and `sent/<name>/` before deleting. Prompts before wiping unless paired with `--yes`. Refuses `catchall`. |

See [Mailboxes: Managing mailboxes](mailboxes.md#managing-mailboxes).

## Hook management

Alias: `aimx hook` works identically to `aimx hooks`.

Authorization: caller must own the target mailbox, or be root. Non-root callers operating on mailboxes they own use the daemon's UDS socket so changes hot-swap into the running config; the daemon verifies ownership via `SO_PEERCRED` and rejects with `EACCES not authorized` otherwise. Root with the daemon stopped falls back to a direct `config.toml` edit + restart hint; non-root with the daemon stopped hard-errors (cannot write the root-owned config).

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
| `--stdin <mode>` | `email` (default) pipes the raw `.md` to the hook's stdin; `none` closes stdin immediately. |
| `--timeout-secs <N>` | Hard subprocess timeout. Default `60`, range `[1, 600]`. SIGTERM at the limit, SIGKILL 5s later. |
| `--fire-on-untrusted` | Fire even when `trusted != "true"`. Only valid on `--event on_receive`. Rejected on `--event after_send`. |
| `--name <name>` | Optional. Matches `^[a-zA-Z0-9_][a-zA-Z0-9_.-]{0,127}$`. Must be globally unique across all mailboxes. When omitted, a derived 12-char hex name is used. |

Example (as the mailbox owner — no sudo needed when the daemon is running):

```bash
aimx hooks create \
  --mailbox accounts \
  --event on_receive \
  --cmd '["/usr/local/bin/claude", "-p", "Read the piped email and act on it.", "--dangerously-skip-permissions"]' \
  --stdin email \
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

Install the aimx plugin / skill for a supported agent into the current user's config directory. Refuses to run as root. Run with no arguments to launch the interactive checkbox TUI; pass `--list` (or call `aimx agents list`) to print the supported-agent registry and exit without installing.

The plugin bundle teaches the agent how to call aimx's MCP tools and includes a "Wiring yourself up as a mailbox hook" section with the verified `cmd` argv to use with [`aimx hooks create`](#aimx-hooks-create).

| Flag | Description |
|------|-------------|
| `<agent>` (positional) | Short name (e.g. `claude-code`, `codex`, `opencode`, `gemini`, `goose`, `openclaw`, `hermes`). Omit for the interactive TUI. |
| `--list` | Print the registry: agent name, destination path, activation hint. Same output as `aimx agents list`. |
| `--no-interactive` | Skip the TUI when no agent is named; print the same plain registry dump. Intended for scripting. |
| `--dangerously-allow-root` | Footgun. Bypass the root-refusal check and wire aimx into `/root`'s home. Prefer `sudo -u <user> aimx agents setup` on any machine with a regular user. |
| `--force` | Overwrite existing destination files without prompting. |
| `--print` | Print the plugin contents to stdout instead of writing to disk. |

See [Agent Integration](agent-integration.md) for per-agent activation steps.

### `aimx agents list`

Print the supported-agent registry as a plain table (agent name, destination path, activation hint).

### `aimx agents remove <agent>`

Inverse of `aimx agents setup`. Removes the plugin files under `$HOME`. Refuses to run as root.

| Flag | Description |
|------|-------------|
| `<agent>` (positional) | Short name; must match the agent previously passed to `aimx agents setup`. |
| `--dangerously-allow-root` | Footgun. Bypass the root-refusal check. |

## Utilities

### `aimx dkim-keygen`

Generate a 2048-bit RSA DKIM keypair under `/etc/aimx/dkim/` (private `0600`, public `0644`). Normally run automatically by `aimx setup`; use directly for key rotation.

| Flag | Default | Description |
|------|---------|-------------|
| `--selector <name>` | `aimx` | DKIM selector name (controls the DNS record `<selector>._domainkey.<domain>`). |
| `--force` | off | Overwrite existing keys. |
