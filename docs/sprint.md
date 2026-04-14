# AIMX — Sprint Plan

**Sprint cadence:** 2.5 days per sprint
**Team:** Solo developer with heavy AI augmentation (Claude Code)
**Total sprints:** 27 (6 original + 2 post-audit hardening + 1 YAML→TOML migration + 2 verifier/setup overhaul + 2 post-Sprint-11 bug fixes + 2 verifier ops + 1 deployment + 1 service rename + 1 setup UX + 5 embedded SMTP + 1 verify cleanup + 1 DKIM permissions fix + 1 IPv6 support + 1 systemd unit hardening)
**Timeline:** ~78.5 calendar days
**v1 Scope:** Full PRD scope including verifier service. Sprint 1 targets earliest possible idea validation on a real VPS. Sprints 7–8 address findings from post-v1 code review audit. Sprints 10–11 overhaul the verifier service (remove email echo, add EHLO probe) and rewrite the setup flow (root check, MTA conflict detection, install-before-check). Sprints 12–13 fix critical bugs found during post-Sprint-11 debugging: Caddy self-probe loop / XFF SSRF risk in the verifier service, and the preflight chicken-and-egg problem on fresh VPSes. Sprints 14–15 are review-driven operational quality work on the verifier service (request logging, Docker packaging). Sprint 17 renames the verify service to verifier across all code, Docker, CI, and documentation. Sprints 19–23 replace OpenSMTPD with an embedded SMTP server (hand-rolled tokio listener for inbound, lettre + hickory-resolver for outbound) and update all documentation, making aimx a true single-binary solution with no external runtime dependencies and cross-platform Unix support. Sprint 24 cleans up `aimx verify` (EHLO-only checks, sudo requirement, remove `/reach` endpoint, AIMX capitalization). Sprint 27 hardens the generated systemd unit with restart rate-limiting, resource limits, and network-readiness dependencies.

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
