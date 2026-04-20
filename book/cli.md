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

Stop the daemon and remove the installed init-system service file. Non-destructive. Leaves `/etc/aimx/` and `/var/lib/aimx/` intact.

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

Compose an RFC 5322 message and submit it to `aimx serve` via `/run/aimx/send.sock`. Does not require root. The daemon handles DKIM signing and MX delivery.

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

List hooks. Prints a table of `NAME`, `MAILBOX`, `EVENT`, `CMD`. Anonymous hooks (those without an explicit `name =`) appear under their derived 12-char hex name.

| Flag | Description |
|------|-------------|
| `--mailbox <name>` | Filter to one mailbox. |

### `aimx hooks create`

Create a hook. Pass `--name` to pick an explicit name; omit it to let AIMX derive a stable 12-char hex name from `sha256(event + cmd + dangerously_support_untrusted)`. Either way, the final name is printed on success.

| Flag | Description |
|------|-------------|
| `--mailbox <name>` | Owning mailbox. Must already exist. |
| `--event <event>` | `on_receive` or `after_send`. |
| `--cmd <command>` | Shell command executed via `sh -c` when the hook fires. |
| `--name <name>` | Optional. Matches `^[a-zA-Z0-9_][a-zA-Z0-9_.-]{0,127}$`. Must be globally unique across all mailboxes. When omitted, a derived name is used. |
| `--dangerously-support-untrusted` | Fire even when `trusted != "true"`. Only valid on `--event on_receive`. |

### `aimx hooks delete <name>`

Delete a hook by name. Works for both explicit and derived names (as shown in `aimx hooks list`).

| Flag | Description |
|------|-------------|
| `-y`, `--yes` | Skip the confirmation prompt. |

See [Hooks & Trust](hooks.md).

## Agent integration

### `aimx mcp`

Start the MCP server in stdio mode. Launched on-demand by MCP clients, not a background service.

No flags. See [MCP Server](mcp.md).

### `aimx agent-setup [agent]`

Install the aimx plugin / skill for a supported agent into the current user's config directory. Refuses to run as root. Run with no arguments (or `--list`) to print the supported-agent registry and exit without installing.

| Flag | Description |
|------|-------------|
| `<agent>` (positional) | Short name (e.g. `claude-code`, `codex`, `opencode`, `gemini`, `goose`, `openclaw`, `hermes`). Omit to print the registry. |
| `--list` | Print the registry: agent name, destination path, activation hint. Equivalent to running with no positional argument. |
| `--force` | Overwrite existing destination files without prompting. |
| `--print` | Print the plugin contents to stdout instead of writing to disk. |

See [Agent Integration](agent-integration.md) for per-agent activation steps.

## Utilities

### `aimx dkim-keygen`

Generate a 2048-bit RSA DKIM keypair under `/etc/aimx/dkim/` (private `0600`, public `0644`). Normally run automatically by `aimx setup`; use directly for key rotation.

| Flag | Default | Description |
|------|---------|-------------|
| `--selector <name>` | `aimx` | DKIM selector name (controls the DNS record `<selector>._domainkey.<domain>`). |
| `--force` | off | Overwrite existing keys. |
