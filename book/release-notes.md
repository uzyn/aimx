# Release Notes

Version-by-version changelog of operator-visible behavior changes. Use this as the canonical source for "what changed" between aimx releases; individual book chapters describe the current behavior only.

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
- **Setup wizard refactor.** Wizard asks two decisions (domain, trusted senders) instead of five-plus. Removed: hook-template checkbox, Gmail / deliverability section, `none | verified` trust toggle. Added: loud warning when trusted-senders list is empty, prominent `q`-to-skip on the DNS loop, drop-through to `aimx agent-setup` as `$SUDO_USER` on completion.
- **`aimx agent-setup` TUI.** No-argument default is now an interactive checkbox picker with detected-status rendering (`[x] (already wired)`, `[ ]`, `[-] (not detected)`). `--no-interactive` and `<agent>` subcommands remain for scripting. `--dangerously-allow-root` escape hatch for direct-root VPS setups.
- **Version metadata.** `aimx --version` bakes tag + git SHA + target triple + build timestamp at compile time via `build.rs`.
- **CLI branding reconciliation.** `src/term.rs` now drives every colored / marked surface: `✓ ✗ ⚠ →` marks on TTY, `[OK] [FAIL] [WARN] [>]` fallback when color is disabled. Copper accent (`#B9531C` truecolor) on the prompt arrow. CI lint gate rejects raw `.red()` / `.green()` / `.bold()` outside `term.rs`.

### Behavioral shifts (carried forward from pre-launch)

- **`aimx mailboxes create <name>` without `--owner` now prompts interactively.** Previously this command hard-errored with a `useradd` hint when the local-part of the address did not resolve to an existing Linux user. The new behavior:
  - On a TTY with `AIMX_NONINTERACTIVE` unset: the command prompts for the Linux user that should own the mailbox, re-prompts up to five times if the entered username does not resolve via `getpwnam`, and finally errors with an actionable `useradd` hint if every attempt fails.
  - With `AIMX_NONINTERACTIVE=1`: the command errors immediately (exit 1) with the same hint whenever the local-part default does not resolve. Scripted installers should set this variable so the command never blocks.
  - With a piped / closed stdin AND `AIMX_NONINTERACTIVE` unset: the prompt loop burns its five attempts (each `read_line` returns EOF immediately) before erroring. Still fails fast, just noisier than the non-interactive path. Set `AIMX_NONINTERACTIVE=1` whenever you pipe input to avoid the extra output.

- **`aimx setup` drops the `--non-interactive` flag.** The legacy hook-template checkbox phase was removed earlier so the flag no longer gated any interactive prompt. Scripts that passed `--non-interactive` must drop the argument; the `AIMX_NONINTERACTIVE=1` environment variable remains the canonical way to force non-interactive behavior for helpers that still support it (today: `aimx mailboxes create`).
