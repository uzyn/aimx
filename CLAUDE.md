# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What is aimx

Self-hosted email for AI agents. One binary, one setup command. Built-in SMTP server handles inbound. Direct SMTP delivery for outbound. aimx handles everything: ingest to Markdown, DKIM signing, MCP server, hooks (`on_receive` / `after_send`). `aimx serve` is the SMTP daemon. All other commands are short-lived processes.

## Build and test commands

```bash
# Build
cargo build
cargo build --release

# Install locally
cargo build --release && sudo cp target/release/aimx /usr/local/bin/

# Tests (all unit + integration)
cargo test

# Single test
cargo test test_name
cargo test -- --exact module::tests::test_name

# Lint
cargo clippy -- -D warnings
cargo fmt -- --check

# Format
cargo fmt
```

### Verifier service (separate Rust crate)

```bash
cd services/verifier
cargo build
cargo test
cargo clippy -- -D warnings
cargo fmt -- --check
```

CI runs both crates independently (`.github/workflows/ci.yml`).

### Test environment escape hatches

A handful of CLI gating points refuse to run as non-root in production. The
test suite injects an opt-in to keep the post-gate code paths exercised under
a non-root `cargo test` runner. Set this only from the test harness:

- **`AIMX_TEST_SKIP_AUTHZ_CHECK=1`** — bypasses the `authorize()` call
  (gating `Action::HookCrud`) in `aimx hooks --cmd` raw-shell paths so
  the rest of the command (config writes, fallback hints, error
  formatting) stays reachable without `sudo`. Read by `src/hooks.rs`
  only — mailbox CRUD authz is enforced server-side over UDS and never
  consults this env var. Production callers must never set this — it
  neutralizes the authz predicate by design.

Other test-only env vars are documented next to their read sites:
`AIMX_SANDBOX_FORCE_FALLBACK` (force the non-systemd-run hook executor),
`AIMX_CONFIG_DIR` / `AIMX_DATA_DIR` (redirect config + storage paths),
`AIMX_TEST_MAIL_DROP` (use the file-drop transport instead of MX delivery),
`AIMX_INTEGRATION_SUDO=1` (opt the integration suite into the root-only
MAILBOX-CRUD branch when the test runner has sudo).

## Architecture

### Two independent Rust crates

1. **`aimx`** (root `Cargo.toml`): the main CLI binary. Edition 2024.
2. **`aimx-verifier`** (`services/verifier/`): hosted verification service (axum HTTP + SMTP listener). Deployed separately with Docker. Edition 2021.

These are NOT a Cargo workspace. They have independent `Cargo.toml` files and `target/` directories.

### Main binary: subcommand dispatch

`main.rs` parses CLI via clap and dispatches to module-level `run()` functions. Each `src/*.rs` module owns one subcommand:

- `setup.rs`: `aimx setup [domain]`. Interactive setup wizard. Prompts for domain when omitted, generates TLS cert + DKIM keys, creates the `aimx-catchall` system user (sole reserved-uid for catchall mailbox storage), chowns each configured mailbox's `inbox/<name>/` and `sent/<name>/` to `<owner>:<owner> 0700`, installs systemd/OpenRC service file for `aimx serve` (runs as root; subprocesses drop to mailbox owner via `setuid` at fire time), displays colorized [DNS]/[MCP]/[Deliverability]/[User] sections, DNS retry loop, re-entrant detection. Writes the datadir README on completion. Requires root.
- `ingest.rs`: `aimx ingest`. Reads raw `.eml` from stdin (called by `aimx serve` in-process, or via stdin for manual use), parses MIME via `mail-parser`, writes Markdown with TOML frontmatter (`+++` delimiters) using `InboundFrontmatter` (section-ordered: Identity → Parties → Content → Threading → Auth → Storage), routes to `inbox/<mailbox>/`, extracts attachments into Zola-style bundles, fires `on_receive` hooks.
- `send.rs`: `aimx send`. Thin UDS client. Composes an unsigned RFC 5322 message and submits it to `aimx serve` over `/run/aimx/aimx.sock` as one `AIMX/1 SEND` request frame. The client does NOT read `config.toml` or the DKIM key; signing, mailbox resolution, and MX delivery all live in `aimx serve`. Refuses to run as root. Exit codes: `0` OK, `1` daemon ERR, `2` socket-missing / connect failure / root, `3` malformed response.
- `send_protocol.rs`: `AIMX/1` wire protocol codec (shared by `SEND`, `MARK-READ`, `MARK-UNREAD`, `MAILBOX-CREATE`, `MAILBOX-DELETE`, `HOOK-CREATE`, and `HOOK-DELETE` verbs). Length-prefixed, binary-safe framing (`AIMX/1 <VERB>\n`, per-verb headers, `Content-Length:` header, body). Parses requests into a tagged `Request` enum and writes `SendResponse` / `AckResponse` frames. Pure async I/O; no filesystem or network. The daemon parses `From:` from the submitted body itself to resolve the sender mailbox (no client-supplied `From-Mailbox:` header). `HOOK-DELETE` carries a `Hook-Name:` header; the daemon resolves it against the effective name (explicit or derived).
- `send_handler.rs`: daemon-side handler for `AIMX/1 SEND` UDS requests. Parses `From:` from the submitted body, validates sender domain equals `config.domain`, resolves the From local part to a concrete (non-wildcard) configured mailbox, DKIM-signs via shared `Arc<DkimKey>`, delivers via `MailTransport` trait, persists sent copy to `sent/<mailbox>/` with `OutboundFrontmatter`. The catchall (`*@domain`) is inbound-only and never accepted as an outbound sender.
- `state_handler.rs`: daemon-side handler for the `AIMX/1 MARK-READ` / `MARK-UNREAD` verbs. Rewrites the target email's TOML frontmatter in place under a per-mailbox `RwLock` so MCP write ops (`email_mark_read`, `email_mark_unread`) work without the non-root MCP process needing write access to root-owned mailbox files.
- `transport.rs`: daemon-side `MailTransport` trait, `LettreTransport` (real MX delivery), `FileDropTransport` (test-only, triggered by `AIMX_TEST_MAIL_DROP`).
- `mx.rs`: MX resolution. Resolves recipient domain to MX hostnames via `hickory-resolver`, falls back to A record per RFC 5321.
- `mcp.rs`: `aimx mcp`. MCP server over stdio using `rmcp` crate. 12 tools for mailbox/email/hook operations (`mailbox_list`, `mailbox_create`, `mailbox_delete`, `email_list`, `email_read`, `email_send`, `email_reply`, `email_mark_read`, `email_mark_unread`, `hook_create`, `hook_list`, `hook_delete`). `folder` parameter on read/list tools selects `"inbox"` (default) or `"sent"`.
- `hook.rs`: hook manager. Event dispatch for `on_receive` (fired by `ingest.rs`) and `after_send` (fired by `send_handler.rs`). Owns the trust gate (`should_fire_on_receive`), the effective-name resolver (`effective_hook_name` / `derive_hook_name`), and the structured `info` log line emitted per fire. Every hook is a raw argv (`cmd`) stored under `[[mailboxes.<name>.hooks]]`; hooks carry an optional `name` and an `origin` tag (`operator` default, `mcp` when created via UDS `HOOK-CREATE`). When `name` is omitted the effective name is derived deterministically from `sha256(event + joined_argv + fire_on_untrusted)`, and is globally unique across mailboxes (config load rejects collisions).
- `serve.rs`: `aimx serve`. Starts the embedded SMTP daemon. Loads config, refreshes the datadir README if outdated, initializes TLS, loads the DKIM key into `Arc`, runs the SMTP listener and UDS send listener via tokio. Binds `/run/aimx/aimx.sock` (mode `0o666`, world-writable). Options: `--bind`, `--tls-cert`, `--tls-key`. Handles SIGTERM/SIGINT for graceful shutdown.
- `slug.rs`: slug algorithm and filename allocation. `slugify()` transforms subjects into filesystem-safe stems; `allocate_filename()` resolves collisions and decides flat vs. bundle layout.
- `frontmatter.rs`: `InboundFrontmatter` and `OutboundFrontmatter` structs with section-ordered serialization. `compute_thread_id()` derives the 16-hex-char thread root hash. `format_outbound_frontmatter()` for sent copies.
- `datadir_readme.rs`: baked-in `/var/lib/aimx/README.md` template with version-gated refresh. `write()` (unconditional, called by setup), `refresh_if_outdated()` (called by serve startup).
- `smtp/`: embedded SMTP server module. `mod.rs` (listener accept loop), `session.rs` (per-connection SMTP state machine: EHLO, MAIL FROM, RCPT TO, DATA, STARTTLS, QUIT, RSET, NOOP), `tls.rs` (STARTTLS upgrade via tokio-rustls), `tests.rs` (unit tests).
- `portcheck.rs`: `aimx portcheck`. Checks port 25 connectivity via the verifier service.
- `cli.rs`: `clap` CLI surface. The `Cli` parser and `Command` enum enumerating every subcommand and its flags. `main.rs` matches on `Command` to dispatch.
- `config.rs`: `Config` struct + `ConfigHandle` (`RwLock<Arc<Config>>`) used by the daemon for live hot-swap of mailbox config. `config_path()` and `dkim_dir()` resolve paths and honor `AIMX_CONFIG_DIR` for tests. `validate_hooks` runs at `Config::load` (rejects duplicate hook names, `cmd[0]` not absolute, `fire_on_untrusted` on `after_send`, and the legacy `template` / `params` / `run_as` / `origin` / `dangerously_support_untrusted` / `stdin` fields with a pointer to `book/hooks.md`).
- `mailbox.rs`: `aimx mailboxes create | list | delete` (with `mailbox` retained as a clap alias). Client-side logic. Both root and non-root callers prefer the daemon UDS path (`MAILBOX-CREATE` / `MAILBOX-DELETE`) so inbound routing picks up the change without a restart. When the daemon is stopped: root falls back to a direct `config.toml` edit + restart hint; non-root callers cannot fall back (`/etc/aimx/config.toml` is `0640 root:root`) so the CLI exits with the precise *"daemon must be running for non-root mailbox CRUD; start `aimx serve` or run with sudo to fall back to direct config edit"* error. `delete --force` is daemon-side: the wipe of `inbox/<name>/` and `sent/<name>/` runs under per-mailbox lock + `CONFIG_WRITE_LOCK`, atomic with the config rewrite. `--owner` on `create` is honored only by root; non-root callers passing `--owner <other>` get a soft warning and the daemon synthesizes the correct owner.
- `mailbox_handler.rs`: daemon-side handler for `AIMX/1 MAILBOX-CREATE` / `MAILBOX-DELETE`. Owner-bound, not root-gated: per-verb authz via `auth::authorize` with `Action::MailboxCreate { owner_uid }` / `Action::MailboxDelete { mailbox }`. For non-root `MAILBOX-CREATE` the handler synthesizes the owner from the caller's `SO_PEERCRED` uid via `peer_username()` and ignores any client-supplied `Owner:` header (privilege-escalation defense — there is no path for a non-root caller to cause a mailbox to be created with an owner other than their own uid). Root continues to honor `Owner:` so cross-uid creates remain operator-only. Performs atomic temp-then-rename disk writes on `config.toml` and swaps the in-memory `Config` via `ConfigHandle::store` so both views stay consistent.
- `mailbox_locks.rs`: per-mailbox lock map (`MailboxLocks`) shared by ingest, `MARK-*`, and `MAILBOX-CRUD`. Enforces a strict lock hierarchy: outer per-mailbox lock → inner process-wide `CONFIG_WRITE_LOCK`.
- `doctor.rs`: `aimx doctor`. Gathers config path, mailbox counts, unread counts, per-mailbox trust + hooks summary, DKIM key presence, SMTP service state (systemd/OpenRC), recent activity, and the last 10 service log lines.
- `logs.rs`: `aimx logs [--lines N] [--follow]`. Thin wrapper around `journalctl -u aimx` (systemd) or `/var/log/aimx/*.log` / `/var/log/messages` (OpenRC fallback). Uses `SystemOps::tail_service_logs` / `follow_service_logs` so tests can inject a mock instead of spawning journalctl.
- `dkim.rs`: `aimx dkim-keygen`. 2048-bit RSA keypair generation with `0600` private / `0644` public file modes. Supports `--selector` and `--force`.
- `agents_setup.rs`: `aimx agents setup [agent]`. Embeds plugin/skill bundles at compile time via `include_dir!` and installs them into the destination for each supported agent (Claude Code, Codex CLI, OpenCode, Gemini CLI, Goose, OpenClaw). Supports `--list`, `--force`, and `--print`.
- `trust.rs`: effective trust evaluation. `TrustedValue::{None, True, False}` serializes to lowercase strings for the frontmatter `trusted` field. `evaluate_trust()` takes `&Config` and `&MailboxConfig`; it resolves the effective `trust` / `trusted_senders` via `MailboxConfig::effective_trust(&Config)` / `effective_trusted_senders(&Config)`: per-mailbox override if set (`Option::Some`), otherwise the top-level `Config` defaults. Per-mailbox `trusted_senders` fully **replaces** the global list (no merging).
- `term.rs`: semantic color helpers for CLI output (success / error / warn / info / header / highlight / dim / badges). Auto-respects `NO_COLOR` and non-TTY stdout.
- `platform.rs`: tiny OS helper. `is_root()` via `libc::geteuid()` on Unix.
- `uninstall.rs`: `aimx uninstall`. Stops the daemon and removes the service file; preserves `/etc/aimx/` and `/var/lib/aimx/`. Requires root.
- `hooks.rs`: `aimx hooks list | create | delete` CLI (with `hook` clap alias). Owner-gated, not root-gated: the central `auth::authorize` predicate (gating `Action::HookCrud`) runs as the same pre-flight whether the caller used CLI or MCP. The CLI prefers the daemon UDS path (`HOOK-CREATE` / `HOOK-DELETE`) so `Arc<Config>` hot-swaps live without a restart. When the daemon is down, root falls back to a direct `config.toml` edit + `SIGHUP` hint; non-root hard-errors with the canonical *"daemon must be running"* message because it cannot write the root-owned config. `--cmd` takes the argv as a JSON array. Delete resolves against effective names; operator-origin hooks can only be removed via CLI, MCP-origin hooks can also be removed over UDS. The UDS helpers `submit_hook_create_via_daemon` / `submit_hook_delete_via_daemon` live in `src/mcp.rs` and are shared by both the CLI and the MCP server.
- `hook_handler.rs`: daemon-side handler for `HOOK-CREATE` and `HOOK-DELETE`. `HOOK-CREATE` carries the raw argv (`cmd`) plus optional `name` / `fire_on_untrusted` / `timeout_secs`, runs `auth::authorize` on the caller's `SO_PEERCRED` uid, and stamps `origin = "mcp"` on the persisted hook. `HOOK-DELETE` resolves the hook by effective name, runs the same predicate, and is origin-protected (operator-origin hooks are CLI-only). Same write-temp-then-rename + `ConfigHandle::store` pattern as `mailbox_handler`.
- `hook_list_handler.rs`: daemon-side handler for the `AIMX/1 HOOK-LIST` verb. Returns a JSON array `Vec<HookListRow>` filtered to hooks on mailboxes the caller's uid owns; root sees every hook on every mailbox. Mirrors `MAILBOX-LIST`'s frame shape.
- `setup.rs` also contains `run_setup` which drives the full interactive setup flow, and `display_deliverability_section` which prints optional Gmail-whitelist instructions. Reverse DNS (PTR) is the operator's responsibility and is out of scope for aimx.

### Trait-based testing pattern

`setup.rs` defines `SystemOps` and `NetworkOps` traits with real and mock implementations. Tests use `MockSystemOps`/`MockNetworkOps` to simulate OS and network operations without requiring root or network access. `transport.rs` defines the `MailTransport` trait consumed by `send_handler.rs`; production uses `LettreTransport` and tests inject a mock. This pattern is used throughout; extend it when adding system-dependent functionality.

### Config and storage

- Config: `/etc/aimx/config.toml` (mode `0640`, owner `root:root`), parsed via `serde` + `toml` into `Config` struct in `config.rs`. Resolved via `config_path()` which honors `AIMX_CONFIG_DIR` for tests / non-standard installs.
- DKIM keys: `/etc/aimx/dkim/{private,public}.key` (private `0600` root-only, public `0644`). Loaded via `load_private_key(&config::dkim_dir())`.
- Storage: Markdown files with TOML frontmatter (`+++` delimiters, not `---`). Inbound mail lives at `/var/lib/aimx/inbox/<mailbox>/`, outbound sent copies at `/var/lib/aimx/sent/<mailbox>/`. Filenames use `YYYY-MM-DD-HHMMSS-<slug>.md` (UTC). Zero attachments produce a flat `<stem>.md`; one or more attachments produce a Zola-style bundle directory `<stem>/` containing `<stem>.md` plus each attachment as a sibling. There is no mailbox-level `attachments/` subdirectory; attachments are siblings of the `.md` inside the bundle.
- `--data-dir` / `AIMX_DATA_DIR` overrides the **storage** path (mailboxes only). `AIMX_CONFIG_DIR` overrides the **config + DKIM** path.
- Runtime dir: `/run/aimx/` (mode `0755`, owner `root:root`), provided by systemd `RuntimeDirectory=aimx` or OpenRC `checkpath` in `start_pre`. `aimx.sock` sits here as a world-writable UDS (`0o666`), but every verb runs server-side `auth::authorize` keyed on the caller's `SO_PEERCRED` uid (kernel-validated, never client-supplied) — there is no path for a non-root caller to act on a mailbox they don't own. The DKIM key remains `0600 root:root` and is consumed only by the root-running daemon.
- Tests use `tempfile::TempDir` plus `crate::config::test_env::ConfigDirOverride::set(&tmp)` (or the `AIMX_CONFIG_DIR` env var in integration tests) to isolate config + DKIM lookups.

### Email frontmatter format

TOML frontmatter between `+++` delimiters. Inbound fields follow section ordering: Identity (`id`, `message_id`, `thread_id`) → Parties (`from`, `to`, `cc`, `reply_to`, `delivered_to`) → Content (`subject`, `date`, `received_at`, `received_from_ip`, `size_bytes`, `attachments`) → Threading (`in_reply_to`, `references`, `list_id`, `auto_submitted`) → Auth (`dkim`, `spf`, `dmarc`, `trusted`) → Storage (`mailbox`, `read`, `labels`). Optional fields are omitted when empty rather than written as `null`. Auth and storage fields are always written. Outbound (sent) copies add an Outbound block: `outbound`, `delivery_status`, `bcc`, `delivered_at`, `delivery_details`.

### MCP server

Uses `rmcp` crate with `#[tool]` attribute macros on `AimxMcpServer` methods. Stdio transport (launched on-demand by MCP client, no long-running process). Every tool routes through the daemon UDS (`MAILBOX-LIST`, `HOOK-LIST`, and the per-verb handlers) — the MCP process never reads `/etc/aimx/config.toml` directly, so the tools work for non-root operators on a `0600 root:root` install. `AimxMcpServer::load_config` exists only behind `#[cfg(test)]`; the release build has no production caller.

### Verifier service

Axum HTTP server with `/probe` (EHLO handshake) and `/health` endpoints. Runs a concurrent SMTP listener on port 25. Uses `X-AIMX-Client-IP` header from Caddy for caller identification. Deployed via `docker-compose.yml` with `network_mode: host`.

## Key conventions

- Error handling: `Result<(), Box<dyn std::error::Error>>` for all public `run()` functions. Propagate with `?`.
- `aimx serve` is the SMTP daemon (long-running process managed by systemd/OpenRC). All other commands are short-lived.
- `aimx setup` requires root. `aimx send` refuses root (it is a thin UDS client that does not need privilege).
- Integration tests (`tests/integration.rs`) use `assert_cmd` to test the binary as a subprocess with `--data-dir` pointing at temp directories.
- Test fixtures in `tests/fixtures/` (`.eml` files for ingest testing).
- Agent-facing plugins live under `agents/<agent>/` and are embedded at compile time. The canonical primer is `agents/common/aimx-primer.md` with reference docs in `agents/common/references/`. The datadir README template is `src/datadir_readme.md.tpl`.
- CLI output styling is centralised in `src/term.rs`. Raw `.red()` / `.green()` / `.yellow()` / `.blue()` / `.bold()` calls outside `term.rs` are banned — route through `term::success` / `error` / `warn` / `info` / `header` / `highlight` / `dim` / `accent`. Status marks use `term::success_mark()` / `fail_mark()` / `warn_mark()` / `prompt_mark()` (Unicode on TTY, `[OK]`/`[FAIL]`/`[WARN]`/`[>]` on non-TTY). The `cli-colors` CI job enforces both rules.

## Documentation

User-facing guide lives in `book/` (index, getting-started, setup, configuration, mailboxes, hooks, hook-recipes, mcp, agent-integration, cli, faq, troubleshooting). When making changes that affect CLI behavior, setup output, or MCP tools, update the corresponding guide files too.

- `agents/common/aimx-primer.md`: canonical agent-facing primer (bundled into all agent plugins)
- `agents/common/references/`: detailed reference docs (frontmatter schema, MCP tools, workflows, troubleshooting)
