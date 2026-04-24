#!/bin/sh
# aimx install script — POSIX sh (dash / busybox compatible).
#
# Usage:
#   curl -fsSL https://aimx.email/install.sh | sh
#   curl -fsSL https://aimx.email/install.sh | sh -s -- --tag 1.2.3
#
# Drives the full end-to-end install: banner → sudo escalate →
# download + install binary → `sudo aimx setup` (preflight, DNS, TLS,
# trust, service install) → drop to the invoking user for
# `aimx agent-setup` (MCP wiring) → closing message.
#
# Modelled on `just.systems/install.sh` — `say` / `err` / `need` /
# `download` helper idioms, no bashisms, HTTPS-only trust anchor.

set -eu

# ---------------------------------------------------------------------------
# Constants
# ---------------------------------------------------------------------------

GITHUB_REPO="uzyn/aimx"
GITHUB_API="https://api.github.com/repos/${GITHUB_REPO}/releases"
GITHUB_DL="https://github.com/${GITHUB_REPO}/releases/download"
DEFAULT_PREFIX="/usr/local/bin"
UNSUPPORTED_DOC="https://aimx.email/book/installation.html#unsupported-platforms"

# Config path used by backup_existing_config. Overridable for tests via
# AIMX_INSTALL_CONFIG_PATH; production always points at /etc/aimx/config.toml.
AIMX_CONFIG_TOML="${AIMX_INSTALL_CONFIG_PATH:-/etc/aimx/config.toml}"

# ---------------------------------------------------------------------------
# Helpers (say / err / need / download)
# ---------------------------------------------------------------------------

say() {
    printf 'install: %s\n' "$1" >&2
}

verbose() {
    if [ "${AIMX_VERBOSE:-0}" = "1" ]; then
        printf 'install: %s\n' "$1" >&2
    fi
}

err() {
    printf 'install: error: %s\n' "$1" >&2
    cleanup
    exit 1
}

need() {
    if ! command -v "$1" >/dev/null 2>&1; then
        err "required command not found: $1"
    fi
}

# Create temp dir and arm cleanup trap. Safe on every exit path.
_td=""
cleanup() {
    if [ -n "${_td}" ] && [ -d "${_td}" ]; then
        rm -rf "${_td}"
        _td=""
    fi
}

# ---------------------------------------------------------------------------
# UI helpers (color when TTY + !NO_COLOR, plain otherwise).
# Centralised so banner / step list / closing message share one style.
# ---------------------------------------------------------------------------

_ui_color_enabled() {
    if [ -n "${NO_COLOR:-}" ]; then
        return 1
    fi
    if [ ! -t 2 ]; then
        return 1
    fi
    return 0
}

_ui_paint() {
    # $1 = ansi code, $2 = text
    if _ui_color_enabled; then
        printf '\033[%sm%s\033[0m' "$1" "$2"
    else
        printf '%s' "$2"
    fi
}

ui_info() {
    _msg="$1"
    printf '%s %s\n' "$(_ui_paint 34 '[info]')" "${_msg}" >&2
}

ui_warn() {
    _msg="$1"
    printf '%s %s\n' "$(_ui_paint 33 '[warn]')" "${_msg}" >&2
}

ui_error() {
    _msg="$1"
    printf '%s %s\n' "$(_ui_paint 31 '[error]')" "${_msg}" >&2
}

ui_success() {
    _msg="$1"
    printf '%s %s\n' "$(_ui_paint 32 '[ok]')" "${_msg}" >&2
}

ui_section() {
    _msg="$1"
    printf '\n%s\n' "$(_ui_paint '1;36' "== ${_msg} ==")" >&2
}

# download <url> <path>
#   Prefers curl; falls back to wget. Refuses non-HTTPS URLs. Honors
#   GITHUB_TOKEN for api.github.com calls so rate-limited CI runs succeed.
download() {
    _url="$1"
    _dst="$2"
    case "${_url}" in
        https://*) : ;;
        *) err "refusing non-HTTPS URL: ${_url}" ;;
    esac
    _auth_hdr=""
    case "${_url}" in
        https://api.github.com/*)
            if [ -n "${GITHUB_TOKEN:-}" ]; then
                _auth_hdr="Authorization: Bearer ${GITHUB_TOKEN}"
            fi
            ;;
    esac
    verbose "GET ${_url}"
    if command -v curl >/dev/null 2>&1; then
        if [ -n "${_auth_hdr}" ]; then
            curl --proto '=https' --tlsv1.2 -fsSL -H "${_auth_hdr}" \
                -o "${_dst}" "${_url}"
        else
            curl --proto '=https' --tlsv1.2 -fsSL -o "${_dst}" "${_url}"
        fi
    elif command -v wget >/dev/null 2>&1; then
        if [ -n "${_auth_hdr}" ]; then
            wget --https-only -q --header="${_auth_hdr}" \
                -O "${_dst}" "${_url}"
        else
            wget --https-only -q -O "${_dst}" "${_url}"
        fi
    else
        err "need curl or wget on PATH"
    fi
}

help() {
    cat <<'EOF'
aimx install script

USAGE:
    install.sh [FLAGS]

FLAGS:
    -h, --help               Print this help and exit
        --tag <VERSION>      Install a specific release tag (e.g. 1.2.3);
                             overrides AIMX_VERSION env var. Tags are bare
                             SemVer (no `v` prefix); a caller-supplied `v`
                             is stripped leniently.
        --target <TRIPLE>    Override target auto-detection
                             (x86_64-unknown-linux-gnu,
                              aarch64-unknown-linux-gnu,
                              x86_64-unknown-linux-musl,
                              aarch64-unknown-linux-musl)
        --to <DIR>           Install binary into DIR (default /usr/local/bin);
                             overrides AIMX_PREFIX env var
        --force              Re-install even if target version already present

ENVIRONMENT:
    AIMX_VERSION             Release tag to install (e.g. 1.2.3)
    AIMX_PREFIX              Install directory (default /usr/local/bin)
    AIMX_DRY_RUN=1           Print every step without downloading or installing
    AIMX_VERBOSE=1           Trace HTTP requests and filesystem actions
    GITHUB_TOKEN             Token for rate-limited GitHub API calls

EXAMPLES:
    # Latest stable into /usr/local/bin
    curl -fsSL https://aimx.email/install.sh | sh

    # Pin a specific tag
    curl -fsSL https://aimx.email/install.sh | sh -s -- --tag 1.2.3

    # Dry-run: see what would happen without installing
    curl -fsSL https://aimx.email/install.sh | AIMX_DRY_RUN=1 sh

Trust anchor is HTTPS on the GitHub Releases domain. No signature or
checksum verification in this script; skeptical operators can verify
manually via the 'curl + sha256sum -c' block in the release notes.
EOF
}

# ---------------------------------------------------------------------------
# Welcome banner + step list
# ---------------------------------------------------------------------------

print_welcome_banner() {
    # Title line uses the raven emoji; terminals without glyph support
    # render tofu and that is acceptable.
    printf '\n' >&2
    printf '%s\n' "$(_ui_paint '1;35' '🐦‍⬛ AIMX installer')" >&2
    printf '%s\n' "$(_ui_paint 2 'Self-hosted email for AI agents. One binary, one setup.')" >&2
    printf '\n' >&2
    printf '%s\n' "( ) Preflight checks on port 25" >&2
    printf '%s\n' "( ) Set up domain and DNS" >&2
    printf '%s\n' "( ) Set up TLS certificate" >&2
    printf '%s\n' "( ) Set up trust policy" >&2
    printf '%s\n' "( ) Install AIMX" >&2
    printf '%s\n' "( ) Set up MCP for agent(s)" >&2
    printf '\n' >&2
}

# ---------------------------------------------------------------------------
# Privilege / invoker helpers
# ---------------------------------------------------------------------------

# SUDO holds the prefix to use for privileged commands. It is either
# empty (when running as root) or "sudo" (when a non-root invoker has
# sudo on PATH). Populated by resolve_sudo_prefix, which must be called
# once early in main(). Defined here so sourced test harnesses see it.
SUDO=""

# resolve_sudo_prefix — set $SUDO to the right privilege prefix:
#   - already root (euid 0)      → SUDO=""  (run commands directly)
#   - non-root with sudo on PATH → SUDO="sudo"
#   - non-root without sudo      → SUDO=""  (call sites will fail with a
#                                  useful error via ensure_sudo before
#                                  ever running a privileged command)
resolve_sudo_prefix() {
    _euid="$(id -u 2>/dev/null || echo 0)"
    if [ "${_euid}" -eq 0 ]; then
        SUDO=""
    elif command -v sudo >/dev/null 2>&1; then
        SUDO="sudo"
    else
        SUDO=""
    fi
}

ensure_sudo() {
    _euid="$(id -u 2>/dev/null || echo 0)"
    if [ "${_euid}" -eq 0 ]; then
        return 0
    fi
    if command -v sudo >/dev/null 2>&1; then
        if ! sudo -n true >/dev/null 2>&1; then
            ui_info "Administrator privileges required; enter your password"
            # Reattach /dev/tty so `curl | sh` still gets a password prompt.
            # Wrap in a subshell + rc capture so a failing redirect or
            # wrong password yields a user-visible error instead of a
            # silent `set -e` abort.
            _sudo_rc=0
            if [ -e /dev/tty ] && [ -r /dev/tty ]; then
                (sudo -v </dev/tty) || _sudo_rc=$?
            else
                sudo -v || _sudo_rc=$?
            fi
            if [ "${_sudo_rc}" -ne 0 ]; then
                ui_error "failed to obtain sudo credentials"
                exit 1
            fi
        fi
        return 0
    fi
    ui_error "sudo is required for system installs on Linux"
    say "  Install sudo or re-run as root."
    exit 1
}

# detect_invoker
#   Prints the non-root user that should run `aimx agent-setup`.
#   Returns 0 with stdout set on success, non-zero when no non-root
#   user can be identified.
detect_invoker() {
    if [ -n "${SUDO_USER:-}" ] && [ "${SUDO_USER}" != "root" ]; then
        printf '%s' "${SUDO_USER}"
        return 0
    fi
    _me="$(id -un 2>/dev/null || echo '')"
    if [ -n "${_me}" ] && [ "${_me}" != "root" ]; then
        printf '%s' "${_me}"
        return 0
    fi
    return 1
}

# backup_existing_config
#   If /etc/aimx/config.toml exists, rename it to
#   config.toml.bak-YYYYMMDD-HHMMSS-<pid> (UTC). On failure, err out
#   rather than silently continuing. Only config.toml is backed up —
#   DKIM keys and TLS certs are left in place so deliverability survives
#   re-runs. The $$ (pid) suffix prevents collision between concurrent
#   invocations that land in the same second.
backup_existing_config() {
    _cfg="${AIMX_CONFIG_TOML}"
    if [ -f "${_cfg}" ]; then
        _ts="$(date -u +%Y%m%d-%H%M%S)"
        _bak="${_cfg}.bak-${_ts}-$$"
        if ${SUDO} mv -f "${_cfg}" "${_bak}"; then
            ui_info "backed up existing config to ${_bak}"
        else
            err "failed to back up existing ${_cfg}"
        fi
    fi
}

# ---------------------------------------------------------------------------
# Platform detection
# ---------------------------------------------------------------------------

detect_os() {
    _os="$(uname -s)"
    case "${_os}" in
        Linux) printf 'linux' ;;
        *)
            err "aimx is Linux-only; detected ${_os}. See ${UNSUPPORTED_DOC}"
            ;;
    esac
}

detect_arch() {
    _arch="$(uname -m)"
    case "${_arch}" in
        x86_64 | amd64) printf 'x86_64' ;;
        aarch64 | arm64) printf 'aarch64' ;;
        *)
            err "unsupported CPU architecture: ${_arch}. See ${UNSUPPORTED_DOC}"
            ;;
    esac
}

detect_libc() {
    # Presence of a musl dynamic loader under /lib/ld-musl-* signals musl.
    # Otherwise assume glibc — aimx only ships gnu + musl Linux builds.
    for _musl in /lib/ld-musl-* /lib64/ld-musl-*; do
        if [ -e "${_musl}" ]; then
            printf 'musl'
            return 0
        fi
    done
    printf 'gnu'
}

compose_target() {
    _arch="$1"
    _libc="$2"
    printf '%s-unknown-linux-%s' "${_arch}" "${_libc}"
}

# Map a canonical Rust target triple (e.g. `x86_64-unknown-linux-gnu`) to the
# shortened artifact-filename form used by release tarballs
# (`x86_64-linux-gnu`). The canonical triple is still used for
# `cargo build --target`, `aimx --version`, and operator-facing error
# messages — only the tarball filename drops the `-unknown-` vendor field.
artifact_target() {
    printf '%s' "$1" | sed 's/-unknown-/-/'
}

# ---------------------------------------------------------------------------
# Version resolution
# ---------------------------------------------------------------------------

# resolve_latest_tag
#   Fetch https://api.github.com/repos/uzyn/aimx/releases/latest, pluck the
#   "tag_name" value with grep + sed. Deliberately does NOT use jq — matches
#   the just.systems installer.
resolve_latest_tag() {
    _body="${_td}/release.json"
    download "${GITHUB_API}/latest" "${_body}"
    _tag="$(grep -m1 '"tag_name":' "${_body}" \
        | sed -E 's/.*"tag_name":[[:space:]]*"([^"]+)".*/\1/')"
    if [ -z "${_tag}" ]; then
        err "could not parse tag_name from GitHub latest-release response"
    fi
    printf '%s' "${_tag}"
}

# Strip the leading "v" from a tag (v1.2.3 -> 1.2.3) since tarball asset
# names embed the bare version per release.yml. Tags are bare SemVer,
# but this stays lenient against legacy inputs.
tag_to_version() {
    printf '%s' "$1" | sed 's/^v//'
}

# ---------------------------------------------------------------------------
# Running-binary version parsing (upgrade path)
# ---------------------------------------------------------------------------

# parse_installed_tag <bin-path>
#   Runs <bin-path> --version and extracts the second whitespace-separated
#   token, matching the format:
#     aimx <tag> (<git-sha>) <target-triple> built <date>
#   Returns empty string on any failure.
parse_installed_tag() {
    _bin="$1"
    if [ ! -x "${_bin}" ]; then
        return 0
    fi
    _out="$("${_bin}" --version 2>/dev/null || true)"
    if [ -z "${_out}" ]; then
        return 0
    fi
    case "${_out}" in
        aimx\ *) : ;;
        *) return 0 ;;
    esac
    printf '%s' "${_out}" | awk '{print $2}'
}

# Compare two SemVer-ish tags. Prints "older" / "equal" / "newer" describing
# the relationship of $1 relative to $2. Strips the leading "v" and compares
# dot-separated numeric segments pairwise; any pre-release suffix is compared
# lexicographically *only* as a tiebreaker (pre-release < release per SemVer).
compare_tags() {
    _a="$(tag_to_version "$1")"
    _b="$(tag_to_version "$2")"

    _a_core="$(printf '%s' "${_a}" | sed 's/[-+].*//')"
    _b_core="$(printf '%s' "${_b}" | sed 's/[-+].*//')"
    _a_pre="$(printf '%s' "${_a}" | sed -n 's/^[^-]*-\(.*\)$/\1/p')"
    _b_pre="$(printf '%s' "${_b}" | sed -n 's/^[^-]*-\(.*\)$/\1/p')"

    _a1="$(printf '%s' "${_a_core}" | cut -d. -f1)"
    _a2="$(printf '%s' "${_a_core}" | cut -d. -f2)"
    _a3="$(printf '%s' "${_a_core}" | cut -d. -f3)"
    _b1="$(printf '%s' "${_b_core}" | cut -d. -f1)"
    _b2="$(printf '%s' "${_b_core}" | cut -d. -f2)"
    _b3="$(printf '%s' "${_b_core}" | cut -d. -f3)"
    : "${_a1:=0}" "${_a2:=0}" "${_a3:=0}"
    : "${_b1:=0}" "${_b2:=0}" "${_b3:=0}"

    for _pair in "${_a1} ${_b1}" "${_a2} ${_b2}" "${_a3} ${_b3}"; do
        # shellcheck disable=SC2086
        set -- ${_pair}
        if [ "$1" -lt "$2" ]; then
            printf 'older'
            return 0
        fi
        if [ "$1" -gt "$2" ]; then
            printf 'newer'
            return 0
        fi
    done

    if [ -z "${_a_pre}" ] && [ -z "${_b_pre}" ]; then
        printf 'equal'
        return 0
    fi
    if [ -z "${_a_pre}" ] && [ -n "${_b_pre}" ]; then
        printf 'newer'
        return 0
    fi
    if [ -n "${_a_pre}" ] && [ -z "${_b_pre}" ]; then
        printf 'older'
        return 0
    fi
    if [ "${_a_pre}" = "${_b_pre}" ]; then
        printf 'equal'
        return 0
    fi
    _first="$(printf '%s\n%s\n' "${_a_pre}" "${_b_pre}" | LC_ALL=C sort | head -n1)"
    if [ "${_first}" = "${_a_pre}" ]; then
        printf 'older'
    else
        printf 'newer'
    fi
}

# ---------------------------------------------------------------------------
# Service control (upgrade path)
# ---------------------------------------------------------------------------

stop_service() {
    if command -v systemctl >/dev/null 2>&1; then
        if systemctl is-active --quiet aimx 2>/dev/null; then
            verbose "systemctl stop aimx"
            ${SUDO} systemctl stop aimx || err "systemctl stop aimx failed"
            printf 'systemd'
            return 0
        fi
        return 0
    fi
    if command -v rc-service >/dev/null 2>&1; then
        verbose "rc-service aimx stop"
        ${SUDO} rc-service aimx stop 2>/dev/null || true
        printf 'openrc'
        return 0
    fi
    say "warning: no systemd or OpenRC detected; skipping service stop"
    printf 'unknown'
}

start_service() {
    _init="$1"
    case "${_init}" in
        systemd)
            verbose "systemctl start aimx"
            ${SUDO} systemctl start aimx
            ;;
        openrc)
            verbose "rc-service aimx start"
            ${SUDO} rc-service aimx start
            ;;
        unknown)
            say "warning: unrecognized init system; not starting aimx.service"
            ;;
    esac
}

# ---------------------------------------------------------------------------
# Install / upgrade
# ---------------------------------------------------------------------------

TAG=""
TARGET=""
PREFIX=""
FORCE=0
DRY_RUN="${AIMX_DRY_RUN:-0}"

parse_args() {
    while [ "$#" -gt 0 ]; do
        case "$1" in
            -h | --help)
                help
                exit 0
                ;;
            --tag)
                [ "$#" -ge 2 ] || err "--tag requires a value"
                TAG="$2"
                shift 2
                ;;
            --tag=*)
                TAG="${1#--tag=}"
                shift
                ;;
            --target)
                [ "$#" -ge 2 ] || err "--target requires a value"
                TARGET="$2"
                shift 2
                ;;
            --target=*)
                TARGET="${1#--target=}"
                shift
                ;;
            --to)
                [ "$#" -ge 2 ] || err "--to requires a value"
                PREFIX="$2"
                shift 2
                ;;
            --to=*)
                PREFIX="${1#--to=}"
                shift
                ;;
            --force)
                FORCE=1
                shift
                ;;
            *)
                err "unknown argument: $1 (try --help)"
                ;;
        esac
    done
}

# ---------------------------------------------------------------------------
# Post-install orchestration (fresh install only)
# ---------------------------------------------------------------------------

# has_tty — true if /dev/tty is available for interactive prompts.
has_tty() {
    [ -e /dev/tty ] && [ -r /dev/tty ]
}

# run_aimx_setup — invokes `sudo aimx setup` with /dev/tty reattached
# so the interactive wizard (domain prompt, trust, hook picker) works
# even under `curl | sh`. Exits non-zero with diagnostics on failure.
run_aimx_setup() {
    ui_section "Steps 1-5: aimx setup (preflight, DNS, TLS, trust, service)"
    backup_existing_config
    if has_tty; then
        if ! ${SUDO} aimx setup </dev/tty; then
            ui_error "aimx setup failed"
            say "  Inspect 'aimx doctor' output and re-run 'sudo aimx setup'."
            exit 1
        fi
    else
        # No TTY available (e.g. CI). Run without reattaching; setup
        # will respect AIMX_NONINTERACTIVE=1 if set, or error.
        if ! ${SUDO} aimx setup; then
            ui_error "aimx setup failed (no TTY for interactive prompts)"
            say "  Re-run with a terminal attached, or export AIMX_NONINTERACTIVE=1"
            say "  along with the required mailbox defaults. See book/setup.md."
            exit 1
        fi
    fi
    ui_success "Steps 1-5 complete (aimx setup finished)"
}

# run_agent_setup_as_invoker — drops into the original (non-root) user
# to run `aimx agent-setup`'s interactive TUI picker. If no invoker
# can be identified the step is skipped with instructions, NOT fatal.
run_agent_setup_as_invoker() {
    ui_section "Step 6: aimx agent-setup (MCP wiring for your LLM agent)"
    _invoker="$(detect_invoker 2>/dev/null || true)"
    if [ -z "${_invoker}" ]; then
        ui_warn "Step 6 skipped: no non-root user detected"
        say "  Run as your agent user: aimx agent-setup"
        return 0
    fi
    if ! has_tty; then
        ui_warn "Step 6 skipped: no TTY available for the agent picker"
        say "  Run as ${_invoker}: aimx agent-setup"
        return 0
    fi
    _euid="$(id -u 2>/dev/null || echo 0)"
    if [ "${_euid}" -eq 0 ]; then
        # Started as root; drop to the invoker. `sudo -H` resets HOME
        # to the target user's passwd-entry home, and we also pin HOME
        # explicitly via `env HOME=...` because some sudoers
        # configurations do not honor `-H` reliably. The agent plugin
        # bundles (Claude Code, Codex CLI, etc.) end up under the
        # invoker's home, not /root.
        _inv_home="$(getent passwd "${_invoker}" 2>/dev/null | cut -d: -f6)"
        if [ -z "${_inv_home}" ]; then
            _inv_home="/home/${_invoker}"
        fi
        sudo -H -u "${_invoker}" env HOME="${_inv_home}" sh -c 'aimx agent-setup </dev/tty' || {
            ui_warn "Step 6: aimx agent-setup returned non-zero (continuing)"
            say "  Re-run as ${_invoker}: aimx agent-setup"
            return 0
        }
    else
        sh -c 'aimx agent-setup </dev/tty' || {
            ui_warn "Step 6: aimx agent-setup returned non-zero (continuing)"
            say "  Re-run: aimx agent-setup"
            return 0
        }
    fi
    ui_success "Step 6 complete (MCP wired)"
}

# extract_domain — pull the configured domain from /etc/aimx/config.toml.
# Falls back to the literal placeholder "<your-domain>" when the file is
# unreadable. $SUDO is used to step past the 0640 root-only config mode
# (empty when we're already root, "sudo" otherwise).
extract_domain() {
    _cfg="${AIMX_CONFIG_TOML}"
    _dom=""
    if [ -r "${_cfg}" ]; then
        _dom="$(awk -F'"' '/^[[:space:]]*domain[[:space:]]*=/{print $2; exit}' "${_cfg}" 2>/dev/null || true)"
    fi
    if [ -z "${_dom}" ]; then
        _dom="$(${SUDO} awk -F'"' '/^[[:space:]]*domain[[:space:]]*=/{print $2; exit}' "${_cfg}" 2>/dev/null || true)"
    fi
    if [ -z "${_dom}" ]; then
        _dom="<your-domain>"
    fi
    printf '%s' "${_dom}"
}

print_closing_message() {
    _domain="$1"
    printf '\n' >&2
    ui_success "AIMX has been set up successfully."
    printf '\n' >&2
    say "Your agents now have access to set up, send and receive emails from @${_domain}."
    printf '\n' >&2
    say "Once you have linked up your MCP to your LLM, try asking it to set up a mailbox for you, e.g."
    say "  claude -p \"Set up agent@${_domain} and respond to me via email the moment you receive my instructions via email.\""
    printf '\n' >&2
    say "Any questions?"
}

# ---------------------------------------------------------------------------
# main
# ---------------------------------------------------------------------------

main() {
    parse_args "$@"

    # Env-var defaults (flags already win).
    if [ -z "${TAG}" ] && [ -n "${AIMX_VERSION:-}" ]; then
        TAG="${AIMX_VERSION}"
    fi
    if [ -z "${PREFIX}" ] && [ -n "${AIMX_PREFIX:-}" ]; then
        PREFIX="${AIMX_PREFIX}"
    fi
    if [ -z "${PREFIX}" ]; then
        PREFIX="${DEFAULT_PREFIX}"
    fi

    # Banner first so operators see the plan even before sudo prompts.
    print_welcome_banner

    # Platform detection.
    detect_os >/dev/null
    if [ -z "${TARGET}" ]; then
        _arch="$(detect_arch)"
        _libc="$(detect_libc)"
        TARGET="$(compose_target "${_arch}" "${_libc}")"
    fi
    case "${TARGET}" in
        x86_64-unknown-linux-gnu | aarch64-unknown-linux-gnu \
            | x86_64-unknown-linux-musl | aarch64-unknown-linux-musl) : ;;
        *)
            err "unsupported target triple: ${TARGET}. See ${UNSUPPORTED_DOC}"
            ;;
    esac

    need uname
    need tar
    need mkdir
    need rm
    need install
    need awk
    need sed
    need grep

    # Temp dir for downloads.
    _td="$(mktemp -d 2>/dev/null || mktemp -d -t aimx-install)"
    [ -d "${_td}" ] || err "mktemp failed"
    trap cleanup EXIT INT TERM

    # Resolve tag.
    if [ -z "${TAG}" ]; then
        say "resolving latest release from ${GITHUB_API}/latest"
        if [ "${DRY_RUN}" = "1" ]; then
            # Skip network for dry-run.
            TAG="0.0.0"
            say "dry-run: would resolve latest tag from GitHub"
        else
            TAG="$(resolve_latest_tag)"
        fi
    fi
    # Tags are bare SemVer. A caller-supplied `v` prefix would compose a
    # non-existent `/download/v…/` URL; strip it leniently.
    case "${TAG}" in
        v[0-9]*)
            _stripped="${TAG#v}"
            say "normalized tag: ${TAG} -> ${_stripped} (bare SemVer)"
            TAG="${_stripped}"
            ;;
    esac
    _version="$(tag_to_version "${TAG}")"
    _artifact_target="$(artifact_target "${TARGET}")"

    _asset="aimx-${_version}-${_artifact_target}.tar.gz"
    _url="${GITHUB_DL}/${TAG}/${_asset}"
    _install_path="${PREFIX}/aimx"

    say "target:  ${TARGET}"
    say "tarball: ${_url}"
    say "install path: ${_install_path}"

    # Upgrade-vs-fresh decision. Only matters when a binary is already
    # present at ${_install_path}.
    _installed_tag=""
    _is_upgrade=0
    if [ -x "${_install_path}" ]; then
        _installed_tag="$(parse_installed_tag "${_install_path}")"
        if [ -n "${_installed_tag}" ]; then
            _cmp="$(compare_tags "${_installed_tag}" "${TAG}")"
            case "${_cmp}" in
                equal)
                    if [ "${FORCE}" -ne 1 ]; then
                        say "aimx ${_installed_tag} is already installed (pass --force to re-install)"
                        exit 0
                    fi
                    say "re-installing ${TAG} (--force)"
                    _is_upgrade=1
                    ;;
                newer)
                    err "installed ${_installed_tag} is newer than target ${TAG}; run 'aimx upgrade --version ${TAG} --force' to downgrade explicitly"
                    ;;
                older)
                    say "upgrading ${_installed_tag} -> ${TAG}"
                    _is_upgrade=1
                    ;;
            esac
        else
            say "existing binary at ${_install_path} did not report a parseable version; proceeding as upgrade"
            _is_upgrade=1
        fi
    fi

    if [ "${DRY_RUN}" = "1" ]; then
        say "dry-run: would download ${_url}"
        say "dry-run: would extract tarball under ${_td}"
        if [ "${_is_upgrade}" -eq 1 ]; then
            say "dry-run: would stop aimx.service, swap binary, restart (upgrade)"
            say "dry-run: steps 1-4 and 6 would be marked skipped (upgrade)"
        else
            say "dry-run: would ensure_sudo, install to ${_install_path}"
            say "dry-run: would run 'sudo aimx setup' (steps 1-5)"
            say "dry-run: would run 'aimx agent-setup' as invoking user (step 6)"
            say "dry-run: would print closing message with configured domain"
        fi
        say "dry-run: no filesystem changes made"
        exit 0
    fi

    # From here on we touch the filesystem; acquire sudo up front so the
    # operator is prompted once rather than sprinkled across steps. Then
    # resolve the $SUDO prefix so every privileged call site uses the
    # right form (empty when root, "sudo" otherwise).
    ensure_sudo
    resolve_sudo_prefix

    if [ ! -d "${PREFIX}" ]; then
        if ! ${SUDO} mkdir -p "${PREFIX}" 2>/dev/null; then
            err "install prefix ${PREFIX} does not exist and cannot be created"
        fi
    fi

    # Download + extract.
    _tarball="${_td}/${_asset}"
    say "downloading ${_asset}"
    download "${_url}" "${_tarball}"
    verbose "extracting ${_tarball}"
    (cd "${_td}" && tar -xzf "${_asset}") || err "tar extract failed"
    _staged="${_td}/aimx-${_version}-${_artifact_target}/aimx"
    if [ ! -x "${_staged}" ]; then
        err "extracted tarball missing executable aimx at expected path"
    fi

    if [ "${_is_upgrade}" -eq 1 ]; then
        # Upgrade path: non-interactive by design. Steps 1-4 and 6 are
        # skipped because the previous setup is preserved.
        _init="$(stop_service || echo unknown)"

        _prev="${_install_path}.prev"

        if [ -f "${_install_path}" ]; then
            if ! ${SUDO} mv -f "${_install_path}" "${_prev}"; then
                say "✗ failed to preserve ${_install_path} as ${_prev}"
                start_service "${_init}" || true
                err "aborting upgrade; previous binary still in place"
            fi
        fi

        if ! ${SUDO} install -m 0755 "${_staged}" "${_install_path}"; then
            say "✗ install failed; rolling back"
            if [ -f "${_prev}" ]; then
                ${SUDO} mv -f "${_prev}" "${_install_path}" || true
            fi
            start_service "${_init}" || true
            err "upgrade failed at install step; service restored"
        fi

        if ! start_service "${_init}"; then
            say "✗ service start failed; rolling back"
            if [ -f "${_prev}" ]; then
                ${SUDO} mv -f "${_prev}" "${_install_path}" || true
                start_service "${_init}" || true
            fi
            err "upgrade failed at start step; service restored if possible"
        fi

        ui_info "Step 1 skipped (upgrade - previous setup preserved)"
        ui_info "Step 2 skipped (upgrade - DNS unchanged)"
        ui_info "Step 3 skipped (upgrade - TLS unchanged)"
        ui_info "Step 4 skipped (upgrade - trust policy unchanged)"
        if [ -n "${_installed_tag}" ]; then
            ui_success "Step 5 complete (aimx upgraded from ${_installed_tag} to ${TAG})"
        else
            ui_success "Step 5 complete (aimx installed at ${TAG})"
        fi
        ui_info "Step 6 skipped (upgrade - MCP remains wired as before)"
        say "upgrade complete. Run 'aimx doctor' for health."
        exit 0
    fi

    # Fresh install path.
    ${SUDO} install -m 0755 "${_staged}" "${_install_path}" \
        || err "install failed writing ${_install_path}"
    ui_success "aimx ${TAG} installed to ${_install_path}"

    # Steps 1-5: invoke `sudo aimx setup` with TTY reattached.
    run_aimx_setup

    # Step 6: drop to the invoking user and run `aimx agent-setup`.
    run_agent_setup_as_invoker

    # Closing message.
    _domain="$(extract_domain)"
    print_closing_message "${_domain}"
}

# Honor INSTALL_SH_TEST=1 so unit tests can source the script to probe
# individual helpers without triggering the full install flow.
if [ "${INSTALL_SH_TEST:-0}" != "1" ]; then
    main "$@"
fi
