# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What is aimx

Self-hosted email for AI agents. One binary, one setup command. Built-in SMTP server handles inbound; direct SMTP delivery for outbound. AIMX handles everything: ingest to Markdown, DKIM signing, MCP server, channel triggers. `aimx serve` is the SMTP daemon; all other commands are short-lived processes.

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

## Architecture

### Two independent Rust crates

1. **`aimx`** (root `Cargo.toml`) — the main CLI binary. Edition 2024.
2. **`aimx-verifier`** (`services/verifier/`) — hosted verification service (axum HTTP + SMTP listener). Deployed separately with Docker. Edition 2021.

These are NOT a Cargo workspace — they have independent `Cargo.toml` files and `target/` directories.

### Main binary: subcommand dispatch

`main.rs` parses CLI via clap and dispatches to module-level `run()` functions. Each `src/*.rs` module owns one subcommand:

- `setup.rs` — `aimx setup [domain]`: interactive setup wizard. Prompts for domain when omitted, generates TLS cert + DKIM keys, installs systemd/OpenRC service file for `aimx serve`, displays colorized [DNS]/[MCP]/[Deliverability] sections, DNS retry loop, re-entrant detection. Writes the datadir README on completion. Requires root.
- `ingest.rs` — `aimx ingest`: reads raw `.eml` from stdin (called by `aimx serve` in-process, or via stdin for manual use), parses MIME via `mail-parser`, writes Markdown with TOML frontmatter (`+++` delimiters) using `InboundFrontmatter` (section-ordered: Identity → Parties → Content → Threading → Auth → Storage), routes to `inbox/<mailbox>/`, extracts attachments into Zola-style bundles, fires channel triggers.
- `send.rs` — `aimx send`: thin UDS client. Composes an unsigned RFC 5322 message and submits it to `aimx serve` over `/run/aimx/send.sock` as one `AIMX/1 SEND` request frame. The client does NOT read `config.toml` or the DKIM key — signing, mailbox resolution, and MX delivery all live in `aimx serve`. Refuses to run as root. Exit codes: `0` OK, `1` daemon ERR, `2` socket-missing / connect failure / root, `3` malformed response.
- `send_protocol.rs` — `AIMX/1` wire protocol codec (shared by SEND, MARK-READ, and MARK-UNREAD verbs). Length-prefixed, binary-safe framing (`AIMX/1 <VERB>\n`, per-verb headers, `Content-Length:` header, body). Parses requests into a tagged `Request` enum and writes `SendResponse` / `AckResponse` frames. Pure async I/O — no filesystem or network. Sprint 45 removed the `From-Mailbox:` SEND header; the daemon now parses `From:` from the body itself and resolves the sender mailbox.
- `send_handler.rs` — daemon-side handler for `AIMX/1 SEND` UDS requests. Parses `From:` from the submitted body, validates sender domain equals `config.domain`, resolves the From local part to a concrete (non-wildcard) configured mailbox, DKIM-signs via shared `Arc<DkimKey>`, delivers via `MailTransport` trait, persists sent copy to `sent/<mailbox>/` with `OutboundFrontmatter`. The catchall (`*@domain`) is inbound-only and never accepted as an outbound sender.
- `state_handler.rs` — daemon-side handler for the `AIMX/1 MARK-READ` / `MARK-UNREAD` verbs. Rewrites the target email's TOML frontmatter in place under a per-mailbox `RwLock` so MCP write ops (`email_mark_read`, `email_mark_unread`) work without the non-root MCP process needing write access to root-owned mailbox files.
- `transport.rs` — daemon-side `MailTransport` trait, `LettreTransport` (real MX delivery), `FileDropTransport` (test-only, triggered by `AIMX_TEST_MAIL_DROP`).
- `mx.rs` — MX resolution: resolves recipient domain to MX hostnames via `hickory-resolver`, falls back to A record per RFC 5321.
- `mcp.rs` — `aimx mcp`: MCP server over stdio using `rmcp` crate. 9 tools for mailbox/email operations. `folder` parameter on read/list tools selects `"inbox"` (default) or `"sent"`.
- `channel.rs` — channel manager: match filters + shell command triggers on ingest. Fires on inbound only.
- `serve.rs` — `aimx serve`: starts the embedded SMTP daemon. Loads config, refreshes the datadir README if outdated, initializes TLS, loads the DKIM key into `Arc`, runs the SMTP listener and UDS send listener via tokio. Binds `/run/aimx/send.sock` (mode `0o666`, world-writable). Options: `--bind`, `--tls-cert`, `--tls-key`. Handles SIGTERM/SIGINT for graceful shutdown.
- `slug.rs` — slug algorithm and filename allocation (Sprint 36 / FR-13b). `slugify()` transforms subjects into filesystem-safe stems; `allocate_filename()` resolves collisions and decides flat vs. bundle layout.
- `frontmatter.rs` — `InboundFrontmatter` and `OutboundFrontmatter` structs with section-ordered serialization. `compute_thread_id()` derives the 16-hex-char thread root hash. `format_outbound_frontmatter()` for sent copies.
- `datadir_readme.rs` — baked-in `/var/lib/aimx/README.md` template with version-gated refresh. `write()` (unconditional, called by setup), `refresh_if_outdated()` (called by serve startup).
- `smtp/` — embedded SMTP server module: `mod.rs` (listener accept loop), `session.rs` (per-connection SMTP state machine: EHLO, MAIL FROM, RCPT TO, DATA, STARTTLS, QUIT, RSET, NOOP), `tls.rs` (STARTTLS upgrade via tokio-rustls), `tests.rs` (unit tests).
- `verify.rs` — `aimx verify`: checks port 25 connectivity via the verifier service.
- `setup.rs` also contains `run_setup` which drives the full interactive setup flow, and `display_deliverability_section` which prints optional Gmail-whitelist instructions. Reverse DNS (PTR) is the operator's responsibility and is out of scope for aimx.

### Trait-based testing pattern

`setup.rs` defines `SystemOps` and `NetworkOps` traits with real and mock implementations. Tests use `MockSystemOps`/`MockNetworkOps` to simulate OS and network operations without requiring root or network access. `transport.rs` defines the `MailTransport` trait consumed by `send_handler.rs`; production uses `LettreTransport` and tests inject a mock. This pattern is used throughout — extend it when adding system-dependent functionality.

### Config and storage

- Config: `/etc/aimx/config.toml` (mode `0640`, owner `root:root`) — parsed via `serde` + `toml` into `Config` struct in `config.rs`. Resolved via `config_path()` which honors `AIMX_CONFIG_DIR` for tests / non-standard installs.
- DKIM keys: `/etc/aimx/dkim/{private,public}.key` (private `0600` root-only, public `0644`). Loaded via `load_private_key(&config::dkim_dir())`.
- Storage: Markdown files with TOML frontmatter (`+++` delimiters, not `---`). Inbound mail lives at `/var/lib/aimx/inbox/<mailbox>/`, outbound sent copies at `/var/lib/aimx/sent/<mailbox>/`. Filenames use `YYYY-MM-DD-HHMMSS-<slug>.md` (UTC). Zero attachments produce a flat `<stem>.md`; one or more attachments produce a Zola-style bundle directory `<stem>/` containing `<stem>.md` plus each attachment as a sibling. The previous mailbox-level `attachments/` directory is gone (Sprint 36).
- `--data-dir` / `AIMX_DATA_DIR` overrides the **storage** path (mailboxes only, v0.2). `AIMX_CONFIG_DIR` overrides the **config + DKIM** path.
- Runtime dir: `/run/aimx/` (mode `0755`, owner `root:root`) — provided by systemd `RuntimeDirectory=aimx` or OpenRC `checkpath` in `start_pre`. Sprint 34 places `send.sock` here as a world-writable UDS (`0o666`); authorization is out of scope for v0.2.
- Tests use `tempfile::TempDir` plus `crate::config::test_env::ConfigDirOverride::set(&tmp)` (or the `AIMX_CONFIG_DIR` env var in integration tests) to isolate config + DKIM lookups.

### Email frontmatter format

TOML frontmatter between `+++` delimiters. Inbound fields follow section ordering: Identity (`id`, `message_id`, `thread_id`) → Parties (`from`, `to`, `cc`, `reply_to`, `delivered_to`) → Content (`subject`, `date`, `received_at`, `received_from_ip`, `size_bytes`, `attachments`) → Threading (`in_reply_to`, `references`, `list_id`, `auto_submitted`) → Auth (`dkim`, `spf`, `dmarc`, `trusted`) → Storage (`mailbox`, `read`, `labels`). Optional fields are omitted when empty rather than written as `null`. Auth and storage fields are always written. Outbound (sent) copies add an Outbound block: `outbound`, `delivery_status`, `bcc`, `delivered_at`, `delivery_details`.

### MCP server

Uses `rmcp` crate with `#[tool]` attribute macros on `AimxMcpServer` methods. Stdio transport (launched on-demand by MCP client, no long-running process). Each tool method loads config and operates on the filesystem directly.

### Verifier service

Axum HTTP server with `/probe` (EHLO handshake) and `/health` endpoints. Runs a concurrent SMTP listener on port 25. Uses `X-AIMX-Client-IP` header from Caddy for caller identification. Deployed via `docker-compose.yml` with `network_mode: host`.

## Key conventions

- Error handling: `Result<(), Box<dyn std::error::Error>>` for all public `run()` functions. Propagate with `?`.
- `aimx serve` is the SMTP daemon (long-running process managed by systemd/OpenRC). All other commands are short-lived.
- `aimx setup` requires root. `aimx send` refuses root (it is a thin UDS client that does not need privilege).
- Integration tests (`tests/integration.rs`) use `assert_cmd` to test the binary as a subprocess with `--data-dir` pointing at temp directories.
- Test fixtures in `tests/fixtures/` (`.eml` files for ingest testing).
- Agent-facing plugins live under `agents/<agent>/` and are embedded at compile time. The canonical primer is `agents/common/aimx-primer.md` with reference docs in `agents/common/references/`. The datadir README template is `src/datadir_readme.md.tpl`.

## Documentation

User-facing guide lives in `book/` (index, getting-started, setup, configuration, mailboxes, channels, channel-recipes, mcp, agent-integration, troubleshooting). When making changes that affect CLI behavior, setup output, or MCP tools, update the corresponding guide files too.

- `docs/prd.md` — product requirements document
- `docs/sprint.md` — sprint plan (do not modify `[DONE]` or `[IN PROGRESS]` sprints)
- `agents/common/aimx-primer.md` — canonical agent-facing primer (bundled into all agent plugins)
- `agents/common/references/` — detailed reference docs (frontmatter schema, MCP tools, workflows, troubleshooting)
