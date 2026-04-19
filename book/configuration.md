# Configuration

AIMX uses a single TOML configuration file for all settings.

## Config file location

The default config file is at `/etc/aimx/config.toml` (mode `0640`, owner `root:root`). It is created automatically by `aimx setup`.

Starting with v0.2, the config lives under `/etc/aimx/` (separate from the data directory) so that DKIM secrets and config are owned by root and inaccessible to non-root processes.

### Data directory override

The **data directory** (`/var/lib/aimx/` by default) holds mailboxes only in v0.2 — config and DKIM keys are under `/etc/aimx/`. To relocate it:

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

This changes where `config.toml` and the DKIM keypair (`dkim/private.key`, `dkim/public.key`) are read from. Under normal operation you should **not** set this — `aimx setup` writes to `/etc/aimx/` and every command picks it up from there.

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

`aimx setup` asks for the default trust policy interactively on the first
run; on re-entry the existing top-level values on disk are preserved.

### Mailbox settings

Mailboxes are defined under `[mailboxes.<name>]`:

| Setting | Type | Default | Description |
|---------|------|---------|-------------|
| `address` | string | *(required)* | Email address pattern (e.g. `support@domain.com` or `*@domain.com` for catchall) |
| `trust` | string | *(inherited)* | Override the global default. Allowed values: `none` or `verified`. Omit to inherit. |
| `trusted_senders` | array | *(inherited)* | Override the global allowlist. Setting this **replaces** the global list (no merging). Omit to inherit. |
| `hooks` | array | `[]` | Hooks fired on `on_receive` (inbound) and `after_send` (outbound) events |

See [Mailboxes](mailboxes.md) for mailbox management and [Hooks & Trust](hooks.md) for hook configuration.

#### Upgrading an older config to use the global defaults

If you edited `config.toml` before the global-default fields existed and explicitly set `trust = "none"` or `trusted_senders = []` on every mailbox, those per-mailbox values now **shadow** any top-level default you add later — an `Option::Some(...)` at the mailbox level always wins.

This is the defined "replace" semantic, but it's an easy foot-gun when tightening policy globally. When you switch the top-level to `trust = "verified"`, also delete the redundant per-mailbox `trust = "none"` / `trusted_senders = []` lines from mailboxes you actually want to inherit the new default. `aimx setup` writes new mailboxes without those lines from the start, so only hand-edited or pre-upgrade configs need the cleanup.

### Inbound email verification

AIMX verifies three authentication mechanisms on every inbound email and records the results in the email's TOML frontmatter:

| Field | Values | Description |
|-------|--------|-------------|
| `dkim` | `pass`, `fail`, `none` | DKIM signature verification result. `none` when no DKIM signature is present. |
| `spf` | `pass`, `fail`, `none` | SPF record check against the sending server's IP. `none` when no SPF record exists or no IP could be extracted. |
| `dmarc` | `pass`, `fail`, `none` | DMARC alignment check combining DKIM and SPF results against the sender's published DMARC policy. `none` when no DMARC record is published or the check could not be performed. |

All three fields are always written (never omitted), so agents can reliably check authentication status without guessing whether a missing field means "not checked" or "failed."

The `trusted` frontmatter field summarizes the effective trust evaluation for the email's mailbox (its own `trust` / `trusted_senders` if set, otherwise the top-level defaults):

| Value | Meaning |
|-------|---------|
| `"none"` | Effective `trust` is `none` (default) -- no trust evaluation performed. |
| `"true"` | Effective `trust` is `verified`, sender matches the effective `trusted_senders`, AND DKIM passed. |
| `"false"` | Effective `trust` is `verified`, any other outcome. |

Trust only gates hook execution (`on_receive`) -- all email is stored regardless of the `trusted` result.

### Hook settings

Hooks are defined as `[[mailboxes.<name>.hooks]]` arrays:

| Setting | Type | Description |
|---------|------|-------------|
| `id` | string | Required globally-unique 12-char `[a-z0-9]` identifier |
| `event` | string | `"on_receive"` or `"after_send"` |
| `type` | string | Hook kind, default `"cmd"` (only `cmd` is supported today) |
| `cmd` | string | Shell command to execute |
| `from` | glob | `on_receive` only: sender filter |
| `to` | glob | `after_send` only: recipient filter |
| `subject` | string | Case-insensitive substring filter |
| `has_attachment` | bool | Attachment-presence filter |
| `dangerously_support_untrusted` | bool | `on_receive` only: fire even when `trusted != "true"` |

See [Hooks & Trust](hooks.md) for full details on events, match filters, and trust policies.

## Storage layout

```
/etc/aimx/                   # Config + secrets (root-owned, mode 0755)
├── config.toml              # Configuration file (mode 0640, root:root)
└── dkim/
    ├── private.key          # RSA private key (mode 0600, root-only)
    └── public.key           # RSA public key (mode 0644)

/run/aimx/                   # Runtime directory (mode 0755, root:root)
└── send.sock                # World-writable UDS for aimx send

/var/lib/aimx/               # Mailbox storage
├── inbox/
│   ├── catchall/            # Default mailbox
│   │   ├── 2025-04-15-143022-hello.md
│   │   └── 2025-04-15-153300-invoice-march/   # Attachment bundle
│   │       ├── 2025-04-15-153300-invoice-march.md
│   │       ├── invoice.pdf
│   │       └── receipt.png
│   └── support/             # Named mailbox
│       └── ...
└── sent/
    └── support/             # Outbound sent copies
        └── ...
```

## IPv6 delivery (advanced)

By default, `aimx serve` delivers outbound email over IPv4 only (submitted to it by `aimx send` via `/run/aimx/send.sock`). This matches the SPF record that `aimx setup` generates (which lists only the server's IPv4 address) and is the right choice for most users.

If your server has a global IPv6 address and you want outbound mail to use it:

1. Set the flag in `/etc/aimx/config.toml`:

   ```toml
   enable_ipv6 = true
   ```

2. Re-run `aimx setup` so the wizard detects the flag, displays the required AAAA + `ip6:` SPF records, and verifies them for you:

   ```bash
   sudo aimx setup agent.yourdomain.com
   ```

   `aimx setup` is re-entrant — it skips install steps on an existing configuration and jumps straight to DNS guidance + verification. When `enable_ipv6 = true`, it shows the extra AAAA row in the DNS table and includes the `ip6:` mechanism in the generated SPF record, then verifies both.

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

**When `enable_ipv6` is unset or `false`:** `aimx setup` ignores IPv6 entirely — no AAAA is advertised, no `ip6:` SPF is generated, and existing AAAA records in DNS are not validated (their presence is harmless). `aimx portcheck` only probes port 25 connectivity and is unaffected by this flag.

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

# Catchall mailbox -- receives all unmatched addresses
[mailboxes.catchall]
address = "*@agent.yourdomain.com"

# Notify on any incoming email (opts in to fire on untrusted mail)
[[mailboxes.catchall.hooks]]
id = "notifyall001"
event = "on_receive"
cmd = 'ntfy pub agent-mail "New email: $AIMX_SUBJECT from $AIMX_FROM"'
dangerously_support_untrusted = true

# ----------------------------
# Named mailbox with a per-mailbox trust override
# ----------------------------
[mailboxes.support]
address = "support@agent.yourdomain.com"

# Per-mailbox overrides (both optional — omit to inherit the top-level defaults).
# Setting `trusted_senders` here fully replaces the global list (no merging).
trust = "verified"
trusted_senders = ["*@yourcompany.com", "boss@gmail.com"]

# Log all incoming emails (default gate: only fires when trusted == "true")
[[mailboxes.support.hooks]]
id = "supportlog01"
event = "on_receive"
cmd = 'echo "{date} | $AIMX_FROM | $AIMX_SUBJECT" >> /var/log/aimx-support.log'

# Trigger agent on emails from Gmail with attachments
[[mailboxes.support.hooks]]
id = "supportgmai"
event = "on_receive"
cmd = 'claude -p "Process this email: $(cat \"$AIMX_FILEPATH\")"'
from = "*@gmail.com"
has_attachment = true

# ----------------------------
# Another mailbox
# ----------------------------
[mailboxes.notifications]
address = "notifications@agent.yourdomain.com"
```

---

Next: [Mailboxes & Email](mailboxes.md) | [Hooks & Trust](hooks.md)
