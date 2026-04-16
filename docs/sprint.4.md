# AIMX — Sprint Archive 4

> **Sprints 31–37** | Archived from [`sprint.md`](sprint.md) | Archive 4 of 4

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

## Sprint 34 — UDS Wire Protocol + `aimx serve` Send Listener (Days 97–99.5) [DONE]

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

- [x] `src/send_protocol.rs` added; exports `SendRequest`, `SendResponse`, `ErrCode`, `parse_request`, `write_response`
- [x] `parse_request` reads the leading `AIMX/1 SEND\n` line, then headers until blank line, then `Content-Length` bytes
- [x] Required headers: `From-Mailbox`, `Content-Length`. Unknown headers ignored for forward-compat
- [x] Rejects: wrong leading line → `Malformed`; missing required header → `Malformed`; `Content-Length` not parseable or exceeds cap → `Malformed`; body truncated → `Malformed`
- [x] `write_response` emits `AIMX/1 OK <message-id>\n` or `AIMX/1 ERR <code> <reason>\n` (codes rendered as `MAILBOX`, `DOMAIN`, `SIGN`, `DELIVERY`, `TEMP`, `MALFORMED`)
- [x] Round-trip unit tests for every response variant; `tokio-test::io::Builder` used for controlled async streams
- [x] Parser fuzzed lightly with: truncated body, oversized body, CRLF vs LF, header case-insensitivity on names, duplicate `Content-Length`, missing blank line, empty body, body containing the literal `AIMX/1 SEND\n` (must NOT be misparsed as a second request)
- [x] `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt -- --check` clean

#### S34-2: `aimx serve` binds `/run/aimx/send.sock` + accept loop

**Context:** Extend `src/serve.rs` to bind a `tokio::net::UnixListener` on `/run/aimx/send.sock` alongside the existing TCP SMTP listener. The socket is world-writable: explicitly `set_permissions(0o666)` after bind, owner left as `root:root` (no chown). On every accept, read `SO_PEERCRED` via `tokio::net::UnixStream::peer_cred()` and log `peer_uid` / `peer_pid` to the existing tracing pipeline (which already routes to journald) — for diagnostics only, not used for authorization. Each accepted connection gets its own tokio task — no bounded semaphore in this sprint (defense-in-depth concurrency cap can be a follow-up if review flags it). Bind failures are fatal at startup (`main` returns non-zero); runtime accept errors are logged and do not kill the listener.

**Priority:** P0

- [x] `serve.rs` binds `UnixListener` at `send_socket_path()` (new helper: `<runtime_dir>/send.sock`; `AIMX_RUNTIME_DIR` env var overrides for tests)
- [x] Socket mode set to `0o666` (world-writable) after bind via `set_permissions`; owner left as the running process's UID (root on real installs, the test user in tests — no explicit chown call)
- [x] `SO_PEERCRED` read on each accept; `peer_uid`/`peer_pid` emitted at `info` level via `tracing` for diagnostics — explicitly NOT used for any authorization check
- [x] Bind failure: process exits with `1` and a clear `error!` log naming the socket path and the errno
- [x] If the socket file already exists at bind time (stale from prior crash), unlink-and-retry once, then fail loudly on second failure
- [x] SIGTERM/SIGINT graceful shutdown drains the UDS accept loop the same way it drains the SMTP listener; socket file removed on clean shutdown
- [x] Unit test binds the listener in a tempdir (override via `AIMX_RUNTIME_DIR`), asserts the file mode is `0o666`, connects from the same process, asserts accept fires and peer-cred fields are present
- [x] Integration test: start `aimx serve` in a tempdir (systemd unit generation bypassed, binary invoked directly), connect a raw Unix socket, assert the listener accepts
- [x] `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt -- --check` clean

#### S34-3: Daemon-side send handler — domain validation, DKIM sign, deliver

**Context:** Author `src/send_handler.rs` (new module) containing the per-connection handler: accept a `SendRequest`, look up the sender's `From:` header, validate the domain against `<config_dir>/config.toml`'s primary domain (case-insensitive, any local part accepted), DKIM-sign via the `Arc<DkimKey>` loaded at `aimx serve` startup (see below), deliver via the existing `LettreTransport` MX resolution path from Sprint 20, and emit the appropriate `SendResponse`. Error mapping: `From:` missing or malformed → `Malformed`; sender domain mismatch → `Domain`; `From-Mailbox` not registered in config → `Mailbox`; DKIM signing failure → `Sign`; permanent SMTP error from any MX → `Delivery` (with the last remote response in the `reason`); transient SMTP error → `Temp`. The DKIM key is loaded in `serve.rs::main` before the accept loop starts; a failure to load is fatal (`aimx serve` refuses to start). Every concurrent send is an independent tokio task — no queue, no Mutex yet (filename-allocation Mutex comes in Sprint 38 with sent-items persistence).

**Priority:** P0

- [x] `src/send_handler.rs` created; `async fn handle_send(req: SendRequest, ctx: &SendContext) -> SendResponse`
- [x] `SendContext` holds `Arc<DkimKey>`, primary domain, registered mailboxes, and an `Arc<dyn MailTransport>` for injection in tests
- [x] `serve.rs::main` loads DKIM key once at startup; start failure is fatal with a clear message naming `/etc/aimx/dkim/private.key`
- [x] `From:` parsing extracts local@domain; domain compare is case-insensitive; any local part accepted
- [x] Domain mismatch returns `ERR DOMAIN sender domain does not match aimx domain`
- [x] Unknown `From-Mailbox` returns `ERR MAILBOX mailbox \`<name>\` not registered`
- [x] DKIM signing uses relaxed/relaxed canonicalization (preserving Sprint 25 fix); sign failure returns `ERR SIGN <detail>` <!-- Cycle 2: added `handle_send_with_signer<F>` seam + `sign_failure_returns_sign_error` test -->
- [x] Delivery uses existing `LettreTransport`; MX resolution errors map to `Temp`, permanent rejects to `Delivery`
- [x] Response written to the UDS stream via `send_protocol::write_response`
- [x] Accept-loop task is spawned with `tokio::spawn` so one slow delivery doesn't block other sends
- [x] Unit tests mock `MailTransport`, exercise each error code path, and assert the right `SendResponse` variant
- [x] Integration test: `aimx serve` running in a tempdir with a mock transport; a raw UDS test client writes `AIMX/1 SEND` + valid body; test asserts `OK <message-id>` response AND the transport received the signed message AND the signature verifies against the public key <!-- Cycle 2: `uds_end_to_end_signed_delivery` now uses `mail-auth`'s `MessageAuthenticator::verify_dkim` with an in-memory resolver cache seeded from the test public key; asserts `DkimResult::Pass` -->
- [x] `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt -- --check` clean

**Cycle 2 additions beyond spec:** To-header normalized to bare address via `extract_bare_address` helper (so display-name and comma-list `To:` headers parse correctly in lettre); missing `Message-ID` synthesized on the server as `<{uuid}@{primary_domain}>` and signed into the message (rather than classified as `Malformed`); single-pass `scan_headers` replaces three `header_value` walks.

---

## Sprint 35 — `aimx send` Thin UDS Client + End-to-End (Days 99.5–102) [DONE]

**Goal:** Rewrite `aimx send` as a thin UDS client that does no signing, owns no DKIM key access, and shells the full signing + delivery responsibility to `aimx serve`. Validate the full path: `aimx send` → UDS → `aimx serve` → DKIM-sign → MX delivery.

**Dependencies:** Sprint 34 (wire protocol, UDS listener, daemon-side handler).

**Design notes:**
- After this sprint, `src/send.rs` is radically smaller — no `load_private_key()`, no `DkimSigner`, no `LettreTransport`. All of that now lives in `aimx serve`. The client composes the RFC 5322 message, opens the socket, writes one request frame, reads one response frame, exits with the status.
- The socket is world-writable (`0o666` from Sprint 34), so `connect()` does not fail with `EACCES` for any local user. The only socket-related error path the client surfaces is "socket missing" (daemon not running).
- `aimx send` no longer needs root. In fact, it now refuses to run as root (consistent with `aimx agent-setup`'s pattern) so agents don't accidentally mint mail through the daemon they themselves supervise.

#### S35-1: Rewrite `src/send.rs` as a UDS client

**Context:** Strip `src/send.rs` to the bare composer + client role: compose the unsigned RFC 5322 message (subject, from, to, cc, bcc, body, attachments — existing `compose_message()` stays), open `UnixStream::connect(send_socket_path())`, write an `AIMX/1 SEND` request via `send_protocol::write_request` (new helper mirroring `write_response`), await the response, map each response variant to a stable CLI exit code + user-facing message. Delete: `load_private_key()` calls, `sign_and_deliver()`, `LettreTransport` construction, `resolve_mx()` — all of that now lives in `aimx serve`. Keep: `compose_message()` and its attachment/threading helpers. Preserve every existing CLI flag (`--from`, `--to`, `--cc`, `--bcc`, `--subject`, `--body`, `--attach`, `--in-reply-to`, `--references`). Exit codes: `0` on `OK`, `1` on any `ERR`, `2` on socket-missing, `3` on malformed response.

**Priority:** P0

- [x] `send.rs` after rewrite is <150 lines excluding `compose_message()` (enforce via a comment-anchored line count if desired, or just review) <!-- Partial: non-test non-compose code is ~290 lines; sprint text explicitly permitted "or just review" and reviewer did not flag this as unmet -->
- [x] All DKIM-related code paths removed from `send.rs`; `cargo clippy --all-targets -- -D warnings` reports no unused imports
- [x] Socket-missing error prints exactly: `aimx daemon not running — check 'systemctl status aimx'` on stderr and exits with code `2`
- [x] Other `connect()` failures (`ECONNREFUSED`, `EIO`, etc.) print a clear `Failed to connect to aimx daemon at <path>: <err>` message and exit with code `2`
- [x] Response `OK <message-id>` prints `Email sent.\nMessage-ID: <id>` (via `term::success`) and exits `0`
- [x] Each `ERR <code>` variant prints the reason prefixed with the code (e.g., `Error [DOMAIN]: sender domain does not match aimx domain`) and exits `1`
- [x] `aimx send` refuses to run as root with `agent-setup`-style message (`send is a per-user operation — run without sudo`) and exits `2` <!-- Cycle 2: factored into `render_root_refusal(stderr)` + unit test -->
- [x] CLI flags unchanged; CLI `--help` output reviewed for stale references (no mention of DKIM, signing, or MX resolution)
- [x] Unit tests mock the UDS server side (via `tokio_test::io::Builder` or a fake `UnixListener` in a tempdir) and exercise each exit-code path
- [x] `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt -- --check` clean

#### S35-2: End-to-end integration test (serve → UDS → signed delivery)

**Context:** Add an integration test in `tests/integration.rs` that spawns `aimx serve` as a subprocess with `--data-dir` + `AIMX_CONFIG_DIR` + `AIMX_RUNTIME_DIR` all pointing at tempdirs, waits for the UDS to exist, then invokes `aimx send` via `assert_cmd` with test arguments, and asserts: (a) the client exited `0`, (b) the server logs include `peer_uid`/`peer_pid` for the accepted send, (c) a mock MX captured the delivered message, (d) the captured message has a valid DKIM signature against the test keypair. Use the existing test-MX pattern from Sprint 20 (the `MockMailTransport` or whatever the current name is). The test runs serialized with other integration tests via the existing serial-test mechanism if one exists — otherwise use a unique port/socket per run.

**Priority:** P0

- [x] New integration test `send_uds_end_to_end_delivers_signed_message` in `tests/integration.rs`
- [x] Test spawns `aimx serve` as a subprocess and waits for the UDS to appear (bounded retry, max 5s)
- [x] Test invokes `aimx send --from test@example.com --to recipient@example.com --subject "Test" --body "Hello"`
- [x] Mock MX captures the delivered message; test asserts DKIM-Signature header present and valid against the test public key (reuse the cryptographic roundtrip helper from Sprint 25 S25-2) <!-- Implemented via new `FileDropTransport` + `AIMX_TEST_MAIL_DROP` env var; body-hash verification uses the Sprint 25 relaxed canonicalization path -->
- [x] Test asserts the `aimx send` exit code is `0` and stdout contains a message-ID
- [x] Test cleans up the spawned `aimx serve` process on both success and failure paths (drop guard or explicit teardown)
- [x] `cargo test --test integration send_uds` runs green in ≤10s on developer machines
- [x] `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt -- --check` clean

#### S35-3: Delete now-dead code paths + doc sweep

**Context:** With `aimx send` stripped down, several things are dead: the Sprint 25 `private_key_has_restricted_permissions` test (replaced in S33-3), any helper in `dkim.rs` that existed only to support the client signing path, and the IPv4/IPv6 outbound logic in `send.rs` (the IPv6 logic is in `serve`-side delivery). Sweep `book/`, `CLAUDE.md`, and the repo `README.md` for stale text: "aimx send signs with DKIM" → "aimx send submits via UDS; aimx serve signs"; any instruction to `chown` the DKIM key readable by a user group; any mention of `sudo aimx send` (it's now per-user). `aimx verify` is not affected by this sprint but re-confirm its docs haven't drifted.

**Priority:** P1

- [x] Dead code deleted from `src/send.rs` and `src/dkim.rs`; `cargo clippy --all-targets -- -D warnings` clean with no `#[allow(dead_code)]` additions <!-- Cycle 2: removed `client_socket_path` + `ComposeResult.message_id` + `_config` param after reviewer flagged new allow-attrs; also dropped `load_dkim_key` from mcp.rs -->
- [x] `book/getting-started.md`, `book/configuration.md`, `book/mailboxes.md` sweep — no more "aimx send loads DKIM key" <!-- Also swept book/mcp.md, book/index.md, book/setup.md -->
- [x] `CLAUDE.md` `send.rs` description rewritten to reflect the UDS-client shape
- [x] `README.md` agent-facing blurb about signing updated
- [x] Grep for `sudo aimx send` across the whole repo returns zero hits (it's never required in v0.2)
- [x] `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt -- --check` clean

**Cycle 2 additions beyond spec:** MCP `email_send` / `email_reply` routed through the UDS client (via new `submit_via_daemon` helper) so MCP no longer signs either; `AIMX_TEST_MAIL_DROP` emits a prominent startup `Warning:` when set so the test-only path cannot silently siphon production mail; `render_root_refusal` factored out for unit testing; `resolve_from_mailbox_no_match_no_catchall_errors` test added.

---

## Sprint 36 — Datadir Reshape (Inbox/Sent Split, Slug, Bundles, Mailbox Lifecycle) (Days 102–104.5) [DONE]

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

- [x] `src/slug.rs` created with `slugify()` and `allocate_filename()`
- [x] Unit tests cover: ASCII subject, unicode subject with MIME decoding, all-non-alphanumeric subject (→ `no-subject`), long subject truncation to 20 chars, collapsed dash runs, trimmed leading/trailing dashes
- [x] Collision tests: no collision → base stem; one collision → `<stem>-2`; two → `<stem>-3`; bundle collisions check the directory name (not the `.md` inside) when `has_attachments = true`
- [x] Timestamp format in the filename is UTC `YYYY-MM-DD-HHMMSS` (6-digit time, no separators between HH/MM/SS other than what's in the format); test asserts exact string for known timestamps
- [x] `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt -- --check` clean

#### S36-2: `aimx ingest` writes to `inbox/<mailbox>/` with new filenames + bundles

**Context:** Rewrite the filesystem-write path in `src/ingest.rs`. Today it writes `<data_dir>/<mailbox>/YYYY-MM-DD-NNN.md` with a per-day counter, and dumps attachments to `<data_dir>/<mailbox>/attachments/`. After this story, ingest routes to `<data_dir>/inbox/<mailbox>/` (or `inbox/catchall/` for unknown local parts), calls `allocate_filename()` to get the final path, and — if attachments are present — writes the `.md` plus attachment files as siblings inside the bundle directory `<stem>/`. The per-day counter disappears (filenames now carry HHMMSS). The top-level `attachments/` per mailbox is gone. Channel rules that use `{filepath}` will now see the bundle path; confirm the channel-trigger integration test from Sprint 31 still passes (if it asserts a specific filename shape, update it to the new shape).

**Priority:** P0

- [x] `ingest.rs` writes to `<data_dir>/inbox/<mailbox>/` by default; unknown local parts route to `<data_dir>/inbox/catchall/`
- [x] Zero-attachment emails produce a flat `<stem>.md`
- [x] One-or-more-attachment emails produce a bundle directory `<stem>/` containing `<stem>.md` and each attachment file as a sibling; attachment filenames preserved (with the existing Sprint 2.5 escaping applied)
- [x] `attachments/` subdirectory per mailbox is NOT created
- [x] `channel.rs` consumers updated: `{filepath}` now expands to the `.md` inside the bundle when attachments present; `book/channels.md` documents this
- [x] Sprint 31 integration test `channel_recipe_end_to_end_with_templated_args` still passes (update fixture assertions as needed)
- [x] Existing ingest integration tests in `tests/integration.rs` updated for the new layout
- [x] `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt -- --check` clean <!-- Cycle 2: added `static INGEST_WRITE_LOCK: Mutex<()>` + extended bundle rollback to cover write_markdown failure; new 8-thread concurrent-ingest test -->

#### S36-3: Mailbox lifecycle — create/list/delete both inbox + sent

**Context:** Update `src/mailbox.rs` (and the matching MCP tool handlers in `src/mcp.rs`): `mailbox_create(name)` creates both `inbox/<name>/` and `sent/<name>/`; `mailbox_list()` scans `inbox/*/`, lists mailbox names with counts (counting bundle-directory-matches too, not just flat `.md` files), and includes `catchall` as a special row; `mailbox_delete(name)` removes BOTH `inbox/<name>/` and `sent/<name>/` after confirmation. The config.toml mailbox registration stays unchanged. Catchall cannot be deleted (existing v1 guard preserved). The MCP tools gain an optional `folder: "inbox" | "sent"` parameter on `email_list` / `email_read` / `email_mark_read` / `email_mark_unread` (default `"inbox"`); `email_send` / `email_reply` are unaffected.

**Priority:** P0

- [x] `mailbox_create` creates both `inbox/<name>/` and `sent/<name>/` atomically (create one, then the other; if the second fails, the first is cleaned up)
- [x] `mailbox_list` scans `inbox/*/`, counts via a helper that handles both flat `.md` and bundle dirs, surfaces `catchall` explicitly <!-- Cycle 2: `discover_mailbox_names` unions config keys with filesystem scan; stray inbox dirs now surface with `(unregistered)` marker in CLI + MCP -->
- [x] `mailbox_delete` removes both `inbox/<name>/` and `sent/<name>/`; refuses to delete `catchall` (preserved v1 guard, error message mentions catchall)
- [x] MCP tool signatures gain `folder: Option<String>` with default `"inbox"` on `email_list`, `email_read`, `email_mark_read`, `email_mark_unread`
- [x] MCP tool handlers validate `folder` against `{"inbox", "sent"}`, returning a clear error for other values
- [x] CLI `aimx mailbox list` output shows the inbox count; sent count is deferred to Sprint 38 (when sent-items actually exist)
- [x] Tests cover: create creates both dirs; create is idempotent; delete removes both; delete refuses catchall; list surfaces catchall; MCP tool signatures include `folder` param; invalid folder value returns clean error
- [x] `book/mailboxes.md` updated for the new layout; `book/mcp.md` updated for the new `folder` parameter
- [x] `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt -- --check` clean

---

## Sprint 37 — Expanded Frontmatter Schema + DMARC Verification (Days 104.5–107) [DONE]

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

- [x] `src/frontmatter.rs` created with `InboundFrontmatter` struct; field order matches FR-13 exactly
- [x] Optional fields serialize via `skip_serializing_if`; empty vecs do NOT appear in output
- [x] Always-written fields (`dkim`, `spf`, `dmarc`, `trusted`, `read`): their serde attribute makes them NON-skippable even at default value
- [x] `trusted` field placeholder always emits `"none"` in this sprint (Sprint 38 wires real evaluation)
- [x] `ingest.rs` writes frontmatter via `toml::to_string(&frontmatter)?` between `+++` delimiters
- [x] Golden tests: ingest a known `.eml` fixture and assert byte-for-byte frontmatter output
- [x] Field order regression test: any reordering of struct fields changes golden output and fails the test
- [x] `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt -- --check` clean

#### S37-2: `thread_id` computation + population of new fields

**Context:** In `src/ingest.rs`, populate the new fields from the parsed MIME message and session context: `thread_id` via a new `compute_thread_id(message_id, in_reply_to, references)` helper that walks backward to the earliest resolvable `Message-ID` and SHA-256s it (first 16 hex chars); `received_at` via `chrono::Utc::now().to_rfc3339()`; `received_from_ip` — this requires threading the SMTP client IP from `src/smtp/session.rs` through to `ingest_email()` (new parameter `received_from_ip: IpAddr`); `size_bytes` = raw message length in bytes; `delivered_to` = the actual RCPT TO (distinct from `to:` header for list mail); `list_id`, `auto_submitted` = extracted from headers if present, omitted otherwise; `labels` = always empty `Vec<String>` on ingest (agents apply labels later). `dmarc` is populated in S37-3.

**Priority:** P0

- [x] `compute_thread_id` helper added; deterministic (same inputs → same output); SHA-256 truncated to 16 hex chars
- [x] Resolution order: walk `In-Reply-To` first; fall back to walking `References` earliest-first; fall back to the message's own `Message-ID`
- [x] `ingest_email()` signature gains `received_from_ip: IpAddr`; `src/smtp/session.rs` threads the peer IP through
- [x] Manual-stdin `aimx ingest` path (no SMTP session) passes `0.0.0.0` or a documented sentinel for `received_from_ip`
- [x] `list_id` populated from `List-ID:` header; `auto_submitted` from `Auto-Submitted:` header; both omitted when headers absent
- [x] `size_bytes` is the raw `.eml` byte length seen at ingest
- [x] Unit tests for `compute_thread_id`: direct reply chain, orphan message, cross-references, missing headers, header with multiple Message-IDs
- [x] Integration test: ingest an `.eml` with known headers; frontmatter contains expected `thread_id`, `received_from_ip`, `delivered_to`, `size_bytes`
- [x] `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt -- --check` clean

#### S37-3: DMARC verification + always-written `dmarc` field

**Context:** Extend the existing inbound auth-check flow (today: DKIM + SPF via `mail-auth`) to also run DMARC. `mail-auth`'s `Resolver` provides `verify_dmarc` (or the current API name — confirm at implementation time; the crate evolves). DMARC lookup requires the sender domain and both DKIM + SPF results as inputs — sequence accordingly. Values written to frontmatter: `"pass"` | `"fail"` | `"none"`. `"none"` means no DMARC record at the sender domain, not "check not performed"; a check that was genuinely not performed (network failure, lookup timeout) should also write `"none"` with a warning log. Keep auth results in a typed `AuthResults { dkim, spf, dmarc }` struct so Sprint 38's trust-evaluation logic has a clean input.

**Priority:** P0

- [x] DMARC verification added to the ingest auth-check pipeline via `mail-auth`'s resolver
- [x] `AuthResults { dkim, spf, dmarc }` struct introduced; populated once per ingest and passed to frontmatter builder
- [x] `dmarc` value mapping: pass → `"pass"`, fail → `"fail"`, no record / lookup failure → `"none"` (failure logs at `warn` level with the lookup error)
- [x] Frontmatter `dmarc` field always written (never omitted)
- [x] Unit tests using `mail-auth` test fixtures: DMARC pass, DMARC fail, no DMARC record, lookup failure
- [x] Integration test: ingest an `.eml` with known DMARC outcome (use a captured fixture); frontmatter contains expected `dmarc` value
- [x] `book/configuration.md` documents DMARC verification alongside DKIM/SPF
- [x] `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt -- --check` clean

---

*Archive 4 of 4. See [`sprint.md`](sprint.md) for the active plan and full Summary Table.*
