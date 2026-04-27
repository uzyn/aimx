# Configuration

aimx uses a single TOML configuration file for all settings.

## Config file location

The default config file is at `/etc/aimx/config.toml` (mode `0640`, owner `root:root`). It is created automatically by `aimx setup`. Config and DKIM secrets live under `/etc/aimx/` (root-owned, unreadable by non-root processes), separate from mailbox data under `/var/lib/aimx/`.

### Data directory override

The **data directory** (`/var/lib/aimx/` by default) holds mailboxes only. Config and DKIM keys are under `/etc/aimx/`. To relocate it:

```bash
# CLI flag (works with any command)
aimx --data-dir /custom/path doctor

# Environment variable
export AIMX_DATA_DIR=/custom/path
aimx doctor
```

The `--data-dir` flag takes precedence over the environment variable.

### Config directory override

For tests or non-standard installs, override the config directory with:

```bash
export AIMX_CONFIG_DIR=/custom/etc/path
```

This changes where `config.toml` and the DKIM keypair (`dkim/private.key`, `dkim/public.key`) are read from. Under normal operation you should **not** set this. `aimx setup` writes to `/etc/aimx/` and every command picks it up from there.

## Environment variables

| Variable | Default | Purpose |
|----------|---------|---------|
| `AIMX_DATA_DIR` | `/var/lib/aimx` | Override the mailbox data directory. Equivalent to `--data-dir`. The flag wins when both are set. |
| `AIMX_CONFIG_DIR` | `/etc/aimx` | Override the config + DKIM directory. For tests and non-standard installs only. |
| `AIMX_TEST_MAIL_DROP` | *(unset)* | When set to a directory path, `aimx serve` writes every outbound submission to that directory instead of delivering via SMTP. The daemon logs a startup warning so it cannot be left on in production by accident. |
| `NO_COLOR` | *(unset)* | Standard convention. When set to any value, aimx CLI output disables ANSI color. |

Hook commands receive additional `AIMX_*` env vars carrying the triggering email's header fields. See [Hooks & Trust: Hook context](hooks.md#hook-context-env-vars-and-stdin).

## Settings reference

### Top-level settings

| Setting | Type | Default | Description |
|---------|------|---------|-------------|
| `domain` | string | *(required)* | The email domain (e.g. `agent.yourdomain.com`) |
| `data_dir` | string | `/var/lib/aimx` | Directory for storing mailboxes (config and keys live under `/etc/aimx/`) |
| `dkim_selector` | string | `dkim` | DKIM selector name used in DNS records |
| `trust` | string | `none` | Default trust policy for every mailbox: `none` or `verified`. Per-mailbox `trust` replaces this default. |
| `trusted_senders` | array | `[]` | Default allowlist of glob patterns applied to every mailbox. Per-mailbox `trusted_senders` replaces this list (no merging). |
| `verify_host` | string | `https://check.aimx.email` | Base URL of the verifier service used by `aimx portcheck` and `aimx setup`. Can be overridden per-invocation with the `--verify-host` flag. |
| `enable_ipv6` | bool | `false` | Advanced. Opt into IPv6 outbound delivery. See [IPv6 delivery](#ipv6-delivery-advanced). |

`aimx setup` asks for a list of trusted sender addresses interactively on
the first run (comma-separated, accepts plain addresses and globs like
`*@company.com`). A non-empty list sets `trust = "verified"` with that
allowlist; leaving the prompt blank sets `trust = "none"` and the wizard
prints a loud warning that hooks will NOT fire for inbound email. On
re-entry the existing top-level values on disk are preserved and the
prompt is skipped.

### Mailbox settings

Mailboxes are defined under `[mailboxes.<name>]`:

| Setting | Type | Default | Description |
|---------|------|---------|-------------|
| `address` | string | *(required)* | Email address pattern (e.g. `support@domain.com` or `*@domain.com` for catchall) |
| `owner` | string | *(required)* | Linux username that owns the mailbox storage and runs hooks. Must resolve via `getpwnam(3)` at config load. The reserved username `aimx-catchall` is used for catchall mailboxes (created on demand by `aimx setup`); the reserved value `root` is allowed but only settable by hand-editing `config.toml`. |
| `trust` | string | *(inherited)* | Override the global default. Allowed values: `none` or `verified`. Omit to inherit. |
| `trusted_senders` | array | *(inherited)* | Override the global allowlist. Setting this **replaces** the global list (no merging). Omit to inherit. |
| `hooks` | array | `[]` | Hooks fired on `on_receive` (inbound) and `after_send` (outbound) events. Forbidden on catchall mailboxes (config-load error). |

**Reserved mailbox names.** `catchall` and `aimx-catchall` are reserved literals; `aimx mailboxes create` rejects them, and `MAILBOX-CREATE` over the daemon's UDS rejects them with `Validation: reserved`. The catchall is provisioned by `aimx setup` (when configured) under the wildcard address `*@<domain>` and uses owner `aimx-catchall`.

See [Mailboxes](mailboxes.md) for mailbox management and [Hooks & Trust](hooks.md) for hook configuration.

### Inbound email verification

aimx verifies three authentication mechanisms on every inbound email and records the results in the email's TOML frontmatter:

| Field | Values | Description |
|-------|--------|-------------|
| `dkim` | `pass`, `fail`, `none` | DKIM signature verification result. `none` when no DKIM signature is present. |
| `spf` | `pass`, `fail`, `none` | SPF record check against the sending server's IP. `none` when no SPF record exists or no IP could be extracted. |
| `dmarc` | `pass`, `fail`, `none` | DMARC alignment check combining DKIM and SPF results against the sender's published DMARC policy. `none` when no DMARC record is published or the check could not be performed. |

All three fields are always written (never omitted), so agents can reliably check authentication status without guessing whether a missing field means "not checked" or "failed."

The `trusted` frontmatter field summarizes the effective trust evaluation for the email's mailbox (its own `trust` / `trusted_senders` if set, otherwise the top-level defaults):

| Value | Meaning |
|-------|---------|
| `"none"` | Effective `trust` is `none` (default). No trust evaluation performed. |
| `"true"` | Effective `trust` is `verified`, sender matches the effective `trusted_senders`, AND DKIM passed. |
| `"false"` | Effective `trust` is `verified`, any other outcome. |

Trust only gates hook execution (`on_receive`). All email is stored regardless of the `trusted` result.

### Hook settings

Hooks are defined as `[[mailboxes.<name>.hooks]]` arrays. `cmd` is an argv array exec'd directly as the mailbox's `owner` — the daemon `setuid`s to `mailbox.owner_uid()` before `exec`. The first element must be an absolute path; there is no shell wrapping. If you need shell expansion, spell out `cmd = ["/bin/sh", "-c", "..."]` explicitly.

| Setting | Type | Description |
|---------|------|-------------|
| `name` | string | Optional. Matches `^[a-zA-Z0-9_][a-zA-Z0-9_.-]{0,127}$`. When omitted, aimx derives a stable 12-char hex name from `sha256(event + joined_argv + fire_on_untrusted)`. Names must be globally unique across mailboxes — including derived ones. |
| `event` | string | `"on_receive"` or `"after_send"` |
| `type` | string | Hook kind, default `"cmd"` (only `cmd` is supported today) |
| `cmd` | array of strings | Argv exec'd directly. Required and non-empty; `cmd[0]` must be an absolute path. There is no shell wrapping — spell out `["/bin/sh", "-c", "..."]` explicitly when you need shell expansion. |
| `timeout_secs` | int | Hard subprocess timeout in seconds. Default `60`, range `[1, 600]`. SIGTERM at the limit, SIGKILL 5s later. |
| `fire_on_untrusted` | bool | `on_receive` only: fire even when `trusted != "true"`. Rejected on `after_send` hooks at config load with `ERR fire_on_untrusted is on_receive only`. |

The raw `.md` (frontmatter + body) is always piped to the hook's stdin and the same path is also exposed as `$AIMX_FILEPATH`. If your hook only needs the subject or sender, read `$AIMX_SUBJECT` / `$AIMX_FROM` and ignore stdin — the daemon writes the full email but does not require the child to consume it.

Unknown fields on a hook table are rejected at config load. The legacy fields `stdin`, `template`, `params`, `run_as`, `origin`, and `dangerously_support_untrusted` are also rejected with a pointer to `book/hooks.md` — the template-hook surface, `aimx-hook` shared-uid sandbox, and the per-hook stdin opt-out have been retired in favor of the per-mailbox owner model with an unconditional pipe. See [Hooks & Trust](hooks.md) for full details on events and trust policies.

## Storage layout

```text
/etc/aimx/                   # Config + secrets (root-owned, mode 0755)
├── config.toml              # Configuration file (mode 0640, root:root)
└── dkim/
    ├── private.key          # RSA private key (mode 0600, root-only)
    └── public.key           # RSA public key (mode 0644)

/run/aimx/                   # Runtime directory (mode 0755, root:root)
└── aimx.sock                # World-writable UDS for aimx send / hook / mailbox verbs

/var/lib/aimx/               # Mailbox storage
├── inbox/                   # Each mailbox dir is `<owner>:<owner> 0700`
│   ├── catchall/            # Default mailbox (owner: aimx-catchall)
│   │   ├── 2025-04-15-143022-hello.md
│   │   └── 2025-04-15-153300-invoice-march/   # Attachment bundle
│   │       ├── 2025-04-15-153300-invoice-march.md
│   │       ├── invoice.pdf
│   │       └── receipt.png
│   └── support/             # Named mailbox (owner: support-bot)
│       └── ...
└── sent/
    └── support/             # Outbound sent copies
        └── ...
```

Each mailbox directory is chowned to `<owner>:<owner>` mode `0700` at create time and stays that way through every subsequent write (ingest, send, mark-read). Files inside are written by the daemon as `root:root 0644` — root bypasses dir perms regardless, while the mailbox owner reads via uid match. Other users cannot traverse the directory.

## IPv6 delivery (advanced)

By default, `aimx serve` delivers outbound email over IPv4 only (submitted to it by `aimx send` via `/run/aimx/aimx.sock`). This matches the SPF record that `aimx setup` generates (which lists only the server's IPv4 address) and is the right choice for most users.

If your server has a global IPv6 address and you want outbound mail to use it:

1. Set the flag in `/etc/aimx/config.toml`:

   ```toml
   enable_ipv6 = true
   ```

2. Re-run `aimx setup` so the wizard detects the flag, displays the required AAAA + `ip6:` SPF records, and verifies them for you:

   ```bash
   sudo aimx setup agent.yourdomain.com
   ```

   `aimx setup` is re-entrant. It skips install steps on an existing configuration and jumps straight to DNS guidance + verification. When `enable_ipv6 = true`, it shows the extra AAAA row in the DNS table and includes the `ip6:` mechanism in the generated SPF record, then verifies both.

3. Restart the SMTP daemon so the updated config is in effect:

   ```bash
   sudo systemctl restart aimx
   ```

**Required DNS when enabled.** Before IPv6 outbound will pass SPF at receivers like Gmail, you need:

| Type | Name | Value |
|------|------|-------|
| AAAA | `agent.yourdomain.com` | your server's IPv6 address |
| TXT (SPF) | `agent.yourdomain.com` | `v=spf1 ip4:<your-ipv4> ip6:<your-ipv6> -all` |

See the full DNS records table in [Setup](setup.md#dns-configuration) for formats. Without these DNS updates, messages delivered over IPv6 will fail SPF and may be rejected under your DMARC policy.

**When `enable_ipv6` is unset or `false`:** `aimx setup` ignores IPv6 entirely. No AAAA is advertised, no `ip6:` SPF is generated, and existing AAAA records in DNS are not validated (their presence is harmless). `aimx portcheck` only probes port 25 connectivity and is unaffected by this flag.

Leave `enable_ipv6` unset (or `false`) if any of these apply:
- Your server does not have a global IPv6 address
- You do not control the AAAA / SPF DNS records
- You just want outbound mail to work reliably with the default SPF record

## Full config example

```toml
# Domain for this email server (required)
domain = "agent.yourdomain.com"

# Data directory (default: /var/lib/aimx)
data_dir = "/var/lib/aimx"

# DKIM selector name (default: aimx)
dkim_selector = "aimx"

# Custom verifier service host (optional)
# verify_host = "https://verify.yourdomain.com"

# Opt into IPv6 outbound delivery (advanced, default: false)
# enable_ipv6 = true

# ----------------------------
# Default trust policy (applies to every mailbox unless overridden)
# ----------------------------
# trust = "verified"
# trusted_senders = ["*@yourcompany.com"]

# ----------------------------
# Mailboxes
# ----------------------------

# Catchall mailbox: receives all unmatched addresses.
# Owned by the reserved `aimx-catchall` system user; hooks on the catchall
# are forbidden at config load.
[mailboxes.catchall]
address = "*@agent.yourdomain.com"
owner = "aimx-catchall"

# ----------------------------
# Named mailbox with a per-mailbox trust override
# ----------------------------
[mailboxes.support]
address = "support@agent.yourdomain.com"
owner = "support-bot"  # Linux user 'support-bot' must exist on the host

# Per-mailbox overrides (both optional. Omit to inherit the top-level defaults).
# Setting `trusted_senders` here fully replaces the global list (no merging).
trust = "verified"
trusted_senders = ["*@yourcompany.com", "boss@gmail.com"]

# Log all incoming emails (default gate: only fires when trusted == "true")
[[mailboxes.support.hooks]]
name = "support_log"
event = "on_receive"
cmd = ["/bin/sh", "-c", 'echo "$AIMX_DATE | $AIMX_FROM | $AIMX_SUBJECT" >> /var/log/aimx-support.log']

# Trigger Claude Code on every trusted incoming email; runs as `support-bot`.
[[mailboxes.support.hooks]]
name = "support_agent"
event = "on_receive"
cmd = ["/usr/local/bin/claude", "-p", "Read the piped email and act on it via the aimx MCP server.", "--dangerously-skip-permissions"]

# ----------------------------
# Another mailbox
# ----------------------------
[mailboxes.notifications]
address = "notifications@agent.yourdomain.com"
owner = "ubuntu"
```

---

Next: [Mailboxes & Email](mailboxes.md) | [Hooks & Trust](hooks.md)
