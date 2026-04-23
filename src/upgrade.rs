//! `aimx upgrade` — single-verb binary self-update (PRD §6.4, FR-4.1–4.6).
//!
//! # Flow (happy path)
//!
//! 1. Root check (FR-4.1).
//! 2. Resolve the target manifest: `--version <tag>` → `release_by_tag`,
//!    else `latest_release`. Compare its tag against
//!    [`crate::version::release_tag`]. Equal and no `--force` → print
//!    "up to date" and exit.
//! 3. Resolve the install path from `/proc/self/exe` (handles
//!    `AIMX_PREFIX=/opt/aimx` installs) and look up the target-specific
//!    tarball asset in the manifest using [`crate::version::target_triple`].
//! 4. Fetch the tarball bytes (HTTPS-only; trust anchor is the GitHub
//!    Releases domain per PRD §7).
//! 5. Dry-run short-circuit: if `--dry-run`, print the preview and exit.
//! 6. Extract the tarball into a fresh `tempfile::tempdir`; verify it
//!    contains an executable `aimx-<version>-<target>/aimx`.
//! 7. Stop the service; atomically swap the binary (preserving the old
//!    one at `<install_path>.prev`); start the service; poll port 25
//!    for readiness.
//! 8. On any step failure between stop and the final start-ready poll,
//!    roll back by restoring `.prev` and starting the service again,
//!    then return [`UpgradeError::RolledBack`].
//!
//! # Trust model
//!
//! No checksum or signature verification on the default path (PRD §9).
//! HTTPS on the GitHub Releases domain is the v1 trust anchor.
//! [`crate::release::verify_sha256`] remains available for operator-run
//! `sha256sum -c` workflows against the published `SHA256SUMS` asset.

use std::fs;
use std::path::{Path, PathBuf};

use crate::cli::UpgradeArgs;
use crate::release::{ReleaseError, ReleaseManifest, ReleaseOps};
use crate::setup::SystemOps;
use crate::term;
use crate::version;

/// Service name the upgrade flow drives. Matches `install_service_file`
/// in `src/setup.rs`.
pub const SERVICE_NAME: &str = "aimx";

/// Named steps of the upgrade flow. [`UpgradeReport`] records each
/// step the real flow executed so tests can assert the full sequence
/// and [`UpgradeError::RolledBack`] can name the failing one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpgradeStep {
    ResolveManifest,
    CompareTag,
    FetchTarball,
    ExtractTarball,
    StopService,
    PreserveOldBinary,
    InstallNewBinary,
    StartService,
    WaitForReady,
}

impl std::fmt::Display for UpgradeStep {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Self::ResolveManifest => "resolve manifest",
            Self::CompareTag => "compare tag",
            Self::FetchTarball => "fetch tarball",
            Self::ExtractTarball => "extract tarball",
            Self::StopService => "stop service",
            Self::PreserveOldBinary => "preserve old binary",
            Self::InstallNewBinary => "install new binary",
            Self::StartService => "start service",
            Self::WaitForReady => "wait for service ready",
        };
        f.write_str(s)
    }
}

/// Outcome of a successful / dry-run / up-to-date invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Running tag already matches the target tag and `--force` was not
    /// set. No fetches, no writes.
    UpToDate,
    /// `--dry-run` exercised resolve + fetch, then stopped.
    DryRun,
    /// Full swap completed; service restarted.
    Upgraded,
}

/// What the upgrade run did, in order, for testability.
#[derive(Debug, Default, Clone)]
pub struct UpgradeReport {
    pub steps: Vec<UpgradeStep>,
    pub current_tag: String,
    pub target_tag: String,
    pub tarball_url: Option<String>,
    pub install_path: Option<PathBuf>,
    pub outcome: Option<Outcome>,
}

impl UpgradeReport {
    fn record(&mut self, step: UpgradeStep) {
        self.steps.push(step);
    }
}

#[derive(Debug)]
#[allow(clippy::enum_variant_names)]
pub enum UpgradeError {
    /// Invoked by a non-root user.
    NotRoot,
    /// Release manifest / tarball fetch / parse failure (pre-swap).
    /// Surfaces the underlying [`ReleaseError`] so operators see the
    /// HTTP status or transport cause directly.
    Release(ReleaseError),
    /// Filesystem or process error that did not reach the stop-service
    /// step. Nothing to roll back. Tarball-shape mismatches land here
    /// with [`UpgradeStep::ExtractTarball`].
    PreSwap { step: UpgradeStep, cause: String },
    /// Failure after `stop_service` succeeded. The service was restored
    /// onto the previous binary (best effort) before this error was
    /// returned. Tests assert on the failed step; the CLI renders it
    /// as `✗ <step>: <cause>` + a rollback hint line.
    RolledBack {
        failed_step: UpgradeStep,
        cause: String,
        previous_tag: String,
    },
    /// The rollback itself failed — service may be in an indeterminate
    /// state. Operator must intervene.
    RollbackFailed {
        original_step: UpgradeStep,
        original_cause: String,
        rollback_step: UpgradeStep,
        rollback_cause: String,
    },
}

impl std::fmt::Display for UpgradeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotRoot => write!(f, "aimx upgrade requires root"),
            Self::Release(e) => write!(f, "{e}"),
            Self::PreSwap { step, cause } => write!(f, "{step}: {cause}"),
            Self::RolledBack {
                failed_step,
                cause,
                previous_tag,
            } => write!(f, "{failed_step}: {cause}; rolled back to {previous_tag}"),
            Self::RollbackFailed {
                original_step,
                original_cause,
                rollback_step,
                rollback_cause,
            } => write!(
                f,
                "{original_step}: {original_cause}; rollback failed at {rollback_step}: {rollback_cause}"
            ),
        }
    }
}

impl std::error::Error for UpgradeError {}

impl From<ReleaseError> for UpgradeError {
    fn from(e: ReleaseError) -> Self {
        UpgradeError::Release(e)
    }
}

/// Entry point for the `aimx upgrade` subcommand wired up in `main.rs`.
/// Prints progress / result lines to stdout via [`term`] helpers and
/// returns a boxed error so the existing `dispatch` loop surfaces it
/// uniformly with every other subcommand.
pub fn run(
    args: UpgradeArgs,
    release: &dyn ReleaseOps,
    sys: &dyn SystemOps,
) -> Result<(), Box<dyn std::error::Error>> {
    let report = run_upgrade(&args, release, sys).map_err(render_error)?;
    print_result(&report);
    Ok(())
}

fn render_error(e: UpgradeError) -> Box<dyn std::error::Error> {
    if let UpgradeError::RolledBack {
        failed_step,
        cause,
        previous_tag,
    } = &e
    {
        eprintln!("{} {failed_step}: {cause}", term::error("✗"));
        eprintln!("→ rolled back to {previous_tag}; check logs with 'aimx logs'",);
    }
    Box::new(e)
}

fn print_result(report: &UpgradeReport) {
    match report.outcome {
        Some(Outcome::UpToDate) => {
            println!(
                "{} aimx {} is up to date.",
                term::success("✓"),
                term::highlight(&report.current_tag)
            );
        }
        Some(Outcome::DryRun) => {
            // Dry-run preview lines are already printed by run_upgrade.
        }
        Some(Outcome::Upgraded) => {
            println!(
                "{} aimx {} → {}. Service restarted.",
                term::success("✓"),
                term::highlight(&report.current_tag),
                term::highlight(&report.target_tag)
            );
        }
        None => {}
    }
}

/// Core upgrade flow. Pure enough to unit-test with [`MockReleaseOps`]
/// + `MockSystemOps`; prints operator-facing progress via [`term`].
pub fn run_upgrade(
    args: &UpgradeArgs,
    release: &dyn ReleaseOps,
    sys: &dyn SystemOps,
) -> Result<UpgradeReport, UpgradeError> {
    let mut report = UpgradeReport {
        current_tag: version::release_tag().to_string(),
        ..Default::default()
    };

    if !sys.check_root() {
        return Err(UpgradeError::NotRoot);
    }

    // Step 1: resolve the target manifest.
    report.record(UpgradeStep::ResolveManifest);
    let manifest = match args.version.as_deref() {
        Some(tag) => release.release_by_tag(tag)?,
        None => release.latest_release()?,
    };
    report.target_tag = manifest.tag.clone();

    // Step 2: compare tags.
    report.record(UpgradeStep::CompareTag);
    if !args.force && manifest.tag == report.current_tag {
        report.outcome = Some(Outcome::UpToDate);
        return Ok(report);
    }

    // Step 3: resolve the binary path + the matching tarball asset URL.
    let install_path = resolve_install_path(sys)?;
    report.install_path = Some(install_path.clone());
    let target = version::target_triple();
    let tarball_name = tarball_filename(&manifest.tag, target);
    let tarball_url = manifest
        .asset_url(&tarball_name)
        .map_err(UpgradeError::Release)?
        .to_string();
    report.tarball_url = Some(tarball_url.clone());

    // Step 4: fetch the tarball bytes.
    report.record(UpgradeStep::FetchTarball);
    let tarball_bytes = release.fetch_asset(&tarball_url)?;

    // Dry-run stops here with an operator-facing preview.
    if args.dry_run {
        print_dry_run_preview(&report, &manifest, &tarball_url, &install_path);
        report.outcome = Some(Outcome::DryRun);
        return Ok(report);
    }

    // Step 5: extract to a temp dir and locate the staged binary.
    report.record(UpgradeStep::ExtractTarball);
    let extract_dir = tempfile::tempdir().map_err(|e| UpgradeError::PreSwap {
        step: UpgradeStep::ExtractTarball,
        cause: format!("create tempdir: {e}"),
    })?;
    let staged = extract_tarball(&tarball_bytes, extract_dir.path(), &manifest.tag, target)
        .map_err(|cause| UpgradeError::PreSwap {
            step: UpgradeStep::ExtractTarball,
            cause,
        })?;

    // Step 6: stop → swap → start, with rollback.
    report.record(UpgradeStep::StopService);
    sys.stop_service(SERVICE_NAME)
        .map_err(|e| UpgradeError::PreSwap {
            step: UpgradeStep::StopService,
            cause: e.to_string(),
        })?;

    // From here on, any failure must attempt rollback.
    let prev_path = prev_path(&install_path);

    // Preserve old binary at <install_path>.prev. `rename` is atomic on
    // the same filesystem; tempdir is under /tmp which may not be the
    // same fs as /usr/local/bin, so we stage via a sibling rename.
    if let Err(e) = preserve_previous_binary(&install_path, &prev_path) {
        return attempt_rollback(
            sys,
            &install_path,
            &prev_path,
            UpgradeStep::PreserveOldBinary,
            e,
            &report.current_tag,
        );
    }
    report.record(UpgradeStep::PreserveOldBinary);

    if let Err(e) = install_binary_result(&staged, &install_path) {
        return attempt_rollback(
            sys,
            &install_path,
            &prev_path,
            UpgradeStep::InstallNewBinary,
            e,
            &report.current_tag,
        );
    }
    report.record(UpgradeStep::InstallNewBinary);

    if let Err(e) = sys.start_service(SERVICE_NAME) {
        return attempt_rollback(
            sys,
            &install_path,
            &prev_path,
            UpgradeStep::StartService,
            e.to_string(),
            &report.current_tag,
        );
    }
    report.record(UpgradeStep::StartService);

    if !sys.wait_for_service_ready() {
        return attempt_rollback(
            sys,
            &install_path,
            &prev_path,
            UpgradeStep::WaitForReady,
            "service did not reach ready state within timeout".to_string(),
            &report.current_tag,
        );
    }
    report.record(UpgradeStep::WaitForReady);

    report.outcome = Some(Outcome::Upgraded);
    Ok(report)
}

/// Derive the upgrade target path from `/proc/self/exe`. `AIMX_PREFIX`
/// installs (e.g. `/opt/aimx/bin/aimx`) resolve here correctly without
/// a hardcoded fallback. Canonicalisation resolves `aimx → aimx.prev`
/// symlinks or a future `/usr/local/bin/aimx → /opt/aimx/bin/aimx`
/// redirection so the upgrade swaps the real file rather than the link.
pub fn resolve_install_path(sys: &dyn SystemOps) -> Result<PathBuf, UpgradeError> {
    let raw = sys
        .get_aimx_binary_path()
        .map_err(|e| UpgradeError::PreSwap {
            step: UpgradeStep::ResolveManifest,
            cause: format!("resolve current executable: {e}"),
        })?;
    // If the binary is a symlink (rare; install.sh uses `install -m`
    // which writes a real file), follow it so we swap the target.
    let canonical = std::fs::canonicalize(&raw).unwrap_or(raw);
    Ok(canonical)
}

/// Standard tarball filename per PRD FR-1.2: `aimx-<version>-<target>.tar.gz`.
/// The leading `v` is stripped from the tag — `v1.2.3` → version `1.2.3` —
/// matching `.github/workflows/release.yml`.
pub fn tarball_filename(tag: &str, target: &str) -> String {
    let version = tag.strip_prefix('v').unwrap_or(tag);
    format!("aimx-{version}-{target}.tar.gz")
}

/// Expected directory inside the tarball (flat layout produced by the
/// release pipeline). Kept as a separate helper for testability.
pub fn tarball_inner_dir(tag: &str, target: &str) -> String {
    let version = tag.strip_prefix('v').unwrap_or(tag);
    format!("aimx-{version}-{target}")
}

fn prev_path(install_path: &Path) -> PathBuf {
    let mut s = install_path.as_os_str().to_os_string();
    s.push(".prev");
    PathBuf::from(s)
}

/// Extract `bytes` (a gzipped tar) into `dest_dir` and return the path
/// to the staged `aimx` binary inside `aimx-<version>-<target>/`.
/// Returns a [`String`] error message so the caller can wrap it in a
/// [`UpgradeError::PreSwap`] / [`UpgradeError::InvalidTarball`].
pub fn extract_tarball(
    bytes: &[u8],
    dest_dir: &Path,
    tag: &str,
    target: &str,
) -> Result<PathBuf, String> {
    let decoder = flate2::read::GzDecoder::new(bytes);
    let mut archive = tar::Archive::new(decoder);
    archive
        .unpack(dest_dir)
        .map_err(|e| format!("tar extract: {e}"))?;

    let inner = dest_dir.join(tarball_inner_dir(tag, target));
    let staged = inner.join("aimx");
    if !staged.exists() {
        return Err(format!(
            "tarball missing executable at {}/aimx",
            inner.display()
        ));
    }

    // Verify the staged binary is readable + non-empty. We don't try to
    // execute it here — that would bypass the swap-then-start rollback
    // guarantees below.
    let meta = fs::metadata(&staged).map_err(|e| format!("stat staged binary: {e}"))?;
    if !meta.is_file() || meta.len() == 0 {
        return Err("staged binary is empty or not a regular file".to_string());
    }
    Ok(staged)
}

/// Move the currently-installed binary to `<install_path>.prev`. Uses
/// `rename` for atomicity on the same filesystem — `fs::rename` on Unix
/// overwrites the destination atomically, so we rely on that instead of
/// pre-deleting a stale `.prev`. If the rename fails for an unrelated
/// reason (cross-filesystem, ENOSPC, etc.), the previous cycle's `.prev`
/// is preserved untouched. If the install path does not exist (fresh
/// install on a path-not-yet-written machine), returns `Ok(())` — there
/// is nothing to preserve.
fn preserve_previous_binary(install_path: &Path, prev_path: &Path) -> Result<(), String> {
    if !install_path.exists() {
        return Ok(());
    }
    fs::rename(install_path, prev_path).map_err(|e| {
        format!(
            "rename {} → {}: {e}",
            install_path.display(),
            prev_path.display()
        )
    })
}

/// Install the staged binary at `install_path` with mode `0755`. Uses a
/// sibling temp file + `rename` so a crash mid-write leaves either the
/// previous binary (in `.prev`) or the new one — never a partial write.
fn install_binary(staged: &Path, install_path: &Path) -> String {
    // Copy staged → sibling of install_path so the rename is same-fs.
    let parent = match install_path.parent() {
        Some(p) => p,
        None => {
            return format!(
                "install path {} has no parent directory",
                install_path.display()
            );
        }
    };
    let tmp = parent.join(".aimx.upgrade.tmp");
    if let Err(e) = fs::copy(staged, &tmp) {
        return format!("copy staged → {}: {e}", tmp.display());
    }
    if let Err(e) = set_executable(&tmp) {
        let _ = fs::remove_file(&tmp);
        return format!("chmod 0755 {}: {e}", tmp.display());
    }
    if let Err(e) = fs::rename(&tmp, install_path) {
        let _ = fs::remove_file(&tmp);
        return format!("rename {} → {}: {e}", tmp.display(), install_path.display());
    }
    String::new()
}

/// Wrapper that turns [`install_binary`]'s `String` (empty = OK) into
/// a `Result`. The empty-string convention lets the caller keep the
/// rollback branches flat.
fn install_binary_result(staged: &Path, install_path: &Path) -> Result<(), String> {
    let err = install_binary(staged, install_path);
    if err.is_empty() { Ok(()) } else { Err(err) }
}

#[cfg(unix)]
fn set_executable(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = fs::metadata(path)?.permissions();
    perms.set_mode(0o755);
    fs::set_permissions(path, perms)
}

#[cfg(not(unix))]
fn set_executable(_path: &Path) -> std::io::Result<()> {
    Ok(())
}

/// Attempt to restore `<install_path>.prev` → `<install_path>` and
/// start the service back up. Always returns `Err` — either the
/// rollback succeeded (→ [`UpgradeError::RolledBack`]) or it didn't
/// (→ [`UpgradeError::RollbackFailed`]).
fn attempt_rollback(
    sys: &dyn SystemOps,
    install_path: &Path,
    prev_path: &Path,
    failed_step: UpgradeStep,
    cause: String,
    previous_tag: &str,
) -> Result<UpgradeReport, UpgradeError> {
    // If .prev exists, try to restore it. A .prev may not exist if
    // preservation itself failed or we never got that far — in that
    // case we just attempt to start the service on whatever is there.
    if prev_path.exists() {
        // Drop any partially-installed new binary so the rename can
        // land on a clean slot.
        if install_path.exists()
            && let Err(e) = fs::remove_file(install_path)
        {
            return Err(UpgradeError::RollbackFailed {
                original_step: failed_step,
                original_cause: cause,
                rollback_step: UpgradeStep::PreserveOldBinary,
                rollback_cause: format!("remove {}: {e}", install_path.display()),
            });
        }
        if let Err(e) = fs::rename(prev_path, install_path) {
            return Err(UpgradeError::RollbackFailed {
                original_step: failed_step,
                original_cause: cause,
                rollback_step: UpgradeStep::PreserveOldBinary,
                rollback_cause: format!(
                    "rename {} → {}: {e}",
                    prev_path.display(),
                    install_path.display()
                ),
            });
        }
    }

    // Best-effort start. `RolledBack` is the success case for the
    // rollback itself — the operator's service is running again, just
    // on the old binary.
    if let Err(e) = sys.start_service(SERVICE_NAME) {
        return Err(UpgradeError::RollbackFailed {
            original_step: failed_step,
            original_cause: cause,
            rollback_step: UpgradeStep::StartService,
            rollback_cause: e.to_string(),
        });
    }

    Err(UpgradeError::RolledBack {
        failed_step,
        cause,
        previous_tag: previous_tag.to_string(),
    })
}

/// Print the dry-run preview (FR-4.2). The exact wording is matched by
/// the integration tests, so changes here must sync with those.
fn print_dry_run_preview(
    report: &UpgradeReport,
    manifest: &ReleaseManifest,
    tarball_url: &str,
    install_path: &Path,
) {
    println!("{}", term::header("aimx upgrade (dry-run)"));
    println!("  current: {}", term::highlight(&report.current_tag));
    println!("  target:  {}", term::highlight(&manifest.tag));
    println!("  tarball: {tarball_url}");
    println!("  install path: {}", install_path.display());
    println!("  actions (would run): stop {SERVICE_NAME} → swap binary → start {SERVICE_NAME}");
    println!(
        "{}",
        term::dim("(dry-run: no service or filesystem changes)")
    );
}

// ---------------------------------------------------------------------------
// Hooks used by unit tests in tests/ (re-exported through a test-only
// wrapper so tests can call install_binary_result without duplicating the
// rename machinery).
// ---------------------------------------------------------------------------

#[cfg(test)]
#[allow(dead_code)]
pub(crate) fn test_install_binary(staged: &Path, install_path: &Path) -> Result<(), String> {
    install_binary_result(staged, install_path)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
#[allow(clippy::field_reassign_with_default)]
mod tests {
    use super::*;
    use crate::cli::UpgradeArgs;
    use crate::release::ReleaseManifest;
    use crate::release::mock::MockReleaseOps;
    use crate::setup::tests::MockSystemOps;
    use std::io::Write;
    use tempfile::TempDir;

    fn current_target() -> &'static str {
        version::target_triple()
    }

    fn make_manifest(tag: &str, asset_names: &[&str]) -> ReleaseManifest {
        let asset_urls = asset_names
            .iter()
            .map(|n| ((*n).to_string(), format!("https://example.invalid/{n}")))
            .collect();
        ReleaseManifest {
            tag: tag.to_string(),
            published_at: "2026-04-20T00:00:00Z".to_string(),
            asset_urls,
        }
    }

    /// Build a minimal well-formed `aimx-<version>-<target>/aimx` tarball
    /// (a placeholder binary) as gzipped bytes.
    fn make_tarball(tag: &str, target: &str, body: &[u8]) -> Vec<u8> {
        let version = tag.strip_prefix('v').unwrap_or(tag);
        let inner = format!("aimx-{version}-{target}");
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        {
            let mut builder = tar::Builder::new(&mut gz);
            let mut header = tar::Header::new_gnu();
            header.set_path(format!("{inner}/aimx")).unwrap();
            header.set_size(body.len() as u64);
            header.set_mode(0o755);
            header.set_cksum();
            builder.append(&header, body).unwrap();
            builder.finish().unwrap();
        }
        gz.finish().unwrap()
    }

    fn args_default() -> UpgradeArgs {
        UpgradeArgs {
            dry_run: false,
            version: None,
            force: false,
        }
    }

    fn write_fake_binary(dir: &Path) -> PathBuf {
        let path = dir.join("aimx");
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(b"#!/bin/sh\nexit 0\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut p = std::fs::metadata(&path).unwrap().permissions();
            p.set_mode(0o755);
            std::fs::set_permissions(&path, p).unwrap();
        }
        path
    }

    fn seed_install_path(sys: &mut MockSystemOps, install_path: PathBuf) {
        sys.override_aimx_binary_path = Some(install_path);
    }

    #[test]
    fn up_to_date_short_circuits_without_fetches() {
        let mut sys = MockSystemOps::default();
        let tmp = TempDir::new().unwrap();
        let install = write_fake_binary(tmp.path());
        seed_install_path(&mut sys, install);

        let release = MockReleaseOps::default();
        release.push_latest(Ok(make_manifest(
            version::release_tag(),
            &[&tarball_filename(version::release_tag(), current_target())],
        )));

        let report = run_upgrade(&args_default(), &release, &sys).unwrap();
        assert_eq!(report.outcome, Some(Outcome::UpToDate));
        // No fetch step should appear.
        assert!(!report.steps.contains(&UpgradeStep::FetchTarball));
        // No file writes.
        assert!(sys.restarted_services.borrow().is_empty());
        assert!(sys.stopped_services.borrow().is_empty());
    }

    #[test]
    fn not_root_refuses() {
        let mut sys = MockSystemOps::default();
        sys.is_root = false;
        let release = MockReleaseOps::default();
        let err = run_upgrade(&args_default(), &release, &sys).unwrap_err();
        assert!(matches!(err, UpgradeError::NotRoot));
    }

    #[test]
    fn happy_path_stop_swap_start_sequence() {
        let mut sys = MockSystemOps::default();
        let tmp = TempDir::new().unwrap();
        let install = write_fake_binary(tmp.path());
        seed_install_path(&mut sys, install.clone());

        let tag = "v9.9.9-test";
        let target = current_target();
        let asset = tarball_filename(tag, target);
        let bytes = make_tarball(tag, target, b"fake aimx binary");
        let release = MockReleaseOps::default();
        release.push_latest(Ok(make_manifest(tag, &[&asset])));
        release.push_asset(Ok(bytes));

        let report = run_upgrade(&args_default(), &release, &sys).unwrap();
        assert_eq!(report.outcome, Some(Outcome::Upgraded));

        // Every step recorded in order.
        assert_eq!(
            report.steps,
            vec![
                UpgradeStep::ResolveManifest,
                UpgradeStep::CompareTag,
                UpgradeStep::FetchTarball,
                UpgradeStep::ExtractTarball,
                UpgradeStep::StopService,
                UpgradeStep::PreserveOldBinary,
                UpgradeStep::InstallNewBinary,
                UpgradeStep::StartService,
                UpgradeStep::WaitForReady,
            ]
        );

        // Service calls in order: stop then start (never restart).
        assert_eq!(&*sys.stopped_services.borrow(), &[SERVICE_NAME.to_string()]);
        assert_eq!(&*sys.started_services.borrow(), &[SERVICE_NAME.to_string()]);

        // Binary at install_path is the new one.
        let installed = std::fs::read(&install).unwrap();
        assert_eq!(installed, b"fake aimx binary");
        // .prev exists and holds the original placeholder body.
        let prev = prev_path(&install);
        assert!(prev.exists(), ".prev must be preserved after upgrade");
        let prev_bytes = std::fs::read(&prev).unwrap();
        assert!(prev_bytes.starts_with(b"#!/bin/sh"));
    }

    #[test]
    fn dry_run_fetches_but_does_not_swap() {
        let mut sys = MockSystemOps::default();
        let tmp = TempDir::new().unwrap();
        let install = write_fake_binary(tmp.path());
        seed_install_path(&mut sys, install.clone());

        let tag = "v9.9.9-dryrun";
        let target = current_target();
        let asset = tarball_filename(tag, target);
        let bytes = make_tarball(tag, target, b"dryrun body");
        let release = MockReleaseOps::default();
        release.push_latest(Ok(make_manifest(tag, &[&asset])));
        release.push_asset(Ok(bytes));

        let mut args = args_default();
        args.dry_run = true;
        let report = run_upgrade(&args, &release, &sys).unwrap();

        assert_eq!(report.outcome, Some(Outcome::DryRun));
        assert!(report.steps.contains(&UpgradeStep::FetchTarball));
        assert!(!report.steps.contains(&UpgradeStep::StopService));
        assert!(!report.steps.contains(&UpgradeStep::InstallNewBinary));
        assert!(sys.stopped_services.borrow().is_empty());
        assert!(sys.started_services.borrow().is_empty());

        // Binary untouched.
        let installed = std::fs::read(&install).unwrap();
        assert!(installed.starts_with(b"#!/bin/sh"));
        let prev = prev_path(&install);
        assert!(!prev.exists(), "dry-run must not create a .prev file");
    }

    #[test]
    fn dry_run_with_download_failure_returns_release_error() {
        let mut sys = MockSystemOps::default();
        let tmp = TempDir::new().unwrap();
        let install = write_fake_binary(tmp.path());
        seed_install_path(&mut sys, install);

        let tag = "v9.9.9-fail";
        let target = current_target();
        let asset = tarball_filename(tag, target);
        let release = MockReleaseOps::default();
        release.push_latest(Ok(make_manifest(tag, &[&asset])));
        release.push_asset(Err(ReleaseError::Transport("simulated".to_string())));

        let mut args = args_default();
        args.dry_run = true;
        let err = run_upgrade(&args, &release, &sys).unwrap_err();
        assert!(matches!(err, UpgradeError::Release(_)));
    }

    #[test]
    fn version_flag_uses_release_by_tag_and_skips_up_to_date() {
        let mut sys = MockSystemOps::default();
        let tmp = TempDir::new().unwrap();
        let install = write_fake_binary(tmp.path());
        seed_install_path(&mut sys, install.clone());

        let tag = version::release_tag();
        let target = current_target();
        let asset = tarball_filename(tag, target);
        let bytes = make_tarball(tag, target, b"pinned body");
        let release = MockReleaseOps::default();
        release.push_by_tag(Ok(make_manifest(tag, &[&asset])));
        release.push_asset(Ok(bytes));

        // With --version equal to current tag *and* --force, we must
        // re-install rather than short-circuit. This is the FR-4.4
        // same-tag + --force test (PRD §11 resolved open question).
        let args = UpgradeArgs {
            dry_run: false,
            version: Some(tag.to_string()),
            force: true,
        };
        let report = run_upgrade(&args, &release, &sys).unwrap();
        assert_eq!(report.outcome, Some(Outcome::Upgraded));
        assert_eq!(&*sys.stopped_services.borrow(), &[SERVICE_NAME.to_string()]);
    }

    #[test]
    fn force_same_tag_without_version_flag_still_reinstalls() {
        let mut sys = MockSystemOps::default();
        let tmp = TempDir::new().unwrap();
        let install = write_fake_binary(tmp.path());
        seed_install_path(&mut sys, install);

        let tag = version::release_tag();
        let target = current_target();
        let asset = tarball_filename(tag, target);
        let bytes = make_tarball(tag, target, b"reinstalled body");
        let release = MockReleaseOps::default();
        release.push_latest(Ok(make_manifest(tag, &[&asset])));
        release.push_asset(Ok(bytes));

        let args = UpgradeArgs {
            dry_run: false,
            version: None,
            force: true,
        };
        let report = run_upgrade(&args, &release, &sys).unwrap();
        assert_eq!(report.outcome, Some(Outcome::Upgraded));
    }

    #[test]
    fn dry_run_composes_with_version_and_force() {
        let mut sys = MockSystemOps::default();
        let tmp = TempDir::new().unwrap();
        let install = write_fake_binary(tmp.path());
        seed_install_path(&mut sys, install);

        let tag = "v1.2.3";
        let target = current_target();
        let asset = tarball_filename(tag, target);
        let bytes = make_tarball(tag, target, b"hello");
        let release = MockReleaseOps::default();
        release.push_by_tag(Ok(make_manifest(tag, &[&asset])));
        release.push_asset(Ok(bytes));

        let args = UpgradeArgs {
            dry_run: true,
            version: Some(tag.to_string()),
            force: true,
        };
        let report = run_upgrade(&args, &release, &sys).unwrap();
        assert_eq!(report.outcome, Some(Outcome::DryRun));
        assert_eq!(report.target_tag, tag);
    }

    #[test]
    fn unknown_tag_surfaces_release_error() {
        let mut sys = MockSystemOps::default();
        let tmp = TempDir::new().unwrap();
        let install = write_fake_binary(tmp.path());
        seed_install_path(&mut sys, install);

        let release = MockReleaseOps::default();
        release.push_by_tag(Err(ReleaseError::Http {
            status: 404,
            body: "Not Found".to_string(),
        }));

        let args = UpgradeArgs {
            dry_run: false,
            version: Some("v0.0.0-does-not-exist".to_string()),
            force: false,
        };
        let err = run_upgrade(&args, &release, &sys).unwrap_err();
        match err {
            UpgradeError::Release(ReleaseError::Http { status, .. }) => {
                assert_eq!(status, 404);
            }
            other => panic!("expected Release(Http{{404}}), got {other:?}"),
        }
    }

    #[test]
    fn missing_tarball_asset_errors_before_swap() {
        let mut sys = MockSystemOps::default();
        let tmp = TempDir::new().unwrap();
        let install = write_fake_binary(tmp.path());
        seed_install_path(&mut sys, install.clone());

        // Manifest that has no matching-target asset.
        let release = MockReleaseOps::default();
        release.push_latest(Ok(make_manifest(
            "v9.9.9-missing",
            &["aimx-9.9.9-missing-SHA256SUMS"],
        )));

        let err = run_upgrade(&args_default(), &release, &sys).unwrap_err();
        assert!(matches!(
            err,
            UpgradeError::Release(ReleaseError::AssetNotFound(_))
        ));
        // Nothing was touched.
        assert!(sys.stopped_services.borrow().is_empty());
        let prev = prev_path(&install);
        assert!(!prev.exists());
    }

    #[test]
    fn rollback_fires_on_start_failure() {
        let mut sys = MockSystemOps::default();
        sys.start_service_fails = true;
        let tmp = TempDir::new().unwrap();
        let install = write_fake_binary(tmp.path());
        seed_install_path(&mut sys, install.clone());

        let tag = "v9.9.9-rollback";
        let target = current_target();
        let asset = tarball_filename(tag, target);
        let bytes = make_tarball(tag, target, b"new but service wont start");
        let release = MockReleaseOps::default();
        release.push_latest(Ok(make_manifest(tag, &[&asset])));
        release.push_asset(Ok(bytes));

        // Every invocation of `start_service` fails — so rollback's own
        // `start_service` call fails too, yielding `RollbackFailed`.
        let err = run_upgrade(&args_default(), &release, &sys).unwrap_err();
        match &err {
            UpgradeError::RollbackFailed {
                original_step,
                rollback_step,
                ..
            } => {
                assert_eq!(*original_step, UpgradeStep::StartService);
                assert_eq!(*rollback_step, UpgradeStep::StartService);
            }
            other => panic!("expected RollbackFailed, got {other:?}"),
        }

        // But the binary was restored.
        let installed = std::fs::read(&install).unwrap();
        assert!(installed.starts_with(b"#!/bin/sh"));
    }

    #[test]
    fn rollback_fires_on_stop_failure_without_modifying_binary() {
        let mut sys = MockSystemOps::default();
        sys.stop_service_fails = true;
        let tmp = TempDir::new().unwrap();
        let install = write_fake_binary(tmp.path());
        seed_install_path(&mut sys, install.clone());

        let tag = "v9.9.9-stopfail";
        let target = current_target();
        let asset = tarball_filename(tag, target);
        let bytes = make_tarball(tag, target, b"wont even start");
        let release = MockReleaseOps::default();
        release.push_latest(Ok(make_manifest(tag, &[&asset])));
        release.push_asset(Ok(bytes));

        let err = run_upgrade(&args_default(), &release, &sys).unwrap_err();
        // Stop failed, so we are in PreSwap — nothing to roll back.
        match err {
            UpgradeError::PreSwap { step, .. } => assert_eq!(step, UpgradeStep::StopService),
            other => panic!("expected PreSwap(StopService), got {other:?}"),
        }
        // Binary is unchanged (stop-service failed before swap).
        let installed = std::fs::read(&install).unwrap();
        assert!(installed.starts_with(b"#!/bin/sh"));
        let prev = prev_path(&install);
        assert!(!prev.exists());
    }

    #[test]
    fn rollback_fires_on_wait_for_ready_timeout() {
        let mut sys = MockSystemOps::default();
        sys.service_ready = false;
        let tmp = TempDir::new().unwrap();
        let install = write_fake_binary(tmp.path());
        seed_install_path(&mut sys, install.clone());

        let tag = "v9.9.9-waitfail";
        let target = current_target();
        let asset = tarball_filename(tag, target);
        let bytes = make_tarball(tag, target, b"started but never ready");
        let release = MockReleaseOps::default();
        release.push_latest(Ok(make_manifest(tag, &[&asset])));
        release.push_asset(Ok(bytes));

        let err = run_upgrade(&args_default(), &release, &sys).unwrap_err();
        match err {
            UpgradeError::RolledBack { failed_step, .. } => {
                assert_eq!(failed_step, UpgradeStep::WaitForReady);
            }
            other => panic!("expected RolledBack(WaitForReady), got {other:?}"),
        }
        // Binary reverted.
        let installed = std::fs::read(&install).unwrap();
        assert!(installed.starts_with(b"#!/bin/sh"));
        // Service came back up on the old binary: one start call for
        // the upgrade, one start call for rollback.
        assert_eq!(
            sys.started_services.borrow().len(),
            2,
            "expected start_service called twice (upgrade + rollback)"
        );
    }

    #[test]
    fn extract_tarball_rejects_malformed_payload() {
        let tmp = TempDir::new().unwrap();
        let err =
            extract_tarball(b"not a tarball", tmp.path(), "v1.0.0", current_target()).unwrap_err();
        assert!(err.contains("tar extract"));
    }

    #[test]
    fn tarball_filename_strips_leading_v() {
        assert_eq!(
            tarball_filename("v1.2.3", "x86_64-unknown-linux-gnu"),
            "aimx-1.2.3-x86_64-unknown-linux-gnu.tar.gz"
        );
        // Tags without the leading v are taken verbatim.
        assert_eq!(
            tarball_filename("1.2.3", "aarch64-unknown-linux-musl"),
            "aimx-1.2.3-aarch64-unknown-linux-musl.tar.gz"
        );
    }

    #[test]
    fn prev_path_appends_suffix() {
        assert_eq!(
            prev_path(Path::new("/usr/local/bin/aimx")),
            PathBuf::from("/usr/local/bin/aimx.prev")
        );
        assert_eq!(
            prev_path(Path::new("/opt/aimx/bin/aimx")),
            PathBuf::from("/opt/aimx/bin/aimx.prev")
        );
    }

    /// N2 regression guard: feed `run_upgrade` a malformed tarball and
    /// assert the PreSwap(ExtractTarball) outcome — the service is never
    /// stopped and `.prev` is never created. Closes the S4-3 AC
    /// "Rollback fires on tarball-extraction failure" at the `run_upgrade`
    /// level (the in-helper test only covered `extract_tarball` directly).
    #[test]
    fn run_upgrade_malformed_tarball_returns_preswap_extract_error() {
        let mut sys = MockSystemOps::default();
        let tmp = TempDir::new().unwrap();
        let install = write_fake_binary(tmp.path());
        seed_install_path(&mut sys, install.clone());

        let tag = "v9.9.9-malformed";
        let target = current_target();
        let asset = tarball_filename(tag, target);
        // Not a gzipped tar — `extract_tarball` must reject this.
        let garbage = b"not a tarball at all".to_vec();
        let release = MockReleaseOps::default();
        release.push_latest(Ok(make_manifest(tag, &[&asset])));
        release.push_asset(Ok(garbage));

        let err = run_upgrade(&args_default(), &release, &sys).unwrap_err();
        match &err {
            UpgradeError::PreSwap { step, .. } => {
                assert_eq!(*step, UpgradeStep::ExtractTarball);
            }
            other => panic!("expected PreSwap(ExtractTarball), got {other:?}"),
        }

        // Service was never touched — pre-swap means no rollback was
        // needed, so stop/start must have zero calls.
        assert!(
            sys.stopped_services.borrow().is_empty(),
            "stop_service must not run on extract-tarball failure"
        );
        assert!(
            sys.started_services.borrow().is_empty(),
            "start_service must not run on extract-tarball failure"
        );
        // Binary untouched and `.prev` never created.
        let installed = std::fs::read(&install).unwrap();
        assert!(installed.starts_with(b"#!/bin/sh"));
        let prev = prev_path(&install);
        assert!(!prev.exists(), ".prev must not exist after pre-swap abort");
    }

    /// N3 regression guard: make the `install_binary` step fail and
    /// assert `RolledBack { failed_step: InstallNewBinary }` with the
    /// `.prev` body restored onto `install_path`. Forces the failure by
    /// planting a *directory* at the sibling temp-file path
    /// (`<parent>/.aimx.upgrade.tmp`) that `install_binary` uses as its
    /// staging target: `fs::copy` refuses to overwrite a directory, so
    /// the install fails cleanly at the copy step. `preserve_previous_binary`
    /// runs before that and succeeds — putting us squarely in the
    /// post-stop / post-preserve rollback branch.
    #[test]
    fn run_upgrade_install_binary_failure_rolls_back_to_prev() {
        let mut sys = MockSystemOps::default();
        let tmp = TempDir::new().unwrap();
        let install_dir = tmp.path().join("bin");
        std::fs::create_dir(&install_dir).unwrap();
        let install = write_fake_binary(&install_dir);
        seed_install_path(&mut sys, install.clone());

        // Plant a directory where `install_binary` expects to write a
        // sibling temp file. `fs::copy(staged, &tmp)` fails with
        // "Is a directory" on Linux, deterministically driving the
        // InstallNewBinary branch of `attempt_rollback`.
        let blocker = install_dir.join(".aimx.upgrade.tmp");
        std::fs::create_dir(&blocker).unwrap();

        let tag = "v9.9.9-installfail";
        let target = current_target();
        let asset = tarball_filename(tag, target);
        let bytes = make_tarball(tag, target, b"would-be new body");
        let release = MockReleaseOps::default();
        release.push_latest(Ok(make_manifest(tag, &[&asset])));
        release.push_asset(Ok(bytes));

        let err = run_upgrade(&args_default(), &release, &sys).unwrap_err();

        match &err {
            UpgradeError::RolledBack { failed_step, .. } => {
                assert_eq!(
                    *failed_step,
                    UpgradeStep::InstallNewBinary,
                    "expected RolledBack at InstallNewBinary, got {failed_step:?}"
                );
            }
            other => panic!("expected RolledBack(InstallNewBinary), got {other:?}"),
        }

        // `.prev` was restored back onto `install_path` — the running
        // binary is the original.
        let installed = std::fs::read(&install).unwrap();
        assert!(
            installed.starts_with(b"#!/bin/sh"),
            "install path should hold the original binary after rollback"
        );
        // Rollback fired a start_service (the upgrade never got there).
        assert_eq!(
            sys.started_services.borrow().len(),
            1,
            "rollback should have called start_service exactly once"
        );
        // Service was stopped once (before the failed install).
        assert_eq!(
            sys.stopped_services.borrow().len(),
            1,
            "stop_service should have run before install failed"
        );
    }

    /// N4 regression guard: the canonical `RolledBack { failed_step:
    /// StartService }` outcome. The upgrade's `start_service` fails
    /// once, rollback's own `start_service` succeeds, and the service
    /// is left running on the previous binary. Requires the fail-once
    /// knob on `MockSystemOps` (`start_service_failures_remaining`) —
    /// the sticky `start_service_fails` bool would fail rollback's
    /// start too and yield `RollbackFailed` instead.
    #[test]
    fn rollback_succeeds_when_start_fails_once() {
        let mut sys = MockSystemOps::default();
        // Fail the upgrade's start, then allow rollback's start to succeed.
        sys.start_service_failures_remaining.set(1);
        let tmp = TempDir::new().unwrap();
        let install = write_fake_binary(tmp.path());
        seed_install_path(&mut sys, install.clone());

        let tag = "v9.9.9-startfailonce";
        let target = current_target();
        let asset = tarball_filename(tag, target);
        let bytes = make_tarball(tag, target, b"new body (start fails once)");
        let release = MockReleaseOps::default();
        release.push_latest(Ok(make_manifest(tag, &[&asset])));
        release.push_asset(Ok(bytes));

        let err = run_upgrade(&args_default(), &release, &sys).unwrap_err();
        match &err {
            UpgradeError::RolledBack {
                failed_step,
                previous_tag,
                ..
            } => {
                assert_eq!(*failed_step, UpgradeStep::StartService);
                assert_eq!(previous_tag, version::release_tag());
            }
            other => panic!("expected RolledBack(StartService), got {other:?}"),
        }

        // Binary restored to the original.
        let installed = std::fs::read(&install).unwrap();
        assert!(installed.starts_with(b"#!/bin/sh"));
        // Two start_service calls: one failed, one succeeded.
        assert_eq!(
            sys.started_services.borrow().len(),
            2,
            "expected start_service called twice (upgrade + rollback)"
        );
        assert_eq!(
            sys.start_service_failures_remaining.get(),
            0,
            "fail-once counter should be drained"
        );
    }

    #[test]
    fn stale_prev_overwritten_by_new_upgrade() {
        let mut sys = MockSystemOps::default();
        let tmp = TempDir::new().unwrap();
        let install = write_fake_binary(tmp.path());
        // Plant an existing .prev from a fictional earlier upgrade.
        let prev = prev_path(&install);
        std::fs::write(&prev, b"old prev body").unwrap();
        seed_install_path(&mut sys, install.clone());

        let tag = "v9.9.9-overwrite";
        let target = current_target();
        let asset = tarball_filename(tag, target);
        let bytes = make_tarball(tag, target, b"new body");
        let release = MockReleaseOps::default();
        release.push_latest(Ok(make_manifest(tag, &[&asset])));
        release.push_asset(Ok(bytes));

        run_upgrade(&args_default(), &release, &sys).unwrap();

        // .prev now holds the prior-generation binary (the one at
        // `install` before this run), not the old pre-planted body.
        let prev_bytes = std::fs::read(&prev).unwrap();
        assert!(prev_bytes.starts_with(b"#!/bin/sh"));
        assert_ne!(prev_bytes, b"old prev body");
    }
}
