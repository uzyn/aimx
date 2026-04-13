# Configuration

aimx uses a single TOML configuration file for all settings.

## Config file location

The default config file is at `/var/lib/aimx/config.toml`. This file is created automatically by `aimx setup`.

### Data directory override

The data directory (which contains `config.toml`, DKIM keys, and all mailboxes) can be overridden:

```bash
# CLI flag (works with any command)
aimx --data-dir /custom/path status

# Environment variable
export AIMX_DATA_DIR=/custom/path
aimx status
```

The `--data-dir` flag takes precedence over the environment variable.

## Settings reference

### Top-level settings

| Setting | Type | Default | Description |
|---------|------|---------|-------------|
| `domain` | string | *(required)* | The email domain (e.g. `agent.yourdomain.com`) |
| `data_dir` | string | `/var/lib/aimx` | Directory for storing config, keys, and mailboxes |
| `dkim_selector` | string | `dkim` | DKIM selector name used in DNS records |
| `verify_host` | string | `https://check.aimx.email` | Base URL of the verifier service used by `aimx verify` and `aimx setup`. Can be overridden per-invocation with the `--verify-host` flag. |

### Mailbox settings

Mailboxes are defined under `[mailboxes.<name>]`:

| Setting | Type | Default | Description |
|---------|------|---------|-------------|
| `address` | string | *(required)* | Email address pattern (e.g. `support@domain.com` or `*@domain.com` for catchall) |
| `trust` | string | `none` | Trust policy: `none` or `verified` |
| `trusted_senders` | array | `[]` | Glob patterns for senders that bypass DKIM verification |
| `on_receive` | array | `[]` | Channel rules triggered on incoming email |

See [Mailboxes](mailboxes.md) for mailbox management and [Channel Rules](channels.md) for trigger configuration.

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
/var/lib/aimx/
├── config.toml              # Configuration file
├── dkim/
│   ├── private.key          # RSA private key (mode 0600)
│   └── public.key           # RSA public key
├── catchall/                # Default mailbox
│   ├── 2025-01-15-001.md
│   ├── 2025-01-15-002.md
│   └── attachments/
│       └── document.pdf
└── support/                 # Named mailbox
    ├── 2025-01-15-001.md
    └── attachments/
```

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
