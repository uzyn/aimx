#!/bin/sh
# aimx portcheck — POSIX sh alias for `install.sh --port-check-only`.
#
# Usage:
#   curl -fsSL https://aimx.email/portcheck.sh | sh
#   curl -fsSL https://aimx.email/portcheck.sh | sh -s -- --verify-host https://check.example.com
#
# Re-fetches install.sh from the same origin and execs it with
# `--port-check-only` forced on. Any extra args are forwarded so
# `--verify-host`, `--help`, etc. still work.
#
# Trust anchor is HTTPS on aimx.email — same as install.sh.

set -eu

URL="${AIMX_INSTALL_URL:-https://aimx.email/install.sh}"

case "${URL}" in
    https://*) : ;;
    *)
        printf 'portcheck: refusing non-HTTPS install URL: %s\n' "${URL}" >&2
        exit 1
        ;;
esac

_td="$(mktemp -d 2>/dev/null || mktemp -d -t aimx-portcheck)"
[ -d "${_td}" ] || { printf 'portcheck: mktemp failed\n' >&2; exit 1; }
trap 'rm -rf "${_td}"' EXIT INT TERM

if command -v curl >/dev/null 2>&1; then
    curl --proto '=https' --tlsv1.2 -fsSL -o "${_td}/install.sh" "${URL}"
elif command -v wget >/dev/null 2>&1; then
    wget --https-only -q -O "${_td}/install.sh" "${URL}"
else
    printf 'portcheck: need curl or wget on PATH\n' >&2
    exit 1
fi

exec sh "${_td}/install.sh" --port-check-only "$@"
