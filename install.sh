#!/bin/sh
# aimx install script — POSIX sh (dash / busybox compatible).
#
# Usage:
#   curl -fsSL https://aimx.email/install.sh | sh
#   curl -fsSL https://aimx.email/install.sh | sh -s -- --tag v1.2.3
#
# Modelled on `just.systems/install.sh` — `say` / `err` / `need` / `download`
# helper idioms, no bashisms, HTTPS-only trust anchor. Implements PRD
# onboarding FR-2.1–2.9 (see docs/onboarding-prd.md).

set -eu

# ---------------------------------------------------------------------------
# Constants
# ---------------------------------------------------------------------------

GITHUB_REPO="uzyn/aimx"
GITHUB_API="https://api.github.com/repos/${GITHUB_REPO}/releases"
GITHUB_DL="https://github.com/${GITHUB_REPO}/releases/download"
DEFAULT_PREFIX="/usr/local/bin"
UNSUPPORTED_DOC="https://aimx.email/book/installation.html#unsupported-platforms"

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
        --tag <VERSION>      Install a specific release tag (e.g. v1.2.3);
                             overrides AIMX_VERSION env var
        --target <TRIPLE>    Override target auto-detection
                             (x86_64-unknown-linux-gnu,
                              aarch64-unknown-linux-gnu,
                              x86_64-unknown-linux-musl,
                              aarch64-unknown-linux-musl)
        --to <DIR>           Install binary into DIR (default /usr/local/bin);
                             overrides AIMX_PREFIX env var
        --force              Re-install even if target version already present

ENVIRONMENT:
    AIMX_VERSION             Release tag to install (e.g. v1.2.3)
    AIMX_PREFIX              Install directory (default /usr/local/bin)
    AIMX_DRY_RUN=1           Print every step without downloading or installing
    AIMX_VERBOSE=1           Trace HTTP requests and filesystem actions
    GITHUB_TOKEN             Token for rate-limited GitHub API calls

EXAMPLES:
    # Latest stable into /usr/local/bin
    curl -fsSL https://aimx.email/install.sh | sh

    # Pin a specific tag
    curl -fsSL https://aimx.email/install.sh | sh -s -- --tag v1.2.3

    # Dry-run: see what would happen without installing
    curl -fsSL https://aimx.email/install.sh | AIMX_DRY_RUN=1 sh

Trust anchor is HTTPS on the GitHub Releases domain. No signature or
checksum verification in this script; skeptical operators can verify
manually via the 'curl + sha256sum -c' block in the release notes.
EOF
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
# names embed the bare version per release.yml (FR-1.2).
tag_to_version() {
    printf '%s' "$1" | sed 's/^v//'
}

# ---------------------------------------------------------------------------
# Running-binary version parsing (upgrade path)
# ---------------------------------------------------------------------------

# parse_installed_tag <bin-path>
#   Runs <bin-path> --version and extracts the second whitespace-separated
#   token, matching the Sprint 2 format:
#     aimx <tag> (<git-sha>) <target-triple> built <date>
#   Returns empty string on any failure.
parse_installed_tag() {
    _bin="$1"
    if [ ! -x "${_bin}" ]; then
        return 0
    fi
    # Capture stdout only; a binary that aborts on --version prints nothing
    # here and we treat that as "unknown version" (falls through to install).
    _out="$("${_bin}" --version 2>/dev/null || true)"
    if [ -z "${_out}" ]; then
        return 0
    fi
    # Expect the literal "aimx" prefix. If not, treat as unknown.
    case "${_out}" in
        aimx\ *) : ;;
        *) return 0 ;;
    esac
    # Second whitespace-separated token is the tag.
    printf '%s' "${_out}" | awk '{print $2}'
}

# Compare two SemVer-ish tags. Prints "older" / "equal" / "newer" describing
# the relationship of $1 relative to $2 (i.e. is $1 older/equal/newer than
# $2?). Strips the leading "v" and compares dot-separated numeric segments
# pairwise; any pre-release suffix (-rc1, -fixture) is compared
# lexicographically *only* as a tiebreaker (pre-release < release per
# SemVer). Good enough for the install-script upgrade heuristic; the real
# Sprint 4 `aimx upgrade` uses a proper crate.
compare_tags() {
    _a="$(tag_to_version "$1")"
    _b="$(tag_to_version "$2")"

    _a_core="$(printf '%s' "${_a}" | sed 's/[-+].*//')"
    _b_core="$(printf '%s' "${_b}" | sed 's/[-+].*//')"
    _a_pre="$(printf '%s' "${_a}" | sed -n 's/^[^-]*-\(.*\)$/\1/p')"
    _b_pre="$(printf '%s' "${_b}" | sed -n 's/^[^-]*-\(.*\)$/\1/p')"

    # Pad to three segments.
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

    # Cores equal. Apply pre-release tiebreaker: no-pre > has-pre; if both
    # have pre, compare strings.
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
    # Lexicographic tiebreaker via sort; POSIX `sort` supports -C (silent
    # order check; exit 0 iff already sorted). First line is the "smaller".
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
            systemctl stop aimx || err "systemctl stop aimx failed"
            printf 'systemd'
            return 0
        fi
        return 0
    fi
    if command -v rc-service >/dev/null 2>&1; then
        verbose "rc-service aimx stop"
        rc-service aimx stop 2>/dev/null || true
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
            systemctl start aimx
            ;;
        openrc)
            verbose "rc-service aimx start"
            rc-service aimx start
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
        TAG="$(resolve_latest_tag)"
    fi
    _version="$(tag_to_version "${TAG}")"

    _asset="aimx-${_version}-${TARGET}.tar.gz"
    _url="${GITHUB_DL}/${TAG}/${_asset}"
    _install_path="${PREFIX}/aimx"

    # Print the three facts operators want to see before any download.
    say "target:  ${TARGET}"
    say "tarball: ${_url}"
    say "install: ${_install_path}"

    # Upgrade-vs-fresh decision (FR-2.5). Only matters when a binary is
    # already present at ${_install_path}.
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
            # Binary present but --version unparseable. Treat as upgrade
            # so the service-aware swap path runs.
            say "existing binary at ${_install_path} did not report a parseable version; proceeding as upgrade"
            _is_upgrade=1
        fi
    fi

    if [ "${DRY_RUN}" = "1" ]; then
        say "dry-run: would download ${_url}"
        say "dry-run: would extract tarball under ${_td}"
        if [ "${_is_upgrade}" -eq 1 ]; then
            say "dry-run: would stop aimx.service, swap binary, restart"
        else
            say "dry-run: would install to ${_install_path}"
        fi
        say "dry-run: no filesystem changes made"
        exit 0
    fi

    # For the upgrade path and writes to system dirs, enforce root.
    _euid="$(id -u 2>/dev/null || echo 0)"
    if [ "${_is_upgrade}" -eq 1 ] && [ "${_euid}" -ne 0 ]; then
        err "upgrade path needs root to stop aimx.service and write to ${PREFIX}; re-run with sudo"
    fi
    if [ ! -d "${PREFIX}" ]; then
        # Try to create it. If we can't, surface a clear error.
        if ! mkdir -p "${PREFIX}" 2>/dev/null; then
            err "install prefix ${PREFIX} does not exist and cannot be created (re-run with sudo, or use --to)"
        fi
    fi
    if [ ! -w "${PREFIX}" ]; then
        err "install prefix ${PREFIX} is not writable (re-run with sudo, or use --to)"
    fi

    # Download + extract.
    _tarball="${_td}/${_asset}"
    say "downloading ${_asset}"
    download "${_url}" "${_tarball}"
    verbose "extracting ${_tarball}"
    (cd "${_td}" && tar -xzf "${_asset}") || err "tar extract failed"
    _staged="${_td}/aimx-${_version}-${TARGET}/aimx"
    if [ ! -x "${_staged}" ]; then
        err "extracted tarball missing executable aimx at expected path"
    fi

    if [ "${_is_upgrade}" -eq 1 ]; then
        _init="$(stop_service)"
        _prev="${_install_path}.prev"

        # Preserve the existing binary at .prev.
        if [ -f "${_install_path}" ]; then
            if ! mv -f "${_install_path}" "${_prev}"; then
                say "✗ failed to preserve ${_install_path} as ${_prev}"
                start_service "${_init}"
                err "aborting upgrade; previous binary still in place"
            fi
        fi

        # Install new binary.
        if ! install -m 0755 "${_staged}" "${_install_path}"; then
            # Rollback.
            say "✗ install failed; rolling back"
            if [ -f "${_prev}" ]; then
                mv -f "${_prev}" "${_install_path}" || true
            fi
            start_service "${_init}"
            err "upgrade failed at install step; service restored"
        fi

        # Start service.
        if ! start_service "${_init}"; then
            say "✗ service start failed; rolling back"
            if [ -f "${_prev}" ]; then
                mv -f "${_prev}" "${_install_path}" || true
                start_service "${_init}" || true
            fi
            err "upgrade failed at start step; service restored if possible"
        fi

        if [ -n "${_installed_tag}" ]; then
            say "aimx ${_installed_tag} -> ${TAG}. Service restarted."
        else
            say "aimx installed at ${TAG}. Service restarted."
        fi
        say "upgrade complete. Run 'aimx doctor' for health."
        exit 0
    fi

    # Fresh install.
    install -m 0755 "${_staged}" "${_install_path}" \
        || err "install failed writing ${_install_path}"

    say "✓ aimx ${TAG} installed to ${_install_path}"
    say "→ next: sudo aimx setup"
}

main "$@"
