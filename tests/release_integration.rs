//! Tier-2 integration tests.
//!
//! Gated behind `--features integration` so `cargo test` without the flag
//! never touches the network. CI runs `cargo test --features integration`
//! once per PR to exercise these paths against the live `0.0.0-fixture`
//! release (bare SemVer tag, no `-unknown-` vendor in tarball
//! filenames).
//!
//! The fixture release is **permanent** (never deleted). If its SHA-256
//! sums ever drift (e.g. because the release was re-published), update
//! the table below **and** `tests/fixtures/releases/0.0.0-fixture-sha256sums.txt`.

#![cfg(feature = "integration")]

use std::collections::HashMap;

const FIXTURE_TAG: &str = "0.0.0-fixture";
const FIXTURE_RELEASE_URL: &str =
    "https://api.github.com/repos/uzyn/aimx/releases/tags/0.0.0-fixture";

/// Expected SHA-256 checksums for the four fixture tarballs. Captured
/// from the fixture's `SHA256SUMS` asset.
fn expected_sums() -> HashMap<&'static str, &'static str> {
    HashMap::from([
        (
            "aimx-0.0.0-fixture-aarch64-linux-gnu.tar.gz",
            "42562c6dcf55c620ddc83de420da34fbeea3a0f7da771ef0cd867c444cfe2ddd",
        ),
        (
            "aimx-0.0.0-fixture-x86_64-linux-musl.tar.gz",
            "70de9987ad766bc959fac906acd95ee95343a411c410cc5e44b4c2d85d835ca7",
        ),
        (
            "aimx-0.0.0-fixture-aarch64-linux-musl.tar.gz",
            "ffb77b6b86007bd7e38f439c9af9e3e09fecef1f75d4cbc2329185476cc771cd",
        ),
        (
            "aimx-0.0.0-fixture-x86_64-linux-gnu.tar.gz",
            "08dd37f4d4013ad0d06189be168aeec7fddf56689fe87f2c61034d9b4aba9eff",
        ),
    ])
}

/// Map a canonical Rust target triple (`x86_64-unknown-linux-gnu`) to
/// the shortened artifact-target form (`x86_64-linux-gnu`) used in
/// tarball filenames. The canonical triple is still what
/// `aimx --version` prints in its `<target>` slot.
///
/// NOTE: the same `-unknown-` → `-` mapping is duplicated in four places
/// on purpose — `release.yml` (bash, drives the CI publish step),
/// `install.sh` (POSIX sh, drives curl-pipe installs), `src/upgrade.rs`
/// (Rust, drives `aimx upgrade`), and this file (Rust, drives the Tier-2
/// integration test). aimx ships as a single binary with no library crate
/// so the Rust sites can't share a helper; the sh and bash sites can't
/// reuse Rust at all. Keep all four in lockstep. See also the pinned
/// contract test `tarball_inner_dir_matches_artifact_target` in
/// `src/upgrade.rs`.
fn artifact_target(target: &str) -> String {
    target.replacen("-unknown-", "-", 1)
}

/// End-to-end: hit the real `0.0.0-fixture` release, pick the tarball that
/// matches the current target triple, download it, and compare its SHA-256
/// to both the value baked into this test and the `.sha256` asset GitHub
/// serves.
///
/// This exercises the `RealReleaseOps` surface — HTTPS enforcement, JSON
/// parsing, and asset fetching — against a real endpoint, without any
/// mocking. If it fails, one of: GitHub is down, the fixture release was
/// mutated, or `rustls` / TLS broke in CI.
#[test]
fn fixture_release_tarball_sha256_matches() {
    use std::process::Command;

    // Install the process-default rustls CryptoProvider. `ureq` is built
    // with `rustls-no-provider` (so it reuses whatever provider the rest
    // of the process has installed) but this test constructs a bare
    // `ureq::Agent` without going through `RealReleaseOps::fetch_bytes`,
    // which is where `install_rustls_provider()` normally runs. Without
    // this call, the first TLS handshake panics with "No CryptoProvider
    // for Rustls". `.ok()` — `install_default` returns `Err` if one is
    // already installed, which is fine (idempotent). Matches the pattern
    // used in `src/smtp/tls.rs` and `src/smtp/tests.rs`.
    rustls::crypto::aws_lc_rs::default_provider()
        .install_default()
        .ok();

    // Call the aimx binary's `--version` to discover the build target —
    // we don't link against the crate from integration tests, so we can't
    // call `version::target_triple()` directly. The version renderer
    // places the target between `) ` and ` built `.
    let output = Command::new(assert_cmd::cargo::cargo_bin("aimx"))
        .arg("--version")
        .output()
        .expect("run aimx --version");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let line = stdout.lines().next().unwrap();
    let target = line
        .split(") ")
        .nth(1)
        .and_then(|s| s.split(" built ").next())
        .expect("target triple in --version");

    let artifact = artifact_target(target);
    let tarball_name = format!("aimx-0.0.0-fixture-{artifact}.tar.gz");
    let expected = expected_sums();
    let expected_sha = match expected.get(tarball_name.as_str()) {
        Some(s) => *s,
        None => {
            eprintln!(
                "skipping — running target {target:?} is not one of the four \
                 fixture-release targets; tier-2 check does not apply"
            );
            return;
        }
    };

    // Fetch the release manifest directly so we have the real asset URLs
    // (redirects to release-assets.githubusercontent.com happen under the
    // hood; ureq follows them by default).
    let agent = ureq::Agent::config_builder()
        .timeout_global(Some(std::time::Duration::from_secs(60)))
        .user_agent(concat!("aimx/", env!("CARGO_PKG_VERSION"), " (tier2-test)"))
        .build()
        .new_agent();

    let mut resp = agent
        .get(FIXTURE_RELEASE_URL)
        .header("Accept", "application/vnd.github+json")
        .call()
        .expect("fetch fixture release manifest");
    assert!(
        (200..300).contains(&resp.status().as_u16()),
        "unexpected HTTP status {} for {FIXTURE_RELEASE_URL}",
        resp.status()
    );
    let body = resp
        .body_mut()
        .with_config()
        .limit(4 * 1024 * 1024)
        .read_to_vec()
        .expect("read manifest body");
    let manifest: serde_json::Value =
        serde_json::from_slice(&body).expect("manifest is valid JSON");

    assert_eq!(
        manifest["tag_name"].as_str(),
        Some(FIXTURE_TAG),
        "fixture tag_name drifted"
    );

    let assets = manifest["assets"].as_array().expect("assets array");
    let tarball_url = assets
        .iter()
        .find(|a| a["name"].as_str() == Some(tarball_name.as_str()))
        .and_then(|a| a["browser_download_url"].as_str())
        .unwrap_or_else(|| panic!("tarball {tarball_name} missing from fixture release"))
        .to_string();

    // Fetch the tarball and compute SHA-256.
    let mut resp = agent
        .get(&tarball_url)
        .call()
        .expect("fetch fixture tarball");
    assert!((200..300).contains(&resp.status().as_u16()));
    let tarball_bytes = resp
        .body_mut()
        .with_config()
        .limit(64 * 1024 * 1024)
        .read_to_vec()
        .expect("read tarball bytes");

    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(&tarball_bytes);
    let digest = hasher.finalize();
    let actual = hex(&digest);

    assert_eq!(
        actual, expected_sha,
        "SHA-256 drift for {tarball_name}: expected {expected_sha}, got {actual}"
    );

    // Cross-check against the published `.sha256` asset.
    let sha_asset_name = format!("{tarball_name}.sha256");
    let sha_url = assets
        .iter()
        .find(|a| a["name"].as_str() == Some(sha_asset_name.as_str()))
        .and_then(|a| a["browser_download_url"].as_str())
        .unwrap_or_else(|| panic!("{sha_asset_name} missing from fixture release"))
        .to_string();
    let mut resp = agent.get(&sha_url).call().expect("fetch .sha256 asset");
    assert!((200..300).contains(&resp.status().as_u16()));
    let sha_body = resp
        .body_mut()
        .with_config()
        .limit(1024)
        .read_to_vec()
        .expect("read .sha256");
    let sha_line = String::from_utf8(sha_body).expect("utf-8 .sha256");
    let published_hex = sha_line.split_whitespace().next().expect("hex in .sha256");
    assert_eq!(
        published_hex.to_ascii_lowercase(),
        actual,
        "published .sha256 disagrees with downloaded tarball digest"
    );
}

/// Wire-through check for the `aimx upgrade` verb.
///
/// Asserts that running `aimx upgrade --dry-run --version 0.0.0-fixture`
/// as a non-root user hits the root check and exits with the expected
/// refusal message — proving the verb is plumbed through `cli.rs` /
/// `main.rs` / `upgrade::run` without actually making any network calls.
///
/// **This test does NOT drive `RealReleaseOps::fetch_asset` end-to-end.**
/// The full production HTTPS path is exercised by
/// `real_release_ops_end_to_end_against_fixture_as_root`, which is
/// `#[ignore]`-gated and run under sudo in the `integration-isolation`
/// CI job. Without that sudo step the production-path-with-TLS gap
/// remains open in CI.
#[test]
fn real_release_ops_wireup_non_root_refuses() {
    use std::process::Command;

    // SAFETY: pointing at the tag URL lets us exercise `latest_release`
    // without depending on whichever release is `latest` on GitHub at
    // test time. Not strictly needed for the non-root refusal branch,
    // but kept for symmetry with the root path.
    unsafe {
        std::env::set_var("AIMX_RELEASE_MANIFEST_URL", FIXTURE_RELEASE_URL);
    }

    // This test is only valid when running non-root. Under sudo, the
    // sibling `real_release_ops_end_to_end_against_fixture_as_root`
    // test drives the full HTTPS path instead.
    let euid = unsafe { libc::geteuid() };
    if euid == 0 {
        eprintln!(
            "skipping — running as root; the sibling #[ignore]-gated test \
             covers the root path under the `integration-isolation` CI job"
        );
        unsafe {
            std::env::remove_var("AIMX_RELEASE_MANIFEST_URL");
        }
        return;
    }

    let out = Command::new(assert_cmd::cargo::cargo_bin("aimx"))
        .args(["upgrade", "--dry-run", "--version", "0.0.0-fixture"])
        .output()
        .expect("run aimx upgrade --dry-run");
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        combined.contains("aimx upgrade requires root"),
        "expected root-check refusal, got: {combined}"
    );
    unsafe {
        std::env::remove_var("AIMX_RELEASE_MANIFEST_URL");
    }
}

/// Drives the **production** `RealReleaseOps`
/// path end-to-end against the live `0.0.0-fixture` release. Runs
/// `aimx upgrade --dry-run --version 0.0.0-fixture` as root —
/// `--dry-run` stops after `fetch_asset` and never touches the
/// filesystem or the service, so the test is safe under sudo.
///
/// Gated by `#[ignore]` + `AIMX_INTEGRATION_SUDO=1` so a casual
/// `cargo test --features integration` does not shell out under root.
/// The `.github/workflows/ci.yml` `integration-isolation` job runs
/// this test under sudo via:
///
/// ```text
/// sudo -E env "PATH=$PATH" AIMX_INTEGRATION_SUDO=1 \
///   cargo test --features integration --test release_integration \
///   -- --ignored --exact \
///   real_release_ops_end_to_end_against_fixture_as_root
/// ```
///
/// which is the sibling pattern to the `isolation` / `uds_authz` /
/// MAILBOX-CRUD sudo-gated tests already in that job.
#[test]
#[ignore = "requires root + AIMX_INTEGRATION_SUDO=1; run in CI under sudo"]
fn real_release_ops_end_to_end_against_fixture_as_root() {
    use std::process::Command;

    let euid = unsafe { libc::geteuid() };
    assert_eq!(
        euid, 0,
        "this test must run as root so `aimx upgrade` passes the root \
         check and exercises RealReleaseOps::fetch_asset against the \
         fixture release; re-run under sudo with AIMX_INTEGRATION_SUDO=1"
    );
    assert_eq!(
        std::env::var("AIMX_INTEGRATION_SUDO").ok().as_deref(),
        Some("1"),
        "AIMX_INTEGRATION_SUDO=1 must be set to opt into network + root \
         integration tests"
    );

    // Discover the running target triple.
    let out = Command::new(assert_cmd::cargo::cargo_bin("aimx"))
        .arg("--version")
        .output()
        .expect("run aimx --version");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let line = stdout.lines().next().unwrap();
    let target = line
        .split(") ")
        .nth(1)
        .and_then(|s| s.split(" built ").next())
        .expect("target in --version");

    let artifact = artifact_target(target);
    let tarball_name = format!("aimx-0.0.0-fixture-{artifact}.tar.gz");
    let expected = expected_sums();
    if !expected.contains_key(tarball_name.as_str()) {
        eprintln!("skipping — target {target:?} is not a fixture-release target");
        return;
    }

    // Install the rustls provider explicitly so when the spawned `aimx`
    // subprocess constructs `RealReleaseOps`, its internal
    // `install_rustls_provider` idempotently finds the slot taken.
    // (Belt-and-braces — the subprocess has its own provider init.)
    rustls::crypto::aws_lc_rs::default_provider()
        .install_default()
        .ok();

    // Drive the exact production path — `RealReleaseOps::release_by_tag`
    // → `fetch_asset` over real HTTPS — via `aimx upgrade --dry-run`.
    // The dry-run exits cleanly after the fetch without touching the
    // service or filesystem, so this is safe to run under sudo in CI.
    //
    // Point the env override at the canonical `/releases/latest` URL
    // rather than the per-tag URL — `RealReleaseOps::release_by_tag_url`
    // runs `rewrite_to_tag` on the override, which maps `.../latest`
    // to `.../tags/<tag>`. Passing the per-tag URL directly would
    // double-append `/tags/<tag>` and 404.
    let out = Command::new(assert_cmd::cargo::cargo_bin("aimx"))
        .args(["upgrade", "--dry-run", "--version", "0.0.0-fixture"])
        .env(
            "AIMX_RELEASE_MANIFEST_URL",
            "https://api.github.com/repos/uzyn/aimx/releases/latest",
        )
        .output()
        .expect("run aimx upgrade --dry-run (root)");
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.status.success(), "dry-run failed: {combined}");
    assert!(
        combined.contains("0.0.0-fixture"),
        "expected target tag in dry-run output: {combined}"
    );
    assert!(
        combined.contains(&tarball_name),
        "expected tarball name {tarball_name} in dry-run output: {combined}"
    );
}

fn hex(bytes: &[u8]) -> String {
    const H: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(H[(b >> 4) as usize] as char);
        out.push(H[(b & 0x0f) as usize] as char);
    }
    out
}
