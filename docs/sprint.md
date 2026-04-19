# AIMX — Sprint Plan

**Sprint cadence:** 2.5 days per sprint
**Team:** Solo developer with heavy AI augmentation (Claude Code)
**Total sprints:** 51 (6 original + 2 post-audit hardening + 1 YAML→TOML migration + 2 verifier/setup overhaul + 2 post-Sprint-11 bug fixes + 2 verifier ops + 1 deployment + 1 service rename + 1 setup UX + 5 embedded SMTP + 1 verify cleanup + 1 DKIM permissions fix + 1 IPv6 support + 1 systemd unit hardening + 1 CLI color consistency + 1 CI binary releases + 3 agent integration + 1 channel-trigger cookbook + 1 non-blocking cleanup + 1 scope-reversal (33.1) + 8 v0.2 pre-launch reshape + 1 post-v0.2 backlog cleanup + 1 CLI UX fixes + 1 pre-launch README + hardening sweep + 4 post-launch hardening + 1 post-v1 cleanup + 4 post-v1 DX/hooks work)
**Timeline:** ~143 calendar days (v1: ~92 days, v0.2 reshape: ~22.5 days, post-v0.2 cleanup: ~2.5 days, CLI UX fixes: ~2.5 days, pre-launch sweep: ~2.5 days, post-launch hardening + cleanup: ~10 days, post-v1 DX/hooks work: ~10 days through Day 143)
**v1 Scope:** Full PRD scope including verifier service. Sprint 1 targets earliest possible idea validation on a real VPS. Sprints 7–8 address findings from post-v1 code review audit. Sprints 10–11 overhaul the verifier service (remove email echo, add EHLO probe) and rewrite the setup flow (root check, MTA conflict detection, install-before-check). Sprints 12–13 fix critical bugs found during post-Sprint-11 debugging: Caddy self-probe loop / XFF SSRF risk in the verifier service, and the preflight chicken-and-egg problem on fresh VPSes. Sprints 14–15 are review-driven operational quality work on the verifier service (request logging, Docker packaging). Sprint 17 renames the verify service to verifier across all code, Docker, CI, and documentation. Sprints 19–23 replace OpenSMTPD with an embedded SMTP server (hand-rolled tokio listener for inbound, lettre + hickory-resolver for outbound) and update all documentation, making aimx a true single-binary solution with no external runtime dependencies and cross-platform Unix support. Sprint 24 cleans up `aimx verify` (EHLO-only checks, sudo requirement, remove `/reach` endpoint, AIMX capitalization). Sprint 27 hardens the generated systemd unit with restart rate-limiting, resource limits, and network-readiness dependencies. Sprint 27.5 unifies user-facing CLI output under a single semantic color palette. (Sprint 27.6 — CI binary release workflow — is deferred to the Non-blocking Review Backlog until we're production-ready.) Sprints 28–30 ship per-agent integration packages (Claude Code, Codex CLI, OpenCode, Gemini CLI, Goose, OpenClaw) plus the `aimx agent-setup <agent>` installer that drops a plugin/skill/recipe into the agent's standard location without mutating its primary config. Sprint 31 adds a channel-trigger cookbook covering email→agent invocation patterns for every supported agent. Sprint 32 is a non-blocking cleanup sprint consolidating review feedback across v1.

**v0.2 Scope (pre-launch reshape, Sprints 33–40 + 33.1 scope-reversal):** Five tightly-coupled themes that reshape AIMX into its launch form. Sprint 33 splits the filesystem (config + DKIM secrets to `/etc/aimx/`, data stays at `/var/lib/aimx/` but world-readable). Sprint 33.1 (scope reversal, inserted after Sprint 33 merged) drops PTR/reverse-DNS handling (operator responsibility, out of aimx scope) and drops the `aimx` system group introduced in S33-4 — authorization on the UDS send socket is explicitly out of scope for v0.2 and the socket becomes world-writable (`0o666`). Sprints 34–35 shrink the trust boundary: DKIM signing and outbound delivery move inside `aimx serve`, exposed to clients over a world-writable Unix domain socket at `/run/aimx/send.sock`; the DKIM private key becomes root-only (`600`) and is never read by non-root processes. Sprint 36 reshapes the datadir (`inbox/` vs `sent/` split per mailbox, `YYYY-MM-DD-HHMMSS-<slug>.md` filenames with a deterministic slug algorithm, Zola-style attachment bundles). Sprint 37 expands the inbound frontmatter schema (new fields: `thread_id`, `received_at`, `received_from_ip`, `size_bytes`, `delivered_to`, `list_id`, `auto_submitted`, `dmarc`, `labels`) and adds DMARC verification. Sprint 38 surfaces the per-mailbox trust evaluation as a new always-written `trusted` frontmatter field (the v1 per-mailbox trust model — `trust: none|verified` + `trusted_senders` — is preserved unchanged; `trusted` is the *result*, not a new *policy*) and persists sent mail with a full outbound block. Sprint 39 restructures the shared agent primer into a progressive-disclosure skill bundle (`agents/common/aimx-primer.md` + `references/`), standardizes author metadata to `U-Zyn Chua <chua@uzyn.com>`, and reverses an earlier draft's storage-layout redaction policy. Sprint 40 ships the baked-in `/var/lib/aimx/README.md` agent-facing layout guide (versioned via `include_str!`, refreshed on `aimx serve` startup when the version differs), replaces stale `/var/log/aimx.log` references with `journalctl -u aimx`, and brings every affected `book/` chapter and `CLAUDE.md` up to date. No migration tooling is written — v0.2 ships pre-launch, with no existing installs to upgrade.

---


## Sprint Archive

Completed sprints 1–41 have been archived for context window efficiency.

| Archive | Sprints | File |
|---------|---------|------|
| 1 | 1–8 | [`sprint.1.md`](sprint.1.md) |
| 2 | 9–21 | [`sprint.2.md`](sprint.2.md) |
| 3 | 22–30 | [`sprint.3.md`](sprint.3.md) |
| 4 | 31–41 | [`sprint.4.md`](sprint.4.md) |

---

## Sprint 42 — CLI UX: Config Error Messages + Setup Port-Check Race + Version Hash (Days 118–120.5) [DONE]

**Goal:** Fix P0 UX issues that block first-time setup and improve build traceability: (1) commands that require config give a cryptic "os error 2" instead of pointing the user to `aimx setup`, (2) `aimx setup` fails the inbound port 25 check because it races against `aimx serve` startup, and (3) `aimx --version` includes the git commit hash so pre-release builds are distinguishable.

**Dependencies:** Sprint 41 (all prior work complete)

#### S42-1: Helpful error message when config file is missing

**Context:** Running `aimx status` (or any config-dependent command: `mcp`, `send`, `mailbox`, `serve`) on a fresh VPS before `aimx setup` produces `Error: No such file or directory (os error 2)` — the raw ENOENT from trying to open `/etc/aimx/config.toml`. Users can't tell what's missing or what to do next. The fix should catch the "config not found" case in the config loading path and produce a message like: `Config file not found at /etc/aimx/config.toml — run 'sudo aimx setup' first`. This should cover all subcommands that load config (status, mcp, send, mailbox, serve, agent-setup).

**Priority:** P0

- [x] `config::load()` (or the call site in `main.rs`) catches `io::ErrorKind::NotFound` on the config file and returns a clear error naming the expected path and suggesting `sudo aimx setup`
- [x] Error message includes the actual path attempted (respects `AIMX_CONFIG_DIR` override)
- [x] All config-dependent subcommands benefit from the fix (status, mcp, send, mailbox, serve, agent-setup, dkim-keygen) — no raw "os error 2" leaks to the user
- [x] Unit test: calling config load with a nonexistent path produces the expected error message, not a raw IO error
- [x] `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt -- --check` clean

#### S42-2: Wait-for-ready loop in `aimx setup` before port checks

**Context:** After `install_service_file()` calls `restart_service("aimx")`, setup immediately runs the outbound + inbound port 25 checks. `restart_service()` returns as soon as `systemctl restart aimx` exits — not when `aimx serve` has finished binding port 25. The outbound check (local → remote verifier) usually passes because it doesn't need the local listener. The inbound check (remote verifier → local port 25 EHLO) fails because `aimx serve` hasn't bound yet. Standalone `aimx verify` doesn't have this problem because it either detects an already-running daemon or spawns its own listener and waits for readiness. Fix: after restarting the service and before running port checks, poll for `aimx serve` readiness — e.g., attempt a TCP connect to `127.0.0.1:25` in a retry loop (up to ~5 seconds, ~500ms between attempts). If the loop times out, proceed with the checks anyway (they'll fail with the existing error message, which is still accurate).

**Priority:** P0

- [x] After `restart_service("aimx")` returns, a wait-for-ready loop polls `127.0.0.1:25` (TCP connect) with ~500ms interval, up to ~5s total
- [x] Loop exits early as soon as a connection succeeds (port is bound)
- [x] If the loop times out (service didn't bind within 5s), setup proceeds to the port checks without error — the existing "Inbound port 25... FAIL" message covers this case
- [x] The wait loop is behind the `SystemOps` trait (or `NetworkOps`) so tests can mock it without real sleeps
- [x] Existing setup tests still pass; new test verifies that setup proceeds after the wait loop succeeds
- [x] `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt -- --check` clean

#### S42-3: Include git commit hash in `aimx --version` output

**Context:** Pre-release, `aimx --version` prints only `aimx 0.1.0` (from `Cargo.toml`). When testing builds on a VPS it's impossible to tell which commit the binary was built from. Add a `build.rs` that captures the 8-character short git hash at compile time and bakes it into the version string so `aimx --version` prints e.g. `aimx 0.1.0 (abcd1234)`. If the build happens outside a git repo (e.g. `cargo install` from a tarball), fall back gracefully to just the version number without a hash.

**Priority:** P1

- [x] New `build.rs` at the repo root runs `git rev-parse --short=8 HEAD` and sets a `GIT_HASH` env var via `cargo:rustc-env`
- [x] If `git` is unavailable or the working directory isn't a repo, `GIT_HASH` is set to `"unknown"` (no build failure)
- [x] `cli.rs` composes the clap version string as `format!("{} ({})", env!("CARGO_PKG_VERSION"), env!("GIT_HASH"))` — output: `aimx 0.1.0 (abcd1234)`
- [x] When `GIT_HASH` is `"unknown"`, version string omits the parenthetical — output: `aimx 0.1.0`
- [x] `build.rs` emits `cargo:rerun-if-changed=.git/HEAD` and `cargo:rerun-if-changed=.git/refs` so the hash updates on new commits without full rebuilds
- [x] `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt -- --check` clean

---

## Sprint 43 — Pre-launch README Sweep + Hardening (Days 120.5–123) [DONE]

**Goal:** Bring `README.md` up to date with the v0.2 reshape (Sprints 33–40) before public release; fix correctness and UX gaps surfaced by external review: `aimx status` OpenRC support, HTML-body size cap, `Received:` IP parser, transport error classification, attachment-filename safety, and `dkim-keygen` permission errors.

**Dependencies:** Sprint 42 (all v0.2 + post-v0.2 work complete).

#### S43-1: README.md pre-launch sweep

**Context:** The README has multiple stale sections from before the v0.2 reshape. (a) Storage layout (266–281) shows `/var/lib/aimx/config.toml`, `/var/lib/aimx/dkim/`, flat `catchall/` with shared `attachments/` — actual layout is config + DKIM at `/etc/aimx/` (private `0600`, public `0644`) and datadir split into `inbox/<mailbox>/` + `sent/<mailbox>/` with Zola-style per-email bundles. (b) Configuration section (188–190) says config lives in the data directory; it's at `/etc/aimx/`. `AIMX_CONFIG_DIR` is never mentioned. (c) Email format example (287–305) uses the pre-Sprint-37 flat schema, missing `thread_id`, `received_at`, `received_from_ip`, `delivered_to`, `size_bytes`, `list_id`, `auto_submitted`, `dmarc`, `trusted`, `labels`. (d) Trust policy section (255–264) doesn't mention the `trusted` frontmatter field from Sprint 38. This is a top-to-bottom sweep, not just the four identified sections.

**Priority:** P0

- [x] Storage layout rewritten for `/etc/aimx/{config.toml,dkim/}` + `/var/lib/aimx/{inbox,sent}/<mailbox>/`, with a Zola bundle example and permission notes (DKIM private `0600` root-only, public `0644`, datadir world-readable by design)
- [x] Configuration section: `/etc/aimx/config.toml` is canonical; documents `AIMX_CONFIG_DIR` override (for tests / non-standard installs) separately from `--data-dir` / `AIMX_DATA_DIR`
- [x] Email format example rewritten with all current inbound fields in the `frontmatter.rs` section order; includes a short outbound-block example or pointer to `book/mailboxes.md`
- [x] Trust policy section mentions the `trusted: "none" | "true" | "false"` frontmatter surface alongside per-mailbox `trust` + `trusted_senders`
- [x] DKIM key management section notes keys live at `/etc/aimx/dkim/` and `aimx dkim-keygen` requires root (or `AIMX_CONFIG_DIR` for dev)
- [x] Top-to-bottom pass against `book/` + `CLAUDE.md` + actual code — every other drift (MCP tool list, send examples, channel variables, DNS records) verified or corrected
- [x] Repo-wide grep for `/var/lib/aimx/<mailbox>/` bare (without `inbox/`/`sent/`) returns zero hits in `README.md`
- [x] `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt -- --check` clean

#### S43-2: `aimx status` uses `SystemOps::is_service_running`

**Context:** `status.rs:125-130` hardcodes `Command::new("systemctl").args(["is-active", "--quiet", "aimx"])`. On Alpine/Fedora/Gentoo OpenRC hosts — which `book/setup.md` claims are supported — this always reports the daemon as "not running" because `systemctl` is absent or behaves differently. The codebase already has a `SystemOps::is_service_running` abstraction (used by `setup.rs`) that handles systemd vs OpenRC. Reuse it.

**Priority:** P1

- [x] `status.rs` replaces the hardcoded `systemctl` invocation with `SystemOps::is_service_running("aimx")`
- [x] `status::run` instantiates a `RealSystemOps` at the call site (or accepts it as a parameter) — whichever matches the codebase's existing pattern
- [x] Test mocks `SystemOps::is_service_running` returning `true` and `false`, asserts `status` output accordingly
- [x] Manual verification note in the test file or PR description that status now works on an OpenRC host
- [x] `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt -- --check` clean

#### S43-3: HTML body size cap before `html2text::from_read`

**Context:** `src/ingest.rs:482-483` calls `html2text::from_read(html.as_bytes(), 80)` on the HTML part with no size guard. SMTP `max_message_size = 25 MB` bounds raw DATA (so the "100 MB DoS" framing in the review is wrong), but 25 MB of pathological HTML can still consume significant CPU in `html2text`. Cap the input at a safe bound (~2 MB) and truncate with a visible marker in the rendered body when exceeded. 2 MB is far above realistic HTML email (typical marketing HTML < 500 KB) so legitimate messages are unaffected.

**Priority:** P1

- [x] `ingest.rs` defines `const HTML_CONVERSION_CAP: usize = 2 * 1024 * 1024;`
- [x] When HTML length exceeds the cap, only the first `HTML_CONVERSION_CAP` bytes are passed to `html2text`; the rendered body appends a marker like `\n\n[...HTML body truncated at 2 MB for rendering...]`
- [x] Within-cap messages behave identically to today
- [x] Unit test: under-cap → full conversion; over-cap → truncated with marker; empty HTML → empty string (unchanged)
- [x] `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt -- --check` clean

#### S43-4: `parse_ip_from_received` trusts only bracketed forms

**Context:** `src/ingest.rs:429-452` first scans for bracketed-form IPs (`[1.2.3.4]`) — the RFC 5321 canonical marker for the connecting client — but then falls back to a whitespace-split scan that accepts any token that parses as an IP. That fallback happily picks up IPs embedded in comments or HELO strings (e.g. `Received: from evil.example.com (HELO mail.legit[1.2.3.4])` — the fallback will return `1.2.3.4` even when no true bracketed form exists). The frontmatter `received_from_ip` field then carries an attacker-controlled value. Drop the fallback.

**Priority:** P2

- [x] `parse_ip_from_received` returns `None` when no bracketed non-loopback IP is found (word-by-word fallback removed)
- [x] Existing tests relying on the fallback updated or removed
- [x] New test: `Received:` header with IP only in a free-text comment (no brackets) returns `None`
- [x] Behavior spot-checked against at least three real `Received:` header shapes from ingest fixtures
- [x] `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt -- --check` clean

#### S43-5: `LettreTransport` error classification via lettre `Error` methods

**Context:** Sprint 41 (S41-4) typed the error *surface* — `MailTransport::send` returns `Result<_, TransportError>` — but `src/transport.rs:257-266` still classifies errors via `msg.contains("Connection refused")` / `msg.contains("timed out")` on the lettre error's `Display` string. Substring matching is brittle across lettre upgrades. Lettre's `smtp::Error` exposes structured classification (`is_transient()`, `is_permanent()`, `is_timeout()`, etc.). Use those.

**Priority:** P2

- [x] `LettreTransport::send` classifies via `lettre::transport::smtp::Error` accessor methods, not `msg.contains(...)`
- [x] Short inline comment documents which lettre `Error` shapes map to `TransportError::Temp` vs `TransportError::Permanent`
- [x] Existing send-handler tests still pass; behavior preserved (same variant for same scenario)
- [x] If lettre's API allows constructing `Error` values in tests, add a test per branch; otherwise rely on existing end-to-end coverage with a note in the PR
- [x] `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt -- --check` clean

#### S43-6: `aimx dkim-keygen` helpful permission-denied message

**Context:** `aimx dkim-keygen` without root on a default install writes to `/etc/aimx/dkim/` (via `config::dkim_dir()`), which fails with the raw `io::Error`: `Error: Permission denied (os error 13)`. No hint about `sudo` or the `AIMX_CONFIG_DIR` override (which is how tests and dev loops legitimately run dkim-keygen against a tempdir without root). Catch `ErrorKind::PermissionDenied` in `dkim::run_keygen` / `generate_keypair` / `write_file_with_mode` and wrap with a message naming the directory and suggesting `sudo` or `AIMX_CONFIG_DIR`. Do NOT add a hard root check — that would break the override path.

**Priority:** P2

- [x] `io::ErrorKind::PermissionDenied` from the dkim write path is wrapped with a clear message naming the target directory and suggesting `sudo aimx dkim-keygen` or `AIMX_CONFIG_DIR=<path> aimx dkim-keygen`
- [x] Other IO errors (disk full, etc.) surface their native message unmodified
- [x] Test: set `AIMX_CONFIG_DIR` to a read-only tempdir, run `aimx dkim-keygen`, assert error text mentions both the attempted path and either `sudo` or `AIMX_CONFIG_DIR`
- [x] `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt -- --check` clean

#### S43-7: Attachment filename safety + subject/filename NFC normalization

**Context:** Two related hardening items in the ingest path against malicious inbound email. (a) Attachment filenames from `mail-parser` flow into filesystem paths at `ingest.rs:504-512`. `Path::file_name()` already strips directory components and rejects `.` / `..`, so direct path-traversal is blocked. But filenames can still contain: control characters (`\0`, `\r`, `\n`, C0, DEL); bidi overrides and zero-width joiners (confuse agents and humans); leading `-` (interpreted as flags by naive downstream CLI tools); NFC/NFD-collision Unicode (two visually identical names differing in composition); pathological lengths (filesystem ENAMETOOLONG). Channel-trigger `{filepath}` templates already `shell_escape_value` every substitution (`channel.rs:13-16`), so the primary RCE vector is closed — but attachment filenames also flow into the `attachments = [...]` frontmatter field, which agents may shell out to. Defense in depth. (b) Slug generation in `slug.rs:28-53` does not NFC-normalize the subject before slugging, so two subjects looking identical but differing in Unicode composition yield different slugs / filenames.

Fix: `sanitize_attachment_filename(raw: &str, index: usize) -> String` — NFC-normalize, strip control chars + DEL + bidi/invisible controls, replace path separators and backslash with `_`, collapse unsafe-char runs to a single `_`, trim leading/trailing whitespace + `.` + `-`, cap at 200 bytes (leaves headroom under typical 255-byte `NAME_MAX`). Empty result → fall back to `attachment-<index>`. Also prepend an NFC normalization step to `slug::slugify` before its existing ASCII-folding pass.

**Priority:** P1

- [x] New `sanitize_attachment_filename(raw: &str, index: usize) -> String` helper (in `ingest.rs` or a sibling module)
- [x] `prepare_attachments` calls the helper on every entry; sanitized name is used for both the on-disk bundle file AND the `attachments` frontmatter entry (one source of truth)
- [x] `slug::slugify` NFC-normalizes input before ASCII folding (add `unicode-normalization` crate if not already present transitively)
- [x] Unit tests for `sanitize_attachment_filename` cover: embedded NUL, CR/LF, `../../etc/passwd`, leading `-rf`, 500-char name (truncated to ≤200 bytes on a char boundary), empty-after-sanitization (falls back to `attachment-<n>`), Windows-style `a\\b\\c.pdf`, NFD-form Unicode, bidi-override sequence, zero-width joiner
- [x] Unit test for `slugify`: NFD and NFC forms of the same visible subject produce the same slug
- [x] Integration test: ingest a fixture `.eml` with two attachments named `../../etc/passwd` and `\x00rce.sh`; assert files land under the expected bundle directory with sanitized names and no path escape
- [x] `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt -- --check` clean

---

## Sprint 44 — Post-launch Security + Quick Fixes (Days 123–125.5) [DONE]

**Goal:** Close the four highest-priority findings from the 2026-04-17 manual test run with small, targeted patches: shell-injection fix in channel triggers (security), operator-visible DKIM sanity check at daemon startup, corrected Claude Code plugin hint, and a restart-hint on `aimx mailbox create`. Also fix the docs nit that caused forwarded-message noise in the test log. Finding #2 (SPF envelope MAIL FROM) already shipped in commit `cd22428` and is excluded. Finding #10 is mostly an operator-side DNS republish; only its two small code add-ons (startup DKIM sanity check + louder setup warning) are in scope here.

**Dependencies:** Sprint 43 (all pre-launch work complete). Independent of Sprint 45 / 46.

**Design notes:**
- Shell-injection fix uses env-vars instead of string substitution for user-controlled template fields (`{from}`, `{subject}`, `{to}`, `{mailbox}`, `{filepath}`) — passing them via `.env()` on the `sh -c` `Command` escapes everything automatically. `{id}` and `{date}` stay as template substitutions (aimx-controlled, opaque/safe). This is a hard break for existing operator configs; pre-launch, so we refuse-to-load with a migration error rather than maintaining a compat shim.
- DKIM startup check: daemon resolves `dkim._domainkey.{config.domain}` once at startup, compares the DNS `p=` value to the SPKI-base64 of the loaded public key, and logs a loud warning on mismatch. **Does not** block startup — DNS may not yet have propagated right after setup and we don't want a crash loop. Also upgrades the setup-time mismatch line to red + adds a second line explaining the receiver-side consequence, so operators don't breeze past it (as happened in T13).

#### S44-1: Env-var channel-trigger expansion (fix shell injection)

**Context:** Finding #9 from the manual test run (P0 security). `src/channel.rs:17-29 substitute_template` substitutes `{from}`, `{subject}`, etc. into a pre-quoted shell command via `.replace()` + `shell_escape::escape`. Any user-controlled header (e.g. `From: U-Zyn Chua <chua@uzyn.com>`) breaks the quoting, AND a crafted `From:` could embed `$()`, backticks, redirects, or `; cmd` to run arbitrary commands as root (daemon runs as root) on every matching trigger. The shipping recipe in `book/channel-recipes.md` reproduces the bug for any real-world `Name <addr>` From. Fix: drop `shell_escape_value`; pass user-controlled values as env vars (`AIMX_FROM`, `AIMX_SUBJECT`, `AIMX_TO`, `AIMX_MAILBOX`, `AIMX_FILEPATH`) on the `Command`; keep `{id}` and `{date}` as template substitutions since both are aimx-controlled (opaque hex / ISO-8601, safe). Templates referencing legacy `{from}` / `{subject}` / `{to}` / `{mailbox}` / `{filepath}` must refuse to load with a clear error pointing at the migration (pre-launch, no compat shim).

**Priority:** P0 (security)

- [x] `src/channel.rs`: `substitute_template` rewritten to only expand `{id}` and `{date}`; `shell_escape_value` deleted
- [x] Command spawn point uses `Command::new("sh").arg("-c").arg(&script).env("AIMX_FROM", …).env("AIMX_SUBJECT", …).env("AIMX_TO", …).env("AIMX_MAILBOX", …).env("AIMX_FILEPATH", …)` — every user-controlled field goes via env
- [x] Config loader rejects any `on_receive.cmd` containing `{from}`/`{subject}`/`{to}`/`{mailbox}`/`{filepath}` with an error naming the offending mailbox + the env-var migration
- [x] `book/channel-recipes.md` rewritten to use `"$AIMX_FROM"`, `"$AIMX_SUBJECT"`, etc. for every recipe (all agents + the shell-log example)
- [x] `docs/manual-test.md` T8 recipe updated to the env-var pattern
- [x] New unit tests covering injection attempts: `U-Zyn Chua <chua@uzyn.com>` (angle-bracket redirect, the T8 repro), `` `whoami` ``, `$(rm -rf /)`, `foo; ls`, `foo\nbar`, subject with embedded single/double quotes — all must run the intended command with the payload safely landing in the env var
- [x] New unit test: config with a legacy placeholder in `on_receive.cmd` fails to load with the migration error
- [x] `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt -- --check` clean
- [x] Extra: `shell-escape` crate removed from `Cargo.toml`; `book/channels.md`, `book/configuration.md`, `docs/manual-setup.md`, `docs/prd.md` FR-30, `docs/idea.md` all swept; integration test `ingest_rejects_legacy_placeholder_config_at_cli` added

#### S44-2: DKIM DNS sanity check at daemon startup + louder setup warning

**Context:** Finding #10 from the manual test run (P0; root cause of #6). On the test VPS the on-disk DKIM private key and the DNS-published DKIM public key had drifted: every outbound signature failed verification at Gmail, silently. Setup's DNS check catches the mismatch but prints it as a single line lost among PASS lines, and the running daemon never re-checks. Code fix has two parts: (a) at `aimx serve` startup, after the DKIM key is loaded in `src/serve.rs`, resolve `dkim._domainkey.{config.domain}` via the already-configured `hickory-resolver`, compare the DNS `p=` value to the SPKI-base64 of the on-disk public key, and log a **loud** mismatch warning to stderr + journald. Must NOT block startup — DNS may not have propagated in a fresh setup, and we don't want to crash-loop. (b) at `aimx setup` (`src/setup.rs verify_dkim`), upgrade the mismatch line to the semantic red helper and follow with a second line stating receiver-side consequence.

**Priority:** P0

- [x] Helper `public_key_spki_base64(path: &Path) -> Result<String>` in `src/dkim.rs` (extract from existing setup code if already derived there; otherwise new); unit-tested against a fixture key
- [x] `src/serve.rs` startup: after DKIM key load and before binding listeners, resolve TXT `dkim._domainkey.{config.domain}` via the existing resolver; if DNS resolution fails, log at `warn` and continue (transient, non-fatal); if DNS `p=` differs from on-disk SPKI, log a multi-line warning to stderr + journal stating mismatch detected, receiver DKIM will fail, and suggesting `aimx setup` to republish DNS
- [x] Startup never blocks or exits on mismatch — daemon proceeds to bind SMTP + UDS listeners normally
- [x] `src/setup.rs verify_dkim` mismatch branch: render the FAIL line via `term::error_red` (or existing semantic helper) and append a second line: "⚠ Outbound DKIM signatures will FAIL verification at receivers until DNS matches."
- [x] Integration test: spin `aimx serve` with a mocked resolver returning a mismatched `p=`; assert the startup log contains the mismatch warning; assert the daemon still binds both listeners and accepts mail <!-- Partial: substituted with unit-test coverage via `DkimTxtResolver` trait + mock resolver exercising `run_dkim_startup_check` / `log_dkim_startup_check` across Match/Mismatch/NoRecord/NoPTag/ResolveError branches + pure `evaluate_dkim_startup` branch tests. Reviewer accepted deferral. Full end-to-end on live `run_serve` deferred as non-blocker. -->
- [x] Integration test: spin `aimx serve` with a resolver that fails the DKIM TXT lookup; assert startup logs a `warn` and continues <!-- Partial: ResolveError branch exercised via fake resolver returning error; end-to-end daemon-level integration deferred. -->
- [x] `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt -- --check` clean

#### S44-3: `aimx agent-setup claude-code` hint fix

**Context:** Finding #7 from the manual test run (P1). `src/agent_setup.rs:111-113 claude_code_hint` prints `"Plugin installed. Restart Claude Code to pick it up (it is auto-discovered from ~/.claude/plugins/)."` — but Claude Code does NOT auto-activate local plugins in `installed_plugins.json`, and `claude -p` especially cannot see the MCP server without an explicit `claude mcp add`. Codex's hint text at `src/agent_setup.rs:115-136` already does this correctly. Mirror the Codex pattern for claude-code. Do not shell out to `claude mcp add` — keeps the tool loosely coupled and avoids PATH dependency at setup time.

**Priority:** P1

- [x] `claude_code_hint` rewritten to instruct the operator to run `claude mcp add --scope user aimx /usr/local/bin/aimx mcp`, mirroring Codex's hint structure (install-location line, blank line, command line, blank line, restart note)
- [x] Existing `src/agent_setup.rs` tests that assert on the hint string updated; new assertion that the hint contains `claude mcp add --scope user aimx`
- [x] `book/agent-integration.md` Claude Code section updated to document the `claude mcp add` step explicitly (remove the "auto-discovered" claim that current docs may mirror)
- [x] `agents/claude-code/README.md` updated similarly
- [x] `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt -- --check` clean
- [x] Extra: `--data-dir` override threaded through the hint with POSIX single-quote escaping

#### S44-4: `aimx mailbox create` / `delete` prints service-restart hint

**Context:** Finding #1 tier-1 from the manual test run (P2 DX). `aimx mailbox create foo` writes `[mailboxes.foo]` to `/etc/aimx/config.toml` but the running daemon holds a Config cloned at startup (`src/serve.rs:139`) — no SIGHUP, no inotify. Inbound mail to `foo@domain` silently routes to `catchall` until the operator restarts the daemon. The command's success line gives no hint this is required. Tier-1 fix: print a follow-up line after the success line for both `create` and `delete`. Tier-2 (route mailbox CRUD via UDS so the daemon picks up changes live) is Sprint 46; tier-1 ships now because it's one line and eliminates the silent-misroute surprise for anyone who installs from a Sprint 44 binary.

**Priority:** P2

- [x] After `println!("Mailbox '{name}' created.")` in `src/mailbox.rs`, print a follow-up hint line pointing the operator at `sudo systemctl restart aimx` (or the OpenRC equivalent) to activate the new mailbox; use the existing `SystemOps` abstraction if it exposes a service-manager hint, otherwise hard-code systemd-first wording with a note about OpenRC
- [x] Same hint printed after `Mailbox '{name}' deleted.`
- [x] Existing `src/mailbox.rs` tests updated to assert on the hint's presence; new test for the delete path
- [x] `book/mailboxes.md` documents the restart requirement so the hint isn't surprising; note Sprint 46 will remove it
- [x] `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt -- --check` clean
- [x] Extra: `restart_hint_command` / `restart_hint_lines` helpers dispatch on `serve::service::detect_init_system()` (systemd / OpenRC / Unknown falls back to systemd)

#### S44-5: `docs/manual-test.md` — specify "compose new" for email steps

**Context:** Finding #3 from the manual test run (P4, docs-only). Testers forwarded/replied to earlier messages in T3/T5/T8/T9, producing `Fwd:`/`Re:` subjects and `in_reply_to`/`references` headers that added noise to the result log. Plan wording didn't specify compose-new vs. reply-to-thread. Trivial docs fix.

**Priority:** P3

- [x] T3, T5, T8, T9 steps in `docs/manual-test.md` updated to specify "compose a new email" rather than "send a test email", with an explicit note against forwarding/replying to prior threads for clean frontmatter
- [x] No code changes; `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt -- --check` still clean

---

## Sprint 45 — Strict Outbound + MCP Writes via Daemon (Days 125.5–128) [DONE]

**Goal:** Remove the privilege-separation and correctness gaps on the send path: (a) `aimx send` stops reading `/etc/aimx/config.toml` entirely — the daemon resolves the sender mailbox from its in-memory `Config`; (b) outbound is tightened to reject both foreign-domain From and any From whose local part doesn't map to an explicitly configured non-wildcard mailbox; (c) MCP write ops (`email_mark_read`, `email_mark_unread`) stop touching mailbox files directly and route through new UDS state-mutation verbs on `aimx serve`. This closes findings #4, #5, and #8 from the 2026-04-17 manual test run. Mailbox CRUD over UDS (finding #1 tier-2) is Sprint 46.

**Dependencies:** Sprint 44 (shell-injection fix + DKIM startup check land first). Sprint 45 touches `src/send.rs`, `src/send_handler.rs`, `src/send_protocol.rs`, `src/mcp.rs`, and `src/main.rs`.

**Design notes:**
- FR-18d (PRD) is tightened: the From mailbox must resolve to a configured non-wildcard mailbox whose address is under `config.domain`. Catchall (`*@domain`) is inbound-only. FR-18e (new) covers the UDS state-mutation verbs introduced this sprint.
- `aimx send` becomes thinner: it no longer loads `config.toml` at all. Daemon receives raw RFC 5322 bytes, parses `From:` itself, runs resolution against its in-memory Config, and rejects with a typed error (`ERR DOMAIN …` or `ERR MAILBOX …`) on failure.
- UDS protocol scaffolding this sprint adds only the MARK verbs (`MARK-READ`, `MARK-UNREAD`). Sprint 46 adds the MAILBOX-CRUD verbs on top of the same codec.
- Per-mailbox `RwLock<()>` in the daemon prevents races between inbound ingest and MCP mutations on the same mailbox (both paths rewrite the same `.md` file).
- Socket permissions and authorization remain unchanged per FR-18b — any local process can invoke the new verbs, same as `SEND` today.

#### S45-1: `aimx send` stops reading `config.toml`; daemon resolves From mailbox

**Context:** Finding #4 from the manual test run (P0; blocks non-root send on a default install). `src/send.rs build_request` calls `resolve_from_mailbox(&config, &args.from)`, and `main.rs` loads `config.toml` before dispatching to `send::run` — fails with EACCES on the default `0640 root:root` install when run as a non-root operator. The manual test session chmod'd config to 0644 as a workaround; that's exactly the privilege-separation regression v0.2 tried to avoid. Fix: daemon derives the mailbox from the submitted message's `From:` header using its own in-memory Config; the client never touches the config file or the DKIM directory. Also drop the `From-Mailbox:` header from the `AIMX/1 SEND` request since the daemon now derives it.

**Priority:** P0

- [x] `src/send_protocol.rs`: remove `From-Mailbox:` from the SEND request encoder and parser; pre-launch, no compat shim <!-- Note: legacy `From-Mailbox:` header is silently ignored on the parser side for forward-compatibility rather than rejected — pre-launch risk is zero either way. -->
- [x] `src/send_handler.rs handle_send_inner`: parse `From:` from the raw message, call `resolve_from_mailbox(&self.config, &from)`; on miss or domain mismatch, return `AIMX/1 ERR <code> …` per FR-18c code set (threaded with S45-2)
- [x] `src/send.rs run`: delete the `Config::load` / `resolve_from_mailbox` call path; client only composes the raw message and opens UDS
- [x] `src/main.rs`: drop the config-load step before `send::run` dispatch (send becomes a path that needs no config file access)
- [x] `src/setup.rs`: confirm `/etc/aimx/config.toml` install mode is `0640 root:root` (manual-test workaround is obsolete once the client doesn't read it)
- [x] `src/send.rs` unit tests for mailbox resolution move to `src/send_handler.rs`
- [x] New integration test: run `aimx send` as a non-root user against a `0640` config; assert success and verify the client never opens the config file (strace-style check optional; at minimum assert the Permission denied error from the manual-test session no longer reproduces)
- [x] `book/mailboxes.md` and `CLAUDE.md` updated — `aimx send` is no longer documented as reading config
- [x] `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt -- --check` clean

#### S45-2: Strict outbound — concrete mailbox + configured domain only

**Context:** Finding #5 from the manual test run + user clarification 2026-04-18 (PRD FR-18d tightened). `resolve_from_mailbox` currently falls back to the wildcard catchall (`*@domain`), so `aimx send --from bogus@domain` succeeds and lands in `sent/catchall/`. Catchall is inbound-routing only; outbound must name a concrete, configured mailbox. User-added constraint: From domain must equal `config.domain` — no sending from a domain aimx isn't authorized for (no DKIM key exists for foreign domains anyway; reject early with a clear error instead of letting the signer fail obliquely). PRD FR-18d already carries the updated semantics after the 2026-04-18 edit; this story enforces them in code.

**Priority:** P0

- [x] `src/send.rs resolve_from_mailbox` (or its new home in `src/send_handler.rs` after S45-1): delete the wildcard fallback branch (`mb.address.starts_with('*')`)
- [x] Before the mailbox lookup, explicitly verify `From:` domain (case-insensitive) equals `config.domain`; on mismatch return `AIMX/1 ERR DOMAIN sender domain '<x>' does not match aimx domain '<config.domain>'`
- [x] Mailbox-miss path returns `AIMX/1 ERR MAILBOX no mailbox matches From: <addr>` with guidance pointing at `aimx mailbox create`
- [x] `book/mailboxes.md` documents the inbound-only semantics of catchall and the concrete-mailbox requirement for outbound; remove any prior implication that catchall can sign outbound
- [x] `book/channels.md` cross-reference updated if it referenced the old wildcard behavior
- [x] Existing tests that asserted wildcard outbound success are flipped to assert the ERR path
- [x] New tests: foreign-domain From (rejected with DOMAIN error); concrete-mailbox send under the configured domain (succeeds); bogus local-part under the configured domain (rejected with MAILBOX error); case-insensitive domain match (`From: x@Agent.Example.Com` matches `domain = "agent.example.com"`)
- [x] `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt -- --check` clean

#### S45-3: UDS protocol scaffolding — `MARK-READ` and `MARK-UNREAD` verbs

**Context:** Groundwork for S45-4 (and for Sprint 46's MAILBOX-CRUD verbs). Extends the `AIMX/1` codec in `src/send_protocol.rs` with two new verbs. Framing mirrors `SEND` exactly (verb line → headers → blank line → body), with `Content-Length: 0` since these carry no body:

```
Client → Server:
  AIMX/1 MARK-READ\n
  Mailbox: <name>\n
  Id: <id>\n
  Folder: inbox|sent\n
  Content-Length: 0\n
  \n

Server → Client:
  AIMX/1 OK\n
or
  AIMX/1 ERR <code> <reason>\n
```

`MARK-UNREAD` has the same shape. Protocol parsing dispatches on the verb token after `AIMX/1 `. Unknown verb → `ERR PROTOCOL`. Consider renaming `src/send_protocol.rs` → `src/uds_protocol.rs` now that it owns more than just SEND; judgment call for the implementer.

**Priority:** P0

- [x] Request parser recognises three verbs (`SEND`, `MARK-READ`, `MARK-UNREAD`) and produces a tagged enum; unknown verb returns `ERR PROTOCOL unknown verb '<x>'`
- [x] Writer helpers mirror `write_request` for each new verb (client side), with typed argument structs
- [x] Response codes stay in the FR-18c set (`OK`, `ERR` with codes from `MAILBOX | DOMAIN | SIGN | DELIVERY | TEMP | MALFORMED | PROTOCOL`); `PROTOCOL` added for codec-level failures <!-- Also added `NOTFOUND` and `IO` codes for MARK-verb handler paths (id not found; file/frontmatter rewrite failure). -->
- [x] Codec unit tests per new verb: happy-path round-trip, malformed header lines, missing required headers (`Mailbox`, `Id`, `Folder`), unknown `Folder` value, empty-body requirement enforced
- [x] Optional file rename to `src/uds_protocol.rs` (update `mod.rs` and all imports if done) <!-- Kept as `send_protocol.rs`; module doc updated to reflect the shared SEND/MARK codec. -->
- [x] `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt -- --check` clean

#### S45-4: MCP write ops route through daemon; per-mailbox concurrency guard

**Context:** Finding #8 from the manual test run (P1, but demoted to P0 because it leaves MCP effectively read-only as non-root). `src/mcp.rs set_read_status` (called by `email_mark_read`) does `std::fs::write(&filepath, …)` directly → fails with EACCES because the MCP server runs as the invoking non-root user and mailbox files are `root:root 0644`. Route all write ops through the daemon via the MARK verbs from S45-3. Read ops (`email_list`, `email_read`) continue to read files directly — files are world-readable by design.

**Priority:** P0

- [x] `src/mcp.rs email_mark_read`: become a thin UDS client — open `/run/aimx/send.sock`, send `MARK-READ`, parse `AIMX/1 OK` / `AIMX/1 ERR <reason>`, surface helpful errors (e.g. "aimx daemon not running — start with `sudo systemctl start aimx`")
- [x] `src/mcp.rs email_mark_unread`: same pattern via `MARK-UNREAD`
- [x] New `src/state_handler.rs` (or extend `src/send_handler.rs` — judgment call) with `handle_mark_read`, `handle_mark_unread` implementations that do the actual frontmatter rewrite, reusing the existing frontmatter serializer
- [x] Daemon acquires a per-mailbox `RwLock<()>` for the duration of the frontmatter rewrite; stored on the daemon state (keyed by mailbox name, lazily-inserted); ingest's append path also takes the same lock so MARK-READ and inbound ingest on the same mailbox cannot interleave a half-written file <!-- Partial: per-mailbox `tokio::sync::RwLock` guards MARK; ingest continues to use the existing process-wide `INGEST_WRITE_LOCK` (std::sync::Mutex). Safety today comes from the two paths writing disjoint files (ingest creates a new `.md`; MARK rewrites an existing one). Reviewer accepted. Writer-unification moved to backlog and tracked across Sprint 45/46. -->
- [x] ERR paths covered: mailbox not configured, id not found, folder invalid, write failure
- [x] Integration test: `email_mark_read` invoked as non-root succeeds; frontmatter `read = true` is persisted; file retains its original ownership (root:root 0644)
- [x] Integration test: concurrent ingest + `MARK-READ` on the same mailbox don't corrupt either file (use tokio `tokio::join!` or spawn pair)
- [x] `book/mcp.md` mentions the daemon-mediated write path so users understand why `aimx serve` must be running for MCP writes
- [x] `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt -- --check` clean
- [x] Extra: `parse_ack_response` tightened to reject trailing garbage on `AIMX/1 OK` (MARK verbs); malformed-frontmatter MARK tests added; `resolve_email_path` deduplicated between `mcp.rs` and `state_handler.rs`

---

## Sprint 46 — Mailbox CRUD via UDS (Daemon Picks Up Changes Live) (Days 128–130.5) [DONE]

**Goal:** Make `aimx mailbox create` / `delete` route through the daemon over UDS so the daemon's in-memory `Config` updates atomically with `config.toml` on disk. Inbound mail to a just-created mailbox routes correctly on the very next SMTP session — no `systemctl restart aimx` required. This closes finding #1 tier-2 from the 2026-04-17 manual test run (the silent-misroute behavior Sprint 44's hint warned about) and finishes the daemon-as-single-writer architecture started in Sprint 45.

**Dependencies:** Sprint 45 (UDS protocol codec for MARK verbs; Sprint 46 extends the same codec). Sprint 44's restart-hint in `src/mailbox.rs` is suppressed when the UDS path succeeds (kept as fallback for when daemon is stopped).

**Design notes:**
- Two new UDS verbs on top of Sprint 45's codec:
  ```
  AIMX/1 MAILBOX-CREATE\n + Name: <name>\n + Content-Length: 0\n + \n
  AIMX/1 MAILBOX-DELETE\n + Name: <name>\n + Content-Length: 0\n + \n
  ```
  Responses reuse `OK` / `ERR <code>` (codes: `MAILBOX` for name conflicts / not-found, `VALIDATION` for name validation failures, `NONEMPTY` for delete with files present).
- Client behaviour (`src/mailbox.rs`): try UDS first; on `ECONNREFUSED`/`ENOENT`/`EACCES` on the socket, fall back to direct `config.toml` edit + print the Sprint 44 restart hint. When UDS succeeds, suppress the hint — the daemon has picked up the change live.
- Daemon-side atomic write: `config.toml` rewritten via write-temp-then-rename; in-memory `Config` swapped under a `RwLock<Arc<Config>>` only after the rename succeeds. Failure leaves both disk and memory in the pre-call state.
- Directory lifecycle: `MAILBOX-CREATE` creates `inbox/<name>/` and `sent/<name>/` if absent. `MAILBOX-DELETE` refuses (returns `ERR NONEMPTY`) when either directory contains files — operator must archive/remove first (matches current CLI semantics).
- Consider whether `Config` should become `Arc<ArcSwap<Config>>` (via the `arc-swap` crate) to avoid a write-lock during ingest — judgment call for the implementer; `RwLock<Arc<Config>>` is simpler and acceptable if ingest latency stays well under 1 ms.

#### S46-1: UDS `MAILBOX-CREATE` — daemon writes config.toml + hot-swaps Config

**Context:** Closes finding #1 tier-2 for the create path. Daemon-side handler validates the name (existing `Config::validate_mailbox_name` rules — no `..`, no `/`, non-empty, etc.), atomically appends `[mailboxes.<name>]` with default fields (`trust = "none"`, empty `on_receive`, empty `trusted_senders`) to `config.toml` via write-temp-then-rename, creates `inbox/<name>/` and `sent/<name>/` directories, and swaps the daemon's in-memory `Config`. Client-side `aimx mailbox create` tries UDS first and falls back to direct edit + Sprint 44's restart hint if the socket is absent.

**Priority:** P1

- [x] `src/send_protocol.rs` (or `uds_protocol.rs`): add `MAILBOX-CREATE` verb parser + writer
- [x] `src/state_handler.rs` `handle_mailbox_create`: validate name; read current config.toml; append stanza; write-temp-then-rename to atomically update disk; create the two directories; swap `RwLock<Arc<Config>>`; return `AIMX/1 OK` on success; on any validation or IO failure return a typed `ERR` <!-- Landed in a new `src/mailbox_handler.rs` rather than `state_handler.rs`; functionally equivalent. Uses a new `ConfigHandle` (`Arc<RwLock<Arc<Config>>>`) shared across every daemon context; a process-wide `CONFIG_WRITE_LOCK` serializes concurrent CREATE/DELETE across different mailbox names (closes a lost-update race caught in Cycle 1 review). -->
- [x] `src/mailbox.rs create`: attempt UDS `MAILBOX-CREATE` first; on socket-missing (`ENOENT`/`ECONNREFUSED`/`EACCES`) fall back to direct `config.toml` edit + restart-hint print (Sprint 44 behavior); when UDS succeeds, suppress the restart hint
- [x] The rest of the daemon (send handler, ingest path) reads Config via the `RwLock<Arc<Config>>` accessor — verify all existing `config.mailboxes.get(…)` call sites thread through correctly
- [x] Integration test: daemon running → `aimx mailbox create foo` via UDS → immediately send Gmail to `foo@domain` → assert the .md lands in `inbox/foo/` (not catchall), no restart required
- [x] Integration test: daemon stopped → `aimx mailbox create foo` falls back to direct config edit + restart-hint present in stdout
- [x] Integration test: concurrent `MAILBOX-CREATE foo` + inbound mail targeting a pre-existing mailbox — neither blocks the other for longer than the write-lock critical section (~microseconds) <!-- Concurrent-create regression test (`concurrent_create_different_names_both_stanzas_survive`, 16 names on multi-thread runtime) added in Cycle 2 to close the lost-update race. -->
- [x] Name-validation tests: `..`, empty string, `/`-containing, duplicate name (already exists) — each returns a distinct `ERR` with the reason
- [x] `book/mailboxes.md` updated — restart is no longer required when daemon is running
- [x] `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt -- --check` clean

#### S46-2: UDS `MAILBOX-DELETE` — safety check + daemon swap

**Context:** Closes finding #1 tier-2 for the delete path. Symmetric to S46-1 but with a safety check: refuse to delete a mailbox whose `inbox/<name>/` or `sent/<name>/` still contains files. Operator must archive or manually remove the files first (matches current CLI semantics per `src/mailbox.rs`). When UDS succeeds, daemon removes the `[mailboxes.<name>]` stanza from config.toml and swaps its in-memory `Config`. Directories are left on disk (operator owns cleanup) — safer than silently deleting files.

**Priority:** P1

- [x] `src/send_protocol.rs` (or `uds_protocol.rs`): add `MAILBOX-DELETE` verb parser + writer
- [x] `src/state_handler.rs` `handle_mailbox_delete`: verify mailbox exists; scan `inbox/<name>/` and `sent/<name>/` for any files — if non-empty return `AIMX/1 ERR NONEMPTY mailbox <name> has <n> files; archive or remove them first`; on success remove the stanza via write-temp-then-rename and swap `Config` <!-- Landed in `src/mailbox_handler.rs` alongside handle_mailbox_create. Empty directories are left on disk (operator owns cleanup); the success message on CLI and MCP notes the leftover dirs. -->
- [x] `src/mailbox.rs delete`: attempt UDS `MAILBOX-DELETE` first; fall back to direct edit + restart-hint when socket absent
- [x] Refuse to delete the `catchall` mailbox via UDS (matches existing CLI guardrail); direct-edit fallback preserves whatever the current rule is
- [x] Integration test: daemon running → create mailbox `qux` → delete via UDS → assert `[mailboxes.qux]` is gone from config.toml and the daemon rejects subsequent inbound to `qux@domain` (routes to catchall)
- [x] Integration test: mailbox with files → `MAILBOX-DELETE` returns `ERR NONEMPTY`; operator then clears files and retry succeeds
- [x] `book/mailboxes.md` documents the NONEMPTY safety behavior and the symmetric live-update semantics
- [x] Sprint 44's restart-hint suppression applies to the delete path too
- [x] `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt -- --check` clean
- [x] Extra: `agents/common/references/mcp-tools.md` updated to document NONEMPTY, catchall guardrail, and leftover-directory behavior on delete

---

## Sprint 47 — Post-v1 Non-blocking Cleanup (Days 130.5–133) [DONE]

**Goal:** Close the 8 non-blocking improvements accumulated across Sprints 44–46 reviews. All are low-risk hardening items — a defense-in-depth pass before v1 tag. No new features; no PRD changes. Grouped into four thematic stories.

**Dependencies:** Sprint 46 (merged).

**Design notes:**
- All items already live in the Non-blocking Review Backlog with full context. This sprint lifts them into first-class stories and resolves them.
- Stories can be implemented independently; no intra-sprint order required.
- S47-4 (writer unification) is the most architecturally substantive — merges Sprint 45's per-mailbox `tokio::sync::RwLock` map with Sprint 36's process-wide `INGEST_WRITE_LOCK` (`std::sync::Mutex`) into a single per-mailbox lock covering both ingest and MARK-* paths. Everything else is small-surface.

#### S47-1: DKIM startup check — end-to-end integration test + runtime-flavor contract

**Context:** Sprint 44 delivered the DKIM startup check with trait-based unit coverage across all five `DkimStartupCheck` branches, but no integration test exercises the wiring in `run_serve` itself. Separately, `HickoryDkimResolver::resolve_dkim_txt` in `src/serve.rs` couples to a multi-threaded tokio runtime via `block_in_place` + `Handle::current().block_on(...)`. This works in `run_serve` (multi-thread flavor), but a future caller on a current-thread runtime would silently break. Pick one of two fixes for the runtime coupling: debug-assert the flavor at entry, or async-ify the trait so the call site just `.await`s.

**Priority:** P3

- [x] New integration test in `tests/integration.rs` spins `aimx serve` against a mock `DkimTxtResolver` and asserts the startup log contains the expected mismatch warning in the `Mismatch` case and a `warn`-level message in the `ResolveError` case; assert both listeners bind afterwards <!-- Two integration tests added, wired through new `AIMX_TEST_DKIM_RESOLVER_OVERRIDE` env var so the live `run_serve` path is exercised. -->
- [x] `HickoryDkimResolver::resolve_dkim_txt` either (a) adds `debug_assert!(matches!(Handle::current().runtime_flavor(), RuntimeFlavor::MultiThread))` with a short comment explaining why, or (b) `async fn resolve_dkim_txt` so `run_serve` can `.await` — whichever fits cleaner <!-- Chose (a): `debug_assert!` on the runtime flavor at entry. -->
- [x] `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt -- --check` clean

#### S47-2: Exhaustiveness & defense-in-depth hardening

**Context:** Two small type-safety / defense-in-depth items from Sprint 44's review. `restart_hint_command` in `src/mailbox.rs` uses a `_` fallback arm that collapses `InitSystem::Systemd` and `InitSystem::Unknown` into the same branch — a future `InitSystem` variant would silently fall through without a compile warning. `execute_triggers` in `src/channel.rs` selectively `.env()`s the `AIMX_*` vars but inherits the rest of the parent-process env — no reachable exploit today, but `.env_clear()` before the selective `.env()` calls (re-adding `PATH`, `HOME`, plus the `AIMX_*` set) is a one-line defense-in-depth upgrade.

**Priority:** P3

- [x] `restart_hint_command`: replace the `_` fallback with an explicit `InitSystem::Unknown =>` arm (or mark `InitSystem` `#[non_exhaustive]`) so adding a new init-system variant fails to compile until the match is updated
- [x] Add a test that destructures every current `InitSystem` variant and asserts the expected hint string (so the exhaustive check is validated at compile time, not only in production)
- [x] `execute_triggers` in `src/channel.rs`: call `.env_clear()` on the `Command`, then re-add `PATH`, `HOME`, plus the five `AIMX_*` vars; short inline comment explaining why
- [x] New unit test asserts an unrelated env var set on the parent process (e.g. `AIMX_LEAK_TEST=sentinel`) does NOT appear in the trigger's environment <!-- Cycle 2: replaced raw `set_var`/`remove_var` with an RAII `EnvVarGuard` so the sentinel is cleaned up even if an assertion panics. -->
- [x] `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt -- --check` clean

#### S47-3: Validation tightening + TOML-rewrite preservation + stronger rename-failure test

**Context:** Three Sprint 46 items in the same area. (a) `validate_mailbox_name` (duplicated in `src/mailbox.rs` and `src/mailbox_handler.rs`) accepts whitespace and other RFC-5322-unsafe local parts — e.g. `"hello world"` makes it through but then produces an invalid email address when interpolated. (b) `write_config_atomic` in `src/mailbox_handler.rs` drops unknown TOML fields and erases comments on rewrite (pre-existing, symmetric to `Config::save`); addressable with a pass-through serde representation or a TOML-edit crate. (c) `create_failure_at_disk_write_leaves_handle_and_disk_unchanged` forces failure via a nonexistent parent directory, which fails at `File::create` and never exercises the `rename(2)` failure branch — tighten the test so the temp write succeeds and the rename fails (e.g. read-only target directory).

**Priority:** P3

- [x] Consolidate `validate_mailbox_name` into a single canonical helper (one of `src/mailbox.rs`, `src/mailbox_handler.rs`, or `src/config.rs` — whichever already owns mailbox-name invariants), tighten to reject whitespace and any character outside a safe local-part class (`[a-z0-9._-]`, case-folded), and thread both call sites through it
- [x] New unit tests cover: `"hello world"` rejected, `"a b"` rejected, `"..foo"` rejected, `""` rejected, `"good-mailbox.1"` accepted
- [x] `write_config_atomic`: either adopt a TOML-editing crate (e.g. `toml_edit`) that preserves comments and unknown stanzas, OR document the current behavior with a comment + add a test asserting a known-unknown stanza survives (whichever the implementer judges less risky for v1) <!-- Chose the document-current-behaviour path. The "stanza survives" wording conflates the two paths; on the documented path the stanza is dropped (v1 contract). Test `unknown_stanza_is_dropped_on_rewrite` pins this so any future regression toward a preserving editor without updating the doc comment will trip. -->
- [x] `create_failure_at_disk_write_leaves_handle_and_disk_unchanged`: rewrite to make the temp write succeed and the rename fail (read-only target dir on the parent, or equivalent), so the test genuinely exercises the rollback-on-rename-failure branch; assertion remains: disk + handle unchanged <!-- Implementer used a non-empty-directory target to force `rename(2)` failure rather than a read-only dir; same effect. -->
- [x] `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt -- --check` clean

#### S47-4: Unify ingest + MARK writers under one per-mailbox lock

**Context:** Sprint 45 left the daemon with two independent lock models: MARK-* uses a per-mailbox `tokio::sync::RwLock` map (in `src/state_handler.rs`), and inbound ingest uses a single process-wide `std::sync::Mutex` (`INGEST_WRITE_LOCK` in `src/ingest.rs`). Safety today comes from the two paths writing disjoint files (ingest creates a new `.md` stem; MARK rewrites an existing one), and the Sprint 45 review accepted that rationale. But both paths do "read → modify → write" on files under the same mailbox tree, and any future story that touches both sides (e.g. a MARK verb that deletes or an ingest path that re-opens an existing file) will lose the invariant. Unify: both ingest and MARK-* acquire the same per-mailbox write lock for the duration of their critical section. Document the lock hierarchy (outer: per-mailbox; inner: process-wide `CONFIG_WRITE_LOCK` for mailbox CRUD) to prevent deadlocks.

**Priority:** P2

- [x] Introduce a single shared per-mailbox lock map (most likely in `src/state_handler.rs` or a new `src/mailbox_locks.rs`) keyed by mailbox name, exposed via an `Arc<Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>>` accessor pattern (or an `ArcSwap<HashMap<..>>` if hot-path reads dominate) <!-- Implemented as new `src/mailbox_locks.rs` with `MailboxLocks` type. -->
- [x] `ingest.rs`: replace the `INGEST_WRITE_LOCK` with an async acquisition of the per-mailbox lock before the file-allocation + write critical section; remove the old global mutex
- [x] `state_handler.rs` MARK-* handlers: acquire the same per-mailbox lock (replacing the current `RwLock` map) before read-modify-write of the target `.md` file
- [x] Update the module-level comment in `src/state_handler.rs` (currently documents the two-lock regime) to reflect the unified model; point to the lock hierarchy (per-mailbox lock outer, `CONFIG_WRITE_LOCK` inner)
- [x] Integration test: concurrent ingest + `MARK-READ` on the same mailbox AND on the same target file (once the file exists) — assert no torn writes and no half-written frontmatter on either side
- [x] Integration test: concurrent `MAILBOX-CREATE` + ingest to the just-created mailbox — assert ordering holds (the config-write happens, then ingest sees the new mailbox; no deadlock on the two locks) <!-- Cycle 2: lingering watchdog thread fixed via `Arc<AtomicBool>` cancel flag + `.join()` rather than `drop(JoinHandle)`. -->
- [x] `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt -- --check` clean
- [x] Extra: `MAILBOX-*` handlers also acquire the unified per-mailbox lock; `StateContext` carries an `Arc<MailboxLocks>` shared across all daemon contexts so SEND, MARK, and MAILBOX-CRUD coordinate cleanly

---

## Sprint 48 — `aimx doctor` + `aimx logs` + Mailbox Delete Force + Shell Completion (Days 133–135.5) [DONE]

**Goal:** DX/diagnostic polish. Clean rename `aimx status` → `aimx doctor`. Add dedicated `aimx logs` subcommand (journalctl wrapper). Doctor output expands with config path, per-mailbox trust + hooks summary, and a "Recent logs" tail (last 10 lines, always on). Add `aimx mailbox delete --force` with interactive confirmation for wiping mailboxes with mail. MCP `mailbox_delete` emits a hint pointing at the CLI when it hits NONEMPTY. Rename `aimx mailbox` → `aimx mailboxes` (singular alias retained). Add `aimx completion <shell>` for tab-completion.

**Dependencies:** Sprint 47 (all prior work complete).

**Design notes:**
- Clean rename for `status` → `doctor` (no alias, pre-launch so acceptable).
- `aimx mailbox` → `aimx mailboxes` with `mailbox` retained as a clap alias for muscle-memory.
- `aimx doctor` and `aimx logs` share the log-tail implementation via a new `SystemOps::tail_service_logs(unit, n)` trait method (systemd → `journalctl -u aimx -n <N>`; OpenRC → best-effort fallback).
- `mailbox delete --force` is destructive: wipes both `inbox/<name>/` and `sent/<name>/` contents before removing the config stanza. Interactive `[y/N]` prompt showing file counts unless `--yes`. Still refuses to delete the `catchall` mailbox.

#### S48-1: Rename `aimx status` → `aimx doctor` (clean rename)

**Context:** `aimx status` is diagnostic-shaped (mailbox counts, DKIM presence, service state, recent activity). The CLI convention across tools like `brew doctor`, `flutter doctor`, and `npm doctor` uses "doctor" for exactly this shape. Pre-launch, so no alias — `aimx status` becomes an unknown subcommand. Repo-wide sweep of user-facing text.

**Priority:** P2

- [ ] `src/cli.rs` `Command` enum: rename `Status` variant to `Doctor`; rename `src/status.rs` → `src/doctor.rs` (including `mod` declaration and all imports in `main.rs`)
- [ ] No alias kept — `aimx status` produces the standard clap "unknown subcommand" error
- [ ] Repo-wide sweep: `aimx status` → `aimx doctor` across `book/`, `README.md`, `CLAUDE.md`, `docs/manual-test.md`, `docs/manual-setup.md`, `agents/common/aimx-primer.md`, `agents/common/references/*.md`, and `src/datadir_readme.md.tpl`
- [ ] Existing status tests renamed; new integration assertion on `aimx doctor --help`; assert `aimx status` returns non-zero with a clap error
- [ ] `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt -- --check` clean

#### S48-2: Extend `aimx doctor` output with config path, trust, hooks summary

**Context:** Today's `aimx status` output lists mailbox counts and recent activity but not the per-mailbox trust config, triggers/hooks, or the resolved config file path — operators troubleshooting "why didn't my trigger fire" have to `cat /etc/aimx/config.toml` by hand. Extend output with a Config section naming the resolved path (honoring `AIMX_CONFIG_DIR`), and expand each per-mailbox block to include `trust = "..."`, `trusted_senders: N entries`, and a triggers-by-event count. The triggers-count wording adapts to whichever schema is live (post-S50 it uses "hooks"; pre-S50 it still reads legacy `on_receive`).

**Priority:** P2

- [ ] `doctor` output gains a "Config" section naming the config path resolved via `config::config_path()`
- [ ] Per-mailbox section expanded: `trust = "..."`, `trusted_senders: N entries`, `triggers: N on_receive` (labelled "hooks" after S50)
- [ ] Output uses the semantic color palette from `src/term.rs` (info/header/dim) consistent with other commands
- [ ] Doctor test fixtures updated with mailboxes having non-default trust and at least one trigger; assertions cover the new lines
- [ ] `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt -- --check` clean

#### S48-3: `aimx logs` subcommand

**Context:** Operators debugging AIMX usually want "show me recent activity" without remembering `journalctl -u aimx`. Dedicated subcommand `aimx logs [--lines N] [--follow]` wraps the common case. On systemd: exec `journalctl -u aimx -n <N>` (default 50); `--follow` maps to `journalctl -f -u aimx`. On OpenRC: best-effort fallback reading `/var/log/aimx/*.log` when present, clear message pointing at the service manager otherwise. Logs go through a new `SystemOps::tail_service_logs(unit, n) -> Result<String>` trait method (plus `follow_service_logs` for `--follow`) so tests don't spawn `journalctl`.

**Priority:** P2

- [ ] `src/cli.rs` `Command` enum: new `Logs { lines: Option<usize>, follow: bool }` variant
- [ ] `src/logs.rs`: new module with `run()` that dispatches via `SystemOps::tail_service_logs(unit, n)` (or `follow_service_logs(unit)` when `--follow`)
- [ ] `SystemOps` trait extended: `tail_service_logs` + `follow_service_logs` with systemd + OpenRC implementations; mock implementation for tests
- [ ] `book/troubleshooting.md` updated to recommend `aimx logs` as the first-line debugging step
- [ ] Integration test: `aimx logs --lines 10` exits 0 (mock returns canned output)
- [ ] `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt -- --check` clean

#### S48-4: `aimx doctor` includes last 10 log lines (always on)

**Context:** Doctor surfaces diagnostic state; 10 lines of journalctl tail at the bottom gives operators a quick glance at "is anything unusual happening right now" without a second command. Always-on (no flag) — 10 lines is cheap. Reuses the `SystemOps::tail_service_logs` trait from S48-3 with a hardcoded `n = 10`. When the service isn't running or logs aren't available, the "Recent logs" section prints a single "no logs available" line rather than erroring.

**Priority:** P2

- [ ] `aimx doctor` output appends a "Recent logs" section at the bottom using `SystemOps::tail_service_logs(unit, 10)`
- [ ] When log retrieval fails (service not running, OpenRC without log files, etc.), a single informative line is printed — doctor does not fail overall
- [ ] Integration test: `aimx doctor` output (against a mocked `SystemOps`) contains the "Recent logs" header
- [ ] `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt -- --check` clean

#### S48-5: `aimx mailbox delete --force` (wipe contents with confirmation)

**Context:** Today `aimx mailbox delete <name>` and the UDS `MAILBOX-DELETE` handler return `ERR NONEMPTY` when `inbox/<name>/` or `sent/<name>/` contains files (S46 design). Operator must archive/remove files manually before deletion succeeds. Add `--force` for the "just wipe it" path: recursively removes files under both directories, then proceeds with the normal delete. Force still prompts interactively showing per-directory file counts (`inbox: N files, sent: M files — continue? [y/N]`); add `--yes` to skip the prompt for scripts. CLI-only — MCP deliberately does NOT gain a force variant (S48-6 wires the hint). Still refuses to delete `catchall`.

**Priority:** P2

- [ ] `src/mailbox.rs delete`: add `--force` and `--yes` flags
- [ ] Without `--force`: existing behavior (refuse on NONEMPTY, print restart hint)
- [ ] With `--force` and no `--yes`: show file counts, prompt `[y/N]`, abort on anything but `y`/`yes`
- [ ] With `--force --yes`: proceed without prompt
- [ ] On proceed: wipe `inbox/<name>/` and `sent/<name>/` contents recursively (respect `AIMX_DATA_DIR`), then route through UDS `MAILBOX-DELETE` (daemon sees empty dirs, succeeds) — implementer picks between wiping pre-UDS-call or extending the protocol with a `Force: true` header
- [ ] `catchall` refusal preserved
- [ ] Integration test: create mailbox, ingest one message → `delete --force --yes` succeeds; stanza gone; directories empty/gone
- [ ] Integration test: `delete --force` without `--yes` and a simulated stdin `n` → aborts, files intact
- [ ] `book/mailboxes.md` documents `--force` with a destructive-behavior warning
- [ ] `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt -- --check` clean

#### S48-6: MCP `mailbox_delete` NONEMPTY hint

**Context:** MCP deliberately does NOT get a force variant — destructive wipes stay on the CLI where operators see prompts and can't be triggered remotely by an agent. When the underlying UDS `MAILBOX-DELETE` returns `ERR NONEMPTY`, the MCP tool response must tell the caller/agent: "cannot delete mailbox 'foo' — inbox: 42 files, sent: 7 files. Run `sudo aimx mailboxes delete --force foo` on the host to wipe and remove." Keeps agents honest about what they can and can't destroy via MCP.

**Priority:** P2

- [ ] `src/mcp.rs mailbox_delete`: on `ERR NONEMPTY`, return a structured error containing inbox/sent file counts AND the exact CLI command
- [ ] `agents/common/references/mcp-tools.md` + `agents/common/aimx-primer.md` updated to document this error path
- [ ] Unit test: mock UDS response of `ERR NONEMPTY` produces the expected hint text
- [ ] `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt -- --check` clean

#### S48-7: Rename `aimx mailbox` → `aimx mailboxes` (with `mailbox` alias)

**Context:** `aimx logs` (S48-3) introduces a plural subcommand name, which stands out against the current all-singular surface. Unify by switching to plurals where pluralization is natural: `aimx mailbox` → `aimx mailboxes`, with the singular retained as a clap alias so operator muscle-memory keeps working. Doctor, setup, ingest, send, serve, mcp, verify, portcheck, dkim-keygen, agent-setup stay as-is (no natural plural). Internal file names (`src/mailbox.rs`, `src/mailbox_handler.rs`) stay — only the CLI surface changes. All prose examples in docs standardize on the plural form.

**Priority:** P2

- [ ] `src/cli.rs`: rename the `Mailbox` subcommand to `Mailboxes`, add `#[clap(alias = "mailbox")]`
- [ ] Every example in `book/mailboxes.md`, `README.md`, `CLAUDE.md`, `agents/common/aimx-primer.md`, `agents/common/references/*.md`, `docs/manual-test.md`, `docs/manual-setup.md`, `src/datadir_readme.md.tpl`, restart-hint output strings, and MCP NONEMPTY hint text uses `aimx mailboxes` (plural)
- [ ] One-line note at top of `book/mailboxes.md` documents that `aimx mailbox` also works as an alias
- [ ] Integration test: both `aimx mailboxes list` and `aimx mailbox list` succeed with identical output
- [ ] `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt -- --check` clean

#### S48-8: `aimx completion <shell>` for tab-completion

**Context:** CLI growing (doctor, logs, mailboxes, hooks, agent-setup) makes shell completion meaningfully useful — `aimx ma<tab>` expands to `aimx mailboxes`, `aimx mailboxes c<tab>` expands to `create`. `clap_complete` generates the completion script from the existing `Cli` derive, no manual grammar. Command: `aimx completion <shell>` prints the script to stdout for the operator to pipe into the right file (e.g. `aimx completion bash | sudo tee /etc/bash_completion.d/aimx`).

**Priority:** P2

- [ ] Add `clap_complete` dependency to `Cargo.toml`
- [ ] `src/cli.rs`: new `Completion { shell: clap_complete::Shell }` variant
- [ ] `main.rs` dispatches to a generator that prints the script for the requested shell to stdout
- [ ] Supports at minimum: bash, zsh, fish, elvish
- [ ] `book/getting-started.md` gains a "Shell completion" section with one-line install snippets per shell
- [ ] Integration test: `aimx completion bash` exits 0 and prints non-empty script containing `_aimx`
- [ ] `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt -- --check` clean

---

## Sprint 49 — Frontmatter `read_at` Timestamp (Days 135.5–138) [DONE]

**Goal:** Add optional `read_at` RFC 3339 UTC timestamp to inbound email frontmatter. Written by the MARK-READ handler; removed on MARK-UNREAD. Lets agents and operators see *when* an email was first marked read without a separate audit log.

**Dependencies:** Sprint 48 (independent; ordering is thematic).

#### S49-1: `read_at` frontmatter field

**Context:** Today `read` is a boolean — we know an email has been read but not when. Add `read_at: Option<DateTime<Utc>>` in the Storage section of `InboundFrontmatter`, written atomically by the MARK-READ handler alongside `read = true`. Field is optional (omitted when absent, never serialized as `null`, per FR-19d). On MARK-UNREAD the field is removed entirely (not set to null). Re-MARK-READ sets a new timestamp — reflects "most recent read", not "first read".

**Priority:** P3

- [ ] `src/frontmatter.rs InboundFrontmatter`: add `read_at: Option<DateTime<Utc>>` in the Storage section (after `read`)
- [ ] `src/state_handler.rs` MARK-READ handler: rewrite frontmatter with `read = true` AND `read_at = Utc::now()` in one atomic temp-then-rename pass
- [ ] `src/state_handler.rs` MARK-UNREAD handler: set `read = false` and remove the `read_at` field entirely
- [ ] `src/ingest.rs`: `read_at` remains unset on initial ingest (email is unread)
- [ ] Unit test: MARK-READ writes timestamp; MARK-UNREAD removes it; MARK-READ → MARK-UNREAD → MARK-READ produces a later timestamp than the first read
- [ ] Book updates: `book/mailboxes.md`, `agents/common/references/frontmatter.md`, `src/datadir_readme.md.tpl` document `read_at`
- [ ] `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt -- --check` clean

---

## Sprint 50 — Hooks Foundation: Rename + Schema + Trust Gate + `after_send` + Logs (Days 138–140.5) [DONE]

**Goal:** Rename the "channels" concept to "hooks" across code/config/docs. Extend the hook model with a new `after_send` event for post-delivery observability. Every hook gets a unique 12-char alphanumeric ID. Per-hook `dangerously_support_untrusted` opt-in replaces the current implicit trust gate (mailbox `trust: none` now fires no hooks by default). Every hook fire emits a structured journald log line for traceability — the agreed substitute for the rejected frontmatter `hooks` field. `before_send` was considered and dropped; only `after_send` is in scope for send-side hooks.

**Dependencies:** Sprint 49 (no hard dep; ordering thematic). Pre-launch config schema change — no migration tooling; legacy schema refuses to load with a clear error.

**Design notes:**
- Config schema: `[[mailboxes.<name>.hooks]]` arrays-of-tables, one entry per hook. Fields: `id` (12-char alphanumeric, required), `event` (`on_receive` | `after_send`), `cmd` (required), `from`/`to`/`subject`/`has_attachment` (optional filters, event-dependent), `dangerously_support_untrusted` (optional, `on_receive` only, default `false`).
- Legacy `[[mailboxes.<name>.on_receive]]` refuses to load at `Config::load`: error names the offending mailbox and points at the migration (rename to `hooks` array, add `event = "on_receive"`, add auto-generated 12-char `id =`). Pre-launch; no compat shim.
- Trust gate (replaces FR-35/36/37 semantics): an `on_receive` hook fires iff `trusted == "true"` on the email OR the hook has `dangerously_support_untrusted = true`. Behavioral change: mailbox `trust: none` (no evaluation) means *no* on_receive hook fires unless it explicitly opts in. Mailbox-level `trust` + `trusted_senders` retained unchanged (they determine the `trusted` frontmatter value, which gates hooks).
- `after_send` is fire-and-forget from the client's perspective but the daemon awaits the subprocess to completion (predictable timing). Exit code discarded. Failures logged at `warn`. Hooks cannot affect delivery.
- Hook fire logs (journald, single line per fire): `hook_id=<id> event=<e> mailbox=<m> email_id=<id>|message_id=<id> exit_code=<n> duration_ms=<n>`. Stable format — documented in `book/hooks.md` as the operator-level trace surface.

#### S50-1: Config schema migration — `channels`/`on_receive` → `hooks` with `event`

**Context:** `src/channel.rs` + `src/config.rs` today parse `[[mailboxes.<name>.on_receive]]`. Replace with `[[mailboxes.<name>.hooks]]` where each entry carries an explicit `event` field. Legacy schema refuses to load (pre-launch; no compat shim). Rename `src/channel.rs` → `src/hook.rs`; update all `mod channel` references. Full doc sweep: `book/channels.md` → `book/hooks.md`, `book/channel-recipes.md` → `book/hook-recipes.md`, `book/configuration.md`, `README.md`, `CLAUDE.md`, agent primer + references, datadir README template, PRD §6.6 — every reference to "channel(s)" in a user-facing context becomes "hook(s)".

**Priority:** P1

- [ ] `MailboxConfig`: replace `on_receive: Vec<ChannelTrigger>` with `hooks: Vec<Hook>`; `Hook` struct has `id: String`, `event: HookEvent` (enum: `OnReceive`, `AfterSend`), `cmd: String`, filter fields, `dangerously_support_untrusted: bool` (default `false`)
- [ ] Legacy `on_receive` field rejected at load with: "mailbox '<name>' uses the legacy `on_receive` schema; migrate to `[[mailboxes.<name>.hooks]]` with `event = \"on_receive\"` and auto-generated 12-char `id =`"
- [ ] `src/channel.rs` renamed to `src/hook.rs`; imports + `mod.rs` updated
- [ ] Repo-wide doc sweep: rename `book/channels.md` → `book/hooks.md`, `book/channel-recipes.md` → `book/hook-recipes.md`; every prose reference to "channel"/"channels" in user-facing contexts (README.md, CLAUDE.md, book/, agents/common/*, docs/manual-test.md, docs/manual-setup.md, src/datadir_readme.md.tpl) becomes "hook"/"hooks"
- [ ] `book/SUMMARY.md` (or equivalent mdbook TOC) updated to point at new filenames
- [ ] `agents/common/aimx-primer.md` + `references/*.md` sweep — trust and trigger sections rewritten for the new vocabulary and semantics
- [ ] Unit tests: legacy schema rejected with migration error; new schema loads; missing `id` rejected; missing `event` rejected; unknown `event` rejected
- [ ] `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt -- --check` clean

#### S50-2: Hook IDs (12-char alphanumeric, auto-generated + hand-overridable)

**Context:** Each hook gets a unique ID for tracing, log correlation, and `aimx hooks delete` addressability. Format: 12 characters from `[a-z0-9]`, generated via `OsRng`. IDs must be globally unique across all mailboxes — `Config::load` rejects duplicates with a clear error naming both owning mailboxes. Users can hand-edit custom IDs in `config.toml` (the loader validates format `^[a-z0-9]{12}$` rather than regenerating).

**Priority:** P1

- [ ] `fn generate_hook_id() -> String` in `src/hook.rs` — 12 chars, `[a-z0-9]`, OsRng-backed
- [ ] `Config::load` validates every hook has a well-formed `id` and rejects duplicates across mailboxes with both-mailbox naming in the error
- [ ] Unit tests: generation produces 12-char alphanumeric; duplicate IDs across mailboxes rejected; malformed IDs rejected (too short / too long / uppercase / non-alphanumeric)
- [ ] `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt -- --check` clean

#### S50-3: Trust gate for `on_receive` — trusted-only default + `dangerously_support_untrusted`

**Context:** PRD FR-35 today says `trust: none` (default) fires all triggers regardless of DKIM; `trust: verified` fires only on DKIM pass. New semantics: `on_receive` hooks fire iff the email's `trusted` frontmatter value is `"true"` OR the hook sets `dangerously_support_untrusted = true`. This is a deliberate behavioral inversion — `trust: none` mailboxes (no trust evaluation, so `trusted == "none"`) now fire *no* hooks unless the hook explicitly opts in. Per-mailbox `trust` + `trusted_senders` retained — they still compute `trusted` via `trust.rs`; the new hook flag is the per-hook escape hatch. `dangerously_support_untrusted = true` rejected at config load on any event other than `on_receive`.

**Priority:** P1

- [ ] `fn should_fire_on_receive(hook: &Hook, email_trusted: TrustedValue) -> bool`: `email_trusted == TrustedValue::True || hook.dangerously_support_untrusted`
- [ ] Hook dispatch path in `src/hook.rs` uses the new gate; drops the old mailbox-level `trust` short-circuit at the hook level (mailbox `trust` still drives `trusted` computation in `trust.rs` — no change there)
- [ ] `Config::load` rejects `dangerously_support_untrusted = true` on non-`on_receive` events with a clear error
- [ ] Unit tests covering the four-way matrix:
  - (`trust: verified`, `trusted = "true"`, default hook) → fires
  - (`trust: verified`, `trusted = "false"`, default hook) → does not fire
  - (`trust: none`, `trusted = "none"`, default hook) → does not fire [behavioral change vs FR-35]
  - (`trust: none`, `trusted = "none"`, hook with `dangerously_support_untrusted = true`) → fires
- [ ] Book updates: `book/hooks.md` (new) + `book/configuration.md` + `agents/common/aimx-primer.md` trust section rewritten with the new semantics
- [ ] PRD §6.7 FR-35/36/37 updated in this sprint (see PRD diff)
- [ ] `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt -- --check` clean

#### S50-4: `after_send` event (fire-and-forget, exit code discarded)

**Context:** New hook event fired by `aimx serve` immediately after an outbound MX delivery attempt — whether delivery succeeded, failed, or deferred. Observability-only: the daemon awaits the subprocess to completion (predictable timing; slow hooks make sends slower) but discards the exit code. Hooks cannot abort or retry the send. `before_send` was considered and explicitly dropped — `after_send` is the only send-side hook. Match filters for `after_send`: `to` (glob), `subject` (substring), `has_attachment`. Env vars (all `AIMX_`-prefixed, passed via `.env()` in the S44-1 injection-safe pattern): `AIMX_FROM`, `AIMX_TO`, `AIMX_SUBJECT`, `AIMX_MAILBOX`, `AIMX_HOOK_ID`, `AIMX_FILEPATH` (path to the sent-copy `.md` under `sent/<mailbox>/`), `AIMX_SEND_STATUS` (`delivered` | `failed` | `deferred`). Subprocess spawned with `.env_clear()` + selective re-add of `PATH`, `HOME`, and the `AIMX_*` set, consistent with S47-2 defense-in-depth.

**Priority:** P1

- [ ] `src/send_handler.rs`: after the `MailTransport::send` result is known (success or failure), locate matching `after_send` hooks for the from-mailbox and fire them synchronously
- [ ] Subprocess env: `.env_clear()` then re-add `PATH`, `HOME`, and the `AIMX_*` set
- [ ] Exit code discarded; non-zero logged at `warn`; subprocess runtime > 5s logged at `warn` (operator visibility into slow hooks)
- [ ] Match filters (`to` glob, `subject` substring, `has_attachment`) evaluated pre-fire; filter mismatch → silent skip
- [ ] `AIMX_SEND_STATUS` maps: `Ok` → `delivered`; `TransportError::Temp` → `deferred`; `TransportError::Permanent` → `failed`
- [ ] Unit tests: after_send fires with expected env vars for each send outcome; filter mismatch skips; non-zero exit is logged but does not propagate
- [ ] Integration test: send an email with an `after_send` hook that writes a sentinel file; assert file exists after the send completes with `AIMX_SEND_STATUS=delivered`
- [ ] Book updates: `book/hooks.md` + `book/hook-recipes.md` (both from S50-1) include `after_send` examples
- [ ] `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt -- --check` clean

#### S50-5: Structured hook fire logs + `AIMX_HOOK_ID` added to `on_receive`

**Context:** The traceability substitute for the rejected frontmatter `hooks` field. Every hook fire emits one `info`-level journald line with a stable format: `hook_id=<id> event=<e> mailbox=<m> email_id=<id>|message_id=<id> exit_code=<n> duration_ms=<n>`. Operators grep by `hook_id=<id>` to trace every fire of a given hook. Also add `AIMX_HOOK_ID` to the existing `on_receive` env var set for symmetry — hooks can self-identify for their own logging.

**Priority:** P2

- [ ] `src/hook.rs` execute path emits one `info`-level log line per fire with the fields above, using the existing tracing macro
- [ ] Log format documented verbatim in `book/hooks.md` so operators can build grep workflows
- [ ] `on_receive` env var set gains `AIMX_HOOK_ID`; existing five env vars (`AIMX_FROM`, `AIMX_TO`, `AIMX_SUBJECT`, `AIMX_MAILBOX`, `AIMX_FILEPATH`) preserved
- [ ] Unit test captures log output for a simulated hook fire (success and failure) and asserts the expected key/value pairs
- [ ] `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt -- --check` clean

---

## Sprint 51 — Hooks CLI + UDS Hot-Swap + `mailboxes show` (Days 140.5–143) [IN PROGRESS]

**Goal:** Land the user-facing hooks surface on top of Sprint 50's foundation. Add `aimx mailboxes show <name>` for per-mailbox deep-dive. Add `aimx hooks list | create | delete` for hook CRUD (flag-based, no update — delete and recreate). Extend the UDS protocol with `HOOK-CREATE` / `HOOK-DELETE` verbs so the daemon hot-swaps the in-memory `Arc<Config>` and newly-created hooks fire on the very next event without `systemctl restart aimx`. `hook` is retained as a clap alias for `hooks`.

**Dependencies:** Sprint 50 (hook schema + IDs + trust gate all required).

**Design notes:**
- `aimx hooks list [--mailbox <name>]` scans the in-memory config and prints a table. Global view by default, filtered with `--mailbox`.
- `aimx hooks create` auto-generates the ID and prints it on success. Flag validation: `--from` only on `on_receive`; `--to` only on `after_send`; `--dangerously-support-untrusted` only on `on_receive`.
- `aimx hooks delete <id>` prompts interactively (`[y/N]`) showing the hook's mailbox, event, and cmd unless `--yes`.
- All three route UDS first, fall back to direct `config.toml` edit + Sprint 44 restart hint on socket-missing — same pattern as Sprint 46's `MAILBOX-CREATE`/`DELETE`.
- Daemon-side handlers take the per-mailbox lock (reuses `MailboxLocks` from S47-4) as outer lock, process-wide `CONFIG_WRITE_LOCK` as inner lock — preserves the S47-4 hierarchy.

#### S51-1: `aimx mailboxes show <name>` CLI

**Context:** Companion to `aimx doctor` for deep-dive on a single mailbox. Shows: address, `trust` value, full `trusted_senders` list (not just a count), hooks grouped by event (each entry displays `id`, `cmd` truncated to 60 chars with a `…` suffix when longer, filters in compact form, `dangerously_support_untrusted` flag where set), inbox + sent + unread message counts. Pretty-printed and colorized via `src/term.rs`.

**Priority:** P2

- [ ] `src/cli.rs`: `Mailboxes` subcommand gains a `show { name: String }` variant
- [ ] `src/mailbox.rs show`: pretty-print using `src/term.rs` semantic colors; honors `NO_COLOR` / non-TTY
- [ ] Fixture-based integration test: mailbox with `trust: verified`, 2 trusted_senders entries, 1 `on_receive` hook, 1 `after_send` hook → assert all lines present in stdout
- [ ] `aimx mailbox show <name>` works via the clap alias
- [ ] `book/mailboxes.md` documents `show` with an example
- [ ] `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt -- --check` clean

#### S51-2: `aimx hooks list | create | delete` CLI (flag-based)

**Context:** Top-level hook management, symmetric to `aimx mailboxes`. Pluralized at birth — `hook` retained as a clap alias for muscle-memory. `list` scans config and prints a table. `create` is flag-based (not interactive) — auto-generates ID, prints it on success. `delete <id>` prompts interactively (showing hook details) unless `--yes`. No `update` — delete and recreate.

**Priority:** P1

- [ ] `src/cli.rs`: new `Hooks` subcommand with `#[clap(alias = "hook")]`; sub-subcommands `list`, `create`, `delete`
- [ ] `aimx hooks list [--mailbox <name>]`: table with columns `id`, `mailbox`, `event`, `cmd` (truncated), filters summary
- [ ] `aimx hooks create --mailbox <name> --event <on_receive|after_send> --cmd <cmd> [--from <glob>] [--to <glob>] [--subject <sub>] [--has-attachment] [--dangerously-support-untrusted]`: validates mailbox exists; validates event × filter combinations (`--from` only on `on_receive`; `--to` only on `after_send`; `--dangerously-support-untrusted` only on `on_receive`); auto-generates ID via `hook::generate_hook_id()`; prints the new ID on success
- [ ] `aimx hooks delete <id> [--yes]`: shows `id | mailbox | event | cmd` and prompts `[y/N]` unless `--yes`; `aimx hook delete <id>` works via alias
- [ ] All three dispatch UDS first (S51-3), fall back to direct `config.toml` edit + Sprint 44 restart hint on socket-missing
- [ ] Integration tests: create + list roundtrip, delete with simulated-stdin confirmation, invalid flag combos rejected at parse time
- [ ] `book/hooks.md` (from S50-1) covers the CLI with copy-paste examples per event type
- [ ] `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt -- --check` clean

#### S51-3: UDS `HOOK-CREATE` / `HOOK-DELETE` verbs (daemon hot-swap)

**Context:** Same pattern as Sprint 46's `MAILBOX-CREATE` / `MAILBOX-DELETE`: client speaks UDS, daemon atomically rewrites `config.toml` (write-temp-then-rename) and hot-swaps `Arc<Config>` via `ConfigHandle::store` so newly-created hooks fire on the very next ingest/send without `systemctl restart aimx`. Lock hierarchy preserved (outer: per-mailbox via `MailboxLocks` from S47-4; inner: process-wide `CONFIG_WRITE_LOCK`). `HOOK-CREATE` request body carries a TOML-encoded hook stanza (one hook); `HOOK-DELETE` carries the hook id in a `Hook-Id:` header with empty body. Responses reuse `OK` / `ERR <code>` with codes: `VALIDATION` (bad event × filter combo), `MAILBOX` (mailbox not configured), `NOTFOUND` (hook id not found on delete), `IO` (disk write failure).

**Priority:** P1

- [ ] `src/send_protocol.rs`: add `HOOK-CREATE` + `HOOK-DELETE` verb parsers and writers; `HOOK-CREATE` carries the hook config as a TOML body, `HOOK-DELETE` carries the id in a header
- [ ] `src/hook_handler.rs` (or extend `src/mailbox_handler.rs` — implementer's call): `handle_hook_create` validates, appends to the mailbox's `hooks` array in the in-memory Config clone, atomically rewrites config.toml + swaps via `ConfigHandle`; `handle_hook_delete` locates by id across all mailboxes, removes, atomic write + swap
- [ ] Per-mailbox lock (`MailboxLocks`) taken around the operation; `CONFIG_WRITE_LOCK` under it
- [ ] Client side in `src/hook_client.rs` (new) or `src/hook.rs`: tries UDS first; on `ENOENT` / `ECONNREFUSED` / `EACCES` falls back to direct `config.toml` edit + Sprint 44 restart hint
- [ ] Integration test: daemon running → `aimx hooks create` → immediately receive a matching email → hook fires without restart
- [ ] Integration test: daemon stopped → `aimx hooks create` falls back to direct edit + restart hint in stdout
- [ ] Integration test: concurrent `HOOK-CREATE` on two different mailboxes succeed without stanza loss (mirrors Sprint 46's concurrent-create regression test)
- [ ] `book/hooks.md` documents the live-update semantics (no restart required when daemon is running)
- [ ] `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt -- --check` clean

---

## Summary Table

| Sprint | Days | Focus | Key Output | Status |
|--------|------|-------|------------|--------|
| 1 | 1–2.5 | Core Pipeline + Idea Validation | `aimx ingest`, basic `aimx send`, mailbox CLI, CI pipeline, test fixtures — testable on VPS | Done |
| 2 | 3–5 | DKIM + Production Outbound | DKIM signing, threading, attachments — mail passes Gmail checks | Done |
| 2.5 | 5.5–6 | Non-blocking Cleanup | Ingest/send hardening, test gaps, `--data-dir` CLI option | Done |
| 3 | 6–8.5 | MCP Server | All 9 MCP tools — Claude Code can read/send email | Done |
| 4 | 8–10 | Channel Manager + Inbound Trust | Triggers, match filters, DKIM/SPF verification, trust gating | Done |
| 5 | 10.5–12.5 | Setup Wizard | `aimx setup` — one-command setup with preflight + DNS | Done |
| 5.5 | 12.5–13 | Non-blocking Cleanup | Serialization, resolver dedup, SPF fix, setup backup | Done |
| 6 | 13–15.5 | Verifier Service + Polish | Hosted probe, status/verify CLI, README | Done |
| 7 | 16–18.5 | Security Hardening + Critical Fixes | DKIM enforcement, header injection fix, atomic ingest, verify race fix, setup e2e verify | Done |
| 8 | 19–21.5 | Setup Robustness, CI & Documentation | DNS verification accuracy, data-dir propagation, SPF fix, configurable verify URLs, CI coverage, doc fixes | Done |
| 9 | 22–24.5 | Migrate from YAML to TOML | Replace serde_yaml with toml crate for config and email frontmatter | Done |
| 10 | 25–27.5 | Verifier Service Overhaul | Remove echo, add port 25 listener, EHLO probe, remove ip parameter — no outbound email | Done |
| 11 | 28–30.5 | Setup Flow Rewrite + Client Cleanup | Root check, MTA conflict detection, install-before-check flow, simplified verify, docs | Done |
| 12 | 31–33.5 | aimx-verifier Security Hardening + /reach Endpoint | 4-layer Caddy self-probe fix, `/reach` TCP-only endpoint, self-EHLO trap fix, canonical `Caddyfile` | Done |
| 13 | 34–36.5 | Preflight Flow Fix + PTR Display | Route `aimx preflight` at `/reach`, fix PTR display ordering bug | Done |
| 14 | 37–39.5 | Request Logging for aimx-verifier | Per-request logging for `/probe`, `/reach`, `/health`, and SMTP listener — caller IP, status, elapsed ms | Done |
| 15 | 40–42.5 | Dockerize aimx-verifier | Multi-stage Dockerfile, `docker-compose.yml` with `network_mode: host`, `.dockerignore`, verifier README update | Done |
| 16 | 43–45.5 | Add Caddy to docker-compose | Caddy sibling service in compose (both `network_mode: host`), `DOMAIN` env var, cert volumes, README update | Done |
| 17 | 46–48.5 | Rename Verify Service to Verifier | Rename `services/verify/` → `services/verifier/`, `aimx-verify` → `aimx-verifier` across crate, Docker, CI, and all documentation | Done |
| 18 | 49–51.5 | Guided Setup UX | Interactive domain prompt, debconf pre-seeding, colorized sectioned output ([DNS]/[MCP]/[Deliverability]), re-entrant setup, DNS retry loop, preflight PTR removal, guide update + move to `book/` | Done |
| 19 | 52–54.5 | Embedded SMTP Receiver | Hand-rolled tokio SMTP listener, STARTTLS, ingest integration, connection hardening | Done |
| 20 | 55–57.5 | Direct Outbound Delivery | lettre + hickory-resolver MX resolution, `LettreTransport`, error feedback, remove sendmail | Done |
| 21 | 58–60.5 | `aimx serve` Daemon | CLI wiring, signal handling, systemd/OpenRC service files, end-to-end daemon test | Done |
| 22 | 61–63.5 | Remove OpenSMTPD + Cross-Platform CI | Strip OpenSMTPD from setup/status/verify, Alpine + Fedora CI targets | Done |
| 23 | 64–66.5 | Documentation + PRD Update | Update PRD (NFR-1/2/4, FRs), CLAUDE.md, README, book/, clean up backlog | Done |
| 24 | 67–69.5 | Verify Cleanup + Sudo Requirement | EHLO-only outbound check, remove `/reach` endpoint, `sudo aimx verify`, AIMX capitalization | Done |
| 25 | 70–72.5 | Fix `aimx send` (Permissions + DKIM Signing) | DKIM key `0o644`, fix DKIM signature verification at Gmail — `aimx send` works end-to-end | Done |
| 26 | 73–75.5 | IPv6 Support for Outbound SMTP | Remove IPv4-only workaround, dual-stack SPF records, `ip6:` verification | Done |
| 27 | 76–78.5 | Systemd Unit Hardening | Restart rate-limit, resource limits, network-online deps in generated systemd unit | Done |
| 27.5 | 78.5–79 | CLI Color Consistency | `src/term.rs` semantic helpers, migrate setup.rs, apply across verify/status/mailbox/send/dkim/serve/main | Done |
| 27.6 | — | CI Binary Releases | _Deferred to the Non-blocking Review Backlog — revisit when production-ready_ | Deferred |
| 28 | 79.5–82 | Agent Integration Framework + Claude Code | `agents/` tree, `aimx agent-setup` command, Claude Code plugin, PRD §6.10 | Done |
| 29 | 82–84.5 | Codex CLI + OpenCode + Gemini CLI Integration | Codex plugin, OpenCode skill, Gemini skill, book/ updates | Done |
| 30 | 84.5–87 | Goose + OpenClaw Integration | Goose recipe, OpenClaw skill, README overhaul | Done |
| 31 | 87–89.5 | Channel-Trigger Cookbook | `book/channel-recipes.md`, channel-trigger integration test, cross-links | Done |
| 32 | 89.5–92 | Non-blocking Cleanup | Verifier concurrency bound, outbound DATA sharing + multi-MX errors, TLS/service consistency, NetworkOps dedup, clippy `--all-targets`, cosmetics | Done |
| 33 | 92–94.5 | v0.2 Filesystem Split + `aimx` Group (group reverted in 33.1) | `/etc/aimx/` for config + DKIM keys, `/run/aimx/` via `RuntimeDirectory=aimx`, DKIM private key back to `600` root-only | Done |
| 33.1 | 94.5–97 | Scope Reversal: Drop PTR + `aimx` Group + Non-blocking Cleanup | Strip PTR/reverse-DNS, drop `aimx` system group + group-gating, clear ready-now backlog items, manual E2E validation of Claude Code + Codex CLI plugins | Done |
| 34 | 97–99.5 | v0.2 UDS Wire Protocol + Daemon Send Handler | `src/send_protocol.rs` codec, `aimx serve` binds `/run/aimx/send.sock` (`0o666` world-writable), per-connection handler signs + delivers with `SO_PEERCRED` logged for diagnostics only | Done |
| 35 | 99.5–102 | v0.2 Thin UDS Client + End-to-End | `aimx send` rewritten as UDS client (no DKIM access), end-to-end integration test from client → signed delivery, dead-code + docs sweep | Done |
| 36 | 102–104.5 | v0.2 Datadir Reshape | `inbox/` + `sent/` split per mailbox, `YYYY-MM-DD-HHMMSS-<slug>.md` filenames, Zola-style attachment bundles, mailbox lifecycle touches both trees, MCP `folder` param | Done |
| 37 | 104.5–107 | v0.2 Frontmatter Schema + DMARC | `InboundFrontmatter` struct with section ordering, new fields (`thread_id`, `received_at`, `received_from_ip`, `size_bytes`, `delivered_to`, `list_id`, `auto_submitted`, `dmarc`, `labels`), DMARC verification | Done |
| 38 | 107–109.5 | v0.2 `trusted` Field + Sent-Items Persistence | Always-written `trusted: "none"\|"true"\|"false"` (v1 trust model preserved), sent mail persisted to `sent/<mailbox>/` with outbound block + `delivery_status` | Done |
| 39 | 109.5–112 | v0.2 Primer Skill Bundle + Author Metadata | `agents/common/aimx-primer.md` split into main + `references/`, install-time suffix + references-copy, `U-Zyn Chua <chua@uzyn.com>` standardized repo-wide | Done |
| 40 | 112–114.5 | v0.2 Datadir README + Journald + Book/ | Baked-in `/var/lib/aimx/README.md` with version-gate refresh on `aimx serve` startup, `journalctl -u aimx` replaces stale `/var/log/aimx.log`, full `book/` + `CLAUDE.md` pass | Done |
| 41 | 115–117.5 | Post-v0.2 Backlog Cleanup | Outbound frontmatter fixes, SPF dedup, UDS slow-loris timeout, typed transport errors, DNS error surfacing, test DKIM cache, stale dead_code sweep | Done |
| 42 | 118–120.5 | CLI UX: Config Errors + Setup Race + Version Hash | Helpful error when config missing, wait-for-ready loop in `aimx setup` before port checks, git commit hash in `aimx --version` | Done |
| 43 | 120.5–123 | Pre-launch README Sweep + Hardening | `README.md` v0.2 sweep, `status` uses `SystemOps`, HTML body size cap, bracketed-only `Received:` IP parse, typed lettre error classification, `dkim-keygen` permission-denied UX, attachment filename safety + NFC normalization | Done |
| 44 | 123–125.5 | Post-launch Security + Quick Fixes | Env-var channel-trigger expansion (shell-injection fix), DKIM DNS sanity check at daemon startup + louder setup warning, Claude Code agent-setup hint fix, `aimx mailbox create/delete` restart hint, manual-test.md compose-new clarification | Done |
| 45 | 125.5–128 | Strict Outbound + MCP Writes via Daemon | `aimx send` stops reading config.toml (daemon resolves From), strict outbound (concrete mailbox + configured domain only, wildcard is inbound-only), UDS `MARK-READ`/`MARK-UNREAD` verbs + MCP write ops via daemon with per-mailbox RwLock | Done |
| 46 | 128–130.5 | Mailbox CRUD via UDS (Daemon Picks Up Changes Live) | UDS `MAILBOX-CREATE`/`MAILBOX-DELETE` verbs + daemon hot-swaps `Arc<Config>`, `aimx mailbox create/delete` route through daemon first and suppress restart hint on success, directory lifecycle + NONEMPTY safety on delete | Done |
| 47 | 130.5–133 | Post-v1 Non-blocking Cleanup | DKIM startup integration test + runtime-flavor contract, exhaustive `InitSystem` match + `.env_clear()` defense-in-depth, `validate_mailbox_name` tightening + `write_config_atomic` preservation + stronger rename-failure test, unify ingest + MARK writers under one per-mailbox lock | Done |
| 48 | 133–135.5 | Doctor + Logs + Delete --force + Completion | `aimx status` → `aimx doctor` (clean rename), extended output with config path + trust + hooks summary + last 10 log lines, new `aimx logs` subcommand, `aimx mailbox delete --force` with interactive confirmation, MCP NONEMPTY hint, `aimx mailbox` → `aimx mailboxes` (singular alias retained), `aimx completion <shell>` for tab-completion | Done |
| 49 | 135.5–138 | Frontmatter `read_at` | MARK-READ writes `read_at` timestamp; MARK-UNREAD removes the field | Done |
| 50 | 138–140.5 | Hooks Foundation | Rename `channels` → `hooks` across code/config/docs, 12-char alphanumeric hook IDs, trust gate rewrite (`on_receive` trusted-only + per-hook `dangerously_support_untrusted` opt-in), `after_send` event, structured journald hook-fire logs | Done |
| 51 | 140.5–143 | Hooks CLI + UDS Hot-Swap | `aimx mailboxes show <name>`, `aimx hooks list \| create \| delete` (flag-based, `hook` alias), UDS `HOOK-CREATE` / `HOOK-DELETE` verbs with live `Arc<Config>` swap | In Progress |

## Deferred to v2

| Feature | Rationale |
|---------|-----------|
| Package manager distribution (apt/brew/nix) | v1 ships as `cargo install`; packaging is post-launch polish |
| `webhook` trigger type | `cmd` covers all use cases via curl; native webhook is convenience |
| Web dashboard | Agents don't need a UI; operators use CLI or MCP |
| IMAP/POP3/JMAP | Agents access via MCP/filesystem; traditional mail clients are not the use case |
| Email encryption (PGP/S/MIME) | Adds significant complexity; defer until there's demand |
| Rate limiting / spam filtering | Rely on DMARC policy for v1 |
| Multi-tenant hosted offering | Architecture supports it; business decision for later |

## Non-blocking Review Backlog

This section collects non-blocking feedback from sprint reviews. Questions need human answers (edit inline). Improvements accumulate until triaged into a sprint.

> Completed backlog items 1–58 archived. See [`sprint.backlog.1.md`](sprint.backlog.1.md).

### Questions

Items needing human judgment. Answer inline by replacing the `_awaiting answer_` text, then check the box.

- [x] **(Sprint 2.5)** `serde_yaml` 0.9 is unmaintained/deprecated — should we migrate to an alternative YAML serializer? — Migrate to TOML (`toml` crate) instead. _Triaged into Sprint 9_

### Improvements

Concrete items with clear implementation direction. Will be triaged into a cleanup sprint periodically.

- [x] **(Sprint 38)** Parity test docstring in `src/trust.rs` says "IFF" (if and only if) but the test only checks one direction. — _Triaged into Sprint 41 (S41-1)._
- [x] **(Sprint 38)** `received_at` in `OutboundFrontmatter` serializes as empty string `""` for outbound messages instead of being omitted. — _Triaged into Sprint 41 (S41-1)._
- [x] **(Sprint 38)** `date` field in outbound frontmatter uses a fresh `Utc::now()` timestamp instead of parsing the `Date:` header. — _Triaged into Sprint 41 (S41-1)._
- [x] **(Sprint 37)** SPF is still verified twice in `src/ingest.rs` — redundant DNS lookup per ingest. — _Triaged into Sprint 41 (S41-2)._
- [x] **(Sprint 35)** `LettreTransport::resolve_ipv4` in `src/transport.rs` swallows DNS failures with `unwrap_or_default()`. — _Triaged into Sprint 41 (S41-5)._
- [x] **(Sprint 35, PR #65)** Stale `#[allow(dead_code)]` on `write_request` in `send_protocol.rs:285` — Sprint 35 shipped, function is now used by `send.rs`, `serve.rs`, and tests. — _Triaged into Sprint 41 (S41-7)._
- [x] **(Sprint 40, PR #70)** Missing integration test: `aimx serve` in tempdir with stale `README.md` refreshed at startup. — _Triaged into Sprint 41 (S41-7)._
- [x] **(Sprint 36, PR #66)** `mailbox_list` reads `config.mailboxes.keys()` instead of scanning `inbox/*/` — stray dirs not in config are invisible. — _Not a bug: config-authoritative mailbox list is the intended design (2026-04-16)._
- [x] **(Sprint 36, PR #66)** Concurrent-ingest race on bundle directories — two ingests with the same subject/second can cross-contaminate attachment files. — _Deferred by user decision (2026-04-16). Unlikely in practice; locking design needed._
- [x] **(Sprint 34, PR #64)** `LettreTransport::send` parses full `To:` header as `lettre::Address` — fails on display-name or multi-recipient form. — _Already fixed: `send_handler.rs:148` now uses `extract_bare_address(&to_header)` to normalize before transport._
- [x] **(Sprint 44, PR #79)** DKIM startup check lacks an end-to-end integration test against live `run_serve`. _Triaged into Sprint 47 (S47-1)._
- [x] **(Sprint 44, PR #79)** `HickoryDkimResolver::resolve_dkim_txt` depends on multi-threaded tokio runtime; debug-assert or async-ify. _Triaged into Sprint 47 (S47-1)._
- [x] **(Sprint 44, PR #79)** `restart_hint_command` uses `_` fallback arm collapsing `InitSystem::Systemd` and `InitSystem::Unknown`. _Triaged into Sprint 47 (S47-2)._
- [x] **(Sprint 44, PR #79)** `execute_triggers` inherits parent-process env; consider `.env_clear()` + selective re-add. _Triaged into Sprint 47 (S47-2)._
- [x] **(Sprint 46, PR #81)** `write_config_atomic` drops unknown TOML fields and erases comments on rewrite. _Triaged into Sprint 47 (S47-3)._
- [x] **(Sprint 46, PR #81)** `validate_mailbox_name` accepts whitespace and RFC-5322-unsafe local parts. _Triaged into Sprint 47 (S47-3)._
- [x] **(Sprint 46, PR #81)** `create_failure_at_disk_write_leaves_handle_and_disk_unchanged` never exercises the rename failure branch. _Triaged into Sprint 47 (S47-3)._
- [x] **(Sprint 45, PR #78 → Sprint 46, PR #81)** MARK-* and inbound ingest are not serialized against each other; unify writers under one per-mailbox lock. _Triaged into Sprint 47 (S47-4)._
- [ ] **(Sprint 48, PR #96)** Add integration test for `aimx mailboxes delete --force` socket-missing fallback path — currently the shared logic with the non-force path is covered, but no explicit test pins `ENOENT`/`ECONNREFUSED` behaviour for the force variant.
- [ ] **(Sprint 48, PR #96)** Structure the NONEMPTY ack response (`AckResponse`) to carry `inbox_count` and `sent_count` as typed fields instead of the MCP client regex-parsing `"{} files"` out of the reason string — enables dropping the `parse_nonempty_counts` fallback and the defensive `(0,0)` path.
- [ ] **(Sprint 48, PR #96)** Daemon-side NONEMPTY reason string at `src/mailbox_handler.rs:195-201` still uses `"{} files"` uniformly (the CLI prompt was fixed by `pluralize_files`, but the MCP-facing wire string was not). Will clean up naturally once the structured `AckResponse` from the item above lands.
- [ ] **(Sprint 48, PR #96)** Replace `->` with `→` on the doctor trust/hooks sub-lines for visual consistency with other commands — deferred because it would churn three pinned tests; no operator has reported garbled output.
- [x] **(Sprint 50, PR #98)** `extract_email_for_match` in `src/hook.rs` + `src/trust.rs` slice-panicked when `>` preceded `<` (e.g. `"foo>bar<baz>"`). _Hardened to `rfind('<')` + tail `find('>')` (mirrors `send_handler::extract_bare_address`); regression tests in `hook::tests::extract_email_for_match_handles_inverted_angle_brackets` + `trust::tests::extract_email_for_match_no_panic_on_inverted_brackets`._
- [x] **(Sprint 50, PR #98)** `after_send` structured log line never surfaced `message_id`, leaving TEMP failures with an empty `email_id=` tag. _Threaded `message_id` through `AfterSendContext`; `execute_after_send` now passes it to `run_and_log` so the log falls back to `message_id=<id>` when `filepath` is empty. Regression test: `hook::tests::after_send_log_line_falls_back_to_message_id_when_filepath_empty`._
- [x] **(Sprint 50, PR #98)** `has_attachment` filter on `after_send` hooks was advertised but `send_handler::fire_after_send_hooks` hardcodes `has_attachment: false` — filter could never meaningfully match. _Rejected at `Config::load` (outbound via UDS is text-only in v0.2). Docs updated in `book/hooks.md` + `book/configuration.md`; regression test: `config::tests::load_rejects_has_attachment_on_after_send`._
- [ ] **(Sprint 50, PR #98)** No integration assertion that `AIMX_SEND_STATUS` correctly surfaces `deferred` / `failed` (only `delivered` is covered end-to-end). Existing `Behavior::TempErr` / `Behavior::PermanentErr` mock transports should make this straightforward.
- [ ] **(Sprint 50, PR #98)** Finish the channel→hook doc sweep stragglers: `src/cli.rs:23` (`long_about` still says "Channel rules"), `src/setup.rs:1373-1374` (trust prompt still mentions "channel triggers"), `src/agent_setup.rs:135` (stale comment referencing "channel-trigger recipes").
- [ ] **(Sprint 50, PR #98)** Reconcile PRD FR-36 with FR-37b — FR-36 says "when a sender matches, `trusted` is set to `\"true\"` without requiring DKIM evaluation," but FR-37b (controlling rule) and the implementation in `src/trust.rs` require both allowlist match AND DKIM pass.

### Deferred Feature Sprints

Feature sprints that were planned, then deferred by the user. Full spec preserved so the work can be promoted back to an active sprint without loss. Revisit when the gating condition is met.

- [ ] **(Originally Sprint 27.6 — deferred by user pending production readiness)** **CI Binary Releases.**
  **Goal:** Publish prebuilt `aimx` binaries for common Linux architectures so users can `curl | tar` instead of installing Rust and running `cargo build`. Tags produce attached GitHub Release artifacts; `main` merges produce workflow artifacts (90-day retention).
  **Gating condition:** Revisit when AIMX is ready to promote to production users (e.g., once the PRD's v1 scope is otherwise complete and distribution is the remaining gap).
  **Scope / acceptance criteria (preserved verbatim):**
  - New `.github/workflows/release.yml` triggered on `push: tags: ['v*']`
  - Release workflow matrix builds four targets: `x86_64-unknown-linux-gnu`, `x86_64-unknown-linux-musl`, `aarch64-unknown-linux-gnu`, `aarch64-unknown-linux-musl`
  - Each job produces `aimx-<version>-<target>.tar.gz` containing `aimx` binary (+x permissions preserved), `LICENSE`, `README.md`
  - Final aggregation step computes `SHA256SUMS` (one line per tarball) and uploads it alongside the tarballs
  - All artifacts attached to the GitHub Release matching the tag via `softprops/action-gh-release` (Release auto-created if missing)
  - `ci.yml` gains a `build-binaries` job that runs only on `push` to `main` and uploads the same four binaries as workflow artifacts with 90-day retention
  - Cross-compilation for aarch64 uses `cross` or documented equivalent; musl builds reuse the Alpine-style musl toolchain pattern
  - `aimx --version` output of a downloaded binary matches the git tag (requires `Cargo.toml` version to match the tag; maintainer step documented in the workflow or README)
  - Binary on each Linux target runs `aimx --help` cleanly on a matching OS (manual validation at least once — fresh VPS, Alpine VM, aarch64 instance)
  - Existing CI jobs remain unchanged — release work is additive
  - `README.md` and `book/getting-started.md` gain an "Install from prebuilt binary" section with a `curl … | tar -xzf -` one-liner and SHA256 verification via `sha256sum -c SHA256SUMS`
  - Dry-run validation: push a `v0.0.0-test` tag (or use `workflow_dispatch`), confirm all four tarballs + SHA256SUMS land on the Release; delete the test tag/release afterwards
  - PRD §9 In Scope already mentions this work; no PRD change needed on promotion
  **Out of scope:** verifier service binary (deployed via Docker), macOS/Windows targets, auto-tagging/version bumps
