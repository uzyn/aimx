//! Hook-fire mailbox isolation integration test.
//!
//! Companion to `tests/isolation.rs`, which proves that an inbound `.md`
//! delivered to `aimx-it-alice` cannot be read by `aimx-it-bob`. This
//! file exercises the *hook* angle: simulate the daemon firing an
//! `on_receive` hook that runs as the mailbox owner (`alice`), and
//! confirm that hook child cannot reach into another mailbox's
//! directory.
//!
//! Three subprocess shapes are checked, mirroring the three directory
//! traversal primitives a hook child might attempt:
//!
//! 1. `/bin/cat <bob>/test.md` — read a file inside bob's inbox.
//! 2. `/bin/ls <bob>/`         — directory listing.
//! 3. `/bin/test -r <bob>/`    — readability check.
//!
//! All three must fail when the caller is `alice` (uid != bob, dir
//! mode 0o700 owned by bob:bob), and the failure must surface with the
//! kernel's `EACCES` rendering ("Permission denied" in current GNU
//! coreutils; the assertion accepts any reasonable rendering).
//!
//! Test users (`aimx-test-alice`, `aimx-test-bob`) and their primary
//! groups must be present on the host. Gated behind both `#[ignore]`
//! and `AIMX_INTEGRATION_SUDO=1` so the test only runs inside a CI job
//! that explicitly elevates with `sudo`. The CI workflow under
//! `.github/workflows/ci.yml` provisions the users via `useradd`
//! before invoking `cargo test --test mailbox_isolation -- --ignored`.

#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const ALICE: &str = "aimx-test-alice";
const BOB: &str = "aimx-test-bob";

/// RAII guard removing the test users on drop. Best-effort: a missing
/// user is fine (re-runs of the same job, manual cleanup).
struct UserTeardown;

impl Drop for UserTeardown {
    fn drop(&mut self) {
        let _ = Command::new("userdel").arg(ALICE).status();
        let _ = Command::new("userdel").arg(BOB).status();
    }
}

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

/// Set ownership and mode on `path` via the libc syscalls. Used during
/// fixture setup; tests run as root (sudo) so EPERM is unexpected.
fn chown_chmod(path: &Path, uid: u32, gid: u32, mode: u32) {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let c_path = CString::new(path.as_os_str().as_bytes()).expect("path has NUL");
    unsafe {
        let rc = libc::chown(c_path.as_ptr(), uid, gid);
        assert_eq!(rc, 0, "chown {} failed: {}", path.display(), errno_str());
        let rc = libc::chmod(c_path.as_ptr(), mode as libc::mode_t);
        assert_eq!(rc, 0, "chmod {} failed: {}", path.display(), errno_str());
    }
}

fn errno_str() -> String {
    std::io::Error::last_os_error().to_string()
}

/// Spawn `argv` via `runuser -u <user> -- <argv>` and return
/// `(success, stderr)`. Closes stdin/stdout so only stderr is captured.
fn spawn_as(user: &str, argv: &[&str]) -> (bool, String) {
    let mut cmd = Command::new("runuser");
    cmd.arg("-u").arg(user).arg("--");
    for a in argv {
        cmd.arg(a);
    }
    let output = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("failed to spawn runuser");
    (
        output.status.success(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

fn make_world_traversable(path: &Path) {
    // Walk up from `path` to root and ensure every component along the
    // way is at least `o+rx`. The CARGO_TARGET_DIR-derived tempdir lives
    // under the user's home on most CI runners; if any ancestor is
    // mode 0700 the runuser-into-alice invocation can't even reach
    // `/tmp/...`. Keep the parent dirs `0o755` so traversal works; the
    // per-mailbox dir under test is what enforces isolation.
    let mut cur = path.to_path_buf();
    while let Some(parent) = cur.parent().map(|p| p.to_path_buf()) {
        if parent == cur {
            break;
        }
        if let Ok(meta) = std::fs::metadata(&parent) {
            let mode = meta.permissions().mode();
            if mode & 0o001 == 0 || mode & 0o004 == 0 {
                let _ =
                    std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(mode | 0o5));
            }
        }
        cur = parent;
    }
}

fn test_root() -> PathBuf {
    // /tmp is world-traversable on Ubuntu runners; keep the fixture
    // under /tmp so we don't have to fight with the runner's $HOME
    // perms.
    let pid = std::process::id();
    PathBuf::from(format!("/tmp/aimx-mb-isolation-{pid}"))
}

#[test]
#[ignore = "requires two test uids on the host (aimx-test-alice, aimx-test-bob)"]
fn alice_hook_cannot_read_bob_inbox() {
    if std::env::var_os("AIMX_INTEGRATION_SUDO").is_none() {
        eprintln!(
            "AIMX_INTEGRATION_SUDO is not set; skipping (test requires root + user provisioning)."
        );
        return;
    }
    assert_eq!(
        unsafe { libc::geteuid() },
        0,
        "mailbox isolation test must run as root (AIMX_INTEGRATION_SUDO=1 + sudo)"
    );

    let _guard = UserTeardown;
    ensure_user(ALICE);
    ensure_user(BOB);

    let alice_uid = uid_of(ALICE);
    let alice_gid = alice_uid;
    let bob_uid = uid_of(BOB);
    let bob_gid = bob_uid;

    let root = test_root();
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o755)).unwrap();
    make_world_traversable(&root);

    let inbox_root = root.join("inbox");
    std::fs::create_dir_all(&inbox_root).unwrap();
    std::fs::set_permissions(&inbox_root, std::fs::Permissions::from_mode(0o755)).unwrap();

    // Per-mailbox directories: each owned by its mailbox owner, mode
    // 0o700. This is the isolation invariant the dir-perm migration
    // installs at setup / mailbox-create time.
    let alice_inbox = inbox_root.join(ALICE);
    std::fs::create_dir_all(&alice_inbox).unwrap();
    chown_chmod(&alice_inbox, alice_uid, alice_gid, 0o700);

    let bob_inbox = inbox_root.join(BOB);
    std::fs::create_dir_all(&bob_inbox).unwrap();
    chown_chmod(&bob_inbox, bob_uid, bob_gid, 0o700);

    // Drop a fixture email into bob's inbox and chown it to bob.
    let bob_email = bob_inbox.join("2026-04-15-000000-test.md");
    std::fs::write(
        &bob_email,
        b"+++\nmailbox = \"aimx-test-bob\"\n+++\n\nsecret\n",
    )
    .unwrap();
    chown_chmod(&bob_email, bob_uid, bob_gid, 0o600);

    let bob_email_str = bob_email
        .to_str()
        .expect("bob email path must be valid utf-8");
    let bob_inbox_str = bob_inbox
        .to_str()
        .expect("bob inbox path must be valid utf-8");

    // (1) /bin/cat as alice on bob's email file.
    let (ok, stderr) = spawn_as(ALICE, &["/bin/cat", bob_email_str]);
    assert!(
        !ok,
        "alice must NOT be able to read bob's email; stderr: {stderr}"
    );
    assert!(
        stderr.to_ascii_lowercase().contains("permission denied"),
        "/bin/cat must surface EACCES; got stderr: {stderr}"
    );

    // (2) /bin/ls as alice on bob's inbox dir.
    let (ok, stderr) = spawn_as(ALICE, &["/bin/ls", bob_inbox_str]);
    assert!(
        !ok,
        "alice must NOT be able to list bob's inbox; stderr: {stderr}"
    );
    assert!(
        stderr.to_ascii_lowercase().contains("permission denied"),
        "/bin/ls must surface EACCES; got stderr: {stderr}"
    );

    // (3) /bin/test -r as alice on bob's inbox dir. `test -r` exits
    // non-zero when the path is unreadable; it does not write to
    // stderr, so the assertion is on exit status alone.
    let (ok, _stderr) = spawn_as(ALICE, &["/bin/test", "-r", bob_inbox_str]);
    assert!(
        !ok,
        "alice's readability check on bob's inbox must fail (test -r returns non-zero)"
    );

    // Sanity check: the same probes succeed when run as bob himself,
    // so the failures above are an isolation property and not a
    // fixture mistake.
    let (ok, stderr) = spawn_as(BOB, &["/bin/cat", bob_email_str]);
    assert!(
        ok,
        "bob must be able to read his own email; stderr: {stderr}"
    );

    let _ = std::fs::remove_dir_all(&root);
    drop(_guard);
}
