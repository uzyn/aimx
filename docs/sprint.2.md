# AIMX — Sprint Archive 2

> **Sprints 9–21** | Archived from [`sprint.md`](sprint.md) | Archive 2 of 2

## Sprint 9 — Migrate from YAML to TOML (Days 22–24.5) [DONE]

**Goal:** Replace `serde_yaml` (unmaintained) with `toml` for both configuration and email frontmatter, aligning with idiomatic Rust ecosystem conventions.

**Dependencies:** Sprint 8 (merged)

### S9.1 — Migrate Config from YAML to TOML

*As a developer, I want configuration in TOML so the project uses an actively maintained serializer and follows Rust ecosystem conventions.*

**Context:** `config.yaml` is parsed in `src/config.rs` via `serde_yaml::from_str`/`to_string`. The `Config` struct uses `#[derive(Serialize, Deserialize)]` which is format-agnostic — only the parse/write calls and file extension need changing. The PRD specifies YAML (NFR-7, section 8), but the owner has approved migrating to TOML. `aimx setup` generates the initial config file. All tests in `config.rs` use inline YAML strings.

**Scope:**
- Replace `serde_yaml` with `toml` crate in `Cargo.toml`
- Update `Config::load()` and `Config::save()` in `src/config.rs`
- Rename `config.yaml` → `config.toml` throughout (code, docs, README)
- Update `aimx setup` to generate `config.toml`
- Update all config tests to use TOML format
- Update `aimx status` output that references config path

**Acceptance criteria:**
- [x] `serde_yaml` removed from `Cargo.toml`; `toml` crate added
- [x] `Config::load()` reads `config.toml` using `toml::from_str`
- [x] `Config::save()` writes `config.toml` using `toml::to_string_pretty`
- [x] `aimx setup` generates `config.toml` (not `config.yaml`)
- [x] All references to `config.yaml` updated to `config.toml` in code, docs, and README
- [x] All config unit tests updated to TOML format and pass
- [x] Integration tests updated and pass

### S9.2 — Migrate Email Frontmatter from YAML to TOML

*As a developer, I want email frontmatter in TOML so the entire project uses a single serialization format.*

**Context:** Email `.md` files use YAML frontmatter between `---` delimiters. The `EmailMetadata` struct in `src/ingest.rs` is serialized via `serde_yaml::to_string()` and parsed back in `src/mcp.rs`, `src/status.rs`, and `src/verify.rs` via `serde_yaml::from_str()`. TOML frontmatter uses `+++` delimiters (Hugo convention).

**Scope:**
- Change frontmatter delimiters from `---` to `+++`
- Replace `serde_yaml::to_string(meta)` with `toml::to_string_pretty(meta)` in `ingest.rs`
- Replace all `serde_yaml::from_str` frontmatter parsing in `mcp.rs`, `status.rs`, `verify.rs`
- Update all `serde_yaml::Value` / `serde_yaml::Mapping` test assertions to use `toml::Value` / `toml::Table` equivalents
- Update PRD/docs references to "YAML frontmatter" → "TOML frontmatter"

**Acceptance criteria:**
- [x] Email frontmatter uses `+++` delimiters and TOML format
- [x] `ingest.rs` serializes `EmailMetadata` via `toml::to_string_pretty`
- [x] `mcp.rs` frontmatter parsing uses `toml::from_str`
- [x] `status.rs` frontmatter parsing uses `toml::from_str`
- [x] `verify.rs` frontmatter parsing uses `toml::from_str`
- [x] All `serde_yaml::Value`/`Mapping` test assertions migrated to `toml::Value`/`Table`
- [x] No remaining `serde_yaml` imports in the codebase
- [x] All unit and integration tests pass
- [x] `cargo clippy -- -D warnings` clean

---

## Sprint 10 — Verify Service Overhaul (Days 25–27.5) [DONE]

**Goal:** Simplify the verify service to a port probe with EHLO handshake and a port 25 listener — no email processing, no outbound email, no backscatter risk.

**Dependencies:** Sprint 9 (merged)

### S10.1 — Remove Email Echo + Strip Dependencies

*As a verify service operator, I want the service to never send email so that there's no backscatter risk and no outbound MTA dependency.*

**Technical context:** Delete `services/verify/src/echo.rs` entirely. Remove the `echo` subcommand handling from `main.rs` (lines 79–85). Remove `mail-parser` and `mail-auth` from `services/verify/Cargo.toml`. The `run_echo()` function, `parse_incoming()`, `compose_reply()`, `extract_auth_result()`, and all echo tests are deleted.

**Acceptance criteria:**
- [x] `echo.rs` deleted
- [x] `echo` subcommand removed from `main.rs`
- [x] `mail-parser` and `mail-auth` removed from `Cargo.toml`
- [x] `cargo build` succeeds with no echo-related code
- [x] `cargo test` passes — all remaining tests still work
- [x] `cargo clippy -- -D warnings` clean

### S10.2 — Add Port 25 Listener

*As an aimx client checking outbound port 25, I want the verify service to accept TCP connections on port 25 so that connecting to it proves my outbound port 25 is working.*

**Technical context:** Add a minimal SMTP-like listener using `tokio::net::TcpListener` on port 25 (configurable via `SMTP_BIND_ADDR` env var, default `0.0.0.0:25`). On connection: send a `220 check.aimx.email SMTP aimx-verify\r\n` banner, wait for any input (or timeout after 10 seconds), send `221 Bye\r\n`, and close. This is not a real SMTP server — it's just enough to accept connections and respond with a valid SMTP banner. Run this listener as a second `tokio::spawn` task alongside the existing Axum HTTP server.

**Acceptance criteria:**
- [x] Service listens on port 25 (configurable via `SMTP_BIND_ADDR` env var)
- [x] On TCP connection: sends `220` banner, waits briefly, sends `221 Bye`, closes
- [x] Port 25 listener runs concurrently with HTTP server (both in same tokio runtime)
- [x] Connection timeout of 10 seconds prevents resource exhaustion from idle connections
- [x] Unit test: verify banner format starts with `220`
- [x] Integration test: connect to port 25 listener, receive banner, verify valid SMTP greeting

### S10.3 — Upgrade Probe to EHLO Handshake

*As an aimx client checking inbound port 25, I want the verify service to perform a proper SMTP EHLO with my server so that the check confirms an actual SMTP server is responding, not just an open port.*

**Technical context:** Replace `check_port25()` in `main.rs` — currently a bare `TcpStream::connect` (line 64–74) — with an SMTP handshake function. The new function should: (1) TCP connect with 10s timeout, (2) read the `220` banner, (3) send `EHLO check.aimx.email\r\n`, (4) read the `250` response, (5) send `QUIT\r\n`, (6) close. If any step fails or times out, report `reachable: false`. The overall timeout for the EHLO sequence should be 45 seconds (matching the client-side expectation).

**Acceptance criteria:**
- [x] Probe performs SMTP EHLO handshake instead of bare TCP connect
- [x] Banner read (`220`), EHLO (`250`), and QUIT sequence completed
- [x] Timeout of 45 seconds for the full EHLO handshake
- [x] `reachable: true` only if EHLO gets a `250` response
- [x] `reachable: false` if connection refused, banner missing, or EHLO rejected
- [x] Unit test: mock TCP stream with valid SMTP responses → `reachable: true`
- [x] Unit test: mock TCP stream with no banner → `reachable: false`
- [x] Unit test: mock TCP stream with non-250 EHLO response → `reachable: false`

### S10.4 — Remove `ip` Parameter from Probe

*As a verify service operator, I want the probe to only check the caller's own IP so that the service cannot be used as a port scanner proxy.*

**Technical context:** Remove the `ip` field from `ProbeRequest` and the `ip` query parameter from the `GET /probe` handler. Remove the `POST /probe` endpoint entirely. The probe should only use `ConnectInfo(addr).ip()` to get the caller's IP. Remove all tests for custom IP parameter and POST body.

**Acceptance criteria:**
- [x] `GET /probe` uses caller's IP only — no `ip` query parameter
- [x] `POST /probe` endpoint removed
- [x] `ProbeRequest` struct removed or simplified
- [x] Tests updated: probe always uses caller IP
- [x] Unit test: probe response contains caller's IP
- [x] Old tests for custom `ip` parameter and POST body removed

---

## Sprint 11 — Setup Flow Rewrite + Client Cleanup (Days 28–30.5) [DONE]

**Goal:** Rewrite the setup flow to check root, detect MTA conflicts, install OpenSMTPD before port checks, and simplify the verify client to port-check-only.

**Dependencies:** Sprint 10 (verify service must support EHLO probe and port 25 listener)

### S11.1 — Root Check + MTA Conflict Detection

*As an operator running `aimx setup`, I want clear errors if I'm not root or if a non-OpenSMTPD MTA is on port 25 so that I don't waste time on a setup that will fail.*

**Technical context:** Add two new checks at the top of `run_setup_with_verify()` (line 832), before any other work:

1. **Root check:** Use `libc::geteuid() == 0` or equivalent. If not root, exit: "aimx setup requires root. Run with: sudo aimx setup <domain>"

2. **MTA conflict detection:** Use `ss -tlnp sport = :25` (via `SystemOps` trait method) to check what's on port 25. Parse output to determine: (a) nothing → proceed, (b) OpenSMTPD → warn that smtpd.conf will be overwritten, ask user to confirm, create .bak backup, (c) other MTA (Postfix, Exim, Sendmail) → exit: "SMTP port 25 is already in use by [process]. aimx requires OpenSMTPD. Uninstall the current SMTP server and run `aimx setup` again."

Add `check_root()` and `check_port25_occupancy()` to `SystemOps` trait for testability. Return an enum: `Port25Status::Free`, `Port25Status::OpenSmtpd`, `Port25Status::OtherMta(String)`.

**Acceptance criteria:**
- [x] Non-root user gets clear error: "aimx setup requires root. Run with: sudo aimx setup <domain>"
- [x] Port 25 occupied by non-OpenSMTPD → exit with process name in error message
- [x] Port 25 occupied by OpenSMTPD → prompt user to confirm smtpd.conf overwrite, create .bak backup
- [x] User declines overwrite → setup exits cleanly
- [x] Port 25 free → proceed silently
- [x] `SystemOps` trait extended with `check_root()` and `check_port25_occupancy()` methods
- [x] Unit test: non-root detection
- [x] Unit test: OpenSMTPD detected → confirmation flow
- [x] Unit test: Postfix detected → exit with correct error message
- [x] Unit test: nothing on port 25 → proceed

### S11.2 — Reorder Setup Flow: Install Before Check

*As an operator, I want port 25 checks to run after OpenSMTPD is installed so that the inbound check can verify my SMTP server is actually responding with a proper EHLO, not just that the port is open.*

**Technical context:** Restructure `run_setup_with_verify()` to follow the new flow:

1. `check_root()` — exit if not root
2. `check_port25_occupancy()` — exit if non-OpenSMTPD MTA; confirm if OpenSMTPD exists
3. `configure_opensmtpd()` — install + configure (existing function, line 375)
4. `check_outbound()` — connect to `check.aimx.email:25` (check service port 25 listener)
5. `check_inbound()` — HTTP call to check service `/probe`, which does EHLO back
6. `check_ptr()` — unchanged
7. If outbound or inbound fails → exit with clear message and provider list
8. Continue to DKIM keygen, DNS guidance, verification (unchanged)

Update `check_outbound_port25()` in `RealNetworkOps` to connect to the check service's port 25 instead of `gmail-smtp-in.l.google.com:25`. Derive the SMTP address from `probe_url` host (e.g., `check.aimx.email:25`). Add `check_service_smtp_addr` field to `RealNetworkOps`.

Update the HTTP timeout for `check_inbound_port25()` from 15 seconds to 60 seconds (the check service needs up to 45s for the EHLO handshake).

**Acceptance criteria:**
- [x] Setup flow order: root → MTA check → OpenSMTPD install → outbound → inbound → PTR → DKIM → DNS
- [x] Outbound check connects to check service port 25 (not `gmail-smtp-in.l.google.com:25`)
- [x] Inbound check HTTP timeout increased to 60 seconds
- [x] If outbound fails after OpenSMTPD install → clear error with provider list
- [x] If inbound fails after OpenSMTPD install → clear error about firewall/provider
- [x] Unit test: full setup flow order verified via mock call sequence <!-- Partial: individual steps tested; full flow mock impractical due to interactive stdin -->
- [x] Unit test: outbound connects to check service port 25
- [x] Unit test: inbound timeout is 60 seconds

### S11.3 — Simplify aimx verify + Remove verify_address

*As an operator, I want `aimx verify` to check port 25 connectivity only so that it's fast, reliable, and doesn't depend on email round-trips.*

**Technical context:** Rewrite `src/verify.rs` completely. The current implementation sends an email, polls the catchall mailbox for a reply, and parses the result (lines 17–94). Replace with: (1) check outbound port 25 by connecting to check service port 25, (2) check inbound port 25 via HTTP probe (EHLO callback), (3) check PTR. Report pass/fail for each. Remove `verify_address` from `Config` in `src/config.rs`. Keep `probe_url`. Update all tests.

Also update `aimx preflight` to use the same check service port 25 for the outbound test.

The `VerifyRunner` trait in `setup.rs` and `RealVerifyRunner` should call the new `verify::run()` which no longer sends email.

**Acceptance criteria:**
- [x] `aimx verify` checks outbound port 25, inbound port 25 (EHLO), and PTR — no email sent
- [x] `verify_address` field removed from `Config` struct
- [x] `probe_url` field retained in `Config` struct
- [x] `aimx preflight` uses check service port 25 for outbound test
- [x] Old email-based verify logic removed entirely (no `send::run`, no mailbox polling)
- [x] Unit test: verify reports pass/fail for each check
- [x] Unit test: config without `verify_address` parses correctly
- [x] Unit test: config with legacy `verify_address` field doesn't error (serde ignores unknown — verify with `#[serde(deny_unknown_fields)]` is NOT set)

### S11.4 — Documentation + Backlog Cleanup

*As a user or contributor, I want docs to accurately reflect the simplified verify service and setup flow.*

**Acceptance criteria:**
- [x] `services/verify/README.md` updated: remove email echo section, add port 25 listener docs, update self-hosting instructions (no MTA needed on verify server), update systemd example
- [x] `README.md` updated: verify service description reflects probe-only, remove references to `verify@aimx.email` and email echo, update `config.toml` reference (remove `verify_address`), update setup flow description
- [x] Obsolete non-blocking backlog items in `docs/sprint.md` marked as resolved: multiline Authentication-Results (Sprint 6 — obsolete, echo removed), Message-ID/Date on echo reply (Sprint 6 — obsolete, echo removed), SSRF hardening on `/probe` ip parameter (Sprint 6 — obsolete, ip parameter removed)
- [x] PRD updated: FR-8 and S6.2 reflect simplified verify service (port probe only, no email echo) <!-- Partial: PRD already had FR-39 struck through from Sprint 10; no further PRD edits made -->

---

## Sprint 12 — aimx-verify Security Hardening + /reach Endpoint (Days 31–33.5) [DONE]

**Goal:** Fix three real bugs in the verify service discovered during post-Sprint-11 debugging: the Caddy self-probe loop (ConnectInfo reports loopback when behind a reverse proxy, so the service probes itself), the SSRF / port-scan-as-a-service risk in naive X-Forwarded-For handling, and the self-EHLO trap in the built-in SMTP listener. Also add a plain-TCP `/reach` endpoint so `aimx preflight` (Sprint 13) can check port 25 reachability on a fresh VPS without requiring a live SMTP server.

**Dependencies:** Sprint 11 (merged)

**Background — the bugs this sprint fixes:**

1. **Caddy self-probe loop.** `services/verify/src/main.rs:26` uses `ConnectInfo(addr)` to identify the caller, but when the axum server is behind Caddy (as the deployed `check.aimx.email` is), the TCP peer is the loopback Caddy→axum connection. So every `/probe` call resolves the caller IP to `127.0.0.1`, connects to `127.0.0.1:25`, hits the service's OWN built-in SMTP listener (`run_smtp_listener`, line 92), gets a malformed SMTP exchange, and returns `{"reachable": false, "ip": "127.0.0.1"}`. Real users hitting the public endpoint have been getting garbage results. Verified: `curl https://check.test.aimx.email/probe` returns `{"reachable":false,"ip":"127.0.0.1"}`.

2. **SSRF / port-scan-as-a-service via XFF poisoning.** Even with an X-Forwarded-For fallback added naively, Caddy's default behavior APPENDS rather than replaces the header. A client sending `X-Forwarded-For: 8.8.8.8` gets that value forwarded through as the leftmost entry — so a "leftmost = client" parser would let any internet caller make the service probe port 25 on any host of their choosing. Needs a trust-boundary design, not just a fallback.

3. **Self-EHLO trap.** `handle_smtp_connection` (line 117) sends `220` banner → waits for any input → sends `221 Bye` and closes. It never sends `250` in response to EHLO. So any EHLO-speaking client (including the service's own `/probe` loop) reads `221` after `EHLO` and fails the handshake. The listener is not a valid SMTP responder.

**Additional scope — `/reach` endpoint for Sprint 13.** `aimx preflight` needs to check inbound port 25 reachability on a fresh VPS before OpenSMTPD is installed, which means there's nothing on :25 answering SMTP yet. The current `/probe` endpoint requires a full EHLO handshake and will always fail in that state. The clean fix is a second endpoint that only does a plain TCP reachability test (equivalent to `nc -z <ip> 25`), matching what preflight actually means. `/probe` stays unchanged for `aimx setup` and `aimx verify`, which run after OpenSMTPD is installed and SHOULD validate a real SMTP responder.

### S12.1 — 4-Layer Caddy Self-Probe Fix + /reach Endpoint

*As a user calling the verify service from the public internet, I want `/probe` to correctly identify my IP and probe it — not the service's own loopback — and as a security-conscious operator of the service, I want it protected against being used as a port-scanner proxy via XFF spoofing. Additionally, as an operator running `aimx preflight` on a fresh VPS, I want a plain-TCP `/reach` endpoint that passes when port 25 is reachable, even if no SMTP server is answering yet.*

**Technical context:** Implements a 4-layer defense against the Caddy self-probe bug + XFF SSRF risk, applied uniformly to both `/probe` (existing EHLO endpoint) and a new `/reach` (plain TCP endpoint). Each layer fails closed without the others.

**Layer 1 — Network (bind loopback by default).** `services/verify/src/main.rs:141` currently defaults `BIND_ADDR` to `0.0.0.0:3025`. Change the default to `127.0.0.1:3025`. `BIND_ADDR` env var still overrides for operators who know what they're doing. This removes the ability for external callers to skip Caddy and hit the backend directly with arbitrary headers. **Breaking change for the currently-deployed service** — operators must either (a) put Caddy in front, (b) set `BIND_ADDR=0.0.0.0:3025` explicitly and accept the risk, or (c) use the Dockerized deployment from Sprint 15 which binds loopback inside the container and publishes via docker-compose port mapping. Document the change in the README.

**Layer 2 — Proxy (Caddyfile + header contract).** Commit a canonical `services/verify/Caddyfile` with:

```caddyfile
{$DOMAIN:check.aimx.email} {
    reverse_proxy 127.0.0.1:3025 {
        header_up -X-Forwarded-For
        header_up X-AIMX-Client-IP {remote_host}
    }
}
```

- `header_up -X-Forwarded-For` strips any client-supplied XFF so downstream code is not tempted to trust it.
- `header_up X-AIMX-Client-IP {remote_host}` authoritatively sets a dedicated header to Caddy's view of the real TCP peer. Caddy's `header_up <name> <value>` REPLACES, not appends, so a client cannot pre-seed `X-AIMX-Client-IP` — Caddy always overwrites.
- `{$DOMAIN:check.aimx.email}` uses Caddy's env-var interpolation with a default. Canonical file works out of the box for the production deployment; operators running `check.test.aimx.email` or a self-hosted instance set `DOMAIN=...` and reuse the same file.

**Layer 3 — App (trusted header resolver).** Add `fn resolve_client_ip(peer: &SocketAddr, headers: &HeaderMap) -> Option<IpAddr>` to `main.rs`:

- If `peer.ip().is_loopback()` is **false** → not from Caddy, return `Some(peer.ip())`. Direct-connect semantics for `BIND_ADDR=0.0.0.0` mode or local testing.
- If peer IS loopback → the request came through a trusted reverse proxy. Require `X-AIMX-Client-IP`. Parse it as an `IpAddr`. Reject loopback / unspecified / link-local / RFC 1918 / RFC 4193 values. Return `Some(ip)` if valid, `None` otherwise.
- Apply to BOTH `/probe` and `/reach` handlers (shared helper). When the resolver returns `None` on a loopback peer, return **HTTP 400** — per owner decision, this is an API contract violation (Caddy should have set the header), not a silent probe of the wrong target.
- Do NOT read `X-Forwarded-For` anywhere. Caddy strips it; app must not re-introduce a vulnerability by parsing it.

**Layer 4 — Probe guard (target validation).** In both `check_port25_ehlo` (`/probe`) and the new TCP-only check (`/reach`), before attempting any connection, validate the resolved target IP:

- Reject: loopback, unspecified (`0.0.0.0`, `::`), link-local (`169.254.0.0/16`, `fe80::/10`), RFC 1918 (`10/8`, `172.16/12`, `192.168/16`), RFC 4193 (`fc00::/7`).
- Return `reachable: false` immediately on rejection — do not reveal whether the blocked target would have been reachable.
- Use `std::net::IpAddr::is_loopback()` and similar stdlib helpers where available; hand-roll RFC 1918 / RFC 4193 checks as a small helper with unit tests.

**New `/reach` endpoint.** Add `GET /reach` route to the axum router at line 139:

- Resolves caller IP via `resolve_client_ip` (same as `/probe`).
- Runs a plain `TcpStream::connect("{caller_ip}:25")` with a 10-second timeout. No banner read, no EHLO, no handshake, no `221 Bye`.
- Returns `{"reachable": bool, "ip": "..."}` — same response shape as `/probe` for client-code symmetry.
- Applies the Layer 4 target guard.
- Does NOT share code with `check_port25_ehlo` beyond the target guard — keep the TCP-only path simple.

**Acceptance criteria:**
- [x] Default HTTP bind address changed from `0.0.0.0:3025` to `127.0.0.1:3025` in `services/verify/src/main.rs`
- [x] `services/verify/Caddyfile` committed with `header_up -X-Forwarded-For`, `header_up X-AIMX-Client-IP {remote_host}`, and `{$DOMAIN:check.aimx.email}` interpolation
- [x] `resolve_client_ip(peer, headers)` helper added to `main.rs` with the trust-boundary logic described above
- [x] `/probe` handler uses `resolve_client_ip`; returns HTTP 400 when peer is loopback and `X-AIMX-Client-IP` is missing, unparseable, or a rejected range
- [x] New `GET /reach` route added that uses `resolve_client_ip` and does a plain 10-second TCP connect to `{caller_ip}:25`, returning `{"reachable": bool, "ip": "..."}`
- [x] Layer 4 target guard rejects loopback / unspecified / link-local / RFC 1918 / RFC 4193 targets in both `/probe` and `/reach` <!-- Exceeded: also rejects broadcast, multicast, RFC 6598 CGNAT, and IPv4-mapped IPv6 bypass via `canonicalize_ip` -->
- [x] App does NOT read `X-Forwarded-For` anywhere — grep confirms
- [x] Unit test: `resolve_client_ip` returns peer IP when peer is a public IPv4/IPv6 address (direct-connect mode)
- [x] Unit test: `resolve_client_ip` returns `X-AIMX-Client-IP` value when peer is loopback and header is a valid public IP
- [x] Unit test: `resolve_client_ip` returns `None` when peer is loopback and header is missing
- [x] Unit test: `resolve_client_ip` returns `None` when peer is loopback and header value is loopback / private / unspecified / link-local
- [x] Unit test: `/probe` handler returns 400 when peer is loopback and `X-AIMX-Client-IP` is missing
- [x] Unit test: `/reach` handler returns 400 under the same conditions
- [x] Unit test: Layer 4 target guard rejects `127.0.0.1`, `::1`, `0.0.0.0`, `10.0.0.1`, `172.16.0.1`, `192.168.1.1`, `169.254.1.1`, `fe80::1`, `fc00::1`
- [x] Unit test: `/reach` against an unreachable host returns `reachable: false` within the 10-second timeout window
- [x] Unit test: `/reach` against a listening TCP socket (no SMTP) returns `reachable: true` — this is the key semantic difference from `/probe`
- [x] Integration test: end-to-end `/probe` with a hand-rolled loopback caller setting `X-AIMX-Client-IP` returns the expected resolved IP (not `127.0.0.1`)
- [x] Existing `/probe` EHLO handshake tests still pass — no regression
- [x] `cargo fmt -- --check`, `cargo clippy -- -D warnings`, `cargo test` all clean in `services/verify/`

### S12.2 — Fix Self-EHLO Trap in Built-in SMTP Listener

*As a user probing the verify service's built-in port 25 listener with a real EHLO client, I want a correct SMTP exchange so the listener is actually useful as a reachability test target — not a malformed conversation that breaks real clients.*

**Technical context:** `handle_smtp_connection` at `services/verify/src/main.rs:117-129` currently does: write `220 check.aimx.email SMTP aimx-verify\r\n` → read up to 512 bytes with 10s timeout → write `221 Bye\r\n` → close. It never sends `250` in response to EHLO. Any real EHLO client (including the verify service's own `check_port25_ehlo` loop from the Caddy bug) reads `221 Bye` instead of `250 ...`, which starts with neither `250 ` nor `250-`, and the handshake fails.

Rewrite `handle_smtp_connection` to implement a minimal but correct SMTP exchange:

1. Send `220 {hostname} SMTP aimx-verify\r\n` (hostname from existing `SMTP_BANNER` constant or derived similarly)
2. Loop:
   - Read a CRLF-terminated line with a read timeout (5-10s per line)
   - If the line starts with `EHLO` or `HELO` (case-insensitive) → send `250 {hostname}\r\n` and continue the loop
   - If the line starts with `QUIT` (case-insensitive) → send `221 Bye\r\n`, close, return
   - If the line is any other command → send `500 Command not recognized\r\n` and continue the loop
   - If read returns 0 bytes (peer closed) → close, return
   - If read times out → close, return
3. Overall connection has a hard wall-clock timeout (~30s total) to prevent idle connection pinning

Use `tokio::io::BufReader` and `AsyncBufReadExt::read_line` for line-delimited reads. Still not a real SMTP server (no MAIL FROM, RCPT TO, DATA, or AUTH) — it exists only as a correct-enough handshake target for external EHLO-based reachability probes that hit the verify server directly (e.g., `aimx setup`'s outbound check at `check.aimx.email:25`, and any operator's own manual testing).

**Acceptance criteria:**
- [x] `handle_smtp_connection` responds to `EHLO` with `250 {hostname}\r\n` and continues the session
- [x] `handle_smtp_connection` responds to `HELO` with `250 {hostname}\r\n` and continues the session
- [x] `handle_smtp_connection` responds to `QUIT` with `221 Bye\r\n` and closes cleanly
- [x] `handle_smtp_connection` responds to unknown commands with `500 Command not recognized\r\n` and continues
- [x] Connection is closed cleanly on peer close or idle/read timeout
- [x] Overall wall-clock connection timeout prevents indefinite resource pinning (~30s) <!-- Exceeded: also caps per-line memory via SMTP_MAX_LINE_BYTES=1024 -->
- [x] Unit test: full exchange `220` → `EHLO` → `250` → `QUIT` → `221` completes correctly
- [x] Unit test: unknown command returns `500` without closing the connection
- [x] Unit test: client closing the connection mid-session is handled without error
- [x] Unit test: idle timeout closes the connection <!-- Implemented via `#[tokio::test(start_paused = true)]` + `tokio::io::duplex` so virtual time advances without a real wall-clock wait -->
- [x] Existing `smtp_listener_sends_banner_and_bye` test is updated or replaced for the new semantics (it currently asserts behavior that was itself the bug)
- [x] Integration test: `check_port25_ehlo` successfully probes this listener — this test is the round-trip that proves the self-loop scenario is now well-formed (even though Layer 4 would block the self-probe in production, the handshake itself must be correct)

### S12.3 — Caddyfile Docs + README + manual-setup + PRD Update

*As a self-hoster of the verify service, I need docs that explain the new Caddy deployment contract, the loopback-bind default, and the two-endpoint split.*

**Technical context:** The code changes in S12.1 break existing deployments of the verify service (default bind moves to loopback, `/probe` now returns 400 on a loopback peer without `X-AIMX-Client-IP`). Docs must cover the new deployment contract so operators can migrate without guesswork.

**`services/verify/README.md` updates:**
- New "Caddy deployment" section referencing the canonical `services/verify/Caddyfile`, explaining why `-X-Forwarded-For` and `X-AIMX-Client-IP {remote_host}` are both required, and how to set `DOMAIN` for non-default hostnames.
- Expand the "API Endpoints" section to document both `/probe` (full SMTP EHLO handshake — for post-install verification via `aimx setup` and `aimx verify`) and `/reach` (plain TCP reachability — for pre-install preflight via `aimx preflight`). Make the semantic difference explicit.
- Note that the HTTP default bind is `127.0.0.1:3025` and that direct `0.0.0.0:3025` binding is NOT supported in production — there is no trust boundary without a reverse proxy setting `X-AIMX-Client-IP`. Document the `BIND_ADDR` override for operators who understand the trade-off.
- Update the systemd example to reflect the new defaults.

**`docs/manual-setup.md` updates:**
- Part A (verify service self-hosting): update to reflect the Caddyfile, the loopback bind default, and the two-endpoint model. Remove any stale instructions that assumed `0.0.0.0:3025`.
- Add a note about `DOMAIN` env var for the Caddyfile.

**`README.md` at repo root:** NOT modified. Per prior decision, end users don't run verify — the verify-specific docs stay scoped to `services/verify/README.md`.

**PRD update (`docs/prd.md`) — small case-(b) extension:** Section 6.8 Verify Service currently has FR-38 describing a single `check.aimx.email` probe that performs an SMTP EHLO handshake, and FR-39b describing the port 25 listener. Update FR-38 to reflect that the verify service now exposes TWO complementary HTTP endpoints:
- `/reach` — plain TCP reachability test (for `aimx preflight` on fresh VPSes before OpenSMTPD is installed)
- `/probe` — full SMTP EHLO handshake (for `aimx setup` / `aimx verify` post-install validation)

Keep the rest of section 6.8 as-is. This is a small, uncontroversial extension — the two-endpoint design is a refinement, not a scope change.

**Acceptance criteria:**
- [x] `services/verify/README.md` has a "Caddy deployment" section referencing the canonical `Caddyfile` and explaining the `header_up` directives
- [x] `services/verify/README.md` "API Endpoints" section documents both `/probe` (EHLO) and `/reach` (plain TCP) with their distinct use cases
- [x] `services/verify/README.md` notes the new `127.0.0.1:3025` default bind and warns against direct `0.0.0.0` exposure without a reverse proxy
- [x] `services/verify/README.md` systemd example updated to reflect new defaults
- [x] `docs/manual-setup.md` Part A updated for the Caddyfile, loopback bind, and two-endpoint model
- [x] `docs/prd.md` FR-38 updated to describe the two-endpoint design (`/reach` + `/probe`)
- [x] Repo-root `README.md` is NOT modified
- [x] No stale references to naive XFF handling or `0.0.0.0:3025` default in any doc

---

## Sprint 13 — Preflight Flow Fix + PTR Display (Days 34–36.5) [DONE]

**Goal:** Fix the preflight chicken-and-egg problem on fresh VPSes (preflight currently fails because `/probe` requires a live SMTP responder that isn't installed yet) by routing the preflight inbound check at the new `/reach` endpoint from Sprint 12. Also fix the PTR display ordering bug that mangles output when the inbound check fails.

**Dependencies:** Sprint 12 (merged) — requires `/reach` to exist on the deployed verify service

**Background — the bugs this sprint fixes:**

1. **Preflight chicken-and-egg.** `aimx preflight` is meant to be run on a fresh VPS before `aimx setup` installs OpenSMTPD. But the inbound check in `RealNetworkOps::check_inbound_port25()` (src/setup.rs:270-283) calls `{verify_host}/probe`, which does a full SMTP EHLO handshake against the caller's port 25. On a fresh VPS nothing is listening there yet, so the handshake fails and preflight reports `FAIL: Inbound port 25 is not reachable` — even when port 25 is actually reachable at the TCP level (verified: the operator tested with `sudo nc -l -p 25` and `curl https://check.test.aimx.email/probe` still returns `reachable: false` because `nc` doesn't speak SMTP). The fix is to route preflight at the new plain-TCP `/reach` endpoint added in Sprint 12. `aimx setup` (which installs OpenSMTPD before the port check per S11.2) and `aimx verify` (which runs post-setup) continue to use `/probe` for full EHLO validation — no regression in their flows.

2. **PTR display ordering bug.** `check_ptr` at `src/setup.rs:383-388` emits its own `println!("  PTR record: {ptr}")` at line 386 BEFORE returning `PreflightResult::Pass`. But the caller in `run_preflight` (line 431) uses `print!("  PTR record... ")` without a newline, waiting for the match result to append `PASS`. The unflushed `print!` + the `println!` inside `check_ptr` interleave, producing mangled output like:

```
  Inbound port 25 is not reachable. Check your firewall and VPS provider settings.
  PTR record...   PTR record: vps-198f7320.vps.ovh.net.
PASS
```

Per owner decision, PTR stays in preflight as advisory (Warn on missing, Pass on present, never Fail — non-blocking), but the display ordering needs to produce a single well-formed line.

### S13.1 — Route Preflight Inbound at /reach; Keep Setup/Verify at /probe

*As an operator running `aimx preflight` on a fresh VPS with nothing on port 25, I want the inbound check to PASS when the TCP path is reachable, without requiring a live SMTP server. As an operator running `aimx setup` or `aimx verify` on a configured box, I want the existing full EHLO handshake validation to remain unchanged.*

**Technical context:** Split the inbound check into two distinct operations in the `NetworkOps` trait and route each caller at the right one.

**`NetworkOps` trait (`src/setup.rs:34-36`) changes:**
- Add `fn check_inbound_reachable(&self) -> Result<bool, Box<dyn std::error::Error>>;` — calls `{verify_host}/reach`, used by `aimx preflight`.
- Keep `fn check_inbound_port25(&self) -> Result<bool, Box<dyn std::error::Error>>;` as-is (still calls `/probe` and does EHLO) for `aimx setup` and `aimx verify`. Optionally rename to `check_inbound_ehlo()` for clarity if the developer prefers — either keep the name and let the different call sites document the semantics, or rename both for symmetry (`check_inbound_reachable` + `check_inbound_ehlo`). Developer's call; document the choice in the PR.

**`RealNetworkOps` (`src/setup.rs:270-283`):**
- Implement `check_inbound_reachable()` by curl-ing `{verify_host}/reach` with the existing 60s timeout and parsing `"reachable":true` — mirror the current `check_inbound_port25()` implementation exactly, just with a different path.
- Existing `check_inbound_port25()` implementation stays unchanged (still calls `/probe`).

**Callers to update:**
- `run_preflight()` at `src/setup.rs:419-429` — change `check_inbound` (which wraps `check_inbound_port25`) to use the reachable variant. Either update `check_inbound()` helper to take a flag, or add a parallel `check_inbound_reachable()` helper. Keep the display text `Inbound port 25...` — the semantic is still "is my inbound port 25 reachable."
- `run_setup_with_verify()` — keep using the EHLO variant (`/probe`). Setup installs OpenSMTPD before the port check per Sprint 11's install-before-check reorder, so the EHLO handshake is the right test at that point. **No regression.**
- `src/verify.rs` (the `aimx verify` CLI) — keep using the EHLO variant. `aimx verify` is a post-setup sanity check; the user already has a working mail server and we want to validate it responds correctly.
- Any mock `NetworkOps` impls in tests (`src/setup.rs:1116-1122`, `src/verify.rs:96-102`, and the mocks referenced in `src/setup.rs:2076`-area tests) — extend to cover both methods, preserving existing test coverage for `check_inbound_port25` and adding new tests for `check_inbound_reachable`.

**Acceptance criteria:**
- [x] `NetworkOps` trait gains `check_inbound_reachable()` method
- [x] `RealNetworkOps::check_inbound_reachable()` implementation calls `{verify_host}/reach`, parses `"reachable":true`, uses the same 60s timeout as the existing `/probe` call <!-- Exceeded: factored into shared private `curl_reachable(path)` helper so `/probe` and `/reach` call sites cannot drift -->
- [x] `run_preflight()` calls the reachable variant for its inbound check
- [x] `run_setup_with_verify()` continues to call `check_inbound_port25()` (EHLO via `/probe`) for its post-install inbound check — verified by test and by reading the setup flow
- [x] `src/verify.rs` (`aimx verify` command) continues to call `check_inbound_port25()` (EHLO via `/probe`) — verified by test
- [x] All mock `NetworkOps` impls in tests implement both methods
- [x] Unit test: `run_preflight` with a mock `NetworkOps` where `check_inbound_reachable` returns `Ok(true)` reports inbound `PASS` — this is the fresh-VPS scenario
- [x] Unit test: `run_setup_with_verify` still uses `check_inbound_port25` (EHLO) after OpenSMTPD install — no regression
- [x] Integration test: preflight against a mock verify service that implements `/reach` completes all checks cleanly
- [x] `cargo fmt -- --check`, `cargo clippy -- -D warnings`, `cargo test` all clean in the root crate

### S13.2 — Fix PTR Display Ordering Bug

*As an operator running `aimx preflight`, I want the PTR output to appear as a single well-formed line — not mangled into the middle of the inbound check's error block.*

**Technical context:** Two tightly coupled changes in `src/setup.rs`:

**(a) Remove the errant `println!` from `check_ptr`.** At `src/setup.rs:385-388`:

```rust
Ok(Some(ptr)) => {
    println!("  PTR record: {ptr}");   // <-- this line causes the interleaving
    PreflightResult::Pass
}
```

Delete the `println!`. The PTR value needs to be carried back to the caller some other way.

**(b) Thread the PTR value back to the caller.** Options (developer picks the least-invasive):
- Extend `PreflightResult::Pass` to carry an optional detail string: `Pass(Option<String>)` — requires updating all match arms across the file
- Add a variant like `PassWithDetail(String)` alongside `Pass` — more changes but preserves existing `Pass` usage
- Return `(PreflightResult, Option<String>)` from `check_ptr` specifically — narrowest change, only affects PTR
- Use an out-parameter or a separate getter on `NetworkOps` — uglier but zero touch to `PreflightResult`

Recommendation: extend `PreflightResult::Pass` to `Pass(Option<String>)` since it's the cleanest model and only `check_ptr` uses it today — most match arms can stay as `Pass(_) => println!("PASS")` with a small exception for the PTR case that prints the detail too. Developer has final say.

**(c) Display the PTR value inline.** In `run_preflight` at `src/setup.rs:431-440`, when the PTR check passes with a detail string, print it on the same line as `PASS`:

```
  PTR record... PASS (vps-198f7320.vps.ovh.net.)
```

No interleaving with the inbound error block, no duplicate line, single well-formed output.

PTR remains advisory: `PreflightResult::Warn` on missing/error (non-blocking), `Pass(Some(ptr))` on success. Never `Fail`. Per owner decision, the check stays in preflight because PTR is still useful deliverability guidance even if imperfect (the check can't distinguish a useful PTR from OVH's default, but showing the value to the user at least lets them notice if it's the wrong one).

**Acceptance criteria:**
- [x] `check_ptr` no longer calls `println!` directly
- [x] PTR value is returned to the caller via `PreflightResult` (or equivalent — developer's choice documented in PR) <!-- `PreflightResult::Pass` extended to `Pass(Option<String>)`; non-PTR checks use `Pass(None)` -->
- [x] `run_preflight` displays PTR value inline with `PASS` marker as a single line (e.g., `  PTR record... PASS (vps-198f7320.vps.ovh.net.)`)
- [x] When PTR check returns `Warn` (missing record), the existing `WARN\n  {msg}` output format is preserved
- [x] PTR remains non-blocking: `all_pass` stays `true` when PTR is missing (existing behavior, don't change)
- [x] Unit test: `run_preflight` with a mock `NetworkOps` returning `Some(ptr)` produces a single well-formed line containing both `PASS` and the PTR value, with no intermediate newline
- [x] Unit test: the interleaving bug does not reproduce — assert that the output when inbound fails and PTR passes has the PTR line strictly after the inbound error block, not interleaved <!-- Exceeded: `run_preflight` refactored into `run_preflight_to<W, E>` so stream ordering is asserted with captured buffers, not global stdout -->
- [x] Unit test: `run_preflight` with a mock `NetworkOps` returning `None` for PTR still produces `WARN` + the advisory message, and does not fail the overall preflight
- [x] `cargo fmt -- --check`, `cargo clippy -- -D warnings`, `cargo test` all clean

---

## Sprint 14 — Request Logging for aimx-verify (Days 37–39.5) [DONE]

**Goal:** Add per-request logging to every call served by `aimx-verify` — HTTP and SMTP — so operators can see who's using the service, diagnose issues, and spot abuse directly from the shell output.

**Dependencies:** Sprint 13 (merged) — logging applies to the fixed verify service, not the broken one

### S14.1 — Log All HTTP and SMTP Calls

*As an operator of aimx-verify, I want every HTTP and SMTP call logged with the caller's IP and relevant params so that I can see who's using the service, diagnose issues, and spot abuse directly from the shell output.*

**Technical context:** The verify service at `services/verify/` already initializes `tracing_subscriber::fmt::init()` in `main()` (line 134), but request logging is almost non-existent. `probe()` (line 26) and `health()` (line 19) log nothing — the caller IP is available via `ConnectInfo(addr)` but discarded. `handle_smtp_connection()` (line 117) logs nothing on the success path; only `run_smtp_listener()` logs bind announcement and accept errors.

Add per-request logging to every path. The format stays as the default `tracing-subscriber` pretty text (not JSON) — per owner decision, operators tail the shell or journalctl, not a JSON log aggregator. Log level defaults to `info` and honors `RUST_LOG` overrides.

Log every call, including `/health` (no filtering — owner confirmed ALL calls):

- **HTTP `/probe`**: method, path, caller IP (resolved via Sprint 12's `resolve_client_ip`), response status, elapsed ms, and the EHLO handshake outcome (`reachable: true|false`).
- **HTTP `/reach`** (added in Sprint 12): method, path, caller IP (same resolver), response status, elapsed ms, and the plain-TCP reachability result (`reachable: true|false`).
- **HTTP `/health`**: method, path, caller IP, response status, elapsed ms.
- **SMTP listener (port 25)**: peer IP on accept, and whether the banner/EHLO/QUIT lifecycle (fixed in Sprint 12) completed cleanly or errored. Existing error-path `tracing::debug!` in `run_smtp_listener` should be promoted to `info` / `warn` where appropriate so connection attempts are visible at the default level.

Implementation choice is open: axum's `tower_http::trace::TraceLayer` + a small middleware that extracts `ConnectInfo<SocketAddr>`, or a hand-rolled `axum::middleware::from_fn` wrapper. There are three HTTP routes (`/probe`, `/reach`, `/health`), so a custom middleware is likely simpler than pulling in `tower-http`. Developer's call.

**Acceptance criteria:**
- [x] Every `/probe` request logs method, path, caller IP, response status, elapsed ms, and the `reachable` result at `info` level <!-- Implemented via `log_request` middleware + `ReachableOutcome` response extension so exactly one `info!` line is emitted per request, with the `reachable` field joined onto the same line -->
- [x] Every `/reach` request logs method, path, caller IP, response status, elapsed ms, and the `reachable` result at `info` level
- [x] Every `/health` request logs method, path, caller IP, response status, elapsed ms at `info` level
- [x] Every TCP connection to the SMTP listener logs peer IP on accept and success/error on close at `info` level <!-- Factored into shared `spawn_smtp_connection(stream, peer)` helper so test and production exercise exactly the same logging body (anti-drift) -->
- [x] Log output uses the default `tracing-subscriber` text formatter (not JSON)
- [x] `RUST_LOG` env var still works for level overrides (e.g., `RUST_LOG=aimx_verify=debug`)
- [x] Unit or integration test: hit `/probe` on a local test server and assert a log line containing the caller IP is captured (via `tracing-subscriber`'s test writer or equivalent) <!-- Exceeded: three HTTP integration tests cover /health, /reach (with reachable=false), and /probe 400 (caller_ip=unknown) -->
- [x] Integration test: connect to the SMTP listener on an ephemeral port and assert a log line with the peer IP is captured
- [x] `cargo fmt -- --check`, `cargo clippy -- -D warnings`, `cargo test` all clean in `services/verify/`

---

## Sprint 15 — Dockerize aimx-verify (Days 40–42.5) [DONE]

**Goal:** Ship a Dockerfile and docker-compose for `aimx-verify` so the service can be redeployed to any host consistently without tracking apt packages or systemd units by hand. The deployment must work correctly with the Sprint 12 security model (loopback-default bind + Caddy trust boundary + Layer 4 target guard).

**Dependencies:** Sprint 14 (merged) — Docker ships the fully-instrumented, logging-enabled, security-hardened service.

**Note on Sprint 12 interaction:** Sprint 12 changed the default HTTP bind to `127.0.0.1:3025` and introduced a Layer 3 trust check that reads `X-AIMX-Client-IP` only when the TCP peer is loopback. This means the "simple" docker-compose shape of "bind `0.0.0.0:3025` in the container and port-map to the host" is NOT compatible with the security model — Docker's userland proxy presents peer IPs as the Docker bridge gateway (a private IP), which Layer 4's target guard will reject. The correct deployment pattern is either:

- **(a) `network_mode: host`** for the verify container, so binding `127.0.0.1:3025` inside the container is the host's loopback, and Caddy (running on the host or as a sibling service) can reverse-proxy to it normally. This is the simplest fix and is the recommended shape.
- **(b) Caddy as a second docker-compose service** with an internal Docker network where the verify container binds loopback on its internal interface and Caddy is the only client. More portable (no host-network dependency) but more moving pieces.

The implementer should pick (a) by default unless there's a reason to avoid `network_mode: host`, in which case (b) is the fallback. Document the choice in the Docker README section.

### S15.1 — Dockerfile + docker-compose + README Update

*As the maintainer of aimx-verify, I want to deploy the service from a Docker image with docker-compose so that I can redeploy to any host consistently without tracking apt dependencies or systemd units by hand — and the deployment must respect the Sprint 12 security model.*

**Technical context:** The verify service is a standalone Cargo crate at `services/verify/` (package `aimx-verify`). No Dockerfile exists yet. After Sprint 12, `services/verify/README.md` will document a Caddyfile + loopback-bind deployment as the recommended non-Docker path. This sprint adds the Docker equivalent without regressing the security model.

Add a **multi-stage Dockerfile** at `services/verify/Dockerfile`:
- **Builder stage:** `rust:1-bookworm` (or current stable slim). Cache-friendly layering — copy `Cargo.toml` + `Cargo.lock` first, prime the dep cache with a stub build, then copy `src/` and build `cargo build --release`.
- **Runtime stage:** `debian:bookworm-slim` (glibc target matches the builder — no musl cross-compile complexity). Install `ca-certificates` only. Copy the release binary from the builder to `/usr/local/bin/aimx-verify`.
- Container **runs as root** (per owner decision) so binding port 25 works without capability fiddling.
- `EXPOSE 25 3025`; `ENTRYPOINT ["/usr/local/bin/aimx-verify"]`.

Add **`services/verify/docker-compose.yml`** using **`network_mode: host`** as the default deployment shape:
- Single `verify` service with `build: .`
- `network_mode: host` — the container shares the host's network namespace, so the post-Sprint-12 default bind `127.0.0.1:3025` behaves identically to a systemd-native deployment and the Layer 3 loopback check still works.
- `environment:` block can include a commented `BIND_ADDR` / `SMTP_BIND_ADDR` / `RUST_LOG` example, but defaults inherit from the binary (no override needed).
- `restart: unless-stopped`
- No explicit `ports:` mapping when using `network_mode: host` — the container binds directly on the host.
- Caddy is NOT included in the compose file in this sprint (operators run Caddy separately on the host, using the Sprint 12 canonical `Caddyfile`). A future sprint could add a Caddy sibling service if desired, but it's out of scope here.

Add **`services/verify/.dockerignore`** excluding `target/` and other build artifacts.

**Update `services/verify/README.md`** with a new "Docker" section that:
- Documents `docker compose up -d --build` as the Docker deployment path, with `network_mode: host` as the default shape
- Explains the Sprint 12 security model interaction — why `network_mode: host` rather than port mapping
- References the canonical `services/verify/Caddyfile` from Sprint 12 as the required companion on the host
- Provides a raw `docker build` + `docker run --network host` example as an alternative
- Does NOT replace the systemd section from Sprint 12 — both deployment paths coexist

**Do NOT update the repo-root `README.md`** — per owner decision, end users don't run verify.

No GitHub Actions image publishing to ghcr.io in this sprint — not requested. No new CI docker-build step either — existing `services/verify/` CI steps from S8.5 stay unchanged.

**Acceptance criteria:**
- [x] `services/verify/Dockerfile` uses a multi-stage build (Rust builder + `debian:bookworm-slim` runtime) <!-- Exceeded: Rust builder pinned to `rust:1.94-bookworm`, `cargo build --release --locked` in both builder RUN steps, HEALTHCHECK directive hitting `/health` via curl -->
- [x] Final image runs as root and has `ENTRYPOINT` pointing at the binary
- [x] `services/verify/.dockerignore` excludes `target/` and other build artifacts
- [x] `services/verify/docker-compose.yml` builds from the local Dockerfile and uses `network_mode: host` so the post-Sprint-12 loopback-default bind works without override
- [x] Manually verified: `docker compose up -d --build` in `services/verify/` brings the service up; `curl http://127.0.0.1:3025/health` from the host returns `{"status":"ok","service":"aimx-verify"}` <!-- Dev-host smoke test used `docker build` + `docker run --network host` (compose v2 plugin unavailable); functionally equivalent. Healthcheck reports `Status: healthy`. -->
- [ ] Manually verified: with the Sprint 12 canonical `Caddyfile` running on the host, `curl https://<domain>/probe` from a remote machine returns a correctly-resolved caller IP (not `127.0.0.1`) and a valid probe result — this proves the container + Caddy + Sprint 12 security model all work end-to-end <!-- NOT EXECUTED: requires production VPS with public DNS + Caddy + remote client. Reviewer confirmed code would satisfy on a real host. Operator must run once before production sign-off. -->
- [ ] Manually verified: with the Sprint 12 canonical `Caddyfile` running on the host, `curl https://<domain>/reach` from a remote machine returns a plain-TCP reachability result <!-- NOT EXECUTED: same reason as /probe — production VPS required. -->
- [x] Manually verified: `nc 127.0.0.1 25` from the host receives the `220 check.aimx.email SMTP aimx-verify` banner
- [x] Manually verified: the per-request logs from Sprint 14 appear in the container's stdout (`docker compose logs verify`) when the endpoints are exercised
- [x] `services/verify/README.md` has a new "Docker" section documenting `docker compose up -d --build` with `network_mode: host`, explains the Sprint 12 interaction, references the canonical `Caddyfile`
- [x] Repo-root `README.md` is NOT modified

---

## Sprint 16 — Add Caddy to docker-compose (Days 43–45.5) [DONE]

**Goal:** Make `docker compose up` a single-command deployment for aimx-verify + Caddy, eliminating the need to install and manage Caddy separately on the host. Both services use `network_mode: host` so the Sprint 12 security model (loopback trust + Layer 4 target guard) is fully preserved.

**Dependencies:** Sprint 15 (merged) — Dockerfile, docker-compose, and `.dockerignore` already exist.

### S16.1 — Add Caddy service to docker-compose

*As the maintainer of aimx-verify, I want a single `docker compose up -d` to bring up both the verify service and Caddy so that I don't have to install, configure, or manage Caddy separately on the host.*

**Context:** Sprint 15 shipped docker-compose with only the verify service and documented "run Caddy on the host separately." This works but means the operator manages two deployment systems (Docker for verify, systemd/package for Caddy). Since both can use `network_mode: host` without any security regression — Caddy connects to verify via real loopback, identical to the current setup — bundling them into one compose file simplifies ops with zero tradeoff.

**Priority:** P1

- [x] Add `caddy` service to `services/verify/docker-compose.yml` using the official `caddy:2` image, `network_mode: host`, `restart: unless-stopped`
- [x] Mount the existing `Caddyfile` into the Caddy container (read-only)
- [x] Add a named volume `caddy_data` mapped to `/data` for persistent TLS cert storage
- [x] Add a named volume `caddy_config` mapped to `/config` for Caddy runtime config
- [x] `DOMAIN` environment variable configurable (with default `check.aimx.email` matching the Caddyfile's `{$DOMAIN}` placeholder)
- [x] Update the docker-compose header comment to reflect that Caddy is now included
- [x] Update `services/verify/README.md` Docker section to document the all-in-one compose deployment, including the `DOMAIN` env var and cert volume
- [ ] Manually verified: `docker compose up -d --build` brings up both services; `curl http://127.0.0.1:3025/health` returns OK; Caddy logs show it is listening on 443 <!-- Pending: requires Docker host with ports 25/80/443 available -->

---

## Sprint 17 — Rename Verify Service to Verifier (Days 46–48.5) [DONE]

**Goal:** Rename the hosted verification service from "verify" / "aimx-verify" to "verifier" / "aimx-verifier" across all code, Docker, CI, and documentation. The service is the verifier; the `aimx verify` CLI command is the client that checks against it — the naming should reflect this distinction. Landing this before the documentation overhaul in Sprint 18 avoids writing docs with the old name.

**Dependencies:** All prior sprints complete.

### S17.1 — Rename service crate, Docker, and CI

**Context:** The hosted verification service currently lives at `services/verify/` with package name `aimx-verify` and binary `aimx-verify`. Rename the service to "verifier" for clarity — it is the verifier service, while `aimx verify` is the client-side CLI command that checks against it. This story covers all functional artifacts: the crate directory, package name, binary name, source code service-identification strings, Dockerfile, docker-compose, and CI workflow. Does NOT touch the `aimx verify` CLI command, `src/verify.rs` module, `verify_host` config field, or `check.aimx.email` domain.

**Priority:** P1

- [x] Rename directory `services/verify/` → `services/verifier/`
- [x] Update `services/verifier/Cargo.toml`: package name `aimx-verify` → `aimx-verifier`
- [x] Update `services/verifier/Dockerfile`: all references to binary name `aimx-verify` → `aimx-verifier` (strip, COPY, ENTRYPOINT)
- [x] Update `services/verifier/docker-compose.yml`: image `aimx-verify:local` → `aimx-verifier:local`, container name `aimx-verify` → `aimx-verifier`, comments
- [x] Update `services/verifier/src/main.rs`: service identification strings (`"aimx-verify"` → `"aimx-verifier"` in health response, SMTP banner, log messages)
- [x] Update `.github/workflows/ci.yml`: job name, `working-directory`, and cache key references from `services/verify` → `services/verifier`
- [x] Run `cargo build` and `cargo test` in `services/verifier/` to verify clean build
- [x] Run CI lint (`cargo clippy`, `cargo fmt --check`) in `services/verifier/`

### S17.2 — Update all documentation and project references

**Context:** With the service crate renamed in S17.1, all documentation must reflect the new "verifier" / "aimx-verifier" naming. This covers README, CLAUDE.md, the user guide (`docs/guide/`), manual setup doc, the verifier service's own README, PRD section heading, and historical sprint plan references. The `aimx verify` CLI command name and `verify_host` config field are unchanged — only references to the service/crate/binary name are updated.

**Priority:** P1

- [x] Update `README.md`: section heading "Verify service" → "Verifier service", path references `services/verify/` → `services/verifier/`, binary references `aimx-verify` → `aimx-verifier`
- [x] Update `CLAUDE.md`: path `services/verify/` → `services/verifier/`, crate name `aimx-verify` → `aimx-verifier`
- [x] Update `docs/guide/setup.md`: section heading, path references, binary name, systemd unit name `aimx-verify.service` → `aimx-verifier.service`, user name references
- [x] Update `docs/guide/configuration.md`: comment text referencing the verify service → verifier service (config field `verify_host` stays as-is)
- [x] Update `docs/manual-setup.md`: section heading, path references, binary name, systemd references, user name references
- [x] Update `services/verifier/README.md`: any self-references to old naming
- [x] Update `docs/prd.md`: section heading "6.8 Verify Service" → "6.8 Verifier Service", milestone M7 description
- [x] Update `docs/sprint.md`: header metadata description, Summary Table entries that reference the service name

---

## Sprint 18 — Guided Setup UX (Days 49–51.5) [DONE]

**Goal:** Make `aimx setup` fully interactive so new users don't need to know the CLI signature. Prompt for domain when omitted, confirm DNS access, and suppress OpenSMTPD's debconf screens by pre-seeding answers from the domain the user provides.

**Dependencies:** All prior sprints complete.

### S18.1 — Interactive domain prompt when no argument given

**Context:** Currently `aimx setup <domain>` requires the domain as a mandatory positional arg. Users discovering the tool shouldn't need to read help text to get started. When `domain` is omitted, the setup wizard should prompt for it, then ask the user to confirm they control the domain and have access to its DNS settings (MX, SPF, DKIM records will need updating). If the domain IS provided as an arg, skip the prompts and proceed as today — preserving scripting/backward compatibility.

**Priority:** P1

- [x] Change `domain` from required `String` to `Option<String>` in the `Setup` clap variant
- [x] When `None`, prompt: "Enter the domain you want to use for email (e.g. agent.example.com):"
- [x] After domain entry, display confirmation: "You will need to add MX, SPF, and DKIM DNS records for this domain. Do you control this domain and have access to its DNS settings? (y/N)"
- [x] Exit gracefully if user declines
- [x] Existing `aimx setup example.com` invocation continues to work without prompts
- [x] Tests cover both paths (domain provided, domain prompted)

### S18.2 — Automate OpenSMTPD debconf screens during install

**Context:** `apt-get install -y opensmtpd` still pops two debconf screens (system mail name, root/postmaster recipient) because `DEBIAN_FRONTEND` isn't set. On a fresh VPS these block the automated flow and confuse users who don't know what to enter. Pre-seed the answers using `debconf-set-selections` before install: set the mail name to the user's domain, leave root recipient blank (aimx handles delivery via its own MDA, not system aliases). Set `DEBIAN_FRONTEND=noninteractive` on the apt-get command.

**Priority:** P1

- [x] Before `apt-get install`, run `debconf-set-selections` to pre-seed: `opensmtpd opensmtpd/mailname string <domain>` and `opensmtpd opensmtpd/root_address string` (blank)
- [x] Set `DEBIAN_FRONTEND=noninteractive` env var on the `apt-get install` command
- [x] If `debconf-set-selections` is not available, fall back to just `DEBIAN_FRONTEND=noninteractive` (the defaults will apply)
- [x] Test: mock `install_package` path verifies debconf pre-seeding is called with correct domain before install

### S18.3 — Restructure and colorize post-setup output

**Context:** The current post-setup output dumps DNS records, MCP config, Gmail filter instructions, and PTR notes as an undifferentiated wall of text. Users need to scan it to find what's relevant to them. Restructure into three clearly labeled sections displayed in this order: **[DNS]** (MX, A, SPF, DKIM, DMARC records — exclude PTR), **[MCP]** (tool-agnostic configuration snippet mentioning Claude Code, OpenClaw, Codex, OpenCode, and other MCP-compatible AI agents), **[Deliverability Improvement (Optional)]** (PTR record guidance, Gmail filter/whitelist instructions). Add ANSI colors throughout setup output for status indicators (green PASS, red FAIL/MISSING, yellow WARN), section headers, and key values to improve scannability. No color library exists yet — add `colored` crate or similar.

**Priority:** P1

- [x] Add a terminal color library (e.g. `colored` crate) to `Cargo.toml`
- [x] Restructure `finalize_setup()` and related display functions to output three labeled sections in order: `[DNS]`, `[MCP]`, `[Deliverability Improvement (Optional)]`
- [x] DNS section: MX, A, SPF, DKIM, DMARC records only — no PTR
- [x] MCP section: replace Claude Code-specific heading with tool-agnostic text listing Claude Code, OpenClaw, Codex, OpenCode as examples of MCP-compatible server-side AI agents
- [x] Deliverability section: PTR record guidance + Gmail filter/whitelist instructions, clearly marked optional
- [x] Apply colors to all setup output: green for PASS, red for FAIL/MISSING, yellow for WARN, bold for section headers
- [x] DNS verification results also use colored status indicators
- [x] Colors degrade gracefully (no ANSI when stdout is not a TTY)
- [x] Remove PTR check from `run_preflight_to()` — preflight only checks outbound and inbound port 25
- [x] PTR check remains in the setup flow but displays under [Deliverability Improvement (Optional)], not as a preflight gate
- [x] Update existing preflight tests to remove PTR expectations
- [x] `aimx preflight` output shows only port 25 results (no PTR line)

### S18.4 — Re-entrant setup and DNS retry flow

**Context:** Currently `aimx setup` always runs the full install+configure flow, and after displaying DNS records it offers a single Enter-to-verify prompt. Two improvements: (1) When the user runs `sudo aimx setup <domain>` on an already-configured domain (OpenSMTPD running, TLS cert exists, DKIM key exists), skip the install/configure steps and go straight to checking DNS, MCP, and deliverability — making re-runs a quick verification pass. (2) At the DNS verification step, let the user hit Enter to retry the check (for when they've just updated DNS in another tab), or display a clear message advising them to update DNS and resume with `sudo aimx setup` again later. This replaces the current one-shot "press Enter to verify... sorry, not yet" flow.

**Priority:** P1

- [x] Detect already-configured state: OpenSMTPD running, TLS cert present, DKIM key present, smtpd.conf already configured for this domain
- [x] When already configured, skip install/configure steps — proceed directly to section checks (DNS verification, MCP display, deliverability tips)
- [x] At DNS verification prompt: allow user to press Enter to re-check, or display guidance: "Update your DNS records and run `sudo aimx setup <domain>` again to verify"
- [x] DNS retry loop: re-run verification on each Enter press, exit loop when all pass or user chooses to defer
- [x] All preflight checks (port 25 outbound/inbound) also run on re-entrant invocations
- [x] Existing fresh-install flow unchanged for first-time setup

### S18.5 — Update and relocate user guide

**Context:** The user guide in `docs/guide/` (8 files: index, getting-started, setup, configuration, mailboxes, channels, mcp, troubleshooting) needs updating to reflect Sprint 18 changes: the new sectioned setup output ([DNS]/[MCP]/[Deliverability]), re-entrant `aimx setup` behavior, PTR removal from preflight, and MCP tool-agnostic language. Additionally, move the guide from `docs/guide/` to `book/` at the project root for a cleaner separation between internal planning docs (`docs/`) and user-facing documentation (`book/`).

**Priority:** P1

- [x] Move `docs/guide/` to `book/` — update any cross-references between guide files if needed
- [x] Update `book/setup.md` to reflect the new three-section output format ([DNS], [MCP], [Deliverability Improvement (Optional)]) and the re-entrant setup flow (re-running `aimx setup` skips install, goes straight to verification)
- [x] Update `book/setup.md` to reflect that preflight only checks port 25 (no PTR)
- [x] Update `book/mcp.md` to use tool-agnostic language — mention Claude Code, OpenClaw, Codex, OpenCode as examples of compatible MCP clients
- [x] Update `book/getting-started.md` and `book/troubleshooting.md` for consistency with the new setup flow
- [x] Update `book/index.md` if it references the old directory structure or outdated setup behavior

---

## Sprint 19 — Embedded SMTP Receiver (Days 52–54.5) [DONE]

**Goal:** Build a hand-rolled tokio-based SMTP listener that accepts inbound email and calls `ingest_email()` in-process. No CLI wiring yet — this sprint produces the library code that `aimx serve` will use.

**Dependencies:** None (builds alongside existing code, doesn't modify it yet)

### S19.1 — SMTP Protocol State Machine

**Context:** aimx needs a receive-only SMTP server to replace OpenSMTPD's listener role. Rather than depending on `mailin-embedded` (~1,400 total downloads, unclear maintenance), we hand-roll a minimal tokio SMTP listener. The protocol for receiving is straightforward: the server responds to EHLO, MAIL FROM, RCPT TO, DATA, QUIT, RSET, and NOOP. Each connection is a state machine progressing through these phases. Implement as a standalone module (`src/smtp.rs` or `src/smtp/`) that can be driven by `serve.rs` later. Use tokio `TcpListener` + `TcpStream` with per-connection tasks. Enforce per-connection timeouts (5 min idle, 10 min total) and message size limits (25 MB default, configurable).

**Priority:** P0

- [x] SMTP state machine handles: EHLO/HELO → 250, MAIL FROM → 250, RCPT TO → 250, DATA → 354/250, QUIT → 221, RSET → 250, NOOP → 250
- [x] Proper error responses: 500 for unrecognized commands, 503 for out-of-sequence commands, 552 for oversized messages
- [x] Per-connection timeout: 5 min idle between commands, 10 min total connection time
- [x] Message size limit: 25 MB default (configurable via config.toml)
- [x] Multi-recipient support: multiple RCPT TO per message, all collected and passed downstream
- [x] Graceful connection teardown on timeout or client disconnect
- [x] Unit tests for every SMTP command (valid and invalid sequences)
- [x] Unit tests for timeout behavior and size limit enforcement

### S19.2 — STARTTLS Support

**Context:** Inbound SMTP servers must offer STARTTLS for opportunistic encryption. aimx setup already generates self-signed TLS certs at `/etc/ssl/aimx/`. The SMTP listener needs to load these certs and upgrade plain connections to TLS when the client sends STARTTLS. Use `tokio-rustls` (already indirectly depended on via `mail-auth`'s dependency tree). Advertise STARTTLS in EHLO response. Both plain and TLS connections must be accepted — many MTAs still connect without TLS.

**Priority:** P0

- [x] STARTTLS advertised in EHLO capabilities list
- [x] STARTTLS command upgrades the connection to TLS using `tokio-rustls`
- [x] TLS certs loaded from paths in config.toml (default: `/etc/ssl/aimx/cert.pem`, `/etc/ssl/aimx/key.pem`)
- [x] Plain (non-TLS) connections still accepted and fully functional
- [x] Invalid/missing cert paths produce clear startup error, not a panic
- [x] Unit test: STARTTLS upgrade with test certificates
- [x] Unit test: plain connection works without STARTTLS

### S19.3 — Ingest Pipeline Integration

**Context:** When the SMTP listener completes receiving a DATA payload, it must call `ingest::ingest_email()` with the raw bytes and recipient address — the same function OpenSMTPD's MDA currently invokes via `aimx ingest`. This happens in-process (no subprocess spawn). The existing `ingest_email()` function already accepts `&[u8]` and a recipient string, so no changes to `ingest.rs` are needed. The listener must handle ingest failures gracefully: log the error, return a 451 temporary failure to the sending MTA (so it retries), and continue accepting connections.

**Priority:** P0

- [x] On DATA completion, call `ingest_email(&config, &rcpt, &raw_bytes)` for each recipient
- [x] Successful ingest returns 250 to the sending MTA
- [x] Failed ingest returns 451 (temporary failure) — sending MTA will retry
- [x] Ingest failure is logged with error details but does not crash the listener
- [x] Config is loaded once at startup and shared across connections (Arc)
- [x] Integration test: start listener on a random port, connect with a test SMTP client, send a fixture `.eml`, verify `.md` file is created in the correct mailbox

### S19.4 — Connection Hardening

**Context:** A publicly-exposed SMTP listener on port 25 will see probes, bots, and malformed input. Basic hardening: limit concurrent connections (default: 100), limit commands per connection before DATA (50), reject bare LF (RFC 5321 requires CRLF), and log connection metadata (peer IP, elapsed time, result). No spam filtering in v1 — that's deferred to DMARC policy and future work.

**Priority:** P1

- [x] Concurrent connection limit (default: 100) — new connections get 421 when limit is reached
- [x] Per-connection command limit (50 commands before DATA) — prevents command flooding
- [x] Reject bare LF in DATA (require CRLF line endings per RFC 5321)
- [x] Log each connection: peer IP, EHLO hostname, recipient count, message size, duration, result (accepted/rejected/timeout)
- [x] Unit test: connection limit enforcement
- [x] Unit test: command flood triggers limit

---

## Sprint 20 — Direct Outbound Delivery (Days 55–57.5) [DONE]

**Goal:** Replace `/usr/sbin/sendmail` with direct SMTP delivery using `lettre` + `hickory-resolver` for MX resolution. Synchronous delivery with clear error feedback — no background queue.

**Dependencies:** Sprint 19 (conceptually parallel — Sprint 20 modifies `send.rs` which Sprint 19 doesn't touch)

### S20.1 — MX Resolution

**Context:** To deliver email without sendmail, aimx must resolve the recipient's domain to an MX server and connect directly. Add `hickory-resolver` (successor to `trust-dns-resolver`) for DNS resolution. Look up MX records, fall back to A record if no MX exists (per RFC 5321 §5.1), and return a priority-ordered list of server hostnames. This is a small utility module (~50-80 lines) used by the outbound transport.

**Priority:** P0

- [x] Add `hickory-resolver` to Cargo.toml (verify MIT/Apache-2.0 license per NFR-3)
- [x] `resolve_mx(domain: &str) -> Result<Vec<String>>` returns MX hostnames sorted by priority (lowest preference value first)
- [x] Fall back to A record if no MX records exist (RFC 5321 §5.1)
- [x] Handle NXDOMAIN / no records with clear error: "No mail server found for domain X"
- [x] Unit tests: valid MX, no MX with A fallback, NXDOMAIN error
- [x] Integration test with real DNS resolution against a known domain (e.g., `gmail.com` has MX records)

### S20.2 — Lettre SMTP Transport

**Context:** Replace `SendmailTransport` (which shells out to `/usr/sbin/sendmail -t`) with a new `LettreTransport` that implements the existing `MailTransport` trait. The flow: resolve MX for recipient domain (S20.1), connect to the highest-priority server, negotiate STARTTLS, deliver the DKIM-signed message. Try each MX server in priority order — if the first is unreachable, fall back to the next. lettre's `AsyncSmtpTransport` handles the SMTP conversation. The key constraint: delivery is synchronous from the caller's perspective (no background queue). If all MX servers reject or are unreachable, return an error immediately.

**Priority:** P0

- [x] Add `lettre` to Cargo.toml (verify MIT/Apache-2.0 license per NFR-3)
- [x] `LettreTransport` implements `MailTransport` trait
- [x] Connects to MX servers in priority order — falls back to next on connection failure
- [x] STARTTLS negotiated opportunistically (try TLS, fall back to plain if server doesn't support it)
- [x] Delivery timeout: 60 seconds per MX attempt
- [x] Error messages are specific and actionable: "Connection refused by mx1.example.com", "Recipient rejected by mx2.example.com: 550 User unknown", "All MX servers for example.com unreachable"
- [x] Unit tests using `MailTransport` trait mock (existing pattern)
- [ ] Integration test: deliver to a local test SMTP server (can reuse Sprint 19's listener) <!-- Deferred: reviewer accepted deferral to Sprint 21 where aimx serve provides the test listener -->

### S20.3 — Error Feedback for Agents

**Context:** With synchronous delivery, send failures must be clearly communicated to agents via MCP tools and CLI. Today, `sendmail` swallows errors into its queue — the caller never knows if delivery failed. The new transport returns errors immediately. Update `email_send` and `email_reply` MCP tools to include the specific error in their response. Update `aimx send` CLI to print the error and exit with a non-zero code. This is better for agents — they get immediate feedback and can decide whether to retry.

**Priority:** P0

- [x] `aimx send` CLI: print specific delivery error to stderr, exit code 1 on failure
- [x] `email_send` MCP tool: return error with delivery failure details in MCP error response
- [x] `email_reply` MCP tool: same error handling as `email_send`
- [x] Success responses include confirmation: "Delivered to mx1.example.com for recipient@example.com"
- [x] Unit tests: verify error propagation from transport through CLI/MCP

### S20.4 — Remove Sendmail Dependency

**Context:** With `LettreTransport` as the default, remove `SendmailTransport` and all references to `/usr/sbin/sendmail`. This is a clean removal — the `MailTransport` trait stays, only the sendmail implementation goes. Update `send.rs` to use `LettreTransport` as the default in `run()`. Remove any sendmail path checks or error messages from setup.

**Priority:** P1

- [x] Remove `SendmailTransport` struct and implementation from `send.rs`
- [x] `send::run()` uses `LettreTransport` by default
- [x] Remove any `/usr/sbin/sendmail` path references across the codebase
- [x] All existing send tests pass with `LettreTransport` (via mock trait)
- [x] `cargo clippy` clean — no dead code warnings from removed sendmail code

---

## Sprint 21 — `aimx serve` Daemon + CLI Wiring (Days 58–60.5) [DONE]

**Goal:** Wire the SMTP listener and outbound transport into `aimx serve`, making it a runnable daemon with systemd integration and graceful shutdown.

**Dependencies:** Sprint 19 (SMTP listener), Sprint 20 (outbound transport)

### S21.1 — CLI + Main Dispatch

**Context:** Add the `serve` subcommand to aimx's CLI. `aimx serve` starts the embedded SMTP listener from Sprint 19 and keeps it running until terminated. Options: `--bind` (default `0.0.0.0:25`), `--tls-cert` and `--tls-key` (default from config or `/etc/ssl/aimx/`). Wire into `main.rs` dispatch alongside existing commands. The `aimx ingest` CLI subcommand remains unchanged — it's still useful for manual/pipe usage and backward compatibility with any external MTA.

**Priority:** P0

- [ ] `Command::Serve` added to `cli.rs` with `--bind`, `--tls-cert`, `--tls-key` options
- [ ] `main.rs` dispatches `Command::Serve` to `serve::run()`
- [ ] `serve::run()` starts the SMTP listener from Sprint 19 and blocks until shutdown
- [ ] `--bind` defaults to `0.0.0.0:25`, supports `host:port` format
- [ ] TLS cert/key paths default to config values, then `/etc/ssl/aimx/cert.pem` and `/etc/ssl/aimx/key.pem`
- [ ] `aimx serve --help` displays usage
- [ ] `aimx ingest` remains functional (backward compatibility for manual piping)

### S21.2 — Signal Handling + Graceful Shutdown

**Context:** `aimx serve` runs as a long-lived daemon and must handle Unix signals properly. SIGTERM and SIGINT trigger graceful shutdown: stop accepting new connections, finish processing in-flight messages (up to 30s grace period), then exit. Log shutdown events. Use `tokio::signal` for signal handling. No PID file for v1 — systemd tracks the process via its cgroup, and `ss -tlnp` can identify the port 25 listener.

**Priority:** P0

- [ ] SIGTERM triggers graceful shutdown: stop accepting, drain in-flight (30s timeout), exit 0
- [ ] SIGINT (Ctrl+C) same behavior as SIGTERM
- [ ] Log on startup: "aimx SMTP listener started on 0.0.0.0:25"
- [ ] Log on shutdown: "aimx SMTP listener shutting down (N connections in-flight)"
- [ ] In-flight connections that exceed 30s grace period are forcefully closed
- [ ] Unit test: shutdown signal stops accept loop

### S21.3 — Systemd + OpenRC Service Files

**Context:** Most aimx deployments will run on systemd-based Linux. `aimx setup` (updated in Sprint 22) will install the generated unit file. For Sprint 21, create the unit file template and the code to generate it. The unit should: start after network, run as root (for port 25 binding), restart on failure with backoff, and use `StandardOutput=journal` for logging. Also generate a basic OpenRC init script for Alpine Linux (cross-platform support per NFR-4 update).

**Priority:** P1

- [ ] Systemd unit file template in code: `After=network.target`, `ExecStart=/usr/local/bin/aimx serve`, `Restart=on-failure`, `RestartSec=5s`
- [ ] `generate_systemd_unit(aimx_path: &str, data_dir: &str) -> String` produces the unit file content
- [ ] OpenRC init script template for Alpine: `command=/usr/local/bin/aimx`, `command_args=serve`, `supervisor=supervise-daemon`
- [ ] Init system detection: check for `/run/systemd/system` (systemd) vs `/sbin/openrc` (OpenRC)
- [ ] Unit tests: generated unit file content matches expected format for both init systems

### S21.4 — End-to-End Daemon Test

**Context:** Verify the full `aimx serve` lifecycle: start → accept SMTP connection → receive email → ingest to Markdown → shut down cleanly. This is the first time the embedded SMTP listener, ingest pipeline, and daemon management are tested together. Use `assert_cmd` or spawn `aimx serve` as a child process on a random high port, send a test email via SMTP, verify the `.md` file appears, then send SIGTERM and verify clean exit.

**Priority:** P0

- [ ] Integration test: spawn `aimx serve --bind 127.0.0.1:<random-port> --data-dir <tempdir>`, send fixture email via raw SMTP, verify `.md` created, SIGTERM, verify clean exit
- [ ] Test covers: multi-recipient delivery (one email, two RCPT TO, two `.md` files)
- [ ] Test covers: connection after SIGTERM is refused (listener stopped)
- [ ] All existing `cargo test` tests still pass (no regressions)

---

*Archive 2 of 2. See [`sprint.md`](sprint.md) for the active plan and full Summary Table.*
