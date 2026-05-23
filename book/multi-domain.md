# Multi-domain

AIMX hosts multiple sending and receiving domains from one binary. Each
domain has its own DKIM keypair, its own catchall, and its own mailboxes.
The first entry in the `domains` array is the **default** — bare local
parts (`research`, `support`, `agent`) resolve against it.

This page is the operator reference for everything multi-domain: when to
add a second domain, the `aimx domains` CLI, per-domain config, per-domain
DKIM, the per-domain storage layout, what the upgrade migration does on
the first restart, how to remove a domain, what's deliberately out of
scope, and how to roll back if you need to.

## When to add a second domain

You want a second domain on the same AIMX install when:

- You run AIMX for one identity (`personal.com`) but want a second
  identity (`side-project.com`) with its own DKIM and DMARC story without
  paying for a second VPS or running a relay.
- You're a freelancer with one `consultancy.com` plus per-client domains
  (`acme.consultancy.com`, `widgets.consultancy.com`) and want each
  engagement to send under its own brand.
- You're consolidating identities you already own onto one host because
  the operational cost of N AIMX instances is the dominant pain.

Multi-domain is **not** a multi-tenant feature. There is exactly one
operator. The trust model is unchanged: every mailbox still belongs to one
Linux user; root still owns `/etc/aimx/` and the DKIM keys; the UDS still
authorizes on `SO_PEERCRED`. A second domain is a routing convenience,
not a hosted-service surface.

## `aimx domains` CLI

`aimx domains` (alias: `aimx domain`) manages the domain list. The CLI
prefers the daemon UDS so changes hot-reload without a restart; the
daemon is required for non-root callers (the config is `0640 root:root`).
See [CLI Reference: Domain management](cli.md#domain-management).

### `aimx domains list`

```bash
aimx domains list
```

Prints a table of every configured domain: name, default marker, DKIM key
presence + DNS verification status, mailbox count, and any per-domain
overrides (signature, selector, trust). The first row of the table is the
default domain.

### `aimx domains add <domain>`

```bash
sudo aimx domains add side-project.com
```

Generates a 2048-bit RSA DKIM keypair under
`/etc/aimx/dkim/side-project.com/`, appends `side-project.com` to the
`domains` array, prints the four DNS records to publish (MX, SPF, DMARC,
DKIM), runs the same DNS-verification loop as `aimx setup`, and
hot-reloads the daemon over the UDS so `@side-project.com` mail is
accepted immediately.

Flags:

- `--selector <name>` — DKIM selector for the new domain (default
  `aimx`).
- `--no-dns-check` — skip the verification loop when you publish DNS
  out-of-band. The records are still printed.

The add is **root-only** (the same authz that gates mailbox creation in
other root-only contexts). It refuses if the domain is already in
`domains` and points you at `aimx domains list`.

### `aimx domains remove <domain> [--force]`

```bash
# Refuses if any mailbox still lives under side-project.com:
sudo aimx domains remove side-project.com

# Cascade: deletes every mailbox on side-project.com and its
# /var/lib/aimx/side-project.com/ storage tree, then drops the domain
# from `domains`. The DKIM keys at /etc/aimx/dkim/side-project.com/
# are preserved on disk.
sudo aimx domains remove --force side-project.com
```

Without `--force`, the command refuses and lists the mailboxes still
keyed at the target domain. With `--force`, the daemon takes every
per-mailbox lock for the target domain in sorted FQDN order, then
`CONFIG_WRITE_LOCK`, then atomically wipes the storage tree, the
`[mailboxes."<local>@<domain>"]` entries, the optional
`[domain."<domain>"]` sub-table, and the domain string itself.

Removing the **last** remaining domain is hard-blocked even with
`--force` — an AIMX install must have at least one domain to be
functional. To tear AIMX down entirely, use [`aimx uninstall`](cli.md#aimx-uninstall).

The DKIM key files at `/etc/aimx/dkim/<domain>/` are deliberately
**preserved** on remove so accidentally removing a domain you still own
isn't a key-destruction event. Delete them by hand once you're sure
(`sudo rm -rf /etc/aimx/dkim/<domain>/`).

## Per-domain config sub-tables

Per-domain overrides live under `[domain."<domain>"]` in `config.toml`.
The key is singular `domain` because TOML cannot let `domains` be both
the top-level array and a parent table on the same key. This mirrors the
existing `aimx domain`/`aimx domains` clap alias.

```toml
domains = ["a.com", "b.com"]

# Global defaults (unchanged):
trust = "verified"
trusted_senders = ["*@company.com"]
dkim_selector = "aimx"
signature = "Sent from AIMX.  \nhttps://aimx.email"

# Optional per-domain overrides:
[domain."b.com"]
signature = "Sent from B Corp"
dkim_selector = "s2025"
trust = "verified"
trusted_senders = ["*@trusted-partner.com"]
```

Every field under `[domain."<d>"]` is optional. Resolution order is:

| Field | Resolution |
|------|------|
| `trust`, `trusted_senders` | per-mailbox → per-domain → global |
| `signature` | per-domain → global → built-in default |
| `dkim_selector` | per-domain → global → built-in default `"aimx"` |

A per-mailbox `trusted_senders` list fully **replaces** the per-domain
list. A per-domain `trusted_senders` fully replaces the global list.
There is no merging at either layer.

## Per-domain DKIM

Each domain has its own keypair at
`/etc/aimx/dkim/<domain>/{private,public}.key` (mode `0600` / `0644`,
owner `root:root`). The daemon loads every key into an
`ArcSwap<HashMap<String, DkimKey>>` at startup and hot-swaps on
`DOMAIN-ADD` / `DOMAIN-REMOVE`. Outbound signing in `send_handler` picks
the key for the From: domain and signs with that domain's resolved
selector — never `domains[0]`'s key.

`aimx dkim-keygen` accepts `--domain <domain>` to operate on a specific
domain. Without the flag, it operates on the default
domain (`domains[0]`). See [CLI Reference: `aimx dkim-keygen`](cli.md#aimx-dkim-keygen).

```bash
# Rotate the b.com selector to s2025 without touching a.com:
sudo aimx dkim-keygen --domain b.com --selector s2025 --force
```

## Storage layout

Multi-domain installs nest mailboxes under `<data_dir>/<domain>/`:

```text
/var/lib/aimx/
├── .layout-version              # migration marker; do not edit
├── README.md                    # auto-generated datadir guide
├── a.com/
│   ├── inbox/
│   │   ├── catchall/            # *@a.com lands here
│   │   └── support/
│   └── sent/
│       └── support/
└── side-project.com/
    ├── inbox/
    │   └── info/
    └── sent/
        └── info/
```

`--data-dir` / `AIMX_DATA_DIR` continues to govern the root path; the
`<domain>/` nesting happens inside whatever root is configured. The
daemon enforces `0o755` on every `<data_dir>/<domain>/` directory on
every startup so non-root mailbox owners can `x` into their own
`inbox/<name>/` (which itself stays `0o700`). If you hand-tighten a
per-domain directory to `0o700`, the next `aimx serve` restart will
widen it back to `0o755` — the asymmetric posture is intentional.

## Upgrade migration walkthrough

The upgrade from a v1 (single-domain) install to multi-domain happens
**atomically on the first `aimx serve` startup under the new binary**.
Storage, DKIM keys, and `config.toml` all move to the canonical
multi-domain shape in one locked transaction. There is no opt-out, no
lazy path, no CLI flag that skips it.

The migration is idempotent (guarded by `.layout-version`), so
subsequent restarts are no-ops.

### Before upgrade (v1, single-domain install on `mydomain.com`)

```text
/etc/aimx/config.toml
  domain = "mydomain.com"
  [mailboxes.info]
  [mailboxes.support]
  [mailboxes.alice]

/etc/aimx/dkim/private.key
/etc/aimx/dkim/public.key

/var/lib/aimx/inbox/{info,support,alice}/...
/var/lib/aimx/sent/{info,support,alice}/...
```

### Step 1: Operator runs `aimx upgrade`

`aimx upgrade` swaps `/usr/local/bin/aimx` (or your `AIMX_PREFIX` path)
atomically, preserves the old binary at `<install_path>.prev`, restarts
`aimx.service`, and prints a one-screen reminder summarizing what
happens on next start.

### Step 2: systemd starts `aimx serve` under the new binary

The daemon detects v1 layout (any of `.layout-version` absent +
`/var/lib/aimx/inbox/` present, or `/etc/aimx/dkim/private.key` next to
no `<domain>/` subdir, or `domain = "..."` without `domains = [...]`,
or any local-part-keyed `[mailboxes.<local>]`) and performs the
migration under `CONFIG_WRITE_LOCK` plus every per-mailbox lock:

1. **Storage rename.** `rename(2)` `/var/lib/aimx/inbox` →
   `/var/lib/aimx/mydomain.com/inbox/`, same for `sent`. Same-filesystem
   rename, constant time, atomic.
2. **DKIM rename.** `mkdir -p /etc/aimx/dkim/mydomain.com/` (mode
   `0700`, owner `root:root`), then rename `private.key` and
   `public.key` into it.
3. **Config rewrite.** `write_atomic` `config.toml` to:
   ```toml
   domains = ["mydomain.com"]
   [mailboxes."info@mydomain.com"]
   [mailboxes."support@mydomain.com"]
   [mailboxes."alice@mydomain.com"]
   ```
4. **Marker.** Write `/var/lib/aimx/.layout-version` containing `2`.
5. Log one INFO line summarizing every move with a pointer back to
   this page.

The renames are constant-time regardless of how much mail is stored;
the slow step is the TOML serialize, which completes well under a
second on a typical install.

After the migration, the daemon accepts SMTP and UDS traffic and mail
flow resumes.

### Step 3: Day-to-day after upgrade

- Inbound to `info@mydomain.com`, `support@mydomain.com`,
  `alice@mydomain.com` works exactly as before.
- Outbound from any mailbox signs with the (now-relocated) DKIM key
  under `/etc/aimx/dkim/mydomain.com/`.
- `aimx doctor` reports one domain (`mydomain.com`), marks it as
  default, shows the DKIM key path with the per-domain nesting.
- `aimx mailboxes list` and the MCP `mailbox_list` tool return FQDN
  names (`info@mydomain.com`, etc.) — different from v1 output.
- `/etc/aimx/config.toml` is visibly different (normalized shape).
  Semantically equivalent to before.

### Step 4 (optional): Add a second domain

```bash
sudo aimx domains add side-project.com
```

After publishing DNS, `domains = ["mydomain.com", "side-project.com"]`,
the new per-domain storage tree at `/var/lib/aimx/side-project.com/` is
created lazily on first mailbox creation under it, and a new DKIM
keypair lives at `/etc/aimx/dkim/side-project.com/`.

### Migration safety

- **Atomic per step.** Each rename and the `write_atomic` config
  rewrite are independently atomic. The daemon refuses to accept SMTP
  or UDS traffic until the entire transaction completes.
- **Idempotent.** Re-running with `.layout-version: 2` is a single
  stat call — a no-op fast path. The migration runs exactly once.
- **Hard-fail on partial completion.** If any step fails, the daemon
  refuses to start with a clear error pointing at `aimx logs`. A
  half-migrated state is detectable from path existence and the next
  start resumes from the first incomplete step. There is no silent
  fallback.
- **No data loss tolerated.** The migration uses `rename(2)`
  exclusively — no copy, no rewrite, no risk of half-written files.

If something goes wrong, capture `aimx logs --lines 200`, the state
of `/var/lib/aimx/`, `/etc/aimx/dkim/`, and `/etc/aimx/config.toml`
before touching anything else.

## Removal semantics

- `aimx domains remove <domain>` (no `--force`) refuses with a JSON
  list of every mailbox FQDN still keyed at the target domain.
- `aimx domains remove --force <domain>` takes every per-mailbox
  lock for the target domain in sorted FQDN order (outer), then
  `CONFIG_WRITE_LOCK` (inner), then atomically:
  1. Wipes `inbox/<local>/` and `sent/<local>/` for every mailbox on
     the target domain, via the same code path
     `aimx mailboxes delete --force` uses.
  2. Removes the empty per-domain root with `rmdir(2)`.
  3. Removes the `[domain."<domain>"]` sub-table from in-memory
     `Config`.
  4. Removes every `[mailboxes."<local>@<domain>"]` entry.
  5. Removes the domain string from `domains`.
  6. `write_atomic`s the new `config.toml`.
  7. Hot-swaps the in-memory `Arc<Config>` via `ConfigHandle::store`.
  8. Drops the per-domain DKIM map entry **before** the config swap
     so a concurrent SEND never sees a configured domain with no key.
- **Last-domain hard-block.** Removing the only remaining domain is
  always refused. Use `aimx uninstall` to tear AIMX down entirely.
- **DKIM keys preserved.** The keypair at `/etc/aimx/dkim/<domain>/`
  stays on disk so re-adding the same domain is recoverable. The
  command prints the path so you know where they are.
- **No undo.** Force removal wipes mail content. Archive first if you
  care about it.

## Light scope (what we deliberately don't do)

Multi-domain is intentionally small. The following are out of scope
and stay out of scope:

- **MCP `domain_create` / `domain_delete` / `domain_list` tools.**
  Domain management is operator-only and requires `sudo`. Agents
  can infer the domain list from the FQDN-shaped mailbox names
  returned by `mailbox_list`.
- **Per-domain TLS certs / per-domain EHLO hostnames.** AIMX
  presents one server identity. The cert's CN/SAN must cover the
  EHLO hostname, which is `domains[0]`.
- **Per-domain verifier endpoints / per-domain port-25 checks.**
  The verifier service (`services/verifier/`) is server-level.
- **Per-domain rate limits, quotas, or per-domain operators.**
  Multi-tenant features stay out — this is one operator with many
  identities, not a hosted service.
- **Per-mailbox `signature` override.** The per-domain override is
  enough for v1.
- **`aimx domains rotate-dkim <domain>`.** DKIM rotation is folded
  into a future hardening track; use the selector swap recipe in
  the [FAQ](faq.md#how-do-i-rotate-the-dkim-key-without-a-delivery-gap)
  in the meantime.
- **Cross-domain hook semantics.** Hooks remain strictly
  per-mailbox. A hook on `support@a.com` and a hook on
  `support@b.com` are independent.
- **Aliasing one mailbox across multiple domains.** Operators who
  want `support` to receive both `@a.com` and `@b.com` configure two
  mailboxes with hooks that forward to a common path.
- **`aimx domains set-default <domain>`** reordering CLI. Ships in
  a follow-up; in the meantime, hand-edit `domains` in
  `config.toml` and restart the daemon.

## Rollback procedure

Rollback is a rare operator-driven action, never a CLI subcommand.
Rolling back to a pre-multi-domain (v1) binary after the migration
ran is mechanical and lossless if you're still on a single domain
and haven't made any post-upgrade config changes beyond the
automatic rewrite. If you've added a second domain since the
migration, the second domain's mail and DKIM key must be exported
or discarded first — the v1 binary cannot read them.

```bash
# 1. Stop the daemon.
sudo systemctl stop aimx
# (or: sudo rc-service aimx stop)

# 2. Move storage back to the v1 layout. Replace <domain> with the
# value at domains[0] (the only entry left after step 0).
sudo mv /var/lib/aimx/<domain>/inbox /var/lib/aimx/inbox
sudo mv /var/lib/aimx/<domain>/sent  /var/lib/aimx/sent
sudo rmdir /var/lib/aimx/<domain>

# 3. Move the DKIM keys back to the v1 location.
sudo mv /etc/aimx/dkim/<domain>/private.key /etc/aimx/dkim/private.key
sudo mv /etc/aimx/dkim/<domain>/public.key  /etc/aimx/dkim/public.key
sudo rmdir /etc/aimx/dkim/<domain>

# 4. Hand-edit /etc/aimx/config.toml back to the v1 shape:
#    - `domains = ["<domain>"]` → `domain = "<domain>"`
#    - `[mailboxes."<local>@<domain>"]` → `[mailboxes.<local>]`
#    - Remove any `[domain."<domain>"]` sub-tables.

# 5. Remove the layout marker so the v1 binary doesn't trip over it.
sudo rm /var/lib/aimx/.layout-version

# 6. Install the older binary (the one preserved at .prev works) and
# restart.
sudo mv /usr/local/bin/aimx.prev /usr/local/bin/aimx
sudo systemctl start aimx
```

If you had a second domain when you started the rollback, its
mailboxes are now unreachable — the v1 binary doesn't know about
them. Either archive that directory tree somewhere safe before
running step 2, or accept the loss.
