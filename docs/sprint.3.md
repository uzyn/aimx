# AIMX — Sprint Archive 3

> **Sprints 22–30** | Archived from [`sprint.md`](sprint.md) | Archive 3 of 4

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

*Archive 3 of 4. See [`sprint.md`](sprint.md) for the active plan and full Summary Table.*
