#!/bin/sh
# Shell-level tests for install.sh helpers.
#
# Run as: sh tests/install_sh.sh
#
# Exercises the pure-shell helper functions (detect_invoker,
# backup_existing_config, ensure_sudo, parse_args) without touching the
# real filesystem or network. Sourcing install.sh under INSTALL_SH_TEST=1
# skips the auto-invocation of `main` at the bottom of the script.

set -eu

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
ROOT_DIR="$(cd "${SCRIPT_DIR}/.." && pwd)"
INSTALL_SH="${ROOT_DIR}/install.sh"

PASS=0
FAIL=0
FAILED_NAMES=""

assert_eq() {
    _label="$1"
    _want="$2"
    _got="$3"
    if [ "${_want}" = "${_got}" ]; then
        PASS=$((PASS + 1))
        printf '  ok  %s\n' "${_label}"
    else
        FAIL=$((FAIL + 1))
        FAILED_NAMES="${FAILED_NAMES} ${_label}"
        printf '  FAIL %s\n' "${_label}" >&2
        printf '       want: %s\n' "${_want}" >&2
        printf '       got:  %s\n' "${_got}" >&2
    fi
}

assert_contains() {
    _label="$1"
    _hay="$2"
    _needle="$3"
    case "${_hay}" in
        *"${_needle}"*)
            PASS=$((PASS + 1))
            printf '  ok  %s\n' "${_label}"
            ;;
        *)
            FAIL=$((FAIL + 1))
            FAILED_NAMES="${FAILED_NAMES} ${_label}"
            printf '  FAIL %s\n' "${_label}" >&2
            printf '       needle: %s\n' "${_needle}" >&2
            printf '       hay:    %s\n' "${_hay}" >&2
            ;;
    esac
}

assert_not_contains() {
    _label="$1"
    _hay="$2"
    _needle="$3"
    case "${_hay}" in
        *"${_needle}"*)
            FAIL=$((FAIL + 1))
            FAILED_NAMES="${FAILED_NAMES} ${_label}"
            printf '  FAIL %s\n' "${_label}" >&2
            printf '       forbidden needle: %s\n' "${_needle}" >&2
            printf '       hay:              %s\n' "${_hay}" >&2
            ;;
        *)
            PASS=$((PASS + 1))
            printf '  ok  %s\n' "${_label}"
            ;;
    esac
}

assert_zero() {
    _label="$1"
    _code="$2"
    if [ "${_code}" = "0" ]; then
        PASS=$((PASS + 1))
        printf '  ok  %s\n' "${_label}"
    else
        FAIL=$((FAIL + 1))
        FAILED_NAMES="${FAILED_NAMES} ${_label}"
        printf '  FAIL %s (rc=%s)\n' "${_label}" "${_code}" >&2
    fi
}

# ---------------------------------------------------------------------------
# 1. Syntax checks
# ---------------------------------------------------------------------------

echo "# syntax"

_rc=0
sh -n "${INSTALL_SH}" 2>/dev/null || _rc=$?
assert_zero "sh -n install.sh" "${_rc}"

if command -v dash >/dev/null 2>&1; then
    _rc=0
    dash -n "${INSTALL_SH}" 2>/dev/null || _rc=$?
    assert_zero "dash -n install.sh" "${_rc}"
else
    echo "  skip dash -n (no dash on PATH)"
fi

# ---------------------------------------------------------------------------
# 2. shellcheck (optional)
# ---------------------------------------------------------------------------

echo "# shellcheck"
if command -v shellcheck >/dev/null 2>&1; then
    _rc=0
    shellcheck "${INSTALL_SH}" || _rc=$?
    assert_zero "shellcheck install.sh" "${_rc}"
    _rc=0
    shellcheck -s sh "${SCRIPT_DIR}/install_sh.sh" || _rc=$?
    assert_zero "shellcheck tests/install_sh.sh" "${_rc}"
else
    echo "  skip (shellcheck not installed)"
fi

# ---------------------------------------------------------------------------
# 3. Sourcing the script under INSTALL_SH_TEST=1 must not invoke main
# ---------------------------------------------------------------------------

echo "# source guard"

# If the guard is honored, the sourced script defines helpers but never
# reaches the filesystem-mutating main path. We run it under a subshell
# so side effects (sets, traps, cleanup) don't leak into the outer harness.
_out="$(
    INSTALL_SH_TEST=1 sh -c '
        . "'"${INSTALL_SH}"'"
        # If we got here, main() did not auto-fire.
        echo "SOURCED_OK"
        # Probe for helpers.
        type detect_invoker >/dev/null 2>&1 && echo "HAS detect_invoker"
        type ensure_sudo >/dev/null 2>&1 && echo "HAS ensure_sudo"
        type backup_existing_config >/dev/null 2>&1 && echo "HAS backup_existing_config"
        type print_install_banner >/dev/null 2>&1 && echo "HAS print_install_banner"
        type parse_args >/dev/null 2>&1 && echo "HAS parse_args"
        type ui_info >/dev/null 2>&1 && echo "HAS ui_info"
        type ui_warn >/dev/null 2>&1 && echo "HAS ui_warn"
        type ui_error >/dev/null 2>&1 && echo "HAS ui_error"
        type ui_success >/dev/null 2>&1 && echo "HAS ui_success"
    ' 2>&1
)"
assert_contains "INSTALL_SH_TEST=1 suppresses main" "${_out}" "SOURCED_OK"
assert_contains "helper: detect_invoker"           "${_out}" "HAS detect_invoker"
assert_contains "helper: ensure_sudo"              "${_out}" "HAS ensure_sudo"
assert_contains "helper: backup_existing_config"   "${_out}" "HAS backup_existing_config"
assert_contains "helper: print_install_banner"     "${_out}" "HAS print_install_banner"
assert_contains "helper: parse_args"               "${_out}" "HAS parse_args"
assert_contains "helper: ui_info"                  "${_out}" "HAS ui_info"
assert_contains "helper: ui_warn"                  "${_out}" "HAS ui_warn"
assert_contains "helper: ui_error"                 "${_out}" "HAS ui_error"
assert_contains "helper: ui_success"               "${_out}" "HAS ui_success"

# Removed-helper regression: the binary owns the six-step UI now.
# `has_tty` was removed when the post-install handoff was rewritten to
# guard on `[ -t 0 ]` (stdin-is-a-tty) instead of `/dev/tty` existence.
_out="$(
    INSTALL_SH_TEST=1 sh -c '
        . "'"${INSTALL_SH}"'"
        for _h in print_welcome_banner print_final_banner print_step_list \
                  set_step _step_glyph extract_domain print_closing_message \
                  run_agent_setup_as_invoker run_aimx_setup has_tty; do
            if type "$_h" >/dev/null 2>&1; then
                echo "STILL_DEFINED $_h"
            fi
        done
    ' 2>&1
)"
assert_not_contains "removed helpers stay gone" "${_out}" "STILL_DEFINED"

# ---------------------------------------------------------------------------
# 3b. TTY-stdin gate regression: never redirect `</dev/tty` over an
# already-terminal stdin (sudo's use_pty bridge breaks if you do).
# Source-grep the guard structure rather than try to pty-test live.
# ---------------------------------------------------------------------------

echo "# TTY-stdin gate"

# (a) The literal `[ -t 0 ]` test must appear at least twice — once in
#     ensure_sudo, once in the post-install handoff.
_t0_count="$(grep -c '\[ -t 0 \]' "${INSTALL_SH}" || true)"
if [ "${_t0_count:-0}" -ge 2 ]; then
    PASS=$((PASS + 1))
    printf '  ok  [ -t 0 ] guard present in 2+ sites (count=%s)\n' "${_t0_count}"
else
    FAIL=$((FAIL + 1))
    FAILED_NAMES="${FAILED_NAMES} tty-stdin-guard-count"
    printf '  FAIL [ -t 0 ] guard count=%s, expected >= 2\n' "${_t0_count:-0}" >&2
fi

# (b) The script must contain an unredirected `exec ${SUDO} aimx setup`
#     form (the [ -t 0 ] branch) — proves the fix is in place, not just
#     the legacy redirect form.
_unredirected="$(grep -c 'exec \${SUDO} aimx setup$' "${INSTALL_SH}" || true)"
if [ "${_unredirected:-0}" -ge 1 ]; then
    PASS=$((PASS + 1))
    printf '  ok  unredirected `exec ${SUDO} aimx setup` present (count=%s)\n' "${_unredirected}"
else
    FAIL=$((FAIL + 1))
    FAILED_NAMES="${FAILED_NAMES} tty-stdin-unredirected"
    printf '  FAIL no unredirected `exec ${SUDO} aimx setup` line found\n' >&2
fi

# (c) The redirected `exec ${SUDO} aimx setup </dev/tty` form must appear
#     exactly once and its preceding line must be an `elif [ -e /dev/tty`
#     guard (i.e., reached only when [ -t 0 ] was already false).
_redirected_line="$(grep -n 'exec \${SUDO} aimx setup </dev/tty' "${INSTALL_SH}" || true)"
_redirected_count="$(printf '%s\n' "${_redirected_line}" | grep -c . || true)"
if [ "${_redirected_count:-0}" -eq 1 ]; then
    PASS=$((PASS + 1))
    printf '  ok  redirected handoff appears exactly once\n'
    _ln="$(printf '%s' "${_redirected_line}" | cut -d: -f1)"
    _prev_ln=$((_ln - 1))
    _prev_text="$(sed -n "${_prev_ln}p" "${INSTALL_SH}")"
    case "${_prev_text}" in
        *"elif [ -e /dev/tty ]"*)
            PASS=$((PASS + 1))
            printf '  ok  redirected handoff is gated behind elif [ -e /dev/tty ]\n'
            ;;
        *)
            FAIL=$((FAIL + 1))
            FAILED_NAMES="${FAILED_NAMES} tty-stdin-redirect-gate"
            printf '  FAIL redirected handoff not gated by elif [ -e /dev/tty ] (prev line: %s)\n' "${_prev_text}" >&2
            ;;
    esac
else
    FAIL=$((FAIL + 1))
    FAILED_NAMES="${FAILED_NAMES} tty-stdin-redirect-count"
    printf '  FAIL redirected handoff count=%s, expected 1\n' "${_redirected_count:-0}" >&2
fi

# (d) Same shape for sudo -v in ensure_sudo: the redirected
#     `sudo -v </dev/tty` must be inside an `elif [ -e /dev/tty ]` branch.
_sudov_line="$(grep -n 'sudo -v </dev/tty' "${INSTALL_SH}" || true)"
if [ -n "${_sudov_line}" ]; then
    _ln="$(printf '%s' "${_sudov_line}" | head -n1 | cut -d: -f1)"
    _prev_ln=$((_ln - 1))
    _prev_text="$(sed -n "${_prev_ln}p" "${INSTALL_SH}")"
    case "${_prev_text}" in
        *"elif [ -e /dev/tty ]"*)
            PASS=$((PASS + 1))
            printf '  ok  sudo -v </dev/tty is gated behind elif [ -e /dev/tty ]\n'
            ;;
        *)
            FAIL=$((FAIL + 1))
            FAILED_NAMES="${FAILED_NAMES} sudo-v-redirect-gate"
            printf '  FAIL sudo -v </dev/tty not gated by elif [ -e /dev/tty ] (prev line: %s)\n' "${_prev_text}" >&2
            ;;
    esac
else
    FAIL=$((FAIL + 1))
    FAILED_NAMES="${FAILED_NAMES} sudo-v-redirect-missing"
    printf '  FAIL no sudo -v </dev/tty line found at all\n' >&2
fi

# ---------------------------------------------------------------------------
# 4. detect_invoker
# ---------------------------------------------------------------------------

echo "# detect_invoker"

# Case A: SUDO_USER="alice" → prints "alice" and returns 0.
_out="$(
    INSTALL_SH_TEST=1 SUDO_USER=alice sh -c '
        . "'"${INSTALL_SH}"'"
        if detect_invoker; then
            printf "\nRC=0"
        else
            printf "\nRC=%d" $?
        fi
    '
)"
assert_contains "SUDO_USER=alice → prints alice" "${_out}" "alice"
assert_contains "SUDO_USER=alice → rc 0"         "${_out}" "RC=0"

# Case B: SUDO_USER="root" → treated as "no sudo indirection".
# Whether it succeeds depends on euid; the key is that the stdout
# is NOT "root".
_out="$(
    INSTALL_SH_TEST=1 SUDO_USER=root sh -c '
        . "'"${INSTALL_SH}"'"
        _x="$(detect_invoker 2>/dev/null || true)"
        printf "INVOKER=[%s]" "${_x}"
    '
)"
assert_not_contains "SUDO_USER=root → does not echo root" "${_out}" "INVOKER=[root]"

# Case C: SUDO_USER unset, running as non-root user → prints current id -un.
_me="$(id -un 2>/dev/null || echo '')"
if [ "${_me}" != "root" ] && [ -n "${_me}" ]; then
    _out="$(
        INSTALL_SH_TEST=1 sh -c '
            unset SUDO_USER
            . "'"${INSTALL_SH}"'"
            detect_invoker
        '
    )"
    assert_eq "non-root user → prints current user" "${_me}" "${_out}"
fi

# ---------------------------------------------------------------------------
# 5. backup_existing_config
# ---------------------------------------------------------------------------

echo "# backup_existing_config"

_tmp="$(mktemp -d 2>/dev/null || mktemp -d -t aimxtest)"
trap 'rm -rf "'"${_tmp}"'"' EXIT INT TERM

mkdir -p "${_tmp}/etc/aimx" "${_tmp}/bin"
printf 'domain = "example.com"\n' > "${_tmp}/etc/aimx/config.toml"

# Stub `sudo` on PATH: passthrough, stripping leading flags. The tests
# inject this PATH so the helper doesn't touch real /etc/aimx.
cat > "${_tmp}/bin/sudo" <<'SUDOEOF'
#!/bin/sh
# Passthrough stub: discard leading sudo flags, exec the command.
while [ $# -gt 0 ]; do
    case "$1" in
        -n|-v|-S|-H|-i|-p|--preserve-env|--non-interactive) shift ;;
        -u) shift 2 ;;
        -*) shift ;;
        --) shift; break ;;
        *) break ;;
    esac
done
exec "$@"
SUDOEOF
chmod +x "${_tmp}/bin/sudo"

# Drive the helper with a custom config path via an env override we add
# to install.sh specifically for testability (AIMX_INSTALL_CONFIG_PATH).
_out="$(
    INSTALL_SH_TEST=1 \
    AIMX_INSTALL_CONFIG_PATH="${_tmp}/etc/aimx/config.toml" \
    PATH="${_tmp}/bin:${PATH}" \
    sh -c '
        . "'"${INSTALL_SH}"'"
        backup_existing_config
    ' 2>&1
)"
# Original file must now be gone.
if [ ! -f "${_tmp}/etc/aimx/config.toml" ]; then
    PASS=$((PASS + 1))
    printf '  ok  original config.toml removed\n'
else
    FAIL=$((FAIL + 1))
    FAILED_NAMES="${FAILED_NAMES} backup-original-removed"
    printf '  FAIL original config.toml still present\n' >&2
fi
# Some config.toml.bak-* must exist.
_bak="$(ls "${_tmp}/etc/aimx/" 2>/dev/null | grep '^config.toml.bak-' || true)"
assert_contains "backup file created" "${_bak}" "config.toml.bak-"

# Re-run with no config present: helper must be a no-op and exit clean.
rm -f "${_tmp}/etc/aimx/"*
_rc=0
INSTALL_SH_TEST=1 \
AIMX_INSTALL_CONFIG_PATH="${_tmp}/etc/aimx/config.toml" \
PATH="${_tmp}/bin:${PATH}" \
sh -c '
    . "'"${INSTALL_SH}"'"
    backup_existing_config
' >/dev/null 2>&1 || _rc=$?
assert_zero "backup no-op when config missing" "${_rc}"

# ---------------------------------------------------------------------------
# 6. ensure_sudo (with mocked sudo)
# ---------------------------------------------------------------------------

echo "# ensure_sudo"

# Mock sudo that always succeeds on -n true (passwordless).
cat > "${_tmp}/bin/sudo" <<'SUDOEOF'
#!/bin/sh
case "$1" in
    -n)
        shift
        case "$1" in true) exit 0 ;; esac
        exec "$@"
        ;;
    -v) exit 0 ;;
esac
exec "$@"
SUDOEOF
chmod +x "${_tmp}/bin/sudo"

_rc=0
INSTALL_SH_TEST=1 \
PATH="${_tmp}/bin:${PATH}" \
sh -c '
    . "'"${INSTALL_SH}"'"
    ensure_sudo
' >/dev/null 2>&1 || _rc=$?
assert_zero "ensure_sudo passwordless branch" "${_rc}"

# Mock sudo that fails -n true but succeeds on -v (password prompt path).
cat > "${_tmp}/bin/sudo" <<'SUDOEOF'
#!/bin/sh
case "$1" in
    -n) exit 1 ;;
    -v) exit 0 ;;
esac
exit 0
SUDOEOF
chmod +x "${_tmp}/bin/sudo"

# Password-prompt branch: the key contract is that we print the
# "Administrator privileges required" hint before invoking `sudo -v`.
# Whether `sudo -v` itself returns 0 depends on /dev/tty availability
# in the host environment, which we cannot portably mock, so only the
# user-visible message is asserted. The outer `|| true` shields the
# assignment from dash's set-e-on-failed-subst behavior when the helper
# exits 1 due to the unmockable tty redirect.
_out="$(
    INSTALL_SH_TEST=1 \
    PATH="${_tmp}/bin:${PATH}" \
    sh -c '
        . "'"${INSTALL_SH}"'"
        ensure_sudo || true
    ' 2>&1 || true
)"
assert_contains "ensure_sudo prompts user on password branch" "${_out}" "Administrator privileges required"

# No sudo at all → hard error (run in a fake empty PATH).
_rc=0
INSTALL_SH_TEST=1 \
PATH="${_tmp}/empty-bin" \
sh -c '
    mkdir -p "'"${_tmp}"'/empty-bin"
    . "'"${INSTALL_SH}"'"
    ensure_sudo
' >/dev/null 2>&1 && _rc=0 || _rc=$?
if [ "${_rc}" -ne 0 ]; then
    PASS=$((PASS + 1))
    printf '  ok  ensure_sudo errors when no sudo available\n'
else
    FAIL=$((FAIL + 1))
    FAILED_NAMES="${FAILED_NAMES} ensure_sudo-no-sudo"
    printf '  FAIL ensure_sudo should have errored\n' >&2
fi

# ---------------------------------------------------------------------------
# 6b. fail-fast: ensure_sudo runs BEFORE any GitHub network call
# ---------------------------------------------------------------------------

echo "# ensure_sudo runs before network call"

# Build a hermetic PATH containing only the basic shell tools `need`
# checks for (`uname tar mkdir rm install awk sed grep`) plus our
# stubbed `id` and `curl` / `wget`. Crucially the real `sudo` is
# absent: ensure_sudo must bail with the no-sudo error BEFORE any
# network call.
_isobin="${_tmp}/iso-bin"
mkdir -p "${_isobin}"
_curlmark="${_tmp}/curl-was-called"
rm -f "${_curlmark}"

# Symlink each required tool from /usr/bin or /bin into the iso PATH.
# `sh` is needed because install.sh runs subshells; ensure_sudo also
# spawns `command` and the script's own helpers. We hunt explicitly
# under /usr/bin and /bin so a user-shell alias for `command -v grep`
# doesn't return a bare relative-name.
for _t in uname tar mkdir rm install awk sed grep cat date mktemp sh head cut sort dirname basename; do
    for _d in /usr/bin /bin; do
        if [ -x "${_d}/$_t" ]; then
            ln -sf "${_d}/$_t" "${_isobin}/$_t"
            break
        fi
    done
done

# Stub `id` so euid reads as 1000 (non-root) but `id -un` falls back to
# the real binary so detect_invoker can still resolve a username.
_real_id="$(command -v id)"
cat > "${_isobin}/id" <<IDEOF
#!/bin/sh
case "\$1" in
    -u) echo 1000 ;;
    *) exec "${_real_id}" "\$@" ;;
esac
IDEOF
chmod +x "${_isobin}/id"

cat > "${_isobin}/curl" <<CURLEOF
#!/bin/sh
echo "called" > "${_curlmark}"
exit 7
CURLEOF
chmod +x "${_isobin}/curl"

cat > "${_isobin}/wget" <<WGETEOF
#!/bin/sh
echo "called" > "${_curlmark}"
exit 7
WGETEOF
chmod +x "${_isobin}/wget"

# Capture stdout+stderr to a temp file so we can read $? from the
# parent shell (assignment inside `$(...)` lives in a subshell and
# does not propagate).
_outfile="${_tmp}/fail-fast.out"
_rc=0
PATH="${_isobin}" \
    sh "${INSTALL_SH}" --target x86_64-unknown-linux-gnu --tag 0.1.0 \
    >"${_outfile}" 2>&1 || _rc=$?
_out="$(cat "${_outfile}")"
if [ "${_rc}" -ne 0 ]; then
    PASS=$((PASS + 1))
    printf '  ok  ensure_sudo fails fast (rc=%s)\n' "${_rc}"
else
    FAIL=$((FAIL + 1))
    FAILED_NAMES="${FAILED_NAMES} ensure_sudo-fail-fast-rc"
    printf '  FAIL ensure_sudo did not fail fast (rc=0)\n' >&2
fi
assert_contains "fail-fast names sudo" "${_out}" "sudo"
if [ -f "${_curlmark}" ]; then
    FAIL=$((FAIL + 1))
    FAILED_NAMES="${FAILED_NAMES} curl-called-before-ensure-sudo"
    printf '  FAIL curl was invoked before ensure_sudo\n' >&2
else
    PASS=$((PASS + 1))
    printf '  ok  curl never invoked before ensure_sudo\n'
fi

rm -rf "${_isobin}"

# ---------------------------------------------------------------------------
# 7. parse_args
# ---------------------------------------------------------------------------

echo "# parse_args"

_out="$(
    INSTALL_SH_TEST=1 sh -c '
        . "'"${INSTALL_SH}"'"
        parse_args --tag 1.2.3 --to /tmp/x --force
        printf "TAG=%s PREFIX=%s FORCE=%s" "${TAG}" "${PREFIX}" "${FORCE}"
    '
)"
assert_contains "parse_args --tag" "${_out}" "TAG=1.2.3"
assert_contains "parse_args --to"  "${_out}" "PREFIX=/tmp/x"
assert_contains "parse_args --force" "${_out}" "FORCE=1"

# Equals-form: --tag=VAL / --to=VAL / --target=VAL.
_out="$(
    INSTALL_SH_TEST=1 sh -c '
        . "'"${INSTALL_SH}"'"
        parse_args --tag=1.2.3 --to=/tmp/x --target=x86_64-unknown-linux-gnu
        printf "TAG=%s PREFIX=%s TARGET=%s" "${TAG}" "${PREFIX}" "${TARGET}"
    '
)"
assert_contains "parse_args --tag=VAL" "${_out}" "TAG=1.2.3"
assert_contains "parse_args --to=VAL"  "${_out}" "PREFIX=/tmp/x"
assert_contains "parse_args --target=VAL" "${_out}" "TARGET=x86_64-unknown-linux-gnu"

# ---------------------------------------------------------------------------
# 7b. parse_args --port-check-only / --verify-host
# ---------------------------------------------------------------------------

echo "# parse_args (port-check)"

# --port-check-only sets PORT_CHECK_ONLY=1 and leaves other flags untouched.
_out="$(
    INSTALL_SH_TEST=1 sh -c '
        . "'"${INSTALL_SH}"'"
        parse_args --port-check-only
        printf "PORT_CHECK_ONLY=%s TAG=[%s] PREFIX=[%s] FORCE=%s VERIFY_HOST=[%s]" \
            "${PORT_CHECK_ONLY}" "${TAG}" "${PREFIX}" "${FORCE}" "${VERIFY_HOST}"
    '
)"
assert_contains "parse_args --port-check-only sets PORT_CHECK_ONLY=1" "${_out}" "PORT_CHECK_ONLY=1"
assert_contains "parse_args --port-check-only leaves TAG empty" "${_out}" "TAG=[]"
assert_contains "parse_args --port-check-only leaves PREFIX empty" "${_out}" "PREFIX=[]"
assert_contains "parse_args --port-check-only leaves FORCE=0" "${_out}" "FORCE=0"
assert_contains "parse_args --port-check-only leaves VERIFY_HOST empty" "${_out}" "VERIFY_HOST=[]"

# Negative case: unrelated flag combos must NOT set PORT_CHECK_ONLY=1.
_out="$(
    INSTALL_SH_TEST=1 sh -c '
        . "'"${INSTALL_SH}"'"
        parse_args --tag 1.2.3 --to /tmp/x
        printf "PORT_CHECK_ONLY=%s" "${PORT_CHECK_ONLY}"
    '
)"
assert_contains "negative: --tag/--to does NOT set PORT_CHECK_ONLY" "${_out}" "PORT_CHECK_ONLY=0"

# --verify-host space form.
_out="$(
    INSTALL_SH_TEST=1 sh -c '
        . "'"${INSTALL_SH}"'"
        parse_args --verify-host https://x.example
        printf "VERIFY_HOST=%s" "${VERIFY_HOST}"
    '
)"
assert_contains "parse_args --verify-host VAL" "${_out}" "VERIFY_HOST=https://x.example"

# --verify-host=VAL equals form.
_out="$(
    INSTALL_SH_TEST=1 sh -c '
        . "'"${INSTALL_SH}"'"
        parse_args --verify-host=https://x.example
        printf "VERIFY_HOST=%s" "${VERIFY_HOST}"
    '
)"
assert_contains "parse_args --verify-host=VAL" "${_out}" "VERIFY_HOST=https://x.example"

# --verify-host with no value → err (non-zero exit).
_rc=0
INSTALL_SH_TEST=1 sh -c '
    . "'"${INSTALL_SH}"'"
    parse_args --verify-host
' >/dev/null 2>&1 || _rc=$?
if [ "${_rc}" -ne 0 ]; then
    PASS=$((PASS + 1))
    printf '  ok  parse_args --verify-host (no value) errors out\n'
else
    FAIL=$((FAIL + 1))
    FAILED_NAMES="${FAILED_NAMES} verify-host-missing-value"
    printf '  FAIL parse_args --verify-host (no value) should have errored\n' >&2
fi

# validate_verify_host rejects non-http(s) schemes (call helper directly).
_rc=0
INSTALL_SH_TEST=1 sh -c '
    . "'"${INSTALL_SH}"'"
    parse_args --verify-host ftp://x
    validate_verify_host
' >/dev/null 2>&1 || _rc=$?
if [ "${_rc}" -ne 0 ]; then
    PASS=$((PASS + 1))
    printf '  ok  validate_verify_host rejects ftp://\n'
else
    FAIL=$((FAIL + 1))
    FAILED_NAMES="${FAILED_NAMES} verify-host-bad-scheme"
    printf '  FAIL validate_verify_host should reject ftp://\n' >&2
fi

# Default DEFAULT_VERIFY_HOST.
_out="$(
    INSTALL_SH_TEST=1 sh -c '
        . "'"${INSTALL_SH}"'"
        printf "DEFAULT_VERIFY_HOST=%s" "${DEFAULT_VERIFY_HOST}"
    '
)"
assert_contains "default verify-host is check.aimx.email" "${_out}" "DEFAULT_VERIFY_HOST=https://check.aimx.email"

# resolve_verify_host: flag wins over env var.
_out="$(
    INSTALL_SH_TEST=1 AIMX_VERIFY_HOST=https://env.example sh -c '
        . "'"${INSTALL_SH}"'"
        parse_args --verify-host https://flag.example
        resolve_verify_host
        printf "VERIFY_HOST=%s" "${VERIFY_HOST}"
    '
)"
assert_contains "flag wins over env var" "${_out}" "VERIFY_HOST=https://flag.example"

# resolve_verify_host: env var wins over default.
_out="$(
    INSTALL_SH_TEST=1 AIMX_VERIFY_HOST=https://env.example sh -c '
        . "'"${INSTALL_SH}"'"
        resolve_verify_host
        printf "VERIFY_HOST=%s" "${VERIFY_HOST}"
    '
)"
assert_contains "env var wins over default" "${_out}" "VERIFY_HOST=https://env.example"

# resolve_verify_host: neither flag nor env → default.
_out="$(
    INSTALL_SH_TEST=1 sh -c '
        unset AIMX_VERIFY_HOST
        . "'"${INSTALL_SH}"'"
        resolve_verify_host
        printf "VERIFY_HOST=%s" "${VERIFY_HOST}"
    '
)"
assert_contains "default applies when nothing set" "${_out}" "VERIFY_HOST=https://check.aimx.email"

# --help text must mention the new flag and env var.
_helpout="$(
    INSTALL_SH_TEST=1 sh -c '
        . "'"${INSTALL_SH}"'"
        help
    '
)"
assert_contains "help mentions --port-check-only" "${_helpout}" "--port-check-only"
assert_contains "help mentions --verify-host" "${_helpout}" "--verify-host"
assert_contains "help mentions AIMX_VERIFY_HOST" "${_helpout}" "AIMX_VERIFY_HOST"

# `install.sh --port-check-only --help` exits 0 and prints help.
_rc=0
_out="$(
    sh "${INSTALL_SH}" --port-check-only --help 2>&1
)" || _rc=$?
assert_zero "--port-check-only --help exits 0" "${_rc}"
assert_contains "--port-check-only --help prints help" "${_out}" "AIMX install script"

# `install.sh --help` mentions port-check-only.
_rc=0
sh "${INSTALL_SH}" --help 2>&1 | grep -q port-check-only || _rc=$?
assert_zero "install.sh --help | grep -q port-check-only" "${_rc}"

# derive_smtp_host extracts host[:port]-stripped host from a verify-host URL.
_out="$(
    INSTALL_SH_TEST=1 sh -c '
        . "'"${INSTALL_SH}"'"
        derive_smtp_host https://check.aimx.email/probe
    '
)"
assert_eq "derive_smtp_host strips scheme + path" "check.aimx.email" "${_out}"

_out="$(
    INSTALL_SH_TEST=1 sh -c '
        . "'"${INSTALL_SH}"'"
        derive_smtp_host https://check.aimx.email:3025
    '
)"
assert_eq "derive_smtp_host strips :port" "check.aimx.email" "${_out}"

# print_port_check_banner exists and prints the connectivity-check title.
_out="$(
    INSTALL_SH_TEST=1 NO_COLOR=1 sh -c '
        . "'"${INSTALL_SH}"'"
        print_port_check_banner
    ' 2>&1
)"
assert_contains "port-check banner shows connectivity title" "${_out}" "AIMX port 25 connectivity check"
assert_contains "port-check banner notes no install" "${_out}" "no install will be performed"

# ---------------------------------------------------------------------------
# 7d. AIMX_VERIFY_HOST gating — env var only validates on port-check path
# ---------------------------------------------------------------------------
#
# Regression for review #6: `validate_verify_host` ran unconditionally in
# main(), so a regular install with AIMX_VERIFY_HOST=garbage exported in
# the operator's shell would fail before any install side effect even
# though the env var only gates the port-check path.

echo "# AIMX_VERIFY_HOST gating"

# Regular install (dry-run) must NOT validate AIMX_VERIFY_HOST.
_rc=0
_out="$(
    AIMX_DRY_RUN=1 \
    AIMX_VERIFY_HOST=garbage \
    AIMX_PREFIX="${_tmp}/bin" \
    PATH="${_tmp}/bin:${PATH}" \
    NO_COLOR=1 \
    sh "${INSTALL_SH}" --target x86_64-unknown-linux-gnu --tag 0.1.0 2>&1
)" || _rc=$?
assert_zero "regular install ignores AIMX_VERIFY_HOST=garbage" "${_rc}"
assert_not_contains "regular install does not run validate_verify_host" "${_out}" "verify-host must start with"

# Port-check path WITH AIMX_VERIFY_HOST=garbage SHOULD fail validation.
_rc=0
_out="$(
    AIMX_VERIFY_HOST=garbage \
    sh "${INSTALL_SH}" --port-check-only 2>&1
)" || _rc=$?
if [ "${_rc}" -ne 0 ]; then
    PASS=$((PASS + 1))
    printf '  ok  --port-check-only validates AIMX_VERIFY_HOST (rc=%s)\n' "${_rc}"
else
    FAIL=$((FAIL + 1))
    FAILED_NAMES="${FAILED_NAMES} port-check-validates-env"
    printf '  FAIL --port-check-only with AIMX_VERIFY_HOST=garbage should fail\n' >&2
fi
assert_contains "--port-check-only error names verify-host scheme" "${_out}" "verify-host must start with"

# ---------------------------------------------------------------------------
# 7e. Listener loop continue-on-timeout (review #2)
# ---------------------------------------------------------------------------
#
# The Python listener heredoc must use `continue` (not `break`) on
# socket.timeout so the deadline still bounds the loop while a second
# probe attempt within the deadline is still served. Source-grep for the
# guard rather than spawn a real listener.

echo "# listener continue-on-timeout"

# Pull lines after `except socket.timeout:` and look for `continue` (not `break`)
# before the next `except` clause.
_timeout_block="$(awk '
    /except socket.timeout:/ {found=1; next}
    found && /except / {found=0}
    found {print}
' "${INSTALL_SH}")"
case "${_timeout_block}" in
    *continue*)
        PASS=$((PASS + 1))
        printf '  ok  listener loop uses continue on socket.timeout\n'
        ;;
    *)
        FAIL=$((FAIL + 1))
        FAILED_NAMES="${FAILED_NAMES} listener-continue-on-timeout"
        printf '  FAIL listener loop should `continue` on socket.timeout, not `break`\n' >&2
        ;;
esac
case "${_timeout_block}" in
    *break*)
        FAIL=$((FAIL + 1))
        FAILED_NAMES="${FAILED_NAMES} listener-no-break-on-timeout"
        printf '  FAIL listener loop still has `break` in socket.timeout handler\n' >&2
        ;;
    *)
        PASS=$((PASS + 1))
        printf '  ok  listener loop has no `break` in socket.timeout handler\n'
        ;;
esac

# ---------------------------------------------------------------------------
# 7f. Listener bind liveness probe (review #1)
# ---------------------------------------------------------------------------
#
# When the temp listener fails to bind (bind() race / EACCES), python
# exits 1 silently. The shell must `kill -0 ${_listener_pid}` after the
# bind delay and surface a distinct error rather than letting /probe
# report a generic "unreachable".

echo "# listener bind liveness"

if grep -q 'kill -0 "\${_listener_pid}"' "${INSTALL_SH}"; then
    PASS=$((PASS + 1))
    printf '  ok  port_check_inbound has kill -0 liveness probe on listener pid\n'
else
    FAIL=$((FAIL + 1))
    FAILED_NAMES="${FAILED_NAMES} listener-liveness-probe"
    printf '  FAIL port_check_inbound missing kill -0 liveness probe on listener pid\n' >&2
fi
if grep -q 'failed to spawn temp listener on :25' "${INSTALL_SH}"; then
    PASS=$((PASS + 1))
    printf '  ok  liveness probe surfaces a distinct bind-failed message\n'
else
    FAIL=$((FAIL + 1))
    FAILED_NAMES="${FAILED_NAMES} listener-bind-msg"
    printf '  FAIL bind-failed listener path missing distinct error message\n' >&2
fi

# ---------------------------------------------------------------------------
# 7f-bis. Listener spawn must NOT block on $() capture pipe
# ---------------------------------------------------------------------------
#
# `$(port_check_listener_start)` invokes the function in a subshell whose
# stdout is a capture pipe. The backgrounded python child inherits that
# pipe; if python's stdout isn't redirected, python keeps the pipe write
# end open for its full ~30s lifetime, the parent's read on $() blocks,
# and the kill -0 liveness check ALWAYS reports "dead". The fix is to
# redirect python's stdout to /dev/null in the listener-start helper.
# This is purely a source-grep — runtime behavior was verified manually.

echo "# listener stdout redirect"

# Find port_check_listener_start body.
_pl_start_body="$(awk '
    $0 ~ /^port_check_listener_start\(\)/ {inside=1}
    inside {print}
    inside && /^}/ {inside=0; exit}
' "${INSTALL_SH}")"

case "${_pl_start_body}" in
    *'python3 - >/dev/null 2>'*)
        PASS=$((PASS + 1))
        printf '  ok  listener python3 stdout redirected to /dev/null (avoids $() capture-pipe block)\n'
        ;;
    *)
        FAIL=$((FAIL + 1))
        FAILED_NAMES="${FAILED_NAMES} listener-stdout-redirect"
        printf '  FAIL listener python3 spawn missing >/dev/null redirect; $() will block on capture pipe\n' >&2
        ;;
esac

case "${_pl_start_body}" in
    *'_PORT_CHECK_LISTENER_STDERR'*)
        PASS=$((PASS + 1))
        printf '  ok  listener python3 stderr captured for diagnostics\n'
        ;;
    *)
        FAIL=$((FAIL + 1))
        FAILED_NAMES="${FAILED_NAMES} listener-stderr-capture"
        printf '  FAIL listener python3 spawn does not capture stderr; bind errors are silent\n' >&2
        ;;
esac

# Bind error message in port_check_inbound must include the captured
# stderr (head -n 1 of _PORT_CHECK_LISTENER_STDERR) so operators see
# the actual cause (e.g. "Address already in use") instead of a
# generic "another binder won the race?".
_pi_body="$(awk '
    $0 ~ /^port_check_inbound\(\)/ {inside=1}
    inside {print}
    inside && /^}/ {inside=0; exit}
' "${INSTALL_SH}")"

case "${_pi_body}" in
    *'head -n 1 "${_PORT_CHECK_LISTENER_STDERR}"'*)
        PASS=$((PASS + 1))
        printf '  ok  bind-failed message surfaces python stderr\n'
        ;;
    *)
        FAIL=$((FAIL + 1))
        FAILED_NAMES="${FAILED_NAMES} bind-error-surfacing"
        printf '  FAIL bind-failed message does not surface python stderr (operators see misleading "another binder won the race?")\n' >&2
        ;;
esac

# Liveness check + cleanup kill must use ${_PORT_CHECK_SUDO} so a non-root
# caller can signal a root-owned (sudo-spawned) python child.
case "${_pi_body}" in
    *'${_PORT_CHECK_SUDO} kill -0'*)
        PASS=$((PASS + 1))
        printf '  ok  kill -0 liveness check uses ${_PORT_CHECK_SUDO} prefix\n'
        ;;
    *)
        FAIL=$((FAIL + 1))
        FAILED_NAMES="${FAILED_NAMES} kill0-sudo-prefix"
        printf '  FAIL kill -0 liveness check missing ${_PORT_CHECK_SUDO} prefix; non-root cannot signal root-owned listener\n' >&2
        ;;
esac

# ---------------------------------------------------------------------------
# 7f-ter. Inbound privilege escalation via sudo
# ---------------------------------------------------------------------------
#
# Non-root operators running `curl ... | sh` should still be able to run
# the inbound check when sudo is available. port_check_ensure_inbound_privilege
# mirrors install.sh's `ensure_sudo` flow: tries `sudo -v`, re-points stdin
# to /dev/tty when stdin is the curl pipe, and sets _PORT_CHECK_SUDO="sudo"
# so the listener spawn / kill / kill-0 carry the prefix. Returns non-zero
# (soft skip) when sudo is unavailable or refused — never `exit`s.

echo "# inbound privilege helper"

# The function exists.
if grep -q '^port_check_ensure_inbound_privilege()' "${INSTALL_SH}"; then
    PASS=$((PASS + 1))
    printf '  ok  port_check_ensure_inbound_privilege defined\n'
else
    FAIL=$((FAIL + 1))
    FAILED_NAMES="${FAILED_NAMES} sudo-helper-missing"
    printf '  FAIL port_check_ensure_inbound_privilege missing\n' >&2
fi

_pe_body="$(awk '
    $0 ~ /^port_check_ensure_inbound_privilege\(\)/ {inside=1}
    inside {print}
    inside && /^}/ {inside=0; exit}
' "${INSTALL_SH}")"

case "${_pe_body}" in
    *'sudo -v'*)
        PASS=$((PASS + 1))
        printf '  ok  inbound privilege helper uses `sudo -v` to validate creds\n'
        ;;
    *)
        FAIL=$((FAIL + 1))
        FAILED_NAMES="${FAILED_NAMES} sudo-helper-no-validate"
        printf '  FAIL inbound privilege helper missing `sudo -v` cred validation\n' >&2
        ;;
esac

case "${_pe_body}" in
    *'/dev/tty'*)
        PASS=$((PASS + 1))
        printf '  ok  inbound privilege helper handles `curl | sh` (stdin-is-pipe)\n'
        ;;
    *)
        FAIL=$((FAIL + 1))
        FAILED_NAMES="${FAILED_NAMES} sudo-helper-no-tty"
        printf '  FAIL inbound privilege helper missing /dev/tty fallback for curl-pipe scenarios\n' >&2
        ;;
esac

# Helper must NEVER call exit — port-check failure is a soft skip.
case "${_pe_body}" in
    *'exit '*)
        FAIL=$((FAIL + 1))
        FAILED_NAMES="${FAILED_NAMES} sudo-helper-no-exit"
        printf '  FAIL inbound privilege helper calls exit; should `return` so the outbound result is preserved\n' >&2
        ;;
    *)
        PASS=$((PASS + 1))
        printf '  ok  inbound privilege helper never calls exit (soft skip on failure)\n'
        ;;
esac

# Inbound orchestrator must use the helper instead of the bare euid check.
case "${_pi_body}" in
    *'port_check_ensure_inbound_privilege'*)
        PASS=$((PASS + 1))
        printf '  ok  port_check_inbound delegates privilege check to the helper\n'
        ;;
    *)
        FAIL=$((FAIL + 1))
        FAILED_NAMES="${FAILED_NAMES} inbound-uses-helper"
        printf '  FAIL port_check_inbound still hard-skips on euid != 0; should call port_check_ensure_inbound_privilege\n' >&2
        ;;
esac

# ---------------------------------------------------------------------------
# 7g. Occupancy warning (review #4)
# ---------------------------------------------------------------------------
#
# When port 25 is held by another process (Postfix/Sendmail/Exim), /probe
# will report success against THAT daemon's banner. The shell must warn
# operators that the green [ok] is only meaningful if the holder is aimx.

echo "# occupancy warning"

if grep -q "port 25 is held by another process" "${INSTALL_SH}"; then
    PASS=$((PASS + 1))
    printf '  ok  port_check_inbound warns when port 25 is occupied\n'
else
    FAIL=$((FAIL + 1))
    FAILED_NAMES="${FAILED_NAMES} occupancy-warning"
    printf '  FAIL port_check_inbound missing occupancy warning\n' >&2
fi

# ---------------------------------------------------------------------------
# 7h. Missing-curl exit code is 2 (review #5)
# ---------------------------------------------------------------------------
#
# port_check_main MUST exit 2 when curl is missing (documented contract:
# 0 pass-or-skip, 1 fail, 2 missing required tool). The previous
# implementation used `need curl`, which exits 1 — wrong for this path.
# Source-grep proves the explicit `return 2` is in place; the previous
# `need curl` line must be gone from port_check_main.

echo "# missing-curl exits 2"

# Find port_check_main body (between its opening and closing brace).
_pc_main_start="$(grep -n '^port_check_main()' "${INSTALL_SH}" | head -n1 | cut -d: -f1)"
if [ -n "${_pc_main_start}" ]; then
    # Everything from port_check_main() until the next top-level `}` line.
    _pc_main_body="$(awk '
        $0 ~ /^port_check_main\(\)/ {inside=1}
        inside {print}
        inside && /^}/ {inside=0}
    ' "${INSTALL_SH}")"
    case "${_pc_main_body}" in
        *"required command not found: curl"*)
            PASS=$((PASS + 1))
            printf '  ok  port_check_main uses explicit curl check\n'
            ;;
        *)
            FAIL=$((FAIL + 1))
            FAILED_NAMES="${FAILED_NAMES} missing-curl-explicit-check"
            printf '  FAIL port_check_main missing explicit curl check\n' >&2
            ;;
    esac
    case "${_pc_main_body}" in
        *"need curl"*)
            FAIL=$((FAIL + 1))
            FAILED_NAMES="${FAILED_NAMES} missing-curl-old-need"
            printf '  FAIL port_check_main still calls `need curl` (exits 1, should exit 2)\n' >&2
            ;;
        *)
            PASS=$((PASS + 1))
            printf '  ok  port_check_main no longer calls `need curl`\n'
            ;;
    esac
else
    FAIL=$((FAIL + 1))
    FAILED_NAMES="${FAILED_NAMES} port-check-main-missing"
    printf '  FAIL could not locate port_check_main() in install.sh\n' >&2
fi

# Live test: with curl scrubbed from PATH, --port-check-only must exit 2.
_isobin2="${_tmp}/iso-bin-no-curl"
mkdir -p "${_isobin2}"
for _t in uname tar mkdir rm install awk sed grep cat date mktemp sh head cut sort dirname basename id; do
    for _d in /usr/bin /bin; do
        if [ -x "${_d}/$_t" ]; then
            ln -sf "${_d}/$_t" "${_isobin2}/$_t"
            break
        fi
    done
done

_rc=0
_out="$(
    PATH="${_isobin2}" \
        sh "${INSTALL_SH}" --port-check-only 2>&1
)" || _rc=$?
assert_eq "--port-check-only without curl exits 2" "2" "${_rc}"
assert_contains "--port-check-only without curl names curl" "${_out}" "curl"

rm -rf "${_isobin2}"

# ---------------------------------------------------------------------------
# 7c. SUDO prefix resolution — root-without-sudo path must succeed
# ---------------------------------------------------------------------------

echo "# SUDO prefix (root without sudo)"

# Tear down the prior stubs; rebuild a PATH that has NO sudo binary.
rm -f "${_tmp}/bin/sudo"

# Re-create a config file to back up.
mkdir -p "${_tmp}/etc/aimx"
printf 'domain = "example.com"\n' > "${_tmp}/etc/aimx/config.toml"

# Stub `id` so euid always reads 0 (simulates running as root).
_real_id="$(command -v id)"
cat > "${_tmp}/bin/id" <<IDEOF
#!/bin/sh
case "\$1" in
    -u) echo 0 ;;
    *) exec "${_real_id}" "\$@" ;;
esac
IDEOF
chmod +x "${_tmp}/bin/id"

# `id -u` stubbed to 0; sudo is absent from PATH. resolve_sudo_prefix must
# set SUDO="" and all call sites must succeed without invoking sudo.
_rc=0
_out="$(
    INSTALL_SH_TEST=1 \
    AIMX_INSTALL_CONFIG_PATH="${_tmp}/etc/aimx/config.toml" \
    PATH="${_tmp}/bin:/usr/bin:/bin" \
    sh -c '
        . "'"${INSTALL_SH}"'"
        resolve_sudo_prefix
        # Non-colon form: distinguish empty-string from truly-unset.
        printf "SUDO=[%s]\n" "${SUDO-UNSET}"
        backup_existing_config
    ' 2>&1
)" || _rc=$?
assert_zero "backup_existing_config succeeds with empty SUDO (root, no sudo)" "${_rc}"
assert_contains "SUDO is empty on root" "${_out}" "SUDO=[]"
# The backup file should exist even though sudo was never called.
_bak="$(ls "${_tmp}/etc/aimx/" 2>/dev/null | grep '^config.toml.bak-' || true)"
assert_contains "backup created via empty SUDO" "${_bak}" "config.toml.bak-"

# Clean up for later sections.
rm -f "${_tmp}/etc/aimx/"*
rm -f "${_tmp}/bin/id"

# Rebuild passthrough sudo for remaining tests.
cat > "${_tmp}/bin/sudo" <<'SUDOEOF'
#!/bin/sh
while [ $# -gt 0 ]; do
    case "$1" in
        -n|-v|-S|-H|-i|-p|--preserve-env|--non-interactive) shift ;;
        -u) shift 2 ;;
        -*) shift ;;
        --) shift; break ;;
        *) break ;;
    esac
done
exec "$@"
SUDOEOF
chmod +x "${_tmp}/bin/sudo"

# ---------------------------------------------------------------------------
# 8. AIMX_DRY_RUN smoke — thin install banner, no checklist (binary owns it)
# ---------------------------------------------------------------------------

echo "# dry-run smoke"

# Point PATH at our sudo stub so nothing real is ever invoked.
_out="$(
    AIMX_DRY_RUN=1 \
    AIMX_PREFIX="${_tmp}/bin" \
    PATH="${_tmp}/bin:${PATH}" \
    NO_COLOR=1 \
    sh "${INSTALL_SH}" --target x86_64-unknown-linux-gnu --tag 0.1.0 2>&1
)"
assert_contains "dry-run prints thin install banner" "${_out}" "AIMX installer"
assert_contains "dry-run names binary handoff" "${_out}" "aimx setup"
assert_contains "dry-run notes no FS changes" "${_out}" "no filesystem changes"
# The shell must NOT print the six checklist titles itself anymore — those
# are the binary's job. The thin banner is just two lines.
assert_not_contains "shell does not print checklist" "${_out}" "Preflight checks on port 25"
assert_not_contains "shell does not print step 6 title" "${_out}" "Set up MCP for agent"

# Dry-run must NOT run a port check (no port-check banner emitted).
assert_not_contains "dry-run does not switch to port-check banner" "${_out}" "AIMX port 25 connectivity check"
assert_not_contains "dry-run does not show outbound check line" "${_out}" "Outbound port 25"

# ---------------------------------------------------------------------------
# 9. prompt_reinstall — equal-version re-run flow
# ---------------------------------------------------------------------------
#
# When `install.sh` finds the binary already at the target tag and --force
# was not passed, it asks the operator whether to re-run `aimx setup`.
# The helper is structured around an overridable `_prompt_read` so tests
# can stub the answer without faking /dev/tty.

echo "# prompt_reinstall"

_out="$(
    INSTALL_SH_TEST=1 NO_COLOR=1 sh -c '
        . "'"${INSTALL_SH}"'"
        _prompt_read() { _ans="y"; }
        if prompt_reinstall; then echo YES; else echo NO; fi
    ' 2>&1
)"
assert_contains "prompt_reinstall returns 0 on y" "${_out}" "YES"
assert_contains "prompt_reinstall prints the prompt to stderr" "${_out}" "AIMX is already installed"

_out="$(
    INSTALL_SH_TEST=1 NO_COLOR=1 sh -c '
        . "'"${INSTALL_SH}"'"
        _prompt_read() { _ans="Yes"; }
        if prompt_reinstall; then echo YES; else echo NO; fi
    ' 2>&1
)"
assert_contains "prompt_reinstall accepts Yes" "${_out}" "YES"

_out="$(
    INSTALL_SH_TEST=1 NO_COLOR=1 sh -c '
        . "'"${INSTALL_SH}"'"
        _prompt_read() { _ans="n"; }
        if prompt_reinstall; then echo YES; else echo NO; fi
    ' 2>&1
)"
assert_contains "prompt_reinstall returns 1 on n" "${_out}" "NO"

_out="$(
    INSTALL_SH_TEST=1 NO_COLOR=1 sh -c '
        . "'"${INSTALL_SH}"'"
        _prompt_read() { _ans=""; }
        if prompt_reinstall; then echo YES; else echo NO; fi
    ' 2>&1
)"
assert_contains "prompt_reinstall defaults to no on empty input" "${_out}" "NO"

_out="$(
    INSTALL_SH_TEST=1 NO_COLOR=1 sh -c '
        . "'"${INSTALL_SH}"'"
        _prompt_read() { return 1; }
        if prompt_reinstall; then echo YES; else echo NO; fi
    ' 2>&1
)"
assert_contains "prompt_reinstall returns 1 when no TTY" "${_out}" "NO"

# ---------------------------------------------------------------------------
# 10. AIMX branding sweep — no lowercase brand-as-noun in install.sh prose
# ---------------------------------------------------------------------------
#
# Lowercase `aimx` is fine for command literals (`aimx setup`), service
# unit names (`aimx.service`), paths (`/etc/aimx/...`), and the GitHub
# repo identifier (`uzyn/aimx`). It is NOT fine when the brand is the
# subject/object of an English sentence in operator-facing strings.
# Catch the canonical regressions: "installing aimx", "aimx is", and
# "aimx <ver> installed/upgraded".

echo "# branding"

_brand_hits="$(grep -nE '(installing|installed|upgraded) aimx\b' "${INSTALL_SH}" || true)"
assert_eq "no 'installing/installed/upgraded aimx' in install.sh" "" "${_brand_hits}"

_brand_hits="$(grep -nE '\baimx is\b' "${INSTALL_SH}" || true)"
assert_eq "no 'aimx is' prose in install.sh" "" "${_brand_hits}"

# `aimx <SEMVER>` prose ("aimx 0.0.7 is already installed"). Allow path
# fragments like `aimx-0.0.7-...` (the hyphen separates them) and shell
# redirects like `aimx 2>/dev/null` (the `>` does). The regex requires
# the digit-dot-digit shape that real version tokens always have.
_brand_hits="$(grep -nE 'aimx [0-9]+\.[0-9]' "${INSTALL_SH}" || true)"
assert_eq "no 'aimx <semver>' prose in install.sh" "" "${_brand_hits}"

# ---------------------------------------------------------------------------
# Report
# ---------------------------------------------------------------------------

echo
TOTAL=$((PASS + FAIL))
if [ "${FAIL}" -eq 0 ]; then
    printf '%s/%s tests passed\n' "${PASS}" "${TOTAL}"
    exit 0
else
    printf '%s/%s tests passed — FAIL:%s\n' "${PASS}" "${TOTAL}" "${FAILED_NAMES}" >&2
    exit 1
fi
