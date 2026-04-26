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

Print server health: configuration path, mailbox counts and unread counts, a per-mailbox table (Mailbox, Address, Total, Unread, Trust, Senders, Hooks), DKIM key presence, SMTP service state, DNS record verification, and a pointer to `aimx logs` for recent service logs.

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

Compose an RFC 5322 message and submit it to `aimx serve` via `/run/aimx/aimx.sock`. Does not require root. The daemon handles DKIM signing and MX delivery.

| Flag | Description |
|------|-------------|
| `--from <addr>` | Sender address. Must resolve to an explicitly configured (non-wildcard) mailbox. |
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

Create a mailbox registering `<name>@<domain>` and directories under `inbox/<name>/` and `sent/<name>/`. Hot-reloaded by the daemon; no restart needed.

| Flag | Description |
|------|-------------|
| `--owner <user>` | Linux user that owns the mailbox's storage. When omitted, the CLI prompts (defaulting to `<name>` if a user with that name already exists). Under `AIMX_NONINTERACTIVE=1` the default is accepted when available, otherwise the command errors hard so scripted installs fail fast. |

### `aimx mailboxes list`

List all mailboxes with addresses, total count, and unread count.

### `aimx mailboxes show <name>`

Print a mailbox's address, effective trust policy, `trusted_senders`, configured hooks grouped by event, and inbox / sent / unread counts.

### `aimx mailboxes delete <name>`

Delete a mailbox. Refuses non-empty mailboxes with `ERR NONEMPTY` unless `--force` is passed. `catchall` cannot be deleted.

| Flag | Description |
|------|-------------|
| `-y`, `--yes` | Skip the confirmation prompt. |
| `--force` | Recursively wipe `inbox/<name>/` and `sent/<name>/` before deleting. Prompts before wiping unless paired with `--yes`. Refuses `catchall`. |

See [Mailboxes: Managing mailboxes](mailboxes.md#managing-mailboxes).

## Hook management

Alias: `aimx hook` works identically to `aimx hooks`.

### `aimx hooks list`

List hooks. Prints a table of `NAME`, `MAILBOX`, `EVENT`, `ORIGIN`, `CMD`. Anonymous hooks (those without an explicit `name =`) appear under their derived 12-char hex name. Template-bound hooks show a blank `CMD` column; see `aimx hooks templates` for their argv shape.

| Flag | Description |
|------|-------------|
| `--mailbox <name>` | Filter to one mailbox. |

### `aimx hooks templates`

List hook templates enabled on this install. Prints a table of `NAME`, `DESCRIPTION`, `PARAMS`, `EVENTS`. Alias: `aimx hooks template-list`.

Empty output means no templates are enabled — run `sudo aimx setup` and tick the templates you want.

### `aimx hooks create`

Create a hook. Exactly one of `--template` or `--cmd` must be supplied.

**Template path (preferred).** `--template` + `--param` submit the request over the daemon's UDS socket. The daemon validates the template exists, all params are declared, the event is allowed, and stamps `origin = "mcp"` on the resulting hook. No root required when the daemon is running.

**Raw-cmd path (power user).** `--cmd` writes `/etc/aimx/config.toml` directly and SIGHUPs the daemon to hot-reload. Requires `sudo`. Stamps `origin = "operator"`.

| Flag | Description |
|------|-------------|
| `--mailbox <name>` | Owning mailbox. Must already exist. |
| `--event <event>` | `on_receive` or `after_send`. |
| `--template <name>` | Bind to a `[[hook_template]]`. Mutually exclusive with `--cmd`. |
| `--param KEY=VAL` | Declared parameter value for the template. Repeatable. |
| `--cmd <command>` | Raw shell command executed via `sh -c` when the hook fires. Requires `sudo`. Mutually exclusive with `--template`. |
| `--name <name>` | Optional. Matches `^[a-zA-Z0-9_][a-zA-Z0-9_.-]{0,127}$`. Must be globally unique across all mailboxes. When omitted, a derived 12-char hex name is used. |
| `--dangerously-support-untrusted` | Fire even when `trusted != "true"`. Only valid on `--event on_receive` + `--cmd`. Rejected with `--template` because MCP-origin hooks cannot opt into untrusted mail. |

Examples:

```bash
# Template hook (agent-origin, safe over UDS)
aimx hooks create \
  --mailbox accounts \
  --event on_receive \
  --template invoke-claude \
  --param prompt="File this and draft a reply."

# Raw-cmd hook (operator-origin, needs sudo)
sudo aimx hooks create \
  --mailbox support \
  --event on_receive \
  --cmd 'curl -fsS https://hooks.example.com/notify -d "$AIMX_SUBJECT"'
```

### `aimx hooks delete <name>`

Delete a hook by name. Works for both explicit and derived names (as shown in `aimx hooks list`). Operator-origin hooks require `sudo`; MCP-origin hooks go through the UDS socket.

| Flag | Description |
|------|-------------|
| `-y`, `--yes` | Skip the confirmation prompt. |

See [Hooks & Trust](hooks.md).

## Agent integration

### `aimx mcp`

Start the MCP server in stdio mode. Launched on-demand by MCP clients, not a background service.

No flags. See [MCP Server](mcp.md).

### `aimx agents setup [agent]`

Install the aimx plugin / skill for a supported agent into the current user's config directory, probe `$PATH` for the agent binary, and register an `invoke-<agent>-<username>` hook template over the daemon's UDS. Refuses to run as root. Run with no arguments to launch the interactive checkbox TUI; pass `--list` (or call `aimx agents list`) to print the supported-agent registry and exit without installing.

| Flag | Description |
|------|-------------|
| `<agent>` (positional) | Short name (e.g. `claude-code`, `codex`, `opencode`, `gemini`, `goose`, `openclaw`, `hermes`). Omit for the interactive TUI. |
| `--list` | Print the registry: agent name, destination path, activation hint. Same output as `aimx agents list`. |
| `--no-interactive` | Skip the TUI when no agent is named; print the same plain registry dump. Intended for scripting. |
| `--dangerously-allow-root` | Footgun. Bypass the root-refusal check and wire aimx into `/root`'s home. Prefer `sudo -u <user> aimx agents setup` on any machine with a regular user. |
| `--force` | Overwrite existing destination files without prompting. |
| `--print` | Print the plugin contents and the template that would be registered to stdout instead of writing to disk. |
| `--no-template` | Install plugin files only; skip the `$PATH` probe and `TEMPLATE-CREATE` over UDS. |
| `--redetect` | Re-probe `$PATH` and update the existing `invoke-<agent>-<username>` template's `cmd[0]` (for when the agent binary moved). |

See [Agent Integration](agent-integration.md) for per-agent activation steps.

### `aimx agents list`

Print the supported-agent registry as a plain table (agent name, destination path, activation hint).

### `aimx agents remove <agent>`

Inverse of `aimx agents setup`. Removes the plugin files under `$HOME` and submits `TEMPLATE-DELETE` for `invoke-<agent>-<caller_username>` to the daemon. Refuses to run as root. When the daemon is unreachable, plugin files are still removed and the command exits `2` with a pointer to `sudo aimx hooks prune --orphans`.

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
