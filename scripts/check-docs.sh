#!/usr/bin/env bash
# Lightweight shell-level doc lint. Parses `aimx --help` subcommand
# names and rejects command-line references in `book/` to
# `aimx <verb>` where `<verb>` is not a valid CLI verb. Catches the
# class of doc drift that reviewed on PR #141 (`aimx config trust add`).
#
# Scope (deliberately narrow, to minimise false positives on prose):
#   - Only lines inside fenced ```...``` code blocks are scanned.
#   - Only the first `aimx <verb>` token on each such line is
#     examined, after stripping an optional leading `$ ` / `# `
#     shell prompt and an optional leading `sudo `.
#   - Lines that are obviously not `aimx` invocations (e.g.
#     `systemctl start aimx`, `curl ... | sh`) are ignored.
#
# Exit 0: clean. Exit 1: drift detected.

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
BOOK_DIR="${ROOT_DIR}/book"

if [ ! -d "${BOOK_DIR}" ]; then
    echo "check-docs: book/ directory not found at ${BOOK_DIR}" >&2
    exit 1
fi

# Resolve the help source. Prefer an already-built binary; fall back
# to `cargo run --quiet`. CI builds the binary earlier in the job.
if [ -x "${ROOT_DIR}/target/release/aimx" ]; then
    HELP_CMD=("${ROOT_DIR}/target/release/aimx" --help)
elif [ -x "${ROOT_DIR}/target/debug/aimx" ]; then
    HELP_CMD=("${ROOT_DIR}/target/debug/aimx" --help)
elif command -v aimx >/dev/null 2>&1; then
    HELP_CMD=(aimx --help)
else
    HELP_CMD=(cargo run --quiet --manifest-path "${ROOT_DIR}/Cargo.toml" -- --help)
fi

HELP_OUT="$("${HELP_CMD[@]}" 2>/dev/null)"

VERBS="$(
    printf '%s\n' "${HELP_OUT}" \
        | awk '/^Commands:/ {flag=1; next} /^[A-Za-z]/ && flag {flag=0} flag' \
        | awk '{print $1}' \
        | grep -E '^[a-z][a-z0-9-]*$' \
        | sort -u
)"

if [ -z "${VERBS}" ]; then
    echo "check-docs: could not extract subcommand verbs from \`aimx --help\`" >&2
    printf '%s\n' "${HELP_OUT}" >&2
    exit 1
fi

# Tokens that legitimately follow `aimx ` in command-line examples
# even though they are not subcommand verbs. Kept narrow — every
# entry is a conscious allowlist decision.
ALLOWED_NON_VERBS=(
    # Top-level flags.
    --help
    --version
    --data-dir
    -h
    -V
    # Documented clap aliases (src/cli.rs). Keep in sync with
    # `#[command(... alias = "...")]` attributes.
    hook
    mailbox
    # Words that appear after `aimx ` inside quoted-output examples
    # rather than as subcommand invocations (e.g. the success banner
    # `aimx is running for <domain>.` and the upgrade banner
    # `aimx v<old> → v<new>. Service restarted.`).
    is
    v
)

# Walk each book/*.md. Emit command-like `aimx <verb>` tokens from:
#   1. Fenced code blocks — every line, strip prompt/sudo prefixes.
#   2. Inline backtick spans whose content starts with `aimx ` or
#      `sudo aimx ` (command-like spans only; not arbitrary
#      backticked prose that happens to contain "aimx").
# shellcheck disable=SC2016  # single-quoted: this is an awk program, not shell
EXTRACT_SCRIPT='
function emit_from_line(line,    tok) {
    sub(/^[ \t]+/, "", line)
    sub(/^\$[ \t]+/, "", line)
    sub(/^#[ \t]+/, "", line)
    sub(/^sudo[ \t]+/, "", line)
    while (match(line, /^[A-Z_][A-Z0-9_]*=[^ \t]*[ \t]+/)) {
        line = substr(line, RLENGTH + 1)
    }
    if (match(line, /^aimx[ \t]+[-A-Za-z][-A-Za-z0-9]*/) == 0) return
    tok = substr(line, 1, RLENGTH)
    sub(/^aimx[ \t]+/, "", tok)
    print tok
}
BEGIN { in_fence = 0 }
/^[ \t]*```/ { in_fence = !in_fence; next }
in_fence {
    emit_from_line($0)
    next
}
{
    # Outside a fence: parse inline backtick spans.
    line = $0
    while (1) {
        i = index(line, "`")
        if (i == 0) break
        rest = substr(line, i + 1)
        j = index(rest, "`")
        if (j == 0) break
        span = substr(rest, 1, j - 1)
        # Only consider command-like spans.
        if (span ~ /^(sudo[ \t]+)?aimx[ \t]+[-A-Za-z]/) {
            emit_from_line(span)
        }
        line = substr(rest, j + 1)
    }
}
'

EXTRACTED="$(
    for f in "${BOOK_DIR}"/*.md; do
        awk "${EXTRACT_SCRIPT}" "$f"
    done | sort -u
)"

FAIL=0
DRIFT=""

while IFS= read -r word; do
    [ -z "${word}" ] && continue
    # shellcheck disable=SC2086  # word-split VERBS into one-per-line intentionally
    if printf '%s\n' ${VERBS} | grep -Fxq -- "${word}"; then
        continue
    fi
    if printf '%s\n' "${ALLOWED_NON_VERBS[@]}" | grep -Fxq -- "${word}"; then
        continue
    fi
    DRIFT="${DRIFT}${word}"$'\n'
    FAIL=1
done <<< "${EXTRACTED}"

if [ "${FAIL}" -eq 1 ]; then
    echo "check-docs: book/ has command-line \`aimx <verb>\` references where <verb> is not in \`aimx --help\`:"
    printf '%s' "${DRIFT}" | sed 's/^/  - aimx /'
    echo
    echo "Known verbs (from \`aimx --help\`):"
    # shellcheck disable=SC2086  # word-split VERBS into one-per-line intentionally
    printf '%s\n' ${VERBS} | sed 's/^/  /'
    echo
    echo "If the reference is intentional (new clap alias, new flag), add it to ALLOWED_NON_VERBS in scripts/check-docs.sh."
    exit 1
fi

echo "check-docs: ok (no unknown \`aimx <verb>\` command lines in book/)"
