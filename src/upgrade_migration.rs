//! One-shot upgrade migration from the v1 (single-domain) on-disk
//! layout to the v2 (multi-domain) canonical layout.
//!
//! Runs at daemon startup, before the SMTP listener spawns and before
//! the UDS binds. The migration is **atomic per step** and **idempotent**
//! across restarts: re-running with the `.layout-version: 2` marker
//! present is a fast no-op; re-running after a crash mid-flow detects
//! which steps already completed and resumes from the first incomplete
//! one.
//!
//! # Step order (load-bearing)
//!
//! 1. **Storage rename** — `<data_dir>/inbox` → `<data_dir>/<domain>/inbox/`
//!    and the same for `sent/`. `rename(2)` is atomic on the same
//!    filesystem and constant-time regardless of how much mail is
//!    stored.
//! 2. **DKIM rename** — create `<dkim_dir>/<domain>/` (mode `0700`)
//!    and move `private.key` + `public.key` into it.
//! 3. **Config rewrite** — `domain = "x.com"` → `domains = ["x.com"]`
//!    and `[mailboxes.<local>]` → `[mailboxes."<local>@<domain>"]`,
//!    written via the existing [`crate::config::write_atomic`] helper.
//! 4. **Marker write** — `<data_dir>/.layout-version` containing `2\n`.
//!
//! Why this order? Per PRD: prefer "DKIM key exists but domain not in
//! config (orphaned key, harmless)" over "domain in config but DKIM key
//! missing (broken outbound)". A crash between step 1 and step 2 leaves
//! the install detectable as half-migrated and resumable; a crash
//! between step 2 and step 3 leaves an orphaned per-domain DKIM
//! directory that the next run absorbs cleanly. The marker is last so
//! a partial run never claims to be done.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::config::{Config, write_atomic};

/// On-disk marker filename written under `<data_dir>/`. Presence with
/// value `"2"` short-circuits the migration on every subsequent boot.
pub const LAYOUT_MARKER_FILENAME: &str = ".layout-version";

/// Current on-disk layout version. Bumped only when a new structural
/// migration ships — `"2"` is the multi-domain layout.
pub const CURRENT_LAYOUT_VERSION: &str = "2";

/// Indicators that fired during v1-shape detection. Carried by
/// [`LayoutState::NeedsMigration`] so the caller can log exactly why a
/// migration is about to run.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Indicators {
    /// `<data_dir>/inbox/` exists and `<data_dir>/<default_domain>/inbox/`
    /// does not.
    pub legacy_inbox_dir: bool,
    /// `<data_dir>/sent/` exists and `<data_dir>/<default_domain>/sent/`
    /// does not. (Not strictly required to trigger migration on its own,
    /// but recorded for the log line.)
    pub legacy_sent_dir: bool,
    /// `<dkim_dir>/private.key` exists and
    /// `<dkim_dir>/<default_domain>/private.key` does not.
    pub legacy_dkim_key: bool,
    /// `config.toml` carries `domain = "..."` without `domains = [...]`.
    pub legacy_config_domain_field: bool,
    /// `[mailboxes.<local>]` keys exist without an `@` in the key.
    pub legacy_mailbox_local_part_keys: bool,
}

impl Indicators {
    /// True when at least one v1-shape signal fired.
    pub fn any(&self) -> bool {
        self.legacy_inbox_dir
            || self.legacy_sent_dir
            || self.legacy_dkim_key
            || self.legacy_config_domain_field
            || self.legacy_mailbox_local_part_keys
    }
}

/// Tri-state result of [`detect_layout_state`].
///
/// - [`Self::Migrated`] — `.layout-version: 2` present. Fast no-op.
/// - [`Self::NeedsMigration`] — at least one v1 indicator fired and
///   no marker is present. Run the migration.
/// - [`Self::FreshInstall`] — no marker and no v1 indicators. Write
///   the marker proactively so future restarts short-circuit.
/// - [`Self::Corrupted`] — marker file present with garbage / wrong
///   version. Hard startup error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LayoutState {
    Migrated,
    NeedsMigration(Indicators),
    FreshInstall,
    Corrupted(String),
}

/// Migration error type, surfaced by every step. Carries a human-
/// readable reason that bubbles into the daemon's startup hard-fail
/// message verbatim, with a pointer at `book/multi-domain.md`.
#[derive(Debug)]
pub enum MigrationError {
    /// Reading or writing a file failed.
    Io { path: PathBuf, cause: io::Error },
    /// `std::fs::rename` returned EXDEV (cross-filesystem). We do not
    /// fall back to copy+delete — atomicity matters more than
    /// convenience here.
    CrossDevice { src: PathBuf, dst: PathBuf },
    /// Generic structural failure (e.g. parent dir of marker is missing).
    Other(String),
}

impl std::fmt::Display for MigrationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io { path, cause } => write!(f, "{}: {cause}", path.display()),
            Self::CrossDevice { src, dst } => write!(
                f,
                "{src} and {dst} must be on the same filesystem; \
                 see book/multi-domain.md for manual recovery",
                src = src.display(),
                dst = dst.display()
            ),
            Self::Other(s) => f.write_str(s),
        }
    }
}

impl std::error::Error for MigrationError {}

/// Detect the on-disk layout state for an install rooted at
/// `data_dir` / `dkim_dir` / `config_path`. The decision tree:
///
/// 1. If `.layout-version` exists under `data_dir`:
///    - content trims to `"2"` → [`LayoutState::Migrated`].
///    - any other content → [`LayoutState::Corrupted`].
///    - read error (other than NotFound) → [`LayoutState::Corrupted`].
/// 2. Otherwise scan for v1-shape indicators. If any fire,
///    [`LayoutState::NeedsMigration(indicators)`].
/// 3. Otherwise [`LayoutState::FreshInstall`].
///
/// `default_domain` is the `domains[0]` of the loaded `Config`; the
/// detector uses it to compute the "destination" paths whose presence
/// would mean a step already ran (e.g. `<data_dir>/<default_domain>/inbox/`).
///
/// Pure function over the filesystem state; no writes, no locks.
pub fn detect_layout_state(
    data_dir: &Path,
    dkim_dir: &Path,
    config_path: &Path,
    default_domain: &str,
) -> LayoutState {
    // 1. Marker is the source of truth.
    let marker = data_dir.join(LAYOUT_MARKER_FILENAME);
    match fs::read_to_string(&marker) {
        Ok(s) => {
            let trimmed = s.trim();
            if trimmed == CURRENT_LAYOUT_VERSION {
                return LayoutState::Migrated;
            }
            return LayoutState::Corrupted(format!(
                "{} contains '{trimmed}', expected '{}'",
                marker.display(),
                CURRENT_LAYOUT_VERSION,
            ));
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            // Fall through to v1-shape detection.
        }
        Err(e) => {
            return LayoutState::Corrupted(format!("cannot read {}: {e}", marker.display()));
        }
    }

    // 2. V1-shape indicators.
    let mut ind = Indicators::default();

    let legacy_inbox = data_dir.join("inbox");
    let new_inbox = data_dir.join(default_domain).join("inbox");
    if legacy_inbox.is_dir() && !new_inbox.is_dir() {
        ind.legacy_inbox_dir = true;
    }
    let legacy_sent = data_dir.join("sent");
    let new_sent = data_dir.join(default_domain).join("sent");
    if legacy_sent.is_dir() && !new_sent.is_dir() {
        ind.legacy_sent_dir = true;
    }
    let legacy_dkim = dkim_dir.join("private.key");
    let new_dkim = dkim_dir.join(default_domain).join("private.key");
    if legacy_dkim.is_file() && !new_dkim.is_file() {
        ind.legacy_dkim_key = true;
    }
    if let Ok(content) = fs::read_to_string(config_path) {
        let (has_legacy_field, has_local_keys) = inspect_config_for_legacy(&content);
        if has_legacy_field {
            ind.legacy_config_domain_field = true;
        }
        if has_local_keys {
            ind.legacy_mailbox_local_part_keys = true;
        }
    }

    if ind.any() {
        return LayoutState::NeedsMigration(ind);
    }

    LayoutState::FreshInstall
}

/// Scan a raw `config.toml` for the two legacy markers that trigger
/// migration: a top-level `domain = "..."` field, and any `[mailboxes.X]`
/// header whose `X` lacks an `@`. Robust against blank lines, comments,
/// and either quoted-or-bareword TOML headers.
///
/// Returns `(has_legacy_domain_field, has_legacy_local_part_keys)`.
fn inspect_config_for_legacy(content: &str) -> (bool, bool) {
    let mut has_legacy_field = false;
    let mut has_canonical_field = false;
    let mut has_local_keys = false;

    for raw_line in content.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        // Once we cross into a `[...]` section, only mailbox table
        // headers matter for the local-key heuristic; the legacy
        // top-level scalar `domain = "..."` only appears at the root.
        // Cheap to scan the full file because TOML files are tiny.
        // Match `domain = "x.com"` (bare scalar) but NOT
        // `[domain."b.com"]` (the per-domain sub-table header, which
        // would have started with `[`).
        if let Some(rest) = line.strip_prefix("domain")
            && rest.trim_start().starts_with('=')
        {
            has_legacy_field = true;
        }
        if let Some(rest) = line.strip_prefix("domains")
            && rest.trim_start().starts_with('=')
        {
            has_canonical_field = true;
        }
        if let Some(header) = line.strip_prefix("[mailboxes.") {
            // Strip the trailing `]`. Header is one of:
            //   [mailboxes.info]            -> "info]"
            //   [mailboxes."info@a.com"]    -> "\"info@a.com\"]"
            let inner = header.trim_end_matches(']').trim();
            // Drop surrounding quotes if present.
            let unquoted = inner.trim_matches('"');
            if !unquoted.contains('@') {
                has_local_keys = true;
            }
        }
    }

    // The `domain = "..."` field is a legacy signal only when the
    // canonical `domains = [...]` form is absent. If both are present
    // the loader itself rejects this mix, so we never
    // see that state at startup; defensively avoid double-reporting.
    if has_canonical_field {
        has_legacy_field = false;
    }
    (has_legacy_field, has_local_keys)
}

/// Rename the legacy storage tree into the per-domain layout.
///
/// Idempotent: when the destination exists and the source does not,
/// the rename is skipped. Catches EXDEV / `CrossesDevices` and surfaces
/// [`MigrationError::CrossDevice`] with an actionable hint — atomic
/// renames are the only correct primitive here.
///
/// Runs steps 1 and 2 of the storage half of the migration: `inbox`
/// then `sent` (independent of each other, but both must complete for
/// the install to be on the v2 layout).
///
/// The per-domain storage dir is created (or chmodded) to `0o755` so
/// non-root mailbox owners can traverse into their own `inbox/<name>/`
/// subdir, exactly as they could under `<data_dir>/` on v1. The daemon
/// runs with `umask 0o077`, so without this explicit chmod the dir
/// would land at `0o700` and every non-root MCP read would surface
/// EACCES. The `inbox/<name>/` subdirs themselves remain `0o700` and
/// owner-locked; only the per-domain traversal bit is opened.
pub fn relocate_storage_for_default_domain(
    data_dir: &Path,
    default_domain: &str,
) -> Result<StorageRelocationReport, MigrationError> {
    let domain_dir = data_dir.join(default_domain);
    if !domain_dir.exists() {
        fs::create_dir_all(&domain_dir).map_err(|e| MigrationError::Io {
            path: domain_dir.clone(),
            cause: e,
        })?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = fs::Permissions::from_mode(0o755);
        fs::set_permissions(&domain_dir, perms).map_err(|e| MigrationError::Io {
            path: domain_dir.clone(),
            cause: e,
        })?;
    }

    let inbox_move = move_if_pending(&data_dir.join("inbox"), &domain_dir.join("inbox"))?;
    let sent_move = move_if_pending(&data_dir.join("sent"), &domain_dir.join("sent"))?;

    Ok(StorageRelocationReport {
        inbox: inbox_move,
        sent: sent_move,
    })
}

/// Report from [`relocate_storage_for_default_domain`].
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct StorageRelocationReport {
    pub inbox: MoveOutcome,
    pub sent: MoveOutcome,
}

/// Report from [`relocate_dkim_for_default_domain`].
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DkimRelocationReport {
    pub private_key: MoveOutcome,
    pub public_key: MoveOutcome,
    /// True iff the per-domain DKIM dir was newly created during this
    /// call. Informational; carried into the log line for operators.
    pub created_domain_dir: bool,
}

/// Outcome of a single rename attempt — useful for logging exactly
/// which sub-step ran on a given migration call.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum MoveOutcome {
    /// Source exists and was renamed onto the destination.
    Renamed { from: PathBuf, to: PathBuf },
    /// Destination already exists and source is absent — already done.
    AlreadyDone,
    /// Neither source nor destination exists. Counts as a no-op for
    /// idempotency: e.g. a fresh single-domain install that never had a
    /// `public.key` separately on disk, or a `sent/` directory that
    /// existed only after the first outbound send.
    #[default]
    NothingToDo,
}

/// Rename the legacy DKIM keys into a per-domain subdirectory.
///
/// Creates `<dkim_dir>/<default_domain>/` with mode `0700` if absent,
/// then renames `private.key` and `public.key`. Idempotent per file.
/// EXDEV produces [`MigrationError::CrossDevice`] just like storage
/// relocation.
pub fn relocate_dkim_for_default_domain(
    dkim_dir: &Path,
    default_domain: &str,
) -> Result<DkimRelocationReport, MigrationError> {
    let domain_dir = dkim_dir.join(default_domain);
    let created_domain_dir = !domain_dir.exists();
    if created_domain_dir {
        fs::create_dir_all(&domain_dir).map_err(|e| MigrationError::Io {
            path: domain_dir.clone(),
            cause: e,
        })?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let perms = fs::Permissions::from_mode(0o700);
            fs::set_permissions(&domain_dir, perms).map_err(|e| MigrationError::Io {
                path: domain_dir.clone(),
                cause: e,
            })?;
        }
    }

    let private_key = move_if_pending(
        &dkim_dir.join("private.key"),
        &domain_dir.join("private.key"),
    )?;
    let public_key = move_if_pending(&dkim_dir.join("public.key"), &domain_dir.join("public.key"))?;

    Ok(DkimRelocationReport {
        private_key,
        public_key,
        created_domain_dir,
    })
}

/// Move `src` onto `dst` exactly once, idempotently. The contract:
///
/// - source exists, destination absent → `rename(src, dst)`,
///   [`MoveOutcome::Renamed`].
/// - source absent, destination present → no-op, [`MoveOutcome::AlreadyDone`].
/// - source absent, destination absent → no-op, [`MoveOutcome::NothingToDo`].
/// - source present, destination present → defensive error: we do
///   **not** overwrite, because a re-run after a partial failure
///   should never silently clobber a half-migrated tree. Surfaces as
///   [`MigrationError::Other`].
/// - EXDEV → [`MigrationError::CrossDevice`].
fn move_if_pending(src: &Path, dst: &Path) -> Result<MoveOutcome, MigrationError> {
    let src_exists = src.exists();
    let dst_exists = dst.exists();
    match (src_exists, dst_exists) {
        (false, false) => Ok(MoveOutcome::NothingToDo),
        (false, true) => Ok(MoveOutcome::AlreadyDone),
        (true, true) => Err(MigrationError::Other(format!(
            "both {} and {} exist; refusing to overwrite — \
             see book/multi-domain.md for manual recovery",
            src.display(),
            dst.display(),
        ))),
        (true, false) => {
            // Make sure the destination's parent exists. `relocate_*`
            // create the per-domain dir itself; this is defense in depth.
            if let Some(parent) = dst.parent()
                && !parent.exists()
            {
                fs::create_dir_all(parent).map_err(|e| MigrationError::Io {
                    path: parent.to_path_buf(),
                    cause: e,
                })?;
            }
            match fs::rename(src, dst) {
                Ok(()) => Ok(MoveOutcome::Renamed {
                    from: src.to_path_buf(),
                    to: dst.to_path_buf(),
                }),
                Err(e) if is_cross_device_error(&e) => Err(MigrationError::CrossDevice {
                    src: src.to_path_buf(),
                    dst: dst.to_path_buf(),
                }),
                Err(e) => Err(MigrationError::Io {
                    path: src.to_path_buf(),
                    cause: e,
                }),
            }
        }
    }
}

/// Recognize EXDEV across libc / `io::Error` variants. The stable
/// [`io::ErrorKind::CrossesDevices`] only landed in 1.85; the older
/// raw OS error is `18` on Linux. We accept either so a build on an
/// older toolchain still surfaces the actionable error message.
fn is_cross_device_error(e: &io::Error) -> bool {
    if e.kind() == io::ErrorKind::CrossesDevices {
        return true;
    }
    e.raw_os_error() == Some(18)
}

/// Rewrite `config.toml` into the canonical multi-domain shape on disk.
///
/// What this step normalizes on-disk:
///
/// - `domain = "x.com"` becomes `domains = ["x.com"]` (legacy single-
///   domain field is gone from the serialized output).
/// - Per-domain sub-tables (operator-written `[domain."b.com"]`)
///   round-trip through the serializer unchanged.
///
/// What this step **does not** rewrite on-disk:
///
/// - Legacy local-part-keyed mailboxes (`[mailboxes.info]`). The
///   serializer preserves the operator-friendly key the loader carried
///   in memory so the runtime data plane and every downstream CLI that
///   looks up mailboxes by `<local>` (ingest, send, hooks create /
///   delete, mailboxes show) keeps working unchanged. The on-disk
///   FQDN re-key (`[mailboxes."<local>@<domain>"]`) is performed later
///   as part of the runtime data plane rewire that teaches every
///   callsite to look up mailboxes by FQDN.
///
/// The returned `Config` is the input unchanged — the in-memory shape
/// is preserved across the migration for the same reason.
///
/// Idempotent: when the input `Config` is already in canonical shape
/// (no legacy `domain` field, all mailboxes already FQDN-keyed), the
/// function still writes the canonical TOML serialization on disk.
/// This is intentional — re-writing the same logical content is cheap
/// and guarantees the final disk shape matches the canonical
/// serializer on every run.
pub fn rewrite_config_to_canonical_shape(
    config_path: &Path,
    in_memory_config: &Config,
) -> Result<Config, MigrationError> {
    write_atomic(config_path, in_memory_config).map_err(|e| MigrationError::Io {
        path: config_path.to_path_buf(),
        cause: e,
    })?;
    Ok(in_memory_config.clone())
}

/// Write `<data_dir>/.layout-version` containing `"2\n"`, mode `0644`.
///
/// Idempotent: an existing marker with the correct value is left
/// untouched. An existing marker with a wrong value is overwritten
/// only by the migration codepath (callers detecting the wrong value
/// must surface [`LayoutState::Corrupted`] first, which is a startup
/// hard error — they should not silently rewrite). The function
/// itself is unconditional: callers gate via [`detect_layout_state`].
pub fn write_layout_version_marker(data_dir: &Path) -> Result<(), MigrationError> {
    let marker = data_dir.join(LAYOUT_MARKER_FILENAME);
    let body = format!("{CURRENT_LAYOUT_VERSION}\n");
    // Write via temp-then-rename so a concurrent reader never sees a
    // truncated value. The marker is tiny but consistency is cheap.
    let parent = marker.parent().unwrap_or(Path::new("."));
    if !parent.exists() {
        fs::create_dir_all(parent).map_err(|e| MigrationError::Io {
            path: parent.to_path_buf(),
            cause: e,
        })?;
    }
    let tmp = parent.join(format!(
        ".{LAYOUT_MARKER_FILENAME}.tmp.{}",
        std::process::id()
    ));
    {
        use std::io::Write;
        let mut f = fs::File::create(&tmp).map_err(|e| MigrationError::Io {
            path: tmp.clone(),
            cause: e,
        })?;
        f.write_all(body.as_bytes())
            .map_err(|e| MigrationError::Io {
                path: tmp.clone(),
                cause: e,
            })?;
        f.sync_all().map_err(|e| MigrationError::Io {
            path: tmp.clone(),
            cause: e,
        })?;
    }
    fs::rename(&tmp, &marker).map_err(|e| {
        let _ = fs::remove_file(&tmp);
        MigrationError::Io {
            path: marker.clone(),
            cause: e,
        }
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = fs::Permissions::from_mode(0o644);
        fs::set_permissions(&marker, perms).map_err(|e| MigrationError::Io {
            path: marker.clone(),
            cause: e,
        })?;
    }
    Ok(())
}

/// Aggregate report from a successful migration. Carried into the
/// single operator-visible INFO log line so journalctl shows every
/// path that moved.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MigrationReport {
    pub storage: StorageRelocationReport,
    pub dkim: DkimRelocationReport,
    pub config_path: PathBuf,
    pub config_rewritten: bool,
    pub marker_written: bool,
}

/// Run the full migration in order: storage → DKIM → config → marker.
///
/// Each step is independently idempotent. The function returns a
/// [`MigrationReport`] on success and a [`MigrationError`] on the
/// first failure. The caller (`serve.rs`) is responsible for the lock
/// hierarchy (`CONFIG_WRITE_LOCK` outer, every per-mailbox lock inner
/// in sorted FQDN order) — this function only does file work.
pub fn run_migration(
    data_dir: &Path,
    dkim_dir: &Path,
    config_path: &Path,
    in_memory_config: &Config,
) -> Result<(MigrationReport, Config), MigrationError> {
    let default_domain = in_memory_config.default_domain();

    let storage = relocate_storage_for_default_domain(data_dir, default_domain)?;
    let dkim = relocate_dkim_for_default_domain(dkim_dir, default_domain)?;
    let rewritten = rewrite_config_to_canonical_shape(config_path, in_memory_config)?;
    write_layout_version_marker(data_dir)?;

    Ok((
        MigrationReport {
            storage,
            dkim,
            config_path: config_path.to_path_buf(),
            config_rewritten: true,
            marker_written: true,
        },
        rewritten,
    ))
}

/// Outcome of [`run_startup_migration`] surfaced back to `serve.rs`.
///
/// Carries enough information for the daemon to log the right line and
/// (when migration ran) swap the freshly-rewritten `Config` into the
/// `ConfigHandle`. Reload is the caller's job — this module deliberately
/// stays free of the `ConfigHandle` dependency so the unit tests can
/// exercise the full path without spinning up a daemon-level fixture.
///
/// The `Migrated` payload is boxed so the enum stays small on the
/// stack — `MigrationReport` carries `PathBuf`s, `Config` carries a
/// full mailbox map, and we hand the outcome back through several
/// pattern-match sites where a 240+-byte variant would dominate the
/// rest of the API.
#[derive(Debug)]
pub enum StartupMigrationOutcome {
    /// `.layout-version: 2` was already present. Fast path; the daemon
    /// continues with the `Config` it was started with.
    AlreadyMigrated,
    /// No v1 indicators and no marker — the daemon is on a fresh
    /// install. The marker was written proactively so subsequent
    /// restarts take [`Self::AlreadyMigrated`]. No INFO log line.
    Fresh,
    /// Migration ran end-to-end. Carries the rewritten `Config`
    /// (in-memory shape preserved from the legacy load; the on-disk shape is
    /// canonical FQDN) for the caller to swap into the live
    /// `ConfigHandle`.
    Migrated(Box<MigratedOutcome>),
}

/// Heap-allocated payload for [`StartupMigrationOutcome::Migrated`].
#[derive(Debug)]
pub struct MigratedOutcome {
    pub report: MigrationReport,
    pub rewritten: Config,
}

/// Drive the full startup migration flow against a loaded `Config`.
///
/// Sequence:
/// 1. [`detect_layout_state`] determines whether this is `Migrated`,
///    `FreshInstall`, `NeedsMigration`, or `Corrupted`.
/// 2. `Migrated` → return immediately (no locks, no log).
/// 3. `Corrupted` → return the error verbatim.
/// 4. `FreshInstall` → write the marker, no log.
/// 5. `NeedsMigration` → acquire the lock hierarchy outer-to-inner
///    (CONFIG_WRITE_LOCK held by the caller per the contract on
///    [`run_migration`]; per-mailbox locks in sorted FQDN order are
///    deferred to the caller too because [`crate::mailbox_locks::MailboxLocks`]
///    is owned by `serve.rs`). Run [`run_migration`].
///
/// **The lock hierarchy is enforced by the caller** so this module
/// stays free of the tokio-async dependency surface. `serve.rs` takes
/// the locks, calls this function inside the critical section, and
/// emits the operator-visible INFO log on return.
pub fn run_startup_migration(
    data_dir: &Path,
    dkim_dir: &Path,
    config_path: &Path,
    in_memory_config: &Config,
) -> Result<StartupMigrationOutcome, StartupMigrationError> {
    let default_domain = in_memory_config.default_domain();
    match detect_layout_state(data_dir, dkim_dir, config_path, default_domain) {
        LayoutState::Migrated => Ok(StartupMigrationOutcome::AlreadyMigrated),
        LayoutState::Corrupted(msg) => Err(StartupMigrationError::Corrupted(msg)),
        LayoutState::FreshInstall => {
            write_layout_version_marker(data_dir).map_err(StartupMigrationError::Migration)?;
            Ok(StartupMigrationOutcome::Fresh)
        }
        LayoutState::NeedsMigration(_indicators) => {
            let (report, rewritten) =
                run_migration(data_dir, dkim_dir, config_path, in_memory_config)
                    .map_err(StartupMigrationError::Migration)?;
            Ok(StartupMigrationOutcome::Migrated(Box::new(
                MigratedOutcome { report, rewritten },
            )))
        }
    }
}

/// Startup-time wrapper around [`MigrationError`] that also surfaces
/// [`LayoutState::Corrupted`] (a marker-file integrity error rather
/// than a filesystem error). Both variants flow into the canonical
/// `serve.rs` hard-fail message.
#[derive(Debug)]
pub enum StartupMigrationError {
    Migration(MigrationError),
    Corrupted(String),
}

impl std::fmt::Display for StartupMigrationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Migration(e) => write!(f, "{e}"),
            Self::Corrupted(s) => f.write_str(s),
        }
    }
}

impl std::error::Error for StartupMigrationError {}

/// Resolve the directory containing the active DKIM keypair given a
/// loaded `Config` and the canonical `<dkim_dir>` root.
///
/// The migration ships the on-disk relocation but a follow-up rewire
/// will replace this with a per-domain `HashMap` lookup keyed on the
/// From: domain. Bridge until then: if
/// `<dkim_dir>/<default_domain>/private.key` exists, return the
/// per-domain dir (post-migration shape, and the shape every
/// multi-domain build will eventually require); otherwise return
/// `<dkim_dir>` itself (fresh installs that just ran `aimx setup` keep
/// their keys at the legacy root until the per-domain DKIM loader and
/// the `aimx dkim-keygen --domain <d>` flag land).
///
/// Returning a single `PathBuf` lets `serve.rs` stay decoupled from
/// the structural change.
pub fn resolve_active_dkim_dir(config: &Config, dkim_dir: &Path) -> PathBuf {
    let per_domain = dkim_dir.join(config.default_domain());
    if per_domain.join("private.key").is_file() {
        return per_domain;
    }
    dkim_dir.to_path_buf()
}

/// Format the single operator-visible INFO log line emitted by the
/// daemon after a successful migration. Pulled out so the wording can
/// be pinned in tests without reaching into the `tracing` subscriber.
pub fn format_migration_log_line(report: &MigrationReport, rewritten_default: &str) -> String {
    let mut parts: Vec<String> = Vec::new();
    parts.push(format!("default_domain={rewritten_default}"));
    if let MoveOutcome::Renamed { from, to } = &report.storage.inbox {
        parts.push(format!("inbox={}→{}", from.display(), to.display()));
    }
    if let MoveOutcome::Renamed { from, to } = &report.storage.sent {
        parts.push(format!("sent={}→{}", from.display(), to.display()));
    }
    if let MoveOutcome::Renamed { from, to } = &report.dkim.private_key {
        parts.push(format!("dkim_private={}→{}", from.display(), to.display()));
    }
    if let MoveOutcome::Renamed { from, to } = &report.dkim.public_key {
        parts.push(format!("dkim_public={}→{}", from.display(), to.display()));
    }
    if report.config_rewritten {
        parts.push(format!("config_rewritten={}", report.config_path.display()));
    }
    if report.marker_written {
        parts.push(format!("layout_version={CURRENT_LAYOUT_VERSION}"));
    }
    format!(
        "upgrade migration completed: {}; see book/multi-domain.md",
        parts.join(" ")
    )
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::Mutex;
    use tempfile::TempDir;

    /// Serialize tests that mutate the process umask. `umask(2)` is
    /// thread-unsafe and cargo runs tests in parallel.
    static UMASK_SERIALIZE: Mutex<()> = Mutex::new(());

    /// RAII guard that sets the process umask and restores the previous
    /// value on drop. Holds [`UMASK_SERIALIZE`] for the whole lifetime.
    struct UmaskGuard {
        prev: u32,
        _lock: std::sync::MutexGuard<'static, ()>,
    }

    impl UmaskGuard {
        fn set(new: u32) -> Self {
            let lock = UMASK_SERIALIZE.lock().unwrap_or_else(|p| p.into_inner());
            // SAFETY: umask(2) is thread-unsafe but the mutex above
            // serializes every caller in the test binary.
            let prev = unsafe { libc::umask(new as libc::mode_t) } as u32;
            Self { prev, _lock: lock }
        }
    }

    impl Drop for UmaskGuard {
        fn drop(&mut self) {
            unsafe {
                libc::umask(self.prev as libc::mode_t);
            }
        }
    }

    fn touch(path: &Path) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, b"").unwrap();
    }

    fn touch_with(path: &Path, body: &[u8]) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, body).unwrap();
    }

    fn write_legacy_config(path: &Path, domain: &str, mailbox_locals: &[&str]) {
        let mut s = format!("domain = \"{domain}\"\n\n");
        for local in mailbox_locals {
            s.push_str(&format!(
                "[mailboxes.{local}]\naddress = \"{local}@{domain}\"\nowner = \"ops\"\n\n",
            ));
        }
        fs::write(path, s).unwrap();
    }

    fn write_canonical_config(path: &Path, domain: &str, mailbox_locals: &[&str]) {
        let mut s = format!("domains = [\"{domain}\"]\n\n");
        for local in mailbox_locals {
            s.push_str(&format!(
                "[mailboxes.\"{local}@{domain}\"]\naddress = \"{local}@{domain}\"\nowner = \"ops\"\n\n",
            ));
        }
        fs::write(path, s).unwrap();
    }

    // --- detect_layout_state ------------------------------------------------

    #[test]
    fn detect_pristine_v1_layout_returns_needs_migration() {
        let tmp = TempDir::new().unwrap();
        let data_dir = tmp.path().join("data");
        let dkim_dir = tmp.path().join("dkim");
        let cfg = tmp.path().join("config.toml");

        fs::create_dir_all(data_dir.join("inbox").join("info")).unwrap();
        fs::create_dir_all(data_dir.join("sent").join("info")).unwrap();
        touch(&dkim_dir.join("private.key"));
        touch(&dkim_dir.join("public.key"));
        write_legacy_config(&cfg, "mydomain.com", &["info", "support"]);

        let state = detect_layout_state(&data_dir, &dkim_dir, &cfg, "mydomain.com");
        match state {
            LayoutState::NeedsMigration(ind) => {
                assert!(ind.legacy_inbox_dir);
                assert!(ind.legacy_sent_dir);
                assert!(ind.legacy_dkim_key);
                assert!(ind.legacy_config_domain_field);
                assert!(ind.legacy_mailbox_local_part_keys);
            }
            other => panic!("expected NeedsMigration, got {other:?}"),
        }
    }

    #[test]
    fn detect_marker_present_returns_migrated() {
        let tmp = TempDir::new().unwrap();
        let data_dir = tmp.path().join("data");
        let dkim_dir = tmp.path().join("dkim");
        let cfg = tmp.path().join("config.toml");
        fs::create_dir_all(&data_dir).unwrap();
        fs::write(data_dir.join(LAYOUT_MARKER_FILENAME), "2\n").unwrap();
        // Marker wins even when v1-shape indicators are still around.
        fs::create_dir_all(data_dir.join("inbox").join("info")).unwrap();
        write_legacy_config(&cfg, "mydomain.com", &["info"]);

        let state = detect_layout_state(&data_dir, &dkim_dir, &cfg, "mydomain.com");
        assert_eq!(state, LayoutState::Migrated);
    }

    #[test]
    fn detect_marker_with_wrong_version_returns_corrupted() {
        let tmp = TempDir::new().unwrap();
        let data_dir = tmp.path().join("data");
        let dkim_dir = tmp.path().join("dkim");
        let cfg = tmp.path().join("config.toml");
        fs::create_dir_all(&data_dir).unwrap();
        fs::write(data_dir.join(LAYOUT_MARKER_FILENAME), "99\n").unwrap();

        let state = detect_layout_state(&data_dir, &dkim_dir, &cfg, "anywhere.com");
        match state {
            LayoutState::Corrupted(msg) => {
                assert!(msg.contains("99"));
                assert!(msg.contains("expected '2'"));
            }
            other => panic!("expected Corrupted, got {other:?}"),
        }
    }

    #[test]
    fn detect_fresh_install_returns_fresh() {
        let tmp = TempDir::new().unwrap();
        let data_dir = tmp.path().join("data");
        let dkim_dir = tmp.path().join("dkim");
        let cfg = tmp.path().join("config.toml");
        fs::create_dir_all(&data_dir).unwrap();
        // Write a canonical config that wouldn't trigger any v1 signals.
        write_canonical_config(&cfg, "fresh.example.com", &[]);

        let state = detect_layout_state(&data_dir, &dkim_dir, &cfg, "fresh.example.com");
        assert_eq!(state, LayoutState::FreshInstall);
    }

    #[test]
    fn detect_half_migrated_storage_only_still_triggers_migration() {
        // Storage already moved (no `inbox/` at the root, present under
        // `<domain>/inbox/`) but DKIM still at the root → re-run must
        // see the DKIM indicator and resume.
        let tmp = TempDir::new().unwrap();
        let data_dir = tmp.path().join("data");
        let dkim_dir = tmp.path().join("dkim");
        let cfg = tmp.path().join("config.toml");

        fs::create_dir_all(data_dir.join("mydomain.com").join("inbox").join("info")).unwrap();
        touch(&dkim_dir.join("private.key"));
        write_legacy_config(&cfg, "mydomain.com", &["info"]);

        let state = detect_layout_state(&data_dir, &dkim_dir, &cfg, "mydomain.com");
        match state {
            LayoutState::NeedsMigration(ind) => {
                assert!(!ind.legacy_inbox_dir, "storage already moved");
                assert!(ind.legacy_dkim_key, "DKIM still legacy");
                assert!(ind.legacy_config_domain_field);
            }
            other => panic!("expected NeedsMigration on half-migrated install, got {other:?}"),
        }
    }

    #[test]
    fn detect_handles_per_domain_subtable_without_misclassifying_as_legacy() {
        // `[domain."b.com"]` is the *canonical* per-domain override
        // sub-table, NOT the legacy `domain = "..."` scalar. The
        // detector must not flag it as a legacy field.
        let tmp = TempDir::new().unwrap();
        let data_dir = tmp.path().join("data");
        let dkim_dir = tmp.path().join("dkim");
        let cfg = tmp.path().join("config.toml");
        fs::create_dir_all(&data_dir).unwrap();
        let body = "domains = [\"a.com\", \"b.com\"]\n\n\
                    [domain.\"b.com\"]\nsignature = \"Sent from B\"\n\n\
                    [mailboxes.\"info@a.com\"]\naddress = \"info@a.com\"\nowner = \"ops\"\n";
        fs::write(&cfg, body).unwrap();
        // Pre-write marker so the detector returns Migrated (which is
        // what a fully-migrated multi-domain install looks like). The
        // canonical-config heuristic is the only thing under test here.
        fs::write(data_dir.join(LAYOUT_MARKER_FILENAME), "2\n").unwrap();
        let state = detect_layout_state(&data_dir, &dkim_dir, &cfg, "a.com");
        assert_eq!(state, LayoutState::Migrated);
    }

    // --- inspect_config_for_legacy -----------------------------------------

    #[test]
    fn inspect_legacy_domain_only() {
        let s = "domain = \"x.com\"\n\n[mailboxes.\"info@x.com\"]\naddress=\"info@x.com\"\n";
        let (legacy, locals) = inspect_config_for_legacy(s);
        assert!(legacy);
        assert!(!locals);
    }

    #[test]
    fn inspect_canonical_with_local_keys_does_not_double_flag() {
        // Should never happen in practice (the config parser rejects mixed),
        // but the heuristic should still suppress `has_legacy_field`
        // when `domains` is present.
        let s = "domains = [\"x.com\"]\ndomain = \"x.com\"\n\n[mailboxes.info]\naddress=\"info@x.com\"\n";
        let (legacy, locals) = inspect_config_for_legacy(s);
        assert!(!legacy);
        assert!(locals);
    }

    #[test]
    fn inspect_legacy_local_part_keys_only() {
        let s = "domains = [\"x.com\"]\n\n[mailboxes.info]\naddress=\"info@x.com\"\n";
        let (legacy, locals) = inspect_config_for_legacy(s);
        assert!(!legacy);
        assert!(locals);
    }

    #[test]
    fn inspect_canonical_only() {
        let s = "domains = [\"x.com\"]\n\n[mailboxes.\"info@x.com\"]\naddress=\"info@x.com\"\n";
        let (legacy, locals) = inspect_config_for_legacy(s);
        assert!(!legacy);
        assert!(!locals);
    }

    #[test]
    fn inspect_handles_comments_and_blank_lines() {
        let s = "# header\n\n  # indented\ndomain = \"x.com\"   # trailing comment\n";
        let (legacy, _) = inspect_config_for_legacy(s);
        assert!(legacy);
    }

    // --- relocate_storage --------------------------------------------------

    #[test]
    fn storage_relocation_renames_inbox_and_sent() {
        let tmp = TempDir::new().unwrap();
        let data_dir = tmp.path().to_path_buf();
        fs::create_dir_all(data_dir.join("inbox").join("info")).unwrap();
        fs::write(data_dir.join("inbox").join("info").join("a.md"), b"body").unwrap();
        fs::create_dir_all(data_dir.join("sent").join("info")).unwrap();

        let report = relocate_storage_for_default_domain(&data_dir, "x.com").unwrap();
        match report.inbox {
            MoveOutcome::Renamed { .. } => {}
            other => panic!("expected inbox Renamed, got {other:?}"),
        }
        match report.sent {
            MoveOutcome::Renamed { .. } => {}
            other => panic!("expected sent Renamed, got {other:?}"),
        }
        // Files moved.
        assert!(
            data_dir
                .join("x.com")
                .join("inbox")
                .join("info")
                .join("a.md")
                .is_file()
        );
        assert!(!data_dir.join("inbox").is_dir());
    }

    #[test]
    fn storage_relocation_idempotent_when_already_done() {
        let tmp = TempDir::new().unwrap();
        let data_dir = tmp.path().to_path_buf();
        fs::create_dir_all(data_dir.join("x.com").join("inbox").join("info")).unwrap();
        fs::create_dir_all(data_dir.join("x.com").join("sent").join("info")).unwrap();

        let report = relocate_storage_for_default_domain(&data_dir, "x.com").unwrap();
        assert_eq!(report.inbox, MoveOutcome::AlreadyDone);
        assert_eq!(report.sent, MoveOutcome::AlreadyDone);
    }

    #[test]
    fn storage_relocation_handles_missing_sent() {
        // A v1 install that's never sent outbound has no `sent/` dir.
        // The relocation must skip silently rather than fail.
        let tmp = TempDir::new().unwrap();
        let data_dir = tmp.path().to_path_buf();
        fs::create_dir_all(data_dir.join("inbox").join("info")).unwrap();

        let report = relocate_storage_for_default_domain(&data_dir, "x.com").unwrap();
        match report.inbox {
            MoveOutcome::Renamed { .. } => {}
            other => panic!("expected Renamed, got {other:?}"),
        }
        assert_eq!(report.sent, MoveOutcome::NothingToDo);
    }

    /// Regression: the per-domain storage dir must be `0o755` so a
    /// non-root mailbox owner (running `aimx mcp` under their own uid)
    /// can `x`-traverse into `<data_dir>/<domain>/inbox/<name>/`. The
    /// daemon runs with `umask 0o077`, so `create_dir_all` would land
    /// the dir at `0o700` without the explicit chmod and every
    /// non-root MCP read would surface EACCES.
    #[test]
    fn storage_relocation_per_domain_dir_is_world_traversable() {
        let tmp = TempDir::new().unwrap();
        let data_dir = tmp.path().to_path_buf();
        fs::create_dir_all(data_dir.join("inbox").join("info")).unwrap();

        // Force the umask the daemon sets so the test reproduces the
        // production code path (default `cargo test` umask is `0o022`
        // and would hide the regression).
        let _guard = UmaskGuard::set(0o077);

        relocate_storage_for_default_domain(&data_dir, "x.com").unwrap();
        let dir_mode = fs::metadata(data_dir.join("x.com"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(
            dir_mode, 0o755,
            "per-domain storage dir must be 0o755 so non-root mailbox \
             owners can traverse into their own inbox/<name>/ subdir"
        );
    }

    /// The per-domain mode is also enforced when the dir already exists
    /// from an earlier partial run (defense in depth — a re-entry
    /// after a crash mid-migration must heal an over-tightened dir).
    #[test]
    fn storage_relocation_chmods_existing_per_domain_dir() {
        let tmp = TempDir::new().unwrap();
        let data_dir = tmp.path().to_path_buf();
        // Pre-existing per-domain dir, locked to 0o700 (what a crashed
        // earlier run under the daemon's umask would have left behind).
        fs::create_dir_all(data_dir.join("x.com")).unwrap();
        fs::set_permissions(data_dir.join("x.com"), fs::Permissions::from_mode(0o700)).unwrap();
        fs::create_dir_all(data_dir.join("inbox").join("info")).unwrap();

        relocate_storage_for_default_domain(&data_dir, "x.com").unwrap();
        let dir_mode = fs::metadata(data_dir.join("x.com"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(dir_mode, 0o755);
    }

    #[test]
    fn storage_relocation_refuses_when_both_src_and_dst_exist() {
        let tmp = TempDir::new().unwrap();
        let data_dir = tmp.path().to_path_buf();
        fs::create_dir_all(data_dir.join("inbox")).unwrap();
        fs::create_dir_all(data_dir.join("x.com").join("inbox")).unwrap();

        let err = relocate_storage_for_default_domain(&data_dir, "x.com").unwrap_err();
        match err {
            MigrationError::Other(msg) => assert!(msg.contains("refusing to overwrite")),
            other => panic!("expected Other(refusing), got {other:?}"),
        }
    }

    // --- relocate_dkim -----------------------------------------------------

    #[test]
    fn dkim_relocation_renames_and_preserves_modes() {
        let tmp = TempDir::new().unwrap();
        let dkim_dir = tmp.path().to_path_buf();
        touch_with(&dkim_dir.join("private.key"), b"-----PRIVATE-----");
        touch_with(&dkim_dir.join("public.key"), b"-----PUBLIC-----");
        fs::set_permissions(
            dkim_dir.join("private.key"),
            fs::Permissions::from_mode(0o600),
        )
        .unwrap();
        fs::set_permissions(
            dkim_dir.join("public.key"),
            fs::Permissions::from_mode(0o644),
        )
        .unwrap();

        let report = relocate_dkim_for_default_domain(&dkim_dir, "x.com").unwrap();
        assert!(report.created_domain_dir);
        match report.private_key {
            MoveOutcome::Renamed { .. } => {}
            other => panic!("expected private Renamed, got {other:?}"),
        }
        match report.public_key {
            MoveOutcome::Renamed { .. } => {}
            other => panic!("expected public Renamed, got {other:?}"),
        }
        // Modes preserved across rename.
        let priv_mode = fs::metadata(dkim_dir.join("x.com").join("private.key"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        let pub_mode = fs::metadata(dkim_dir.join("x.com").join("public.key"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(priv_mode, 0o600, "private.key must stay 0600 across rename");
        assert_eq!(pub_mode, 0o644, "public.key must stay 0644 across rename");
        // Per-domain dir is 0700.
        let dir_mode = fs::metadata(dkim_dir.join("x.com"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(dir_mode, 0o700, "per-domain DKIM dir must be 0700");
    }

    #[test]
    fn dkim_relocation_skips_missing_public_key() {
        let tmp = TempDir::new().unwrap();
        let dkim_dir = tmp.path().to_path_buf();
        touch_with(&dkim_dir.join("private.key"), b"-----PRIVATE-----");

        let report = relocate_dkim_for_default_domain(&dkim_dir, "x.com").unwrap();
        match report.private_key {
            MoveOutcome::Renamed { .. } => {}
            other => panic!("expected private Renamed, got {other:?}"),
        }
        assert_eq!(report.public_key, MoveOutcome::NothingToDo);
    }

    #[test]
    fn dkim_relocation_idempotent_when_already_done() {
        let tmp = TempDir::new().unwrap();
        let dkim_dir = tmp.path().to_path_buf();
        let nested = dkim_dir.join("x.com");
        fs::create_dir_all(&nested).unwrap();
        touch_with(&nested.join("private.key"), b"-----PRIVATE-----");
        touch_with(&nested.join("public.key"), b"-----PUBLIC-----");

        let report = relocate_dkim_for_default_domain(&dkim_dir, "x.com").unwrap();
        assert!(
            !report.created_domain_dir,
            "domain dir already existed, must report not-created"
        );
        assert_eq!(report.private_key, MoveOutcome::AlreadyDone);
        assert_eq!(report.public_key, MoveOutcome::AlreadyDone);
    }

    // --- rewrite_config ----------------------------------------------------

    #[test]
    fn rewrite_config_promotes_legacy_domain_field_but_preserves_mailbox_keys() {
        let tmp = TempDir::new().unwrap();
        let cfg_path = tmp.path().join("config.toml");
        write_legacy_config(&cfg_path, "x.com", &["info", "support"]);

        // Load via Config::load to get the same in-memory shape the
        // daemon would see at startup (legacy local-part keys preserved).
        let cfg = Config::load_ignore_warnings(&cfg_path).unwrap();
        assert!(
            cfg.mailboxes.contains_key("info"),
            "config-load invariant: legacy local-part keys preserved on load"
        );

        let returned = rewrite_config_to_canonical_shape(&cfg_path, &cfg).unwrap();

        // In-memory shape preserved end-to-end — the migration is
        // structural, not semantic, and the runtime data plane keeps
        // looking up mailboxes by their operator-friendly key.
        assert!(returned.mailboxes.contains_key("info"));
        assert!(returned.mailboxes.contains_key("support"));

        // On disk: `domains = [...]` replaces the legacy `domain = "..."`
        // field, but mailbox keys keep their operator-friendly form so
        // downstream CLI lookups (`hooks create alice`, `mailboxes show`)
        // continue to resolve.
        let reloaded = Config::load_ignore_warnings(&cfg_path).unwrap();
        assert_eq!(reloaded.domains, vec!["x.com"]);
        assert!(
            reloaded.mailboxes.contains_key("info"),
            "operator-friendly local-part keys preserved on disk"
        );
        assert!(reloaded.mailboxes.contains_key("support"));

        // Serialized file body no longer carries the legacy scalar.
        let serialized = fs::read_to_string(&cfg_path).unwrap();
        for line in serialized.lines() {
            let line = line.trim();
            let looks_like_legacy_scalar = line.starts_with("domain")
                && !line.starts_with("domains")
                && !line.starts_with("domain.")
                && line.contains('=');
            assert!(
                !looks_like_legacy_scalar,
                "legacy `domain = ...` field must not survive the rewrite, found: {line}"
            );
        }
    }

    #[test]
    fn rewrite_config_is_idempotent_on_canonical_input() {
        let tmp = TempDir::new().unwrap();
        let cfg_path = tmp.path().join("config.toml");
        write_canonical_config(&cfg_path, "x.com", &["info"]);

        let cfg = Config::load_ignore_warnings(&cfg_path).unwrap();
        let first = rewrite_config_to_canonical_shape(&cfg_path, &cfg).unwrap();
        let first_disk = fs::read_to_string(&cfg_path).unwrap();
        let second = rewrite_config_to_canonical_shape(&cfg_path, &first).unwrap();
        let second_disk = fs::read_to_string(&cfg_path).unwrap();
        assert_eq!(
            first_disk, second_disk,
            "second rewrite must be byte-identical"
        );
        assert!(second.mailboxes.contains_key("info@x.com"));
    }

    // --- write_layout_version_marker --------------------------------------

    #[test]
    fn marker_write_emits_2_and_is_idempotent() {
        let tmp = TempDir::new().unwrap();
        let data_dir = tmp.path().to_path_buf();
        write_layout_version_marker(&data_dir).unwrap();
        let body = fs::read_to_string(data_dir.join(LAYOUT_MARKER_FILENAME)).unwrap();
        assert_eq!(body, "2\n");
        let mode = fs::metadata(data_dir.join(LAYOUT_MARKER_FILENAME))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o644);

        // Re-running is safe: same content.
        write_layout_version_marker(&data_dir).unwrap();
        let body2 = fs::read_to_string(data_dir.join(LAYOUT_MARKER_FILENAME)).unwrap();
        assert_eq!(body2, "2\n");
    }

    // --- run_migration full path ------------------------------------------

    // --- run_startup_migration orchestration ------------------------------

    #[test]
    fn startup_migration_returns_already_migrated_when_marker_present() {
        let tmp = TempDir::new().unwrap();
        let data_dir = tmp.path().join("data");
        let dkim_dir = tmp.path().join("dkim");
        let cfg_path = tmp.path().join("config.toml");
        fs::create_dir_all(&data_dir).unwrap();
        fs::write(data_dir.join(LAYOUT_MARKER_FILENAME), "2\n").unwrap();
        write_canonical_config(&cfg_path, "x.com", &["info"]);

        let cfg = Config::load_ignore_warnings(&cfg_path).unwrap();
        let outcome = run_startup_migration(&data_dir, &dkim_dir, &cfg_path, &cfg).unwrap();
        assert!(matches!(outcome, StartupMigrationOutcome::AlreadyMigrated));
    }

    #[test]
    fn startup_migration_writes_marker_on_fresh_install_and_emits_no_log() {
        let tmp = TempDir::new().unwrap();
        let data_dir = tmp.path().join("data");
        let dkim_dir = tmp.path().join("dkim");
        let cfg_path = tmp.path().join("config.toml");
        fs::create_dir_all(&data_dir).unwrap();
        write_canonical_config(&cfg_path, "fresh.example.com", &[]);

        let cfg = Config::load_ignore_warnings(&cfg_path).unwrap();
        let outcome = run_startup_migration(&data_dir, &dkim_dir, &cfg_path, &cfg).unwrap();
        assert!(matches!(outcome, StartupMigrationOutcome::Fresh));
        // Marker written.
        assert_eq!(
            fs::read_to_string(data_dir.join(LAYOUT_MARKER_FILENAME)).unwrap(),
            "2\n",
        );

        // Second call: now `AlreadyMigrated`.
        let again = run_startup_migration(&data_dir, &dkim_dir, &cfg_path, &cfg).unwrap();
        assert!(matches!(again, StartupMigrationOutcome::AlreadyMigrated));
    }

    #[test]
    fn startup_migration_runs_full_path_on_v1_install() {
        let tmp = TempDir::new().unwrap();
        let data_dir = tmp.path().join("data");
        let dkim_dir = tmp.path().join("dkim");
        let cfg_path = tmp.path().join("config.toml");

        fs::create_dir_all(data_dir.join("inbox").join("info")).unwrap();
        fs::write(
            data_dir.join("inbox").join("info").join("a.md"),
            b"+++\nfrom = \"x@y.com\"\n+++\n",
        )
        .unwrap();
        fs::create_dir_all(data_dir.join("sent").join("info")).unwrap();
        touch_with(&dkim_dir.join("private.key"), b"PK");
        touch_with(&dkim_dir.join("public.key"), b"PUB");
        write_legacy_config(&cfg_path, "x.com", &["info", "support"]);

        let cfg = Config::load_ignore_warnings(&cfg_path).unwrap();
        let outcome = run_startup_migration(&data_dir, &dkim_dir, &cfg_path, &cfg).unwrap();
        match outcome {
            StartupMigrationOutcome::Migrated(inner) => {
                let MigratedOutcome { report, rewritten } = *inner;
                assert!(report.config_rewritten);
                assert!(report.marker_written);
                // The returned Config preserves the in-memory shape end-
                // to-end (legacy local-part keys) so the runtime data
                // plane keeps working in this session.
                assert!(rewritten.mailboxes.contains_key("info"));
                let line = format_migration_log_line(&report, rewritten.default_domain());
                assert!(line.contains("default_domain=x.com"));
                assert!(line.contains("layout_version=2"));
                assert!(line.contains("see book/multi-domain.md"));
            }
            other => panic!("expected Migrated outcome, got {other:?}"),
        }
        // On-disk shape: `domains = [...]` replaces the legacy scalar,
        // but mailbox keys keep their operator-friendly local-part
        // form (the FQDN re-key is deferred to the runtime rewire).
        let reloaded = Config::load_ignore_warnings(&cfg_path).unwrap();
        assert_eq!(reloaded.domains, vec!["x.com"]);
        assert!(reloaded.mailboxes.contains_key("info"));

        // Idempotent: second call sees the marker.
        let cfg2 = Config::load_ignore_warnings(&cfg_path).unwrap();
        let again = run_startup_migration(&data_dir, &dkim_dir, &cfg_path, &cfg2).unwrap();
        assert!(matches!(again, StartupMigrationOutcome::AlreadyMigrated));
    }

    #[test]
    fn startup_migration_returns_corrupted_for_bad_marker() {
        let tmp = TempDir::new().unwrap();
        let data_dir = tmp.path().join("data");
        let dkim_dir = tmp.path().join("dkim");
        let cfg_path = tmp.path().join("config.toml");
        fs::create_dir_all(&data_dir).unwrap();
        fs::write(data_dir.join(LAYOUT_MARKER_FILENAME), "99\n").unwrap();
        write_canonical_config(&cfg_path, "x.com", &[]);

        let cfg = Config::load_ignore_warnings(&cfg_path).unwrap();
        let err = run_startup_migration(&data_dir, &dkim_dir, &cfg_path, &cfg).unwrap_err();
        match err {
            StartupMigrationError::Corrupted(msg) => assert!(msg.contains("99")),
            other => panic!("expected Corrupted, got {other:?}"),
        }
    }

    #[test]
    fn full_migration_end_to_end_against_v1_fixture() {
        let tmp = TempDir::new().unwrap();
        let data_dir = tmp.path().join("data");
        let dkim_dir = tmp.path().join("dkim");
        let cfg_path = tmp.path().join("config.toml");

        // Build a realistic v1 fixture in-place.
        fs::create_dir_all(data_dir.join("inbox").join("info")).unwrap();
        fs::write(
            data_dir.join("inbox").join("info").join("hello.md"),
            b"+++\nfrom = \"alice@example.com\"\n+++\nbody",
        )
        .unwrap();
        fs::create_dir_all(data_dir.join("sent").join("info")).unwrap();
        touch_with(&dkim_dir.join("private.key"), b"PK");
        touch_with(&dkim_dir.join("public.key"), b"PUB");
        write_legacy_config(&cfg_path, "x.com", &["info", "support"]);

        let cfg = Config::load_ignore_warnings(&cfg_path).unwrap();
        let pre_state = detect_layout_state(&data_dir, &dkim_dir, &cfg_path, "x.com");
        match &pre_state {
            LayoutState::NeedsMigration(ind) => assert!(ind.any()),
            other => panic!("expected NeedsMigration, got {other:?}"),
        }

        let (report, returned) = run_migration(&data_dir, &dkim_dir, &cfg_path, &cfg).unwrap();

        // Storage relocated.
        assert!(
            data_dir
                .join("x.com")
                .join("inbox")
                .join("info")
                .join("hello.md")
                .is_file()
        );
        assert!(!data_dir.join("inbox").is_dir());
        // DKIM relocated.
        assert!(dkim_dir.join("x.com").join("private.key").is_file());
        assert!(!dkim_dir.join("private.key").is_file());
        // In-memory shape preserved end-to-end (legacy local-part keys
        // round-trip through the rewrite); the on-disk shape promotes
        // `domain` → `domains` but keeps the operator-friendly mailbox
        // keys for downstream CLI compatibility.
        assert!(returned.mailboxes.contains_key("info"));
        assert!(returned.mailboxes.contains_key("support"));
        let reloaded = Config::load_ignore_warnings(&cfg_path).unwrap();
        assert_eq!(reloaded.domains, vec!["x.com"]);
        assert!(reloaded.mailboxes.contains_key("info"));
        assert!(reloaded.mailboxes.contains_key("support"));
        // Marker present.
        assert_eq!(
            fs::read_to_string(data_dir.join(LAYOUT_MARKER_FILENAME)).unwrap(),
            "2\n"
        );
        // Report flags.
        assert!(report.config_rewritten);
        assert!(report.marker_written);

        // Second detection: now Migrated.
        let post_state =
            detect_layout_state(&data_dir, &dkim_dir, &cfg_path, returned.default_domain());
        assert_eq!(post_state, LayoutState::Migrated);

        // Second run is a no-op at the detection layer.
        let third = detect_layout_state(&data_dir, &dkim_dir, &cfg_path, returned.default_domain());
        assert_eq!(third, LayoutState::Migrated);
    }
}
