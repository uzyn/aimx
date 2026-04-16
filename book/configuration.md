# Configuration

AIMX uses a single TOML configuration file for all settings.

## Config file location

The default config file is at `/etc/aimx/config.toml` (mode `0640`, owner `root:root`). It is created automatically by `aimx setup`.

Starting with v0.2, the config lives under `/etc/aimx/` (separate from the data directory) so that DKIM secrets and config are owned by root and inaccessible to non-root processes.

### Data directory override

The **data directory** (`/var/lib/aimx/` by default) holds mailboxes only in v0.2 — config and DKIM keys are under `/etc/aimx/`. To relocate it:

```bash
# CLI flag (works with any command)
aimx --data-dir /custom/path status

# Environment variable
export AIMX_DATA_DIR=/custom/path
aimx status
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
| `verify_host` | string | `https://check.aimx.email` | Base URL of the verifier service used by `aimx verify` and `aimx setup`. Can be overridden per-invocation with the `--verify-host` flag. |
| `enable_ipv6` | bool | `false` | Advanced. Opt into IPv6 outbound delivery. See [IPv6 delivery](#ipv6-delivery-advanced). |

### Mailbox settings

Mailboxes are defined under `[mailboxes.<name>]`:

| Setting | Type | Default | Description |
|---------|------|---------|-------------|
| `address` | string | *(required)* | Email address pattern (e.g. `support@domain.com` or `*@domain.com` for catchall) |
| `trust` | string | `none` | Trust policy: `none` or `verified` |
| `trusted_senders` | array | `[]` | Glob patterns for senders that bypass DKIM verification |
| `on_receive` | array | `[]` | Channel rules triggered on incoming email |

See [Mailboxes](mailboxes.md) for mailbox management and [Channel Rules](channels.md) for trigger configuration.

### Inbound email verification

AIMX verifies three authentication mechanisms on every inbound email and records the results in the email's TOML frontmatter:

| Field | Values | Description |
|-------|--------|-------------|
| `dkim` | `pass`, `fail`, `none` | DKIM signature verification result. `none` when no DKIM signature is present. |
| `spf` | `pass`, `fail`, `none` | SPF record check against the sending server's IP. `none` when no SPF record exists or no IP could be extracted. |
| `dmarc` | `pass`, `fail`, `none` | DMARC alignment check combining DKIM and SPF results against the sender's published DMARC policy. `none` when no DMARC record is published or the check could not be performed. |

All three fields are always written (never omitted), so agents can reliably check authentication status without guessing whether a missing field means "not checked" or "failed."

The `trusted` frontmatter field summarizes the per-mailbox trust evaluation:

| Value | Meaning |
|-------|---------|
| `"none"` | Mailbox `trust` is `none` (default) -- no trust evaluation performed. |
| `"true"` | Mailbox `trust` is `verified`, sender matches `trusted_senders`, AND DKIM passed. |
| `"false"` | Mailbox `trust` is `verified`, any other outcome. |

Trust only gates channel trigger execution -- all email is stored regardless of the `trusted` result.

### Channel rule settings

Channel rules are defined as `[[mailboxes.<name>.on_receive]]` arrays:

| Setting | Type | Description |
|---------|------|-------------|
| `type` | string | Trigger type (currently only `cmd`) |
| `command` | string | Shell command to execute with [template variables](channels.md#template-variables) |
| `match` | table | Optional filters: `from` (glob), `subject` (substring), `has_attachment` (bool) |

See [Channel Rules](channels.md) for full details on triggers, match filters, and trust policies.

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

**When `enable_ipv6` is unset or `false`:** `aimx setup` ignores IPv6 entirely — no AAAA is advertised, no `ip6:` SPF is generated, and existing AAAA records in DNS are not validated (their presence is harmless). `aimx verify` only probes port 25 connectivity and is unaffected by this flag.

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

# DKIM selector name (default: dkim)
dkim_selector = "dkim"

# Custom verifier service host (optional)
# verify_host = "https://verify.yourdomain.com"

# Opt into IPv6 outbound delivery (advanced, default: false)
# enable_ipv6 = true

# ----------------------------
# Mailboxes
# ----------------------------

# Catchall mailbox -- receives all unmatched addresses
[mailboxes.catchall]
address = "*@agent.yourdomain.com"

# Notify on any incoming email
[[mailboxes.catchall.on_receive]]
type = "cmd"
command = 'ntfy pub agent-mail "New email: {subject} from {from}"'

# ----------------------------
# Named mailbox with trust policy
# ----------------------------
[mailboxes.support]
address = "support@agent.yourdomain.com"

# Only trigger on DKIM-verified emails
trust = "verified"

# These senders always trigger, bypassing DKIM check
trusted_senders = ["*@yourcompany.com", "boss@gmail.com"]

# Log all incoming emails
[[mailboxes.support.on_receive]]
type = "cmd"
command = 'echo "{date} | {from} | {subject}" >> /var/log/aimx-support.log'

# Trigger agent on emails from Gmail with attachments
[[mailboxes.support.on_receive]]
type = "cmd"
command = 'claude -p "Process this email: $(cat {filepath})"'

[mailboxes.support.on_receive.match]
from = "*@gmail.com"
has_attachment = true

# ----------------------------
# Another mailbox
# ----------------------------
[mailboxes.notifications]
address = "notifications@agent.yourdomain.com"
```

---

Next: [Mailboxes & Email](mailboxes.md) | [Channel Rules](channels.md)
