# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Fixed

- `aimx hooks` CLI now routes through the daemon UDS for non-root callers, so
  mailbox owners can manage their hooks without sudo ([#254]).
- `hickory-resolver` 0.26 API compatibility: adapted to the removed
  `LookupControlFlow::Skip` variant.

### Dependencies

- Bump sparkv 0.1 → 0.2.0 ([#257]).
- Bump hickory-resolver 0.25 → 0.26 ([#235]).
- Bump mail-auth 0.9 → 0.10 ([#261]).
- Bump lol_html 2.9 → 3.0 ([#253]).

## [0.2.0] — 2026-05-14

**First public release.** Blog post:
[Introducing AIMX](https://uzyn.com/2026/introducing-aimx/).

### Added

- SMTP daemon (`aimx serve`) with embedded STARTTLS, DKIM signing, and
  per-mailbox ownership enforced via `SO_PEERCRED`.
- Markdown email storage with TOML frontmatter — LLM- and RAG-friendly
  without a parser.
- MCP stdio server (`aimx mcp`) with 12 tools for mailbox/email/hook
  operations. Launched on-demand, no long-running process.
- Inbound email pipeline: `aimx serve` receives SMTP → `aimx ingest`
  parses MIME → writes Markdown to `inbox/<mailbox>/` → fires
  `on_receive` hooks.
- Outbound email pipeline: `aimx send` (UDS client) → daemon DKIM-signs
  → MX delivery → persisted to `sent/<mailbox>/` → fires `after_send`
  hooks.
- Interactive setup wizard (`aimx setup`) with guided DNS, STARTTLS
  certificate generation, DKIM key generation, systemd/OpenRC service
  installation, and MCP agent wiring.
- `aimx doctor` for server health, DNS verification, and recent logs.
- `aimx agents setup` for one-line MCP integration into Claude Code,
  Codex CLI, OpenCode, Gemini CLI, Goose, OpenClaw, Hermes, and
  NanoClaw.
- `aimx upgrade` for non-interactive binary replacement.
- `aimx portcheck` for port 25 connectivity verification.
- `aimx dkim-keygen` for DKIM keypair generation.
- Hook system with `on_receive` and `after_send` events, trust gating,
  and per-mailbox configuration.
- Built-in trust modeling with DKIM-based evaluation and
  per-mailbox/per-sender `trusted_senders` overrides.
- Attachment handling: flat `.md` for zero attachments, bundle
  directories with sibling files for one or more attachments.
- Multi-architecture releases: `x86_64-linux-{gnu,musl}` and
  `aarch64-linux-{gnu,musl}`.
- POSIX-compatible `install.sh` with platform auto-detection,
  upgrade path, and recovery from aborted setup.
- Comprehensive user guide (`book/`) with getting-started, setup,
  configuration, mailboxes, hooks, MCP, and agent integration docs.

[0.2.0]: https://github.com/uzyn/aimx/releases/tag/0.2.0
[#254]: https://github.com/uzyn/aimx/pull/254
[#257]: https://github.com/uzyn/aimx/pull/257
[#235]: https://github.com/uzyn/aimx/pull/235
[#261]: https://github.com/uzyn/aimx/pull/261
[#253]: https://github.com/uzyn/aimx/pull/253
