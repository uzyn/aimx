# Release notes

## Unreleased — multi-domain support

This release adds **light multi-domain support**: one AIMX install can
host multiple sending and receiving domains. Each domain has its own
DKIM keypair, its own catchall, and its own mailboxes. The first entry
in `domains` is the default — bare local parts resolve against it.

The change preserves the single-binary, single-operator model. There
are no new multi-tenant features (no per-domain ACLs, no per-domain
rate limits, no hosted-service surface).

### What changes on first restart after upgrade

The upgrade migration runs **atomically on the first `aimx serve`
startup under the new binary**. It is idempotent (guarded by
`/var/lib/aimx/.layout-version`) — subsequent restarts are no-ops.

For an existing single-domain install on `mydomain.com`:

1. **`config.toml` is visibly rewritten** from the v1 shape to the
   normalized multi-domain shape:
   - `domain = "mydomain.com"` → `domains = ["mydomain.com"]`
   - `[mailboxes.info]` → `[mailboxes."info@mydomain.com"]` (every
     local-part-keyed mailbox is re-keyed to its FQDN)
   - Per-domain sub-tables (`[domain."<d>"]`) remain absent until you
     add an override.

2. **Storage relocates** from
   `/var/lib/aimx/{inbox,sent}/<mailbox>/` to
   `/var/lib/aimx/<domain>/{inbox,sent}/<mailbox>/`. The renames use
   `rename(2)` — same-filesystem, constant time, atomic.

3. **DKIM keys relocate** from `/etc/aimx/dkim/{private,public}.key`
   to `/etc/aimx/dkim/<domain>/{private,public}.key`. The key
   material is unchanged; only the on-disk path moves.

4. A `.layout-version` marker is written so the migration runs
   exactly once.

The change is **purely structural** — there are no semantic
differences for single-domain installs. Inbound continues to route to
the same mailboxes, outbound continues to sign with the same DKIM
key, hooks continue to fire the same way. `aimx mailboxes list` and
the MCP `mailbox_list` tool return FQDN names
(`info@mydomain.com`) instead of bare local parts — that's the only
observable difference at the API boundary.

`aimx upgrade` prints a one-screen reminder of these points before
completing.

### What's new

- `aimx domains list` — print configured domains with DKIM status and
  per-domain mailbox counts.
- `aimx domains add <domain>` — append a domain to `domains`,
  generate a DKIM keypair, print DNS records, verify, hot-reload.
- `aimx domains remove <domain> [--force]` — remove a domain, with
  cascade-delete via `--force`. Last-domain hard-block; DKIM keys
  preserved on disk.
- `aimx dkim-keygen --domain <domain>` — generate or rotate keys for
  a specific domain.
- Per-domain config sub-tables: `[domain."<domain>"]` supports
  optional `signature`, `dkim_selector`, `trust`, `trusted_senders`
  overrides. Resolution order is per-mailbox → per-domain → global.
- Per-domain DKIM signing: outbound signs with the From: domain's
  key, never `domains[0]`'s.
- Per-domain catchall: `*@<domain>` is independent per domain.
- `aimx doctor` reports per-domain DKIM, mailbox counts, and unread
  counts; marks the default domain.
- MCP tools (`mailbox_list`, `email_list`, etc.) return and accept
  FQDN mailbox names. Bare local parts still resolve to the default
  domain for backward compatibility.

### Rollback

Rollback to a pre-multi-domain binary is documented in
[`book/multi-domain.md`](book/multi-domain.md#rollback-procedure).
Short version: stop the daemon, move storage and DKIM keys back to
the v1 paths, hand-edit `config.toml` back to the v1 shape, delete
`/var/lib/aimx/.layout-version`, install the older binary, restart.
The procedure is mechanical and lossless if you're still on a single
domain and haven't added a second domain since the upgrade.

### Where to go next

- [`book/multi-domain.md`](book/multi-domain.md) — full operator
  reference (CLI, per-domain config, DKIM, storage, upgrade
  migration, rollback).
- [`book/cli.md#domain-management`](book/cli.md#domain-management) — `aimx
  domains list / add / remove` reference.
- [`book/troubleshooting.md#multi-domain`](book/troubleshooting.md#multi-domain)
  — corrupted marker, EXDEV, half-migrated state,
  DKIM-key-not-found.
- [`book/faq.md#multi-domain`](book/faq.md#multi-domain) — quick
  answers.
