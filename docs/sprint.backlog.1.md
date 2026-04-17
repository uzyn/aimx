# AIMX — Backlog Archive 1

> **Completed backlog items** | Archived from [`sprint.md`](sprint.md) | Archive 1 of 1

---

## Improvements

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
- [x] **(Sprint 34)** Add a per-read idle timeout inside `send_protocol::parse_request` to bound slow-loris exposure on the world-writable UDS socket. — _Triaged into Sprint 41 (S41-3)._
- [x] **(Sprint 34)** Replace the substring-based `send_handler::classify_transport_error` with a typed error surface on `MailTransport`. — _Triaged into Sprint 41 (S41-4)._
- [x] **(Sprint 34)** Cache the test DKIM keypair across integration tests via `once_cell` + a process-scoped `TempDir`. — _Triaged into Sprint 41 (S41-6)._

---

*Archive 1 of 1. See [`sprint.md`](sprint.md) for the active backlog (open items + recent completed items).*
