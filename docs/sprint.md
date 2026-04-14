# AIMX — Sprint Plan

**Sprint cadence:** 2.5 days per sprint
**Team:** Solo developer with heavy AI augmentation (Claude Code)
**Total sprints:** 31 (6 original + 2 post-audit hardening + 1 YAML→TOML migration + 2 verifier/setup overhaul + 2 post-Sprint-11 bug fixes + 2 verifier ops + 1 deployment + 1 service rename + 1 setup UX + 5 embedded SMTP + 1 verify cleanup + 1 DKIM permissions fix + 1 IPv6 support + 1 systemd unit hardening + 3 agent integration + 1 channel-trigger cookbook)
**Timeline:** ~90.5 calendar days
**v1 Scope:** Full PRD scope including verifier service. Sprint 1 targets earliest possible idea validation on a real VPS. Sprints 7–8 address findings from post-v1 code review audit. Sprints 10–11 overhaul the verifier service (remove email echo, add EHLO probe) and rewrite the setup flow (root check, MTA conflict detection, install-before-check). Sprints 12–13 fix critical bugs found during post-Sprint-11 debugging: Caddy self-probe loop / XFF SSRF risk in the verifier service, and the preflight chicken-and-egg problem on fresh VPSes. Sprints 14–15 are review-driven operational quality work on the verifier service (request logging, Docker packaging). Sprint 17 renames the verify service to verifier across all code, Docker, CI, and documentation. Sprints 19–23 replace OpenSMTPD with an embedded SMTP server (hand-rolled tokio listener for inbound, lettre + hickory-resolver for outbound) and update all documentation, making aimx a true single-binary solution with no external runtime dependencies and cross-platform Unix support. Sprint 24 cleans up `aimx verify` (EHLO-only checks, sudo requirement, remove `/reach` endpoint, AIMX capitalization). Sprint 27 hardens the generated systemd unit with restart rate-limiting, resource limits, and network-readiness dependencies. Sprints 28–30 ship per-agent integration packages (Claude Code, Codex CLI, OpenCode, Goose, OpenClaw) plus the `aimx agent-setup <agent>` installer that drops a plugin/skill/recipe into the agent's standard location without mutating its primary config. Sprint 31 adds a channel-trigger cookbook covering email→agent invocation patterns for every supported agent.

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

## Sprint 27 — Systemd Unit Hardening (Days 76–78.5) [NOT STARTED]

**Goal:** Harden the systemd unit generated by `aimx setup` with proper restart rate-limiting, resource limits, and network-readiness dependencies. Systemd only at this stage — the OpenRC script stays untouched.

**Dependencies:** Sprint 26

#### S27-1: Harden `generate_systemd_unit` with restart + daemon settings

**Context:** `generate_systemd_unit()` in `src/serve.rs:101` emits a minimal unit with `Restart=on-failure` and `RestartSec=5s` but lacks restart rate-limiting (a misconfigured install could restart-loop indefinitely), resource limits (SMTP concurrency headroom), and proper network-readiness (`After=network.target` returns before DNS is usable, which matters for outbound MX resolution on cold boot). Update the template to add: `StartLimitBurst=5` + `StartLimitIntervalSec=60s` (rate-limit restarts), `LimitNOFILE=65536` + `TasksMax=4096` (resource limits), `After=network-online.target nss-lookup.target` + `Wants=network-online.target` (network readiness), and `ReadWritePaths={data_dir}` (forward-compat for future sandboxing — no-op without `ProtectSystem=`, but emitting it now avoids another rewrite later). Do NOT add `ExecReload=/bin/kill -HUP $MAINPID` — `aimx serve`'s signal handler (`src/serve.rs:77–79`) listens on SIGTERM/SIGINT only, no SIGHUP reload exists, so an `ExecReload` directive would be a lie. Do NOT add `StateDirectory=aimx` — it forces systemd to create/manage `/var/lib/aimx`, which conflicts with `--data-dir` overrides (setup already creates the data dir with correct ownership for DKIM keys). Do NOT touch `generate_openrc_script()` — OpenRC is out of scope for this sprint. Do NOT switch to a non-root user + `CAP_NET_BIND_SERVICE`; running as root stays (DKIM key ownership, port 25 binding, data-dir writes). Upgrade path for existing installations: users re-run `sudo aimx setup` — re-entrant detection in `setup.rs` already handles "aimx service already running," so no new CLI surface is needed.

**Priority:** P1

- [ ] `generate_systemd_unit()` in `src/serve.rs` emits the new template with `StartLimitBurst=5`, `StartLimitIntervalSec=60s`, `LimitNOFILE=65536`, `TasksMax=4096`, `After=network-online.target nss-lookup.target`, `Wants=network-online.target`, and `ReadWritePaths={data_dir}`
- [ ] `Restart=on-failure`, `RestartSec=5s`, `Type=simple`, `StandardOutput=journal`, `StandardError=journal`, and the `[Install]` section (`WantedBy=multi-user.target`) preserved
- [ ] `ExecReload` NOT added (no SIGHUP handler); `StateDirectory=` NOT added (conflicts with `--data-dir`); `generate_openrc_script()` untouched
- [ ] Existing test `systemd_unit_contains_required_fields` (at `src/serve.rs:158`) extended to assert every new field
- [ ] Existing test `systemd_unit_custom_paths` (at `src/serve.rs:172`) still passes with the new template
- [ ] New test asserts `ReadWritePaths=` substitutes the `data_dir` argument (e.g., `generate_systemd_unit(..., "/custom/dir")` contains `ReadWritePaths=/custom/dir`)
- [ ] `install_service_file()` in `src/setup.rs:129` still passes its existing tests — no code change expected in `setup.rs`
- [ ] `cargo test`, `cargo clippy -- -D warnings`, `cargo fmt -- --check` all clean
- [ ] `book/troubleshooting.md` mentions `systemctl reset-failed aimx` for clearing a rate-limited service that hit `StartLimitBurst`
- [ ] Live validation on `vps-198f7320`: `sudo aimx setup agent.zeroshot.lol` (re-entrant), confirm `/etc/systemd/system/aimx.service` contains the new directives via `systemctl cat aimx`, `systemctl status aimx` is healthy, `systemd-analyze verify /etc/systemd/system/aimx.service` returns no warnings <!-- Deferred: requires live VPS; not CI-testable -->

---

## Sprint 28 — Agent Integration Framework + Claude Code (Days 79–81.5) [NOT STARTED]

**Goal:** Stand up the `agents/` tree and the `aimx agent-setup <agent>` command, and ship the Claude Code integration end-to-end as the reference implementation. Establishes the pattern all subsequent agents plug into.

**Dependencies:** Sprint 27.

**Design notes (apply to all stories below):**
- `aimx agent-setup` runs as the current user. It writes to `$HOME` / `$XDG_CONFIG_HOME`-based locations only — never `/etc` or `/var`, never requires root.
- Plugin source trees live at `agents/<agent>/` in the repo and are embedded into the binary at compile time via `include_dir!` (MIT/Apache-2.0) so install works offline.
- The installer never mutates the agent's own primary config file. On success it prints the exact activation command the user should run (or a "plugin auto-discovered on next launch" hint if the agent picks it up from a known dir).
- `--force` overwrites existing destination files without prompting. `--print` writes all plugin contents to stdout and performs no disk writes (for dry-run and CI).
- Tests use `TempDir` + `HOME` override; no real agent CLI required.

#### S28-1: `agents/common/aimx-primer.md` — canonical agent-facing primer

**Context:** Before authoring any per-agent skill/recipe, AIMX needs a single canonical document describing how an LLM should think about and interact with AIMX — written for the agent, not the human operator. Each per-agent package re-wraps this primer in its native format (`SKILL.md`, Goose recipe `prompt`, OpenClaw skill, etc.) via `include_str!` at compile time so there's no drift. Content must be concrete, concise, LLM-friendly (no marketing): the nine MCP tools (`mailbox_create/list/delete`, `email_list/read/send/reply`, `email_mark_read/unread`) with parameters, the storage layout (`/var/lib/aimx/<mailbox>/YYYY-MM-DD-NNN.md`, `attachments/`), the TOML-frontmatter fields (`id`, `message_id`, `from`, `to`, `subject`, `date`, `in_reply_to`, `references`, `attachments`, `mailbox`, `read`, `dkim`, `spf`), read/unread semantics, mailbox naming, and the trust model (DKIM/SPF verification results stored in frontmatter, not gating reads).

**Priority:** P0

- [ ] `agents/common/aimx-primer.md` created with sections: Tools, Storage layout, Frontmatter, Mailboxes, Read/unread, Trust model
- [ ] Each MCP tool documented with its parameter names and types, matching `src/mcp.rs` exactly (no drift)
- [ ] Frontmatter section lists every field and its semantics; matches `ingest.rs` output
- [ ] No forward references to unimplemented features; grep for "TODO" / "FIXME" returns nothing
- [ ] Length < 300 lines (LLM context budget); reviewed for tone (instructional, not promotional)

#### S28-2: `agents/claude-code/` plugin package

**Context:** Claude Code plugin format is a directory containing `.claude-plugin/plugin.json` (manifest with optional `mcpServers` block) and `skills/<name>/SKILL.md` (skill with YAML frontmatter). The plugin's MCP entry points at the installed `aimx` binary (default `/usr/local/bin/aimx` — match how `aimx setup` already hard-codes this path in `display_mcp_section`). The skill re-wraps `agents/common/aimx-primer.md` with Claude Code's required frontmatter (`name`, `description`). Before writing the manifest, verify the current Claude Code plugin schema against official docs — the research memo in this task may be stale.

**Priority:** P0

- [ ] `agents/claude-code/.claude-plugin/plugin.json` exists with `name: "aimx"`, `description`, `version` (tracks binary version), `author`, and `mcpServers.aimx` entry (`command: "/usr/local/bin/aimx"`, `args: ["mcp"]`; honor `--data-dir` override when setup used a non-default path by allowing the user to re-run `aimx agent-setup claude-code --data-dir <path>`)
- [ ] `agents/claude-code/skills/aimx/SKILL.md` exists with valid frontmatter (`name: aimx`, `description`) and body = `agents/common/aimx-primer.md` content (assembled at build time, not duplicated on disk; choose one of: build script concatenation, `include_str!` inside binary, or a pre-commit hook — pick simplest)
- [ ] `agents/claude-code/README.md` is a short human-facing README pointing at `aimx agent-setup claude-code`
- [ ] Plugin loads cleanly in Claude Code on a real machine (manual validation); MCP tools appear; the skill is discoverable
- [ ] Plugin schema verified against current Claude Code plugin docs (link the doc URL in the README)

#### S28-3: `src/agent_setup.rs` + `aimx agent-setup` CLI command

**Context:** New module + subcommand. The module owns: (a) an embedded assets bundle covering `agents/` via `include_dir!`, (b) an agent registry table keyed by short name (`claude-code`) mapping to (source subtree, destination path template, activation hint), (c) the install routine (resolve destination under `$HOME` / `$XDG_CONFIG_HOME`, walk embedded source, write files with `0o644` / dirs with `0o755`, handle overwrite prompts, print activation hint). CLI wires `aimx agent-setup <agent>` with `--list`, `--force`, `--print`, and `--data-dir` (inherited from global args — passes through to the MCP command path baked into the plugin when the user wants a non-default data dir). The `SystemOps`/trait pattern used elsewhere (see `setup.rs`) should be applied so tests use a mock HOME.

**Priority:** P0

- [ ] `src/agent_setup.rs` created; `Cargo.toml` adds `include_dir` (verify license is MIT or Apache-2.0 before adding)
- [ ] `AgentSpec` struct captures `name`, `source_subdir`, `dest_template` (with `$HOME`/`$XDG_CONFIG_HOME` placeholders), `activation_hint` (templated string)
- [ ] CLI subcommand `aimx agent-setup <agent>` with flags `--list`, `--force`, `--print`, plus the inherited global `--data-dir`
- [ ] `--list` prints agent name + destination + activation hint for every registered agent
- [ ] Install writes files with mode `0o644`, directories `0o755`; refuses to overwrite existing files unless `--force`; prompts interactively if stdin is a TTY and `--force` not set
- [ ] Unknown agent name returns non-zero exit with a clear "unknown agent; run `aimx agent-setup --list`" message
- [ ] `--print` writes the plugin tree to stdout in a diffable format (e.g., `=== path ===\n<contents>\n`); no disk writes
- [ ] Unit tests cover: Claude Code install to temp HOME lays out expected files; `--force` overwrites; `--print` writes no files; unknown agent errors; `--list` output is stable
- [ ] Never requires root; refuses root with a clear message ("agent-setup is a per-user operation — run without sudo")
- [ ] `cargo test`, `cargo clippy -- -D warnings`, `cargo fmt -- --check` clean

#### S28-4: Register Claude Code + simplify `aimx setup` MCP output

**Context:** With framework + plugin in place, register `claude-code` in the agent registry: source `agents/claude-code/`, destination `~/.claude/plugins/aimx/` (verify the canonical location against current Claude Code docs at implementation time — may be `~/.claude/plugins/` at the parent dir instead), activation `Restart Claude Code — plugin auto-discovered.` (or `claude plugin install ~/.claude/plugins/aimx` if Claude Code requires an explicit install step for file-installed plugins). Then rework `display_mcp_section()` in `src/setup.rs` to replace the generic JSON snippet (currently lines ~852–881) with: a short intro, the list of supported agents from `agent_setup::registry()`, and the recommended command `aimx agent-setup <agent>`. The setup wizard output stays short; details live in `book/agent-integration.md` (S28-5).

**Priority:** P0

- [ ] `claude-code` registered in the `agent_setup.rs` registry with verified destination + activation hint
- [ ] `display_mcp_section()` in `src/setup.rs` no longer emits a generic `{"mcpServers": ...}` JSON snippet
- [ ] `display_mcp_section()` lists supported agents and recommends `aimx agent-setup <agent>` (the list is pulled from the registry, not duplicated by hand)
- [ ] Existing `mcp_config_snippet(data_dir)` helper in `src/setup.rs` is removed (or marked internal and kept only for tests if something else depends on it — audit call sites first)
- [ ] Tests for `setup.rs` MCP-section output updated to assert the new text
- [ ] Manual validation: `sudo aimx setup <domain>` output shows the new MCP section; running the printed `aimx agent-setup claude-code` lays the plugin down

#### S28-5: PRD update + `book/agent-integration.md`

**Context:** The PRD gains a new §6.10 (Agent Integrations), a P0 user story, and scope edits — these were pre-staged with this sprint's planning and must be finalized as part of the sprint (the PRD edits are committed alongside code in this sprint). The book needs a new chapter `agent-integration.md` explaining the installer, listing supported agents with install commands (Sprint 28 only ships Claude Code; future sprints append to this page), and linking to each agent's `agents/<agent>/README.md`. `book/mcp.md` stays focused on the MCP server surface; `agent-integration.md` is the integration-onboarding chapter.

**Priority:** P0

- [ ] `docs/prd.md` §5 adds the "aimx agent-setup" P0 user story (already in place from planning; re-verify in this sprint)
- [ ] `docs/prd.md` §6 gains §6.10 Agent Integrations with FR-49, FR-50, FR-51, FR-52 (already in place from planning; re-verify)
- [ ] `docs/prd.md` §6.1 FR-10 narrowed to point at `aimx agent-setup` (already in place from planning; re-verify)
- [ ] `docs/prd.md` §9 In Scope / Out of Scope updated (already in place from planning; re-verify)
- [ ] `book/agent-integration.md` created with: what `aimx agent-setup` does, supported agents table (Claude Code only in this sprint), per-agent activation steps, troubleshooting
- [ ] `book/SUMMARY.md` (or equivalent mdbook index) links `agent-integration.md`
- [ ] `book/mcp.md` adds a one-line pointer "To install AIMX into your agent, see [Agent Integration](agent-integration.md)" near the top

---

## Sprint 29 — Codex CLI + OpenCode Integration (Days 82–84.5) [NOT STARTED]

**Goal:** Add Codex CLI and OpenCode to the `aimx agent-setup` registry with full plugin/skill packages.

**Dependencies:** Sprint 28 (framework + Claude Code reference).

**Design note:** Before authoring each agent's package, verify the current plugin/skill format and canonical destination path against that agent's official docs. The Sprint 28 research memo is a starting point, not a source of truth — agent formats drift.

#### S29-1: `agents/codex/` plugin + registry entry

**Context:** Codex CLI uses TOML config at `~/.codex/config.toml` for MCP servers and has a plugin system with `.codex-plugin/plugin.json` manifests (mirrors Claude Code's structure per research memo; confirm at implementation time). Plugins bundle skills under `skills/<name>/SKILL.md`. The Codex plugin re-wraps the common primer. Destination on disk: `~/.codex/plugins/aimx/` (verify). Activation hint: the exact `codex plugin install ...` command if Codex requires explicit installation, or a "restart Codex" message otherwise.

**Priority:** P0

- [ ] `agents/codex/.codex-plugin/plugin.json` + `agents/codex/skills/aimx/SKILL.md` + `agents/codex/README.md` authored, re-using the common primer
- [ ] `codex` registered in `agent_setup.rs` registry with verified destination + activation hint
- [ ] Unit tests: `aimx agent-setup codex` against temp HOME lays out the expected tree; `--print` emits it to stdout
- [ ] Plugin format and destination path verified against current Codex CLI docs (link in the README)
- [ ] Manual validation on a machine with Codex CLI installed: plugin is picked up; MCP tools appear

#### S29-2: `agents/opencode/` skill + registry entry

**Context:** OpenCode (anomalyco) uses a skills system compatible with Claude Code's `SKILL.md` format, discovered from `.opencode/skills/` (project) or `~/.config/opencode/skills/` (user). Its MCP config is separate — in `opencode.json` / `opencode.jsonc` under the root key `mcp.<name>` with `command` as a single array combining binary + args. Two ways to handle MCP wiring: (a) write an `mcp.json` snippet file alongside the skill that the user pastes into `opencode.json`, or (b) just write the skill and have the activation hint print the exact JSONC block to paste. Prefer (b) — simpler, no extra file, matches the "print the activation command" pattern. Decide and document in `agents/opencode/README.md`.

**Priority:** P0

- [ ] `agents/opencode/skills/aimx/SKILL.md` authored, re-using the common primer
- [ ] `agents/opencode/README.md` documents the MCP wiring step (printed JSONC snippet) and the skill install destination
- [ ] `opencode` registered in `agent_setup.rs` registry; activation hint prints the JSONC snippet the user appends to `opencode.json`
- [ ] Unit tests: install + `--print` behavior + activation-hint text stability
- [ ] Canonical OpenCode skill destination verified against current OpenCode docs (link in README)

#### S29-3: Update `book/agent-integration.md` + `--list` output

**Context:** Extend the book chapter and the `aimx agent-setup --list` output to cover Codex and OpenCode. `--list` already reads from the registry so this comes for free once the two entries are registered; the book update is manual. Also update the README at repo root to mention all three supported agents (Claude Code, Codex, OpenCode) after this sprint.

**Priority:** P1

- [ ] `book/agent-integration.md` gains Codex and OpenCode sections (install command, activation step, troubleshooting quirks)
- [ ] `aimx agent-setup --list` output snapshot updated; tests pass
- [ ] Repo `README.md` lists all three agents in the agent-support section
- [ ] Links between `book/agent-integration.md` and each agent's `agents/<agent>/README.md` resolve

---

## Sprint 30 — Goose + OpenClaw Integration (Days 85–87.5) [NOT STARTED]

**Goal:** Add Goose (recipe-based) and OpenClaw (skill-based, JSON5 config) to `aimx agent-setup`, completing the v1 agent-integration roster.

**Dependencies:** Sprint 29.

**Design note:** Goose's integration shape differs from the others — Goose uses YAML "recipes" with `title` + `prompt` + `extensions` rather than plugins+skills. The recipe bundles both the MCP extension config AND the agent-facing instructions (the primer) in one file. OpenClaw uses skill directories similar to Claude Code but with a separate MCP config (JSON5 at `~/.openclaw/openclaw.json` under `mcp.servers`). Verify formats against current docs at implementation time.

#### S30-1: `agents/goose/aimx-recipe.yaml` + registry entry

**Context:** Goose recipes are YAML files with required `title` + `prompt` and optional `extensions` (list of MCP servers), `parameters`, etc. For AIMX, the recipe's `prompt` re-wraps the common primer, and `extensions` includes a stdio entry for `aimx mcp` so the recipe self-installs the MCP server when run. Destination: the user's local Goose recipes directory — when `GOOSE_RECIPE_GITHUB_REPO` is set, print guidance to commit the file there; otherwise write to `~/.config/goose/recipes/aimx.yaml` (verify canonical path). Activation hint prints `goose run --recipe aimx` (the form Goose uses to execute a recipe by name).

**Priority:** P0

- [ ] `agents/goose/aimx-recipe.yaml` authored: `title: "AIMX Email"`, `prompt: |` = common primer content, `extensions:` = stdio entry for `aimx mcp`
- [ ] `goose` registered in `agent_setup.rs`; destination respects `GOOSE_RECIPE_GITHUB_REPO` env var (documented in activation hint); falls back to `~/.config/goose/recipes/aimx.yaml` when env var unset
- [ ] Activation hint prints the correct invocation verb (`goose run --recipe aimx` or equivalent)
- [ ] Unit tests cover: default path install, `GOOSE_RECIPE_GITHUB_REPO` set path, `--print` output
- [ ] Recipe format verified against current Goose docs (link in `agents/goose/README.md`)

#### S30-2: `agents/openclaw/` skill + registry entry

**Context:** OpenClaw skills live in `~/.openclaw/skills/<name>/` with a `SKILL.md` carrying YAML frontmatter (`name`, `description`, optional `metadata` with `requires`, `emoji`, `os`, `install`). MCP wiring is separate — added to `~/.openclaw/openclaw.json` under `mcp.servers.aimx`, or via `openclaw mcp set aimx '{...}'`. Prefer the CLI: activation hint prints the `openclaw mcp set aimx '{"command":"aimx","args":["mcp"]}'` command so the user wires MCP with one pasted command (no config-file editing, no JSON5 parsing on our end).

**Priority:** P0

- [ ] `agents/openclaw/skills/aimx/SKILL.md` authored with valid OpenClaw frontmatter (`name: aimx`, `description`), body = common primer
- [ ] `agents/openclaw/README.md` documents the two-step activation: copy skill via `aimx agent-setup openclaw`, then run the printed `openclaw mcp set` command
- [ ] `openclaw` registered in `agent_setup.rs`; activation hint prints the exact `openclaw mcp set` command
- [ ] Unit tests: install layout + activation-hint stability
- [ ] OpenClaw skill and MCP command syntax verified against current OpenClaw docs (link in README)

#### S30-3: Final docs pass + README overhaul

**Context:** With all five v1 agents shipped, tidy the user-facing docs. `book/agent-integration.md` gets Goose and OpenClaw sections. The top-level `README.md` agent-integration section lists all five with one-line install commands and retires any lingering "copy this JSON snippet" prose. Spot-check `book/mcp.md` and `book/getting-started.md` for stale generic-snippet references.

**Priority:** P1

- [ ] `book/agent-integration.md` has sections for all five agents: Claude Code, Codex CLI, OpenCode, Goose, OpenClaw
- [ ] Top-level `README.md` shows a five-row table of supported agents + install commands in the integration section
- [ ] `grep -r "mcpServers" book/ docs/` returns only references inside `book/agent-integration.md` or the PRD (not stale "paste this snippet" prose elsewhere)
- [ ] `aimx agent-setup --list` output (tested via snapshot) shows all five agents in a stable, sorted order

---

## Sprint 31 — Channel-Trigger Cookbook (Days 88–90.5) [NOT STARTED]

**Goal:** Document email→agent channel-trigger recipes side-by-side for every supported agent. No new CLI surface — this is a docs + integration-test sprint leveraging the existing `cmd` trigger plumbing (`src/channel.rs`).

**Dependencies:** Sprint 30.

#### S31-1: `book/channel-recipes.md` — side-by-side agent invocation examples

**Context:** Channel rules in AIMX already fire shell commands with template variables (`{filepath}`, `{from}`, `{subject}`, `{mailbox}`, `{id}`, etc.) — see `src/channel.rs` and FR-30/31. The missing piece is canonical, agent-specific documentation: which agent CLI flag maps to "take this email and act on it," what approval mode to use so the trigger runs non-interactively, where the agent's output goes (stderr/stdout/log file), and how to pass `{filepath}` safely. One chapter covers all five MCP-supported agents plus Aider (the no-MCP case). Each subsection includes a complete `config.toml` snippet the user can copy.

**Priority:** P0

- [ ] `book/channel-recipes.md` authored with subsections for Claude Code (`claude -p`), Codex CLI (`codex exec`), OpenCode (`opencode run`), Goose (`goose run -t`), OpenClaw (`openclaw run` or shell equivalent — verify; OpenClaw may not have a non-interactive mode suitable for triggers, in which case document the limitation), Aider (`aider --message`)
- [ ] Each subsection contains: (1) a working `[[mailbox.catchall.channel]]` TOML snippet, (2) an explanation of the agent-specific flags (approval mode, output format, non-interactive), (3) notes on exit-code handling and where logs go
- [ ] Chapter opens with a "what counts as a channel-trigger recipe" overview and a cross-reference to `book/channels.md` (the trigger mechanics)
- [ ] Chapter closes with a summary table: agent · MCP-supported? · channel-trigger CLI · approval-mode flag · non-interactive flag
- [ ] Flag references verified against each agent's current docs (CLI help output or official docs URL linked per agent)

#### S31-2: Integration test for a representative channel recipe

**Context:** Today `src/channel.rs` has unit tests for filter matching and template expansion, but no end-to-end test covering "email ingested → channel rule matches → shell command runs with templated args." Adding one test protects the channel pipeline from regressions that would silently break all recipe users. Use Claude Code's `claude --help` (or `/bin/echo` as an agent-agnostic baseline) as the command so the test stays fast and doesn't require a real agent. Assert that the command ran, received the expected `{filepath}` expansion, and did not block ingest delivery on failure.

**Priority:** P1

- [ ] New integration test under `tests/` (or `src/channel.rs` tests) drives the full ingest→trigger path end-to-end using a fixture `.eml` and an assert-able command (e.g., a shell one-liner that writes `{filepath}` to a temp marker)
- [ ] Test asserts: the marker file was created, its contents contain the expected `filepath` and `subject` tokens, and ingest delivery completed even when the command exits non-zero
- [ ] Runs in CI against Ubuntu, Alpine, Fedora (shares the existing CI matrix)

#### S31-3: Cross-link and README update

**Context:** The cookbook is worthless if users don't find it. Link it from (a) `book/channels.md` ("for agent-specific recipes, see Channel Recipes"), (b) `book/agent-integration.md` ("once your agent is installed, see Channel Recipes for email-triggered workflows"), (c) `README.md` top-level, and (d) each agent's `agents/<agent>/README.md`. Also add an entry to the AIMX-side summary table (MCP support vs channel-trigger support) at the top of the cookbook.

**Priority:** P1

- [ ] `book/channels.md`, `book/agent-integration.md`, top-level `README.md`, and each `agents/<agent>/README.md` link `book/channel-recipes.md`
- [ ] `book/SUMMARY.md` (mdbook index) lists `channel-recipes.md`
- [ ] All cross-links resolve in a local `mdbook build`
- [ ] Top of `channel-recipes.md` has a summary table of all five v1 agents + Aider: MCP support · Channel-trigger pattern · Notes

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
| 27 | 76–78.5 | Systemd Unit Hardening | Restart rate-limit, resource limits, network-online deps in generated systemd unit | Not Started |
| 28 | 79–81.5 | Agent Integration Framework + Claude Code | `agents/` tree, `aimx agent-setup` command, Claude Code plugin, PRD §6.10 | Not Started |
| 29 | 82–84.5 | Codex CLI + OpenCode Integration | Codex plugin, OpenCode skill, book/ updates | Not Started |
| 30 | 85–87.5 | Goose + OpenClaw Integration | Goose recipe, OpenClaw skill, README overhaul | Not Started |
| 31 | 88–90.5 | Channel-Trigger Cookbook | `book/channel-recipes.md`, channel-trigger integration test, cross-links | Not Started |

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
- [ ] **(Sprint 12)** `run_smtp_listener` spawns per-accept with no concurrency bound — deferred from Sprint 12 with an inline comment at `services/verifier/src/main.rs` pointing at Sprint 14. Per-connection bounds are already tight (30s wall, 10s per-line, 1 KiB per-line), so this is defense-in-depth DoS hardening. Add a bounded semaphore or `tower::limit::ConcurrencyLimit`-style gate around accept loop
- [ ] **(Sprint 12)** Cosmetic: in `smtp_session`, fold `let mut writer = writer;` into the destructuring pattern as `let (reader, mut writer) = tokio::io::split(stream);` — zero behavioral change, post-merge cleanup suggestion from reviewer
- [x] **(Sprint 18)** `setup_with_domain_arg_skips_prompt` test passes `None` as `data_dir` and has a tautological assertion — _Fixed: test now uses `TempDir` and asserts meaningful port 25 failure_
- [x] **(Sprint 18)** `is_already_configured` uses `c.contains(domain)` substring match for smtpd.conf domain detection — _Obsolete: smtpd.conf detection removed in Sprint 22; `is_already_configured` now checks aimx service status_
- [ ] **(Sprint 19)** `deliver_message()` clones DATA payload per recipient (`data.clone()`) — for messages near 25MB with many recipients this could spike memory. Use `Arc<Vec<u8>>` to share the buffer. Low priority: typical case is 1-2 recipients
- [ ] **(Sprint 20)** `LettreTransport` `last_error` only retains the final MX failure — when all MX servers fail, only the last server's error is reported. Consider collecting all errors for better debugging
- [x] **(Sprint 20)** `extract_domain` handles `"Display Name <user@domain>"` format divergence with `lettre::Address::parse` — _Obsolete: `send.rs` now manually strips `<>` before parsing, mitigating the divergence; all call sites pass bare addresses_
- [ ] **(Sprint 21)** Inconsistent TLS file check in `can_read_tls` in `serve.rs` — cert uses `metadata().is_file()`, key uses `File::open()`. Use the same approach for both for consistency
- [ ] **(Sprint 22)** `restart_service()` and `is_service_running()` hardcode `systemctl` — on OpenRC systems, `install_service_file` writes the init script correctly but service management still calls systemctl. Pre-existing issue, not a regression
- [ ] **(Sprint 22)** `_domain` parameter in `is_already_configured` is now unused since smtpd.conf domain matching was removed — consider removing the parameter in a future cleanup
- [x] **(Sprint 24)** `CLAUDE.md` line 68 still says `setup.rs also contains run_preflight for aimx preflight` but `run_preflight` no longer exists — _Fixed: updated to reference `run_setup` and `display_deliverability_section`_
- [x] **(Sprint 24)** `docs/manual-setup.md` line 14: "provides two functions, all exposed" — _Fixed: "all" → "both"_
- [x] **(Sprint 24)** `docs/prd.md` NFR-5: "aimx ingest" in prose without backticks — _Fixed: added backticks_
- [ ] **(Sprint 26)** `get_server_ip()` and `get_server_ipv6()` each invoke `hostname -I` separately — could share a single call, but would require breaking the `NetworkOps` trait interface or adding caching. Not a correctness issue
