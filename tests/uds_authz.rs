//! Sprint 4 S4-4: cross-user UDS authz integration test.
//!
//! Reuses the `aimx-it-alice` / `aimx-it-bob` fixture from Sprint 2
//! (`tests/isolation.rs`). Spins up a real `aimx serve` subprocess under
//! a tempdir, then issues raw `AIMX/1` frames under bob's uid via
//! `runuser -u aimx-it-bob python3 -c ...`. Every attack must come
//! back with `AIMX/1 ERR EACCES`. The root control run succeeds and
//! emits the `decision="root_bypass"` info log captured from the
//! serve subprocess's stderr.
//!
//! Gated on both `#[ignore]` and `AIMX_INTEGRATION_SUDO=1` so it only
//! runs inside the CI step that explicitly elevates.
//!
//! Teardown uses a `Drop` guard so `userdel` and the killed serve
//! subprocess are cleaned up even on assertion failure.

#![cfg(unix)]

use std::io::Read;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const ALICE: &str = "aimx-it-alice";
const BOB: &str = "aimx-it-bob";

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
        let _ = Command::new("userdel").arg(ALICE).status();
        let _ = Command::new("userdel").arg(BOB).status();
    }
}

fn useradd(name: &str) {
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
            let check = Command::new("id").arg(name).status().ok();
            matches!(check, Some(s) if s.success())
        },
        "useradd failed for {name}"
    );
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

/// Raw `AIMX/1 SEND` framing. The daemon parses `From:` out of the
/// body to resolve the sender mailbox and runs authz against the
/// caller's uid. bob as alice: EACCES.
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

/// Raw `AIMX/1 MARK-READ` framing.
fn raw_mark_frame(mailbox: &str, id: &str) -> String {
    format!(
        "AIMX/1 MARK-READ\r\nMailbox: {mailbox}\r\nId: {id}\r\nFolder: inbox\r\nContent-Length: 0\r\n\r\n"
    )
}

/// Raw `AIMX/1 HOOK-CREATE` framing with a trivial JSON body. The
/// template the hook refers to does not need to exist — authz runs
/// before template resolution per PRD §6.5 (authz mismatch ⇒ EACCES,
/// unknown mailbox ⇒ ENOENT).
fn raw_hook_create_frame(mailbox: &str) -> String {
    let body = "{\"params\":{}}";
    format!(
        "AIMX/1 HOOK-CREATE\r\nMailbox: {mailbox}\r\nEvent: on_receive\r\nTemplate: invoke-claude\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    )
}

/// Connect to the socket as `user` via `runuser` + Python, write the
/// supplied raw frame, read the framed response. Python is the
/// path-of-least-resistance here because every Ubuntu CI runner ships
/// `python3` and we avoid adding a separate test helper binary.
///
/// The Python script is written to a tempfile (rather than passed via
/// `-c`) so multi-line `while` loops keep their indentation — `-c` on
/// some shells collapses newlines into spaces, which breaks Python's
/// significant whitespace.
fn send_frame_as(user: Option<&str>, socket: &Path, frame: &[u8]) -> Vec<u8> {
    let script_dir = std::env::temp_dir().join(format!(
        "aimx-uds-authz-py-{}-{}",
        std::process::id(),
        rand_suffix()
    ));
    std::fs::create_dir_all(&script_dir).unwrap();
    // Script must be world-readable so the `runuser` target can
    // execute it. The frame blob is injected as a base64 literal so
    // arbitrary bytes (including CR/LF and quotes) survive the round
    // trip through the shell and Python's source parser.
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
/// the crate's `base64` dep without adding it to dev-dependencies,
/// which would drag an extra build for one 20-line helper.
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
    // Quick smoke test so a subtle bit-twiddling bug doesn't silently
    // corrupt the wire frames the integration test submits.
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

#[test]
#[ignore]
fn bob_cannot_impersonate_alice_over_uds() {
    if std::env::var_os("AIMX_INTEGRATION_SUDO").is_none() {
        eprintln!("AIMX_INTEGRATION_SUDO is not set; skipping (test requires root + user-create).");
        return;
    }
    assert_eq!(
        unsafe { libc::geteuid() },
        0,
        "uds_authz must run as root (AIMX_INTEGRATION_SUDO=1 + sudo)"
    );

    let mut teardown = Teardown { serve: None };
    useradd(ALICE);
    useradd(BOB);
    let alice_uid = uid_of(ALICE);

    let tmp_root = std::env::temp_dir().join(format!("aimx-uds-authz-{}", std::process::id()));
    std::fs::create_dir_all(&tmp_root).unwrap();
    std::fs::set_permissions(&tmp_root, PermissionsExt::from_mode(0o755)).unwrap();

    let data_dir = tmp_root.join("data");
    std::fs::create_dir_all(data_dir.join("inbox")).unwrap();
    std::fs::set_permissions(&data_dir, PermissionsExt::from_mode(0o755)).unwrap();
    std::fs::set_permissions(data_dir.join("inbox"), PermissionsExt::from_mode(0o755)).unwrap();

    // Pre-create alice's inbox dir with the right ownership so a later
    // ingest (if any — not strictly needed for authz rejection tests)
    // has somewhere to drop mail.
    let alice_inbox = data_dir.join("inbox").join(ALICE);
    std::fs::create_dir_all(&alice_inbox).unwrap();
    chown_dir(&alice_inbox, alice_uid, alice_uid);

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

    // Generate a DKIM key pair under AIMX_CONFIG_DIR. `aimx serve`
    // loads the private key at startup and refuses to run without it.
    let keygen_status = Command::new(aimx_binary_path())
        .env("AIMX_CONFIG_DIR", &tmp_root)
        .env("AIMX_DATA_DIR", &data_dir)
        .arg("dkim-keygen")
        .arg("--force")
        .status()
        .expect("dkim-keygen failed to spawn");
    assert!(keygen_status.success(), "dkim-keygen failed");

    // Pre-create the aimx-catchall system user. The invariant in
    // `config.toml` above names it as the catchall owner; if the host
    // doesn't have it, `Config::load` would flag it as orphan and make
    // the catchall mailbox inactive. That's fine for this test (we
    // never exercise catchall), but the resolved Config needs to load
    // cleanly for `aimx serve` to proceed.
    let _ = Command::new("useradd")
        .arg("--system")
        .arg("--no-create-home")
        .arg("--shell")
        .arg("/usr/sbin/nologin")
        .arg("aimx-catchall")
        .status();

    // Runtime directory holds the aimx.sock. AIMX_RUNTIME_DIR is the
    // test-friendly override, used by the existing codebase so cargo
    // test runs don't collide with /run/aimx/.
    let runtime_dir = tmp_root.join("run");
    std::fs::create_dir_all(&runtime_dir).unwrap();
    std::fs::set_permissions(&runtime_dir, PermissionsExt::from_mode(0o755)).unwrap();

    // Bind SMTP on an ephemeral port on loopback; the authz test
    // never drives inbound so the exact port doesn't matter, it
    // just needs to bind successfully.
    //
    // AIMX_TEST_MAIL_DROP redirects outbound MX delivery to disk so
    // the root SEND control below doesn't blackhole into the real
    // internet.
    let mail_drop = tmp_root.join("mail_drop");
    std::fs::create_dir_all(&mail_drop).unwrap();

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
    // World-writable so bob can connect. Matches the production
    // `0o666` bind mode.
    std::fs::set_permissions(&socket_path, PermissionsExt::from_mode(0o666)).unwrap();

    // --- Attack 1: bob SENDs as alice. Must be EACCES. ---
    let resp_bytes = send_frame_as(
        Some(BOB),
        &socket_path,
        raw_send_frame(
            &format!("{ALICE}@it.example.com"),
            "victim@external.example",
        )
        .as_bytes(),
    );
    let resp = String::from_utf8_lossy(&resp_bytes);
    assert!(
        resp.contains("AIMX/1 ERR EACCES") || resp.contains("Code: EACCES"),
        "bob SEND as alice must be EACCES; got: {resp:?}"
    );

    // --- Attack 2: bob MARK-READ's an email in alice's mailbox. Must be EACCES. ---
    let resp_bytes = send_frame_as(
        Some(BOB),
        &socket_path,
        raw_mark_frame(ALICE, "2026-01-01-nothing").as_bytes(),
    );
    let resp = String::from_utf8_lossy(&resp_bytes);
    assert!(
        resp.contains("AIMX/1 ERR EACCES") || resp.contains("Code: EACCES"),
        "bob MARK-READ on alice must be EACCES; got: {resp:?}"
    );

    // --- Attack 3: bob HOOK-CREATE on alice's mailbox. Must be EACCES. ---
    let resp_bytes = send_frame_as(
        Some(BOB),
        &socket_path,
        raw_hook_create_frame(ALICE).as_bytes(),
    );
    let resp = String::from_utf8_lossy(&resp_bytes);
    assert!(
        resp.contains("AIMX/1 ERR EACCES") || resp.contains("Code: EACCES"),
        "bob HOOK-CREATE on alice must be EACCES; got: {resp:?}"
    );

    // --- Control: root MARK-READ on alice's (nonexistent) email
    // returns NOTFOUND rather than EACCES, proving root bypassed the
    // ownership check. The authz path fires and emits a
    // `decision="root_bypass"` info log (captured from serve stderr
    // below). ---
    let resp_bytes = send_frame_as(None, &socket_path, raw_mark_frame(ALICE, "nope").as_bytes());
    let resp = String::from_utf8_lossy(&resp_bytes);
    assert!(
        resp.contains("AIMX/1 OK")
            || resp.contains("AIMX/1 ERR NOTFOUND")
            || resp.contains("Code: NOTFOUND"),
        "root MARK-READ should bypass authz and hit file-not-found; got: {resp:?}"
    );

    // Drop the teardown (stops serve + userdels). Stderr of the
    // serve subprocess captures the structured `aimx::uds` log lines.
    // We pull them out below before the Drop runs so the `root_bypass`
    // assertion has something to look at.
    let mut child = teardown.serve.take().expect("serve child must exist");
    let _ = child.kill();
    let mut stderr = String::new();
    if let Some(mut s) = child.stderr.take() {
        let _ = s.read_to_string(&mut stderr);
    }
    if let Some(mut s) = child.stdout.take() {
        let mut out = String::new();
        let _ = s.read_to_string(&mut out);
        // Merge into `stderr` for simpler grepping; both streams can
        // carry the `tracing` records depending on how the runtime
        // attaches the subscriber.
        stderr.push_str(&out);
    }
    let _ = child.wait();

    // The root_bypass line is emitted at info level. Accept either
    // `decision="root_bypass"` (structured field) or the log-line
    // literal for robustness against subscriber format changes.
    assert!(
        stderr.contains("root_bypass") || stderr.contains("root-bypass"),
        "serve stderr must contain a root_bypass info log; stderr was:\n{stderr}"
    );

    let _ = std::fs::remove_dir_all(&tmp_root);
    // teardown.serve is None; remaining Drop only removes users.
    drop(teardown);
}
