//! Cross-user UDS authz integration tests.
//!
//! Reuses the `aimx-test-alice` / `aimx-test-bob` fixture also used by
//! `tests/mailbox_isolation.rs`. Spins up a real `aimx serve` subprocess
//! under a tempdir, then issues raw `AIMX/1` frames as alice/bob/root
//! via `runuser -u <user> python3 -c ...`.
//!
//! Per-verb ownership matrix (mirrors `src/uds_authz.rs`):
//!
//! | Verb                                | Owner | Other | Root |
//! |-------------------------------------|-------|-------|------|
//! | `SEND` (as alice)                   | OK    | EACCES| OK   |
//! | `MARK-READ` / `MARK-UNREAD`         | OK*   | EACCES| OK*  |
//! | `HOOK-CREATE` / `HOOK-DELETE`       | OK    | EACCES| OK   |
//! | `MAILBOX-CREATE` / `MAILBOX-DELETE` |  n/a  | EACCES| OK   |
//!
//! `*` MARK targets a non-existent email so the OK path surfaces as
//! `NOTFOUND` after authz accepts; the EACCES path exits before the
//! filesystem lookup.
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
    assert_response_contains(&del, "EACCES");
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

#[test]
#[ignore = "requires two test uids on the host (aimx-test-alice, aimx-test-bob)"]
fn mailbox_create_non_root_forbidden() {
    if !integration_gate() {
        return;
    }
    let fx = spin_up_serve();
    let resp = send_frame_as(
        Some(ALICE),
        &fx.socket_path,
        raw_mailbox_create_frame("alicemade", ALICE).as_bytes(),
    );
    assert_response_contains(&resp, "EACCES");
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

#[test]
#[ignore = "requires two test uids on the host (aimx-test-alice, aimx-test-bob)"]
fn mailbox_delete_non_root_forbidden() {
    if !integration_gate() {
        return;
    }
    let fx = spin_up_serve();
    let resp = send_frame_as(
        Some(ALICE),
        &fx.socket_path,
        raw_mailbox_delete_frame(ALICE).as_bytes(),
    );
    assert_response_contains(&resp, "EACCES");
    drop(fx);
}
