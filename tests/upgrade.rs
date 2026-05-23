//! Integration tests for the one-shot multi-domain upgrade migration.
//!
//! Spawns the real `aimx serve` binary against a v1 fixture install in
//! a `TempDir`, asserts the migration ran, post-migration layout is
//! correct, the original messages are still readable, an SMTP RCPT to
//! the (now-relocated) mailbox lands in the right place, the second
//! restart is a no-op, and a corrupted `.layout-version` causes a hard
//! startup failure.

use assert_cmd::cargo::cargo_bin;
use std::fs;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command as StdCommand, Stdio};
use std::sync::LazyLock;
use tempfile::TempDir;
use wait_timeout::ChildExt as _;

/// Process-scoped cache of a pre-generated 2048-bit RSA DKIM keypair.
/// Generating 2048-bit RSA on every test is multi-hundred-millisecond
/// work; the cache fronts that one-time cost for the whole upgrade
/// test module. Mirrors the pattern used in `tests/integration.rs`.
static DKIM_CACHE: LazyLock<TempDir> = LazyLock::new(|| {
    let cache = TempDir::new().expect("create DKIM cache tempdir");
    // dkim-keygen needs a parseable config.toml (it loads Config at startup).
    let config_content = format!(
        "domain = \"cache.example.com\"\ndata_dir = \"{}\"\n\n\
         [mailboxes.catchall]\naddress = \"*@cache.example.com\"\nowner = \"aimx-catchall\"\n",
        cache.path().display()
    );
    fs::write(cache.path().join("config.toml"), config_content).expect("write cache config.toml");
    let status = StdCommand::new(cargo_bin("aimx"))
        .env("AIMX_CONFIG_DIR", cache.path())
        .arg("dkim-keygen")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("Failed to run aimx dkim-keygen for DKIM cache");
    assert!(
        status.success(),
        "aimx dkim-keygen exited non-zero when populating DKIM cache"
    );
    cache
});

fn cached_dkim_private() -> PathBuf {
    // The cache's `aimx dkim-keygen` (no `--domain`) lands under
    // `<dkim_dir>/<default_domain>/` per the v2 layout. The cache
    // config sets `domain = "cache.example.com"`.
    DKIM_CACHE
        .path()
        .join("dkim")
        .join("cache.example.com")
        .join("private.key")
}
fn cached_dkim_public() -> PathBuf {
    DKIM_CACHE
        .path()
        .join("dkim")
        .join("cache.example.com")
        .join("public.key")
}

fn current_username() -> String {
    // The fixture's `owner = "OWNER_PLACEHOLDER"` is rewritten to the
    // calling user at test runtime so the daemon can chown the mailbox
    // dirs to a real, present uid (`getpwnam` resolves locally).
    let uid = unsafe { libc::geteuid() };
    if uid == 0 {
        if let Some(sudo_user) = std::env::var_os("SUDO_USER") {
            let name = sudo_user.to_string_lossy().into_owned();
            if !name.is_empty() && name != "root" {
                return name;
            }
        }
        return "nobody".to_string();
    }
    let pw = unsafe { libc::getpwuid(uid) };
    if pw.is_null() {
        return "nobody".to_string();
    }
    let cstr = unsafe { std::ffi::CStr::from_ptr((*pw).pw_name) };
    cstr.to_string_lossy().into_owned()
}

fn find_free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// Copy the v1 fixture into a fresh `TempDir`, rewriting the owner
/// placeholder, and seed a cached DKIM keypair so the daemon's startup
/// DKIM load (and any post-migration outbound) finds real keys.
fn install_v1_fixture(tmp: &Path) -> PathBuf {
    let fixture =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/upgrade/v1-single-domain");
    copy_dir_recursive(&fixture, tmp);

    // Replace the OWNER_PLACEHOLDER with a real local username so
    // `getpwnam` resolves and the daemon doesn't refuse to chown.
    let cfg_path = tmp.join("config.toml");
    let mut body = fs::read_to_string(&cfg_path).unwrap();
    body = body.replace("OWNER_PLACEHOLDER", &current_username());
    fs::write(&cfg_path, &body).unwrap();

    // Seed the legacy DKIM keypair at `<cfg>/dkim/{private,public}.key`.
    let dkim_dir = tmp.join("dkim");
    fs::create_dir_all(&dkim_dir).unwrap();
    fs::copy(cached_dkim_private(), dkim_dir.join("private.key")).unwrap();
    fs::copy(cached_dkim_public(), dkim_dir.join("public.key")).unwrap();

    cfg_path
}

fn copy_dir_recursive(src: &Path, dst: &Path) {
    fs::create_dir_all(dst).unwrap();
    for entry in fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let name = entry.file_name();
        let from = entry.path();
        let to = dst.join(&name);
        // Skip `.gitkeep` placeholders that exist only to keep empty
        // directories under version control.
        if name == ".gitkeep" {
            // Still ensure the parent (the empty dir) exists.
            continue;
        }
        if from.is_dir() {
            copy_dir_recursive(&from, &to);
        } else {
            fs::copy(&from, &to).unwrap();
        }
    }
}

/// Spawn `aimx serve` against `tmp` and wait for the SMTP port to bind.
/// Returns the child process; the caller is responsible for `stop_serve`.
fn start_serve(tmp: &Path, port: u16) -> Child {
    let runtime = tmp.join("run");
    fs::create_dir_all(&runtime).unwrap();
    let mut child = StdCommand::new(cargo_bin("aimx"))
        .env("AIMX_CONFIG_DIR", tmp)
        .env("AIMX_RUNTIME_DIR", &runtime)
        // The sandbox-fallback knob keeps test runs free of systemd-run
        // dependencies that aren't in the harness's environment.
        .env("AIMX_SANDBOX_FORCE_FALLBACK", "1")
        .arg("--data-dir")
        .arg(tmp)
        .arg("serve")
        .arg("--bind")
        .arg(format!("127.0.0.1:{port}"))
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn aimx serve");

    let started = std::time::Instant::now();
    loop {
        if started.elapsed() > std::time::Duration::from_secs(30) {
            let _ = child.kill();
            let mut stderr_buf = String::new();
            if let Some(mut err) = child.stderr.take() {
                let _ = err.read_to_string(&mut stderr_buf);
            }
            panic!(
                "aimx serve did not start within 30s on port {port}; stderr:\n{}",
                stderr_buf.trim()
            );
        }
        if std::net::TcpStream::connect(format!("127.0.0.1:{port}")).is_ok() {
            break;
        }
        if let Ok(Some(status)) = child.try_wait() {
            let mut stderr_buf = String::new();
            if let Some(mut err) = child.stderr.take() {
                let _ = err.read_to_string(&mut stderr_buf);
            }
            panic!(
                "aimx serve exited early with {status:?} before binding port {port}; stderr:\n{}",
                stderr_buf.trim()
            );
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    child
}

fn stop_serve(mut child: Child) {
    unsafe {
        libc::kill(child.id() as libc::pid_t, libc::SIGTERM);
    }
    let _ = child.wait_timeout(std::time::Duration::from_secs(10));
}

/// Run `aimx serve` to completion, returning its exit status + stderr.
/// Used by tests that expect a hard-fail at startup (e.g. corrupted
/// `.layout-version`): the daemon never binds, so the test cannot
/// rely on `start_serve` returning successfully.
fn run_serve_expecting_exit(tmp: &Path, port: u16) -> (std::process::ExitStatus, String) {
    let runtime = tmp.join("run");
    fs::create_dir_all(&runtime).unwrap();
    let mut child = StdCommand::new(cargo_bin("aimx"))
        .env("AIMX_CONFIG_DIR", tmp)
        .env("AIMX_RUNTIME_DIR", &runtime)
        .env("AIMX_SANDBOX_FORCE_FALLBACK", "1")
        .arg("--data-dir")
        .arg(tmp)
        .arg("serve")
        .arg("--bind")
        .arg(format!("127.0.0.1:{port}"))
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn aimx serve");

    // Give the daemon a fair window to detect the corrupted marker and
    // refuse to start. 10s is generous; the actual path is microsecond
    // work.
    let outcome = child
        .wait_timeout(std::time::Duration::from_secs(15))
        .expect("wait_timeout");
    let status = match outcome {
        Some(s) => s,
        None => {
            // Daemon did not exit; kill it so the test can fail cleanly.
            let _ = child.kill();
            let _ = child.wait();
            panic!("aimx serve unexpectedly stayed alive after 15s when a hard fail was expected");
        }
    };
    let mut stderr_buf = String::new();
    if let Some(mut err) = child.stderr.take() {
        let _ = err.read_to_string(&mut stderr_buf);
    }
    (status, stderr_buf)
}

fn smtp_send(port: u16, from: &str, rcpt: &str, body_lines: &[&str]) {
    use std::io::{BufRead as _, Write as _};
    let stream = std::net::TcpStream::connect(format!("127.0.0.1:{port}")).unwrap();
    stream
        .set_read_timeout(Some(std::time::Duration::from_secs(10)))
        .unwrap();
    let mut reader = std::io::BufReader::new(stream.try_clone().unwrap());
    let mut writer = stream;

    let mut buf = String::new();
    reader.read_line(&mut buf).unwrap();
    assert!(buf.starts_with("220"), "expected SMTP banner, got: {buf}");

    buf.clear();
    write!(writer, "EHLO test.local\r\n").unwrap();
    loop {
        reader.read_line(&mut buf).unwrap();
        if buf.contains("250 ") {
            break;
        }
    }

    buf.clear();
    write!(writer, "MAIL FROM:<{from}>\r\n").unwrap();
    reader.read_line(&mut buf).unwrap();
    assert!(buf.starts_with("250"), "MAIL FROM failed: {buf}");

    buf.clear();
    write!(writer, "RCPT TO:<{rcpt}>\r\n").unwrap();
    reader.read_line(&mut buf).unwrap();
    assert!(buf.starts_with("250"), "RCPT TO failed: {buf}");

    buf.clear();
    write!(writer, "DATA\r\n").unwrap();
    reader.read_line(&mut buf).unwrap();
    assert!(buf.starts_with("354"), "DATA prompt failed: {buf}");

    for line in body_lines {
        write!(writer, "{line}\r\n").unwrap();
    }
    write!(writer, ".\r\n").unwrap();
    buf.clear();
    reader.read_line(&mut buf).unwrap();
    assert!(buf.starts_with("250"), "DATA end failed: {buf}");

    write!(writer, "QUIT\r\n").unwrap();
    let _ = reader.read_line(&mut buf);
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[test]
fn upgrade_migrates_v1_fixture_end_to_end() {
    let tmp = TempDir::new().unwrap();
    install_v1_fixture(tmp.path());

    // Sanity: fixture is shaped like v1 before the daemon starts.
    assert!(tmp.path().join("inbox").join("info").is_dir());
    assert!(tmp.path().join("inbox").join("support").is_dir());
    assert!(tmp.path().join("dkim").join("private.key").is_file());
    let cfg_before = fs::read_to_string(tmp.path().join("config.toml")).unwrap();
    assert!(
        cfg_before.contains("domain = \"fixture.example\""),
        "fixture must start with legacy `domain = ...`",
    );
    assert!(
        cfg_before.contains("[mailboxes.info]"),
        "fixture must start with legacy local-part mailbox keys",
    );

    let port = find_free_port();
    let child = start_serve(tmp.path(), port);
    stop_serve(child);

    // Post-migration layout.
    let domain = "fixture.example";
    assert!(
        tmp.path()
            .join(domain)
            .join("inbox")
            .join("info")
            .join("2026-01-15-080000-hello-world.md")
            .is_file(),
        "v1 inbox/info/<msg>.md must be readable from new per-domain path",
    );
    assert!(
        tmp.path()
            .join(domain)
            .join("inbox")
            .join("support")
            .join("2026-02-01-120000-bug-report.md")
            .is_file(),
        "v1 inbox/support/<msg>.md must be readable from new per-domain path",
    );
    assert!(
        !tmp.path().join("inbox").exists(),
        "legacy <data_dir>/inbox/ must be gone after migration",
    );
    assert!(
        !tmp.path().join("sent").exists(),
        "legacy <data_dir>/sent/ must be gone after migration",
    );
    assert!(
        tmp.path().join(domain).join("inbox").join("info").is_dir(),
        "per-domain inbox must be in place",
    );

    // DKIM relocated to per-domain dir.
    assert!(
        tmp.path()
            .join("dkim")
            .join(domain)
            .join("private.key")
            .is_file(),
        "private.key must be at <dkim_dir>/<default_domain>/ post-migration",
    );
    assert!(
        !tmp.path().join("dkim").join("private.key").is_file(),
        "legacy <dkim_dir>/private.key must be gone after migration",
    );

    // Config normalized.
    let cfg_after = fs::read_to_string(tmp.path().join("config.toml")).unwrap();
    assert!(
        cfg_after.contains("domains = ["),
        "config must carry `domains = [...]` post-migration; got:\n{cfg_after}"
    );
    assert!(
        !cfg_after
            .lines()
            .any(|l| l.trim_start().starts_with("domain =")),
        "legacy `domain = ...` scalar must be gone; got:\n{cfg_after}"
    );
    // Mailbox keys are rewritten to the canonical FQDN shape on disk by
    // the carry-over re-key in `run_mailbox_key_rekey_at_startup`. The
    // runtime data plane resolves recipients via
    // `Config::resolve_mailbox_for_rcpt` (against `mb.address`), so the
    // FQDN-keyed shape lands without breaking any downstream lookup.
    assert!(
        cfg_after.contains("[mailboxes.\"info@fixture.example\"]"),
        "FQDN mailbox key must land on disk; got:\n{cfg_after}"
    );
    assert!(
        cfg_after.contains("[mailboxes.\"support@fixture.example\"]"),
        "FQDN mailbox key must land on disk; got:\n{cfg_after}"
    );
    assert!(
        !cfg_after.contains("[mailboxes.info]"),
        "legacy local-part mailbox key must be gone; got:\n{cfg_after}"
    );
    assert!(
        !cfg_after.contains("[mailboxes.support]"),
        "legacy local-part mailbox key must be gone; got:\n{cfg_after}"
    );

    // Marker present.
    let marker = tmp.path().join(".layout-version");
    let marker_body = fs::read_to_string(&marker).unwrap();
    assert_eq!(marker_body, "2\n", "layout marker must contain `2\\n`");
}

#[test]
fn upgrade_is_idempotent_on_second_start() {
    let tmp = TempDir::new().unwrap();
    install_v1_fixture(tmp.path());

    // First start runs the migration.
    let port1 = find_free_port();
    let c1 = start_serve(tmp.path(), port1);
    stop_serve(c1);

    let cfg_after_first = fs::read_to_string(tmp.path().join("config.toml")).unwrap();
    let marker_after_first = fs::read_to_string(tmp.path().join(".layout-version")).unwrap();

    // Second start is a no-op at the migration layer. We can't observe
    // "no log line" directly from outside the daemon, but we can pin
    // that the on-disk config + marker are byte-identical to after
    // the first start (which proves no rewrite happened).
    let port2 = find_free_port();
    let c2 = start_serve(tmp.path(), port2);
    stop_serve(c2);

    let cfg_after_second = fs::read_to_string(tmp.path().join("config.toml")).unwrap();
    let marker_after_second = fs::read_to_string(tmp.path().join(".layout-version")).unwrap();

    assert_eq!(
        cfg_after_first, cfg_after_second,
        "second start must not rewrite config.toml",
    );
    assert_eq!(
        marker_after_first, marker_after_second,
        "second start must not rewrite the layout marker",
    );
}

#[test]
fn carry_over_rekey_fires_on_already_migrated_install_with_legacy_mailbox_keys() {
    // Simulate an install that's already on the v2 layout (storage +
    // DKIM relocated, `.layout-version: 2` marker present) but where
    // the mailbox keys on disk are still in their legacy local-part
    // shape. The multi-domain runtime should rewrite the mailbox keys
    // to canonical FQDN on the first start under the new binary, then
    // no-op on every subsequent start.
    let tmp = TempDir::new().unwrap();
    install_v1_fixture(tmp.path());

    // Run a manual "earlier-binary" simulation: relocate storage +
    // DKIM, rewrite the `domain → domains` field, write the marker.
    // Crucially, leave the mailbox keys in their local-part shape on
    // disk — the earlier binary's `rewrite_config_to_canonical_shape`
    // did exactly that.
    let cfg_path = tmp.path().join("config.toml");
    let legacy_cfg = fs::read_to_string(&cfg_path).unwrap();
    // Hand-rewrite `domain = "fixture.example"` to `domains = ["fixture.example"]`
    // but preserve the `[mailboxes.info]` / `[mailboxes.support]` keys.
    let domain_rewrite = legacy_cfg.replace(
        "domain = \"fixture.example\"",
        "domains = [\"fixture.example\"]",
    );
    fs::write(&cfg_path, &domain_rewrite).unwrap();
    // Relocate storage + DKIM under the per-domain root.
    let domain_dir = tmp.path().join("fixture.example");
    fs::create_dir_all(&domain_dir).unwrap();
    fs::rename(tmp.path().join("inbox"), domain_dir.join("inbox")).unwrap();
    fs::rename(tmp.path().join("sent"), domain_dir.join("sent")).unwrap();
    let dkim_dir = tmp.path().join("dkim");
    let dkim_domain_dir = dkim_dir.join("fixture.example");
    fs::create_dir_all(&dkim_domain_dir).unwrap();
    if dkim_dir.join("private.key").is_file() {
        fs::rename(
            dkim_dir.join("private.key"),
            dkim_domain_dir.join("private.key"),
        )
        .unwrap();
    }
    if dkim_dir.join("public.key").is_file() {
        fs::rename(
            dkim_dir.join("public.key"),
            dkim_domain_dir.join("public.key"),
        )
        .unwrap();
    }
    // Write the v2 marker so the upgrade migration short-circuits.
    fs::write(tmp.path().join(".layout-version"), "2\n").unwrap();

    // Confirm the earlier state on disk has legacy mailbox keys.
    let pre_cfg = fs::read_to_string(&cfg_path).unwrap();
    assert!(
        pre_cfg.contains("[mailboxes.info]"),
        "fixture must carry the earlier shape with legacy local-part keys; got:\n{pre_cfg}"
    );

    // First start under the multi-domain binary triggers the
    // mailbox-key FQDN re-key.
    let port = find_free_port();
    let child = start_serve(tmp.path(), port);
    stop_serve(child);

    let after_first = fs::read_to_string(&cfg_path).unwrap();
    assert!(
        after_first.contains("[mailboxes.\"info@fixture.example\"]"),
        "carry-over re-key must move legacy mailbox keys to FQDN; got:\n{after_first}"
    );
    assert!(
        !after_first.contains("[mailboxes.info]"),
        "legacy local-part mailbox key must be gone; got:\n{after_first}"
    );

    // Second start is a no-op — already canonical on disk.
    let port2 = find_free_port();
    let child2 = start_serve(tmp.path(), port2);
    stop_serve(child2);

    let after_second = fs::read_to_string(&cfg_path).unwrap();
    assert_eq!(
        after_first, after_second,
        "second start must not rewrite config.toml — the re-key must be idempotent",
    );
}

#[test]
fn upgrade_hard_fails_on_corrupted_marker() {
    let tmp = TempDir::new().unwrap();
    install_v1_fixture(tmp.path());
    // Plant a bogus marker before first start. Per the design: the
    // marker is the source of truth, and a wrong-version marker is a
    // hard startup error rather than a "ignore + remigrate" path.
    fs::write(tmp.path().join(".layout-version"), "99\n").unwrap();

    let port = find_free_port();
    let (status, stderr) = run_serve_expecting_exit(tmp.path(), port);
    assert!(
        !status.success(),
        "expected non-zero exit on corrupted marker; got {status:?}",
    );
    assert!(
        stderr.contains("upgrade migration failed"),
        "stderr must carry the canonical migration-failure prefix; got:\n{stderr}"
    );
    assert!(
        stderr.contains("book/multi-domain.md"),
        "stderr must point at book/multi-domain.md; got:\n{stderr}"
    );
    assert!(
        stderr.contains("99") || stderr.contains("expected '2'"),
        "stderr must reference the corrupted marker value; got:\n{stderr}"
    );
}

/// Post-migration SMTP RCPT TO must land in the new FQDN-keyed mailbox
/// storage path. The migration deliberately preserves the legacy
/// in-memory friendly-key shape for the current daemon session and
/// teaches `Config::inbox_dir` / `Config::sent_dir` to route to the
/// per-domain layout once the marker is on disk, so RCPT to a v1
/// local-part keeps working post-migration and the file lands under
/// `<data_dir>/<default_domain>/inbox/<local-part>/`.
#[test]
fn upgrade_smtp_rcpt_lands_in_new_fqdn_keyed_path() {
    let tmp = TempDir::new().unwrap();
    install_v1_fixture(tmp.path());
    let port = find_free_port();
    let child = start_serve(tmp.path(), port);

    let body_lines = &[
        "From: smoke@sender.example",
        "To: info@fixture.example",
        "Subject: post-migration smoke",
        "Date: Tue, 03 Feb 2026 10:00:00 +0000",
        "Message-ID: <smoke-1@sender.example>",
        "",
        "Body of the smoke test.",
    ];
    smtp_send(
        port,
        "smoke@sender.example",
        "info@fixture.example",
        body_lines,
    );

    let started = std::time::Instant::now();
    let new_inbox = tmp
        .path()
        .join("fixture.example")
        .join("inbox")
        .join("info");
    loop {
        if started.elapsed() > std::time::Duration::from_secs(10) {
            stop_serve(child);
            let snapshot: Vec<String> = fs::read_dir(&new_inbox)
                .unwrap()
                .filter_map(|e| e.ok())
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect();
            panic!(
                "post-migration ingest never landed under {}; current contents: {snapshot:?}",
                new_inbox.display()
            );
        }
        let landed = fs::read_dir(&new_inbox)
            .unwrap()
            .filter_map(|e| e.ok())
            .any(|e| {
                let n = e.file_name().to_string_lossy().into_owned();
                n.contains("smoke") || n.contains("post-migration")
            });
        if landed {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }

    stop_serve(child);
}
