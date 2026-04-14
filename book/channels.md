# Channel Rules & Trust

Channel rules trigger shell commands when emails arrive at specific mailboxes. Combined with trust policies, they let you build secure, automated workflows around incoming email.

## How channels work

When an email is ingested:

1. The email is parsed and saved as a `.md` file
2. AIMX checks the mailbox's `on_receive` rules
3. Each rule's match filters are evaluated
4. If the trust policy allows it, the command executes
5. Trigger failures are logged but **never block delivery**

Email is always stored regardless of whether triggers succeed or fire.

## Configuring channel rules

Rules are defined in `config.toml` under each mailbox. See [Configuration](configuration.md) for the full config reference.

```toml
[mailboxes.support]
address = "support@agent.yourdomain.com"

[[mailboxes.support.on_receive]]
type = "cmd"
command = 'echo "New email from {from}: {subject}" >> /tmp/email.log'
```

### Rule properties

| Property | Type | Required | Description |
|----------|------|----------|-------------|
| `type` | string | yes | Trigger type (currently only `cmd`) |
| `command` | string | yes | Shell command to execute |
| `match` | table | no | Optional filters to conditionally trigger |

Multiple rules can be defined per mailbox -- each is evaluated independently:

```toml
[[mailboxes.support.on_receive]]
type = "cmd"
command = 'ntfy pub agent-mail "Email from {from}: {subject}"'

[[mailboxes.support.on_receive]]
type = "cmd"
command = 'claude -p "Handle this email: $(cat {filepath})"'
```

## Template variables

The following variables are available in `command` strings:

| Variable | Description | Example |
|----------|-------------|---------|
| `{filepath}` | Full path to the saved `.md` file | `/var/lib/aimx/support/2025-01-15-001.md` |
| `{from}` | Sender email address | `alice@example.com` |
| `{to}` | Recipient email address | `support@agent.yourdomain.com` |
| `{subject}` | Email subject | `Meeting next Thursday` |
| `{mailbox}` | Mailbox name | `support` |
| `{id}` | Email ID | `2025-01-15-001` |
| `{date}` | Email date | `2025-01-15T10:30:00Z` |

Variable values are shell-escaped to prevent injection.

## Match filters

Add a `[mailboxes.<name>.on_receive.match]` section to conditionally trigger rules:

```toml
[[mailboxes.support.on_receive]]
type = "cmd"
command = 'process-invoice {filepath}'

[mailboxes.support.on_receive.match]
from = "*@company.com"
subject = "invoice"
has_attachment = true
```

### Available filters

| Filter | Type | Matching logic |
|--------|------|---------------|
| `from` | string | Glob pattern against sender email address (case-insensitive) |
| `subject` | string | Substring match (case-insensitive) |
| `has_attachment` | bool | `true` requires attachments, `false` requires none |

All specified filters must match (AND logic). If no `match` section is provided, the rule fires on every email to that mailbox.

## Trust policies

Trust policies gate whether channel triggers execute based on sender authentication. This prevents untrusted senders from triggering agent actions.

### Trust modes

| Mode | Behavior |
|------|----------|
| `none` (default) | Triggers fire for **all** incoming emails regardless of authentication |
| `verified` | Triggers only fire when the sender's DKIM signature passes verification |

```toml
[mailboxes.support]
address = "support@agent.yourdomain.com"
trust = "verified"
```

### Trusted senders

The `trusted_senders` list allows specific senders to bypass DKIM verification. It supports glob patterns:

```toml
[mailboxes.support]
address = "support@agent.yourdomain.com"
trust = "verified"
trusted_senders = ["*@yourcompany.com", "boss@gmail.com"]
```

Emails from `trusted_senders` always trigger rules, even if DKIM verification fails.

### How trust interacts with storage

**Email is always stored regardless of trust result.** Trust only gates trigger execution. An email from an unverified sender is still saved as a `.md` file and visible via `email_list` / `email_read` -- it just doesn't execute channel rules.

### DKIM/SPF verification

During email ingest, AIMX verifies:

- **DKIM** -- checks the sender's DKIM signature
- **SPF** -- validates the sending server's IP against the sender domain's SPF record

Results are stored in the email frontmatter (`dkim = "pass|fail|none"`, `spf = "pass|fail|none"`). The `verified` trust mode gates on DKIM pass specifically.

## Examples

### Notify via ntfy

```toml
[[mailboxes.catchall.on_receive]]
type = "cmd"
command = 'ntfy pub agent-mail "New email: {subject} from {from}"'
```

### Trigger Claude Code

```toml
[[mailboxes.schedule.on_receive]]
type = "cmd"
command = 'claude -p "Handle this scheduling request: $(cat {filepath})"'
```

### Log to file

```toml
[[mailboxes.support.on_receive]]
type = "cmd"
command = 'echo "{date} | {from} | {subject}" >> /var/log/aimx-support.log'
```

### Webhook via curl

```toml
[[mailboxes.orders.on_receive]]
type = "cmd"
command = 'curl -X POST http://localhost:3000/api/inbox -d @{filepath}'
```

### Conditional trigger with trust

Only process invoices from verified senders with attachments:

```toml
[mailboxes.accounting]
address = "accounting@agent.yourdomain.com"
trust = "verified"
trusted_senders = ["*@vendor.com"]

[[mailboxes.accounting.on_receive]]
type = "cmd"
command = 'claude -p "Process this invoice: $(cat {filepath})"'

[mailboxes.accounting.on_receive.match]
has_attachment = true
subject = "invoice"
```

---

Next: [MCP Server](mcp.md) | [Configuration](configuration.md) | [Troubleshooting](troubleshooting.md)
