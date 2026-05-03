//! Cross-user UDS authz integration tests.
//!
//! Reuses the `aimx-test-alice` / `aimx-test-bob` fixture also used by
//! `tests/mailbox_isolation.rs`. Spins up a real `aimx serve` subprocess
//! under a tempdir, then issues raw `AIMX/1` frames as alice/bob/root
//! via `runuser -u <user> python3 -c ...`.
//!
//! Per-verb ownership matrix (mirrors `src/uds_authz.rs`):
//!
//! | Verb                                | Owner | Other  | Root |
//! |-------------------------------------|-------|--------|------|
//! | `SEND` (as alice)                   | OK    | EACCES | OK   |
//! | `MARK-READ` / `MARK-UNREAD`         | OK*   | EACCES | OK*  |
//! | `HOOK-CREATE` / `HOOK-DELETE`       | OK    | EACCES | OK   |
//! | `MAILBOX-CREATE`                    | OK†   | OK†    | OK   |
//! | `MAILBOX-DELETE`                    | OK    | NOMBX‡ | OK   |
//!
//! `*` MARK targets a non-existent email so the OK path surfaces as
//! `NOTFOUND` after authz accepts; the EACCES path exits before the
//! filesystem lookup.
//!
//! `†` Sprint 1 user-mailbox track: non-root `MAILBOX-CREATE` succeeds
//! but the wire `Owner:` header is dropped on the floor — the on-disk
//! owner is bound to the caller's `SO_PEERCRED` uid, so a cross-uid
//! create attempt simply produces a mailbox owned by the caller.
//!
//! `‡` Cross-uid `MAILBOX-DELETE` returns the canonical no-such-mailbox
//! response (NFR2: no information leak about whose mailbox it is).
//!
//! Gated on both `#[ignore]` and `AIMX_INTEGRATION_SUDO=1` so it only
//! runs inside a CI step that explicitly elevates with `sudo`.

#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const ALICE: &str = "aimx-test-alice";
const BOB: &str = "aimx-test-bob";

/// RAII guard: kill the serve subprocess and remove the test users on
/// drop. Drop runs on assertion failure too, so the CI step stays
/// hermetic.
struct Teardown {
    serve: Option<Child>,
}

impl Drop for Teardown {
    fn drop(&mut self) {
        if let Some(mut child) = self.serve.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        // We do not `userdel` here — the CI workflow owns user
        // provisioning so re-runs in the same job stay deterministic.
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

fn aimx_binary_path() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_aimx"))
}

fn wait_for_socket(path: &Path) -> bool {
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline {
        if path.exists() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    false
}

fn raw_send_frame(from: &str, to: &str) -> String {
    let body = format!(
        "From: {from}\r\n\
         To: {to}\r\n\
         Subject: authz test\r\n\
         Date: Thu, 01 Jan 2026 12:00:00 +0000\r\n\
         Message-ID: <authz-test@example.com>\r\n\
         \r\n\
         authz body\r\n"
    );
    format!(
        "AIMX/1 SEND\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    )
}

fn raw_mark_frame(verb: &str, mailbox: &str, id: &str) -> String {
    format!("AIMX/1 {verb}\r\nMailbox: {mailbox}\r\nId: {id}\r\nContent-Length: 0\r\n\r\n")
}

fn raw_hook_create_frame(mailbox: &str, name: &str, cmd: &[&str]) -> String {
    let cmd_arr: Vec<String> = cmd.iter().map(|s| s.to_string()).collect();
    let body_json = serde_json::json!({ "cmd": cmd_arr });
    let body = body_json.to_string();
    format!(
        "AIMX/1 HOOK-CREATE\r\nMailbox: {mailbox}\r\nEvent: on_receive\r\nName: {name}\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    )
}

fn raw_hook_delete_frame(name: &str) -> String {
    format!("AIMX/1 HOOK-DELETE\r\nHook-Name: {name}\r\nContent-Length: 0\r\n\r\n")
}

fn raw_mailbox_create_frame(name: &str, owner: &str) -> String {
    // The MAILBOX-CREATE / MAILBOX-DELETE codec uses `Name:` for the
    // mailbox-name header (see src/send_protocol.rs::parse_mailbox_crud_headers).
    // Other verbs use `Mailbox:`; do not unify.
    format!("AIMX/1 MAILBOX-CREATE\r\nName: {name}\r\nOwner: {owner}\r\nContent-Length: 0\r\n\r\n")
}

fn raw_mailbox_delete_frame(name: &str) -> String {
    format!("AIMX/1 MAILBOX-DELETE\r\nName: {name}\r\nContent-Length: 0\r\n\r\n")
}

/// Connect to the socket as `user` (None = current process) via
/// `runuser` + Python, write the supplied raw frame, read the framed
/// response. Python is the path-of-least-resistance — every Ubuntu CI
/// runner ships `python3` and we avoid adding a separate test helper
/// binary.
fn send_frame_as(user: Option<&str>, socket: &Path, frame: &[u8]) -> Vec<u8> {
    let script_dir = std::env::temp_dir().join(format!(
        "aimx-uds-authz-py-{}-{}",
        std::process::id(),
        rand_suffix()
    ));
    std::fs::create_dir_all(&script_dir).unwrap();
    let b64 = base64_encode(frame);
    let socket_str = socket.display().to_string();
    let py = format!(
        r#"import socket, sys, base64
frame = base64.b64decode("{b64}")
s = socket.socket(socket.AF_UNIX)
s.connect("{socket_str}")
s.sendall(frame)
s.shutdown(socket.SHUT_WR)
buf = b""
while True:
    chunk = s.recv(4096)
    if not chunk:
        break
    buf += chunk
sys.stdout.buffer.write(buf)
"#
    );
    let script_path = script_dir.join("client.py");
    std::fs::write(&script_path, py).unwrap();
    std::fs::set_permissions(&script_dir, PermissionsExt::from_mode(0o755)).unwrap();
    std::fs::set_permissions(&script_path, PermissionsExt::from_mode(0o644)).unwrap();

    let output = match user {
        Some(u) => Command::new("runuser")
            .arg("-u")
            .arg(u)
            .arg("--")
            .arg("python3")
            .arg(&script_path)
            .output()
            .expect("failed to spawn runuser python3"),
        None => Command::new("python3")
            .arg(&script_path)
            .output()
            .expect("failed to spawn python3"),
    };
    let _ = std::fs::remove_dir_all(&script_dir);
    assert!(
        output.status.success(),
        "python3 UDS client exited non-zero (user={user:?}): stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

fn rand_suffix() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
}

/// Tiny standalone base64 encoder — integration tests can't pull in
/// the crate's `base64` dep without adding it to dev-dependencies.
fn base64_encode(input: &[u8]) -> String {
    const ALPHA: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    let mut i = 0;
    while i + 3 <= input.len() {
        let a = input[i];
        let b = input[i + 1];
        let c = input[i + 2];
        out.push(ALPHA[(a >> 2) as usize] as char);
        out.push(ALPHA[(((a & 0b11) << 4) | (b >> 4)) as usize] as char);
        out.push(ALPHA[(((b & 0b1111) << 2) | (c >> 6)) as usize] as char);
        out.push(ALPHA[(c & 0b111111) as usize] as char);
        i += 3;
    }
    let remainder = input.len() - i;
    if remainder == 1 {
        let a = input[i];
        out.push(ALPHA[(a >> 2) as usize] as char);
        out.push(ALPHA[((a & 0b11) << 4) as usize] as char);
        out.push_str("==");
    } else if remainder == 2 {
        let a = input[i];
        let b = input[i + 1];
        out.push(ALPHA[(a >> 2) as usize] as char);
        out.push(ALPHA[(((a & 0b11) << 4) | (b >> 4)) as usize] as char);
        out.push(ALPHA[((b & 0b1111) << 2) as usize] as char);
        out.push('=');
    }
    out
}

#[test]
fn base64_encode_roundtrip_matches_stdlib_reference() {
    assert_eq!(base64_encode(b""), "");
    assert_eq!(base64_encode(b"f"), "Zg==");
    assert_eq!(base64_encode(b"fo"), "Zm8=");
    assert_eq!(base64_encode(b"foo"), "Zm9v");
    assert_eq!(base64_encode(b"foob"), "Zm9vYg==");
    assert_eq!(base64_encode(b"fooba"), "Zm9vYmE=");
    assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
}

fn chown_dir(path: &Path, uid: u32, gid: u32) {
    let c = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).unwrap();
    unsafe {
        libc::chown(c.as_ptr(), uid, gid);
        libc::chmod(c.as_ptr(), 0o700);
    }
}

fn uid_of(name: &str) -> u32 {
    let output = Command::new("id")
        .arg("-u")
        .arg(name)
        .output()
        .expect("failed to run id -u");
    assert!(output.status.success());
    String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse::<u32>()
        .expect("uid must parse")
}

/// `stat -c '%U' <path>` — returns the textual owner username of a
/// directory or file. Used by the cross-uid `MAILBOX-CREATE` test to
/// prove the on-disk owner came from `SO_PEERCRED`, not the wire
/// `Owner:` header.
fn stat_owner_username(path: &Path) -> String {
    let output = Command::new("stat")
        .arg("-c")
        .arg("%U")
        .arg(path)
        .output()
        .expect("failed to run stat -c %U");
    assert!(
        output.status.success(),
        "stat -c %U {:?} failed: {}",
        path,
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

/// Pull the `reason` text out of an `AIMX/1 ERR <CODE> <reason>` status
/// line. The codec emits `AIMX/1 ERR <CODE> <reason>\nCode: <code>\n\n`
/// (see `send_protocol::write_ack_response`); tests that need to compare
/// reason strings route through this helper rather than substring-matching
/// the whole frame.
fn extract_err_reason(resp: &[u8]) -> String {
    let s = String::from_utf8_lossy(resp);
    let first_line = s.lines().next().unwrap_or("");
    // `AIMX/1 ERR <CODE> <reason>` — strip the prefix + code.
    if let Some(rest) = first_line.strip_prefix("AIMX/1 ERR ")
        && let Some((_code, reason)) = rest.split_once(' ')
    {
        return reason.trim().to_string();
    }
    String::new()
}

/// Per-test fixture: tempdir + running daemon + ready socket. The
/// `Drop` impl on `Teardown` kills the daemon so test failures don't
/// leak processes.
struct Fixture {
    // Held for its `Drop` side-effect (kills the serve subprocess);
    // never read directly.
    #[allow(dead_code)]
    teardown: Teardown,
    socket_path: PathBuf,
    tmp_root: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        // teardown's Drop already runs; just clean the tempdir.
        let _ = std::fs::remove_dir_all(&self.tmp_root);
    }
}

fn spin_up_serve() -> Fixture {
    assert_eq!(
        unsafe { libc::geteuid() },
        0,
        "uds_authz tests must run as root (AIMX_INTEGRATION_SUDO=1 + sudo)"
    );

    ensure_user(ALICE);
    ensure_user(BOB);
    // The catchall system user is named in `config.toml` below; if it's
    // missing the daemon flags it as orphan.
    let _ = Command::new("useradd")
        .arg("--system")
        .arg("--no-create-home")
        .arg("--shell")
        .arg("/usr/sbin/nologin")
        .arg("aimx-catchall")
        .status();

    let alice_uid = uid_of(ALICE);
    let bob_uid = uid_of(BOB);

    let tmp_root = std::env::temp_dir().join(format!(
        "aimx-uds-authz-{}-{}",
        std::process::id(),
        rand_suffix()
    ));
    std::fs::create_dir_all(&tmp_root).unwrap();
    std::fs::set_permissions(&tmp_root, PermissionsExt::from_mode(0o755)).unwrap();

    let data_dir = tmp_root.join("data");
    std::fs::create_dir_all(data_dir.join("inbox")).unwrap();
    std::fs::create_dir_all(data_dir.join("sent")).unwrap();
    std::fs::set_permissions(&data_dir, PermissionsExt::from_mode(0o755)).unwrap();
    std::fs::set_permissions(data_dir.join("inbox"), PermissionsExt::from_mode(0o755)).unwrap();
    std::fs::set_permissions(data_dir.join("sent"), PermissionsExt::from_mode(0o755)).unwrap();

    for (user, uid) in [(ALICE, alice_uid), (BOB, bob_uid)] {
        for sub in ["inbox", "sent"] {
            let dir = data_dir.join(sub).join(user);
            std::fs::create_dir_all(&dir).unwrap();
            chown_dir(&dir, uid, uid);
        }
    }

    let config_content = format!(
        "domain = \"it.example.com\"\n\
         data_dir = \"{data_dir}\"\n\
         dkim_selector = \"aimx\"\n\n\
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
    std::fs::set_permissions(&config_path, PermissionsExt::from_mode(0o644)).unwrap();

    let keygen_status = Command::new(aimx_binary_path())
        .env("AIMX_CONFIG_DIR", &tmp_root)
        .env("AIMX_DATA_DIR", &data_dir)
        .arg("dkim-keygen")
        .arg("--force")
        .status()
        .expect("dkim-keygen failed to spawn");
    assert!(keygen_status.success(), "dkim-keygen failed");

    let runtime_dir = tmp_root.join("run");
    std::fs::create_dir_all(&runtime_dir).unwrap();
    std::fs::set_permissions(&runtime_dir, PermissionsExt::from_mode(0o755)).unwrap();

    let mail_drop = tmp_root.join("mail_drop");
    std::fs::create_dir_all(&mail_drop).unwrap();

    let mut teardown = Teardown { serve: None };
    let serve = Command::new(aimx_binary_path())
        .env("AIMX_CONFIG_DIR", &tmp_root)
        .env("AIMX_DATA_DIR", &data_dir)
        .env("AIMX_RUNTIME_DIR", &runtime_dir)
        .env("AIMX_TEST_MAIL_DROP", &mail_drop)
        .arg("serve")
        .arg("--bind")
        .arg("127.0.0.1:0")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("aimx serve failed to spawn");
    teardown.serve = Some(serve);

    let socket_path = runtime_dir.join("aimx.sock");
    assert!(
        wait_for_socket(&socket_path),
        "aimx.sock did not appear at {} within 15s",
        socket_path.display()
    );
    std::fs::set_permissions(&socket_path, PermissionsExt::from_mode(0o666)).unwrap();

    Fixture {
        teardown,
        socket_path,
        tmp_root,
    }
}

fn assert_response_contains(resp: &[u8], needle: &str) {
    let s = String::from_utf8_lossy(resp);
    assert!(
        s.contains(needle),
        "expected response to contain {needle:?}; got {s:?}"
    );
}

fn assert_response_does_not_contain(resp: &[u8], needle: &str) {
    let s = String::from_utf8_lossy(resp);
    assert!(
        !s.contains(needle),
        "did not expect response to contain {needle:?}; got {s:?}"
    );
}

fn integration_gate() -> bool {
    if std::env::var_os("AIMX_INTEGRATION_SUDO").is_none() {
        eprintln!("AIMX_INTEGRATION_SUDO is not set; skipping (test requires root + user-create).");
        return false;
    }
    true
}

// ---------- SEND ----------

#[test]
#[ignore = "requires two test uids on the host (aimx-test-alice, aimx-test-bob)"]
fn send_as_owner_ok() {
    if !integration_gate() {
        return;
    }
    let fx = spin_up_serve();
    let resp = send_frame_as(
        Some(ALICE),
        &fx.socket_path,
        raw_send_frame(
            &format!("{ALICE}@it.example.com"),
            "victim@external.example",
        )
        .as_bytes(),
    );
    // OK arrives whether the SMTP delivery succeeded or hit a transport
    // error — the check we care about is that authz didn't reject. Any
    // wire frame except `EACCES` proves the verb passed authz.
    assert_response_does_not_contain(&resp, "EACCES");
    drop(fx);
}

#[test]
#[ignore = "requires two test uids on the host (aimx-test-alice, aimx-test-bob)"]
fn send_as_other_forbidden() {
    if !integration_gate() {
        return;
    }
    let fx = spin_up_serve();
    let resp = send_frame_as(
        Some(BOB),
        &fx.socket_path,
        raw_send_frame(
            &format!("{ALICE}@it.example.com"),
            "victim@external.example",
        )
        .as_bytes(),
    );
    assert_response_contains(&resp, "EACCES");
    drop(fx);
}

#[test]
#[ignore = "requires two test uids on the host (aimx-test-alice, aimx-test-bob)"]
fn send_as_root_any_ok() {
    if !integration_gate() {
        return;
    }
    let fx = spin_up_serve();
    let resp = send_frame_as(
        None,
        &fx.socket_path,
        raw_send_frame(
            &format!("{ALICE}@it.example.com"),
            "victim@external.example",
        )
        .as_bytes(),
    );
    assert_response_does_not_contain(&resp, "EACCES");
    drop(fx);
}

// ---------- MARK-READ / MARK-UNREAD ----------

#[test]
#[ignore = "requires two test uids on the host (aimx-test-alice, aimx-test-bob)"]
fn mark_read_as_owner_ok_passes_authz() {
    if !integration_gate() {
        return;
    }
    let fx = spin_up_serve();
    let resp = send_frame_as(
        Some(ALICE),
        &fx.socket_path,
        raw_mark_frame("MARK-READ", ALICE, "2026-01-01-nothing").as_bytes(),
    );
    // The email doesn't exist, so the response is NOTFOUND — but
    // authz accepted (otherwise we would see EACCES first).
    assert_response_does_not_contain(&resp, "EACCES");
    drop(fx);
}

#[test]
#[ignore = "requires two test uids on the host (aimx-test-alice, aimx-test-bob)"]
fn mark_read_as_other_forbidden() {
    if !integration_gate() {
        return;
    }
    let fx = spin_up_serve();
    let resp = send_frame_as(
        Some(BOB),
        &fx.socket_path,
        raw_mark_frame("MARK-READ", ALICE, "2026-01-01-nothing").as_bytes(),
    );
    assert_response_contains(&resp, "EACCES");
    drop(fx);
}

#[test]
#[ignore = "requires two test uids on the host (aimx-test-alice, aimx-test-bob)"]
fn mark_read_as_root_any_ok() {
    if !integration_gate() {
        return;
    }
    let fx = spin_up_serve();
    let resp = send_frame_as(
        None,
        &fx.socket_path,
        raw_mark_frame("MARK-READ", ALICE, "2026-01-01-nothing").as_bytes(),
    );
    assert_response_does_not_contain(&resp, "EACCES");
    drop(fx);
}

#[test]
#[ignore = "requires two test uids on the host (aimx-test-alice, aimx-test-bob)"]
fn mark_unread_as_owner_ok_passes_authz() {
    if !integration_gate() {
        return;
    }
    let fx = spin_up_serve();
    let resp = send_frame_as(
        Some(ALICE),
        &fx.socket_path,
        raw_mark_frame("MARK-UNREAD", ALICE, "2026-01-01-nothing").as_bytes(),
    );
    assert_response_does_not_contain(&resp, "EACCES");
    drop(fx);
}

#[test]
#[ignore = "requires two test uids on the host (aimx-test-alice, aimx-test-bob)"]
fn mark_unread_as_other_forbidden() {
    if !integration_gate() {
        return;
    }
    let fx = spin_up_serve();
    let resp = send_frame_as(
        Some(BOB),
        &fx.socket_path,
        raw_mark_frame("MARK-UNREAD", ALICE, "2026-01-01-nothing").as_bytes(),
    );
    assert_response_contains(&resp, "EACCES");
    drop(fx);
}

#[test]
#[ignore = "requires two test uids on the host (aimx-test-alice, aimx-test-bob)"]
fn mark_unread_as_root_any_ok() {
    if !integration_gate() {
        return;
    }
    let fx = spin_up_serve();
    let resp = send_frame_as(
        None,
        &fx.socket_path,
        raw_mark_frame("MARK-UNREAD", ALICE, "2026-01-01-nothing").as_bytes(),
    );
    assert_response_does_not_contain(&resp, "EACCES");
    drop(fx);
}

// ---------- HOOK-CREATE / HOOK-DELETE ----------

#[test]
#[ignore = "requires two test uids on the host (aimx-test-alice, aimx-test-bob)"]
fn hook_create_as_owner_ok() {
    if !integration_gate() {
        return;
    }
    let fx = spin_up_serve();
    let resp = send_frame_as(
        Some(ALICE),
        &fx.socket_path,
        raw_hook_create_frame(ALICE, "alice_owner_ok", &["/bin/true"]).as_bytes(),
    );
    assert_response_contains(&resp, "AIMX/1 OK");
    drop(fx);
}

#[test]
#[ignore = "requires two test uids on the host (aimx-test-alice, aimx-test-bob)"]
fn hook_create_as_other_forbidden() {
    if !integration_gate() {
        return;
    }
    let fx = spin_up_serve();
    let resp = send_frame_as(
        Some(BOB),
        &fx.socket_path,
        raw_hook_create_frame(ALICE, "bob_attempts_alice", &["/bin/true"]).as_bytes(),
    );
    assert_response_contains(&resp, "EACCES");
    drop(fx);
}

#[test]
#[ignore = "requires two test uids on the host (aimx-test-alice, aimx-test-bob)"]
fn hook_create_as_root_any_ok() {
    if !integration_gate() {
        return;
    }
    let fx = spin_up_serve();
    let resp = send_frame_as(
        None,
        &fx.socket_path,
        raw_hook_create_frame(ALICE, "root_for_alice", &["/bin/true"]).as_bytes(),
    );
    assert_response_contains(&resp, "AIMX/1 OK");
    drop(fx);
}

#[test]
#[ignore = "requires two test uids on the host (aimx-test-alice, aimx-test-bob)"]
fn hook_delete_as_owner_ok() {
    if !integration_gate() {
        return;
    }
    let fx = spin_up_serve();
    // Create a hook as alice first, then delete it as alice.
    let create = send_frame_as(
        Some(ALICE),
        &fx.socket_path,
        raw_hook_create_frame(ALICE, "alice_to_delete", &["/bin/true"]).as_bytes(),
    );
    assert_response_contains(&create, "AIMX/1 OK");
    let del = send_frame_as(
        Some(ALICE),
        &fx.socket_path,
        raw_hook_delete_frame("alice_to_delete").as_bytes(),
    );
    assert_response_contains(&del, "AIMX/1 OK");
    drop(fx);
}

#[test]
#[ignore = "requires two test uids on the host (aimx-test-alice, aimx-test-bob)"]
fn hook_delete_as_other_forbidden() {
    if !integration_gate() {
        return;
    }
    let fx = spin_up_serve();
    // Create a hook as alice, then bob tries to delete it.
    let create = send_frame_as(
        Some(ALICE),
        &fx.socket_path,
        raw_hook_create_frame(ALICE, "alice_owned_hook", &["/bin/true"]).as_bytes(),
    );
    assert_response_contains(&create, "AIMX/1 OK");
    let del = send_frame_as(
        Some(BOB),
        &fx.socket_path,
        raw_hook_delete_frame("alice_owned_hook").as_bytes(),
    );
    // NFR2 opacity: a non-owner caller must not be able to distinguish
    // "hook exists on a mailbox you don't own" from "hook doesn't exist
    // anywhere." Both surface as ENOENT with the same canonical reason;
    // the wire response must not leak that authz rejected the request.
    assert_response_contains(&del, "ENOENT");
    assert_response_contains(&del, "hook 'alice_owned_hook' not found");
    assert_response_does_not_contain(&del, "not authorized");
    assert_response_does_not_contain(&del, "EACCES");
    drop(fx);
}

#[test]
#[ignore = "requires two test uids on the host (aimx-test-alice, aimx-test-bob)"]
fn hook_delete_as_root_any_ok() {
    if !integration_gate() {
        return;
    }
    let fx = spin_up_serve();
    let create = send_frame_as(
        Some(ALICE),
        &fx.socket_path,
        raw_hook_create_frame(ALICE, "alice_owned_for_root", &["/bin/true"]).as_bytes(),
    );
    assert_response_contains(&create, "AIMX/1 OK");
    let del = send_frame_as(
        None,
        &fx.socket_path,
        raw_hook_delete_frame("alice_owned_for_root").as_bytes(),
    );
    assert_response_contains(&del, "AIMX/1 OK");
    drop(fx);
}

// ---------- MAILBOX-CREATE / MAILBOX-DELETE ----------

#[test]
#[ignore = "requires two test uids on the host (aimx-test-alice, aimx-test-bob)"]
fn mailbox_create_root_ok() {
    if !integration_gate() {
        return;
    }
    let fx = spin_up_serve();
    let resp = send_frame_as(
        None,
        &fx.socket_path,
        raw_mailbox_create_frame("freshmbx", ALICE).as_bytes(),
    );
    assert_response_contains(&resp, "AIMX/1 OK");
    drop(fx);
}

/// Sprint 1 NFR1 — the daemon must bind the on-disk owner of a non-root
/// `MAILBOX-CREATE` to the **caller's** uid (resolved via `SO_PEERCRED`),
/// completely ignoring whatever the wire `Owner:` header says. This is
/// the strongest end-to-end privilege-escalation regression guard the
/// sprint introduces — alice submits `Owner: aimx-test-bob` and the
/// resulting mailbox must be owned by alice on disk, never by bob.
///
/// Replaces the retired `mailbox_create_non_root_forbidden` test that
/// asserted the pre-Sprint-1 root-only behavior. The unit test in
/// `mailbox_handler.rs` covers the synthesis logic in isolation; only
/// this test exercises the real `SO_PEERCRED` codepath end-to-end.
#[test]
#[ignore = "requires two test uids on the host (aimx-test-alice, aimx-test-bob)"]
fn mailbox_create_non_root_owner_synthesized_from_peercred() {
    if !integration_gate() {
        return;
    }
    let fx = spin_up_serve();
    // Alice submits `Owner: aimx-test-bob` — the wire owner the daemon
    // must drop on the floor for non-root callers. The synthesized
    // owner is the caller's own username, derived from the `SO_PEERCRED`
    // uid, never the wire header.
    let resp = send_frame_as(
        Some(ALICE),
        &fx.socket_path,
        raw_mailbox_create_frame("alicemade", BOB).as_bytes(),
    );
    assert_response_contains(&resp, "AIMX/1 OK");

    // On-disk owner must be alice (the SO_PEERCRED caller), never bob
    // (the wire `Owner:` header). NFR1 in one assertion.
    let inbox = fx.tmp_root.join("data").join("inbox").join("alicemade");
    let on_disk = stat_owner_username(&inbox);
    assert_eq!(
        on_disk, ALICE,
        "expected on-disk owner to equal SO_PEERCRED caller {ALICE}, got {on_disk} \
         (wire Owner:{BOB} must NOT be honored for non-root callers)"
    );
    let sent = fx.tmp_root.join("data").join("sent").join("alicemade");
    let on_disk_sent = stat_owner_username(&sent);
    assert_eq!(on_disk_sent, ALICE, "sent dir owner must match inbox dir");

    drop(fx);
}

/// Sprint 1 happy path — non-root caller creates a mailbox they
/// legitimately own. Pins the new default flow end-to-end through real
/// `SO_PEERCRED` so CI proves the relaxed authz actually allows the
/// case the sprint exists to enable.
#[test]
#[ignore = "requires two test uids on the host (aimx-test-alice, aimx-test-bob)"]
fn mailbox_create_non_root_owner_ok() {
    if !integration_gate() {
        return;
    }
    let fx = spin_up_serve();
    // Wire owner matches caller — happy path.
    let resp = send_frame_as(
        Some(ALICE),
        &fx.socket_path,
        raw_mailbox_create_frame("aliceowned", ALICE).as_bytes(),
    );
    assert_response_contains(&resp, "AIMX/1 OK");
    let inbox = fx.tmp_root.join("data").join("inbox").join("aliceowned");
    assert_eq!(stat_owner_username(&inbox), ALICE);
    drop(fx);
}

#[test]
#[ignore = "requires two test uids on the host (aimx-test-alice, aimx-test-bob)"]
fn mailbox_delete_root_ok() {
    if !integration_gate() {
        return;
    }
    let fx = spin_up_serve();
    // Create then delete as root.
    let create = send_frame_as(
        None,
        &fx.socket_path,
        raw_mailbox_create_frame("ephemeral", ALICE).as_bytes(),
    );
    assert_response_contains(&create, "AIMX/1 OK");
    let del = send_frame_as(
        None,
        &fx.socket_path,
        raw_mailbox_delete_frame("ephemeral").as_bytes(),
    );
    assert_response_contains(&del, "AIMX/1 OK");
    drop(fx);
}

/// Sprint 1 NFR2 — the daemon's wire response for a non-root caller
/// trying to delete a mailbox owned by a different uid must be
/// byte-identical to the genuine "no such mailbox" response. The
/// daemon must not leak whether a mailbox exists to a caller who is
/// not its owner.
///
/// Replaces the retired `mailbox_delete_non_root_forbidden` test. Bob
/// owns the `aimx-test-bob` mailbox in the test config; alice tries
/// to delete it and must see the same response the daemon would emit
/// for a genuinely missing mailbox like `aimx-test-bob` if it never
/// existed.
#[test]
#[ignore = "requires two test uids on the host (aimx-test-alice, aimx-test-bob)"]
fn mailbox_delete_non_root_cross_uid_returns_no_such_mailbox() {
    if !integration_gate() {
        return;
    }
    let fx = spin_up_serve();
    // Compute the canonical no-such-mailbox text by issuing a delete
    // for a definitely-missing mailbox name. Captured via root so the
    // authz layer does not interpose. This pins the byte-identity the
    // cross-uid delete must match without the test depending on the
    // exact format string.
    let canonical = send_frame_as(
        None,
        &fx.socket_path,
        raw_mailbox_delete_frame("definitely-not-a-mailbox").as_bytes(),
    );
    let canonical_reason = extract_err_reason(&canonical);

    // Alice tries to delete bob's mailbox. Daemon must return the
    // no-such-mailbox shape (NOT the not-authorized shape) so the
    // existence of bob's mailbox is not leaked across the privilege
    // boundary.
    let resp = send_frame_as(
        Some(ALICE),
        &fx.socket_path,
        raw_mailbox_delete_frame(BOB).as_bytes(),
    );
    let resp_text = String::from_utf8_lossy(&resp).to_string();
    let resp_reason = extract_err_reason(&resp);

    // The reason text must match the canonical no-such-mailbox shape
    // (with the target name substituted). Strip the substituted name
    // from each side to compare the templates byte-for-byte; this pins
    // both responses to a single helper (`no_such_mailbox_reason`) so a
    // future divergence is caught immediately.
    let canonical_template = canonical_reason.replace("definitely-not-a-mailbox", "<NAME>");
    let resp_template = resp_reason.replace(BOB, "<NAME>");
    assert_eq!(
        resp_template, canonical_template,
        "cross-uid delete must return the canonical no-such-mailbox response template; \
         canonical = {canonical_reason:?}, got = {resp_reason:?}"
    );
    assert!(
        resp_reason.contains("does not exist"),
        "expected canonical no-such-mailbox response, got reason {resp_reason:?} \
         (canonical for missing was {canonical_reason:?})"
    );
    // Safety net: by construction the daemon synthesizes the same
    // helper for both paths, so the response also must not leak any
    // word that would signal a permission error.
    for leak in [
        "owner",
        "permission",
        "EACCES",
        "root",
        "authorized",
        "denied",
    ] {
        assert!(
            !resp_text.to_lowercase().contains(&leak.to_lowercase()),
            "wire response leaked '{leak}': {resp_text:?}"
        );
    }

    // Defense in depth: bob's mailbox still exists on disk after the
    // failed cross-uid delete attempt.
    let bob_inbox = fx.tmp_root.join("data").join("inbox").join(BOB);
    assert!(
        bob_inbox.exists(),
        "bob's inbox must still exist after alice's failed cross-uid delete"
    );

    drop(fx);
}

/// Sprint 1 happy path — non-root caller creates and then deletes a
/// mailbox they own. Pins the new default flow end-to-end through
/// real `SO_PEERCRED` so CI exercises both verbs from a non-root
/// caller in one transaction.
#[test]
#[ignore = "requires two test uids on the host (aimx-test-alice, aimx-test-bob)"]
fn mailbox_delete_non_root_owner_ok() {
    if !integration_gate() {
        return;
    }
    let fx = spin_up_serve();
    let create = send_frame_as(
        Some(ALICE),
        &fx.socket_path,
        raw_mailbox_create_frame("aliceephemeral", ALICE).as_bytes(),
    );
    assert_response_contains(&create, "AIMX/1 OK");
    let del = send_frame_as(
        Some(ALICE),
        &fx.socket_path,
        raw_mailbox_delete_frame("aliceephemeral").as_bytes(),
    );
    assert_response_contains(&del, "AIMX/1 OK");
    drop(fx);
}
