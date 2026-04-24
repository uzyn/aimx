//! Per-mailbox isolation integration test.
//!
//! This test creates two real Linux users (`aimx-it-alice`, `aimx-it-bob`)
//! via `useradd`, delivers an email to alice's inbox, and asserts that a
//! subprocess running as bob cannot read the file (EACCES) while a
//! subprocess running as alice can.
//!
//! Gated on both `#[ignore]` and the `AIMX_INTEGRATION_SUDO=1` env var so
//! it only runs inside a CI job that explicitly elevates. The
//! `integration-isolation` GitHub Actions job in
//! `.github/workflows/ci.yml` does exactly that.
//!
//! Teardown uses a `Drop` guard so `userdel` always runs, even on
//! assertion failure. The guard is also invoked via an explicit `drop`
//! at the end of the test so failures inside the guard's `Drop` don't
//! mask the test's assertion panic.

#![cfg(unix)]

use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const ALICE: &str = "aimx-it-alice";
const BOB: &str = "aimx-it-bob";

/// RAII guard that deletes the test users on drop. Added up front so
/// any panic / failure inside the test body still triggers teardown.
struct UserTeardown;

impl Drop for UserTeardown {
    fn drop(&mut self) {
        // Best-effort; `userdel` failures on a missing user are fine.
        let _ = Command::new("userdel").arg(ALICE).status();
        let _ = Command::new("userdel").arg(BOB).status();
    }
}

fn useradd(name: &str) {
    // `--system` keeps the uid below the regular range; no home dir is
    // created so teardown is just a `userdel`. `nologin` shell so the
    // account is unusable as an interactive login even if the test
    // leaves it behind on a catastrophic failure.
    let status = Command::new("useradd")
        .arg("--system")
        .arg("--no-create-home")
        .arg("--shell")
        .arg("/usr/sbin/nologin")
        .arg(name)
        .status()
        .expect("failed to spawn useradd");
    assert!(
        status.success() || {
            // If the user already exists from a prior aborted run, our
            // Drop guard will clean up at the end.
            let check = Command::new("id").arg(name).status().ok();
            matches!(check, Some(s) if s.success())
        },
        "useradd failed for {name}"
    );
}

fn uid_of(name: &str) -> u32 {
    let output = Command::new("id")
        .arg("-u")
        .arg(name)
        .output()
        .expect("failed to run id -u");
    assert!(
        output.status.success(),
        "id -u {name} failed: {:?}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse::<u32>()
        .expect("uid must parse")
}

/// Run `cat <path>` as `user` via `runuser`. Returns the subprocess
/// exit status + stderr so the caller can assert on EACCES vs success.
fn read_as(user: &str, path: &Path) -> (bool, String) {
    let output = Command::new("runuser")
        .arg("-u")
        .arg(user)
        .arg("--")
        .arg("cat")
        .arg(path)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .expect("failed to spawn runuser");
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    (output.status.success(), stderr)
}

fn aimx_binary_path() -> PathBuf {
    // `assert_cmd::cargo::cargo_bin` is the canonical way but adding it
    // here would require a dev-dep rebuild for a single symbol. The
    // CARGO_BIN_EXE_<name> env var is populated for integration tests.
    PathBuf::from(env!("CARGO_BIN_EXE_aimx"))
}

#[test]
#[ignore]
fn alice_reads_own_mailbox_bob_gets_eacces() {
    if std::env::var_os("AIMX_INTEGRATION_SUDO").is_none() {
        eprintln!("AIMX_INTEGRATION_SUDO is not set; skipping (test requires root + user-create).");
        return;
    }
    assert_eq!(
        unsafe { libc::geteuid() },
        0,
        "isolation test must run as root (AIMX_INTEGRATION_SUDO=1 + sudo)"
    );

    let _guard = UserTeardown;
    useradd(ALICE);
    useradd(BOB);
    let alice_uid = uid_of(ALICE);
    let alice_gid = alice_uid;

    // Bare minimum config + data dir under `/tmp` (which is traversable
    // by non-root by default). Using `/tmp/aimx-it-<pid>` keeps the
    // test hermetic.
    let tmp_root = std::env::temp_dir().join(format!("aimx-it-isolation-{}", std::process::id()));
    std::fs::create_dir_all(&tmp_root).unwrap();
    // Parent dir must be 0o755 so bob can traverse down to his attempt.
    // The per-mailbox dir is what enforces isolation.
    let perms = std::os::unix::fs::PermissionsExt::from_mode(0o755);
    std::fs::set_permissions(&tmp_root, perms).unwrap();

    let data_dir = tmp_root.join("data");
    std::fs::create_dir_all(&data_dir).unwrap();
    std::fs::set_permissions(
        &data_dir,
        std::os::unix::fs::PermissionsExt::from_mode(0o755),
    )
    .unwrap();
    std::fs::create_dir_all(data_dir.join("inbox")).unwrap();
    std::fs::set_permissions(
        data_dir.join("inbox"),
        std::os::unix::fs::PermissionsExt::from_mode(0o755),
    )
    .unwrap();

    let config_content = format!(
        "domain = \"it.example.com\"\n\
         data_dir = \"{data_dir}\"\n\n\
         [mailboxes.catchall]\n\
         address = \"*@it.example.com\"\n\
         owner = \"aimx-catchall\"\n\n\
         [mailboxes.{alice}]\n\
         address = \"{alice}@it.example.com\"\n\
         owner = \"{alice}\"\n\n\
         [mailboxes.{bob}]\n\
         address = \"{bob}@it.example.com\"\n\
         owner = \"{bob}\"\n",
        data_dir = data_dir.display(),
        alice = ALICE,
        bob = BOB,
    );
    let config_path = tmp_root.join("config.toml");
    std::fs::write(&config_path, &config_content).unwrap();

    // DKIM keys are required for `aimx ingest` to construct a
    // resolved config — the stdin ingest path loads the config but
    // does not actually DKIM-sign anything on inbound.
    let dkim_dir = tmp_root.join("dkim");
    std::fs::create_dir_all(&dkim_dir).unwrap();
    // Minimal 2048-bit key check is done via `aimx dkim-keygen` below.
    let keygen_status = Command::new(aimx_binary_path())
        .env("AIMX_CONFIG_DIR", &tmp_root)
        .env("AIMX_DATA_DIR", &data_dir)
        .arg("dkim-keygen")
        .arg("--force")
        .status()
        .expect("dkim-keygen failed to spawn");
    assert!(keygen_status.success(), "dkim-keygen failed");

    // Pre-create alice's inbox and chown to alice:alice 0700 so ingest
    // has a target it can populate.
    let alice_inbox = data_dir.join("inbox").join(ALICE);
    std::fs::create_dir_all(&alice_inbox).unwrap();
    unsafe {
        libc::chown(
            std::ffi::CString::new(alice_inbox.as_os_str().as_encoded_bytes())
                .unwrap()
                .as_ptr(),
            alice_uid,
            alice_gid,
        );
        libc::chmod(
            std::ffi::CString::new(alice_inbox.as_os_str().as_encoded_bytes())
                .unwrap()
                .as_ptr(),
            0o700,
        );
    }

    // Ingest a small .eml into alice's mailbox via the stdin path.
    let eml = b"From: sender@example.com\r\n\
                 To: aimx-it-alice@it.example.com\r\n\
                 Subject: hello\r\n\
                 Message-ID: <isolation-test@example.com>\r\n\
                 Date: Thu, 01 Jan 2026 12:00:00 +0000\r\n\
                 \r\n\
                 body for isolation test\r\n";
    let mut ingest = Command::new(aimx_binary_path())
        .env("AIMX_CONFIG_DIR", &tmp_root)
        .env("AIMX_DATA_DIR", &data_dir)
        .arg("ingest")
        .arg(format!("{ALICE}@it.example.com"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("aimx ingest failed to spawn");
    use std::io::Write;
    ingest
        .stdin
        .as_mut()
        .unwrap()
        .write_all(eml)
        .expect("write to ingest stdin");
    let ingest_out = ingest.wait_with_output().expect("ingest wait");
    assert!(
        ingest_out.status.success(),
        "ingest must succeed; stderr: {}",
        String::from_utf8_lossy(&ingest_out.stderr)
    );

    // Find the one .md file alice just received.
    let md_files: Vec<PathBuf> = std::fs::read_dir(&alice_inbox)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "md"))
        .collect();
    assert_eq!(md_files.len(), 1, "expected exactly one delivered email");
    let md_path = &md_files[0];

    // File ownership + mode assertions.
    let md_meta = std::fs::metadata(md_path).unwrap();
    assert_eq!(
        md_meta.uid(),
        alice_uid,
        ".md must be owned by {ALICE} (uid {alice_uid})"
    );
    assert_eq!(md_meta.mode() & 0o777, 0o600, ".md must be mode 0o600");

    let dir_meta = std::fs::metadata(&alice_inbox).unwrap();
    assert_eq!(
        dir_meta.uid(),
        alice_uid,
        "inbox/alice must be owned by {ALICE}"
    );
    assert_eq!(dir_meta.mode() & 0o777, 0o700, "inbox/alice must be 0o700");

    // Bob cannot read.
    let (bob_ok, bob_err) = read_as(BOB, md_path);
    assert!(
        !bob_ok,
        "bob must NOT be able to read alice's email; stderr: {bob_err}"
    );
    assert!(
        bob_err.contains("Permission denied") || bob_err.to_ascii_lowercase().contains("denied"),
        "bob's read must fail with permission denied; stderr: {bob_err}"
    );

    // Alice can.
    let (alice_ok, alice_err) = read_as(ALICE, md_path);
    assert!(
        alice_ok,
        "alice must be able to read her own email; stderr: {alice_err}"
    );

    // Best-effort cleanup of the tmp dir; teardown of users happens via
    // the UserTeardown guard on scope exit.
    let _ = std::fs::remove_dir_all(&tmp_root);
    drop(_guard);
}
