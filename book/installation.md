# Installation

AIMX ships as a single statically-compiled binary. Install in one line:

```bash
curl -fsSL https://aimx.email/install.sh | sh
```

This downloads the latest release for your platform, installs `aimx` into `/usr/local/bin/`, and runs `sudo aimx setup`. When setup exits, run `aimx agents setup` yourself as your regular user to wire the MCP server into your agent. No Rust toolchain, no `cargo build`, no source checkout.

## Supported platforms

AIMX is Linux-only. Every release ships four prebuilt targets:

| Canonical target triple | Tarball filename target | Typical distros |
|---|---|---|
| `x86_64-unknown-linux-gnu`   | `x86_64-linux-gnu`   | Debian, Ubuntu, Fedora, RHEL, Rocky, Arch |
| `aarch64-unknown-linux-gnu`  | `aarch64-linux-gnu`  | 64-bit ARM on any glibc distro |
| `x86_64-unknown-linux-musl`  | `x86_64-linux-musl`  | Alpine, statically-linked containers |
| `aarch64-unknown-linux-musl` | `aarch64-linux-musl` | Alpine ARM, statically-linked ARM containers |

The install script auto-detects your OS, CPU arch (`uname -m`), and libc flavor (glibc vs. musl) and picks the matching tarball. Non-Linux platforms are refused with a single-line error — AIMX is Linux-only by policy.

## What the installer does

1. Detects platform and libc; picks the matching release asset.
3. Acquires `sudo` (`sudo -v </dev/tty` so `curl | sh` still gets the password prompt). Fails fast if `sudo` is missing on a non-root box, before any network call.
4. Resolves the target version (latest by default; override with `--tag` or `AIMX_VERSION`).
5. Downloads the tarball over HTTPS from [GitHub Releases](https://github.com/uzyn/aimx/releases).
6. Extracts into a temp directory cleaned up on every exit path.
7. Installs the binary as `install -m 0755 /usr/local/bin/aimx` (override with `--to` / `AIMX_PREFIX`).
8. On a fresh box, backs up any pre-existing `/etc/aimx/config.toml` to `config.toml.bak-YYYYMMDD-HHMMSS`, then `exec`s `sudo aimx setup </dev/tty`. DKIM keys and STARTTLS certs are preserved across re-runs.
9. If an older `aimx` is already installed, the upgrade path runs instead: stop the service, swap the binary atomically, restart. No wizard re-run. If the running version matches the target, the script asks `AIMX is already installed. Re-run setup to (re)configure it? [y/N]` — answer `y` to skip the download and re-enter `aimx setup` (handy if you aborted the wizard partway through), or `N` / Enter / no usable TTY (CI, scripted callers) to exit `0` without touching anything. `--force` skips the prompt and reinstalls the binary.

In CI / non-TTY contexts, set `AIMX_NONINTERACTIVE=1` and supply defaults — see [Setup](setup.md).

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

When you follow up with `aimx agents setup` as your regular user, it resolves itself from `$PATH`. Make sure the install prefix you chose is on your shell's `PATH`, or invoke the binary by its full path (e.g. `/opt/aimx/bin/aimx agents setup`).

## Manual verification

Every tarball is published with an accompanying `.sha256` file and a release-wide `SHA256SUMS` aggregate. To skip `curl | sh` and verify by hand:

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

## Upgrading

Two equivalent paths: use the installer again, or use `aimx upgrade` (recommended).

```bash
# Option 1: use the upgrade subcommand (preferred on an existing box).
sudo aimx upgrade

# Option 2: re-run the installer. Detects an older binary, stops aimx,
# swaps atomically, restarts. No wizard re-run.
curl -fsSL https://aimx.email/install.sh | sh
```

`aimx upgrade` checks `https://api.github.com/repos/uzyn/aimx/releases/latest`, compares the tag against the running binary's version, and if newer:

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

## Building from source

Source builds are for contributors and air-gapped environments. Everyone else should use the one-line installer.

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

## Troubleshooting

**"AIMX is Linux-only" error.**  The install script runs `uname` and refuses anything other than Linux. Run it on a Linux box.

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

## 
