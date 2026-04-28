# Installation

AIMX ships as a single statically-compiled binary. Install it in one line on any supported Linux box:

```bash
curl -fsSL https://aimx.email/install.sh | sh
```

This downloads the latest release for your platform, installs `aimx` into `/usr/local/bin/`, runs the interactive setup wizard (`sudo aimx setup`), then drops back to your user to wire the MCP server into your LLM agent (`aimx agents setup`). One command, end-to-end. No Rust toolchain, no `cargo build`, no source checkout.

## Supported platforms

AIMX is Linux-only. Every release ships four prebuilt targets:

| Canonical target triple | Tarball filename target | Typical distros |
|---|---|---|
| `x86_64-unknown-linux-gnu`   | `x86_64-linux-gnu`   | Debian, Ubuntu, Fedora, RHEL, Rocky, Arch |
| `aarch64-unknown-linux-gnu`  | `aarch64-linux-gnu`  | 64-bit ARM on any glibc distro (e.g. Ubuntu on Raspberry Pi 4/5, AWS Graviton, Hetzner CAX) |
| `x86_64-unknown-linux-musl`  | `x86_64-linux-musl`  | Alpine, statically-linked containers |
| `aarch64-unknown-linux-musl` | `aarch64-linux-musl` | Alpine ARM, statically-linked ARM containers |

The canonical Rust target triple (with the `-unknown-` vendor field) is still what `cargo build --target` and `aimx --version` use. Tarball filenames drop `-unknown-` for readability — the installer composes both forms automatically.

The install script auto-detects your OS, CPU arch (`uname -m`), and libc flavor (glibc vs. musl) and picks the matching tarball. Non-Linux platforms are refused with a single-line error — AIMX is Linux-only by policy.

## What the installer does

The shell script is intentionally thin: it prints a two-line install banner, fails fast if `sudo` is missing on a non-root box, downloads the binary, and `exec`s `sudo aimx setup` so the binary owns the operator-facing wizard. The full six-step checklist (preflight, domain & DNS, TLS, trust, install, MCP wiring) lives inside `aimx setup` itself, not the shell — so `aimx setup` works the same whether you ran it via the installer or directly.

The installer:

1. Prints a thin two-line "AIMX installer" banner — the six-step wizard checklist is the binary's job.
2. Detects your platform and picks the matching release asset.
3. Acquires `sudo` up front (`sudo -v </dev/tty` so `curl | sh` still gets the password prompt). If you are already root, this is a no-op; if `sudo` is missing on a non-root box, the script exits with a clear error **before** any GitHub network call.
4. Resolves the target version (latest release by default; override with `--tag` or `AIMX_VERSION`).
5. Downloads the tarball over HTTPS from GitHub Releases.
6. Extracts it into a temp directory that is cleaned up on every exit path (success, error, or interrupt).
7. Installs the binary with `install -m 0755` into `/usr/local/bin/aimx` (override with `--to` or `AIMX_PREFIX`).
8. On a fresh box, backs up any pre-existing `/etc/aimx/config.toml` to `config.toml.bak-YYYYMMDD-HHMMSS` (DKIM keys and TLS certs are left in place so deliverability survives re-runs), then `exec`s `sudo aimx setup </dev/tty`. The binary takes over from there.
9. `aimx setup` runs the six-step wizard end-to-end: preflight on port 25, domain & DNS, TLS certificate, trust policy, install + service unit, and MCP wiring (the wizard re-execs `aimx agents setup` as `$SUDO_USER`). The wizard owns the welcome banner, the checklist's `☐ → ☑/☒` ticking, and the closing message with `@<your-domain>` substituted.
10. If an older `aimx` is already installed, the upgrade path kicks in instead: stops the service, swaps the binary atomically, and restarts — no wizard re-run. If the same version is already installed, exits without touching anything (pass `--force` to reinstall).

If no TTY is available (CI, fully-scripted contexts), the setup step will error unless you also supply `AIMX_NONINTERACTIVE=1` with the required defaults — see [Setup](setup.md) for the non-interactive contract. Pre-existing `/etc/aimx/config.toml` is backed up to a timestamped `.bak-*` sibling before setup runs; DKIM keys and TLS certs are preserved.

## Flags and environment variables

Everything is optional; defaults cover the common case.

| Flag | Env var | Purpose |
|---|---|---|
| `--tag <VERSION>` | `AIMX_VERSION` | Install a specific release tag (e.g. `0.1.0`). Tags are bare SemVer (no `v` prefix); a caller-supplied `v` is stripped leniently. Flag wins if both are set. |
| `--target <TRIPLE>` | — | Override platform auto-detection. Useful for installing the musl build on a glibc box. |
| `--to <DIR>` | `AIMX_PREFIX` | Install into `<DIR>/aimx` instead of `/usr/local/bin/aimx`. |
| `--force` | — | Re-install even if the target version is already present. |
| `--help` | — | Print usage. |
| — | `AIMX_DRY_RUN=1` | Print every step without downloading or installing anything. Useful for auditing the script before running it for real. |
| — | `AIMX_VERBOSE=1` | Trace HTTP requests and filesystem actions. |
| — | `GITHUB_TOKEN` | Bearer token for GitHub API rate-limited contexts (CI, shared NAT). |

Examples:

```bash
# Install a specific version
curl -fsSL https://aimx.email/install.sh | sh -s -- --tag 0.1.0

# Install into /opt/aimx/bin instead of /usr/local/bin
curl -fsSL https://aimx.email/install.sh | AIMX_PREFIX=/opt/aimx/bin sh

# Audit what the script would do without touching anything
curl -fsSL https://aimx.email/install.sh | AIMX_DRY_RUN=1 sh

# Install the musl build on a glibc machine
curl -fsSL https://aimx.email/install.sh | sh -s -- --target x86_64-unknown-linux-musl
```

### Custom install prefix

`AIMX_PREFIX` and `--to` pick the directory that receives the `aimx` binary. `aimx setup` picks up the actual binary location from `/proc/self/exe`, so the generated systemd / OpenRC service file resolves `ExecStart` to whatever prefix you installed into. A common non-default choice is `AIMX_PREFIX=/opt/aimx/bin`:

```bash
curl -fsSL https://aimx.email/install.sh | AIMX_PREFIX=/opt/aimx/bin sh
sudo /opt/aimx/bin/aimx setup
```

The drop-through to `aimx agents setup` also uses `/proc/self/exe`, so a non-default prefix works end-to-end without extra configuration.

## Verification (skeptical operator path)

If you would rather not pipe a remote script into `sh`, every tarball is published with an accompanying `.sha256` file and a release-wide `SHA256SUMS` aggregate. Verify manually before extracting anything:

```bash
# Tags are bare SemVer (no `v` prefix). Tarball filenames drop the
# `-unknown-` vendor field from the canonical target triple.
TAG=0.1.0
TARBALL_TARGET=x86_64-linux-gnu
TARBALL=aimx-${TAG}-${TARBALL_TARGET}.tar.gz

curl -fL -O "https://github.com/uzyn/aimx/releases/download/${TAG}/${TARBALL}"
curl -fL -O "https://github.com/uzyn/aimx/releases/download/${TAG}/${TARBALL}.sha256"
sha256sum -c "${TARBALL}.sha256"

tar -xzf "${TARBALL}"
sudo install -m 0755 "aimx-${TAG}-${TARBALL_TARGET}/aimx" /usr/local/bin/aimx
aimx --version
```

Every GitHub Release also carries a verbatim `curl + sha256sum -c` block in its release notes so you can copy-paste it without reading the docs.

You can also inspect the install script itself before running it:

```bash
curl -fsSL https://aimx.email/install.sh | less
```

The script is plain POSIX `sh` (Dash- and BusyBox-compatible), roughly 500 lines, and follows the same shape as [`just.systems/install.sh`](https://just.systems/install.sh).

## Trust model (v1)

aimx v1's trust anchor is **HTTPS on the GitHub Releases domain**. The install script enforces HTTPS-only downloads; it does not verify tarball signatures. Skeptical operators can verify SHA-256 manually against the published `SHA256SUMS` file as shown above.

Signed releases (minisign, cosign, GPG, OIDC) are **deferred to v2** — adding a signing story requires release-team coordination (key custody, rotation, verifier tooling in every surface that fetches a binary) that is out of proportion for a solo-maintainer v1. The honest limitation is documented here rather than glossed over.

If you want supply-chain integrity today, pin a specific release tag and verify SHA-256 against `SHA256SUMS` before every install / upgrade.

> **Note on `Content-Type`:** the landing page at `aimx.email/install.sh` is served by GitHub Pages, which does not expose a header-customization API. The raw bytes are served as `application/x-sh` rather than `text/x-sh; charset=utf-8`. Operator-visible `curl | sh` behavior is unaffected.

## Upgrading

Two equivalent paths: use the installer again, or use `aimx upgrade`.

```bash
# Option 1: re-run the installer. Detects an older binary, stops aimx,
# swaps atomically, restarts. No wizard re-run.
curl -fsSL https://aimx.email/install.sh | sh

# Option 2: use the upgrade subcommand (preferred on an existing box).
sudo aimx upgrade
```

`aimx upgrade` checks `https://api.github.com/repos/uzyn/aimx/releases/latest`, compares the tag against the running binary's version, and if newer:

1. Downloads the target-matching tarball.
2. Extracts it into `$TMPDIR`.
3. Stops `aimx.service` (or the OpenRC equivalent).
4. Renames the current `/usr/local/bin/aimx` to `/usr/local/bin/aimx.prev` and moves the new binary into place — atomic `rename(2)` so a crash cannot leave a half-written binary.
5. Restarts the service.
6. Waits for the daemon to come back up, then prints a `✓ aimx serve restarted on <tag>` confirmation line followed by `aimx v<old> → v<new>. Service restarted.`

If any step after the stop fails, the rollback path restores `aimx.prev` and restarts the service. A `✗` line names the failed step. The restart-confirmation line is suppressed on the rollback path.

Flags:

| Flag | Purpose |
|---|---|
| `--dry-run` | Resolve the target version and print the action sequence without touching the running service. |
| `--version <tag>` | Target a specific release (also used for downgrades). |
| `--force` | Re-install the current tag. Useful for repair after a partial swap. |

`aimx upgrade` is non-interactive by design: it never prompts, never runs the setup wizard, never touches DNS, never edits `config.toml`. It is strictly a binary swap plus service restart.

Config-schema backward compatibility is handled inline by serde (`#[serde(alias = ...)]` / `#[serde(default)]`) — there is no separate migration pass. If a future release ever does break config shape, the release notes will call it out and the new binary will refuse to start with a pointer back to them.

### Manual rollback

Every successful upgrade preserves the previous binary at `/usr/local/bin/aimx.prev` (overwritten on the next upgrade). If a new release misbehaves and you want to roll back without waiting for a patch:

```bash
sudo systemctl stop aimx
sudo mv /usr/local/bin/aimx.prev /usr/local/bin/aimx
sudo systemctl start aimx
aimx --version
```

This only covers one generation back. Past that, install a specific older tag with `sudo aimx upgrade --version <tag>`.

## Troubleshooting

**"aimx is Linux-only" error.**  The install script runs `uname` and refuses anything other than Linux. Run it on a Linux box.

**GitHub API rate limits.**  The installer calls `https://api.github.com/repos/uzyn/aimx/releases/latest` for version resolution. Unauthenticated API requests share a per-IP quota. If you hit it, set `GITHUB_TOKEN` to a personal access token with no scopes selected (public-read is implicit):

```bash
curl -fsSL https://aimx.email/install.sh | GITHUB_TOKEN=ghp_... sh
```

Shared-NAT environments (CI, corporate networks) are the usual culprit.

**Unexpected arch.**  Uncommon CPUs like `armv7l` are not supported; override the detection with `--target aarch64-unknown-linux-gnu` if you have a compatible CPU and want to try the 64-bit ARM build.

**Service start failed after upgrade.**  The upgrade path attempts rollback automatically. If `aimx.service` is still down, manually restore `aimx.prev`:

```bash
sudo mv /usr/local/bin/aimx.prev /usr/local/bin/aimx
sudo systemctl start aimx
journalctl -u aimx -n 50
```

Then file an issue with the service log.

**Binary installs but `aimx --version` prints the wrong tag.**  `--version` is baked at build time from `git describe --tags`. If you built from source and the working tree is dirty or ahead of the last tag, the output will reflect that (e.g. `0.1.0-12-gabcdef1-dirty`). Released tarballs always print the exact tag.

## Building from source (contributors)

Source builds are supported for contributors and air-gapped environments. Everyone else should use the one-line installer — it is faster, pins a tested release, and requires no Rust toolchain.

```bash
# Prereqs: rustup, a recent stable toolchain, and git.
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source $HOME/.cargo/env

git clone https://github.com/uzyn/aimx.git
cd aimx
cargo build --release
sudo install -m 0755 target/release/aimx /usr/local/bin/aimx
aimx --version
```

See the top-level `CLAUDE.md` for the full developer workflow (lint, format, tests, verifier service).

---

Next: [Setup](setup.md) to run the interactive wizard, generate DKIM keys, and add DNS records.
