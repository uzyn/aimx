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

- `setup.rs` — `aimx setup [domain]`: interactive setup wizard. Prompts for domain when omitted, generates TLS cert + DKIM keys, installs systemd/OpenRC service file for `aimx serve`, displays colorized [DNS]/[MCP]/[Deliverability] sections, DNS retry loop, re-entrant detection. Requires root.
- `ingest.rs` — `aimx ingest`: reads raw `.eml` from stdin (called by `aimx serve` in-process, or via stdin for manual use), parses MIME, writes Markdown with TOML frontmatter (`+++` delimiters), extracts attachments, fires channel triggers.
- `send.rs` — `aimx send`: composes RFC 5322 message, DKIM-signs it, delivers via direct SMTP to recipient's MX.
- `mx.rs` — MX resolution: resolves recipient domain to MX hostnames via `hickory-resolver`, falls back to A record per RFC 5321.
- `mcp.rs` — `aimx mcp`: MCP server over stdio using `rmcp` crate. 9 tools for mailbox/email operations.
- `channel.rs` — channel manager: match filters + shell command triggers on ingest.
- `serve.rs` — `aimx serve`: starts the embedded SMTP daemon. Loads config, initializes TLS, runs the SMTP listener via tokio. Options: `--bind`, `--tls-cert`, `--tls-key`. Handles SIGTERM/SIGINT for graceful shutdown.
- `smtp/` — embedded SMTP server module: `mod.rs` (listener accept loop), `session.rs` (per-connection SMTP state machine: EHLO, MAIL FROM, RCPT TO, DATA, STARTTLS, QUIT, RSET, NOOP), `tls.rs` (STARTTLS upgrade via tokio-rustls), `tests.rs` (unit tests).
- `verify.rs` — `aimx verify`: checks port 25 connectivity via the verifier service.
- `setup.rs` also contains `run_setup` which drives the full interactive setup flow, and `display_deliverability_section` for optional PTR/deliverability checks.

### Trait-based testing pattern

`setup.rs` defines `SystemOps` and `NetworkOps` traits with real and mock implementations. Tests use `MockSystemOps`/`MockNetworkOps` to simulate OS and network operations without requiring root or network access. The `send.rs` module uses `MailTransport` trait similarly. This pattern is used throughout — extend it when adding system-dependent functionality.

### Config and storage

- Config: `/var/lib/aimx/config.toml` — parsed via `serde` + `toml` crate into `Config` struct in `config.rs`.
- Storage: Markdown files with TOML frontmatter (`+++` delimiters, not `---`). One `.md` file per email, `YYYY-MM-DD-NNN.md` naming.
- `--data-dir` / `AIMX_DATA_DIR` overrides the default path globally.
- Tests use `tempfile::TempDir` for isolated data directories.

### Email frontmatter format

TOML frontmatter between `+++` delimiters. Fields: `id`, `message_id`, `from`, `to`, `subject`, `date`, `in_reply_to`, `references`, `attachments`, `mailbox`, `read`, `dkim`, `spf`.

### MCP server

Uses `rmcp` crate with `#[tool]` attribute macros on `AimxMcpServer` methods. Stdio transport (launched on-demand by MCP client, no long-running process). Each tool method loads config and operates on the filesystem directly.

### Verifier service

Axum HTTP server with `/probe` (EHLO handshake) and `/health` endpoints. Runs a concurrent SMTP listener on port 25. Uses `X-AIMX-Client-IP` header from Caddy for caller identification. Deployed via `docker-compose.yml` with `network_mode: host`.

## Key conventions

- Error handling: `Result<(), Box<dyn std::error::Error>>` for all public `run()` functions. Propagate with `?`.
- `aimx serve` is the SMTP daemon (long-running process managed by systemd/OpenRC). All other commands are short-lived.
- `aimx setup` requires root.
- Integration tests (`tests/integration.rs`) use `assert_cmd` to test the binary as a subprocess with `--data-dir` pointing at temp directories.
- Test fixtures in `tests/fixtures/` (`.eml` files for ingest testing).

## Documentation

User-facing guide lives in `book/` (index, getting-started, setup, configuration, mailboxes, channels, mcp, troubleshooting). When making changes that affect CLI behavior, setup output, or MCP tools, update the corresponding guide files too.

- `docs/prd.md` — product requirements document
- `docs/sprint.md` — sprint plan (do not modify `[DONE]` or `[IN PROGRESS]` sprints)
