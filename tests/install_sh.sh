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
