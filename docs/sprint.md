# AIMX — Sprint Plan

**Sprint cadence:** 2.5 days per sprint
**Team:** Solo developer with heavy AI augmentation (Claude Code)
**Total sprints:** 44 (6 original + 2 post-audit hardening + 1 YAML→TOML migration + 2 verifier/setup overhaul + 2 post-Sprint-11 bug fixes + 2 verifier ops + 1 deployment + 1 service rename + 1 setup UX + 5 embedded SMTP + 1 verify cleanup + 1 DKIM permissions fix + 1 IPv6 support + 1 systemd unit hardening + 1 CLI color consistency + 1 CI binary releases + 3 agent integration + 1 channel-trigger cookbook + 1 non-blocking cleanup + 1 scope-reversal (33.1) + 8 v0.2 pre-launch reshape + 1 post-v0.2 backlog cleanup + 1 CLI UX fixes)
**Timeline:** ~120 calendar days (v1: ~92 days, v0.2 reshape: ~22.5 days, post-v0.2 cleanup: ~2.5 days, CLI UX fixes: ~2.5 days)
**v1 Scope:** Full PRD scope including verifier service. Sprint 1 targets earliest possible idea validation on a real VPS. Sprints 7–8 address findings from post-v1 code review audit. Sprints 10–11 overhaul the verifier service (remove email echo, add EHLO probe) and rewrite the setup flow (root check, MTA conflict detection, install-before-check). Sprints 12–13 fix critical bugs found during post-Sprint-11 debugging: Caddy self-probe loop / XFF SSRF risk in the verifier service, and the preflight chicken-and-egg problem on fresh VPSes. Sprints 14–15 are review-driven operational quality work on the verifier service (request logging, Docker packaging). Sprint 17 renames the verify service to verifier across all code, Docker, CI, and documentation. Sprints 19–23 replace OpenSMTPD with an embedded SMTP server (hand-rolled tokio listener for inbound, lettre + hickory-resolver for outbound) and update all documentation, making aimx a true single-binary solution with no external runtime dependencies and cross-platform Unix support. Sprint 24 cleans up `aimx verify` (EHLO-only checks, sudo requirement, remove `/reach` endpoint, AIMX capitalization). Sprint 27 hardens the generated systemd unit with restart rate-limiting, resource limits, and network-readiness dependencies. Sprint 27.5 unifies user-facing CLI output under a single semantic color palette. (Sprint 27.6 — CI binary release workflow — is deferred to the Non-blocking Review Backlog until we're production-ready.) Sprints 28–30 ship per-agent integration packages (Claude Code, Codex CLI, OpenCode, Gemini CLI, Goose, OpenClaw) plus the `aimx agent-setup <agent>` installer that drops a plugin/skill/recipe into the agent's standard location without mutating its primary config. Sprint 31 adds a channel-trigger cookbook covering email→agent invocation patterns for every supported agent. Sprint 32 is a non-blocking cleanup sprint consolidating review feedback across v1.

**v0.2 Scope (pre-launch reshape, Sprints 33–40 + 33.1 scope-reversal):** Five tightly-coupled themes that reshape AIMX into its launch form. Sprint 33 splits the filesystem (config + DKIM secrets to `/etc/aimx/`, data stays at `/var/lib/aimx/` but world-readable). Sprint 33.1 (scope reversal, inserted after Sprint 33 merged) drops PTR/reverse-DNS handling (operator responsibility, out of aimx scope) and drops the `aimx` system group introduced in S33-4 — authorization on the UDS send socket is explicitly out of scope for v0.2 and the socket becomes world-writable (`0o666`). Sprints 34–35 shrink the trust boundary: DKIM signing and outbound delivery move inside `aimx serve`, exposed to clients over a world-writable Unix domain socket at `/run/aimx/send.sock`; the DKIM private key becomes root-only (`600`) and is never read by non-root processes. Sprint 36 reshapes the datadir (`inbox/` vs `sent/` split per mailbox, `YYYY-MM-DD-HHMMSS-<slug>.md` filenames with a deterministic slug algorithm, Zola-style attachment bundles). Sprint 37 expands the inbound frontmatter schema (new fields: `thread_id`, `received_at`, `received_from_ip`, `size_bytes`, `delivered_to`, `list_id`, `auto_submitted`, `dmarc`, `labels`) and adds DMARC verification. Sprint 38 surfaces the per-mailbox trust evaluation as a new always-written `trusted` frontmatter field (the v1 per-mailbox trust model — `trust: none|verified` + `trusted_senders` — is preserved unchanged; `trusted` is the *result*, not a new *policy*) and persists sent mail with a full outbound block. Sprint 39 restructures the shared agent primer into a progressive-disclosure skill bundle (`agents/common/aimx-primer.md` + `references/`), standardizes author metadata to `U-Zyn Chua <chua@uzyn.com>`, and reverses an earlier draft's storage-layout redaction policy. Sprint 40 ships the baked-in `/var/lib/aimx/README.md` agent-facing layout guide (versioned via `include_str!`, refreshed on `aimx serve` startup when the version differs), replaces stale `/var/log/aimx.log` references with `journalctl -u aimx`, and brings every affected `book/` chapter and `CLAUDE.md` up to date. No migration tooling is written — v0.2 ships pre-launch, with no existing installs to upgrade.

---


## Sprint Archive

Completed sprints 1–37 have been archived for context window efficiency.

| Archive | Sprints | File |
|---------|---------|------|
| 1 | 1–8 | [`sprint.1.md`](sprint.1.md) |
| 2 | 9–21 | [`sprint.2.md`](sprint.2.md) |
| 3 | 22–30 | [`sprint.3.md`](sprint.3.md) |
| 4 | 31–37 | [`sprint.4.md`](sprint.4.md) |

---

## Sprint 38 — `trusted` Frontmatter + Sent-Items Persistence + Outbound Block (Days 107–109.5) [DONE]

**Goal:** Evaluate per-mailbox trust at ingest time and surface the result in the always-written `trusted` frontmatter field. Persist every successfully delivered outbound message to `sent/<from-mailbox>/` with the full outbound frontmatter block.

**Dependencies:** Sprint 37 (frontmatter struct, `AuthResults`), Sprint 34 (daemon-side send handler owns delivery).

**Design notes:**
- `trusted` computation (FR-37b) is explicit:
    - mailbox's `trust` config is `none` (default) → `trusted = "none"` (no evaluation performed)
    - mailbox's `trust` config is `verified`, sender matches `trusted_senders`, AND DKIM passed → `trusted = "true"`
    - mailbox's `trust` config is `verified`, any other outcome → `trusted = "false"`
  The v1 channel-trigger gating behavior is preserved verbatim; `trusted` is the surfaced result of that same evaluation, not a new policy.
- Sent-items persistence runs inside `aimx serve`'s send handler (Sprint 34's `handle_send`), after successful delivery returns. The signed message is what gets persisted (not the unsigned client submission).
- Filename allocation across concurrent sends requires a `Mutex<()>` around "check directory + create file" to avoid two sends racing to the same stem. The critical section is microseconds; the Mutex holds across at most one `fs::metadata` + one `fs::File::create` call.

#### S38-1: `trusted` frontmatter field — compute + always-write

**Context:** Add `fn evaluate_trust(mailbox: &MailboxConfig, auth: &AuthResults, from: &str) -> TrustedValue` in `src/ingest.rs` (or a new `src/trust.rs` module). `TrustedValue` is `enum { None, True, False }` serializing to `"none"`, `"true"`, `"false"`. Logic: `mailbox.trust == Trust::None` → `TrustedValue::None`; `mailbox.trust == Trust::Verified` AND `from` matches any glob in `mailbox.trusted_senders` AND `auth.dkim == DkimResult::Pass` → `TrustedValue::True`; `mailbox.trust == Trust::Verified` otherwise → `TrustedValue::False`. Note: the v1 `trusted_senders` behavior was "allowlisted senders skip verification" — that is a gate on trigger firing, preserved unchanged. The `trusted` field follows the PRD's FR-37b reading, which is stricter: `trusted == "true"` requires BOTH allowlisted AND DKIM-pass. The channel-trigger gate itself stays at v1 semantics (allowlisted OR DKIM-pass for `trust: verified`); it does NOT switch to reading the `trusted` field. This keeps the trigger gate's existing "allowlisted senders skip verification" affordance intact.

**Priority:** P0

- [x] `TrustedValue` enum added with three variants, serializing to `"none"`, `"true"`, `"false"` lowercase
- [x] `evaluate_trust()` implements the three-value logic exactly as specified
- [x] Ingest pipeline calls `evaluate_trust()` and writes the result into the `trusted` frontmatter field
- [x] Channel-trigger gate logic remains at v1 semantics — `src/channel.rs` is NOT modified to read `trusted`; it continues to evaluate allowlist + DKIM independently. Inline comment in `channel.rs` points at `evaluate_trust()` for the rationale.
- [x] Unit tests cover every arm of `evaluate_trust()`: `trust: none` → `"none"`; `trust: verified` + allowlisted + DKIM pass → `"true"`; allowlisted + DKIM fail → `"false"`; not allowlisted + DKIM pass → `"false"`; not allowlisted + DKIM fail → `"false"`
- [x] Parity test: for a `trust: verified` mailbox, `trusted == "true"` IFF the channel-trigger gate would fire, confirming the two pieces of logic agree
- [x] `book/configuration.md` explains the `trusted` field semantics; `book/mcp.md` mentions it in the frontmatter reference
- [x] `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt -- --check` clean

#### S38-2: Outbound frontmatter block (`outbound`, `bcc`, `delivered_at`, `delivery_status`, `delivery_details`)

**Context:** Define `OutboundFrontmatter` (or extend `InboundFrontmatter` with an optional outbound block — prefer a distinct struct so the required/optional split stays clean). Outbound-only fields per FR-19c: `outbound: bool` (always `true` on sent files), `bcc: Option<Vec<String>>` (only meaningful on sent copies), `delivered_at: Option<String>` (RFC 3339 UTC when remote MX accepted), `delivery_status: DeliveryStatus` (`"delivered"` | `"deferred"` | `"failed"` | `"pending"`, always written), `delivery_details: Option<String>` (last remote SMTP response). The outbound file is structurally identical to inbound (same inbound fields at top, outbound block at bottom) so a single reader can parse both by type-tagging on the presence of `outbound = true`.

**Priority:** P0

- [x] `DeliveryStatus` enum serializes to the four lowercase strings
- [x] `OutboundFrontmatter` struct composes `InboundFrontmatter` (identity/parties/content/threading/auth/storage) + the outbound block (outbound/bcc/delivered_at/delivery_status/delivery_details)
- [x] Field ordering in the serialized output: inbound block first, outbound block at the end
- [x] `delivery_status` is ALWAYS written (never omitted); other outbound fields follow omission rules
- [x] Golden test: outbound fixture serializes byte-for-byte to the expected layout
- [x] `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt -- --check` clean

#### S38-3: Sent-items persistence in `aimx serve` send handler

**Context:** Extend Sprint 34's `handle_send` to, after a successful delivery, write the signed message to `<data_dir>/sent/<from_mailbox>/<stem>.md` (or a bundle directory `<stem>/` when the outbound message has attachments). Filename uses the same algorithm as inbound (Sprint 36) with the send's UTC timestamp and the message's `Subject:`-derived slug. The write path: construct `OutboundFrontmatter` from the signed message + delivery result, compose the `.md` body (frontmatter + signed RFC 5322 message as the body), allocate the filename under a process-global `Mutex<()>` guarding "check directory + create file", then release the Mutex and do the actual file write outside the lock. On partial delivery failure (e.g., message was sent to some MX recipients but failed on others), write `delivery_status: "failed"` with the relevant detail — still persist the file so the operator has a record. On transient errors that the daemon retries internally (none today — v1 doesn't queue), skip persistence and return `TEMP` to the client.

**Priority:** P0

- [x] Successful sends write `<data_dir>/sent/<from_mailbox>/<stem>.md` (or bundle directory) with `delivery_status: "delivered"` and `delivered_at` populated
- [x] Signed RFC 5322 bytes are the body of the `.md` file (below the `+++` frontmatter block); the exact DKIM-signed message delivered to the MX is what's persisted
- [x] `Mutex<()>` guards the filename allocation critical section only; the file-write IO happens outside the lock
- [x] Failed sends (permanent `DELIVERY` error): write the file with `delivery_status: "failed"` and `delivery_details` carrying the last remote SMTP response
- [x] `TEMP` errors: do NOT persist (the client will see the transient error and retry itself); inline comment documents why
- [x] `mailbox_list` CLI output now reports sent counts alongside inbox counts
- [x] Integration test: drive an end-to-end send via UDS; assert the sent file exists at the expected path, has the right frontmatter, and contains a DKIM signature that verifies against the public key
- [x] Integration test for permanent-failure persistence: inject a mock MX that always rejects; assert the `.md` is written with `delivery_status: "failed"`
- [x] `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt -- --check` clean

---

## Sprint 39 — Agent Primer as Progressive-Disclosure Skill Bundle + Author Metadata (Days 109.5–112) [DONE]

**Goal:** Restructure the shared agent primer from a single file into a main body + `references/` layout (the anthropics/skills progressive-disclosure pattern), standardize author metadata across every agent package, and reverse the earlier storage-exposure redaction policy so the primer documents the datadir layout explicitly.

**Dependencies:** Sprint 38 (frontmatter schema finalized — the primer documents it, so the primer can't ship before the schema is stable).

**Design notes:**
- Progressive disclosure is a file-layout convention, not a runtime behavior. Claude Code and Codex agents read the main `SKILL.md` by default and load `references/*.md` on demand when the main file points at them. Agents that take a single blob (Goose recipes, Gemini prompts) get only the main primer bundled — the install-time concat step decides per-platform.
- The `install-time concat` extension in `agent_setup.rs` gains a `<platform-suffix>.footer` input (optional, new) and a `references/` copy step. Per-agent registry entries declare whether they support progressive disclosure.
- Storage-exposure reversal (FR-50c): the primer documents the datadir layout plainly. There's no security boundary to protect by concealing it — the datadir is world-readable by design, and the real boundary (DKIM key, UDS socket) is enforced elsewhere.

#### S39-1: Split `agents/common/aimx-primer.md` into main + `references/`

**Context:** Today `agents/common/aimx-primer.md` is one file. Split it: the new main `aimx-primer.md` targets 300–500 lines and covers identity/purpose, the two access surfaces (MCP for writes + direct FS reads), quick-reference summaries of the 9 MCP tools with their signatures, the frontmatter fields agents most often check (`trusted`, `thread_id`, `list_id`, `auto_submitted`, `read`, `labels`), the 4–5 most common workflows inline (check inbox, send, reply, summarize a thread, handle auto-submitted mail), a short trust-model overview (per-mailbox `trust` + `trusted_senders` + the `trusted` frontmatter surface), pointers to `references/*.md`, a pointer to the runtime `/var/lib/aimx/README.md`, and a "what you must not do" safety list. Deep material moves into `agents/common/references/{mcp-tools,frontmatter,workflows,troubleshooting}.md`. `mcp-tools.md` carries full signatures, parameter types, and at least one worked example per tool; `frontmatter.md` carries every field with type + required/optional + notes + the outbound block; `workflows.md` carries 8–12 worked tasks (triage inbox, thread summarization, react to auto-submitted mail, handle attachments, reply-all, filter by list-id, ingest a bounce, mark all-read, etc.); `troubleshooting.md` carries the UDS-protocol error codes, common misconfigurations, and recovery steps.

**Priority:** P0

- [x] `agents/common/aimx-primer.md` rewritten — 300–500 lines (soft cap; enforce via a line-count comment or PR review)
- [x] `agents/common/references/mcp-tools.md`, `frontmatter.md`, `workflows.md`, `troubleshooting.md` created with the content described above
- [x] Main primer explicitly links `references/` files and the runtime `/var/lib/aimx/README.md`
- [x] Main primer documents the storage layout plainly (FR-50c reversal); inline comment cites FR-50c
- [x] `trusted` field documented in the frontmatter quick-reference AND in `references/frontmatter.md` with the three values and the per-mailbox evaluation logic
- [x] All references to removed/renamed v1 paths purged (grep for `/var/lib/aimx/<mailbox>/` outside of `inbox/`/`sent/` context)
- [x] Byte-level test that asserts the main primer's line count stays within the target range (prevents future bloat)
- [x] `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt -- --check` clean

#### S39-2: `agent_setup.rs` install-time concat — `<suffix>.footer` + `references/` copy

**Context:** Extend `src/agent_setup.rs` to support two new concat inputs. First, an optional `<platform>.footer` file appended after the common primer body at install time (the existing `<platform>.header` is prepended — this sprint adds the symmetric suffix). Second, a per-agent registry flag `progressive_disclosure: bool`; when `true`, installer copies `agents/common/references/*.md` to `<destination>/references/*.md` verbatim; when `false`, installer skips the copy but may optionally inline selected reference snippets into the main primer at install time (Goose recipes are the motivating case, where context budget is tighter — for v0.2 we skip inlining and just ship the main primer; inlining is a future enhancement). Per-agent registry update:
    - Claude Code, Codex, OpenClaw → `progressive_disclosure: true`
    - Goose, Gemini, OpenCode → `progressive_disclosure: false`

**Priority:** P0

- [x] `AgentSpec` registry struct gains `progressive_disclosure: bool` (and `suffix_filename: Option<&str>` if `.footer` is used)
- [x] Install flow: header + common primer + optional footer → SKILL.md; references copied only when `progressive_disclosure: true`
- [x] Per-agent `progressive_disclosure` assignments made per the design note above
- [x] Byte-level test for a progressive-disclosure agent: install lays down `SKILL.md` + `references/` tree, contents match fixtures
- [x] Byte-level test for a non-progressive-disclosure agent: install lays down single-blob output with references absent
- [x] `--print` mode emits both `SKILL.md` and `references/` files for progressive-disclosure agents; only `SKILL.md` for others
- [x] `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt -- --check` clean

#### S39-3: Author metadata standardization + repo-wide grep verification

**Context:** Every shipped agent package carries an author field. Today several carry `"AIMX"` or a placeholder from an earlier draft. Standardize to `U-Zyn Chua <chua@uzyn.com>` across: `agents/claude-code/.claude-plugin/plugin.json`, `agents/codex/.codex-plugin/plugin.json`, `agents/goose/aimx.yaml.header` (if the Goose recipe schema carries an author field — confirm at implementation time; if not, skip), `agents/opencode/SKILL.md.header`, `agents/gemini/SKILL.md.header`, `agents/openclaw/SKILL.md.header`. A CI-runnable repo-wide grep asserts no `"AIMX"` author strings or placeholder emails remain under `agents/`.

**Priority:** P1

- [x] All six agent packages carry `U-Zyn Chua <chua@uzyn.com>` in their author field
- [x] Goose: if the recipe schema supports `author`, it's populated; if not, inline comment in `agents/goose/aimx.yaml.header` notes the gap
- [x] `.github/workflows/ci.yml` gains a grep step that fails if `"AIMX"` or placeholder author strings appear under `agents/` (pattern: literal `"author": "AIMX"` and `<chua@example.com>` style placeholders)
- [x] Existing agent integration tests pass; install-layout assertions updated if they captured the author string
- [x] `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt -- --check` clean

---

## Sprint 40 — Datadir `README.md` + Journald Docs + Book/ Pass (Days 112–114.5) [DONE]

**Goal:** Ship the baked-in `/var/lib/aimx/README.md` agent-facing layout guide (versioned, auto-refreshed on daemon startup), replace stale `/var/log/aimx.log` references with `journalctl` commands, and bring every affected `book/` chapter and `CLAUDE.md` up to date with the v0.2 reshape. Last sprint before launch.

**Dependencies:** Sprint 39 (primer structure finalized — the datadir README references the same schema).

**Design notes:**
- Datadir README carries `<!-- aimx-readme-version: N -->` as the first line. Comparison is exact string match, not semver. Bump `N` by 1 whenever the template changes. Refresh triggers: `aimx setup` (always writes), `aimx serve` startup (writes only if the version line differs from the on-disk version line).
- Book/ pass is a docs-only sprint from AIMX's perspective but touches many files. The intent is "no stale `/var/lib/aimx/<mailbox>/`-style references anywhere user-facing." Grep the whole `book/` tree and `README.md` after edits.
- `CLAUDE.md` updates: new module descriptions for `src/send_protocol.rs`, `src/send_handler.rs`, `src/slug.rs`, `src/frontmatter.rs`, `src/datadir_readme.rs`; updated descriptions for `send.rs` (now thin UDS client) and `serve.rs` (now owns DKIM signing + send handler); updated storage conventions section.

#### S40-1: `src/datadir_readme.rs` — template, write, version-gate refresh

**Context:** New module `src/datadir_readme.rs` with: `pub const TEMPLATE: &str = include_str!("datadir_readme.md.tpl");`, `pub const VERSION: u32 = 1;`, `pub fn write(data_dir: &Path)` (writes unconditionally), `pub fn refresh_if_outdated(data_dir: &Path)` (reads existing file, parses the first-line version comment, writes only if differs from `VERSION`). The template file `src/datadir_readme.md.tpl` begins with `<!-- aimx-readme-version: 1 -->` and carries: what the directory is, read vs write access model, directory layout with the v0.2 tree, file naming rules, slug algorithm, bundle rule, frontmatter reference (link to the full spec in `agents/common/references/frontmatter.md` plus an inlined quick-reference), trust/DKIM/SPF/DMARC explanation, thread grouping, handling auto-submitted/list mail, attachments, the UDS send protocol summary, and a pointer to the `aimx` MCP server for all mutations. Top of the file states: "This file is regenerated on AIMX upgrade. User edits will be overwritten."

**Priority:** P0

- [x] `src/datadir_readme.rs` and `src/datadir_readme.md.tpl` created
- [x] Version bump procedure documented in a `// VERSION BUMP:` comment at the top of `datadir_readme.rs`
- [x] `write()` writes the template verbatim to `<data_dir>/README.md` with mode `0o644`
- [x] `refresh_if_outdated()` parses the first line; if the version comment is missing, malformed, or differs from `VERSION`, overwrite; otherwise no-op
- [x] `aimx setup` calls `write()` at the end of setup
- [x] `aimx serve` startup calls `refresh_if_outdated()` before binding listeners
- [x] Unit tests: `write` creates the file; `refresh` no-op when version matches; `refresh` overwrites when version differs; `refresh` overwrites when first line is missing or malformed
- [x] Integration test: run `aimx serve` in a tempdir with a stale README; assert it's refreshed at startup
- [x] `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt -- --check` clean

#### S40-2: Journald documentation + `/var/log/aimx.log` purge

**Context:** `book/troubleshooting.md` still has stale `tail -f /var/log/aimx.log` examples from before `aimx serve` landed — `aimx` has always logged to journald/OpenRC-logger, not to that path. Replace with `journalctl -u aimx -f`, `journalctl -u aimx --since today`, `journalctl -u aimx -n 200`. Add a "Where are the logs?" subsection explaining: systemd → journald; OpenRC → whatever the OpenRC init script configures (document the actual path written by `src/serve.rs`'s init script). `book/channel-recipes.md` has user-authored `/var/log/aimx/<agent>.log` paths in trigger examples — these are legitimate destinations the user chooses for their own trigger scripts, not aimx's own logs; add a header note clarifying this.

**Priority:** P1

- [x] `book/troubleshooting.md`: every `/var/log/aimx.log` occurrence replaced with `journalctl -u aimx` commands
- [x] `book/troubleshooting.md`: new "Where are the logs?" subsection covering systemd + OpenRC
- [x] `book/channel-recipes.md`: header note distinguishing user-chosen trigger-log paths from aimx's own logs
- [x] Grep confirms `/var/log/aimx.log` appears nowhere under `book/`, `docs/`, or `README.md`
- [x] `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt -- --check` clean

#### S40-3: Book/ v0.2 pass + `CLAUDE.md` rewrite

**Context:** Sweep every `book/` chapter for v0.2 changes:
    - `book/getting-started.md`: world-readable datadir security note (single-admin assumption), new directory paths, `aimx` group membership requirement for `aimx send`
    - `book/configuration.md`: config now at `/etc/aimx/config.toml`; document DKIM key at `/etc/aimx/dkim/`; `[trust]` section is unchanged from v1 (still per-mailbox); point at `book/channels.md` for the trust model
    - `book/setup.md`: `aimx` group creation step; `usermod -aG aimx <user>` instruction; re-run setup still re-entrant
    - `book/mailboxes.md`: new `inbox/` + `sent/` layout; bundle rule; naming convention with UTC timestamp + slug; `mailbox list` now shows both inbox and sent counts
    - `book/mcp.md`: new `folder: "inbox" | "sent"` parameter on read/list tools; updated frontmatter field reference; pointer to `agents/common/aimx-primer.md` and the datadir README
    - `book/channels.md`: channel rules fire on inbound only; trust gate semantics unchanged from v1
    - `book/agent-integration.md`: primer-as-skill-bundle note for per-platform pages; `references/` copy behavior for Claude Code + Codex + OpenClaw; single-blob behavior for Goose + Gemini + OpenCode

`CLAUDE.md` updates: new module descriptions (`send_protocol`, `send_handler`, `slug`, `frontmatter`, `datadir_readme`); updated descriptions for `send.rs` (thin UDS client), `serve.rs` (binds UDS, owns signing), `ingest.rs` (new frontmatter schema, inbox/ routing, bundles); updated Key Conventions section.

**Priority:** P0

- [x] Every `book/*.md` chapter listed above is updated with the v0.2 details
- [x] `CLAUDE.md` module descriptions regenerated — old ones removed, new ones added in the right order
- [x] Repo-wide grep for `/var/lib/aimx/<mailbox>/` (without `inbox/` or `sent/` prefix) returns zero hits in `book/`, `docs/`, `README.md`
- [x] Repo-wide grep for `aimx send` under `book/` never mentions `sudo aimx send` <!-- `aimx` group requirement N/A — group dropped in Sprint 33.1; UDS socket is world-writable (0o666) -->
- [x] `book/agent-integration.md` table of supported agents extended with a "progressive disclosure" column
- [x] Spot-check every `agents/<agent>/README.md` for drift against the new primer layout; update as needed
- [x] `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt -- --check` clean

---

## Sprint 41 — Post-v0.2 Backlog Cleanup (Days 115–117.5) [DONE]

**Goal:** Close out the entire non-blocking review backlog accumulated during Sprints 34–40. Fixes outbound frontmatter bugs, consolidates redundant SPF verification, adds UDS slow-loris protection, types the transport error surface, caches test DKIM keys, and sweeps stale `#[allow(dead_code)]` annotations.

**Dependencies:** Sprint 40 (all v0.2 sprints complete)

### S41-1 — Outbound frontmatter fixes

**Context:** Three issues in `src/send_handler.rs` `persist_sent_file()` and `src/frontmatter.rs` `OutboundFrontmatter`:
(a) `received_at` is `String::new()` (line 403) but serializes as `received_at = ""` instead of being omitted — outbound messages have no `received_at`, so the field should use `skip_serializing_if = "String::is_empty"`.
(b) `date` (line 390) uses a fresh `Utc::now().to_rfc3339()` instead of parsing the `Date:` header from the composed RFC 5322 message — the two diverge slightly, and the `Date:` header is the canonical value. The `Date` header is already scanned in `scan_headers` (line 88-98); thread it through to `persist_sent_file`.
(c) Parity test docstring in `src/trust.rs:243` says "IFF" but the test only checks one direction (trusted==true → trigger fires). Either add the reverse-direction check (trigger fires → trusted==true) or narrow the docstring to "implies."

**Priority:** P0

- [x] `OutboundFrontmatter.received_at`: add `#[serde(skip_serializing_if = "String::is_empty")]` — outbound `.md` files no longer emit `received_at = ""`
- [x] `persist_sent_file`: accept a `date: &str` parameter; caller (`handle_send_inner`) passes the scanned `Date:` header value (already available via `scan_headers`). Fall back to `Utc::now().to_rfc3339()` only if `Date:` is missing
- [x] `trust.rs` parity test: add reverse-direction cases — when `should_execute_triggers` returns true, verify `trusted` is `"true"` for the same inputs. OR: narrow the docstring from "IFF" to "implies" if the reverse doesn't hold for all cases (the docstring already notes `trusted == "true"` is strictly stronger). Decide based on what the code says
- [x] Existing golden tests and frontmatter roundtrip tests still pass
- [x] `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt -- --check` clean

### S41-2 — SPF verification deduplication

**Context:** `src/ingest.rs:284-285` calls both `verify_spf_async()` and `build_spf_output()` — each independently calls `resolver.verify_spf()` with the same parameters, resulting in a redundant DNS lookup on every ingest. The DKIM path was already consolidated in Sprint 37 (single `verify_dkim` call, output reused for both string and DMARC input). Apply the same pattern to SPF: call `build_spf_output()` once, derive the string result from the `SpfOutput`.

**Priority:** P1

- [x] Remove `verify_spf_async()` entirely
- [x] Derive the SPF string result (`"pass"`, `"fail"`, `"softfail"`, `"neutral"`, `"none"`) from the `SpfOutput` returned by `build_spf_output()` — add a helper `spf_output_to_string(&SpfOutput) -> String`
- [x] `verify_auth()` calls `build_spf_output()` once; uses the `SpfOutput` for DMARC and the derived string for the `spf` frontmatter field
- [x] All existing auth-related tests still pass; no behavior change
- [x] `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt -- --check` clean

### S41-3 — UDS slow-loris timeout

**Context:** `send_protocol::parse_request_with_limit()` has no per-read timeout. Any local process can connect to the world-writable UDS socket, announce `Content-Length: 26214400`, and stall — parking a 25 MB `Vec<u8>` + tokio task indefinitely. A 30-second `tokio::time::timeout` around the entire `parse_request_with_limit` call in `serve.rs`'s UDS accept loop is cheaper than a full concurrency semaphore and closes the primary abuse vector.

**Priority:** P0

- [x] Wrap the `parse_request` call in `serve.rs`'s UDS handler with `tokio::time::timeout(Duration::from_secs(30), ...)`. On timeout, log a warning and drop the connection (no response needed — the client is the slow party)
- [x] Add a unit test: connect to UDS, send the request line + headers, then stall — verify the handler drops the connection within ~30s (use a short timeout override for testing, e.g. 1s)
- [x] Existing send integration tests still pass (normal sends complete well under 30s)
- [x] `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt -- --check` clean

### S41-4 — Typed transport errors

**Context:** `send_handler.rs:345` `classify_transport_error()` pattern-matches on lowercased substrings (`"unreachable"`, `"timed out"`, `"dns"`, etc.) from `LettreTransport::try_deliver`'s `Box<dyn Error>`. Any future refactor of those error strings will silently re-classify `Temp` vs `Delivery`. Replace with a typed error enum on `MailTransport::send`.

**Priority:** P1

- [x] Define `TransportError { Temp(String), Permanent(String) }` (or equivalent) in `src/transport.rs`
- [x] `MailTransport::send` returns `Result<String, TransportError>` instead of `Result<String, Box<dyn Error>>`
- [x] `LettreTransport::send` classifies errors at the source (DNS/connect failures → `Temp`, SMTP rejects → `Permanent`) and wraps them in the enum
- [x] `FileDropTransport` (test transport) updated for the new return type
- [x] `send_handler.rs`: delete `classify_transport_error()`; match on `TransportError` variants directly
- [x] Existing send handler tests updated; behavior preserved (same error codes for same conditions)
- [x] `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt -- --check` clean

### S41-5 — DNS error surfacing in `resolve_ipv4`

**Context:** `src/transport.rs:199` — `Self::resolve_ipv4(host).unwrap_or_default()` swallows DNS lookup errors, producing an empty `Vec` that triggers `SkipNoIpv4` with the misleading message "no A record (enable_ipv6 = false); skipping." Operators can't distinguish "no A record" from "resolver unreachable."

**Priority:** P1

- [x] `resolve_ipv4` errors are surfaced: log the underlying DNS error before falling back, or propagate a distinct `DnsFailure` error message that names the original error
- [x] The `SkipNoIpv4` path's error message is updated or a new `DnsError` path added so the two cases produce different error text
- [x] Add a test: mock/force a DNS failure and verify the error message mentions the resolver error, not "no A record"
- [x] `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt -- --check` clean

### S41-6 — Test DKIM keypair caching

**Context:** Each integration test that spawns `aimx serve` re-shells `aimx dkim-keygen` to create a fresh 2048-bit RSA keypair (~200ms each). With the Sprint 34+ test suite spawning `aimx serve` in many tests, a sharable keypair cache would cut ~10-15s off the integration run.

**Priority:** P2

- [x] Add a `once_cell::sync::Lazy` (or `std::sync::LazyLock` if MSRV permits) that holds a `TempDir` with a pre-generated DKIM keypair, shared across all integration tests in the process
- [x] Integration tests that need DKIM keys reference the cached dir instead of calling `aimx dkim-keygen`
- [x] All integration tests still pass and are not coupled to each other (read-only access to the shared keypair)
- [x] Measure: `cargo test --test integration` time before and after; expect ~10-15s improvement
- [x] `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt -- --check` clean

### S41-7 — Stale `#[allow(dead_code)]` + integration test gap

**Context:** Two cleanup items:
(a) `src/send_protocol.rs:285` has `#[allow(dead_code)]` on `write_request` with comment "consumed by `aimx send` in Sprint 35" — Sprint 35 shipped and `write_request` is now used by `send.rs:286`, `serve.rs:944`, and tests. The annotation is stale.
(b) PR #70 review flagged a missing integration test: `aimx serve` in a tempdir with a stale `README.md` should refresh it at startup. Unit tests cover this, but no integration test verifies the startup refresh path.

**Priority:** P2

- [x] Remove stale `#[allow(dead_code)]` from `write_request` in `send_protocol.rs:285` and its comment
- [x] Add integration test: write a stale/mismatched `README.md` to a tempdir's data root, start `aimx serve`, verify the file is refreshed to match the baked-in version
- [x] `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt -- --check` clean

---

## Sprint 42 — CLI UX: Config Error Messages + Setup Port-Check Race + Version Hash (Days 118–120.5) [IN PROGRESS]

**Goal:** Fix P0 UX issues that block first-time setup and improve build traceability: (1) commands that require config give a cryptic "os error 2" instead of pointing the user to `aimx setup`, (2) `aimx setup` fails the inbound port 25 check because it races against `aimx serve` startup, and (3) `aimx --version` includes the git commit hash so pre-release builds are distinguishable.

**Dependencies:** Sprint 41 (all prior work complete)

#### S42-1: Helpful error message when config file is missing

**Context:** Running `aimx status` (or any config-dependent command: `mcp`, `send`, `mailbox`, `serve`) on a fresh VPS before `aimx setup` produces `Error: No such file or directory (os error 2)` — the raw ENOENT from trying to open `/etc/aimx/config.toml`. Users can't tell what's missing or what to do next. The fix should catch the "config not found" case in the config loading path and produce a message like: `Config file not found at /etc/aimx/config.toml — run 'sudo aimx setup' first`. This should cover all subcommands that load config (status, mcp, send, mailbox, serve, agent-setup).

**Priority:** P0

- [x] `config::load()` (or the call site in `main.rs`) catches `io::ErrorKind::NotFound` on the config file and returns a clear error naming the expected path and suggesting `sudo aimx setup`
- [x] Error message includes the actual path attempted (respects `AIMX_CONFIG_DIR` override)
- [x] All config-dependent subcommands benefit from the fix (status, mcp, send, mailbox, serve, agent-setup, dkim-keygen) — no raw "os error 2" leaks to the user
- [x] Unit test: calling config load with a nonexistent path produces the expected error message, not a raw IO error
- [x] `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt -- --check` clean

#### S42-2: Wait-for-ready loop in `aimx setup` before port checks

**Context:** After `install_service_file()` calls `restart_service("aimx")`, setup immediately runs the outbound + inbound port 25 checks. `restart_service()` returns as soon as `systemctl restart aimx` exits — not when `aimx serve` has finished binding port 25. The outbound check (local → remote verifier) usually passes because it doesn't need the local listener. The inbound check (remote verifier → local port 25 EHLO) fails because `aimx serve` hasn't bound yet. Standalone `aimx verify` doesn't have this problem because it either detects an already-running daemon or spawns its own listener and waits for readiness. Fix: after restarting the service and before running port checks, poll for `aimx serve` readiness — e.g., attempt a TCP connect to `127.0.0.1:25` in a retry loop (up to ~5 seconds, ~500ms between attempts). If the loop times out, proceed with the checks anyway (they'll fail with the existing error message, which is still accurate).

**Priority:** P0

- [x] After `restart_service("aimx")` returns, a wait-for-ready loop polls `127.0.0.1:25` (TCP connect) with ~500ms interval, up to ~5s total
- [x] Loop exits early as soon as a connection succeeds (port is bound)
- [x] If the loop times out (service didn't bind within 5s), setup proceeds to the port checks without error — the existing "Inbound port 25... FAIL" message covers this case
- [x] The wait loop is behind the `SystemOps` trait (or `NetworkOps`) so tests can mock it without real sleeps
- [x] Existing setup tests still pass; new test verifies that setup proceeds after the wait loop succeeds
- [x] `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt -- --check` clean

#### S42-3: Include git commit hash in `aimx --version` output

**Context:** Pre-release, `aimx --version` prints only `aimx 0.1.0` (from `Cargo.toml`). When testing builds on a VPS it's impossible to tell which commit the binary was built from. Add a `build.rs` that captures the 8-character short git hash at compile time and bakes it into the version string so `aimx --version` prints e.g. `aimx 0.1.0 (abcd1234)`. If the build happens outside a git repo (e.g. `cargo install` from a tarball), fall back gracefully to just the version number without a hash.

**Priority:** P1

- [x] New `build.rs` at the repo root runs `git rev-parse --short=8 HEAD` and sets a `GIT_HASH` env var via `cargo:rustc-env`
- [x] If `git` is unavailable or the working directory isn't a repo, `GIT_HASH` is set to `"unknown"` (no build failure)
- [x] `cli.rs` composes the clap version string as `format!("{} ({})", env!("CARGO_PKG_VERSION"), env!("GIT_HASH"))` — output: `aimx 0.1.0 (abcd1234)`
- [x] When `GIT_HASH` is `"unknown"`, version string omits the parenthetical — output: `aimx 0.1.0`
- [x] `build.rs` emits `cargo:rerun-if-changed=.git/HEAD` and `cargo:rerun-if-changed=.git/refs` so the hash updates on new commits without full rebuilds
- [x] `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt -- --check` clean

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
| 27 | 76–78.5 | Systemd Unit Hardening | Restart rate-limit, resource limits, network-online deps in generated systemd unit | Done |
| 27.5 | 78.5–79 | CLI Color Consistency | `src/term.rs` semantic helpers, migrate setup.rs, apply across verify/status/mailbox/send/dkim/serve/main | Done |
| 27.6 | — | CI Binary Releases | _Deferred to the Non-blocking Review Backlog — revisit when production-ready_ | Deferred |
| 28 | 79.5–82 | Agent Integration Framework + Claude Code | `agents/` tree, `aimx agent-setup` command, Claude Code plugin, PRD §6.10 | Done |
| 29 | 82–84.5 | Codex CLI + OpenCode + Gemini CLI Integration | Codex plugin, OpenCode skill, Gemini skill, book/ updates | Done |
| 30 | 84.5–87 | Goose + OpenClaw Integration | Goose recipe, OpenClaw skill, README overhaul | Done |
| 31 | 87–89.5 | Channel-Trigger Cookbook | `book/channel-recipes.md`, channel-trigger integration test, cross-links | Done |
| 32 | 89.5–92 | Non-blocking Cleanup | Verifier concurrency bound, outbound DATA sharing + multi-MX errors, TLS/service consistency, NetworkOps dedup, clippy `--all-targets`, cosmetics | Done |
| 33 | 92–94.5 | v0.2 Filesystem Split + `aimx` Group (group reverted in 33.1) | `/etc/aimx/` for config + DKIM keys, `/run/aimx/` via `RuntimeDirectory=aimx`, DKIM private key back to `600` root-only | Done |
| 33.1 | 94.5–97 | Scope Reversal: Drop PTR + `aimx` Group + Non-blocking Cleanup | Strip PTR/reverse-DNS, drop `aimx` system group + group-gating, clear ready-now backlog items, manual E2E validation of Claude Code + Codex CLI plugins | Done |
| 34 | 97–99.5 | v0.2 UDS Wire Protocol + Daemon Send Handler | `src/send_protocol.rs` codec, `aimx serve` binds `/run/aimx/send.sock` (`0o666` world-writable), per-connection handler signs + delivers with `SO_PEERCRED` logged for diagnostics only | Done |
| 35 | 99.5–102 | v0.2 Thin UDS Client + End-to-End | `aimx send` rewritten as UDS client (no DKIM access), end-to-end integration test from client → signed delivery, dead-code + docs sweep | Done |
| 36 | 102–104.5 | v0.2 Datadir Reshape | `inbox/` + `sent/` split per mailbox, `YYYY-MM-DD-HHMMSS-<slug>.md` filenames, Zola-style attachment bundles, mailbox lifecycle touches both trees, MCP `folder` param | Done |
| 37 | 104.5–107 | v0.2 Frontmatter Schema + DMARC | `InboundFrontmatter` struct with section ordering, new fields (`thread_id`, `received_at`, `received_from_ip`, `size_bytes`, `delivered_to`, `list_id`, `auto_submitted`, `dmarc`, `labels`), DMARC verification | Done |
| 38 | 107–109.5 | v0.2 `trusted` Field + Sent-Items Persistence | Always-written `trusted: "none"\|"true"\|"false"` (v1 trust model preserved), sent mail persisted to `sent/<mailbox>/` with outbound block + `delivery_status` | Done |
| 39 | 109.5–112 | v0.2 Primer Skill Bundle + Author Metadata | `agents/common/aimx-primer.md` split into main + `references/`, install-time suffix + references-copy, `U-Zyn Chua <chua@uzyn.com>` standardized repo-wide | Done |
| 40 | 112–114.5 | v0.2 Datadir README + Journald + Book/ | Baked-in `/var/lib/aimx/README.md` with version-gate refresh on `aimx serve` startup, `journalctl -u aimx` replaces stale `/var/log/aimx.log`, full `book/` + `CLAUDE.md` pass | Done |
| 41 | 115–117.5 | Post-v0.2 Backlog Cleanup | Outbound frontmatter fixes, SPF dedup, UDS slow-loris timeout, typed transport errors, DNS error surfacing, test DKIM cache, stale dead_code sweep | Done |
| 42 | 118–120.5 | CLI UX: Config Errors + Setup Race + Version Hash | Helpful error when config missing, wait-for-ready loop in `aimx setup` before port checks, git commit hash in `aimx --version` | In Progress |

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
- [x] **(Sprint 38)** Parity test docstring in `src/trust.rs` says "IFF" (if and only if) but the test only checks one direction. — _Triaged into Sprint 41 (S41-1)._
- [x] **(Sprint 38)** `received_at` in `OutboundFrontmatter` serializes as empty string `""` for outbound messages instead of being omitted. — _Triaged into Sprint 41 (S41-1)._
- [x] **(Sprint 38)** `date` field in outbound frontmatter uses a fresh `Utc::now()` timestamp instead of parsing the `Date:` header. — _Triaged into Sprint 41 (S41-1)._
- [x] **(Sprint 37)** SPF is still verified twice in `src/ingest.rs` — redundant DNS lookup per ingest. — _Triaged into Sprint 41 (S41-2)._
- [x] **(Sprint 35)** `LettreTransport::resolve_ipv4` in `src/transport.rs` swallows DNS failures with `unwrap_or_default()`. — _Triaged into Sprint 41 (S41-5)._
- [x] **(Sprint 35, PR #65)** Stale `#[allow(dead_code)]` on `write_request` in `send_protocol.rs:285` — Sprint 35 shipped, function is now used by `send.rs`, `serve.rs`, and tests. — _Triaged into Sprint 41 (S41-7)._
- [x] **(Sprint 40, PR #70)** Missing integration test: `aimx serve` in tempdir with stale `README.md` refreshed at startup. — _Triaged into Sprint 41 (S41-7)._
- [x] **(Sprint 36, PR #66)** `mailbox_list` reads `config.mailboxes.keys()` instead of scanning `inbox/*/` — stray dirs not in config are invisible. — _Not a bug: config-authoritative mailbox list is the intended design (2026-04-16)._
- [x] **(Sprint 36, PR #66)** Concurrent-ingest race on bundle directories — two ingests with the same subject/second can cross-contaminate attachment files. — _Deferred by user decision (2026-04-16). Unlikely in practice; locking design needed._
- [x] **(Sprint 34, PR #64)** `LettreTransport::send` parses full `To:` header as `lettre::Address` — fails on display-name or multi-recipient form. — _Already fixed: `send_handler.rs:148` now uses `extract_bare_address(&to_header)` to normalize before transport._

### Deferred Feature Sprints

Feature sprints that were planned, then deferred by the user. Full spec preserved so the work can be promoted back to an active sprint without loss. Revisit when the gating condition is met.

- [ ] **(Originally Sprint 27.6 — deferred by user pending production readiness)** **CI Binary Releases.**
  **Goal:** Publish prebuilt `aimx` binaries for common Linux architectures so users can `curl | tar` instead of installing Rust and running `cargo build`. Tags produce attached GitHub Release artifacts; `main` merges produce workflow artifacts (90-day retention).
  **Gating condition:** Revisit when AIMX is ready to promote to production users (e.g., once the PRD's v1 scope is otherwise complete and distribution is the remaining gap).
  **Scope / acceptance criteria (preserved verbatim):**
  - New `.github/workflows/release.yml` triggered on `push: tags: ['v*']`
  - Release workflow matrix builds four targets: `x86_64-unknown-linux-gnu`, `x86_64-unknown-linux-musl`, `aarch64-unknown-linux-gnu`, `aarch64-unknown-linux-musl`
  - Each job produces `aimx-<version>-<target>.tar.gz` containing `aimx` binary (+x permissions preserved), `LICENSE`, `README.md`
  - Final aggregation step computes `SHA256SUMS` (one line per tarball) and uploads it alongside the tarballs
  - All artifacts attached to the GitHub Release matching the tag via `softprops/action-gh-release` (Release auto-created if missing)
  - `ci.yml` gains a `build-binaries` job that runs only on `push` to `main` and uploads the same four binaries as workflow artifacts with 90-day retention
  - Cross-compilation for aarch64 uses `cross` or documented equivalent; musl builds reuse the Alpine-style musl toolchain pattern
  - `aimx --version` output of a downloaded binary matches the git tag (requires `Cargo.toml` version to match the tag; maintainer step documented in the workflow or README)
  - Binary on each Linux target runs `aimx --help` cleanly on a matching OS (manual validation at least once — fresh VPS, Alpine VM, aarch64 instance)
  - Existing CI jobs remain unchanged — release work is additive
  - `README.md` and `book/getting-started.md` gain an "Install from prebuilt binary" section with a `curl … | tar -xzf -` one-liner and SHA256 verification via `sha256sum -c SHA256SUMS`
  - Dry-run validation: push a `v0.0.0-test` tag (or use `workflow_dispatch`), confirm all four tarballs + SHA256SUMS land on the Release; delete the test tag/release afterwards
  - PRD §9 In Scope already mentions this work; no PRD change needed on promotion
  **Out of scope:** verifier service binary (deployed via Docker), macOS/Windows targets, auto-tagging/version bumps
