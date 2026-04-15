# AIMX — Sprint Plan

**Sprint cadence:** 2.5 days per sprint
**Team:** Solo developer with heavy AI augmentation (Claude Code)
**Total sprints:** 42 (6 original + 2 post-audit hardening + 1 YAML→TOML migration + 2 verifier/setup overhaul + 2 post-Sprint-11 bug fixes + 2 verifier ops + 1 deployment + 1 service rename + 1 setup UX + 5 embedded SMTP + 1 verify cleanup + 1 DKIM permissions fix + 1 IPv6 support + 1 systemd unit hardening + 1 CLI color consistency + 1 CI binary releases + 3 agent integration + 1 channel-trigger cookbook + 1 non-blocking cleanup + 1 scope-reversal (33.1) + 8 v0.2 pre-launch reshape)
**Timeline:** ~114.5 calendar days (v1: ~92 days, v0.2 reshape: ~22.5 days)
**v1 Scope:** Full PRD scope including verifier service. Sprint 1 targets earliest possible idea validation on a real VPS. Sprints 7–8 address findings from post-v1 code review audit. Sprints 10–11 overhaul the verifier service (remove email echo, add EHLO probe) and rewrite the setup flow (root check, MTA conflict detection, install-before-check). Sprints 12–13 fix critical bugs found during post-Sprint-11 debugging: Caddy self-probe loop / XFF SSRF risk in the verifier service, and the preflight chicken-and-egg problem on fresh VPSes. Sprints 14–15 are review-driven operational quality work on the verifier service (request logging, Docker packaging). Sprint 17 renames the verify service to verifier across all code, Docker, CI, and documentation. Sprints 19–23 replace OpenSMTPD with an embedded SMTP server (hand-rolled tokio listener for inbound, lettre + hickory-resolver for outbound) and update all documentation, making aimx a true single-binary solution with no external runtime dependencies and cross-platform Unix support. Sprint 24 cleans up `aimx verify` (EHLO-only checks, sudo requirement, remove `/reach` endpoint, AIMX capitalization). Sprint 27 hardens the generated systemd unit with restart rate-limiting, resource limits, and network-readiness dependencies. Sprint 27.5 unifies user-facing CLI output under a single semantic color palette. (Sprint 27.6 — CI binary release workflow — is deferred to the Non-blocking Review Backlog until we're production-ready.) Sprints 28–30 ship per-agent integration packages (Claude Code, Codex CLI, OpenCode, Gemini CLI, Goose, OpenClaw) plus the `aimx agent-setup <agent>` installer that drops a plugin/skill/recipe into the agent's standard location without mutating its primary config. Sprint 31 adds a channel-trigger cookbook covering email→agent invocation patterns for every supported agent. Sprint 32 is a non-blocking cleanup sprint consolidating review feedback across v1.

**v0.2 Scope (pre-launch reshape, Sprints 33–40 + 33.1 scope-reversal):** Five tightly-coupled themes that reshape AIMX into its launch form. Sprint 33 splits the filesystem (config + DKIM secrets to `/etc/aimx/`, data stays at `/var/lib/aimx/` but world-readable). Sprint 33.1 (scope reversal, inserted after Sprint 33 merged) drops PTR/reverse-DNS handling (operator responsibility, out of aimx scope) and drops the `aimx` system group introduced in S33-4 — authorization on the UDS send socket is explicitly out of scope for v0.2 and the socket becomes world-writable (`0o666`). Sprints 34–35 shrink the trust boundary: DKIM signing and outbound delivery move inside `aimx serve`, exposed to clients over a world-writable Unix domain socket at `/run/aimx/send.sock`; the DKIM private key becomes root-only (`600`) and is never read by non-root processes. Sprint 36 reshapes the datadir (`inbox/` vs `sent/` split per mailbox, `YYYY-MM-DD-HHMMSS-<slug>.md` filenames with a deterministic slug algorithm, Zola-style attachment bundles). Sprint 37 expands the inbound frontmatter schema (new fields: `thread_id`, `received_at`, `received_from_ip`, `size_bytes`, `delivered_to`, `list_id`, `auto_submitted`, `dmarc`, `labels`) and adds DMARC verification. Sprint 38 surfaces the per-mailbox trust evaluation as a new always-written `trusted` frontmatter field (the v1 per-mailbox trust model — `trust: none|verified` + `trusted_senders` — is preserved unchanged; `trusted` is the *result*, not a new *policy*) and persists sent mail with a full outbound block. Sprint 39 restructures the shared agent primer into a progressive-disclosure skill bundle (`agents/common/aimx-primer.md` + `references/`), standardizes author metadata to `U-Zyn Chua <chua@uzyn.com>`, and reverses an earlier draft's storage-layout redaction policy. Sprint 40 ships the baked-in `/var/lib/aimx/README.md` agent-facing layout guide (versioned via `include_str!`, refreshed on `aimx serve` startup when the version differs), replaces stale `/var/log/aimx.log` references with `journalctl -u aimx`, and brings every affected `book/` chapter and `CLAUDE.md` up to date. No migration tooling is written — v0.2 ships pre-launch, with no existing installs to upgrade.

---


## Sprint Archive

Completed sprints 1–21 have been archived for context window efficiency.

| Archive | Sprints | File |
|---------|---------|------|
| 1 | 1–8 | [`sprint.1.md`](sprint.1.md) |
| 2 | 9–21 | [`sprint.2.md`](sprint.2.md) |

---

## Sprint 22 — Remove OpenSMTPD + Cross-Platform CI (Days 61–63.5) [DONE]

**Goal:** Strip all OpenSMTPD-specific code from setup, status, and verify. Add Alpine and Fedora to CI matrix.

**Dependencies:** Sprint 21 (`aimx serve` is the replacement)

### S22.1 — Simplify setup.rs

**Context:** `setup.rs` currently has ~600 lines dedicated to OpenSMTPD: `install_package()` (apt-get), `debconf_preseed()` (debconf-set-selections), `generate_smtpd_conf()`, `configure_opensmtpd()`, `Port25Status::OpenSmtpd`/`OtherMta` variants, and ~20 associated tests. All of this is replaced by: generate the systemd/OpenRC service file (from S21.3), write it to disk, enable and start the service. The `SystemOps` trait loses `is_package_installed`, `install_package`, `debconf_preseed` and gains `install_service_file`. `check_port25_occupancy` stays but simplifies — any process on port 25 that isn't aimx is a conflict. Re-entrant detection (S18.4) checks for the aimx service instead of OpenSMTPD. The setup UX stays the same: `sudo aimx setup <domain>` → generates config, DKIM keys, TLS certs, service file → starts `aimx serve` → displays DNS records.

**Priority:** P0

- [x] Remove: `install_package()`, `debconf_preseed()`, `generate_smtpd_conf()`, `configure_opensmtpd()`
- [x] Remove: `Port25Status::OpenSmtpd` and `Port25Status::OtherMta` — replace with `Port25Status::Aimx` and `Port25Status::OtherProcess(String)`
- [x] Remove `is_package_installed` from `SystemOps` trait
- [x] Add `install_service_file` to `SystemOps` trait — writes systemd unit or OpenRC script and enables/starts the service
- [x] Setup flow: generate TLS cert → generate DKIM keys → install service file → start `aimx serve` → verify port 25 → display DNS
- [x] Port 25 checks in setup: update error message from "OpenSMTPD has been installed but port 25 is not reachable" to "aimx serve started but port 25 is not reachable"
- [x] MTA conflict in setup: replace OpenSMTPD-specific prompt ("Setup will overwrite /etc/smtpd.conf") with generic "Port 25 is occupied by {name}" error
- [x] Re-entrant detection: check if aimx service is already running (instead of OpenSMTPD + smtpd.conf + debconf)
- [x] Remove `NetworkOps` docstrings referencing OpenSMTPD: "Used by `aimx verify` on a fresh VPS before OpenSMTPD is installed" (line 42-43)
- [x] Update `MockSystemOps`: remove package/debconf mocks, add service file mock
- [x] Remove all OpenSMTPD-related tests (~20 tests); add tests for new service file flow
- [x] `cargo test` passes with no dead code or unused import warnings

### S22.2 — Update status.rs + verify.rs

**Context:** `status.rs` checks `systemctl is-active --quiet opensmtpd` and displays "OpenSMTPD: running/stopped." Change to check aimx service. `verify.rs` currently has a three-way branch on `Port25Status` with significant issues: the `OpenSmtpd` branch calls `check_inbound(net)` twice (redundant — lines 68-93 both call the same EHLO probe), and the `Free` branch requires root to bind a throwaway `TcpListener` on port 25 just to test reachability via `/reach`. With embedded SMTP, the verify flow simplifies dramatically:

- `Port25Status::Aimx` → outbound check + single inbound EHLO probe (via `/probe`). Done.
- `Port25Status::OtherProcess(name)` → error: port 25 occupied by something else.
- `Port25Status::Free` → no temporary listener hack needed. Just tell the user: "aimx serve is not running. Run `sudo aimx setup` or `sudo systemctl start aimx`." No root requirement for `aimx verify`.

**Priority:** P0

- [x] `status.rs`: rename `opensmtpd_running` field → `smtp_running`
- [x] `status.rs`: check `systemctl is-active --quiet aimx` (or port 25 bound by aimx process)
- [x] `status.rs`: display "SMTP server: running" instead of "OpenSMTPD: running"
- [x] `verify.rs`: collapse three-way branch into: `Aimx` (outbound + single EHLO probe), `OtherProcess` (error), `Free` (advise to start aimx serve)
- [x] `verify.rs`: remove duplicate inbound check — currently `check_inbound` is called twice in the OpenSMTPD path; the new `Aimx` path does it once
- [x] `verify.rs`: remove temporary `TcpListener` hack (line 121) and root requirement — `aimx verify` no longer needs root
- [x] `verify.rs`: remove `is_root()` function — no longer needed
- [x] `verify.rs`: update all user-facing messages: remove "OpenSMTPD" references, use "aimx serve" / "SMTP server"
- [x] Update all test fixtures that reference `opensmtpd_running`
- [x] Update verify tests: remove `verify_opensmtpd_*` tests, add `verify_aimx_*` equivalents; remove `verify_free_requires_root` test; add test for `Free` path showing advisory message
- [x] All status/verify tests pass with updated field names and simplified flow

### S22.3 — Cross-Platform CI

**Context:** With OpenSMTPD removed, aimx should compile and test on non-Debian Linux. Add two CI targets: Alpine Linux (musl libc — tests portability to non-glibc) and Fedora (tests RPM-based distros). Use Docker containers in GitHub Actions. These run `cargo build`, `cargo test`, `cargo clippy` — same checks as the existing Ubuntu CI. Start as informational (`continue-on-error: true`), promote to required once stable.

**Priority:** P1

- [x] Add Alpine Linux CI job: `rust:alpine` Docker image, install build deps (musl-dev, openssl-dev or use rustls), run `cargo build && cargo test && cargo clippy -- -D warnings`
- [x] Add Fedora CI job: `fedora:latest` Docker image, install `rust cargo clippy rustfmt`, run same checks
- [x] CI matrix in `.github/workflows/ci.yml` includes: Ubuntu (existing), Alpine (new), Fedora (new)
- [x] Both new targets are `continue-on-error: true` initially (informational, not blocking)
- [x] Fix any compilation issues discovered on Alpine/Fedora (if any — likely musl-related)

---

## Sprint 23 — Documentation + PRD Update (Days 64–66.5) [DONE]

**Goal:** Update all documentation to reflect the embedded SMTP architecture. Update the PRD to formalize the NFR and FR changes. Clean up obsolete backlog items.

**Dependencies:** Sprint 22 (all code changes complete)

### S23.1 — Update PRD

**Context:** The PRD references OpenSMTPD in NFR-1, NFR-2, NFR-4, and functional requirements FR-1b, FR-2, FR-3, FR-11, FR-19, FR-41b, FR-43. Also the Architecture section (§8), Risks table (§10), and Scope (§9). All need updating to reflect: no external runtime dependencies, `aimx serve` as the daemon, cross-Unix portability. This is a targeted edit — update the specific sections, don't rewrite the whole PRD.

**Priority:** P0

- [x] NFR-1: "No runtime dependencies beyond OpenSMTPD" → "No runtime dependencies. Single self-contained binary"
- [x] NFR-2: "No daemon" → "`aimx serve` is the SMTP daemon. All other commands remain short-lived"
- [x] NFR-4: "Linux only. Target Debian/Ubuntu" → "Any Unix where Rust compiles and port 25 is available. CI tests Ubuntu, Alpine, Fedora"
- [x] FR-1b: Remove OpenSMTPD conflict detection — replace with generic port 25 conflict check
- [x] FR-2: "Install and configure OpenSMTPD" → "Start embedded SMTP listener via systemd/OpenRC service"
- [x] FR-11: "Accept raw .eml from OpenSMTPD via stdin" → "Accept raw email from embedded SMTP listener (or stdin for manual use)"
- [x] FR-19: "Hand signed message to OpenSMTPD" → "Deliver via direct SMTP to recipient's MX server"
- [x] FR-41b: Remove debconf pre-seeding — replace with service file installation
- [x] FR-43: "called by OpenSMTPD" → "called by aimx serve or via stdin"
- [x] §8 Architecture: replace OpenSMTPD references with `aimx serve` and direct SMTP delivery
- [x] §10 Risks: replace "OpenSMTPD configuration complexity" with embedded SMTP risks
- [x] §9 Scope: update "In Scope" to reflect new architecture

### S23.2 — Update CLAUDE.md + README

**Context:** CLAUDE.md is the primary codebase orientation file — it currently says "OpenSMTPD handles SMTP" and describes each module in terms of OpenSMTPD. README.md has architecture diagrams and requirements listing Debian/Ubuntu. Both need targeted updates to reflect the new single-binary, no-external-dependency architecture.

**Priority:** P0

- [x] CLAUDE.md line 7: "OpenSMTPD handles SMTP" → "Built-in SMTP server handles inbound; direct SMTP delivery for outbound"
- [x] CLAUDE.md setup.rs description: remove debconf/OpenSMTPD, add service file generation
- [x] CLAUDE.md ingest.rs: "called by OpenSMTPD MDA" → "called by aimx serve or via stdin"
- [x] CLAUDE.md send.rs: "hands to `/usr/sbin/sendmail`" → "delivers via direct SMTP to recipient's MX"
- [x] CLAUDE.md conventions: "No aimx daemon" → "`aimx serve` is the SMTP daemon"
- [x] CLAUDE.md: add `serve.rs` and `smtp.rs` module descriptions
- [x] README.md: update architecture, requirements, setup instructions

### S23.3 — Update book/

**Context:** The user guide in `book/` (8 files) references OpenSMTPD throughout: setup instructions mention apt install, troubleshooting says `journalctl -u opensmtpd`, getting-started lists OpenSMTPD as a dependency. Replace all with `aimx serve` equivalents. The setup guide simplifies significantly — no package installation step.

**Priority:** P0

- [x] `book/setup.md`: remove apt/OpenSMTPD install steps, describe `aimx setup` generating service file and starting `aimx serve`
- [x] `book/getting-started.md`: remove OpenSMTPD from prerequisites, simplify to "download aimx binary, run setup"
- [x] `book/troubleshooting.md`: `journalctl -u opensmtpd` → `journalctl -u aimx`, update common issues
- [x] `book/index.md`: update architecture overview
- [x] `book/configuration.md`: add `aimx serve` config options (bind address, TLS paths) if applicable
- [x] Grep for "opensmtpd", "smtpd", "sendmail" across all `book/*.md` — ensure none remain

### S23.4 — Clean Up Backlog + Summary Table

**Context:** The Non-blocking Review Backlog has items that reference OpenSMTPD and are now obsolete. The Summary Table needs 5 new rows. The Deferred to v2 table references OpenSMTPD defaults. Update all of these to reflect the new architecture.

**Priority:** P1

- [x] Mark backlog item "Quote data dir path in `generate_smtpd_conf`" (Sprint 8) as obsolete — function removed
- [x] Mark backlog item "`parse_port25_status` uses `smtpd` substring match" (Sprint 11) as obsolete — logic replaced
- [x] Mark backlog item "`is_already_configured` uses `c.contains(domain)` substring match for smtpd.conf" (Sprint 18) as obsolete — smtpd.conf no longer generated
- [x] Update "Deferred to v2" entry for rate limiting: "Rely on OpenSMTPD defaults + DMARC" → "Rely on DMARC policy for v1"
- [x] Update "Deferred to v2": remove "Non-Linux platforms" row (now supported via NFR-4 update)
- [x] Update Summary Table with Sprints 19–23
- [x] Update sprint file header: total sprints, timeline, scope description

---

## Sprint 24 — Verify Cleanup + Sudo Requirement (Days 67–69.5) [DONE]

**Goal:** Simplify `aimx verify` to use EHLO-only checks (no TCP-only reachability), require root, remove the `/reach` endpoint from the verifier service, and fix AIMX capitalization across user-facing output.

**Dependencies:** Sprint 23 (all prior work complete)

### S24.1: Switch outbound check from TCP connect to EHLO handshake

**Context:** The outbound port 25 check currently does a bare TCP connect to `check.aimx.email:25` (the verifier's port 25 listener). Since the verifier keeps its port 25 listener and already responds to EHLO, the outbound check should perform an EHLO handshake instead of a dumb TCP connect — this is a more meaningful test that proves SMTP works, not just that a socket is open. Update `check_outbound_port25()` in `RealNetworkOps` to perform an EHLO exchange rather than `TcpStream::connect_timeout`. The verifier's port 25 listener already responds to EHLO so no server-side changes are needed for this story.

**Priority:** P0

- [x] `check_outbound_port25()` performs SMTP EHLO handshake (connect, read 220 banner, send EHLO, read 250, send QUIT) instead of bare TCP connect
- [x] Timeout remains reasonable (10–15s total for the handshake)
- [x] Existing tests updated to reflect new behavior
- [ ] `aimx verify` outbound check passes against real `check.aimx.email:25` (manual VPS validation) <!-- Deferred: requires VPS with port 25; not testable in CI -->

### S24.2: Remove `/reach` endpoint from verifier service

**Context:** The `/reach` endpoint in `services/verifier/` performs a plain TCP connect to the caller's port 25 — a weaker check than `/probe` (EHLO handshake). With outbound now tested via EHLO against the verifier's own port 25, `/reach` serves no purpose. Remove it from the verifier's HTTP router, handler code, tests, README, and any references in the main `aimx` crate (setup.rs mentions `/reach` in comments, `curl_reachable` is shared between `/probe` and `/reach`). Also remove FR-38's `/reach` description and mark FR-39b as obsolete in the PRD.

**Priority:** P0

- [x] `/reach` HTTP handler and route removed from `services/verifier/src/main.rs`
- [x] Any tests for `/reach` removed or updated
- [x] `services/verifier/README.md` updated — no mention of `/reach`
- [x] `curl_reachable()` in `setup.rs` renamed to `curl_probe()` now that it only serves `/probe`
- [x] Grep for `reach` across entire codebase — remove stale references in comments, docs, `book/`, PRD
- [x] FR-38 in PRD updated: remove `/reach` description
- [x] FR-39b in PRD marked obsolete or removed

### S24.3: Require sudo for `aimx verify`

**Context:** `aimx verify` spawns a temp SMTP listener on port 25 when `aimx serve` isn't running, which requires root. Rather than failing with a confusing bind error, require root upfront — consistent with `aimx setup`. The port 25 detection logic stays the same: if aimx is on port 25 → use it; if free → spawn temp listener; if another process → show error with process name. The error message for `OtherProcess` should read exactly: `Port 25 is occupied by \`{name}\`.\nStop or uninstall the process and run \`sudo aimx verify\` again to check.`

**Priority:** P0

- [x] `aimx verify` checks for root at entry (reuse pattern from `aimx setup`) and exits with clear message if not root
- [x] Port 25 detection flow unchanged: `Aimx` → run checks, `Free` → spawn temp listener + run checks, `OtherProcess(name)` → error
- [x] `OtherProcess` error message matches exact wording: `Port 25 is occupied by \`{name}\`.\nStop or uninstall the process and run \`sudo aimx verify\` again to check.`
- [x] FR-48 in PRD updated: remove "No root requirement", add "Requires root"
- [x] Tests updated: add root-check test (mock pattern via refactored `run_verify()` accepting `&dyn SystemOps`), update existing tests as needed
- [x] `book/` and README references to `aimx verify` updated to show `sudo aimx verify`

### S24.4: Fix AIMX capitalization in user-facing output

**Context:** "AIMX" should be capitalized when referring to the product/project. `aimx` (backtick or code-formatted) when referring to the CLI command. Audit all user-facing strings in `src/`, `book/`, `README.md`, and the verifier service. Do not change code identifiers, crate names, binary names, or config keys — only human-readable text (println!, eprintln!, error messages, docs, comments visible to users).

**Priority:** P1

- [x] Audit `src/*.rs` println/eprintln/error strings — fix "aimx" → "AIMX" where it refers to the product (e.g., "Your system is good for AIMX setup")
- [x] Audit `book/*.md` — fix product references to "AIMX", keep command references as `aimx`
- [x] Audit `README.md` — same pattern
- [x] Audit `services/verifier/` user-facing strings and README
- [x] Do NOT rename crate, binary, module, function, or config identifiers
- [x] Audit all `*.md` documentation files (`docs/`, `CLAUDE.md`, etc.) — fix product references to "AIMX" (15 files, 46 lines)

---

## Sprint 25 — Fix `aimx send` (Permissions + DKIM Signing) (Days 70–72.5) [DONE]

**Goal:** Fix the two bugs preventing `aimx send` from working: (1) DKIM private key is unreadable by non-root users, and (2) DKIM-signed emails fail verification at Gmail, causing DMARC rejection.

**Dependencies:** Sprint 24

**Testing environment:** This machine (`vps-198f7320`) has `agent.zeroshot.lol` fully configured with DKIM keys, DNS records (MX, SPF, DKIM, DMARC all verified), and `aimx serve` running. Use `sudo aimx send --from hello@agent.zeroshot.lol --to <recipient> --subject <subject> --body <body>` to test live delivery. DNS DKIM record is correctly split into two TXT strings by the provider; public key in DNS matches local key at `/var/lib/aimx/dkim/public.key`. The developer has sudo access on this machine.

#### S25-1: Make DKIM private key globally readable

**Context:** `generate_keypair()` in `dkim.rs` sets the private key to mode `0o600` (owner-only). Since `aimx setup` runs as root, the key becomes `root:root 0600`. Non-root users (agents, MCP, CLI) can't read it, so `aimx send` fails with a misleading "DKIM private key not found" error. The actual error is permission denied, but `load_private_key()` swallows the IO error. The fix: set the key to `0o644` (globally readable) since all local users need DKIM signing access for direct outbound delivery, and the key is only used for email signing (not authentication). Also fix the error message in `load_private_key` to include the actual IO error so permission vs not-found issues are distinguishable.

**Priority:** P0

- [x] Change `dkim.rs` `generate_keypair()` permission from `0o600` to `0o644`
- [x] Update `load_private_key()` to include the actual IO error in the error message (e.g., "DKIM private key not found at {path}: {error}. Run `aimx dkim-keygen` first.")
- [x] Update the existing permission test (`private_key_has_restricted_permissions`) to expect `0o644`
- [x] Add integration test: generate keypair, verify file mode is `0o644`
- [ ] Verify `aimx send` works without sudo after `sudo aimx setup` on a real system <!-- Deferred: requires live VPS validation -->

#### S25-2: Fix DKIM signature verification failure at Gmail

**Context:** `sudo aimx send` to Gmail is rejected with `5.7.26 Unauthenticated email from agent.zeroshot.lol ... DMARC policy`. DNS is confirmed correct (DKIM key in DNS matches local key, SPF/DMARC/MX all verified, DNS provider correctly splits the TXT record into two strings). The issue is in the signing code itself. Investigation ruled out: DNS truncation (provider splits correctly), `mail_auth` version bugs (v0.7.5 is clean), and canonicalization defaults (`relaxed/relaxed` is correct). Remaining suspects: (1) `args.body` may contain bare `\n` instead of `\r\n`, causing body hash mismatch after Gmail normalizes during verification; (2) the existing `sign_and_verify_roundtrip` test only checks DKIM-Signature header presence — it does not verify the signature cryptographically, so signing bugs go undetected. Test on this machine using `sudo aimx send --from hello@agent.zeroshot.lol --to <recipient> --subject Test --body "Test"`.

**Priority:** P0

- [x] Diagnose: capture raw signed message output and inspect DKIM-Signature header fields (bh=, b=, c=, d=, s=); send to a DKIM analysis tool to identify whether failure is body hash, header hash, or key lookup
- [x] Ensure CRLF normalization: verify `compose_message()` output has consistent `\r\n` throughout, including user-supplied `args.body`; normalize bare `\n` to `\r\n` before signing if needed
- [x] Explicitly set `relaxed/relaxed` canonicalization on `DkimSigner` (protects against upstream default changes)
- [x] Add cryptographic roundtrip test: sign a message, then verify the signature using the public key (not just check header presence)
- [ ] Verify end-to-end: `aimx send` from `agent.zeroshot.lol` delivers to Gmail with DKIM pass <!-- Deferred: requires live VPS validation -->

---

## Sprint 26 — IPv6 Support for Outbound SMTP (Days 73–75.5) [DONE]

> **Follow-up addendum (post-merge):** A later PR (`enable-ipv6-config-flag`)
> flipped the default back to IPv4-only and made IPv6 outbound opt-in via a
> new `enable_ipv6` bool in `config.toml`. The Sprint 26 ACs below still
> describe the original "OS chooses the family" behaviour that shipped when
> this sprint merged; the current shipped default is IPv4-only, and the
> dual-stack SPF / AAAA guidance is only emitted by `aimx setup` when
> `enable_ipv6 = true`. See PRD FR-7, FR-19, resolved-decision #8 and
> `book/configuration.md` "IPv6 delivery (advanced)" for the current
> behaviour.

**Goal:** Remove the IPv4-only workaround from outbound delivery and properly support IPv6 across SPF records, DNS guidance, and verification. The IPv4 preference was added in Sprint 25 as a workaround for SPF failures — now that DKIM is fixed, let the OS resolve addresses naturally and ensure SPF covers both address families.

**Dependencies:** Sprint 25

**Testing environment:** Same as Sprint 25. Use `sudo aimx send --from hello@agent.zeroshot.lol --to chua@uzyn.com --subject Hey --body "Test"` to verify delivery works over whichever address family the OS selects.

#### S26-1: Remove IPv4-only outbound restriction

**Context:** `resolve_ipv4()` in `send.rs:95-104` forces all outbound SMTP connections through IPv4 by filtering DNS results for A records only. This was a workaround for SPF failures when connecting over IPv6 (Sprint 25, commit 47168f8). Now that DKIM signing is correct, the restriction should be removed — let the OS decide which address family to use when connecting to MX servers. Remove `resolve_ipv4()` and the `connect_target` override in `try_deliver()`, so `lettre` connects directly to the MX hostname.

**Priority:** P0

- [x] Remove `resolve_ipv4()` function from `send.rs`
- [x] Remove `connect_target` logic in `try_deliver()` — pass `host` directly to `SmtpTransport::builder_dangerous()`
- [x] Verify existing tests still pass
- [ ] Live test: `sudo aimx send --from hello@agent.zeroshot.lol --to chua@uzyn.com --subject Hey --body "Test"` delivers successfully

#### S26-2: Add IPv6 to DNS guidance and SPF record

**Context:** `setup.rs` detects the server IP via `hostname -I` (which returns both IPv4 and IPv6 addresses) but only uses the first address. The generated SPF record (`v=spf1 ip4:{server_ip} -all`) and DNS guidance only cover IPv4. When the OS connects to a recipient MX over IPv6, SPF fails because the server's IPv6 address isn't in the record. Fix: detect both IPv4 and IPv6 addresses from `hostname -I`, generate SPF with both `ip4:` and `ip6:` mechanisms, add an AAAA record to the DNS guidance, and pass both addresses through the setup flow.

**Priority:** P0

- [x] `get_server_ip()` (or new helper) returns both IPv4 and IPv6 addresses from `hostname -I`
- [x] `generate_dns_records()` produces SPF record with both `ip4:` and `ip6:` when IPv6 is available (e.g., `v=spf1 ip4:X.X.X.X ip6:2001:db8::1 -all`)
- [x] `generate_dns_records()` includes an AAAA record when IPv6 address is available
- [x] `display_dns_guidance()` shows the AAAA record to the user
- [x] Tests updated for dual-stack DNS record generation

#### S26-3: Add `ip6:` support to SPF verification

**Context:** `spf_contains_ip()` in `setup.rs:569-582` only checks `ip4:` mechanisms — this is the open backlog item from Sprint 8. Add `ip6:` mechanism support so that `verify_spf()` correctly validates SPF records containing IPv6 addresses. Also update `verify_all_dns()` to verify SPF against both the server's IPv4 and IPv6 addresses when both are present.

**Priority:** P0

- [x] `spf_contains_ip()` also checks `ip6:` and `+ip6:` prefixes
- [x] `verify_spf()` can verify against IPv6 addresses
- [x] `verify_all_dns()` checks SPF for both IPv4 and IPv6 when both are available
- [x] Unit tests: SPF pass/fail/missing for `ip6:` mechanisms, dual-stack verification
- [x] Mark Sprint 8 backlog item "Add `ip6:` mechanism support to `spf_contains_ip()`" as triaged

---

## Sprint 27 — Systemd Unit Hardening (Days 76–78.5) [DONE]

**Goal:** Harden the systemd unit generated by `aimx setup` with proper restart rate-limiting, resource limits, and network-readiness dependencies. Systemd only at this stage — the OpenRC script stays untouched.

**Dependencies:** Sprint 26

#### S27-1: Harden `generate_systemd_unit` with restart + daemon settings

**Context:** `generate_systemd_unit()` in `src/serve.rs:101` emits a minimal unit with `Restart=on-failure` and `RestartSec=5s` but lacks restart rate-limiting (a misconfigured install could restart-loop indefinitely), resource limits (SMTP concurrency headroom), and proper network-readiness (`After=network.target` returns before DNS is usable, which matters for outbound MX resolution on cold boot). Update the template to add: `StartLimitBurst=5` + `StartLimitIntervalSec=60s` (rate-limit restarts), `LimitNOFILE=65536` + `TasksMax=4096` (resource limits), `After=network-online.target nss-lookup.target` + `Wants=network-online.target` (network readiness), and `ReadWritePaths={data_dir}` (forward-compat for future sandboxing — no-op without `ProtectSystem=`, but emitting it now avoids another rewrite later). Do NOT add `ExecReload=/bin/kill -HUP $MAINPID` — `aimx serve`'s signal handler (`src/serve.rs:77–79`) listens on SIGTERM/SIGINT only, no SIGHUP reload exists, so an `ExecReload` directive would be a lie. Do NOT add `StateDirectory=aimx` — it forces systemd to create/manage `/var/lib/aimx`, which conflicts with `--data-dir` overrides (setup already creates the data dir with correct ownership for DKIM keys). Do NOT touch `generate_openrc_script()` — OpenRC is out of scope for this sprint. Do NOT switch to a non-root user + `CAP_NET_BIND_SERVICE`; running as root stays (DKIM key ownership, port 25 binding, data-dir writes). Upgrade path for existing installations: users re-run `sudo aimx setup` — re-entrant detection in `setup.rs` already handles "aimx service already running," so no new CLI surface is needed.

**Priority:** P1

- [x] `generate_systemd_unit()` in `src/serve.rs` emits the new template with `StartLimitBurst=5`, `StartLimitIntervalSec=60s`, `LimitNOFILE=65536`, `TasksMax=4096`, `After=network-online.target nss-lookup.target`, `Wants=network-online.target`, and `ReadWritePaths={data_dir}`
- [x] `Restart=on-failure`, `RestartSec=5s`, `Type=simple`, `StandardOutput=journal`, `StandardError=journal`, and the `[Install]` section (`WantedBy=multi-user.target`) preserved
- [x] `ExecReload` NOT added (no SIGHUP handler); `StateDirectory=` NOT added (conflicts with `--data-dir`); `generate_openrc_script()` untouched — asserted positively (field content) and negatively (tests assert `!contains("ExecReload=")` and `!contains("StateDirectory=")`)
- [x] Existing test `systemd_unit_contains_required_fields` extended to assert every new field (positive + negative assertions)
- [x] Existing test `systemd_unit_custom_paths` still passes with the new template
- [x] New test `systemd_unit_readwritepaths_follows_data_dir` asserts `ReadWritePaths=` substitutes the `data_dir` argument and that the default path doesn't leak when a custom path is passed
- [x] `install_service_file()` in `src/setup.rs` still passes its existing tests — `git diff main..HEAD -- src/setup.rs` is empty
- [x] `cargo test`, `cargo clippy -- -D warnings`, `cargo fmt -- --check` all clean
- [x] `book/troubleshooting.md:47-52` documents `systemctl reset-failed aimx` for clearing a rate-limited service that hit `StartLimitBurst`
- [ ] Live validation on `vps-198f7320`: `sudo aimx setup agent.zeroshot.lol` (re-entrant), confirm `/etc/systemd/system/aimx.service` contains the new directives via `systemctl cat aimx`, `systemctl status aimx` is healthy, `systemd-analyze verify /etc/systemd/system/aimx.service` returns no warnings <!-- Deferred: requires live VPS; not CI-testable -->

---

## Sprint 27.5 — CLI Color Consistency (Days 78.5–79) [DONE]

**Goal:** Unify user-facing CLI output under a single semantic color palette so every command's success/fail/warn/info/header style matches. Extract `setup.rs`'s ad-hoc colored calls into a reusable helper module and apply it across the remaining commands.

**Dependencies:** Sprint 27.

#### S27.5-1: Extract semantic color helpers + apply across all CLI commands

**Context:** `colored = "3"` is already a dependency (`Cargo.toml:28`) and `setup.rs` uses it in ~20 places with a loose convention: `green` for PASS/success banners, `red` for FAIL, `yellow` for WARN, `bold` for section headers like `[DNS]`/`[MCP]`/`[Deliverability]`. No other command colorizes — `verify.rs`, `status.rs`, `mailbox.rs`, `send.rs`, `serve.rs`, `dkim.rs`, `channel.rs`, `smtp/*.rs`, and `main.rs` error paths all use plain `println!`/`eprintln!`, which reads visually inconsistent. Fix: introduce `src/term.rs` exposing semantic helpers (`success()`, `error()`, `warn()`, `info()`, `header()`, `highlight()`, plus badge helpers `pass_badge()`, `fail_badge()`, `warn_badge()` that return the colored "PASS"/"FAIL"/"WARN" tokens used in multiple sites). Palette is semantic only — no raw hex/RGB — so the `colored` crate's built-in auto-detection (`NO_COLOR` env var, non-TTY output) continues to disable styling on pipes and in CI. Migrate `setup.rs` to the helpers (no visual change) and apply the helpers to every remaining user-facing call site. Errors on stderr (`main.rs` top-level reporter, `Err(e)` branches in each command's `run_*` function, verify/send fail messages) use `error()` (red + bold "Error:" prefix). Section headers use `header()` (bold). Success banners (`Setup complete for ...`, `aimx serve started.`, `Email sent.`) use `success()` (green). Warnings (PTR missing, DNS pending, TLS self-signed) use `warn()` (yellow). Non-user-facing logs (tokio/tracing output, debug `eprintln!` in SMTP session handler) are left alone — they're machine-readable. Do NOT introduce a new command — `aimx check` is NOT added; verification-style output already lives in `aimx verify` and `aimx status`.

**Priority:** P1

- [x] `src/term.rs` created with public helpers: `header`, `success`, `error`, `warn`, `info`, `highlight`, `dim`, plus `pass_badge`, `fail_badge`, `warn_badge`, `missing_badge`, and `success_banner` (the extras — `dim`, `missing_badge`, `success_banner` — were added to cover the MISSING badge and the green+bold "Setup complete!" banner that the original AC list didn't enumerate)
- [x] Module documented with a doc-comment block explaining the semantic palette and the rule that raw `.green()`/`.red()`/`.bold()` calls outside this module are discouraged
- [x] `setup.rs` migrated: every colored-crate call routes through the new helpers with no visible output change (setup assertion tests still pass)
- [x] `verify.rs`: `aimx verify` output colorizes PASS/FAIL/WARN badges and the final summary line; error exits use `error()`
- [x] `status.rs`: `aimx status` colorizes section headers and status badges; mailbox table pads outside the ANSI sequence so colored columns align under both colored and `NO_COLOR` output (regression test `mailbox_table_columns_align_regardless_of_color` guards the fix)
- [x] `mailbox.rs`: `aimx mailbox create/list/delete` colorizes success/error messages; `list` colors mailbox names with `highlight()` with the same column-alignment fix as `status.rs`
- [x] `send.rs`: `aimx send` success line colored; DKIM-signing and MX-resolution errors routed through `error()`
- [x] `dkim.rs`: `aimx dkim-keygen` success message colored; key-already-exists path emits yellow `Warning:` on stderr via `term::warn` and returns `Ok(())` (exit code changed 1 → 0 for this path, disclosed in PR description)
- [x] `serve.rs`: `aimx serve` startup banner uses `header()` + `info()`; SIGTERM graceful-shutdown message uses `info()`; fatal bind errors use `error()`
- [x] `main.rs`: top-level error reporter prefixes with red+bold `Error:` via `error()`
- [x] Grep confirms no raw `.green()`/`.red()`/`.yellow()`/`.blue()`/`.bold()` calls remain OUTSIDE `src/term.rs`
- [x] `NO_COLOR` path produces no ANSI escapes — unit test `no_color_strips_ansi_from_helpers` uses `colored::control::set_override(false)` (parallel-safe; chosen over env-var manipulation)
- [x] Non-TTY detection still works — `colored` handles this by default via `is_terminal()`
- [x] `cargo test`, `cargo clippy -- -D warnings`, `cargo fmt -- --check` clean
- [x] `book/` spot-checked for inline ANSI artifacts — none found

---

## Sprint 28 — Agent Integration Framework + Claude Code (Days 79.5–82) [DONE]

**Goal:** Stand up the `agents/` tree and the `aimx agent-setup <agent>` command, and ship the Claude Code integration end-to-end as the reference implementation. Establishes the pattern all subsequent agents plug into.

**Dependencies:** Sprint 27.5. (Sprint 27.6 — CI binary releases — was deferred to the Non-blocking Review Backlog until we're production-ready; it has no code dependency on agent work.)

**Design notes (apply to all stories below):**
- `aimx agent-setup` runs as the current user. It writes to `$HOME` / `$XDG_CONFIG_HOME`-based locations only — never `/etc` or `/var`, never requires root.
- Plugin source trees live at `agents/<agent>/` in the repo and are embedded into the binary at compile time via `include_dir!` (MIT/Apache-2.0) so install works offline.
- The installer never mutates the agent's own primary config file. On success it prints the exact activation command the user should run (or a "plugin auto-discovered on next launch" hint if the agent picks it up from a known dir).
- `--force` overwrites existing destination files without prompting. `--print` writes all plugin contents to stdout and performs no disk writes (for dry-run and CI).
- Tests use `TempDir` + `HOME` override; no real agent CLI required.

#### S28-1: `agents/common/aimx-primer.md` — canonical agent-facing primer

**Context:** Before authoring any per-agent skill/recipe, AIMX needs a single canonical document describing how an LLM should think about and interact with AIMX — written for the agent, not the human operator. Each per-agent package re-wraps this primer in its native format (`SKILL.md`, Goose recipe `prompt`, OpenClaw skill, etc.) via `include_str!` at compile time so there's no drift. Content must be concrete, concise, LLM-friendly (no marketing): the nine MCP tools (`mailbox_create/list/delete`, `email_list/read/send/reply`, `email_mark_read/unread`) with parameters, the storage layout (`/var/lib/aimx/<mailbox>/YYYY-MM-DD-NNN.md`, `attachments/`), the TOML-frontmatter fields (`id`, `message_id`, `from`, `to`, `subject`, `date`, `in_reply_to`, `references`, `attachments`, `mailbox`, `read`, `dkim`, `spf`), read/unread semantics, mailbox naming, and the trust model (DKIM/SPF verification results stored in frontmatter, not gating reads).

**Priority:** P0

- [x] `agents/common/aimx-primer.md` created with sections: Tools, Storage layout, Frontmatter, Mailboxes, Read/unread, Trust model
- [x] Each MCP tool documented with its parameter names and types, matching `src/mcp.rs` exactly (no drift)
- [x] Frontmatter section lists every field and its semantics; matches `ingest.rs` output
- [x] No forward references to unimplemented features; grep for "TODO" / "FIXME" returns nothing
- [x] Length < 300 lines (LLM context budget); reviewed for tone (instructional, not promotional)

#### S28-2: `agents/claude-code/` plugin package

**Context:** Claude Code plugin format is a directory containing `.claude-plugin/plugin.json` (manifest with optional `mcpServers` block) and `skills/<name>/SKILL.md` (skill with YAML frontmatter). The plugin's MCP entry points at the installed `aimx` binary (default `/usr/local/bin/aimx` — match how `aimx setup` already hard-codes this path in `display_mcp_section`). The skill re-wraps `agents/common/aimx-primer.md` with Claude Code's required frontmatter (`name`, `description`). Before writing the manifest, verify the current Claude Code plugin schema against official docs — the research memo in this task may be stale.

**Priority:** P0

- [x] `agents/claude-code/.claude-plugin/plugin.json` exists with `name: "aimx"`, `description`, `version`, `author`, and `mcpServers.aimx` entry; `--data-dir` rewrites `args` via `serde_json` round-trip
- [x] `agents/claude-code/skills/aimx/SKILL.md` assembled at install time from a `SKILL.md.header` YAML header + the shared primer via `include_dir!`; byte-level test asserts exact concatenation
- [x] `agents/claude-code/README.md` is a short human-facing README pointing at `aimx agent-setup claude-code`
- [ ] Plugin loads cleanly in Claude Code on a real machine (manual validation); MCP tools appear; the skill is discoverable <!-- Partial: deferred — sandbox lacks Claude Code; schema verified against official docs and install-layout unit-tested. Tracked in Non-blocking Review Backlog (Sprint 28). -->
- [x] Plugin schema verified against current Claude Code plugin docs (link the doc URL in the README)

#### S28-3: `src/agent_setup.rs` + `aimx agent-setup` CLI command

**Context:** New module + subcommand. The module owns: (a) an embedded assets bundle covering `agents/` via `include_dir!`, (b) an agent registry table keyed by short name (`claude-code`) mapping to (source subtree, destination path template, activation hint), (c) the install routine (resolve destination under `$HOME` / `$XDG_CONFIG_HOME`, walk embedded source, write files with `0o644` / dirs with `0o755`, handle overwrite prompts, print activation hint). CLI wires `aimx agent-setup <agent>` with `--list`, `--force`, `--print`, and `--data-dir` (inherited from global args — passes through to the MCP command path baked into the plugin when the user wants a non-default data dir). The `SystemOps`/trait pattern used elsewhere (see `setup.rs`) should be applied so tests use a mock HOME.

**Priority:** P0

- [x] `src/agent_setup.rs` created; `Cargo.toml` adds `include_dir = "0.7"` (MIT/Apache-2.0 dual licensed — verified)
- [x] `AgentSpec` struct captures `name`, `source_subdir`, `dest_template`, and `activation_hint` (later refactored to `fn(Option<&Path>) -> String` in Sprint 29 to support snippet-style agents)
- [x] CLI subcommand `aimx agent-setup <agent>` with flags `--list`, `--force`, `--print`, plus the inherited global `--data-dir`
- [x] `--list` prints agent name + destination + activation hint for every registered agent
- [x] Install writes files with mode `0o644`, directories `0o755`; refuses to overwrite existing files unless `--force`; prompts interactively if stdin is a TTY and `--force` not set
- [x] Unknown agent name returns non-zero exit with a clear "unknown agent; run `aimx agent-setup --list`" message
- [x] `--print` writes the plugin tree to stdout in a diffable format (`=== path ===\n<contents>\n`); no disk writes
- [x] Unit tests (18 at Sprint 28) cover: Claude Code install to temp HOME lays out expected files; `--force` overwrites; `--print` writes no files; unknown agent errors; `--list` output is stable; file modes; HOME/XDG substitution; TTY prompt yes/no; byte-for-byte SKILL.md concatenation; malformed plugin.json rejection
- [x] Never requires root; refuses root with a clear message ("agent-setup is a per-user operation — run without sudo")
- [x] `cargo test`, `cargo clippy -- -D warnings`, `cargo fmt -- --check` clean

#### S28-4: Register Claude Code + simplify `aimx setup` MCP output

**Context:** With framework + plugin in place, register `claude-code` in the agent registry: source `agents/claude-code/`, destination `~/.claude/plugins/aimx/` (verify the canonical location against current Claude Code docs at implementation time — may be `~/.claude/plugins/` at the parent dir instead), activation `Restart Claude Code — plugin auto-discovered.` (or `claude plugin install ~/.claude/plugins/aimx` if Claude Code requires an explicit install step for file-installed plugins). Then rework `display_mcp_section()` in `src/setup.rs` to replace the generic JSON snippet (currently lines ~852–881) with: a short intro, the list of supported agents from `agent_setup::registry()`, and the recommended command `aimx agent-setup <agent>`. The setup wizard output stays short; details live in `book/agent-integration.md` (S28-5).

**Priority:** P0

- [x] `claude-code` registered in the `agent_setup.rs` registry with destination `$HOME/.claude/plugins/aimx` + activation hint (restart Claude Code for plugin auto-discovery)
- [x] `display_mcp_section()` in `src/setup.rs` no longer emits a generic `{"mcpServers": ...}` JSON snippet
- [x] `display_mcp_section()` lists supported agents and recommends `aimx agent-setup <agent>` (the list is pulled from the registry via `mcp_section_lines`, not duplicated by hand)
- [x] `mcp_config_snippet(data_dir)` helper in `src/setup.rs` removed; no remaining call sites
- [x] Tests for `setup.rs` MCP-section output updated (3 new unit tests asserting default + custom `--data-dir` paths)
- [ ] Manual validation: `sudo aimx setup <domain>` output shows the new MCP section; running the printed `aimx agent-setup claude-code` lays the plugin down <!-- Partial: setup-flow manual walkthrough deferred alongside the Claude Code plugin manual validation (same sandbox limitation). Tracked in Non-blocking Review Backlog (Sprint 28). -->

#### S28-5: PRD update + `book/agent-integration.md`

**Context:** The PRD gains a new §6.10 (Agent Integrations), a P0 user story, and scope edits — these were pre-staged with this sprint's planning and must be finalized as part of the sprint (the PRD edits are committed alongside code in this sprint). The book needs a new chapter `agent-integration.md` explaining the installer, listing supported agents with install commands (Sprint 28 only ships Claude Code; future sprints append to this page), and linking to each agent's `agents/<agent>/README.md`. `book/mcp.md` stays focused on the MCP server surface; `agent-integration.md` is the integration-onboarding chapter.

**Priority:** P0

- [x] `docs/prd.md` §5 adds the "aimx agent-setup" P0 user story (verified in place)
- [x] `docs/prd.md` §6 gains §6.10 Agent Integrations with FR-49, FR-50, FR-51, FR-52 (verified in place)
- [x] `docs/prd.md` §6.1 FR-10 narrowed to point at `aimx agent-setup` (verified in place)
- [x] `docs/prd.md` §9 In Scope / Out of Scope updated (verified in place)
- [x] `book/agent-integration.md` created with: what `aimx agent-setup` does, supported agents table (Claude Code only in this sprint), per-agent activation steps, troubleshooting
- [x] `book/index.md` links `agent-integration.md` in both Key Capabilities and Guide Contents
- [x] `book/mcp.md` adds a one-line pointer "To install AIMX into your agent, see [Agent Integration](agent-integration.md)" near the top

---

## Sprint 29 — Codex CLI + OpenCode + Gemini CLI Integration (Days 82–84.5) [DONE]

**Goal:** Add Codex CLI, OpenCode, and Gemini CLI to the `aimx agent-setup` registry with full plugin/skill packages.

**Dependencies:** Sprint 28 (framework + Claude Code reference).

**Design note:** Before authoring each agent's package, verify the current plugin/skill format and canonical destination path against that agent's official docs. The Sprint 28 research memo is a starting point, not a source of truth — agent formats drift.

#### S29-1: `agents/codex/` plugin + registry entry

**Context:** Codex CLI uses TOML config at `~/.codex/config.toml` for MCP servers and has a plugin system with `.codex-plugin/plugin.json` manifests (mirrors Claude Code's structure per research memo; confirm at implementation time). Plugins bundle skills under `skills/<name>/SKILL.md`. The Codex plugin re-wraps the common primer. Destination on disk: `~/.codex/plugins/aimx/` (verify). Activation hint: the exact `codex plugin install ...` command if Codex requires explicit installation, or a "restart Codex" message otherwise.

**Priority:** P0

- [x] `agents/codex/.codex-plugin/plugin.json` + `agents/codex/skills/aimx/SKILL.md.header` + `agents/codex/README.md` authored, re-using the common primer via `include_dir!`
- [x] `codex` registered in `agent_setup.rs` registry with destination `$HOME/.codex/plugins/aimx` + activation hint; `--data-dir` rewrites `mcpServers.aimx.args` in `plugin.json`
- [x] Unit tests cover Codex install to temp HOME, `--print` emission, and `mcpServers` schema key (tightened from weak `"mcp"` substring to exact `"mcpServers"`)
- [x] Plugin format documented against current Codex CLI docs (link in the README); inline code comment on `codex_hint` flags that the camelCase `mcpServers` shape mirrors Claude Code on an unvalidated assumption pending manual validation
- [ ] Manual validation on a machine with Codex CLI installed: plugin is picked up; MCP tools appear <!-- Partial: deferred — sandbox lacks Codex CLI; schema assumption documented inline. Tracked in Non-blocking Review Backlog (Sprint 29). -->

#### S29-2: `agents/opencode/` skill + registry entry

**Context:** OpenCode (anomalyco) uses a skills system compatible with Claude Code's `SKILL.md` format, discovered from `.opencode/skills/` (project) or `~/.config/opencode/skills/` (user). Its MCP config is separate — in `opencode.json` / `opencode.jsonc` under the root key `mcp.<name>` with `command` as a single array combining binary + args. Two ways to handle MCP wiring: (a) write an `mcp.json` snippet file alongside the skill that the user pastes into `opencode.json`, or (b) just write the skill and have the activation hint print the exact JSONC block to paste. Prefer (b) — simpler, no extra file, matches the "print the activation command" pattern. Decide and document in `agents/opencode/README.md`.

**Priority:** P0

- [x] `agents/opencode/SKILL.md.header` authored; assembled with the shared primer at install time (skill-only layout — header flat at the top of `agents/opencode/` because the destination path already ends in `skills/aimx/`)
- [x] `agents/opencode/README.md` documents the MCP wiring step (printed JSONC snippet) and the skill install destination `$HOME/.config/opencode/skills/aimx/`
- [x] `opencode` registered in `agent_setup.rs` registry; activation hint prints the JSONC snippet the user appends to `opencode.json`; `--data-dir` threads into the `command` array via `serde_json::json!` so paths with `"` or `\` escape correctly
- [x] Unit tests cover install to temp HOME, `--print` emission of both skill tree AND activation snippet, `--data-dir` threading into the printed snippet, and special-character escaping regression test
- [x] Canonical OpenCode skill destination verified against current OpenCode docs (link in README)

#### S29-3: `agents/gemini/` skill + registry entry

**Context:** Gemini CLI is Google's developer-facing agent CLI with native MCP support. It picks up per-project context from a `GEMINI.md` file at the repo root and activates skills on demand via an `activate_skill` tool — so AIMX can ship as a Gemini skill that re-wraps the common primer. MCP servers are configured in Gemini's user-level settings file (commonly `~/.gemini/settings.json` — verify the canonical path and schema against current Gemini CLI docs at implementation time; the path and key names may have shifted). Destination for the skill itself depends on Gemini's current skills layout (project-local `.gemini/skills/` vs user-level `~/.gemini/skills/`); register at the user-level path to match how AIMX installs for other agents. MCP wiring: Gemini CLI uses a JSON object keyed by server name (similar to Claude Code's `mcpServers`). Prefer the "print the exact JSON snippet as the activation hint" pattern already used for OpenCode — AIMX writes the skill, prints the `settings.json` fragment the user merges, and stops. Do NOT mutate `settings.json` directly (consistent with FR-49).

**Priority:** P0

- [x] `agents/gemini/SKILL.md.header` authored (skill-only layout mirroring OpenCode); assembled with the shared primer at install time
- [x] `agents/gemini/README.md` documents the two-step activation: run `aimx agent-setup gemini` to install the skill, then paste the printed MCP entry into `~/.gemini/settings.json`
- [x] `gemini` registered in `src/agent_setup.rs` registry with destination `$HOME/.gemini/skills/aimx` + activation hint (prints exact `mcpServers.aimx` JSON block to merge); `--data-dir` threads into `args` via `serde_json::json!`
- [x] Unit tests cover install to temp HOME and `--print` emission of both skill tree and MCP JSON snippet
- [x] Skill destination, settings file path, and MCP schema verified against current Gemini CLI docs; URL linked from `agents/gemini/README.md`
- [ ] Manual validation on a machine with Gemini CLI installed: `aimx agent-setup gemini` → merge printed JSON → Gemini sees `aimx` MCP tools and the skill is discoverable <!-- Partial: deferred — sandbox lacks Gemini CLI. Tracked in Non-blocking Review Backlog (Sprint 29). -->

#### S29-4: Update `book/agent-integration.md` + `--list` output

**Context:** Extend the book chapter and the `aimx agent-setup --list` output to cover Codex, OpenCode, and Gemini. `--list` already reads from the registry so this comes for free once the three entries are registered; the book update is manual. Also update the README at repo root to mention all four supported agents (Claude Code, Codex, OpenCode, Gemini) after this sprint.

**Priority:** P1

- [x] `book/agent-integration.md` gains Codex, OpenCode, and Gemini sections (install command, activation step, troubleshooting quirks)
- [x] `aimx agent-setup --list` output automatically covers all four agents via the registry; tests pass
- [x] Repo `README.md` lists all four agents (Claude Code, Codex, OpenCode, Gemini) in the agent-support section
- [x] Links between `book/agent-integration.md` and each agent's `agents/<agent>/README.md` resolve

---

## Sprint 30 — Goose + OpenClaw Integration (Days 84.5–87) [DONE]

**Goal:** Add Goose (recipe-based) and OpenClaw (skill-based, JSON5 config) to `aimx agent-setup`, completing the v1 agent-integration roster.

**Dependencies:** Sprint 29.

**Design note:** Goose's integration shape differs from the others — Goose uses YAML "recipes" with `title` + `prompt` + `extensions` rather than plugins+skills. The recipe bundles both the MCP extension config AND the agent-facing instructions (the primer) in one file. OpenClaw uses skill directories similar to Claude Code but with a separate MCP config (JSON5 at `~/.openclaw/openclaw.json` under `mcp.servers`). Verify formats against current docs at implementation time.

#### S30-1: `agents/goose/aimx-recipe.yaml` + registry entry

**Context:** Goose recipes are YAML files with required `title` + `prompt` and optional `extensions` (list of MCP servers), `parameters`, etc. For AIMX, the recipe's `prompt` re-wraps the common primer, and `extensions` includes a stdio entry for `aimx mcp` so the recipe self-installs the MCP server when run. Destination: the user's local Goose recipes directory — when `GOOSE_RECIPE_GITHUB_REPO` is set, print guidance to commit the file there; otherwise write to `~/.config/goose/recipes/aimx.yaml` (verify canonical path). Activation hint prints `goose run --recipe aimx` (the form Goose uses to execute a recipe by name).

**Priority:** P0

- [x] Recipe authored as `agents/goose/aimx.yaml.header` (filename deviation from plan: ends `aimx.yaml` at install time so `goose run --recipe aimx` resolves correctly) with `title`, `prompt: |` = common primer, `extensions:` stdio entry for `aimx mcp`
- [x] `goose` registered in `agent_setup.rs`; destination `$HOME/.config/goose/recipes/aimx.yaml`; activation hint references `$GOOSE_RECIPE_GITHUB_REPO` by name (deterministic — hint doesn't read env var at render time) for team-sharing guidance
- [x] Activation hint prints `goose run --recipe aimx`
- [x] Unit tests cover default-path install, `--print` output, line-oriented YAML injection of `--data-dir` preserving the `prompt: |` block scalar, byte-for-byte env-independence, and negative tests for `rewrite_recipe_data_dir` error path (missing `args:` injection point)
- [x] Recipe format verified against current Goose docs (link in `agents/goose/README.md`)
- [ ] Manual validation on a machine with Goose installed: `aimx agent-setup goose` → `goose run --recipe aimx` → MCP extension loads <!-- Partial: deferred — sandbox lacks Goose. Tracked in Non-blocking Review Backlog (Sprint 30). -->

#### S30-2: `agents/openclaw/` skill + registry entry

**Context:** OpenClaw skills live in `~/.openclaw/skills/<name>/` with a `SKILL.md` carrying YAML frontmatter (`name`, `description`, optional `metadata` with `requires`, `emoji`, `os`, `install`). MCP wiring is separate — added to `~/.openclaw/openclaw.json` under `mcp.servers.aimx`, or via `openclaw mcp set aimx '{...}'`. Prefer the CLI: activation hint prints the `openclaw mcp set aimx '{"command":"aimx","args":["mcp"]}'` command so the user wires MCP with one pasted command (no config-file editing, no JSON5 parsing on our end).

**Priority:** P0

- [x] `agents/openclaw/SKILL.md.header` authored (flat skill layout) with valid frontmatter; assembled with the common primer at install time via `include_dir!`
- [x] `agents/openclaw/README.md` documents the two-step activation: `aimx agent-setup openclaw`, then run the printed `openclaw mcp set` command
- [x] `openclaw` registered in `agent_setup.rs`; activation hint prints the exact `openclaw mcp set aimx '<json>'` command. JSON body is POSIX-shell-escaped via new `posix_single_quote` helper (`'\''` trick) so `--data-dir` paths containing `'` survive intact
- [x] Unit tests cover install layout, activation-hint stability, and the single-quote round-trip: construct `--data-dir` with embedded `'`, render hint, extract quoted argument, unquote, parse as JSON, assert byte-for-byte equality
- [x] OpenClaw skill and `openclaw mcp set` CLI syntax verified against current OpenClaw docs (link in README)
- [ ] Manual validation on a machine with OpenClaw installed: `aimx agent-setup openclaw` → paste the printed `openclaw mcp set` command → MCP tools appear <!-- Partial: deferred — sandbox lacks OpenClaw. Tracked in Non-blocking Review Backlog (Sprint 30). -->

#### S30-3: Final docs pass + README overhaul

**Context:** With all five v1 agents shipped, tidy the user-facing docs. `book/agent-integration.md` gets Goose and OpenClaw sections. The top-level `README.md` agent-integration section lists all five with one-line install commands and retires any lingering "copy this JSON snippet" prose. Spot-check `book/mcp.md` and `book/getting-started.md` for stale generic-snippet references.

**Priority:** P1

- [x] `book/agent-integration.md` has sections for all six v1 agents: Claude Code, Codex CLI, OpenCode, Gemini CLI, Goose, OpenClaw (Gemini was added in Sprint 29; Goose + OpenClaw now complete the FR-50 roster)
- [x] Top-level `README.md` shows a six-row table of supported agents + install commands (deviation from plan: plan said "five-row" but PRD §6.10 FR-50 lists six v1 agents — implementer correctly sized the table to the spec)
- [x] `grep -r "mcpServers" book/ docs/` returns only references inside `book/agent-integration.md` or the PRD; stale `{"mcpServers": …}` prose purged from `book/mcp.md`, `book/getting-started.md`, `docs/manual-setup.md`
- [x] `aimx agent-setup --list` output covers all six agents via the registry; tests pass

---

## Sprint 31 — Channel-Trigger Cookbook (Days 87–89.5) [DONE]

**Goal:** Document email→agent channel-trigger recipes side-by-side for every supported agent. No new CLI surface — this is a docs + integration-test sprint leveraging the existing `cmd` trigger plumbing (`src/channel.rs`).

**Dependencies:** Sprint 30.

#### S31-1: `book/channel-recipes.md` — side-by-side agent invocation examples

**Context:** Channel rules in AIMX already fire shell commands with template variables (`{filepath}`, `{from}`, `{subject}`, `{mailbox}`, `{id}`, etc.) — see `src/channel.rs` and FR-30/31. The missing piece is canonical, agent-specific documentation: which agent CLI flag maps to "take this email and act on it," what approval mode to use so the trigger runs non-interactively, where the agent's output goes (stderr/stdout/log file), and how to pass `{filepath}` safely. One chapter covers all five MCP-supported agents plus Aider (the no-MCP case). Each subsection includes a complete `config.toml` snippet the user can copy.

**Priority:** P0

- [x] `book/channel-recipes.md` authored with subsections for all six v1 agents plus Aider. OpenClaw documented as a limitation (no non-interactive `run`/`exec` CLI found in current docs) with pointer to `docs.openclaw.ai` per the sprint's explicit allowance
- [x] Each subsection contains a working `[[mailbox.catchall.channel]]` TOML snippet, agent-specific flag explanation (approval mode, output format, non-interactive), and notes on exit-code handling/logs
- [x] Chapter opens with overview and cross-reference to `book/channels.md`
- [x] Chapter closes with summary table covering all six v1 agents plus Aider
- [x] Flag references verified against each agent's current docs; chapter includes an explicit flag-drift warning directing readers to run `<agent> --help` before production use

#### S31-2: Integration test for a representative channel recipe

**Context:** Today `src/channel.rs` has unit tests for filter matching and template expansion, but no end-to-end test covering "email ingested → channel rule matches → shell command runs with templated args." Adding one test protects the channel pipeline from regressions that would silently break all recipe users. Use Claude Code's `claude --help` (or `/bin/echo` as an agent-agnostic baseline) as the command so the test stays fast and doesn't require a real agent. Assert that the command ran, received the expected `{filepath}` expansion, and did not block ingest delivery on failure.

**Priority:** P1

- [x] New integration test `channel_recipe_end_to_end_with_templated_args` in `tests/integration.rs` drives ingest → match → templated shell command end-to-end using `tests/fixtures/plain.eml`
- [x] Test asserts the marker file was created and its contents contain the expected `{filepath}` and `{subject}` expansions; a paired `false` sibling rule proves trigger failure does not block delivery
- [x] Runs in the existing CI matrix (45 integration tests now pass, +1 new)

#### S31-3: Cross-link and README update

**Context:** The cookbook is worthless if users don't find it. Link it from (a) `book/channels.md` ("for agent-specific recipes, see Channel Recipes"), (b) `book/agent-integration.md` ("once your agent is installed, see Channel Recipes for email-triggered workflows"), (c) `README.md` top-level, and (d) each agent's `agents/<agent>/README.md`. Also add an entry to the AIMX-side summary table (MCP support vs channel-trigger support) at the top of the cookbook.

**Priority:** P1

- [x] `book/channels.md`, `book/agent-integration.md`, top-level `README.md`, and each of the six `agents/<agent>/README.md` files link `book/channel-recipes.md`
- [x] `book/SUMMARY.md` created (new mdbook-compatible index) listing `channel-recipes.md` alongside all other chapters
- [ ] All cross-links resolve in a local `mdbook build` <!-- Partial: repo has no `book.toml` yet; `SUMMARY.md` is in mdbook-compatible format ready for future adoption. Tracked in Non-blocking Review Backlog (Sprint 31). -->
- [x] Top of `channel-recipes.md` has a summary table of all six v1 agents + Aider: MCP support · Channel-trigger pattern · Notes

---

## Sprint 32 — Non-blocking Cleanup (Days 89.5–92) [DONE]

**Goal:** Address accumulated non-blocking improvements from sprint reviews (Sprints 12, 19, 20, 21, 22, 26, 27). Grouped by theme; each story is self-contained with enough context for implementation without consulting the original review threads.

**Dependencies:** Sprint 31.

### S32-1: Verifier SMTP listener concurrency bound

**Context:** `services/verifier/src/main.rs` — `run_smtp_listener` spawns a new task per accepted connection with no upper bound. Per-connection hardening is already tight (30s wall clock, 10s per-line, 1 KiB per-line), so this is defense-in-depth DoS hardening, not a correctness bug. Flagged during Sprint 12 review with an inline TODO pointing at Sprint 14; never landed. Add a bounded semaphore or `tower::limit::ConcurrencyLimit`-style gate around the accept loop and remove the inline comment.

**Priority:** P2

- [x] Bounded `tokio::sync::Semaphore` gate added with default cap 128 (tunable via `SMTP_MAX_CONCURRENT` env var)
- [x] Uses `try_acquire_owned` so saturated gate drops the excess connection cleanly rather than blocking the accept loop
- [x] New `smtp_listener_concurrency_gate_drops_excess` test confirms the upper bound is honored
- [x] Inline "deferred to Sprint 14" comment removed
- [x] `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt -- --check` clean in `services/verifier/` (43 tests pass)

### S32-2: Outbound delivery — share DATA buffer across recipients + collect all MX errors

**Context:** Two related outbound-delivery improvements flagged in Sprint 19/20 reviews. (a) `deliver_message()` clones the raw DATA payload per recipient (`data.clone()`) — for 25MB messages with many recipients this spikes memory; switch to `Arc<Vec<u8>>` (or `bytes::Bytes`) to share a single buffer. (b) `LettreTransport` `last_error` only retains the final MX failure when all MX servers fail, which makes debugging multi-MX failures painful — collect every per-MX error and surface them together.

**Priority:** P2

- [x] `deliver_message()` wraps DATA in `Arc<Vec<u8>>` — one allocation serves all recipients
- [x] New `deliver_across_mx` pure helper collects every per-MX error into the returned failure (not just the last one)
- [x] New `deliver_across_mx_collects_all_errors_on_total_failure` test asserts all errors appear in the returned error
- [x] Existing send integration tests still pass
- [x] `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt -- --check` clean

### S32-3: TLS file check + service management consistency

**Context:** Two cross-platform consistency cleanups. (a) `can_read_tls` in `src/serve.rs` checks the cert with `metadata().is_file()` but checks the key with `File::open()` — flagged Sprint 21 review; pick one approach and apply to both. (b) `restart_service()` / `is_service_running()` in `src/setup.rs` hardcode `systemctl` even when `install_service_file()` writes an OpenRC init script; pre-existing (Sprint 22 review), not a regression. Route through the same OS detection already used in `install_service_file()` so service management matches the init system chosen at install time. (c) Remove unused `_domain` parameter from `is_already_configured` (Sprint 22 review) — smtpd.conf domain matching was removed and the param has been dead code since.

**Priority:** P2

- [x] `can_read_tls` uses `File::open()` for both cert and key (unified approach)
- [x] `restart_service()` and `is_service_running()` route through `detect_init_system` via new pure `restart_service_command`/`is_service_running_command` helpers — systemd uses `systemctl`, OpenRC uses `rc-service`
- [x] OpenRC service-management code path unit-tested via the pure command-builder helpers
- [x] `is_already_configured` signature drops `_domain`; all 4 call sites updated
- [x] `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt -- --check` clean

### S32-4: Network ops dedup — single `hostname -I` call

**Context:** Sprint 26 review: `get_server_ip()` and `get_server_ipv6()` each shell out to `hostname -I` separately. Not a correctness issue, but duplicate work and duplicate failure modes. Consolidate into one trait method (e.g., `get_server_ips(&self) -> Result<(Option<Ipv4Addr>, Option<Ipv6Addr>)>`) — this is a breaking change to `NetworkOps`, which is fine since the trait is internal. Update `RealNetworkOps` and `MockNetworkOps` accordingly.

**Priority:** P3

- [x] New `NetworkOps::get_server_ips() -> (Option<Ipv4Addr>, Option<Ipv6Addr>)` method returns both families
- [x] `RealNetworkOps` invokes `hostname -I` exactly once via new `parse_hostname_i_output` helper (skips non-global IPv6)
- [x] `MockNetworkOps` updated in `setup.rs` + `verify.rs` to accept both values
- [x] Existing Sprint 26 IPv4/IPv6 call sites and tests updated — no behavior change
- [x] New dedup + parser unit tests added
- [x] `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt -- --check` clean

### S32-5: CI clippy `--all-targets` adoption + fix pre-existing test lints

**Context:** Sprint 27 review: `cargo clippy --all-targets -- -D warnings` surfaces ~4 pre-existing lint errors in the `test` target (`str::replace` chaining, `manual arithmetic check`, `field_reassign_with_default`). The current CI gate runs `cargo clippy -- -D warnings` which excludes tests, so these never fail CI. Fix the lints and flip the CI invocation to `--all-targets` so regressions in test code get caught going forward.

**Priority:** P2

- [x] All 4 pre-existing test-target lints fixed: `collapsible_str_replace` ×2 in `dkim.rs`, `implicit_saturating_sub` in `send.rs`, `field_reassign_with_default` in `setup.rs`
- [x] `.github/workflows/ci.yml` core-tests + verifier-tests jobs flipped to `cargo clippy --all-targets -- -D warnings`; commented-out Alpine/Fedora jobs updated to match
- [x] `cargo clippy --all-targets -- -D warnings` clean on both `aimx` and `aimx-verifier` crates

### S32-6: Post-merge cosmetic cleanups

**Context:** Two tiny post-merge cleanups that reviewers flagged as zero-behavior-change niceties. Kept small so they don't bloat the cleanup sprint: (a) in the verifier `smtp_session`, fold `let mut writer = writer;` into the destructuring as `let (reader, mut writer) = tokio::io::split(stream);` (Sprint 12 review); (b) audit the sprint file's Non-blocking Review Backlog, mark these Sprint 32 items as `[x]` with `_Triaged into Sprint 32_` once implemented.

**Priority:** P3

- [x] `smtp_session` writer destructuring folded
- [x] Sprint 32-consumed backlog items were already marked `[x] — _Triaged into Sprint 32 (SN-M)_` during the triage-in pass; implementer verified no stragglers remained
- [x] `cargo fmt -- --check` clean

---

## Sprint 33 — Filesystem Split + `aimx` Group Foundation (Days 92–94.5) [DONE]

**Goal:** Move configuration and DKIM secrets to `/etc/aimx/`, establish the `aimx` system group, and provide the `/run/aimx/` runtime directory. No behavior change is visible to agents yet — this is the foundation every subsequent v0.2 sprint builds on.

**Dependencies:** Sprint 32 (v1 complete).

**Design notes (apply to all stories below):**
- Pre-launch: there are no existing installs to migrate. `aimx setup` writes directly to the new locations; no dual-read compatibility shims.
- Every path that was previously `/var/lib/aimx/{config.toml,dkim/*}` must now resolve via the new config-dir lookup. A single `config_dir()` helper (new or promoted) is the source of truth, analogous to today's `data_dir()`.
- Tests: the trait-based mock pattern from v1 (`SystemOps`, `NetworkOps`, `MailTransport`) extends cleanly. Add `FileSystemOps` fakes if a current test hardcodes `/var/lib/aimx/config.toml`.

#### S33-1: `config_dir()` helper + `AIMX_CONFIG_DIR` env var

**Context:** Today `config.rs` opens `/var/lib/aimx/config.toml` directly. Introduce `config_dir()` in `src/config.rs` that resolves, in order: `AIMX_CONFIG_DIR` env var → `/etc/aimx/` default. Mirror the shape of today's `data_dir()` so tests can override via env var in `tempfile::TempDir`. `config.toml` then always resolves to `<config_dir>/config.toml`. The DKIM key pair lives under `<config_dir>/dkim/{private,public}.key` (see S33-3). Retire every direct reference to `/var/lib/aimx/config.toml` or `<data_dir>/config.toml` — grep the whole crate and the verifier crate.

**Priority:** P0

- [x] `config_dir()` added to `src/config.rs`; returns `PathBuf` from `AIMX_CONFIG_DIR` if set, else `/etc/aimx/`
- [x] `config_path()` (or `config_file()`) returns `config_dir().join("config.toml")`
- [x] `--data-dir` CLI flag remains unchanged (it governs `/var/lib/aimx/`, not config) <!-- Re-review confirmed: `Config::load_resolved_with_data_dir(override)` threads the CLI flag through ingest/mailbox/status/send/serve/mcp after initial review flagged it as silently ignored (fixed in a6ddb26). -->
- [x] No callers reference `/var/lib/aimx/config.toml` literally after this story; grep confirms
- [x] Tests use `std::env::set_var("AIMX_CONFIG_DIR", tmp.path())` with the existing parallel-safety pattern (scoped serial guard where required)
- [x] `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt -- --check` clean

#### S33-2: `aimx setup` writes config to `/etc/aimx/config.toml` (mode `640`)

**Context:** `setup.rs` currently creates `/var/lib/aimx/config.toml` via `fs::write`. Change it to create `/etc/aimx/` (mode `755`), then write `config.toml` inside with mode `640`, owner `root:root`. Use a single helper (e.g., `install_config_file`) that applies mode via `std::os::unix::fs::PermissionsExt`. If `AIMX_CONFIG_DIR` is set (tests), skip the mode enforcement — `tempfile::TempDir` can't host root-owned files and tests don't need to verify mode (a dedicated mode-enforcement unit test covers the real-install path).

**Priority:** P0

- [x] `setup.rs` creates `/etc/aimx/` with mode `755` if absent
- [x] Config written to `<config_dir>/config.toml` with mode `640`, owner `root:root` when running as root on a real install <!-- Re-review confirmed: gated on `is_root()` alone after initial review flagged the `AIMX_CONFIG_DIR`-absence gating as too weak (fixed in a6ddb26). Fresh-install path uses atomic `OpenOptions::mode(0o640).create_new(true)`. -->
- [x] Re-entrant setup: if `<config_dir>/config.toml` already exists and matches the domain, proceed (existing re-entrant pattern preserved — see `is_already_configured`)
- [x] Unit test asserts the written mode is `0o640` (use `/tmp`-style path + permissions check; skip only when the test is running non-root via `nix::unistd::geteuid`)
- [x] `book/configuration.md` updated with the new path
- [x] `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt -- --check` clean

#### S33-3: DKIM keys move to `/etc/aimx/dkim/` (private `600`, public `644`)

**Context:** `src/dkim.rs` `generate_keypair()` writes to `<data_dir>/dkim/{private,public}.key` today, and Sprint 25 relaxed the private key to mode `0o644` specifically so the non-root `aimx send` process could read it. v0.2 reverses that: private key moves to `<config_dir>/dkim/private.key` with mode `0o600`, owner `root:root`, and is never again read by non-root code (see S34-2 for how `aimx serve` takes over signing). Public key moves to `<config_dir>/dkim/public.key` with mode `0o644`. `load_private_key()` must now return a clear permission-denied error when invoked non-root, pointing the user at `aimx send` instead of manually running `aimx send`-equivalent code — but in practice, the only remaining non-root caller of `load_private_key()` at the end of this sprint should be tests and the `aimx dkim-keygen` helper.

**Priority:** P0

- [x] `generate_keypair()` writes private key to `<config_dir>/dkim/private.key` with mode `0o600`, public key to `<config_dir>/dkim/public.key` with mode `0o644` <!-- Re-review confirmed: `write_file_with_mode` helper uses atomic `OpenOptions::mode(...).create_new(true)` (added in a6ddb26) to eliminate the write→chmod race flagged in initial review. -->
- [x] The Sprint 25 test `private_key_has_restricted_permissions` is updated back to expect `0o600`
- [x] New test `public_key_is_world_readable` asserts `0o644` on the public key
- [x] `load_private_key()` error message on permission denied reads: `DKIM private key is readable only by root. This command must be invoked by \`aimx serve\` (root) — non-root processes must submit mail via \`aimx send\` instead.`
- [x] All consumers of `load_private_key()` audited — after this story, callers are `aimx serve` (root) and the test harness only <!-- Re-review confirmed: `load_private_key_permission_denied_surfaces_root_guidance` test added in a6ddb26 pins the guidance-message invariant. -->
- [x] `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt -- --check` clean

#### S33-4: `aimx` system group + `/run/aimx/` via `RuntimeDirectory=aimx`

**Context:** `aimx setup` must create an `aimx` system group during setup (idempotent — skip if it already exists) on systems with `groupadd`/`addgroup`. Group membership gates access to `/run/aimx/send.sock` once S34 lands; this sprint just creates the group and the runtime directory. `/run/aimx/` itself is provided by systemd — add `RuntimeDirectory=aimx`, `RuntimeDirectoryMode=0750`, `Group=aimx` to the generated systemd unit (still running as `User=root`). On OpenRC (Alpine) the init script gains an equivalent `checkpath -d -m 0750 -o root:aimx /run/aimx` step. Setup output (`display_deliverability_section` or a new `display_group_section`) prints the `usermod -aG aimx <user>` instruction the operator runs once to join an agent user to the group.

**Priority:** P0

- [x] `setup.rs` creates `aimx` group idempotently (detect `groupadd` vs `addgroup`); `SystemOps` gains `create_system_group(name)` trait method with `MockSystemOps` support
- [x] Generated systemd unit includes `RuntimeDirectory=aimx`, `RuntimeDirectoryMode=0750`, `Group=aimx` (while `User=root` stays); existing unit template tests extended
- [x] Generated OpenRC script has equivalent `checkpath` step; existing unit template tests extended
- [x] Setup output section prints the `usermod -aG aimx <user>` instruction and names the group explicitly
- [x] `book/setup.md` documents the group and the post-setup `usermod` step (+ `newgrp aimx` or logout/login hint)
- [x] Re-running `aimx setup` does NOT error when the group already exists
- [x] Tests cover: group creation command shape for both systemd/groupadd and OpenRC/addgroup; systemd unit contains `RuntimeDirectory=aimx`; `install_service_file` still passes <!-- Re-review confirmed: `run_setup_skips_group_creation_on_reentrant_path` added in a6ddb26 to pin the re-entrant invariant alongside the install-path ordering assertion. -->
- [x] `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt -- --check` clean

---

## Sprint 33.1 — Scope Reversal: Drop PTR + `aimx` Group + Non-blocking Cleanup (Days 94.5–97) [DONE]

**Goal:** Reverse two scope decisions before Sprint 34 builds on them: (a) drop PTR/reverse-DNS handling entirely (operator responsibility, out of aimx scope), (b) drop the `aimx` system group introduced in S33-4 — the UDS send socket in Sprint 34 will be world-writable (`0o666`), so group-gated authorization is no longer needed. Also clear the actionable items on the Non-blocking Review Backlog in the same sprint, and run manual end-to-end validation of the Claude Code + Codex CLI plugins on a real machine.

**Dependencies:** Sprint 33 (merged).

**Design notes:**
- PTR: aimx no longer checks, warns about, or documents reverse DNS. Operators configure it with their VPS provider. aimx's role ends at MX/A/SPF/DKIM/DMARC.
- `aimx` group: `/etc/aimx/` stays `root:root 0755`; DKIM private key stays `root:root 0600` (only `aimx serve` reads it). `/run/aimx/` is still provisioned by systemd `RuntimeDirectory=aimx` but at the default `root:root 0755`. The Sprint 34 socket will be `0o666` — any local user can submit mail. This reverses FR-1d and rewrites FR-18b.
- All eight stories land in one PR because they touch overlapping files (`setup.rs`, `verify.rs`, `book/setup.md`, `docs/prd.md`).

#### S33.1-1: Drop all PTR / reverse-DNS code and docs

**Context:** `check_ptr()` in `setup.rs`, related `NetworkOps::check_ptr_record` trait method, the PTR warning path in `display_deliverability_section`, PTR mentions in `book/setup.md`, `book/troubleshooting.md`, `docs/manual-setup.md`, `book/getting-started.md`, `README.md`, `src/cli.rs`, `src/term.rs` (if any), and `docs/prd.md` (FR-5 — already removed in this sprint's PRD edit pass) all go. Tests that assert PTR-warning output go. `get_server_ips()` / `get_server_ipv6()` stay (still used for MX/A/SPF verification) but the PTR caller paths are removed.

**Priority:** P0

- [x] `check_ptr()` and any PTR-specific helpers removed from `src/setup.rs`
- [x] `NetworkOps::check_ptr_record` trait method and `MockNetworkOps` implementation removed
- [x] `display_deliverability_section` no longer surfaces a PTR status block (the section may be deleted entirely if PTR was its only content)
- [x] All PTR-related tests removed from `src/setup.rs` and `src/verify.rs` test modules
- [x] Docs sweep: `book/setup.md`, `book/troubleshooting.md`, `book/getting-started.md`, `docs/manual-setup.md`, `README.md` — no remaining PTR/reverse-DNS mentions
- [x] `docs/prd.md` FR-5 already removed; FR-7 already updated to drop PTR from the DNS-records list — verify
- [x] `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt -- --check` clean

#### S33.1-2: Drop `aimx` system group and group-gating

**Context:** Reverses S33-4 of the just-merged Sprint 33. The group was created for the UDS socket auth story, but Sprint 34 is redesigned to use a world-writable socket instead. Remove `SystemOps::create_system_group` trait method + real + mock impls, strip `Group=aimx` from the generated systemd unit, drop OpenRC `chown root:aimx` but keep the `/run/aimx/` creation, drop the `[Group access]` setup output section, drop the `usermod -aG aimx` instruction, update `book/setup.md` + `book/configuration.md` + `docs/manual-setup.md`. The `/run/aimx/` runtime directory stays — systemd `RuntimeDirectory=aimx` (with default mode `0755`) still creates it, which is fine for a world-writable socket.

**Priority:** P0

- [x] `SystemOps::create_system_group` trait method removed along with real and `MockSystemOps` implementations
- [x] Generated systemd unit no longer contains `Group=aimx`; `RuntimeDirectory=aimx` retained (default `RuntimeDirectoryMode=0755` — drop the explicit `RuntimeDirectoryMode=0750` line)
- [x] Generated OpenRC script no longer does `chown root:aimx` or `command_user="root:aimx"`; `checkpath -d -m 0755 -o root:root /run/aimx` retained
- [x] `run_setup` call graph: `create_system_group` call removed from install phase; re-entrant short-circuit path unchanged
- [x] Setup output: `[Group access]` section deleted; no `usermod -aG aimx` instruction printed
- [x] `book/setup.md`, `book/configuration.md`, `docs/manual-setup.md` — no remaining references to the `aimx` group
- [x] Tests updated: `systemd_unit_exposes_runtime_dir_and_aimx_group` replaced by `systemd_unit_declares_runtime_dir_without_group`; `group_section_mentions_aimx_group_and_usermod` removed; `run_setup_creates_aimx_group_when_not_configured` and `run_setup_skips_group_creation_on_reentrant_path` removed
- [x] `docs/prd.md` FR-1d already removed; FR-18b already rewritten — verify
- [x] `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt -- --check` clean

#### S33.1-3: Drop `data_dir` param from `verify::run_verify`

**Context:** Backlog item from Sprint 33 review. `verify` only reads `verify_host` from config; the parameter is dead weight.

**Priority:** P1

- [x] `verify::run_verify` signature no longer takes `data_dir: Option<&Path>`
- [x] `main.rs` dispatch updated
- [x] Backlog item marked `[x] _Done in Sprint 33.1_`
- [x] `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt -- --check` clean

#### S33.1-4: Factor `is_root()` into `src/platform.rs`

**Context:** Backlog item from Sprint 33 review. `setup.rs` and `verify.rs` each have their own copy. Create `src/platform.rs` exporting `is_root()` (and a home for future platform helpers); update both callers.

**Priority:** P1

- [x] `src/platform.rs` created; exports `pub fn is_root() -> bool`
- [x] `setup.rs` and `verify.rs` both use `crate::platform::is_root`; local copies deleted
- [x] Module declared in `main.rs`
- [x] Backlog item marked `[x] _Done in Sprint 33.1_`
- [x] `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt -- --check` clean

#### S33.1-5: Drop unused `_data_dir` params from runtime `run()` signatures

**Context:** Backlog item from Sprint 33 review. `ingest::run`, `mailbox::run`, `status::run`, `send::run`, `serve::run`, `mcp::run` each accept `_data_dir: Option<&Path>` as dispatch-layer uniformity but don't use it — the override is threaded via `Config::load_resolved_with_data_dir` inside. Each `run()` will accept the typed override and call `Config::load_resolved_with_data_dir(data_dir_override)` itself.

**Priority:** P1

- [x] Each `run()` signature accepts `data_dir_override: Option<&Path>` (or the existing typed param) and threads it through its own `Config::load_resolved_with_data_dir` call
- [x] All dispatch sites in `main.rs` updated consistently
- [x] No `#[allow(unused)]` or leading-underscore dead params remain
- [x] Backlog item marked `[x] _Done in Sprint 33.1_`
- [x] `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt -- --check` clean

#### S33.1-6: Second `indent_block` test (multi-line, no trailing newline)

**Context:** Sprint 30 nice-to-have. Trivial — pin the current behavior with one extra test in `src/agent_setup.rs`.

**Priority:** P2

- [x] New test `indent_block_handles_multiline_without_trailing_newline` added
- [x] Backlog item marked `[x] _Done in Sprint 33.1_`
- [x] `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt -- --check` clean

#### S33.1-7: OpenClaw research + `aimx agent-setup openclaw` reshape

**Context:** Backlog item from Sprint 30. Research OpenClaw's current CLI (docs.openclaw.ai). If a non-interactive `run`/`exec` CLI exists, wire up the `book/channel-recipes.md` recipe. If not, reshape `aimx agent-setup openclaw` to print step-by-step manual setup instructions (mirroring the Gemini-CLI pattern which emits a printed JSON-merge block rather than writing a file). Update the OpenClaw skill, `book/channel-recipes.md`, and `agents/openclaw/` as needed.

**Priority:** P1

- [x] OpenClaw CLI capabilities documented inline in PR description (link to upstream docs)
- [x] If interactive-only: `aimx agent-setup openclaw` prints step-by-step manual setup guide (exact commands the operator runs)
- [x] If non-interactive: `aimx agent-setup openclaw` writes the skill + prints the single command
- [x] `book/channel-recipes.md` OpenClaw section updated to match
- [x] Existing OpenClaw unit tests updated for the new output
- [x] Backlog items (Sprint 30 OpenClaw + Sprint 31 nice-to-have OpenClaw recipe) marked `[x] _Done in Sprint 33.1_`
- [x] `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt -- --check` clean

#### S33.1-8: Manual E2E validation — Claude Code + Codex CLI plugins (this machine)

**Context:** Backlog items from Sprints 28 + 29. Both agents are installed on the validation machine. Plan: `cargo build --release && sudo cp target/release/aimx /usr/local/bin/`, then run `aimx agent-setup claude-code` and `aimx agent-setup codex`; restart / reload each agent; confirm the 9 AIMX MCP tools appear and the `aimx` skill is discoverable. If Codex CLI's plugin format diverges from the assumed camelCase `mcpServers` shape, capture the divergence and fix `src/agent_setup.rs` + related tests in-sprint.

**Priority:** P0

- [x] `aimx agent-setup claude-code` run on this machine — 9 MCP tools appear in Claude Code, `aimx` skill discoverable (paste test output into PR description)
- [x] `aimx agent-setup codex` run on this machine — plugin accepted by Codex CLI; if format diverges, adjust schema and retest
- [x] If any divergence found → fix + add regression test for the real schema
- [x] Corresponding Non-blocking Backlog items (Sprint 28 Claude Code, Sprint 29 Codex CLI) marked `[x] _Validated in Sprint 33.1_`
- [x] Sprint 29 Gemini CLI, Sprint 30 Goose, Sprint 30 OpenClaw backlog items marked `[x] _Requires manual validation on real agent environments — deferred_` (Gemini + Goose remain deferred; OpenClaw covered by S33.1-7)
- [x] `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt -- --check` clean

---

## Sprint 34 — UDS Wire Protocol + `aimx serve` Send Listener (Days 97–99.5) [IN PROGRESS]

**Goal:** Move DKIM signing and outbound SMTP delivery inside `aimx serve`, exposed to local clients over a world-writable Unix domain socket at `/run/aimx/send.sock` (`root:root 0666`). `aimx send` is not yet rewritten (that's Sprint 35); this sprint stands up the daemon side and proves it works with a hand-written test client.

**Dependencies:** Sprint 33.1 (scope reversal: PTR + `aimx` group dropped).

**Design notes:**
- Wire protocol is text-ish in the framing but binary-safe in the body — `Content-Length` gives an exact byte count. This mirrors HTTP/1.1's framing discipline without inheriting HTTP's parsing surface.
- DKIM key loaded **once** at `aimx serve` startup into `Arc<DkimKey>` — every accepted send reuses the same in-memory key. Reloading on every connection would defeat the point of moving signing inside the daemon.
- No queue, no retries: each UDS accept spawns a per-connection tokio task that signs, delivers, writes the response frame, and exits. Existing `LettreTransport` + `hickory-resolver` MX resolution (from Sprints 19–20) is reused as-is.
- **Authorization is out of scope in v0.2.** The socket is world-writable (`0o666`) — any local user on the host can submit mail. `SO_PEERCRED` is read on accept and logged for diagnostics (`peer_uid` / `peer_pid` to journald) but is **not** used to authorize. Operators needing isolation should restrict host access by other means (OS user policy, container boundaries).

#### S34-1: `src/send_protocol.rs` — wire format codec

**Context:** Author a new module `src/send_protocol.rs` with pure codec logic: `SendRequest { from_mailbox: String, body: Vec<u8> }`, `SendResponse::Ok { message_id: String } | SendResponse::Err { code: ErrCode, reason: String }`, and `ErrCode { Mailbox, Domain, Sign, Delivery, Temp, Malformed }`. Provide async `parse_request<R: AsyncRead + Unpin>(&mut R) -> Result<SendRequest>` and `write_response<W: AsyncWrite + Unpin>(&mut W, &SendResponse) -> Result<()>`. Parser reads headers line-by-line up to the blank line, then reads exactly `Content-Length` bytes. Reject unknown commands (only `AIMX/1 SEND` is accepted), missing required headers, non-UTF-8 header names, and bodies exceeding the configured max size (default: reuse the inbound SMTP DATA limit of 25 MB from NFR-6 configurability). Codec module has zero IO beyond the `AsyncRead`/`AsyncWrite` generics — no tokio-net, no filesystem, no signing.

**Priority:** P0

- [ ] `src/send_protocol.rs` added; exports `SendRequest`, `SendResponse`, `ErrCode`, `parse_request`, `write_response`
- [ ] `parse_request` reads the leading `AIMX/1 SEND\n` line, then headers until blank line, then `Content-Length` bytes
- [ ] Required headers: `From-Mailbox`, `Content-Length`. Unknown headers ignored for forward-compat
- [ ] Rejects: wrong leading line → `Malformed`; missing required header → `Malformed`; `Content-Length` not parseable or exceeds cap → `Malformed`; body truncated → `Malformed`
- [ ] `write_response` emits `AIMX/1 OK <message-id>\n` or `AIMX/1 ERR <code> <reason>\n` (codes rendered as `MAILBOX`, `DOMAIN`, `SIGN`, `DELIVERY`, `TEMP`, `MALFORMED`)
- [ ] Round-trip unit tests for every response variant; `tokio-test::io::Builder` used for controlled async streams
- [ ] Parser fuzzed lightly with: truncated body, oversized body, CRLF vs LF, header case-insensitivity on names, duplicate `Content-Length`, missing blank line, empty body, body containing the literal `AIMX/1 SEND\n` (must NOT be misparsed as a second request)
- [ ] `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt -- --check` clean

#### S34-2: `aimx serve` binds `/run/aimx/send.sock` + accept loop

**Context:** Extend `src/serve.rs` to bind a `tokio::net::UnixListener` on `/run/aimx/send.sock` alongside the existing TCP SMTP listener. The socket is world-writable: explicitly `set_permissions(0o666)` after bind, owner left as `root:root` (no chown). On every accept, read `SO_PEERCRED` via `tokio::net::UnixStream::peer_cred()` and log `peer_uid` / `peer_pid` to the existing tracing pipeline (which already routes to journald) — for diagnostics only, not used for authorization. Each accepted connection gets its own tokio task — no bounded semaphore in this sprint (defense-in-depth concurrency cap can be a follow-up if review flags it). Bind failures are fatal at startup (`main` returns non-zero); runtime accept errors are logged and do not kill the listener.

**Priority:** P0

- [ ] `serve.rs` binds `UnixListener` at `send_socket_path()` (new helper: `<runtime_dir>/send.sock`; `AIMX_RUNTIME_DIR` env var overrides for tests)
- [ ] Socket mode set to `0o666` (world-writable) after bind via `set_permissions`; owner left as the running process's UID (root on real installs, the test user in tests — no explicit chown call)
- [ ] `SO_PEERCRED` read on each accept; `peer_uid`/`peer_pid` emitted at `info` level via `tracing` for diagnostics — explicitly NOT used for any authorization check
- [ ] Bind failure: process exits with `1` and a clear `error!` log naming the socket path and the errno
- [ ] If the socket file already exists at bind time (stale from prior crash), unlink-and-retry once, then fail loudly on second failure
- [ ] SIGTERM/SIGINT graceful shutdown drains the UDS accept loop the same way it drains the SMTP listener; socket file removed on clean shutdown
- [ ] Unit test binds the listener in a tempdir (override via `AIMX_RUNTIME_DIR`), asserts the file mode is `0o666`, connects from the same process, asserts accept fires and peer-cred fields are present
- [ ] Integration test: start `aimx serve` in a tempdir (systemd unit generation bypassed, binary invoked directly), connect a raw Unix socket, assert the listener accepts
- [ ] `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt -- --check` clean

#### S34-3: Daemon-side send handler — domain validation, DKIM sign, deliver

**Context:** Author `src/send_handler.rs` (new module) containing the per-connection handler: accept a `SendRequest`, look up the sender's `From:` header, validate the domain against `<config_dir>/config.toml`'s primary domain (case-insensitive, any local part accepted), DKIM-sign via the `Arc<DkimKey>` loaded at `aimx serve` startup (see below), deliver via the existing `LettreTransport` MX resolution path from Sprint 20, and emit the appropriate `SendResponse`. Error mapping: `From:` missing or malformed → `Malformed`; sender domain mismatch → `Domain`; `From-Mailbox` not registered in config → `Mailbox`; DKIM signing failure → `Sign`; permanent SMTP error from any MX → `Delivery` (with the last remote response in the `reason`); transient SMTP error → `Temp`. The DKIM key is loaded in `serve.rs::main` before the accept loop starts; a failure to load is fatal (`aimx serve` refuses to start). Every concurrent send is an independent tokio task — no queue, no Mutex yet (filename-allocation Mutex comes in Sprint 38 with sent-items persistence).

**Priority:** P0

- [ ] `src/send_handler.rs` created; `async fn handle_send(req: SendRequest, ctx: &SendContext) -> SendResponse`
- [ ] `SendContext` holds `Arc<DkimKey>`, primary domain, registered mailboxes, and an `Arc<dyn MailTransport>` for injection in tests
- [ ] `serve.rs::main` loads DKIM key once at startup; start failure is fatal with a clear message naming `/etc/aimx/dkim/private.key`
- [ ] `From:` parsing extracts local@domain; domain compare is case-insensitive; any local part accepted
- [ ] Domain mismatch returns `ERR DOMAIN sender domain does not match aimx domain`
- [ ] Unknown `From-Mailbox` returns `ERR MAILBOX mailbox \`<name>\` not registered`
- [ ] DKIM signing uses relaxed/relaxed canonicalization (preserving Sprint 25 fix); sign failure returns `ERR SIGN <detail>`
- [ ] Delivery uses existing `LettreTransport`; MX resolution errors map to `Temp`, permanent rejects to `Delivery`
- [ ] Response written to the UDS stream via `send_protocol::write_response`
- [ ] Accept-loop task is spawned with `tokio::spawn` so one slow delivery doesn't block other sends
- [ ] Unit tests mock `MailTransport`, exercise each error code path, and assert the right `SendResponse` variant
- [ ] Integration test: `aimx serve` running in a tempdir with a mock transport; a raw UDS test client writes `AIMX/1 SEND` + valid body; test asserts `OK <message-id>` response AND the transport received the signed message AND the signature verifies against the public key
- [ ] `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt -- --check` clean

---

## Sprint 35 — `aimx send` Thin UDS Client + End-to-End (Days 99.5–102) [NOT STARTED]

**Goal:** Rewrite `aimx send` as a thin UDS client that does no signing, owns no DKIM key access, and shells the full signing + delivery responsibility to `aimx serve`. Validate the full path: `aimx send` → UDS → `aimx serve` → DKIM-sign → MX delivery.

**Dependencies:** Sprint 34 (wire protocol, UDS listener, daemon-side handler).

**Design notes:**
- After this sprint, `src/send.rs` is radically smaller — no `load_private_key()`, no `DkimSigner`, no `LettreTransport`. All of that now lives in `aimx serve`. The client composes the RFC 5322 message, opens the socket, writes one request frame, reads one response frame, exits with the status.
- The socket is world-writable (`0o666` from Sprint 34), so `connect()` does not fail with `EACCES` for any local user. The only socket-related error path the client surfaces is "socket missing" (daemon not running).
- `aimx send` no longer needs root. In fact, it now refuses to run as root (consistent with `aimx agent-setup`'s pattern) so agents don't accidentally mint mail through the daemon they themselves supervise.

#### S35-1: Rewrite `src/send.rs` as a UDS client

**Context:** Strip `src/send.rs` to the bare composer + client role: compose the unsigned RFC 5322 message (subject, from, to, cc, bcc, body, attachments — existing `compose_message()` stays), open `UnixStream::connect(send_socket_path())`, write an `AIMX/1 SEND` request via `send_protocol::write_request` (new helper mirroring `write_response`), await the response, map each response variant to a stable CLI exit code + user-facing message. Delete: `load_private_key()` calls, `sign_and_deliver()`, `LettreTransport` construction, `resolve_mx()` — all of that now lives in `aimx serve`. Keep: `compose_message()` and its attachment/threading helpers. Preserve every existing CLI flag (`--from`, `--to`, `--cc`, `--bcc`, `--subject`, `--body`, `--attach`, `--in-reply-to`, `--references`). Exit codes: `0` on `OK`, `1` on any `ERR`, `2` on socket-missing, `3` on malformed response.

**Priority:** P0

- [ ] `send.rs` after rewrite is <150 lines excluding `compose_message()` (enforce via a comment-anchored line count if desired, or just review)
- [ ] All DKIM-related code paths removed from `send.rs`; `cargo clippy --all-targets -- -D warnings` reports no unused imports
- [ ] Socket-missing error prints exactly: `aimx daemon not running — check 'systemctl status aimx'` on stderr and exits with code `2`
- [ ] Other `connect()` failures (`ECONNREFUSED`, `EIO`, etc.) print a clear `Failed to connect to aimx daemon at <path>: <err>` message and exit with code `2`
- [ ] Response `OK <message-id>` prints `Email sent.\nMessage-ID: <id>` (via `term::success`) and exits `0`
- [ ] Each `ERR <code>` variant prints the reason prefixed with the code (e.g., `Error [DOMAIN]: sender domain does not match aimx domain`) and exits `1`
- [ ] `aimx send` refuses to run as root with `agent-setup`-style message (`send is a per-user operation — run without sudo`) and exits `2`
- [ ] CLI flags unchanged; CLI `--help` output reviewed for stale references (no mention of DKIM, signing, or MX resolution)
- [ ] Unit tests mock the UDS server side (via `tokio_test::io::Builder` or a fake `UnixListener` in a tempdir) and exercise each exit-code path
- [ ] `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt -- --check` clean

#### S35-2: End-to-end integration test (serve → UDS → signed delivery)

**Context:** Add an integration test in `tests/integration.rs` that spawns `aimx serve` as a subprocess with `--data-dir` + `AIMX_CONFIG_DIR` + `AIMX_RUNTIME_DIR` all pointing at tempdirs, waits for the UDS to exist, then invokes `aimx send` via `assert_cmd` with test arguments, and asserts: (a) the client exited `0`, (b) the server logs include `peer_uid`/`peer_pid` for the accepted send, (c) a mock MX captured the delivered message, (d) the captured message has a valid DKIM signature against the test keypair. Use the existing test-MX pattern from Sprint 20 (the `MockMailTransport` or whatever the current name is). The test runs serialized with other integration tests via the existing serial-test mechanism if one exists — otherwise use a unique port/socket per run.

**Priority:** P0

- [ ] New integration test `send_uds_end_to_end_delivers_signed_message` in `tests/integration.rs`
- [ ] Test spawns `aimx serve` as a subprocess and waits for the UDS to appear (bounded retry, max 5s)
- [ ] Test invokes `aimx send --from test@example.com --to recipient@example.com --subject "Test" --body "Hello"`
- [ ] Mock MX captures the delivered message; test asserts DKIM-Signature header present and valid against the test public key (reuse the cryptographic roundtrip helper from Sprint 25 S25-2)
- [ ] Test asserts the `aimx send` exit code is `0` and stdout contains a message-ID
- [ ] Test cleans up the spawned `aimx serve` process on both success and failure paths (drop guard or explicit teardown)
- [ ] `cargo test --test integration send_uds` runs green in ≤10s on developer machines
- [ ] `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt -- --check` clean

#### S35-3: Delete now-dead code paths + doc sweep

**Context:** With `aimx send` stripped down, several things are dead: the Sprint 25 `private_key_has_restricted_permissions` test (replaced in S33-3), any helper in `dkim.rs` that existed only to support the client signing path, and the IPv4/IPv6 outbound logic in `send.rs` (the IPv6 logic is in `serve`-side delivery). Sweep `book/`, `CLAUDE.md`, and the repo `README.md` for stale text: "aimx send signs with DKIM" → "aimx send submits via UDS; aimx serve signs"; any instruction to `chown` the DKIM key readable by a user group; any mention of `sudo aimx send` (it's now per-user). `aimx verify` is not affected by this sprint but re-confirm its docs haven't drifted.

**Priority:** P1

- [ ] Dead code deleted from `src/send.rs` and `src/dkim.rs`; `cargo clippy --all-targets -- -D warnings` clean with no `#[allow(dead_code)]` additions
- [ ] `book/getting-started.md`, `book/configuration.md`, `book/mailboxes.md` sweep — no more "aimx send loads DKIM key"
- [ ] `CLAUDE.md` `send.rs` description rewritten to reflect the UDS-client shape
- [ ] `README.md` agent-facing blurb about signing updated
- [ ] Grep for `sudo aimx send` across the whole repo returns zero hits (it's never required in v0.2)
- [ ] `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt -- --check` clean

---

## Sprint 36 — Datadir Reshape (Inbox/Sent Split, Slug, Bundles, Mailbox Lifecycle) (Days 102–104.5) [NOT STARTED]

**Goal:** Ship the final on-disk layout: `/var/lib/aimx/inbox/<mailbox>/` and `/var/lib/aimx/sent/<mailbox>/`, deterministic `YYYY-MM-DD-HHMMSS-<slug>.md` filenames, Zola-style attachment bundles, and mailbox-create that establishes both inbox + sent subdirectories. Inbound mail lands in the new layout after this sprint; outbound sent-items persistence still comes in Sprint 38.

**Dependencies:** Sprint 35 (client rewrite) — lets this sprint change filesystem shape without worrying about `send.rs` consistency.

**Design notes:**
- Slug algorithm is deterministic; two messages with the same subject in the same second collide, resolved by `-2`, `-3`, … suffix before the `.md` (within a bundle folder, the stem is reused for the `.md` so `<stem>/` and `<stem>/<stem>.md` stay in sync).
- Catchall is inbox-only (FR-9). `mailbox_list` surfaces it as a special entry or filters it — the PRD says either is acceptable; pick surfacing so agents can see it exists.
- MCP tool signatures gain optional `folder: "inbox" | "sent"` parameters (default `"inbox"`) so agents can list sent mail.
- Channel rules fire on inbound only (v1 behavior preserved); `channel.rs` paths are updated to read from `inbox/<mailbox>/` instead of `<mailbox>/`.

#### S36-1: Slug algorithm + filename helper

**Context:** Add `src/slug.rs` with `pub fn slugify(subject: &str) -> String` implementing the exact algorithm from FR-13b: MIME-decode → lowercase → replace every non-alphanumeric char with `-` → collapse runs of `-` → trim leading/trailing `-` → truncate to 20 chars → empty result becomes `no-subject`. MIME decoding uses `mail-parser`'s existing helper (whatever the current version exposes). Add `pub fn allocate_filename(dir: &Path, timestamp: DateTime<Utc>, slug: &str, has_attachments: bool) -> PathBuf` that returns the final path on disk (either `<dir>/<stem>.md` or `<dir>/<stem>/<stem>.md` inside a bundle), handling collisions with `-2`, `-3`, … suffixes. Both helpers are pure (no IO for `slugify`; `allocate_filename` only reads the directory to check collisions).

**Priority:** P0

- [ ] `src/slug.rs` created with `slugify()` and `allocate_filename()`
- [ ] Unit tests cover: ASCII subject, unicode subject with MIME decoding, all-non-alphanumeric subject (→ `no-subject`), long subject truncation to 20 chars, collapsed dash runs, trimmed leading/trailing dashes
- [ ] Collision tests: no collision → base stem; one collision → `<stem>-2`; two → `<stem>-3`; bundle collisions check the directory name (not the `.md` inside) when `has_attachments = true`
- [ ] Timestamp format in the filename is UTC `YYYY-MM-DD-HHMMSS` (6-digit time, no separators between HH/MM/SS other than what's in the format); test asserts exact string for known timestamps
- [ ] `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt -- --check` clean

#### S36-2: `aimx ingest` writes to `inbox/<mailbox>/` with new filenames + bundles

**Context:** Rewrite the filesystem-write path in `src/ingest.rs`. Today it writes `<data_dir>/<mailbox>/YYYY-MM-DD-NNN.md` with a per-day counter, and dumps attachments to `<data_dir>/<mailbox>/attachments/`. After this story, ingest routes to `<data_dir>/inbox/<mailbox>/` (or `inbox/catchall/` for unknown local parts), calls `allocate_filename()` to get the final path, and — if attachments are present — writes the `.md` plus attachment files as siblings inside the bundle directory `<stem>/`. The per-day counter disappears (filenames now carry HHMMSS). The top-level `attachments/` per mailbox is gone. Channel rules that use `{filepath}` will now see the bundle path; confirm the channel-trigger integration test from Sprint 31 still passes (if it asserts a specific filename shape, update it to the new shape).

**Priority:** P0

- [ ] `ingest.rs` writes to `<data_dir>/inbox/<mailbox>/` by default; unknown local parts route to `<data_dir>/inbox/catchall/`
- [ ] Zero-attachment emails produce a flat `<stem>.md`
- [ ] One-or-more-attachment emails produce a bundle directory `<stem>/` containing `<stem>.md` and each attachment file as a sibling; attachment filenames preserved (with the existing Sprint 2.5 escaping applied)
- [ ] `attachments/` subdirectory per mailbox is NOT created
- [ ] `channel.rs` consumers updated: `{filepath}` now expands to the `.md` inside the bundle when attachments present; `book/channels.md` documents this
- [ ] Sprint 31 integration test `channel_recipe_end_to_end_with_templated_args` still passes (update fixture assertions as needed)
- [ ] Existing ingest integration tests in `tests/integration.rs` updated for the new layout
- [ ] `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt -- --check` clean

#### S36-3: Mailbox lifecycle — create/list/delete both inbox + sent

**Context:** Update `src/mailbox.rs` (and the matching MCP tool handlers in `src/mcp.rs`): `mailbox_create(name)` creates both `inbox/<name>/` and `sent/<name>/`; `mailbox_list()` scans `inbox/*/`, lists mailbox names with counts (counting bundle-directory-matches too, not just flat `.md` files), and includes `catchall` as a special row; `mailbox_delete(name)` removes BOTH `inbox/<name>/` and `sent/<name>/` after confirmation. The config.toml mailbox registration stays unchanged. Catchall cannot be deleted (existing v1 guard preserved). The MCP tools gain an optional `folder: "inbox" | "sent"` parameter on `email_list` / `email_read` / `email_mark_read` / `email_mark_unread` (default `"inbox"`); `email_send` / `email_reply` are unaffected.

**Priority:** P0

- [ ] `mailbox_create` creates both `inbox/<name>/` and `sent/<name>/` atomically (create one, then the other; if the second fails, the first is cleaned up)
- [ ] `mailbox_list` scans `inbox/*/`, counts via a helper that handles both flat `.md` and bundle dirs, surfaces `catchall` explicitly
- [ ] `mailbox_delete` removes both `inbox/<name>/` and `sent/<name>/`; refuses to delete `catchall` (preserved v1 guard, error message mentions catchall)
- [ ] MCP tool signatures gain `folder: Option<String>` with default `"inbox"` on `email_list`, `email_read`, `email_mark_read`, `email_mark_unread`
- [ ] MCP tool handlers validate `folder` against `{"inbox", "sent"}`, returning a clear error for other values
- [ ] CLI `aimx mailbox list` output shows the inbox count; sent count is deferred to Sprint 38 (when sent-items actually exist)
- [ ] Tests cover: create creates both dirs; create is idempotent; delete removes both; delete refuses catchall; list surfaces catchall; MCP tool signatures include `folder` param; invalid folder value returns clean error
- [ ] `book/mailboxes.md` updated for the new layout; `book/mcp.md` updated for the new `folder` parameter
- [ ] `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt -- --check` clean

---

## Sprint 37 — Expanded Frontmatter Schema + DMARC Verification (Days 104.5–107) [NOT STARTED]

**Goal:** Land the full inbound frontmatter schema — new fields, section ordering, omission-vs-null discipline — and add DMARC verification alongside existing DKIM/SPF. Every inbound email written after this sprint carries the final v0.2 schema.

**Dependencies:** Sprint 36 (datadir reshape).

**Design notes:**
- `thread_id` is deterministic: same thread always produces the same 16-char hex. Walk `In-Reply-To` and `References` back to the earliest resolvable `Message-ID`; fall back to the current message's own `Message-ID` when unresolvable. SHA-256 the result, take the first 16 hex chars.
- `received_from_ip` is the SMTP client IP — the embedded SMTP listener already has this available at session time; thread it through to `ingest_email()` as a parameter.
- DMARC verification uses the same `mail-auth` crate we already use for DKIM/SPF. Add it alongside the existing checks; it does its own lookup against the sender domain.
- Field-omission rule: omit empty optional fields over writing `null`. Always-written exceptions: `dkim`, `spf`, `dmarc`, `trusted`, `read`, `delivery_status` (the latter is outbound-only, land in Sprint 38).

#### S37-1: Frontmatter struct with sectioned ordering + omission rules

**Context:** Today `src/ingest.rs` writes frontmatter via ad-hoc string formatting. Replace this with a `InboundFrontmatter` struct in `src/frontmatter.rs` (new module) using `serde` + the `toml` crate for serialization, with field ordering enforced by struct field declaration order: `id`, `message_id`, `thread_id`, `from`, `to`, `cc`, `reply_to`, `delivered_to`, `subject`, `date`, `received_at`, `received_from_ip`, `size_bytes`, `attachments`, `in_reply_to`, `references`, `list_id`, `auto_submitted`, `dkim`, `spf`, `dmarc`, `trusted`, `mailbox`, `read`, `labels`. Optional fields use `Option<T>` with `#[serde(skip_serializing_if = "Option::is_none")]`; empty collections use `Vec<T>` with `#[serde(skip_serializing_if = "Vec::is_empty")]`. `trusted` is added as a placeholder field in this sprint but always written as `"none"` — the real evaluation lands in Sprint 38. Between-section blank lines aren't preserved by TOML serializers; accept that and rely on the struct order to produce a stable, diffable output.

**Priority:** P0

- [ ] `src/frontmatter.rs` created with `InboundFrontmatter` struct; field order matches FR-13 exactly
- [ ] Optional fields serialize via `skip_serializing_if`; empty vecs do NOT appear in output
- [ ] Always-written fields (`dkim`, `spf`, `dmarc`, `trusted`, `read`): their serde attribute makes them NON-skippable even at default value
- [ ] `trusted` field placeholder always emits `"none"` in this sprint (Sprint 38 wires real evaluation)
- [ ] `ingest.rs` writes frontmatter via `toml::to_string(&frontmatter)?` between `+++` delimiters
- [ ] Golden tests: ingest a known `.eml` fixture and assert byte-for-byte frontmatter output
- [ ] Field order regression test: any reordering of struct fields changes golden output and fails the test
- [ ] `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt -- --check` clean

#### S37-2: `thread_id` computation + population of new fields

**Context:** In `src/ingest.rs`, populate the new fields from the parsed MIME message and session context: `thread_id` via a new `compute_thread_id(message_id, in_reply_to, references)` helper that walks backward to the earliest resolvable `Message-ID` and SHA-256s it (first 16 hex chars); `received_at` via `chrono::Utc::now().to_rfc3339()`; `received_from_ip` — this requires threading the SMTP client IP from `src/smtp/session.rs` through to `ingest_email()` (new parameter `received_from_ip: IpAddr`); `size_bytes` = raw message length in bytes; `delivered_to` = the actual RCPT TO (distinct from `to:` header for list mail); `list_id`, `auto_submitted` = extracted from headers if present, omitted otherwise; `labels` = always empty `Vec<String>` on ingest (agents apply labels later). `dmarc` is populated in S37-3.

**Priority:** P0

- [ ] `compute_thread_id` helper added; deterministic (same inputs → same output); SHA-256 truncated to 16 hex chars
- [ ] Resolution order: walk `In-Reply-To` first; fall back to walking `References` earliest-first; fall back to the message's own `Message-ID`
- [ ] `ingest_email()` signature gains `received_from_ip: IpAddr`; `src/smtp/session.rs` threads the peer IP through
- [ ] Manual-stdin `aimx ingest` path (no SMTP session) passes `0.0.0.0` or a documented sentinel for `received_from_ip`
- [ ] `list_id` populated from `List-ID:` header; `auto_submitted` from `Auto-Submitted:` header; both omitted when headers absent
- [ ] `size_bytes` is the raw `.eml` byte length seen at ingest
- [ ] Unit tests for `compute_thread_id`: direct reply chain, orphan message, cross-references, missing headers, header with multiple Message-IDs
- [ ] Integration test: ingest an `.eml` with known headers; frontmatter contains expected `thread_id`, `received_from_ip`, `delivered_to`, `size_bytes`
- [ ] `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt -- --check` clean

#### S37-3: DMARC verification + always-written `dmarc` field

**Context:** Extend the existing inbound auth-check flow (today: DKIM + SPF via `mail-auth`) to also run DMARC. `mail-auth`'s `Resolver` provides `verify_dmarc` (or the current API name — confirm at implementation time; the crate evolves). DMARC lookup requires the sender domain and both DKIM + SPF results as inputs — sequence accordingly. Values written to frontmatter: `"pass"` | `"fail"` | `"none"`. `"none"` means no DMARC record at the sender domain, not "check not performed"; a check that was genuinely not performed (network failure, lookup timeout) should also write `"none"` with a warning log. Keep auth results in a typed `AuthResults { dkim, spf, dmarc }` struct so Sprint 38's trust-evaluation logic has a clean input.

**Priority:** P0

- [ ] DMARC verification added to the ingest auth-check pipeline via `mail-auth`'s resolver
- [ ] `AuthResults { dkim, spf, dmarc }` struct introduced; populated once per ingest and passed to frontmatter builder
- [ ] `dmarc` value mapping: pass → `"pass"`, fail → `"fail"`, no record / lookup failure → `"none"` (failure logs at `warn` level with the lookup error)
- [ ] Frontmatter `dmarc` field always written (never omitted)
- [ ] Unit tests using `mail-auth` test fixtures: DMARC pass, DMARC fail, no DMARC record, lookup failure
- [ ] Integration test: ingest an `.eml` with known DMARC outcome (use a captured fixture); frontmatter contains expected `dmarc` value
- [ ] `book/configuration.md` documents DMARC verification alongside DKIM/SPF
- [ ] `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt -- --check` clean

---

## Sprint 38 — `trusted` Frontmatter + Sent-Items Persistence + Outbound Block (Days 107–109.5) [NOT STARTED]

**Goal:** Evaluate per-mailbox trust at ingest time and surface the result in the always-written `trusted` frontmatter field. Persist every successfully delivered outbound message to `sent/<from-mailbox>/` with the full outbound frontmatter block.

**Dependencies:** Sprint 37 (frontmatter struct, `AuthResults`), Sprint 34 (daemon-side send handler owns delivery).

**Design notes:**
- `trusted` computation (FR-37b) is explicit:
    - mailbox's `trust` config is `none` (default) → `trusted = "none"` (no evaluation performed)
    - mailbox's `trust` config is `verified`, sender matches `trusted_senders`, AND DKIM passed → `trusted = "true"`
    - mailbox's `trust` config is `verified`, any other outcome → `trusted = "false"`
  The v1 channel-trigger gating behavior is preserved verbatim; `trusted` is the surfaced result of that same evaluation, not a new policy.
- Sent-items persistence runs inside `aimx serve`'s send handler (Sprint 34's `handle_send`), after successful delivery returns. The signed message is what gets persisted (not the unsigned client submission).
- Filename allocation across concurrent sends requires a `Mutex<()>` around "check directory + create file" to avoid two sends racing to the same stem. The critical section is microseconds; the Mutex holds across at most one `fs::metadata` + one `fs::File::create` call.

#### S38-1: `trusted` frontmatter field — compute + always-write

**Context:** Add `fn evaluate_trust(mailbox: &MailboxConfig, auth: &AuthResults, from: &str) -> TrustedValue` in `src/ingest.rs` (or a new `src/trust.rs` module). `TrustedValue` is `enum { None, True, False }` serializing to `"none"`, `"true"`, `"false"`. Logic: `mailbox.trust == Trust::None` → `TrustedValue::None`; `mailbox.trust == Trust::Verified` AND `from` matches any glob in `mailbox.trusted_senders` AND `auth.dkim == DkimResult::Pass` → `TrustedValue::True`; `mailbox.trust == Trust::Verified` otherwise → `TrustedValue::False`. Note: the v1 `trusted_senders` behavior was "allowlisted senders skip verification" — that is a gate on trigger firing, preserved unchanged. The `trusted` field follows the PRD's FR-37b reading, which is stricter: `trusted == "true"` requires BOTH allowlisted AND DKIM-pass. The channel-trigger gate itself stays at v1 semantics (allowlisted OR DKIM-pass for `trust: verified`); it does NOT switch to reading the `trusted` field. This keeps the trigger gate's existing "allowlisted senders skip verification" affordance intact.

**Priority:** P0

- [ ] `TrustedValue` enum added with three variants, serializing to `"none"`, `"true"`, `"false"` lowercase
- [ ] `evaluate_trust()` implements the three-value logic exactly as specified
- [ ] Ingest pipeline calls `evaluate_trust()` and writes the result into the `trusted` frontmatter field
- [ ] Channel-trigger gate logic remains at v1 semantics — `src/channel.rs` is NOT modified to read `trusted`; it continues to evaluate allowlist + DKIM independently. Inline comment in `channel.rs` points at `evaluate_trust()` for the rationale.
- [ ] Unit tests cover every arm of `evaluate_trust()`: `trust: none` → `"none"`; `trust: verified` + allowlisted + DKIM pass → `"true"`; allowlisted + DKIM fail → `"false"`; not allowlisted + DKIM pass → `"false"`; not allowlisted + DKIM fail → `"false"`
- [ ] Parity test: for a `trust: verified` mailbox, `trusted == "true"` IFF the channel-trigger gate would fire, confirming the two pieces of logic agree
- [ ] `book/configuration.md` explains the `trusted` field semantics; `book/mcp.md` mentions it in the frontmatter reference
- [ ] `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt -- --check` clean

#### S38-2: Outbound frontmatter block (`outbound`, `bcc`, `delivered_at`, `delivery_status`, `delivery_details`)

**Context:** Define `OutboundFrontmatter` (or extend `InboundFrontmatter` with an optional outbound block — prefer a distinct struct so the required/optional split stays clean). Outbound-only fields per FR-19c: `outbound: bool` (always `true` on sent files), `bcc: Option<Vec<String>>` (only meaningful on sent copies), `delivered_at: Option<String>` (RFC 3339 UTC when remote MX accepted), `delivery_status: DeliveryStatus` (`"delivered"` | `"deferred"` | `"failed"` | `"pending"`, always written), `delivery_details: Option<String>` (last remote SMTP response). The outbound file is structurally identical to inbound (same inbound fields at top, outbound block at bottom) so a single reader can parse both by type-tagging on the presence of `outbound = true`.

**Priority:** P0

- [ ] `DeliveryStatus` enum serializes to the four lowercase strings
- [ ] `OutboundFrontmatter` struct composes `InboundFrontmatter` (identity/parties/content/threading/auth/storage) + the outbound block (outbound/bcc/delivered_at/delivery_status/delivery_details)
- [ ] Field ordering in the serialized output: inbound block first, outbound block at the end
- [ ] `delivery_status` is ALWAYS written (never omitted); other outbound fields follow omission rules
- [ ] Golden test: outbound fixture serializes byte-for-byte to the expected layout
- [ ] `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt -- --check` clean

#### S38-3: Sent-items persistence in `aimx serve` send handler

**Context:** Extend Sprint 34's `handle_send` to, after a successful delivery, write the signed message to `<data_dir>/sent/<from_mailbox>/<stem>.md` (or a bundle directory `<stem>/` when the outbound message has attachments). Filename uses the same algorithm as inbound (Sprint 36) with the send's UTC timestamp and the message's `Subject:`-derived slug. The write path: construct `OutboundFrontmatter` from the signed message + delivery result, compose the `.md` body (frontmatter + signed RFC 5322 message as the body), allocate the filename under a process-global `Mutex<()>` guarding "check directory + create file", then release the Mutex and do the actual file write outside the lock. On partial delivery failure (e.g., message was sent to some MX recipients but failed on others), write `delivery_status: "failed"` with the relevant detail — still persist the file so the operator has a record. On transient errors that the daemon retries internally (none today — v1 doesn't queue), skip persistence and return `TEMP` to the client.

**Priority:** P0

- [ ] Successful sends write `<data_dir>/sent/<from_mailbox>/<stem>.md` (or bundle directory) with `delivery_status: "delivered"` and `delivered_at` populated
- [ ] Signed RFC 5322 bytes are the body of the `.md` file (below the `+++` frontmatter block); the exact DKIM-signed message delivered to the MX is what's persisted
- [ ] `Mutex<()>` guards the filename allocation critical section only; the file-write IO happens outside the lock
- [ ] Failed sends (permanent `DELIVERY` error): write the file with `delivery_status: "failed"` and `delivery_details` carrying the last remote SMTP response
- [ ] `TEMP` errors: do NOT persist (the client will see the transient error and retry itself); inline comment documents why
- [ ] `mailbox_list` CLI output now reports sent counts alongside inbox counts
- [ ] Integration test: drive an end-to-end send via UDS; assert the sent file exists at the expected path, has the right frontmatter, and contains a DKIM signature that verifies against the public key
- [ ] Integration test for permanent-failure persistence: inject a mock MX that always rejects; assert the `.md` is written with `delivery_status: "failed"`
- [ ] `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt -- --check` clean

---

## Sprint 39 — Agent Primer as Progressive-Disclosure Skill Bundle + Author Metadata (Days 109.5–112) [NOT STARTED]

**Goal:** Restructure the shared agent primer from a single file into a main body + `references/` layout (the anthropics/skills progressive-disclosure pattern), standardize author metadata across every agent package, and reverse the earlier storage-exposure redaction policy so the primer documents the datadir layout explicitly.

**Dependencies:** Sprint 38 (frontmatter schema finalized — the primer documents it, so the primer can't ship before the schema is stable).

**Design notes:**
- Progressive disclosure is a file-layout convention, not a runtime behavior. Claude Code and Codex agents read the main `SKILL.md` by default and load `references/*.md` on demand when the main file points at them. Agents that take a single blob (Goose recipes, Gemini prompts) get only the main primer bundled — the install-time concat step decides per-platform.
- The `install-time concat` extension in `agent_setup.rs` gains a `<platform-suffix>.footer` input (optional, new) and a `references/` copy step. Per-agent registry entries declare whether they support progressive disclosure.
- Storage-exposure reversal (FR-50c): the primer documents the datadir layout plainly. There's no security boundary to protect by concealing it — the datadir is world-readable by design, and the real boundary (DKIM key, UDS socket) is enforced elsewhere.

#### S39-1: Split `agents/common/aimx-primer.md` into main + `references/`

**Context:** Today `agents/common/aimx-primer.md` is one file. Split it: the new main `aimx-primer.md` targets 300–500 lines and covers identity/purpose, the two access surfaces (MCP for writes + direct FS reads), quick-reference summaries of the 9 MCP tools with their signatures, the frontmatter fields agents most often check (`trusted`, `thread_id`, `list_id`, `auto_submitted`, `read`, `labels`), the 4–5 most common workflows inline (check inbox, send, reply, summarize a thread, handle auto-submitted mail), a short trust-model overview (per-mailbox `trust` + `trusted_senders` + the `trusted` frontmatter surface), pointers to `references/*.md`, a pointer to the runtime `/var/lib/aimx/README.md`, and a "what you must not do" safety list. Deep material moves into `agents/common/references/{mcp-tools,frontmatter,workflows,troubleshooting}.md`. `mcp-tools.md` carries full signatures, parameter types, and at least one worked example per tool; `frontmatter.md` carries every field with type + required/optional + notes + the outbound block; `workflows.md` carries 8–12 worked tasks (triage inbox, thread summarization, react to auto-submitted mail, handle attachments, reply-all, filter by list-id, ingest a bounce, mark all-read, etc.); `troubleshooting.md` carries the UDS-protocol error codes, common misconfigurations, and recovery steps.

**Priority:** P0

- [ ] `agents/common/aimx-primer.md` rewritten — 300–500 lines (soft cap; enforce via a line-count comment or PR review)
- [ ] `agents/common/references/mcp-tools.md`, `frontmatter.md`, `workflows.md`, `troubleshooting.md` created with the content described above
- [ ] Main primer explicitly links `references/` files and the runtime `/var/lib/aimx/README.md`
- [ ] Main primer documents the storage layout plainly (FR-50c reversal); inline comment cites FR-50c
- [ ] `trusted` field documented in the frontmatter quick-reference AND in `references/frontmatter.md` with the three values and the per-mailbox evaluation logic
- [ ] All references to removed/renamed v1 paths purged (grep for `/var/lib/aimx/<mailbox>/` outside of `inbox/`/`sent/` context)
- [ ] Byte-level test that asserts the main primer's line count stays within the target range (prevents future bloat)
- [ ] `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt -- --check` clean

#### S39-2: `agent_setup.rs` install-time concat — `<suffix>.footer` + `references/` copy

**Context:** Extend `src/agent_setup.rs` to support two new concat inputs. First, an optional `<platform>.footer` file appended after the common primer body at install time (the existing `<platform>.header` is prepended — this sprint adds the symmetric suffix). Second, a per-agent registry flag `progressive_disclosure: bool`; when `true`, installer copies `agents/common/references/*.md` to `<destination>/references/*.md` verbatim; when `false`, installer skips the copy but may optionally inline selected reference snippets into the main primer at install time (Goose recipes are the motivating case, where context budget is tighter — for v0.2 we skip inlining and just ship the main primer; inlining is a future enhancement). Per-agent registry update:
    - Claude Code, Codex, OpenClaw → `progressive_disclosure: true`
    - Goose, Gemini, OpenCode → `progressive_disclosure: false`

**Priority:** P0

- [ ] `AgentSpec` registry struct gains `progressive_disclosure: bool` (and `suffix_filename: Option<&str>` if `.footer` is used)
- [ ] Install flow: header + common primer + optional footer → SKILL.md; references copied only when `progressive_disclosure: true`
- [ ] Per-agent `progressive_disclosure` assignments made per the design note above
- [ ] Byte-level test for a progressive-disclosure agent: install lays down `SKILL.md` + `references/` tree, contents match fixtures
- [ ] Byte-level test for a non-progressive-disclosure agent: install lays down single-blob output with references absent
- [ ] `--print` mode emits both `SKILL.md` and `references/` files for progressive-disclosure agents; only `SKILL.md` for others
- [ ] `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt -- --check` clean

#### S39-3: Author metadata standardization + repo-wide grep verification

**Context:** Every shipped agent package carries an author field. Today several carry `"AIMX"` or a placeholder from an earlier draft. Standardize to `U-Zyn Chua <chua@uzyn.com>` across: `agents/claude-code/.claude-plugin/plugin.json`, `agents/codex/.codex-plugin/plugin.json`, `agents/goose/aimx.yaml.header` (if the Goose recipe schema carries an author field — confirm at implementation time; if not, skip), `agents/opencode/SKILL.md.header`, `agents/gemini/SKILL.md.header`, `agents/openclaw/SKILL.md.header`. A CI-runnable repo-wide grep asserts no `"AIMX"` author strings or placeholder emails remain under `agents/`.

**Priority:** P1

- [ ] All six agent packages carry `U-Zyn Chua <chua@uzyn.com>` in their author field
- [ ] Goose: if the recipe schema supports `author`, it's populated; if not, inline comment in `agents/goose/aimx.yaml.header` notes the gap
- [ ] `.github/workflows/ci.yml` gains a grep step that fails if `"AIMX"` or placeholder author strings appear under `agents/` (pattern: literal `"author": "AIMX"` and `<chua@example.com>` style placeholders)
- [ ] Existing agent integration tests pass; install-layout assertions updated if they captured the author string
- [ ] `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt -- --check` clean

---

## Sprint 40 — Datadir `README.md` + Journald Docs + Book/ Pass (Days 112–114.5) [NOT STARTED]

**Goal:** Ship the baked-in `/var/lib/aimx/README.md` agent-facing layout guide (versioned, auto-refreshed on daemon startup), replace stale `/var/log/aimx.log` references with `journalctl` commands, and bring every affected `book/` chapter and `CLAUDE.md` up to date with the v0.2 reshape. Last sprint before launch.

**Dependencies:** Sprint 39 (primer structure finalized — the datadir README references the same schema).

**Design notes:**
- Datadir README carries `<!-- aimx-readme-version: N -->` as the first line. Comparison is exact string match, not semver. Bump `N` by 1 whenever the template changes. Refresh triggers: `aimx setup` (always writes), `aimx serve` startup (writes only if the version line differs from the on-disk version line).
- Book/ pass is a docs-only sprint from AIMX's perspective but touches many files. The intent is "no stale `/var/lib/aimx/<mailbox>/`-style references anywhere user-facing." Grep the whole `book/` tree and `README.md` after edits.
- `CLAUDE.md` updates: new module descriptions for `src/send_protocol.rs`, `src/send_handler.rs`, `src/slug.rs`, `src/frontmatter.rs`, `src/datadir_readme.rs`; updated descriptions for `send.rs` (now thin UDS client) and `serve.rs` (now owns DKIM signing + send handler); updated storage conventions section.

#### S40-1: `src/datadir_readme.rs` — template, write, version-gate refresh

**Context:** New module `src/datadir_readme.rs` with: `pub const TEMPLATE: &str = include_str!("datadir_readme.md.tpl");`, `pub const VERSION: u32 = 1;`, `pub fn write(data_dir: &Path)` (writes unconditionally), `pub fn refresh_if_outdated(data_dir: &Path)` (reads existing file, parses the first-line version comment, writes only if differs from `VERSION`). The template file `src/datadir_readme.md.tpl` begins with `<!-- aimx-readme-version: 1 -->` and carries: what the directory is, read vs write access model, directory layout with the v0.2 tree, file naming rules, slug algorithm, bundle rule, frontmatter reference (link to the full spec in `agents/common/references/frontmatter.md` plus an inlined quick-reference), trust/DKIM/SPF/DMARC explanation, thread grouping, handling auto-submitted/list mail, attachments, the UDS send protocol summary, and a pointer to the `aimx` MCP server for all mutations. Top of the file states: "This file is regenerated on AIMX upgrade. User edits will be overwritten."

**Priority:** P0

- [ ] `src/datadir_readme.rs` and `src/datadir_readme.md.tpl` created
- [ ] Version bump procedure documented in a `// VERSION BUMP:` comment at the top of `datadir_readme.rs`
- [ ] `write()` writes the template verbatim to `<data_dir>/README.md` with mode `0o644`
- [ ] `refresh_if_outdated()` parses the first line; if the version comment is missing, malformed, or differs from `VERSION`, overwrite; otherwise no-op
- [ ] `aimx setup` calls `write()` at the end of setup
- [ ] `aimx serve` startup calls `refresh_if_outdated()` before binding listeners
- [ ] Unit tests: `write` creates the file; `refresh` no-op when version matches; `refresh` overwrites when version differs; `refresh` overwrites when first line is missing or malformed
- [ ] Integration test: run `aimx serve` in a tempdir with a stale README; assert it's refreshed at startup
- [ ] `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt -- --check` clean

#### S40-2: Journald documentation + `/var/log/aimx.log` purge

**Context:** `book/troubleshooting.md` still has stale `tail -f /var/log/aimx.log` examples from before `aimx serve` landed — `aimx` has always logged to journald/OpenRC-logger, not to that path. Replace with `journalctl -u aimx -f`, `journalctl -u aimx --since today`, `journalctl -u aimx -n 200`. Add a "Where are the logs?" subsection explaining: systemd → journald; OpenRC → whatever the OpenRC init script configures (document the actual path written by `src/serve.rs`'s init script). `book/channel-recipes.md` has user-authored `/var/log/aimx/<agent>.log` paths in trigger examples — these are legitimate destinations the user chooses for their own trigger scripts, not aimx's own logs; add a header note clarifying this.

**Priority:** P1

- [ ] `book/troubleshooting.md`: every `/var/log/aimx.log` occurrence replaced with `journalctl -u aimx` commands
- [ ] `book/troubleshooting.md`: new "Where are the logs?" subsection covering systemd + OpenRC
- [ ] `book/channel-recipes.md`: header note distinguishing user-chosen trigger-log paths from aimx's own logs
- [ ] Grep confirms `/var/log/aimx.log` appears nowhere under `book/`, `docs/`, or `README.md`
- [ ] `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt -- --check` clean

#### S40-3: Book/ v0.2 pass + `CLAUDE.md` rewrite

**Context:** Sweep every `book/` chapter for v0.2 changes:
    - `book/getting-started.md`: world-readable datadir security note (single-admin assumption), new directory paths, `aimx` group membership requirement for `aimx send`
    - `book/configuration.md`: config now at `/etc/aimx/config.toml`; document DKIM key at `/etc/aimx/dkim/`; `[trust]` section is unchanged from v1 (still per-mailbox); point at `book/channels.md` for the trust model
    - `book/setup.md`: `aimx` group creation step; `usermod -aG aimx <user>` instruction; re-run setup still re-entrant
    - `book/mailboxes.md`: new `inbox/` + `sent/` layout; bundle rule; naming convention with UTC timestamp + slug; `mailbox list` now shows both inbox and sent counts
    - `book/mcp.md`: new `folder: "inbox" | "sent"` parameter on read/list tools; updated frontmatter field reference; pointer to `agents/common/aimx-primer.md` and the datadir README
    - `book/channels.md`: channel rules fire on inbound only; trust gate semantics unchanged from v1
    - `book/agent-integration.md`: primer-as-skill-bundle note for per-platform pages; `references/` copy behavior for Claude Code + Codex + OpenClaw; single-blob behavior for Goose + Gemini + OpenCode

`CLAUDE.md` updates: new module descriptions (`send_protocol`, `send_handler`, `slug`, `frontmatter`, `datadir_readme`); updated descriptions for `send.rs` (thin UDS client), `serve.rs` (binds UDS, owns signing), `ingest.rs` (new frontmatter schema, inbox/ routing, bundles); updated Key Conventions section.

**Priority:** P0

- [ ] Every `book/*.md` chapter listed above is updated with the v0.2 details
- [ ] `CLAUDE.md` module descriptions regenerated — old ones removed, new ones added in the right order
- [ ] Repo-wide grep for `/var/lib/aimx/<mailbox>/` (without `inbox/` or `sent/` prefix) returns zero hits in `book/`, `docs/`, `README.md`
- [ ] Repo-wide grep for `aimx send` under `book/` never mentions `sudo aimx send` and always mentions the `aimx` group requirement in a nearby paragraph
- [ ] `book/agent-integration.md` table of supported agents extended with a "progressive disclosure" column
- [ ] Spot-check every `agents/<agent>/README.md` for drift against the new primer layout; update as needed
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
| 34 | 97–99.5 | v0.2 UDS Wire Protocol + Daemon Send Handler | `src/send_protocol.rs` codec, `aimx serve` binds `/run/aimx/send.sock` (`0o666` world-writable), per-connection handler signs + delivers with `SO_PEERCRED` logged for diagnostics only | In Progress |
| 35 | 99.5–102 | v0.2 Thin UDS Client + End-to-End | `aimx send` rewritten as UDS client (no DKIM access), end-to-end integration test from client → signed delivery, dead-code + docs sweep | Not started |
| 36 | 102–104.5 | v0.2 Datadir Reshape | `inbox/` + `sent/` split per mailbox, `YYYY-MM-DD-HHMMSS-<slug>.md` filenames, Zola-style attachment bundles, mailbox lifecycle touches both trees, MCP `folder` param | Not started |
| 37 | 104.5–107 | v0.2 Frontmatter Schema + DMARC | `InboundFrontmatter` struct with section ordering, new fields (`thread_id`, `received_at`, `received_from_ip`, `size_bytes`, `delivered_to`, `list_id`, `auto_submitted`, `dmarc`, `labels`), DMARC verification | Not started |
| 38 | 107–109.5 | v0.2 `trusted` Field + Sent-Items Persistence | Always-written `trusted: "none"\|"true"\|"false"` (v1 trust model preserved), sent mail persisted to `sent/<mailbox>/` with outbound block + `delivery_status` | Not started |
| 39 | 109.5–112 | v0.2 Primer Skill Bundle + Author Metadata | `agents/common/aimx-primer.md` split into main + `references/`, install-time suffix + references-copy, `U-Zyn Chua <chua@uzyn.com>` standardized repo-wide | Not started |
| 40 | 112–114.5 | v0.2 Datadir README + Journald + Book/ | Baked-in `/var/lib/aimx/README.md` with version-gate refresh on `aimx serve` startup, `journalctl -u aimx` replaces stale `/var/log/aimx.log`, full `book/` + `CLAUDE.md` pass | Not started |

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

### Questions

Items needing human judgment. Answer inline by replacing the `_awaiting answer_` text, then check the box.

- [x] **(Sprint 2.5)** `serde_yaml` 0.9 is unmaintained/deprecated — should we migrate to an alternative YAML serializer? — Migrate to TOML (`toml` crate) instead. _Triaged into Sprint 9_

### Improvements

Concrete items with clear implementation direction. Will be triaged into a cleanup sprint periodically.

- [x] **(Sprint 1)** Add `--data-dir` or `AIMX_DATA_DIR` CLI option to override the hardcoded `/var/lib/aimx/` path — _Triaged into Sprint 2.5_
- [x] **(Sprint 1)** Enhance integration tests to exercise `ingest_email()` with fixture files through the full pipeline, not just `mail-parser` parseability — _Triaged into Sprint 2.5_
- [x] **(Sprint 1)** Add mailbox name validation to prevent `..`, `/`, or empty strings in `create_mailbox` — _Triaged into Sprint 2.5_
- [x] **(Sprint 1)** Replace hand-rolled `yaml_escape` with `serde_yaml` struct serialization for frontmatter to avoid edge cases (YAML booleans, special characters) — _Triaged into Sprint 2.5_
- [x] **(Sprint 1)** Add `\r` to the quoting condition in `yaml_escape` for hardening (bare `\r` not exploitable but inconsistent) — _Triaged into Sprint 2.5_
- [x] **(Sprint 2)** Escape attachment filenames in MIME `Content-Type`/`Content-Disposition` headers to prevent malformed headers from special characters — _Triaged into Sprint 2.5_
- [x] **(Sprint 2)** Add integration test for `aimx dkim-keygen` CLI command end-to-end (subprocess test) — _Triaged into Sprint 2.5_
- [x] **(Sprint 2)** Refactor duplicated header construction logic in `compose_message()` attachment vs non-attachment paths — _Triaged into Sprint 2.5_
- [x] **(Sprint 2)** Add test verifying `dkim_selector` config value is actually used at runtime in `send::run()` — _Triaged into Sprint 2.5_
- [x] **(Sprint 2.5)** Replace `unwrap_or_default()` on `serde_yaml::to_string()` with `expect()` or error propagation to avoid silent empty frontmatter on serialization failure — _Triaged into Sprint 5.5_
- [x] **(Sprint 3)** Narrow `tokio` features from `"full"` to specific needed features (`rt-multi-thread`, `macros`, `io-util`, `io-std`) for smaller binary — _Triaged into Sprint 5.5_
- [x] **(Sprint 3)** Add unit test for `write_common_headers` with `references = Some(...)` path — _Triaged into Sprint 5.5_
- [x] **(Sprint 4)** Deduplicate DNS resolver creation in `verify_dkim_async` and `verify_spf_async` — create once and pass to both — _Triaged into Sprint 5.5_
- [x] **(Sprint 4)** Fix SPF domain fallback semantics — `sender_domain` derived from `rcpt` is semantically incorrect as fallback for sender's HELO domain — _Triaged into Sprint 5.5_
- [x] **(Sprint 4)** Add captured DKIM-signed `.eml` fixture from Gmail for verification testing (even if DNS-dependent) — _Triaged into Sprint 5.5_
- [x] **(Sprint 4)** Verify `mail-auth` `dkim_headers` field is stable public API, not internal implementation detail — _Triaged into Sprint 5.5_
- [x] **(Sprint 5)** Implement timestamped backup for pre-aimx OpenSMTPD config to avoid overwriting on repeated setup runs — _Triaged into Sprint 5.5_
- [x] **(Sprint 5.5)** Extract SPF domain-selection logic into standalone testable function instead of duplicating inline in tests — _Triaged into Sprint 8 (S8.3)_
- [x] **(Sprint 6)** Fix GitHub URL in README.md and services/verify/README.md (currently wrong owner) — _Triaged into Sprint 8 (S8.6)_
- [x] **(Sprint 6)** Add IP validation on `/probe` endpoint to reject private/internal IPs (SSRF hardening) — _Obsolete: `ip` parameter removed in Sprint 10 (S10.4)_
- [x] **(Sprint 6)** Handle multiline (folded) Authentication-Results headers in `extract_auth_result` — _Obsolete: echo removed in Sprint 10 (S10.1)_
- [x] **(Sprint 6)** Add `Message-ID` and `Date` headers to echo reply (RFC 5322 compliance) — _Obsolete: echo removed in Sprint 10 (S10.1)_
- [x] **(Sprint 6)** Handle missing catchall mailbox gracefully in `aimx verify` — _Triaged into Sprint 7 (S7.4)_
- [x] **(Sprint 8)** Add `ip6:` mechanism support to `spf_contains_ip()` for IPv6 server addresses — _Triaged into Sprint 26, implemented_
- [x] **(Sprint 8)** Quote data dir path in `generate_smtpd_conf` MDA command to handle paths with spaces — _Obsolete: `generate_smtpd_conf` removed in Sprint 22_
- [x] **(Sprint 11)** `parse_port25_status` uses `smtpd` substring match which could misidentify non-OpenSMTPD processes — _Obsolete: OpenSMTPD-specific port parsing removed in Sprint 22_
- [x] **(Sprint 11)** Dead `Fail` branch for PTR in `verify.rs` — _Obsolete: `check_ptr()` is no longer called from `verify.rs`; moved to `setup.rs` where the `Fail` arm is a defensive match on the `PreflightResult` enum_
- [x] **(Sprint 12)** `run_smtp_listener` spawns per-accept with no concurrency bound — _Triaged into Sprint 32 (S32-1)_
- [x] **(Sprint 12)** Cosmetic: `smtp_session` writer destructuring — _Triaged into Sprint 32 (S32-6)_
- [x] **(Sprint 18)** `setup_with_domain_arg_skips_prompt` test passes `None` as `data_dir` and has a tautological assertion — _Fixed: test now uses `TempDir` and asserts meaningful port 25 failure_
- [x] **(Sprint 18)** `is_already_configured` uses `c.contains(domain)` substring match for smtpd.conf domain detection — _Obsolete: smtpd.conf detection removed in Sprint 22; `is_already_configured` now checks aimx service status_
- [x] **(Sprint 19)** `deliver_message()` clones DATA payload per recipient — _Triaged into Sprint 32 (S32-2)_
- [x] **(Sprint 20)** `LettreTransport` `last_error` only retains final MX failure — _Triaged into Sprint 32 (S32-2)_
- [x] **(Sprint 20)** `extract_domain` handles `"Display Name <user@domain>"` format divergence with `lettre::Address::parse` — _Obsolete: `send.rs` now manually strips `<>` before parsing, mitigating the divergence; all call sites pass bare addresses_
- [x] **(Sprint 21)** Inconsistent TLS file check in `can_read_tls` — _Triaged into Sprint 32 (S32-3)_
- [x] **(Sprint 22)** `restart_service()` / `is_service_running()` hardcode `systemctl` — _Triaged into Sprint 32 (S32-3)_
- [x] **(Sprint 22)** `_domain` parameter in `is_already_configured` is unused — _Triaged into Sprint 32 (S32-3)_
- [x] **(Sprint 24)** `CLAUDE.md` line 68 still says `setup.rs also contains run_preflight for aimx preflight` but `run_preflight` no longer exists — _Fixed: updated to reference `run_setup` and `display_deliverability_section`_
- [x] **(Sprint 24)** `docs/manual-setup.md` line 14: "provides two functions, all exposed" — _Fixed: "all" → "both"_
- [x] **(Sprint 24)** `docs/prd.md` NFR-5: "aimx ingest" in prose without backticks — _Fixed: added backticks_
- [x] **(Sprint 26)** `get_server_ip()` and `get_server_ipv6()` each invoke `hostname -I` separately — _Triaged into Sprint 32 (S32-4)_
- [x] **(Sprint 27)** `cargo clippy --all-targets -- -D warnings` pre-existing test-target lints + adopt `--all-targets` in CI — _Triaged into Sprint 32 (S32-5)_
- [x] **(Sprint 28)** Manual end-to-end validation of the Claude Code plugin on a real machine — _Scheduled for validation in Sprint 33.1 (Claude Code installed on current validation machine)._
- [x] **(Sprint 29)** Manual end-to-end validation of the Codex CLI plugin on a real machine — _Scheduled for validation in Sprint 33.1 (Codex CLI installed on current validation machine)._
- [x] **(Sprint 29)** Manual end-to-end validation of the Gemini CLI skill on a real machine — _Deferred — requires manual validation on a real machine with Gemini CLI installed (not currently available). Schema drift can be patched in a follow-up if/when validation happens._
- [x] **(Sprint 30)** Manual end-to-end validation of the Goose recipe on a real machine — _Deferred — requires manual validation on a real machine with Goose installed (not currently available). Schema drift can be patched in a follow-up if/when validation happens._
- [x] **(Sprint 30)** Manual end-to-end validation of the OpenClaw skill on a real machine — _Covered by Sprint 33.1 S33.1-7 (OpenClaw research + `aimx agent-setup openclaw` reshape)._
- [x] **(Sprint 30, nice-to-have)** Add a second `indent_block` test with multi-line input missing a trailing newline in `src/agent_setup.rs` — _Triaged into Sprint 33.1 (S33.1-6)._
- [x] **(Sprint 31)** Add `book.toml` and wire `mdbook build` into CI so cross-link resolution is actually enforced — _Out of scope for v0.2 — deferred by user decision (2026-04-15)._
- [x] **(Sprint 31, nice-to-have)** Swap OpenClaw's limitation note in `book/channel-recipes.md` for a real recipe once OpenClaw ships a non-interactive `run`/`exec` CLI — _Covered by Sprint 33.1 S33.1-7 (research OpenClaw CLI; wire real recipe if non-interactive mode exists, otherwise reshape `aimx agent-setup openclaw` to print manual instructions)._
- [x] **(Sprint 32, nice-to-have)** `RealNetworkOps::check_ptr_record` still calls `get_server_ips()` internally — _Obsolete: PTR code entirely removed in Sprint 33.1 (S33.1-1)._
- [x] **(Sprint 33)** `verify::run_verify` still accepts a `data_dir: Option<&Path>` parameter — _Triaged into Sprint 33.1 (S33.1-3)._
- [x] **(Sprint 33)** Factor the `is_root()` helper out of `setup.rs` and `verify.rs` into a single shared location — _Triaged into Sprint 33.1 (S33.1-4)._
- [x] **(Sprint 33)** `SystemOps::create_system_group` real-impl path has no direct unit coverage — _Obsolete: `create_system_group` entirely removed in Sprint 33.1 (S33.1-2)._
- [x] **(Sprint 33)** Several runtime subcommands accept `_data_dir: Option<&std::path::Path>` as a CLI-dispatch uniformity convenience — _Triaged into Sprint 33.1 (S33.1-5)._

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
