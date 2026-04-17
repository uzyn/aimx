# AIMX — Sprint Plan

**Sprint cadence:** 2.5 days per sprint
**Team:** Solo developer with heavy AI augmentation (Claude Code)
**Total sprints:** 45 (6 original + 2 post-audit hardening + 1 YAML→TOML migration + 2 verifier/setup overhaul + 2 post-Sprint-11 bug fixes + 2 verifier ops + 1 deployment + 1 service rename + 1 setup UX + 5 embedded SMTP + 1 verify cleanup + 1 DKIM permissions fix + 1 IPv6 support + 1 systemd unit hardening + 1 CLI color consistency + 1 CI binary releases + 3 agent integration + 1 channel-trigger cookbook + 1 non-blocking cleanup + 1 scope-reversal (33.1) + 8 v0.2 pre-launch reshape + 1 post-v0.2 backlog cleanup + 1 CLI UX fixes + 1 pre-launch README + hardening sweep + 3 post-launch hardening)
**Timeline:** ~130 calendar days (v1: ~92 days, v0.2 reshape: ~22.5 days, post-v0.2 cleanup: ~2.5 days, CLI UX fixes: ~2.5 days, pre-launch sweep: ~2.5 days, post-launch hardening: ~7.5 days)
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

## Sprint 44 — Post-launch Security + Quick Fixes (Days 123–125.5) [NOT STARTED]

**Goal:** Close the four highest-priority findings from the 2026-04-17 manual test run with small, targeted patches: shell-injection fix in channel triggers (security), operator-visible DKIM sanity check at daemon startup, corrected Claude Code plugin hint, and a restart-hint on `aimx mailbox create`. Also fix the docs nit that caused forwarded-message noise in the test log. Finding #2 (SPF envelope MAIL FROM) already shipped in commit `cd22428` and is excluded. Finding #10 is mostly an operator-side DNS republish; only its two small code add-ons (startup DKIM sanity check + louder setup warning) are in scope here.

**Dependencies:** Sprint 43 (all pre-launch work complete). Independent of Sprint 45 / 46.

**Design notes:**
- Shell-injection fix uses env-vars instead of string substitution for user-controlled template fields (`{from}`, `{subject}`, `{to}`, `{mailbox}`, `{filepath}`) — passing them via `.env()` on the `sh -c` `Command` escapes everything automatically. `{id}` and `{date}` stay as template substitutions (aimx-controlled, opaque/safe). This is a hard break for existing operator configs; pre-launch, so we refuse-to-load with a migration error rather than maintaining a compat shim.
- DKIM startup check: daemon resolves `dkim._domainkey.{config.domain}` once at startup, compares the DNS `p=` value to the SPKI-base64 of the loaded public key, and logs a loud warning on mismatch. **Does not** block startup — DNS may not yet have propagated right after setup and we don't want a crash loop. Also upgrades the setup-time mismatch line to red + adds a second line explaining the receiver-side consequence, so operators don't breeze past it (as happened in T13).

#### S44-1: Env-var channel-trigger expansion (fix shell injection)

**Context:** Finding #9 from the manual test run (P0 security). `src/channel.rs:17-29 substitute_template` substitutes `{from}`, `{subject}`, etc. into a pre-quoted shell command via `.replace()` + `shell_escape::escape`. Any user-controlled header (e.g. `From: U-Zyn Chua <chua@uzyn.com>`) breaks the quoting, AND a crafted `From:` could embed `$()`, backticks, redirects, or `; cmd` to run arbitrary commands as root (daemon runs as root) on every matching trigger. The shipping recipe in `book/channel-recipes.md` reproduces the bug for any real-world `Name <addr>` From. Fix: drop `shell_escape_value`; pass user-controlled values as env vars (`AIMX_FROM`, `AIMX_SUBJECT`, `AIMX_TO`, `AIMX_MAILBOX`, `AIMX_FILEPATH`) on the `Command`; keep `{id}` and `{date}` as template substitutions since both are aimx-controlled (opaque hex / ISO-8601, safe). Templates referencing legacy `{from}` / `{subject}` / `{to}` / `{mailbox}` / `{filepath}` must refuse to load with a clear error pointing at the migration (pre-launch, no compat shim).

**Priority:** P0 (security)

- [ ] `src/channel.rs`: `substitute_template` rewritten to only expand `{id}` and `{date}`; `shell_escape_value` deleted
- [ ] Command spawn point uses `Command::new("sh").arg("-c").arg(&script).env("AIMX_FROM", …).env("AIMX_SUBJECT", …).env("AIMX_TO", …).env("AIMX_MAILBOX", …).env("AIMX_FILEPATH", …)` — every user-controlled field goes via env
- [ ] Config loader rejects any `on_receive.cmd` containing `{from}`/`{subject}`/`{to}`/`{mailbox}`/`{filepath}` with an error naming the offending mailbox + the env-var migration
- [ ] `book/channel-recipes.md` rewritten to use `"$AIMX_FROM"`, `"$AIMX_SUBJECT"`, etc. for every recipe (all agents + the shell-log example)
- [ ] `docs/manual-test.md` T8 recipe updated to the env-var pattern
- [ ] New unit tests covering injection attempts: `U-Zyn Chua <chua@uzyn.com>` (angle-bracket redirect, the T8 repro), `` `whoami` ``, `$(rm -rf /)`, `foo; ls`, `foo\nbar`, subject with embedded single/double quotes — all must run the intended command with the payload safely landing in the env var
- [ ] New unit test: config with a legacy placeholder in `on_receive.cmd` fails to load with the migration error
- [ ] `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt -- --check` clean

#### S44-2: DKIM DNS sanity check at daemon startup + louder setup warning

**Context:** Finding #10 from the manual test run (P0; root cause of #6). On the test VPS the on-disk DKIM private key and the DNS-published DKIM public key had drifted: every outbound signature failed verification at Gmail, silently. Setup's DNS check catches the mismatch but prints it as a single line lost among PASS lines, and the running daemon never re-checks. Code fix has two parts: (a) at `aimx serve` startup, after the DKIM key is loaded in `src/serve.rs`, resolve `dkim._domainkey.{config.domain}` via the already-configured `hickory-resolver`, compare the DNS `p=` value to the SPKI-base64 of the on-disk public key, and log a **loud** mismatch warning to stderr + journald. Must NOT block startup — DNS may not have propagated in a fresh setup, and we don't want to crash-loop. (b) at `aimx setup` (`src/setup.rs verify_dkim`), upgrade the mismatch line to the semantic red helper and follow with a second line stating receiver-side consequence.

**Priority:** P0

- [ ] Helper `public_key_spki_base64(path: &Path) -> Result<String>` in `src/dkim.rs` (extract from existing setup code if already derived there; otherwise new); unit-tested against a fixture key
- [ ] `src/serve.rs` startup: after DKIM key load and before binding listeners, resolve TXT `dkim._domainkey.{config.domain}` via the existing resolver; if DNS resolution fails, log at `warn` and continue (transient, non-fatal); if DNS `p=` differs from on-disk SPKI, log a multi-line warning to stderr + journal stating mismatch detected, receiver DKIM will fail, and suggesting `aimx setup` to republish DNS
- [ ] Startup never blocks or exits on mismatch — daemon proceeds to bind SMTP + UDS listeners normally
- [ ] `src/setup.rs verify_dkim` mismatch branch: render the FAIL line via `term::error_red` (or existing semantic helper) and append a second line: "⚠ Outbound DKIM signatures will FAIL verification at receivers until DNS matches."
- [ ] Integration test: spin `aimx serve` with a mocked resolver returning a mismatched `p=`; assert the startup log contains the mismatch warning; assert the daemon still binds both listeners and accepts mail
- [ ] Integration test: spin `aimx serve` with a resolver that fails the DKIM TXT lookup; assert startup logs a `warn` and continues
- [ ] `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt -- --check` clean

#### S44-3: `aimx agent-setup claude-code` hint fix

**Context:** Finding #7 from the manual test run (P1). `src/agent_setup.rs:111-113 claude_code_hint` prints `"Plugin installed. Restart Claude Code to pick it up (it is auto-discovered from ~/.claude/plugins/)."` — but Claude Code does NOT auto-activate local plugins in `installed_plugins.json`, and `claude -p` especially cannot see the MCP server without an explicit `claude mcp add`. Codex's hint text at `src/agent_setup.rs:115-136` already does this correctly. Mirror the Codex pattern for claude-code. Do not shell out to `claude mcp add` — keeps the tool loosely coupled and avoids PATH dependency at setup time.

**Priority:** P1

- [ ] `claude_code_hint` rewritten to instruct the operator to run `claude mcp add --scope user aimx /usr/local/bin/aimx mcp`, mirroring Codex's hint structure (install-location line, blank line, command line, blank line, restart note)
- [ ] Existing `src/agent_setup.rs` tests that assert on the hint string updated; new assertion that the hint contains `claude mcp add --scope user aimx`
- [ ] `book/agent-integration.md` Claude Code section updated to document the `claude mcp add` step explicitly (remove the "auto-discovered" claim that current docs may mirror)
- [ ] `agents/claude-code/README.md` updated similarly
- [ ] `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt -- --check` clean

#### S44-4: `aimx mailbox create` / `delete` prints service-restart hint

**Context:** Finding #1 tier-1 from the manual test run (P2 DX). `aimx mailbox create foo` writes `[mailboxes.foo]` to `/etc/aimx/config.toml` but the running daemon holds a Config cloned at startup (`src/serve.rs:139`) — no SIGHUP, no inotify. Inbound mail to `foo@domain` silently routes to `catchall` until the operator restarts the daemon. The command's success line gives no hint this is required. Tier-1 fix: print a follow-up line after the success line for both `create` and `delete`. Tier-2 (route mailbox CRUD via UDS so the daemon picks up changes live) is Sprint 46; tier-1 ships now because it's one line and eliminates the silent-misroute surprise for anyone who installs from a Sprint 44 binary.

**Priority:** P2

- [ ] After `println!("Mailbox '{name}' created.")` in `src/mailbox.rs`, print a follow-up hint line pointing the operator at `sudo systemctl restart aimx` (or the OpenRC equivalent) to activate the new mailbox; use the existing `SystemOps` abstraction if it exposes a service-manager hint, otherwise hard-code systemd-first wording with a note about OpenRC
- [ ] Same hint printed after `Mailbox '{name}' deleted.`
- [ ] Existing `src/mailbox.rs` tests updated to assert on the hint's presence; new test for the delete path
- [ ] `book/mailboxes.md` documents the restart requirement so the hint isn't surprising; note Sprint 46 will remove it
- [ ] `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt -- --check` clean

#### S44-5: `docs/manual-test.md` — specify "compose new" for email steps

**Context:** Finding #3 from the manual test run (P4, docs-only). Testers forwarded/replied to earlier messages in T3/T5/T8/T9, producing `Fwd:`/`Re:` subjects and `in_reply_to`/`references` headers that added noise to the result log. Plan wording didn't specify compose-new vs. reply-to-thread. Trivial docs fix.

**Priority:** P3

- [ ] T3, T5, T8, T9 steps in `docs/manual-test.md` updated to specify "compose a new email" rather than "send a test email", with an explicit note against forwarding/replying to prior threads for clean frontmatter
- [ ] No code changes; `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt -- --check` still clean

---

## Sprint 45 — Strict Outbound + MCP Writes via Daemon (Days 125.5–128) [NOT STARTED]

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

- [ ] `src/send_protocol.rs`: remove `From-Mailbox:` from the SEND request encoder and parser; pre-launch, no compat shim
- [ ] `src/send_handler.rs handle_send_inner`: parse `From:` from the raw message, call `resolve_from_mailbox(&self.config, &from)`; on miss or domain mismatch, return `AIMX/1 ERR <code> …` per FR-18c code set (threaded with S45-2)
- [ ] `src/send.rs run`: delete the `Config::load` / `resolve_from_mailbox` call path; client only composes the raw message and opens UDS
- [ ] `src/main.rs`: drop the config-load step before `send::run` dispatch (send becomes a path that needs no config file access)
- [ ] `src/setup.rs`: confirm `/etc/aimx/config.toml` install mode is `0640 root:root` (manual-test workaround is obsolete once the client doesn't read it)
- [ ] `src/send.rs` unit tests for mailbox resolution move to `src/send_handler.rs`
- [ ] New integration test: run `aimx send` as a non-root user against a `0640` config; assert success and verify the client never opens the config file (strace-style check optional; at minimum assert the Permission denied error from the manual-test session no longer reproduces)
- [ ] `book/mailboxes.md` and `CLAUDE.md` updated — `aimx send` is no longer documented as reading config
- [ ] `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt -- --check` clean

#### S45-2: Strict outbound — concrete mailbox + configured domain only

**Context:** Finding #5 from the manual test run + user clarification 2026-04-18 (PRD FR-18d tightened). `resolve_from_mailbox` currently falls back to the wildcard catchall (`*@domain`), so `aimx send --from bogus@domain` succeeds and lands in `sent/catchall/`. Catchall is inbound-routing only; outbound must name a concrete, configured mailbox. User-added constraint: From domain must equal `config.domain` — no sending from a domain aimx isn't authorized for (no DKIM key exists for foreign domains anyway; reject early with a clear error instead of letting the signer fail obliquely). PRD FR-18d already carries the updated semantics after the 2026-04-18 edit; this story enforces them in code.

**Priority:** P0

- [ ] `src/send.rs resolve_from_mailbox` (or its new home in `src/send_handler.rs` after S45-1): delete the wildcard fallback branch (`mb.address.starts_with('*')`)
- [ ] Before the mailbox lookup, explicitly verify `From:` domain (case-insensitive) equals `config.domain`; on mismatch return `AIMX/1 ERR DOMAIN sender domain '<x>' does not match aimx domain '<config.domain>'`
- [ ] Mailbox-miss path returns `AIMX/1 ERR MAILBOX no mailbox matches From: <addr>` with guidance pointing at `aimx mailbox create`
- [ ] `book/mailboxes.md` documents the inbound-only semantics of catchall and the concrete-mailbox requirement for outbound; remove any prior implication that catchall can sign outbound
- [ ] `book/channels.md` cross-reference updated if it referenced the old wildcard behavior
- [ ] Existing tests that asserted wildcard outbound success are flipped to assert the ERR path
- [ ] New tests: foreign-domain From (rejected with DOMAIN error); concrete-mailbox send under the configured domain (succeeds); bogus local-part under the configured domain (rejected with MAILBOX error); case-insensitive domain match (`From: x@Agent.Example.Com` matches `domain = "agent.example.com"`)
- [ ] `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt -- --check` clean

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

- [ ] Request parser recognises three verbs (`SEND`, `MARK-READ`, `MARK-UNREAD`) and produces a tagged enum; unknown verb returns `ERR PROTOCOL unknown verb '<x>'`
- [ ] Writer helpers mirror `write_request` for each new verb (client side), with typed argument structs
- [ ] Response codes stay in the FR-18c set (`OK`, `ERR` with codes from `MAILBOX | DOMAIN | SIGN | DELIVERY | TEMP | MALFORMED | PROTOCOL`); `PROTOCOL` added for codec-level failures
- [ ] Codec unit tests per new verb: happy-path round-trip, malformed header lines, missing required headers (`Mailbox`, `Id`, `Folder`), unknown `Folder` value, empty-body requirement enforced
- [ ] Optional file rename to `src/uds_protocol.rs` (update `mod.rs` and all imports if done)
- [ ] `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt -- --check` clean

#### S45-4: MCP write ops route through daemon; per-mailbox concurrency guard

**Context:** Finding #8 from the manual test run (P1, but demoted to P0 because it leaves MCP effectively read-only as non-root). `src/mcp.rs set_read_status` (called by `email_mark_read`) does `std::fs::write(&filepath, …)` directly → fails with EACCES because the MCP server runs as the invoking non-root user and mailbox files are `root:root 0644`. Route all write ops through the daemon via the MARK verbs from S45-3. Read ops (`email_list`, `email_read`) continue to read files directly — files are world-readable by design.

**Priority:** P0

- [ ] `src/mcp.rs email_mark_read`: become a thin UDS client — open `/run/aimx/send.sock`, send `MARK-READ`, parse `AIMX/1 OK` / `AIMX/1 ERR <reason>`, surface helpful errors (e.g. "aimx daemon not running — start with `sudo systemctl start aimx`")
- [ ] `src/mcp.rs email_mark_unread`: same pattern via `MARK-UNREAD`
- [ ] New `src/state_handler.rs` (or extend `src/send_handler.rs` — judgment call) with `handle_mark_read`, `handle_mark_unread` implementations that do the actual frontmatter rewrite, reusing the existing frontmatter serializer
- [ ] Daemon acquires a per-mailbox `RwLock<()>` for the duration of the frontmatter rewrite; stored on the daemon state (keyed by mailbox name, lazily-inserted); ingest's append path also takes the same lock so MARK-READ and inbound ingest on the same mailbox cannot interleave a half-written file
- [ ] ERR paths covered: mailbox not configured, id not found, folder invalid, write failure
- [ ] Integration test: `email_mark_read` invoked as non-root succeeds; frontmatter `read = true` is persisted; file retains its original ownership (root:root 0644)
- [ ] Integration test: concurrent ingest + `MARK-READ` on the same mailbox don't corrupt either file (use tokio `tokio::join!` or spawn pair)
- [ ] `book/mcp.md` mentions the daemon-mediated write path so users understand why `aimx serve` must be running for MCP writes
- [ ] `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt -- --check` clean

---

## Sprint 46 — Mailbox CRUD via UDS (Daemon Picks Up Changes Live) (Days 128–130.5) [NOT STARTED]

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

- [ ] `src/send_protocol.rs` (or `uds_protocol.rs`): add `MAILBOX-CREATE` verb parser + writer
- [ ] `src/state_handler.rs` `handle_mailbox_create`: validate name; read current config.toml; append stanza; write-temp-then-rename to atomically update disk; create the two directories; swap `RwLock<Arc<Config>>`; return `AIMX/1 OK` on success; on any validation or IO failure return a typed `ERR`
- [ ] `src/mailbox.rs create`: attempt UDS `MAILBOX-CREATE` first; on socket-missing (`ENOENT`/`ECONNREFUSED`/`EACCES`) fall back to direct `config.toml` edit + restart-hint print (Sprint 44 behavior); when UDS succeeds, suppress the restart hint
- [ ] The rest of the daemon (send handler, ingest path) reads Config via the `RwLock<Arc<Config>>` accessor — verify all existing `config.mailboxes.get(…)` call sites thread through correctly
- [ ] Integration test: daemon running → `aimx mailbox create foo` via UDS → immediately send Gmail to `foo@domain` → assert the .md lands in `inbox/foo/` (not catchall), no restart required
- [ ] Integration test: daemon stopped → `aimx mailbox create foo` falls back to direct config edit + restart-hint present in stdout
- [ ] Integration test: concurrent `MAILBOX-CREATE foo` + inbound mail targeting a pre-existing mailbox — neither blocks the other for longer than the write-lock critical section (~microseconds)
- [ ] Name-validation tests: `..`, empty string, `/`-containing, duplicate name (already exists) — each returns a distinct `ERR` with the reason
- [ ] `book/mailboxes.md` updated — restart is no longer required when daemon is running
- [ ] `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt -- --check` clean

#### S46-2: UDS `MAILBOX-DELETE` — safety check + daemon swap

**Context:** Closes finding #1 tier-2 for the delete path. Symmetric to S46-1 but with a safety check: refuse to delete a mailbox whose `inbox/<name>/` or `sent/<name>/` still contains files. Operator must archive or manually remove the files first (matches current CLI semantics per `src/mailbox.rs`). When UDS succeeds, daemon removes the `[mailboxes.<name>]` stanza from config.toml and swaps its in-memory `Config`. Directories are left on disk (operator owns cleanup) — safer than silently deleting files.

**Priority:** P1

- [ ] `src/send_protocol.rs` (or `uds_protocol.rs`): add `MAILBOX-DELETE` verb parser + writer
- [ ] `src/state_handler.rs` `handle_mailbox_delete`: verify mailbox exists; scan `inbox/<name>/` and `sent/<name>/` for any files — if non-empty return `AIMX/1 ERR NONEMPTY mailbox <name> has <n> files; archive or remove them first`; on success remove the stanza via write-temp-then-rename and swap `Config`
- [ ] `src/mailbox.rs delete`: attempt UDS `MAILBOX-DELETE` first; fall back to direct edit + restart-hint when socket absent
- [ ] Refuse to delete the `catchall` mailbox via UDS (matches existing CLI guardrail); direct-edit fallback preserves whatever the current rule is
- [ ] Integration test: daemon running → create mailbox `qux` → delete via UDS → assert `[mailboxes.qux]` is gone from config.toml and the daemon rejects subsequent inbound to `qux@domain` (routes to catchall)
- [ ] Integration test: mailbox with files → `MAILBOX-DELETE` returns `ERR NONEMPTY`; operator then clears files and retry succeeds
- [ ] `book/mailboxes.md` documents the NONEMPTY safety behavior and the symmetric live-update semantics
- [ ] Sprint 44's restart-hint suppression applies to the delete path too
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
| 44 | 123–125.5 | Post-launch Security + Quick Fixes | Env-var channel-trigger expansion (shell-injection fix), DKIM DNS sanity check at daemon startup + louder setup warning, Claude Code agent-setup hint fix, `aimx mailbox create/delete` restart hint, manual-test.md compose-new clarification | Not started |
| 45 | 125.5–128 | Strict Outbound + MCP Writes via Daemon | `aimx send` stops reading config.toml (daemon resolves From), strict outbound (concrete mailbox + configured domain only, wildcard is inbound-only), UDS `MARK-READ`/`MARK-UNREAD` verbs + MCP write ops via daemon with per-mailbox RwLock | Not started |
| 46 | 128–130.5 | Mailbox CRUD via UDS (Daemon Picks Up Changes Live) | UDS `MAILBOX-CREATE`/`MAILBOX-DELETE` verbs + daemon hot-swaps `Arc<Config>`, `aimx mailbox create/delete` route through daemon first and suppress restart hint on success, directory lifecycle + NONEMPTY safety on delete | Not started |

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
