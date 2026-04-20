# AIMX — Hook Templates Sprint Plan

**Sprint cadence:** 2.5 days per sprint
**Team:** Solo developer with heavy AI augmentation (Claude Code)
**Total sprints:** 6
**Timeline:** ~15 calendar days (Days 1–15 of the hook-templates track, independent of the core aimx sprint clock)
**v1 Scope:** Everything in [`hook-templates-prd.md`](hook-templates-prd.md) §9 "In Scope (v1)". Pre-launch feature — no migration path. Sprint 1 front-loads mechanical breaking changes (socket rename, schema, service user). Sprint 2 rewrites the hook executor onto an argv + sandbox + timeout base. Sprint 3 locks down the UDS verb surface so MCP physically cannot submit raw-cmd hooks. Sprint 4 ships the 8 default templates behind an interactive `aimx setup` checkbox and adds the `aimx agent-setup` hint. Sprint 5 adds the four new MCP tools and refreshes the agent-facing primer. Sprint 6 closes with end-to-end integration tests, placeholder-substitution fuzz tests, book docs, and `aimx doctor` extensions.

**Source PRD:** [`hook-templates-prd.md`](hook-templates-prd.md) — every story traces back to a requirement section there. When acceptance criteria reference `§X.Y`, they mean sections of that PRD.

---

## Sprint 1 — Foundation: schema, socket rename, service user (Days 1–2.5) [DONE]

**Goal:** Land all low-risk mechanical changes (socket rename, config schema additions, service user creation) so every downstream sprint builds on a clean base.

**Dependencies:** None — this is the foundation sprint for the hook-templates track.

#### S1-1: Rename runtime socket `send.sock` → `aimx.sock`

**Context:** The UDS at `/run/aimx/send.sock` is named after the first verb it carried (`SEND`) but now handles mailbox CRUD, mark-read, and hook CRUD too. PRD §6.5 calls for renaming to `/run/aimx/aimx.sock` to match the `/run/<service>/<service>.sock` convention used by containerd, podman, and Docker. Mechanically this is a one-word change at the `SEND_SOCKET_NAME` constant (`src/serve.rs` around line 246), but it propagates to all client call sites (`src/hook_client.rs`, `src/mailbox.rs`, `src/hooks.rs`, `src/state_handler.rs`, `src/send.rs`) and every test that references the path literally (`src/serve.rs` tests at ~1457, 1474, 1563, 1616, 1744; `tests/integration.rs` wherever UDS fixtures appear). Pre-launch, so no compatibility shim is needed — the rename is atomic across one release.

**Priority:** P0

- [x] `SEND_SOCKET_NAME` constant in `src/serve.rs` renamed to `AIMX_SOCKET_NAME` with value `"aimx.sock"`; `send_socket_path()` renamed to `aimx_socket_path()` (or equivalent) and updated accordingly
- [x] All client call sites (`hook_client.rs`, `mailbox.rs`, `hooks.rs`, `state_handler.rs`, `send.rs`) resolve the new path via the shared helper — no literal `send.sock` strings remain in non-test code
- [x] All test fixtures, snapshot paths, and test helpers updated to use the new name; `grep -r "send.sock" src/ tests/` returns no hits except in historical comments (if any) that explicitly call out the rename
- [x] Systemd unit generator (`src/setup.rs`) and OpenRC template (same) still declare `RuntimeDirectory=aimx` — the directory name does not change, only the socket filename inside it
- [x] Release notes / `CHANGELOG.md` (if present) note the rename as a breaking change for pre-launch testers; `aimx doctor` surfaces a helpful error if it sees `send.sock` but not `aimx.sock`
- [x] `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt -- --check` clean

#### S1-2: Add `[[hook_template]]` schema with load-time validation

**Context:** PRD §6.1 defines the template block: `name`, `description`, `cmd` (argv array), `params`, `stdin`, `run_as`, `timeout_secs`, `allowed_events`. The schema lands in `src/config.rs` alongside the existing `Config` / `MailboxConfig` / `Hook` structs. Validation is non-trivial: every `{placeholder}` in `cmd` must appear in `params`, every `params` entry must appear in at least one `cmd` string value, no placeholder may live in `cmd[0]` (the binary path), and substitution values must not be able to split into new argv entries. Validation runs at `Config::load()` — a malformed template fails daemon startup, not the first hook fire. Built-in placeholders (`{event}`, `{mailbox}`, `{message_id}`, `{from}`, `{subject}`) are recognized without being declared in `params`.

**Priority:** P0

- [x] New `HookTemplate` struct in `src/config.rs` with `#[derive(Deserialize, Serialize, Clone, Debug)]` and fields matching PRD §6.1 exactly; field defaults applied via `#[serde(default = "...")]` where appropriate (e.g. `timeout_secs` defaults to 60, `run_as` defaults to `"aimx-hook"`, `allowed_events` defaults to both events)
- [x] `Config` struct gains `#[serde(default)] hook_templates: Vec<HookTemplate>`
- [x] New `validate_hook_templates(&[HookTemplate]) -> Result<(), ConfigError>` function called from `Config::load()` after TOML parse; returns `Err` on: duplicate template `name`, unknown placeholder in `cmd` not in `params + builtins`, declared-but-unused `params` entry, placeholder in `cmd[0]`, empty `cmd` array, `timeout_secs > 600` or `< 1`, `run_as` value other than `"aimx-hook"` or `"root"` <!-- Review follow-up: param charset validation + built-in collision rejection also added in fix commit -->
- [x] Unit tests cover each rejection path with a minimal failing TOML input, plus a golden "valid template" round-trip test
- [x] A TOML fixture file in `tests/fixtures/` demonstrating a valid multi-template config is round-tripped through `Config::load → Config::save` without diff
- [x] `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt -- --check` clean

#### S1-3: Extend `Hook` struct with `origin` + `template` + `params`

**Context:** PRD §6.5 introduces the `origin = "operator" | "mcp"` tag that gates UDS `HOOK-DELETE` and drives the MCP visibility rules in §6.6. Template-bound hooks also need to record which template they reference (`template: Option<String>`) and the bound parameter values (`params: BTreeMap<String, String>`), so the hook fire path can re-substitute at runtime (the config can be reloaded with an updated template; hooks should pick up the change). Raw-cmd hooks keep `template = None` and continue to use the existing `cmd` field. Pre-launch, so the struct layout can change freely; absent `origin` in existing TOML defaults to `Operator` via `#[serde(default)]`.

**Priority:** P0

- [x] New `HookOrigin` enum (`Operator`, `Mcp`) with `#[serde(rename_all = "lowercase")]` in `src/hook.rs`; `Operator` is the `Default::default()` value
- [x] `Hook` struct gains `#[serde(default)] origin: HookOrigin`, `#[serde(default)] template: Option<String>`, `#[serde(default)] params: BTreeMap<String, String>`
- [x] Mutual-exclusion validation: if `template` is `Some`, `cmd` must be absent / empty; if `template` is `None`, `cmd` must be non-empty. Enforced in `Config::load` alongside existing hook validation <!-- Also: MCP-origin hooks cannot set dangerously_support_untrusted (both config load + UDS validate_single_hook paths) -->
- [x] `Hook::is_template_bound(&self) -> bool` convenience method. (Note: `Hook::resolve_argv` was originally listed here but moved to Sprint 2's S2-1, where the `hook_substitute.rs` module it would delegate to actually lives — see the S2-1 bullet list below.)
- [x] Unit tests cover: operator-origin hook with raw cmd round-trips; MCP-origin hook with template + params round-trips; mutually-exclusive (`cmd` + `template`) fails validation; missing template reference fails validation
- [x] `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt -- --check` clean

#### S1-4: Create `aimx-hook` system user in `aimx setup`

**Context:** PRD §6.3 and §8.2 require a dedicated `aimx-hook` unprivileged user that hooks drop privileges to before `exec`. `aimx setup` already handles root-only bootstrap (writing `/etc/aimx/`, installing the systemd/OpenRC unit, generating DKIM keys); user creation fits in the same phase. The creation must be idempotent (re-running setup on an already-installed box must not fail), and the user must be usable as both UID and GID target (we create a matching primary group). After user creation, `/var/lib/aimx/inbox` and `/var/lib/aimx/sent` are chowned `root:aimx-hook` and chmod'd `g+rX` recursively so `stdin = "email"` can read piped email content at hook fire time.

**Priority:** P0

- [x] `src/setup.rs` gains an `ensure_hook_user(&dyn SystemOps) -> Result<()>` function that runs `useradd --system --no-create-home --shell /usr/sbin/nologin aimx-hook` (or the platform equivalent via `SystemOps`), gracefully no-ops if `id aimx-hook` already resolves, and creates a matching `aimx-hook` system group <!-- Fallback to BusyBox adduser only on ErrorKind::NotFound (review fix) -->
- [x] `run_setup` calls `ensure_hook_user` after config + DKIM setup and before systemd unit install, so the unit file can reference the user if needed
- [x] `chown_datadir_for_hook_user(&Path, &dyn SystemOps)` helper chowns `<datadir>/inbox` and `<datadir>/sent` to `root:aimx-hook` and chmods `g+rX` recursively; called from `run_setup` after mailbox directories exist
- [x] `MockSystemOps` in the test module records user-creation and chown attempts so unit tests can assert `aimx setup` triggers them in the right order; integration test on a live Linux box is manual (CI does not root-create users)
- [x] Colorized `[User]` section in setup output reports `aimx-hook created` / `aimx-hook already present`, matching the existing `[DNS]` / `[MCP]` style
- [x] `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt -- --check` clean

---

## Sprint 2 — Sandboxed hook executor (Days 2.5–5) [DONE]

**Goal:** Replace today's `sh -c` executor with an argv-based, privilege-dropping, timeout-enforced sandboxed runner. This is the highest-risk sprint in the track — the current `Command::new("sh").arg("-c")` call at `src/hook.rs:309` is tightly coupled and every downstream sprint depends on a working new executor.

**Dependencies:** Sprint 1 (service user must exist; `Hook` struct must carry `template` + `params`).

#### S2-1: Placeholder substitution with argv-safety guarantees

**Context:** PRD §6.1 requires substitution rules that are injection-resistant by construction: `{placeholder}` values fill string slots inside argv entries, never introduce new argv entries, never appear in `cmd[0]`, and never carry whitespace, NUL, or unprintable control chars (newlines and tabs allowed, quoted values are fine as-is since there's no shell). The substitution function is pure — no I/O, no locks — so it can be unit-tested with aggressive fuzz inputs. A new module `src/hook_substitute.rs` keeps this self-contained and testable in isolation.

**Priority:** P0

- [x] New `src/hook_substitute.rs` module exposing `pub fn substitute_argv(template_cmd: &[String], params: &BTreeMap<String, String>, builtins: &BuiltinContext) -> Result<Vec<String>, SubstitutionError>`
- [x] `Hook::resolve_argv(&self, templates: &[HookTemplate], builtins: &BuiltinContext) -> Result<Vec<String>, SubstitutionError>` (moved from S1-3): returns the final argv by delegating to `substitute_argv` for template-bound hooks and returning `vec!["/bin/sh".into(), "-c".into(), self.cmd.clone()]` for raw-cmd hooks. Grouped here because the implementation can't exist without `substitute_argv`
- [x] `BuiltinContext { event, mailbox, message_id, from, subject }` populated by the caller at fire time; missing builtins are substituted as empty strings (PRD: builtins are always available but may be empty when irrelevant)
- [x] `SubstitutionError` variants: `UnknownPlaceholder`, `ParamContainsWhitespace`, `ParamContainsNul`, `ParamContainsControl`, `ParamTooLong` (>8 KiB per value), `PlaceholderInBinaryPath`
- [x] Substitution happens after argv parse: placeholder appears anywhere inside a string slot → replace in-place; a placeholder occupying the entire slot still produces exactly one argv entry
- [x] Unit tests cover: happy path with multiple params; unknown placeholder rejection; whitespace rejection (space, tab, newline); NUL rejection; control-char rejection; long-value rejection; placeholder in `cmd[0]` rejection; placeholders only referenced via builtins; param declared but never used (caught at config load, not substitution)
- [x] Fuzz test (`cargo test --release`) iterates 10_000 random inputs containing shell metacharacters (`;`, `$(...)`, backticks, `|`, `&&`, `>`, newlines) and asserts `substituted.len() == template.len()` — substitution never expands argv count <!-- Implemented as inline 15,624-iteration loop -->
- [x] `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt -- --check` clean

#### S2-2: `spawn_sandboxed` platform helper (systemd-run + setuid fallback)

**Context:** PRD §6.7 defines the execution flow. On systemd boxes (`/run/systemd/system` exists — reuse `detect_init_system()` at `src/serve.rs:782`), spawn hooks via `systemd-run --uid=aimx-hook --gid=aimx-hook --property=ProtectSystem=strict --property=PrivateDevices=yes --property=NoNewPrivileges=yes --property=MemoryMax=256M --property=RuntimeMaxSec={N} --collect --pipe -- <argv>`. On OpenRC / unknown, `fork()` + in-child `setgid(aimx_hook_gid)` + `setuid(aimx_hook_uid)` + `execvp(argv[0], argv)`, wrapped in a parent-side wait-with-timeout loop. The helper returns `(exit_code, stdout_tail, stderr_tail, duration)` so the caller can log uniformly regardless of sandbox path.

**Priority:** P0

- [x] New `src/platform.rs::spawn_sandboxed(argv: &[String], stdin: SandboxStdin, run_as: &str, timeout: Duration) -> Result<SandboxOutcome, SandboxError>` function
- [x] `SandboxStdin` enum: `Email(Vec<u8>)`, `EmailJson(Vec<u8>)`, `None`; written to the child's stdin and closed before timeout starts (no hook blocks on stdin read mid-run)
- [x] Systemd path detected via `detect_init_system()`; shells out to `systemd-run` with the exact `--property=...` set from PRD §6.7; captures stdout + stderr via `--pipe` and parses them from the `systemd-run` output stream
- [x] Fallback path uses `Command` + `pre_exec` (setsid + drop supplementary groups + setgid + setuid) and a parent-side poll loop on the child's wait; SIGTERM at `timeout`, SIGKILL at `timeout + 5s` (both signalled to the process group) <!-- Chose stdlib Command::pre_exec + libc over the nix crate; no new dependency -->
- [x] `SandboxOutcome { exit_code: i32, stdout_tail: Vec<u8>, stderr_tail: Vec<u8>, duration: Duration, sandbox: "systemd-run" | "setuid" }` truncates stdout and stderr at 64 KiB each
- [x] `SandboxError` variants: `UserNotFound` (aimx-hook UID lookup failed), `SpawnFailed(io::Error)`, `SystemdRunFailed(String)`, `TimedOut`, `Killed(i32)` — all carry enough detail for the caller to log meaningfully
- [x] Unit tests use a fake binary in a tempdir (e.g. a tiny shell script that prints, sleeps, or exits with a chosen code) to verify exit code propagation, stdout/stderr capture, truncation at 64 KiB, and timeout behavior (a 2-second sleep with a 500ms timeout must be SIGKILLed); `setuid` path tests are skipped as non-root (CI) but run when `cargo test` is invoked as root <!-- Test-only AIMX_SANDBOX_FORCE_FALLBACK env var forces the fallback path on systemd CI hosts -->
- [x] `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt -- --check` clean

#### S2-3: Structured hook-fire log with new fields

**Context:** The current log line at `src/hook.rs` around line 343 emits `hook_name`, `event`, `mailbox`, `email_id`, `exit_code`, `duration_ms`. PRD §7.3 asks for three new fields: `template` (template name or `-` for raw-cmd), `run_as` (usually `aimx-hook`; `root` only for explicitly-opted raw-cmd hooks), and `stderr_tail` (last 64 KiB of stderr, for quick debugging without `strace`). The log line stays one structured record — consumers like `aimx doctor` parse it via `journalctl -u aimx --output=json`.

**Priority:** P0

- [x] Log line in `src/hook.rs` extended with `template = ?`, `run_as = ?`, `stderr_tail = ?` fields via `tracing::info!` structured kv syntax <!-- Also added sandbox and timed_out for executor visibility -->
- [x] `stderr_tail` is JSON-escaped so multi-line output doesn't break structured parsing; empty stderr is represented as `""`, not `null` <!-- UTF-8-safe truncation uses head...tail form with char-boundary checks -->
- [x] `aimx logs` (which pipes through `journalctl`) does not need changes — structured fields just flow through
- [x] Unit test for the log-line formatter verifies each field appears with the expected key and type, including edge cases (empty stderr, very long stderr truncated at 64 KiB, template=None becomes `-`)
- [x] `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt -- --check` clean

#### S2-4: Wire sandboxed executor into on_receive + after_send

**Context:** The existing `run_and_log` at `src/hook.rs:300` is called from `src/ingest.rs:262` for `on_receive` and `src/send_handler.rs:350` for `after_send`. Both call sites stay the same — only `run_and_log` changes internally: for template hooks it calls `Hook::resolve_argv` then `spawn_sandboxed`; for raw-cmd hooks it passes `["/bin/sh", "-c", cmd]` to `spawn_sandboxed` with `run_as = "aimx-hook"` by default (or `"root"` if the operator explicitly set `run_as = "root"` in the hook block). The key invariant: **no hook ever spawns as root unless the operator set `run_as = "root"` in `config.toml`**, and that field is not settable via UDS.

**Priority:** P0

- [x] `run_and_log` refactored to: (a) resolve argv via `Hook::resolve_argv`, (b) build the `BuiltinContext` from the ingest/send event, (c) prepare stdin per `template.stdin` (or `"email"` default for raw-cmd), (d) call `spawn_sandboxed` with the hook's `run_as` (defaults to `"aimx-hook"`) <!-- Hook struct gained run_as: Option<String>; UDS rejects the field per PRD §6.7 via decode_hook_body stopgap -->
- [x] Raw-cmd hooks with `run_as = "root"` in `config.toml` still work — `spawn_sandboxed` accepts `"root"` and maps it to the current process user (no setuid)
- [x] Integration test spawns a raw-cmd hook that writes its effective UID to a tempfile; assertion verifies UID matches `aimx-hook` (skipped on non-root CI)
- [x] Env vars passed to hooks (`AIMX_MAILBOX`, `AIMX_EVENT`, `AIMX_MESSAGE_ID`, etc. — currently set in `src/hook.rs`) continue to work; check they propagate through `systemd-run --setenv=KEY=VAL` on the systemd path <!-- Expanded: AIMX_MESSAGE_ID, AIMX_EVENT, AIMX_ID, AIMX_DATE on both paths -->
- [x] Removed dead code from the old `Command::new("sh").arg("-c")` path; no fallbacks, no compat shim
- [x] `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt -- --check` clean

---

## Sprint 3 — UDS protocol + CLI wiring + SIGHUP reload (Days 5–7.5) [IN PROGRESS]

**Goal:** Lock down the UDS verb surface so MCP physically cannot submit raw-cmd hooks, and wire the operator's CLI path to write `config.toml` directly with a SIGHUP-based hot-reload.

**Dependencies:** Sprint 1 (schema); Sprint 2 (executor must handle both template and raw-cmd hooks uniformly).

#### S3-1: Tighten UDS `HOOK-CREATE` to template-only

**Context:** PRD §6.5 specifies that `HOOK-CREATE` body changes from TOML-encoded `Hook` to JSON `{ template, params, name? }`. The daemon rejects any body containing `cmd`, `run_as`, `dangerously_support_untrusted`, `timeout_secs`, or `stdin` — these are template properties, not hook properties. Unknown template names produce an `ERR unknown-template` with a hint pointing the caller at `aimx hooks templates` and `sudo aimx hooks template-enable <name>` (the latter is deferred to v2 per PRD §9, but the hint still reads naturally). The daemon stamps `origin = "mcp"` on every hook it creates via this verb.

**Priority:** P0

- [ ] `src/send_protocol.rs::Request::HookCreate` variant changes: body is now `HookTemplateCreateBody { template: String, params: BTreeMap<String, String>, name: Option<String> }` parsed via `serde_json`
- [ ] Parser rejects bodies containing the forbidden fields (`cmd`, `run_as`, `dangerously_support_untrusted`, `timeout_secs`, `stdin`) — this is enforced at the JSON schema level so error messages are precise
- [ ] `src/hook_handler.rs::handle_hook_create` validates: mailbox exists, template exists and is enabled, event is in `template.allowed_events`, all declared `params` are present and pass substitution validation, resulting hook name is unique within the mailbox
- [ ] Daemon stamps `origin = Mcp` on the constructed `Hook` before writing to config
- [ ] Error responses carry specific reasons: `ERR unknown-template`, `ERR missing-param: KEY`, `ERR event-not-allowed`, `ERR mailbox-not-found`, `ERR name-conflict` — all human-readable and actionable
- [ ] Unit tests cover each rejection path with a minimal request fixture
- [ ] Integration test: submit a valid `HOOK-CREATE` → verify hook appears in `config.toml` with `origin = "mcp"`; submit a body with `cmd = "..."` → verify rejection
- [ ] `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt -- --check` clean

#### S3-2: Tighten UDS `HOOK-DELETE` with origin check

**Context:** PRD §6.5 requires `HOOK-DELETE` on the UDS socket to refuse deletion of operator-origin hooks (`origin = "operator"`). An MCP client can remove hooks it created but cannot interfere with the operator's hand-rolled hooks. Operator deletes go through the CLI (`sudo aimx hooks delete`), which writes `config.toml` directly.

**Priority:** P0

- [ ] `src/hook_handler.rs::handle_hook_delete` looks up the target hook by name across all mailboxes, checks `hook.origin`, and returns `ERR origin-protected: hook was created by the operator — remove via \`sudo aimx hooks delete\` instead` if `origin == Operator`
- [ ] Successful delete still swaps `ConfigHandle` via the existing atomic path; the response is `AIMX/1 OK` unchanged
- [ ] Unit test covers operator-protected rejection, successful MCP-origin delete, and unknown-name case
- [ ] `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt -- --check` clean

#### S3-3: CLI `aimx hooks create --template` + `--param`

**Context:** PRD §6.6 exposes template-based creation via MCP, but operators also want to create template hooks from the CLI (e.g. for scripting). `aimx hooks create` grows two new flags: `--template NAME` (mutually exclusive with `--cmd`) and `--param KEY=VAL` (repeatable). When `--template` is set the CLI uses the UDS path (same verb MCP uses, producing `origin = "mcp"` — or we add a `--origin operator` override; PRD is silent, default to `operator` for CLI-origin template hooks since the operator is invoking it). When `--cmd` is set the CLI takes the raw-cmd path (§S3-4). When neither is set, the CLI prints a helpful error.

**Priority:** P0

- [ ] `src/cli.rs::HookCreateArgs` gains `#[arg(long, conflicts_with = "cmd")] template: Option<String>` and `#[arg(long = "param", value_name = "KEY=VAL")] params: Vec<String>`; `cmd` field becomes optional and `ArgGroup` enforces exactly-one-of (`--template`, `--cmd`)
- [ ] When `--template` is used, `src/hooks.rs::create` parses the `Vec<String>` into `BTreeMap<String, String>`, validates locally against the loaded config's templates (fast-fail on missing template / unknown params), then submits via a new `hook_client::submit_hook_template_create_via_daemon`
- [ ] CLI-origin template hooks get `origin = "operator"` in `config.toml` so the operator retains control and MCP cannot later delete them; this is visible via `aimx hooks list` (existing command) showing the `origin` column
- [ ] Error UX: `aimx hooks create --template invoke-claude --param prompt="..."` succeeds; `aimx hooks create --template invoke-claude` (missing required param) fails with a pointer to `aimx hooks templates` for param names
- [ ] Unit + integration tests cover the happy path and the param-parse failure path
- [ ] `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt -- --check` clean

#### S3-4: Raw-cmd CLI path writes `config.toml` directly (no UDS)

**Context:** Raw-cmd hooks must not be submittable over UDS (PRD §6.5). Today `aimx hooks create --cmd "..."` goes through the UDS `HOOK-CREATE` verb with TOML body; in §S3-1 that verb stops accepting `cmd`. The CLI raw-cmd path is therefore rerouted to write `config.toml` directly via the existing atomic-write helper in `src/config.rs`, then SIGHUP the running `aimx serve` so the in-memory `ConfigHandle` swaps to the new config. If no daemon is running, the file write succeeds and the CLI prints a "restart daemon when convenient" hint.

**Priority:** P0

- [ ] `aimx hooks create --cmd "..."` requires root (returns `ERR requires-root: run with sudo` otherwise); the existing `check_root()` helper in `src/setup.rs` (or equivalent) is reused
- [ ] When root, the CLI calls a new `hooks::write_raw_cmd_hook_to_config(&config_path, hook)` helper that reads `config.toml`, appends the new hook to the target mailbox's `hooks` vec, and writes back via atomic temp-then-rename (same pattern used by `mailbox_handler.rs`)
- [ ] After the write, the CLI sends SIGHUP to `aimx serve` if it is running (PID discovered via `/run/aimx/aimx.pid` if present, otherwise `pgrep -x aimx | head -1` fallback); if no daemon is running the CLI prints "config updated — restart aimx when convenient"
- [ ] `hooks create --cmd` does NOT go through UDS even when the daemon is reachable — the separation is absolute
- [ ] Unit test verifies the direct-write + SIGHUP path; integration test (root-gated) verifies a live daemon picks up the new hook within 2 seconds of the SIGHUP
- [ ] `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt -- --check` clean

#### S3-5: SIGHUP handler in `aimx serve`

**Context:** The daemon must hot-reload `config.toml` on SIGHUP so the CLI's raw-cmd path (§S3-4) and operators who hand-edit `config.toml` don't need to restart the service. Reuses the existing `ConfigHandle::store()` atomic swap at `src/config.rs` around line 426. The handler runs inside the existing tokio signal loop in `src/serve.rs` alongside the SIGTERM / SIGINT handlers. On validation failure the daemon keeps the old config and logs a warning — failing-open to the running config is safer than crashing.

**Priority:** P0

- [ ] `src/serve.rs` signal loop adds a SIGHUP branch that calls a new `reload_config(&ConfigHandle) -> Result<ReloadSummary, ReloadError>` function
- [ ] `reload_config` re-reads `config.toml`, runs the full validation chain (hook templates, per-hook origin/template mutual exclusion, trust config), atomically swaps `ConfigHandle` on success, and logs `"config reloaded: M mailboxes, H hooks, T templates"` at info level
- [ ] On validation error, `reload_config` logs at warn level with the specific error and **does not** swap the handle — the running daemon keeps operating on the last known-good config
- [ ] Integration test sends SIGHUP to a live test daemon with a modified `config.toml` and asserts the daemon's in-memory config reflects the change within 2 seconds (polled via an MCP `mailbox_list` tool call)
- [ ] Integration test sends SIGHUP with a malformed `config.toml` and asserts the daemon continues running on the old config (MCP `mailbox_list` still succeeds)
- [ ] `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt -- --check` clean

---

## Sprint 4 — Default templates + interactive setup + agent-setup hint (Days 7.5–10) [NOT STARTED]

**Goal:** Ship the 8 default templates inside the binary, surface them behind an interactive `aimx setup` checkbox, and make `aimx agent-setup` print the matching template-enable hint for each agent.

**Dependencies:** Sprint 1 (schema, service user); Sprint 3 (template hooks are actually creatable — otherwise the setup checkbox would install templates that are useless).

#### S4-1: Embed 8 default templates in the binary

**Context:** PRD §6.2 lists the eight default templates (`invoke-claude`, `invoke-codex`, `invoke-opencode`, `invoke-gemini`, `invoke-goose`, `invoke-openclaw`, `invoke-hermes`, `webhook`). They ship inside the `aimx` binary so `aimx setup` can write them without a separate asset download. A `hook-templates/defaults.toml` file at the repo root, embedded via `include_str!`, keeps them reviewable as plain TOML. Exact binary paths and argv follow PRD §6.2 verbatim; paths are confirmed by running the target agent's `--version` on a reference Linux install during implementation (one commit may adjust a path that drifted since PRD authoring).

**Priority:** P0

- [ ] New `hook-templates/defaults.toml` at the repo root contains eight `[[hook_template]]` blocks matching PRD §6.2; each has `name`, `description`, `cmd`, `params`, `stdin`, `run_as = "aimx-hook"`, `timeout_secs = 60`, `allowed_events = ["on_receive", "after_send"]`
- [ ] `src/setup.rs` (or a new `src/hook_templates_defaults.rs`) uses `include_str!("../hook-templates/defaults.toml")` to bake the TOML into the binary; a `default_templates() -> Vec<HookTemplate>` helper parses and returns it at runtime
- [ ] Parser failure on the embedded TOML panics the binary at startup (compile-time validation via a unit test ensures the embedded file is always valid before merge)
- [ ] Unit test enumerates each default template by name and asserts the `cmd[0]` path, `params` list, and `stdin` mode match PRD §6.2 — a forcing function against accidental drift
- [ ] `invoke-goose` is flagged with an open question in the implementation notes (PRD §11 Q3): does `stdin = "email"` work for Goose recipes, or do we need a `email_yaml` encoding? Sprint 4 ships with `stdin = "email"`; if manual testing reveals breakage, a follow-up story is added to the v2 backlog
- [ ] `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt -- --check` clean

#### S4-2: Interactive checkbox UI in `aimx setup`

**Context:** PRD §6.3 specifies the checkbox flow: after DKIM and systemd-unit setup, present a multi-select list of the eight default templates with all boxes unchecked by default. Re-running `aimx setup` pre-ticks currently-enabled templates. The existing setup UI is hand-rolled prompts; adding `dialoguer` gives us `MultiSelect` with minimal code. `dialoguer` is a mature crate (Rust ecosystem standard for this) and adds no async complexity.

**Priority:** P0

- [ ] `dialoguer = "0.11"` (or latest stable) added to `Cargo.toml` with the `fuzzy-select` feature for the `MultiSelect` widget; license check (MIT) documented in `CLAUDE.md` if the repo tracks dep licenses
- [ ] `src/setup.rs::run_setup` grows a new `configure_hook_templates(&Config, &dyn SystemOps) -> Result<Vec<String>>` phase called after DKIM setup, before systemd unit install
- [ ] The prompt prints the PRD §6.3 copy verbatim (one-line title, three-line explanation, then the checkbox list); defaults are "none checked" on a fresh install, "currently-enabled" on a re-run
- [ ] Selected templates are appended to `config.toml` via the same atomic-write helper used elsewhere; unselected but previously-enabled templates are removed (re-running setup is idempotent in both directions)
- [ ] The prompt is skippable with `--non-interactive` or when stdin is not a TTY (CI, piped setup); in that case no templates are installed and a warning is logged
- [ ] `MockSystemOps` in the test module simulates a TTY with a scripted selection so unit tests cover both the fresh and re-run cases
- [ ] `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt -- --check` clean

#### S4-3: `aimx hooks templates` CLI subcommand

**Context:** PRD §9 "In Scope (v1)" includes `aimx hooks templates` to list enabled templates (the enable/disable subcommands are deferred to v2). The subcommand reads `config.toml` directly (no UDS round-trip needed for a read-only query), matching the pattern used by `aimx hooks list`. Output is a compact table with `NAME`, `DESCRIPTION`, `PARAMS`, `EVENTS` columns, using the existing `term::header` / `term::highlight` color helpers.

**Priority:** P0

- [ ] `src/cli.rs::HookCommand` enum gains a `Templates` variant (no args)
- [ ] `src/hooks.rs::run` dispatches the new variant to a `list_templates(&Config) -> Result<()>` function that prints the table
- [ ] Output format matches `aimx hooks list`: 4-column aligned table with colored headers, truncated descriptions past 60 chars
- [ ] Empty-config output: "No hook templates enabled. Run `sudo aimx setup` and tick the templates you need."
- [ ] Unit test asserts the table renders correctly for a config with 0, 1, and 8 templates
- [ ] `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt -- --check` clean

#### S4-4: `aimx agent-setup` post-install template-enable hint

**Context:** PRD §6.4 adds a per-agent hint after `aimx agent-setup` completes: "To let <AGENT> create its own hooks via MCP, enable the matching template as root: `sudo aimx hooks template-enable invoke-<agent>`". The hint is printed unconditionally (agent-setup runs as the user and can't read root-owned `config.toml` to know if the template is already enabled). A static table in `src/agent_setup.rs` maps `agent.name → template.name` for the six agents that have a matching invoke- template; `openclaw`, `gemini`, `goose`, `hermes` each have their own invoke-template; `webhook` has no agent mapping (it's generic).

**Priority:** P1

- [ ] `src/agent_setup.rs::AgentSpec` gains an optional `matching_template: Option<&'static str>` field; populated as `Some("invoke-claude")` for the `claude-code` spec, `Some("invoke-codex")` for `codex`, etc.
- [ ] Post-install output in `install_plugin` appends a "Hook Templates" section when `matching_template` is `Some`, using the exact PRD §6.4 copy with the correct template name substituted
- [ ] Hint notes that the `template-enable` CLI is v2 (not in this track) — until then, operators tick the template during `aimx setup` or hand-edit `config.toml`; the hint updates in the v2 sprint that ships `template-enable`
- [ ] Unit tests verify the hint appears for every `AgentSpec` with `matching_template = Some(...)` and is absent for any spec with `matching_template = None`
- [ ] `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt -- --check` clean

---

## Sprint 5 — MCP tools + agent primer updates (Days 10–12.5) [NOT STARTED]

**Goal:** Expose templates through MCP with four new tools, and update the agent-facing primer so every bundled agent knows how to use them.

**Dependencies:** Sprint 3 (UDS verbs must be tightened); Sprint 4 (templates must be installable so MCP has something to list).

#### S5-1: MCP `hook_list_templates` tool

**Context:** PRD §6.6. No parameters. Reads the current config (via `Config::load_resolved_with_data_dir`) and returns a JSON array of templates with `name`, `description`, `params` (list of required param names), and `allowed_events`. The agent calls this first, before `hook_create`, to discover what it can wire up.

**Priority:** P0

- [ ] `src/mcp.rs` gains a `#[tool(name = "hook_list_templates", description = "List hook templates available on this install for use with hook_create")]` method on `AimxMcpServer`
- [ ] Return type is `Result<String, String>` — serialized JSON array; no params struct needed
- [ ] Empty-config case returns `[]` (valid JSON) with a companion explanation in the tool description: "empty means no templates are enabled; the operator must install them via aimx setup"
- [ ] Unit test calls the tool against a `Config` with 0 and 3 templates and asserts the serialized JSON matches a golden fixture
- [ ] `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt -- --check` clean

#### S5-2: MCP `hook_create` tool

**Context:** PRD §6.6. Takes `{mailbox, event, template, params, name?}`. Calls a new helper in `src/hook_client.rs` that submits the tightened UDS `HOOK-CREATE` verb. Returns the effective hook name (derived by the daemon if `name` was omitted) plus the substituted argv, so the agent's UI can echo "I created a hook on `accounts` that runs `/usr/local/bin/claude -p 'You are the accounts agent...'`" for confirmation.

**Priority:** P0

- [ ] New `HookCreateParams` struct with fields `mailbox: String`, `event: String` (`"on_receive"` or `"after_send"`), `template: String`, `params: BTreeMap<String, String>`, `name: Option<String>`; each annotated with `#[schemars(description = "...")]` for MCP schema generation
- [ ] `#[tool(name = "hook_create", description = "...")]` method wraps the UDS submit, surfaces daemon errors verbatim, and returns `{effective_name, substituted_argv}` on success
- [ ] Tool description highlights the safety model: "Your agent cannot submit arbitrary shell — every hook must reference a template listed by hook_list_templates; the operator installs templates during aimx setup"
- [ ] Integration test spins up a live daemon with one template configured, submits `hook_create` via the MCP transport, verifies the hook appears in `config.toml` with `origin = "mcp"`
- [ ] `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt -- --check` clean

#### S5-3: MCP `hook_list` tool with origin-masked output

**Context:** PRD §6.6 + §11 Q2. The agent sees all hooks (operator- and MCP-origin) so it has a full picture of the mailbox, but for operator-origin hooks only the `{name, mailbox, event, origin}` are exposed — `cmd` / `params` are masked. This prevents information leakage of operator-authored automation logic to the agent while still letting the agent avoid creating duplicate hooks.

**Priority:** P0

- [ ] `HookListParams { mailbox: Option<String> }` (filter optional)
- [ ] Tool method reads `Config` directly (no UDS) and returns a JSON array; for each hook, the fields are conditional on `origin`: MCP-origin hooks include `{name, mailbox, event, template, params, origin: "mcp"}`; operator-origin hooks include `{name, mailbox, event, origin: "operator"}` only
- [ ] Tool description explains the origin split so the agent understands why operator-origin hook contents are hidden
- [ ] Unit test against a config with mixed operator- and MCP-origin hooks asserts the JSON output matches the masking rule exactly
- [ ] `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt -- --check` clean

#### S5-4: MCP `hook_delete` tool

**Context:** PRD §6.6. Thin wrapper over the tightened UDS `HOOK-DELETE` verb from §S3-2. Takes `{name}`. Surfaces `ERR origin-protected` verbatim when the target hook is operator-origin, so the agent can tell the user "I can't delete that hook — the operator created it and would need to remove it via `sudo aimx hooks delete`".

**Priority:** P0

- [ ] `HookDeleteParams { name: String }`; tool method submits UDS `HOOK-DELETE` via existing `submit_hook_delete_via_daemon` helper
- [ ] Daemon error bodies surface verbatim to the MCP response; the tool description includes an example of `ERR origin-protected` so the agent understands the model
- [ ] Integration test: submit `hook_delete` against an MCP-origin hook (succeeds) and an operator-origin hook (fails with the expected error)
- [ ] `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt -- --check` clean

#### S5-5: Update agent primer + references with "Creating hooks" section

**Context:** PRD §8.3. `agents/common/aimx-primer.md` and `agents/common/references/` are the agent-facing docs embedded into every bundle via `include_dir!` at `src/agent_setup.rs:12`. Adding a new "Creating hooks" section to the primer — plus a new reference file `agents/common/references/hooks.md` covering the four tools, the template model, and example agent prompts — propagates to all agents automatically on the next `cargo build`.

**Priority:** P0

- [ ] `agents/common/aimx-primer.md` gains a "Creating hooks" section (after the existing MCP tool summary) explaining: hooks react to inbound/outbound mail; agents create hooks via the four new tools; the template model prevents arbitrary-shell abuse; always call `hook_list_templates` first
- [ ] New `agents/common/references/hooks.md` file with: full tool signatures, example agent prompts ("file mail from this sender + reply with system status"), the `origin` split explanation, the `ERR origin-protected` case, a troubleshooting subsection ("why does my hook not fire?" → check trust, template enabled, `aimx-hook` user exists)
- [ ] `agent-setup` bundle for each agent with `progressive_disclosure = true` (Claude Code, Codex, OpenClaw) picks up the new reference file automatically via `include_dir!`; bundles without progressive disclosure (Goose, Gemini, OpenCode) get only the primer updates — verify no build failures in the `assemble_plugin_files` paths
- [ ] Unit test on the bundled primer asserts the "Creating hooks" section is present for every agent (catches accidental deletion)
- [ ] `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt -- --check` clean

---

## Sprint 6 — Integration tests + book docs + doctor (Days 12.5–15) [NOT STARTED]

**Goal:** Close out the track with end-to-end integration tests, placeholder-substitution fuzz coverage, book chapter updates, and `aimx doctor` visibility for operators.

**Dependencies:** Sprints 1–5 (the feature is functional; this sprint is validation + documentation + operator visibility).

#### S6-1: End-to-end integration test (MCP → hook fire → sandbox verify)

**Context:** PRD §7 + §9 require test coverage for the full flow. The test spins up `aimx serve` in a tempdir, installs the `webhook` template into `config.toml`, connects an MCP client (in-process rmcp), calls `hook_list_templates` → `hook_create` with a fake webhook URL, ingests a fixture `.eml`, and asserts: (a) the hook fires, (b) the substituted argv matches expectations, (c) the subprocess ran as `aimx-hook` (verified via a log-line inspection since the test likely runs non-root on CI — full UID verification is root-gated).

**Priority:** P0

- [ ] New integration test `tests/integration.rs::hook_templates_end_to_end` that runs in a `tempdir` with `AIMX_DATA_DIR` override
- [ ] Test fixture: a one-template `config.toml` with `webhook` enabled, a single mailbox, and trust configured so on_receive fires on unsigned fixture mail
- [ ] Test spawns a tiny mock-curl binary (shell script in a tempdir, added to PATH) that records its argv and stdin to a file; `webhook` template's `cmd[0]` is pointed at the mock
- [ ] Assertion: after ingest, the mock-curl argv file contains the expected URL, and the stdin file contains JSON with the expected frontmatter fields
- [ ] Assertion: the hook-fire log line (captured via a test log subscriber) shows `run_as = "aimx-hook"` (even if the subprocess actually ran as the test user due to non-root, the log should reflect what the daemon attempted)
- [ ] Root-gated assertion (skipped when non-root): subprocess UID really is `aimx-hook` (verified by the mock-curl writing `id -u` to its output file)
- [ ] `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt -- --check` clean

#### S6-2: Placeholder substitution fuzz test

**Context:** PRD §10 risk table flags placeholder injection as the critical-severity risk. Sprint 2's unit tests cover the intentional edge cases; this sprint adds a property-based fuzz test that hammers the substitution function with 10K+ random inputs containing shell metacharacters, control chars, and long strings, asserting the core invariant: **no input produces more argv entries than the template declared**.

**Priority:** P0

- [ ] New test file `tests/hook_substitute_fuzz.rs` (or inside `src/hook_substitute.rs::tests`) using `proptest` or a simple hand-rolled loop
- [ ] Input generator produces params containing: empty strings, 8 KiB strings, `;`, `$(foo)`, backticks, pipes, ampersands, redirection chars, newlines, tabs, NUL bytes, high-bit Unicode, surrogate pairs
- [ ] For each input: call `substitute_argv` against a fixture 3-entry template; assert either (a) success with `substituted.len() == 3` OR (b) deterministic rejection via `SubstitutionError`
- [ ] 10_000 iterations per run; the test runs in `--release` mode in CI to keep wallclock reasonable
- [ ] `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt -- --check` clean

#### S6-3: Update book chapters for hook templates

**Context:** PRD §8.4 lists nine book chapters that need revisions. This story is large but mechanical — each chapter gets a focused edit matching the PRD's documentation scope. Book is built via `mdbook` (per `book/` dir structure); changes ship in the same release as the code.

**Priority:** P0

- [ ] `book/hooks.md` — rewritten opening to lead with templates; raw-cmd section retitled "Power-user hooks" and moved below the template section; explains the `origin` split and which flows use UDS vs. direct config edit
- [ ] `book/hook-recipes.md` — every recipe rewritten in template form; "Operator-only recipes" subsection preserves raw-cmd examples (e.g. complex shell pipelines that no template covers)
- [ ] `book/mcp.md` — four new tool entries for `hook_list_templates`, `hook_create`, `hook_list`, `hook_delete` with request/response examples
- [ ] `book/setup.md` — new "Hook Templates" subsection showing the interactive checkbox, explaining the `aimx-hook` user creation, noting re-run idempotence
- [ ] `book/agent-integration.md` — per-agent section gains the `sudo aimx hooks template-enable <name>` note (cross-referencing v2 caveat) and links to `mcp.md`
- [ ] `book/configuration.md` — new `[[hook_template]]` schema reference table; socket path updated from `/run/aimx/send.sock` to `/run/aimx/aimx.sock`
- [ ] `book/cli.md` — `aimx hooks create` documents `--template NAME` and `--param KEY=VAL`; new `aimx hooks templates` subcommand; socket path updated
- [ ] `book/troubleshooting.md` — new entries: "aimx-hook user missing", "template not found", "param validation failure", "sandbox denied reading stdin", "SIGHUP reload failed"
- [ ] `book/faq.md` — new Q/A: "Why can my agent create hooks but not run arbitrary commands?" explaining the template safety model in 3–4 paragraphs
- [ ] Book builds without warnings; any internal links updated; `mdbook build` clean
- [ ] `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt -- --check` clean

#### S6-4: `aimx doctor` extensions for hook templates

**Context:** PRD §7.3 asks `aimx doctor` to surface enabled templates with fire counts and warn on missing binaries. This gives the operator a single place to verify "is my template setup actually working" without reading `config.toml` + `journalctl` separately. Fire counts are parsed from the last 24h of structured log lines (`journalctl -u aimx --since="24 hours ago" --output=json` on systemd; log-file scan on OpenRC).

**Priority:** P1

- [ ] `src/doctor.rs` gains a "Hook templates" section that lists every enabled template with: name, description (truncated), `cmd[0]` existence check (warn if not executable), 24h fire count, 24h failure count
- [ ] Section also verifies the `aimx-hook` user exists (UID lookup), reports UID/GID, and checks read access to `<datadir>/inbox` and `<datadir>/sent` (warn if missing)
- [ ] Fire-count parser tolerates journalctl unavailability (OpenRC) by falling back to a log-file scan; if neither is available, count shows `-` instead of failing the whole report
- [ ] Unit tests with fixture log lines verify the count parser handles malformed, truncated, and mixed-origin log lines without panicking
- [ ] `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt -- --check` clean

---

## Summary table

| Sprint | Days | Focus | Status | Key Output |
|--------|------|-------|--------|------------|
| 1 | 1–2.5 | Foundation | Done (PR #111) | Socket renamed, schema lands, `aimx-hook` user created |
| 2 | 2.5–5 | Sandboxed executor | Done (PR #112) | argv + systemd-run/setuid + timeout runner replaces `sh -c` |
| 3 | 5–7.5 | UDS + CLI + SIGHUP | In Progress | `HOOK-CREATE` template-only, raw-cmd via config write, hot-reload works |
| 4 | 7.5–10 | Templates + setup | Not Started | 8 defaults embedded, interactive checkbox, `agent-setup` hint |
| 5 | 10–12.5 | MCP tools + primer | Not Started | Four MCP tools live, `agents/common/` updated |
| 6 | 12.5–15 | Tests + docs + doctor | Not Started | E2E + fuzz tests green, 9 book chapters updated, `aimx doctor` surfaces templates |

## Deferred to v2

Taken directly from [`hook-templates-prd.md`](hook-templates-prd.md) §9 "Out of Scope (future consideration)":

| Feature | Rationale |
|---------|-----------|
| `admin.sock` (root-only UDS) | Not needed for hook templates — the template-only `HOOK-CREATE` verb provides the authorization boundary. Add if other privileged verbs appear later (DKIM rotate, TLS cert swap, etc.) |
| `aimx hooks template-enable` / `template-disable` CLI | v1 requires re-running `aimx setup` to toggle templates; dedicated CLI commands would be friendlier but aren't required for the core flow |
| Per-template `allowed_mailboxes` restriction | Useful for locking a sensitive template (e.g. `webhook` with prod secrets) to one mailbox; waiting for a real operator request before shipping |
| Dry-run / test mode (`aimx hooks test <name>`) | Would fire a template against a synthetic email without delivering it. Nice DX but not required for the launch |
| Per-user / multi-tenant hook isolation | aimx is single-operator by stance; multi-user Unix is explicitly out of scope |
| Operator-authored custom templates (documented path) | The schema already accepts them; v1 leaves this undocumented until validation is hardened against hostile operator input (unlikely attack surface, but belt-and-braces) |
| `email_json` stdin format stabilization | v1 treats it as best-effort frontmatter JSON; breaking changes allowed until a real consumer emerges |

---

*Status: Sprints 1–2 merged (PRs #111, #112; 2026-04-20). Sprint 3 — UDS protocol + CLI wiring + SIGHUP reload — is now active.*

---

## Non-blocking Review Backlog

This section collects non-blocking feedback from sprint reviews. Questions need human answers (edit inline). Improvements accumulate until triaged into a cleanup sprint.

### Questions

_None outstanding._

### Improvements

- [x] **(Sprint 1)** `Hook::resolve_argv` deferred from S1-3 to S2-1 because `hook_substitute.rs` doesn't exist until Sprint 2. AC moved, back-reference left in S1-3, `Hook::is_template_bound()` marked `#[allow(dead_code)]` pending Sprint 2 consumer. _Resolved by re-scoping in the Sprint 1 fix commit._
- [ ] **(Sprint 1)** `validate_hooks` error-message ordering: for an MCP-origin `after_send` hook with `dangerously_support_untrusted = true`, the event-mismatch check fires before the MCP-origin check, so the rejection wording mentions the wrong invariant. Both are valid rejections — only the message text differs. Nice-to-have polish. *(from Sprint 1 re-review nit)*
- [ ] **(Sprint 2)** `spawn_via_systemd_run` preflight + fallback path emit two WARN lines on the same unknown-user + non-root event (one from the systemd preflight, one from the fallback delegation). Cosmetic, dev-box-only. Collapse to a single log line by either (a) silencing the systemd-path warn when it delegates, or (b) silencing the fallback warn when the systemd preflight already logged. *(from Sprint 2 re-review nit)*
- [ ] **(Sprint 2)** `AIMX_SANDBOX_FORCE_FALLBACK` env var is test-only scaffolding but lives in production `src/platform.rs`. Consider gating it behind `#[cfg(any(test, debug_assertions))]` or at least documenting it more explicitly so an operator who stumbles on it doesn't think it's a supported switch. *(from Sprint 2 implementation note)*
- [ ] **(Sprint 2)** Unknown-user fallback for non-root callers (warns and runs as current UID) is a dev/CI safety net that production should never hit. Add a `doctor` check that flags this path if it ever fires in production logs. *(from Sprint 2 implementation note — track via `aimx doctor` work in Sprint 6)*
