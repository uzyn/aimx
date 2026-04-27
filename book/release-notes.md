# Release Notes

Version-by-version changelog of operator-visible behavior changes. Use this as the canonical source for "what changed" between aimx releases; individual book chapters describe the current behavior only.

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
