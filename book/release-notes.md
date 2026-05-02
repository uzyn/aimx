# Release Notes

Version-by-version changelog of operator-visible behavior changes. Use this as the canonical source for "what changed" between aimx releases; individual book chapters describe the current behavior only.

## Unreleased — Mailbox create/delete no longer requires root

Mailbox lifecycle is now **owner-gated** end-to-end. A non-root operator can create and delete the mailboxes they own without `sudo`, and agents can self-serve mailboxes over MCP. Existing sudo-based workflows continue to work unchanged — root passes every action.

### What changed

- **CLI.** `aimx mailboxes create <name>` and `aimx mailboxes delete <name>` (incl. `--force`) work as your regular Linux user when `aimx serve` is running. The daemon resolves your uid via `SO_PEERCRED`, treats it as the canonical owner identity, atomically rewrites `config.toml`, and hot-swaps its in-memory snapshot. The previous "requires root, exit code 2" gate has been removed from both the CLI entry-point and the UDS handler.
- **UDS.** The `MAILBOX-CREATE` and `MAILBOX-DELETE` verbs no longer call `enforce_root`. `MAILBOX-CREATE` from a non-root caller synthesizes the owner from `SO_PEERCRED` and discards any client-supplied `Owner:` header; `MAILBOX-DELETE` checks that the caller's uid equals the resolved mailbox's `owner_uid` before doing any state work.
- **MCP.** Two new tools — `mailbox_create` and `mailbox_delete` — let agents provision and tear down mailboxes owned by the MCP process's uid. Neither tool accepts an `owner` parameter, by construction. `mailbox_delete` accepts an optional `force: bool` that wipes `inbox/<name>/` and `sent/<name>/` daemon-side under per-mailbox lock + `CONFIG_WRITE_LOCK` (atomic with the config rewrite).

### Privilege-escalation defense

The daemon **never** trusts client-supplied owner data from a non-root caller. Every non-root `MAILBOX-CREATE` over the UDS resolves the owner identity from the kernel-validated `SO_PEERCRED` peer uid; the wire field is honored only when the caller is root (so cross-uid creates remain operator-only). There is no path — CLI, UDS, MCP, hand-crafted `socat` request — for a non-root caller to cause a mailbox to be created with an owner other than their own uid. Negative tests in `src/auth.rs`, `src/mailbox_handler.rs`, and `tests/uds_authz.rs` pin the contract.

### What stays root-only

A handful of operations remain root-gated because they cross a genuine privilege boundary, not just a policy line:

- **`aimx setup`**, **`aimx serve`**, **`aimx uninstall`** — host-level service install / run / removal.
- **`aimx dkim-keygen`** — writes the `0600 root:root` private signing key.
- **`aimx portcheck`** — needs to bind / probe port 25.
- **Cross-uid mailbox creates.** Only root may pass `--owner <other>` on `aimx mailboxes create`. Non-root callers passing `--owner <other>` get a soft warning and the daemon silently overrides with the synthesized owner (their own uid).
- **The catchall.** Provisioned during `aimx setup` under the reserved `aimx-catchall` system user; not exposed through any non-root surface.
- **Raw-shell hooks.** `aimx hooks create --cmd '...'` writes a literal `/bin/sh -c "..."` argv into `config.toml` — arbitrary code execution as the chosen `run_as` uid, distinct from mailbox lifecycle.

### Upgrade compatibility

No config-file migration. No daemon restart required by the change itself (though you do need a daemon binary that includes Sprint 1 + Sprint 2 of the user-mailbox track). Sudo-based scripts that already say `sudo aimx mailboxes create / delete` continue to work — the root path is unchanged. Operators can leave their automation alone and simply drop the `sudo` from new mailbox-create commands when they're ready.

If you previously read [Troubleshooting](troubleshooting.md): the old `MAILBOX-CREATE / MAILBOX-DELETE rejected for non-root` entry has been replaced. The new failure mode for a non-root caller is *"daemon must be running for non-root mailbox CRUD; start `aimx serve` or run with sudo to fall back to direct config edit"* — fix it by starting the daemon, or by running the command under `sudo` to keep the existing direct-write fallback path.

## Unreleased — upgrade visibility

Closes the visibility gap on the upgrade path: operators can now confirm whether the running `aimx serve` daemon was actually restarted on the new binary, and detect drift between the on-disk `aimx` and the still-running daemon.

### `AIMX/1 VERSION` UDS verb

A new read-only verb on `/run/aimx/aimx.sock` returns the daemon's `{tag, git_hash, target, build_date}`. Same authorization posture as `MAILBOX-LIST` — no `SO_PEERCRED` filter, the payload is build metadata only. There is no separate remote-version subcommand; consumers go through `aimx doctor`.

### `aimx doctor` renders client + server versions

The Service section now includes two new lines:

```
Client version:   v1.2.4 (a1b2c3d4)
Server version:   v1.2.4 (a1b2c3d4)
```

When the tags differ, restart the service (`systemctl restart aimx`, or `rc-service aimx restart` on OpenRC) so the daemon picks up the new binary. The lines are informational only — no `DoctorFinding`, no exit-code change. If the daemon is offline the Server line renders `(daemon not running)`; if the probe fails within its 500 ms budget it renders the failure reason in dim text. See [Troubleshooting: Version drift](troubleshooting.md#version-drift-between-client-and-daemon).

### `install.sh` upgrade path is louder and self-healing

- The stop and start banners are now promoted from `dbg` to `say`, so the operator running `curl | sh` sees the service control happening.
- The `systemctl is-enabled = true && is-active = false` path now still calls `start_service` after the binary swap. Previously the daemon was never restarted on that path.
- A new `pgrep`-based detector warns (never signals) when a manually-launched `aimx serve` is running outside systemd / OpenRC.
- A single post-start `systemctl is-active` check confirms the daemon came up, with a `journalctl -u aimx -n 20` hint on failure.

### `aimx upgrade` confirms the restart

After `wait_for_service_ready` returns true, `aimx upgrade` prints one extra line:

```
✓ aimx serve restarted on v1.2.4
```

The line is suppressed on the rollback path so a failed upgrade never claims success.

## Unreleased — MCP surface cleanup

Three hard breaks tighten the MCP tool surface around aimx's "no index, no scan" design. Canonical tool docs live in [MCP Server](mcp.md); the new hook model lives in [Hooks & Trust](hooks.md).

### Removed `email_list` filters

- **What was removed:** the `unread`, `from`, `since`, and `subject` parameters on `email_list`.
- **Rationale:** aimx ships no index. Server-side filters silently forced an O(N) scan of every frontmatter block in the mailbox — the opposite of the design intent. The new shape lists a page of metadata (cheap, bounded by `limit`) and the agent filters client-side.
- **New call shape:**

  ```
  email_list(mailbox="alice", limit=50)   # then filter rows where read == false
  ```

`email_list` now returns a JSON array (one row per email) with `id`, `from`, `to`, `subject`, `date`, plus `read` on inbox rows or `delivery_status` on sent rows. Pass `offset` to page past already-seen rows.

### Removed `email_mark_*` `folder` parameter

- **What was removed:** the `folder` parameter on `email_mark_read` and `email_mark_unread` (and its `Folder:` header on the underlying UDS verb).
- **Rationale:** there is no agent workflow that benefits from marking sent copies read or unread. Inbox is the only meaningful target; the `"sent"` value was dead weight. The `MarkFolder::Sent` variant has also been deleted from the codebase.
- **New call shape:**

  ```
  email_mark_read(mailbox="alice", id="2025-06-15-120000-hello")
  ```

The MCP schema rejects a stale `folder` argument with an `unknown field` parse error rather than silently mutating inbox.

### Removed `hook_create` / `config.toml` `stdin`

- **What was removed:** the `stdin` parameter on the `hook_create` MCP tool, and the `stdin` field on `[[mailbox.<name>.hook]]` blocks in `config.toml`. The daemon now always pipes the raw `.md` source to every hook command.
- **Rationale:** closing stdin to a hook gave no real benefit — `$AIMX_FROM`, `$AIMX_SUBJECT`, and `$AIMX_FILEPATH` already cover the "metadata only" case, and the child process is free to ignore stdin.
- **Upgrade-time validation error.** `aimx serve` will refuse to start if any hook block in `config.toml` still carries a `stdin` line. The error names the offending hook so you can grep your logs against it:

  ```
  hook 'X' carries removed field 'stdin' — remove this line and restart aimx serve; the email is always piped to hooks
  ```

  Remediation: open `/etc/aimx/config.toml`, delete every `stdin = "…"` line under your `[[mailbox.*.hook]]` blocks, then `sudo systemctl restart aimx`.
- **New call shape:**

  ```
  hook_create(mailbox="alice", event="on_receive", cmd=["/usr/local/bin/notify"])
  ```

  Selectivity guidance: if your hook only needs the subject or sender, read `$AIMX_SUBJECT` / `$AIMX_FROM` and ignore stdin — the daemon writes the full email to stdin but does not require the child to consume it.

### Soft change — `mailbox_list` and `email_list` now return JSON

`mailbox_list` and `email_list` now return JSON arrays instead of plain-text listings. Existing agents using the bundled skills are already updated. Any custom MCP client that parsed the old plain-text output must switch to a JSON parser; nothing fails at startup, but the next call will surface the shape change.

`mailbox_list` rows: `{ name, inbox_path, sent_path, total, unread, sent_count, registered }`. `email_list` rows on inbox: `{ id, from, to, subject, date, read }`; on sent: `{ id, from, to, subject, date, delivery_status }`.

## 0.1.0 — first public release

aimx ships as a single prebuilt binary for Linux on four targets: `x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu`, `x86_64-unknown-linux-musl`, `aarch64-unknown-linux-musl` (canonical Rust target triples; tarball filenames drop the `-unknown-` vendor field, e.g. `aimx-0.1.0-x86_64-linux-gnu.tar.gz`). One-line install:

```bash
curl -fsSL https://aimx.email/install.sh | sh
```

And one-line in-place upgrade on any existing install:

```bash
sudo aimx upgrade
```

### What landed for first release

- **Release pipeline.** `.github/workflows/release.yml` builds all four tarballs + per-tarball `.sha256` + release-wide `SHA256SUMS` on every bare SemVer tag (e.g. `0.1.0`, `1.2.3-rc1`). Release notes include a verbatim `curl + sha256sum -c` block for skeptical operators. No signing in v1 (deferred to v2).
- **`install.sh`.** Plain POSIX `sh` installer at `https://aimx.email/install.sh`. Auto-detects OS / arch / libc, supports `--tag` / `--target` / `--to` / `--force` plus `AIMX_VERSION` / `AIMX_PREFIX` / `AIMX_DRY_RUN` / `AIMX_VERBOSE` / `GITHUB_TOKEN`. Upgrade path is wizard-free: stop → swap → start.
- **`aimx upgrade`.** Single-verb subcommand; flags `--dry-run`, `--version <tag>`, `--force`. Atomic `rename(2)` binary swap with automatic rollback to `/usr/local/bin/aimx.prev` on failure.
- **Setup wizard refactor.** Wizard asks two decisions (domain, trusted senders) instead of five-plus. Removed: hook-template checkbox, Gmail / deliverability section, `none | verified` trust toggle. Added: loud warning when trusted-senders list is empty, prominent `q`-to-skip on the DNS loop, drop-through to `aimx agents setup` as `$SUDO_USER` on completion.
- **`aimx agents setup` TUI.** No-argument default is now an interactive checkbox picker with detected-status rendering (`[x] (already wired)`, `[ ]`, `[-] (not detected)`). `--no-interactive` and `<agent>` subcommands remain for scripting. `--dangerously-allow-root` escape hatch for direct-root VPS setups.
- **Version metadata.** `aimx --version` bakes tag + git SHA + target triple + build timestamp at compile time via `build.rs`.
- **CLI branding reconciliation.** `src/term.rs` now drives every colored / marked surface: `✓ ✗ ⚠ →` marks on TTY, `[OK] [FAIL] [WARN] [>]` fallback when color is disabled. Copper accent (`#B9531C` truecolor) on the prompt arrow. CI lint gate rejects raw `.red()` / `.green()` / `.bold()` outside `term.rs`.

### Behavioral shifts (carried forward from pre-launch)

- **`aimx mailboxes create <name>` without `--owner` now prompts interactively.** Previously this command hard-errored with a `useradd` hint when the local-part of the address did not resolve to an existing Linux user. The new behavior:
  - On a TTY with `AIMX_NONINTERACTIVE` unset: the command prompts for the Linux user that should own the mailbox, re-prompts up to five times if the entered username does not resolve via `getpwnam`, and finally errors with an actionable `useradd` hint if every attempt fails.
  - With `AIMX_NONINTERACTIVE=1`: the command errors immediately (exit 1) with the same hint whenever the local-part default does not resolve. Scripted installers should set this variable so the command never blocks.
  - With a piped / closed stdin AND `AIMX_NONINTERACTIVE` unset: the prompt loop burns its five attempts (each `read_line` returns EOF immediately) before erroring. Still fails fast, just noisier than the non-interactive path. Set `AIMX_NONINTERACTIVE=1` whenever you pipe input to avoid the extra output.

- **`aimx setup` drops the `--non-interactive` flag.** The legacy hook-template checkbox phase was removed earlier so the flag no longer gated any interactive prompt. Scripts that passed `--non-interactive` must drop the argument; the `AIMX_NONINTERACTIVE=1` environment variable remains the canonical way to force non-interactive behavior for helpers that still support it (today: `aimx mailboxes create`).
