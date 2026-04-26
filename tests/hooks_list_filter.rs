//! Integration test: `aimx hooks list` filters output by caller euid.
//!
//! Reuses the `aimx-test-alice` / `aimx-test-bob` fixture also used by
//! `tests/uds_authz.rs` and `tests/mailbox_isolation.rs`. Spins up a
//! temp config with two mailboxes (one per test user, each carrying a
//! hook) and runs `aimx hooks list` once as alice, once as bob, once as
//! root via `runuser -u <user> -- env AIMX_CONFIG_DIR=… <binary> hooks list`.
//! Asserts:
//!
//! - Alice sees only her hook; bob's hook is absent (no name leak).
//! - Bob sees only his hook; alice's hook is absent.
//! - Root sees both hooks.
//!
//! Coverage gap rationale: `src/mailbox.rs::caller_owns` is unit-tested,
//! but no test wired the predicate end-to-end through `aimx hooks list`.
//! A future refactor that bypassed the filter (e.g., dropped the
//! `rows.retain(...)` call in `src/hooks.rs::list`) would have shipped
//! silently. This test pins the wire-level behaviour.
//!
//! Gated on both `#[ignore]` and `AIMX_INTEGRATION_SUDO=1` so it only
//! runs inside a CI step that explicitly elevates with `sudo`.

#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const ALICE: &str = "aimx-test-alice";
const BOB: &str = "aimx-test-bob";

fn ensure_user(name: &str) {
    let already = Command::new("id")
        .arg(name)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if already {
        return;
    }
    let status = Command::new("useradd")
        .arg("--system")
        .arg("--no-create-home")
        .arg("--shell")
        .arg("/usr/sbin/nologin")
        .arg(name)
        .status()
        .expect("failed to spawn useradd");
    assert!(status.success(), "useradd failed for {name}");
}

fn aimx_binary_path() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_aimx"))
}

fn rand_suffix() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
}

/// Build a tempdir containing a world-readable `config.toml` with two
/// mailboxes (alice + bob), each carrying one hook with a distinctive
/// name we can assert against. Returns the tempdir root so the caller
/// can pass it as `AIMX_CONFIG_DIR`.
fn write_test_config() -> PathBuf {
    let tmp_root = std::env::temp_dir().join(format!(
        "aimx-hooks-list-filter-{}-{}",
        std::process::id(),
        rand_suffix()
    ));
    std::fs::create_dir_all(&tmp_root).unwrap();
    std::fs::set_permissions(&tmp_root, PermissionsExt::from_mode(0o755)).unwrap();

    let data_dir = tmp_root.join("data");
    std::fs::create_dir_all(data_dir.join("inbox")).unwrap();
    std::fs::create_dir_all(data_dir.join("sent")).unwrap();
    std::fs::set_permissions(&data_dir, PermissionsExt::from_mode(0o755)).unwrap();

    let config_content = format!(
        "domain = \"hooks-list-filter.example.com\"\n\
         data_dir = \"{data_dir}\"\n\
         dkim_selector = \"aimx\"\n\n\
         [mailboxes.catchall]\n\
         address = \"*@hooks-list-filter.example.com\"\n\
         owner = \"aimx-catchall\"\n\n\
         [mailboxes.{alice}]\n\
         address = \"{alice}@hooks-list-filter.example.com\"\n\
         owner = \"{alice}\"\n\n\
         [[mailboxes.{alice}.hooks]]\n\
         name = \"alice-hook-marker\"\n\
         event = \"on_receive\"\n\
         cmd = [\"/bin/true\"]\n\n\
         [mailboxes.{bob}]\n\
         address = \"{bob}@hooks-list-filter.example.com\"\n\
         owner = \"{bob}\"\n\n\
         [[mailboxes.{bob}.hooks]]\n\
         name = \"bob-hook-marker\"\n\
         event = \"on_receive\"\n\
         cmd = [\"/bin/true\"]\n",
        data_dir = data_dir.display(),
        alice = ALICE,
        bob = BOB,
    );
    let config_path = tmp_root.join("config.toml");
    std::fs::write(&config_path, &config_content).unwrap();
    // World-readable so the non-root test users can load it. Production
    // `/etc/aimx/config.toml` is `0640 root:root`; this test's tempdir
    // is opt-in to wider perms so the filter logic is reachable from
    // the runuser-invoked CLI.
    std::fs::set_permissions(&config_path, PermissionsExt::from_mode(0o644)).unwrap();
    tmp_root
}

/// Copy the test binary out of `target/debug/` (whose ancestors aren't
/// world-traversable on the GitHub Actions runner: `/home/runner/...`)
/// into a `0755` location under the test's own tempdir. Without this,
/// `runuser -u alice -- <binary>` fails with `Permission denied`
/// before the binary even gets to exec.
fn install_aimx_in(dest_dir: &Path) -> PathBuf {
    let src = aimx_binary_path();
    let dst = dest_dir.join("aimx");
    std::fs::copy(&src, &dst).expect("failed to copy aimx binary into tempdir");
    std::fs::set_permissions(&dst, PermissionsExt::from_mode(0o755))
        .expect("failed to chmod copied aimx binary");
    dst
}

/// Run `aimx hooks list` as `user` (None = current process, i.e. root)
/// using `binary` against the supplied `AIMX_CONFIG_DIR`. Returns
/// combined stdout + stderr (since the CLI sometimes uses both).
fn run_hooks_list_as(user: Option<&str>, binary: &Path, config_dir: &Path) -> String {
    let output = match user {
        Some(u) => Command::new("runuser")
            .arg("-u")
            .arg(u)
            .arg("--")
            .arg("env")
            .arg(format!("AIMX_CONFIG_DIR={}", config_dir.display()))
            .arg("NO_COLOR=1")
            .arg(binary)
            .arg("hooks")
            .arg("list")
            .output()
            .expect("failed to spawn runuser env aimx hooks list"),
        None => Command::new(binary)
            .env("AIMX_CONFIG_DIR", config_dir)
            .env("NO_COLOR", "1")
            .arg("hooks")
            .arg("list")
            .output()
            .expect("failed to spawn aimx hooks list"),
    };
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.status.success(),
        "aimx hooks list exited non-zero (user={user:?}): {combined}"
    );
    combined
}

#[test]
#[ignore = "requires two test uids on the host (aimx-test-alice, aimx-test-bob)"]
fn hooks_list_filters_to_caller_owned_mailboxes_for_non_root_callers() {
    if std::env::var("AIMX_INTEGRATION_SUDO").as_deref() != Ok("1") {
        eprintln!("skipping: AIMX_INTEGRATION_SUDO=1 not set");
        return;
    }
    assert_eq!(
        unsafe { libc::geteuid() },
        0,
        "this test must run as root (sudo cargo test ...)"
    );

    ensure_user(ALICE);
    ensure_user(BOB);
    // Catchall owner is referenced in the test config; loader bails
    // (orphan owner) without it.
    let _ = Command::new("useradd")
        .arg("--system")
        .arg("--no-create-home")
        .arg("--shell")
        .arg("/usr/sbin/nologin")
        .arg("aimx-catchall")
        .status();

    let config_dir = write_test_config();
    let aimx_bin = install_aimx_in(&config_dir);

    // Alice sees only her hook.
    let alice_out = run_hooks_list_as(Some(ALICE), &aimx_bin, &config_dir);
    assert!(
        alice_out.contains("alice-hook-marker"),
        "alice should see her own hook in `aimx hooks list` output: {alice_out}"
    );
    assert!(
        !alice_out.contains("bob-hook-marker"),
        "alice MUST NOT see bob's hook (filter regression): {alice_out}"
    );

    // Bob sees only his hook.
    let bob_out = run_hooks_list_as(Some(BOB), &aimx_bin, &config_dir);
    assert!(
        bob_out.contains("bob-hook-marker"),
        "bob should see his own hook in `aimx hooks list` output: {bob_out}"
    );
    assert!(
        !bob_out.contains("alice-hook-marker"),
        "bob MUST NOT see alice's hook (filter regression): {bob_out}"
    );

    // Root sees both — confirms the filter is uid-gated, not always-on.
    let root_out = run_hooks_list_as(None, &aimx_bin, &config_dir);
    assert!(
        root_out.contains("alice-hook-marker"),
        "root should see alice's hook: {root_out}"
    );
    assert!(
        root_out.contains("bob-hook-marker"),
        "root should see bob's hook: {root_out}"
    );

    // Cleanup tempdir; the test users are owned by the CI workflow.
    let _ = std::fs::remove_dir_all(&config_dir);
}
