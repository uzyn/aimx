//! End-to-end integration tests for the `DOMAIN-REMOVE` UDS verb and
//! the `aimx domains remove` CLI.
//!
//! Setup mirrors `tests/domains_uds.rs`: spin up `aimx serve` against
//! a multi-domain v2 install, exercise the CLI as a separate
//! subprocess, and assert the expected wire / on-disk effects.
//!
//! Every test here requires root because `Action::DomainCrud` is
//! root-only. Non-root local runs skip with a single stderr line.

use std::io::{BufRead, Write};
use std::net::TcpStream;
use std::path::Path;
use std::process::{Command as StdCommand, Stdio};
use std::sync::LazyLock;
use std::time::{Duration, Instant};

use tempfile::TempDir;
use wait_timeout::ChildExt;

fn aimx_binary_path() -> std::path::PathBuf {
    let target_dir = std::env::var("CARGO_TARGET_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target"));
    let profile = if cfg!(debug_assertions) {
        "debug"
    } else {
        "release"
    };
    target_dir.join(profile).join("aimx")
}

/// `DOMAIN-CRUD` is root-only. Tests that route through the daemon
/// need to bail out on a non-root local run.
fn skip_if_not_root() -> bool {
    if unsafe { libc::geteuid() } == 0 {
        return false;
    }
    eprintln!("skipping DOMAIN-REMOVE UDS test: requires root; DOMAIN-CRUD is root-only");
    true
}

/// One-shot DKIM keypair cache shared across tests. Avoids re-running
/// `aimx dkim-keygen` (~200ms) per test.
static DR_DKIM_CACHE: LazyLock<TempDir> = LazyLock::new(|| {
    let cache = TempDir::new().expect("create DKIM cache");
    let config = format!(
        "domain = \"dr-cache.example.com\"\ndata_dir = \"{}\"\n\n[mailboxes.catchall]\naddress = \"*@dr-cache.example.com\"\nowner = \"aimx-catchall\"\n",
        cache.path().display()
    );
    std::fs::write(cache.path().join("config.toml"), config).unwrap();
    let status = StdCommand::new(aimx_binary_path())
        .env("AIMX_CONFIG_DIR", cache.path())
        .arg("dkim-keygen")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("failed to spawn dkim-keygen");
    assert!(status.success(), "dkim-keygen exited non-zero");
    cache
});

fn install_dkim_under(domain_dir: &Path) {
    std::fs::create_dir_all(domain_dir).unwrap();
    let cache_dkim = DR_DKIM_CACHE
        .path()
        .join("dkim")
        .join("dr-cache.example.com");
    for name in ["private.key", "public.key"] {
        let src = cache_dkim.join(name);
        let dst = domain_dir.join(name);
        if src.exists() {
            std::fs::copy(&src, &dst).unwrap();
        }
    }
}

fn current_username() -> String {
    unsafe {
        let uid = libc::geteuid();
        if uid == 0 {
            if let Some(sudo_user) = std::env::var_os("SUDO_USER") {
                let name = sudo_user.to_string_lossy().into_owned();
                if !name.is_empty() && name != "root" {
                    return name;
                }
            }
            return "nobody".to_string();
        }
        let pw = libc::getpwuid(uid);
        if pw.is_null() {
            return format!("uid{uid}");
        }
        let cstr = std::ffi::CStr::from_ptr((*pw).pw_name);
        cstr.to_string_lossy().to_string()
    }
}

/// Provision a two-domain v2 install (a.com + b.com) under `tmp`.
/// `b_mailboxes` is the list of local-parts to create on b.com so
/// each individual test can pick its preferred blocker scenario.
fn setup_two_domain_env(tmp: &Path, b_mailboxes: &[&str]) {
    let owner = current_username();
    let mut cfg = format!(
        r#"domains = ["a.com", "b.com"]
data_dir = "{tmp_path}"

[mailboxes."info@a.com"]
address = "info@a.com"
owner = "{owner}"
"#,
        tmp_path = tmp.display(),
    );
    for name in b_mailboxes {
        cfg.push_str(&format!(
            "\n[mailboxes.\"{name}@b.com\"]\naddress = \"{name}@b.com\"\nowner = \"{owner}\"\n",
        ));
    }
    std::fs::write(tmp.join("config.toml"), cfg).unwrap();
    std::fs::write(tmp.join(".layout-version"), "2\n").unwrap();

    install_dkim_under(&tmp.join("dkim").join("a.com"));
    install_dkim_under(&tmp.join("dkim").join("b.com"));

    // Per-domain dirs + a.com info mailbox storage.
    for domain in ["a.com", "b.com"] {
        let domain_root = tmp.join(domain);
        std::fs::create_dir_all(&domain_root).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&domain_root, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
    }
    for folder in ["inbox", "sent"] {
        let p = tmp.join("a.com").join(folder).join("info");
        std::fs::create_dir_all(&p).unwrap();
    }
    for name in b_mailboxes {
        for folder in ["inbox", "sent"] {
            let p = tmp.join("b.com").join(folder).join(name);
            std::fs::create_dir_all(&p).unwrap();
        }
    }
}

/// Single-domain install (a.com only) so the last-domain hard-block
/// can be exercised end-to-end.
fn setup_single_domain_env(tmp: &Path) {
    let owner = current_username();
    let cfg = format!(
        r#"domains = ["a.com"]
data_dir = "{tmp_path}"

[mailboxes."info@a.com"]
address = "info@a.com"
owner = "{owner}"
"#,
        tmp_path = tmp.display(),
    );
    std::fs::write(tmp.join("config.toml"), cfg).unwrap();
    std::fs::write(tmp.join(".layout-version"), "2\n").unwrap();
    install_dkim_under(&tmp.join("dkim").join("a.com"));
    for folder in ["inbox", "sent"] {
        std::fs::create_dir_all(tmp.join("a.com").join(folder).join("info")).unwrap();
    }
}

/// Seed a `.md` stub into the named mailbox so the cascade wipe path
/// has something to remove.
fn seed_mailbox_message(tmp: &Path, domain: &str, local: &str) {
    let dir = tmp.join(domain).join("inbox").join(local);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("2026-05-23-stub.md"),
        "+++\nid = \"stub\"\n+++\n\nhello\n",
    )
    .unwrap();
}

fn find_free_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.local_addr().unwrap().port()
}

fn wait_for_listener(port: u16) {
    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(30) {
        if TcpStream::connect(format!("127.0.0.1:{port}")).is_ok() {
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    panic!("aimx serve did not start within 30s on port {port}");
}

fn start_serve(tmp: &Path, port: u16) -> std::process::Child {
    let runtime = tmp.join("run");
    std::fs::create_dir_all(&runtime).ok();
    StdCommand::new(aimx_binary_path())
        .env("AIMX_CONFIG_DIR", tmp)
        .env("AIMX_RUNTIME_DIR", &runtime)
        .arg("--data-dir")
        .arg(tmp)
        .arg("serve")
        .arg("--bind")
        .arg(format!("127.0.0.1:{port}"))
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn aimx serve")
}

fn shutdown(child: &mut std::process::Child) {
    unsafe {
        libc::kill(child.id() as libc::pid_t, libc::SIGTERM);
    }
    let _ = child.wait_timeout(Duration::from_secs(10));
}

fn run_domains_remove(tmp: &Path, domain: &str, force: bool) -> std::process::Output {
    let runtime = tmp.join("run");
    let mut cmd = StdCommand::new(aimx_binary_path());
    cmd.env("AIMX_CONFIG_DIR", tmp)
        .env("AIMX_RUNTIME_DIR", &runtime)
        .env("NO_COLOR", "1")
        .arg("--data-dir")
        .arg(tmp)
        .arg("domains")
        .arg("remove")
        .arg(domain);
    if force {
        cmd.arg("--force");
    }
    cmd.output().expect("failed to spawn aimx domains remove")
}

fn smtp_rcpt_status(port: u16, from: &str, rcpt: &str) -> String {
    let stream = TcpStream::connect(format!("127.0.0.1:{port}")).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let mut reader = std::io::BufReader::new(stream.try_clone().unwrap());
    let mut writer = stream;

    let mut buf = String::new();
    reader.read_line(&mut buf).unwrap();
    assert!(buf.starts_with("220"), "banner: {buf}");

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
    assert!(buf.starts_with("250"), "MAIL FROM: {buf}");

    buf.clear();
    write!(writer, "RCPT TO:<{rcpt}>\r\n").unwrap();
    reader.read_line(&mut buf).unwrap();
    let rcpt_response = buf.clone();

    let _ = write!(writer, "QUIT\r\n");
    let mut sink = String::new();
    let _ = reader.read_line(&mut sink);

    rcpt_response
}

/// Clean remove (no mailboxes on b.com, no `--force` needed). The CLI
/// exits 0, config drops b.com, the DKIM keypair is preserved on
/// disk, and the running daemon hot-reloads (SMTP RCPT to `@b.com`
/// is now rejected).
#[test]
fn remove_clean_no_blockers_drops_domain_and_preserves_dkim() {
    if skip_if_not_root() {
        return;
    }
    let tmp = TempDir::new().unwrap();
    setup_two_domain_env(tmp.path(), &[]); // no b.com mailboxes
    let port = find_free_port();
    let mut child = start_serve(tmp.path(), port);
    wait_for_listener(port);

    let out = run_domains_remove(tmp.path(), "b.com", false);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "clean remove must succeed: stdout={stdout} stderr={stderr}",
    );
    assert!(
        stdout.contains("Removed domain"),
        "missing success line; stdout={stdout}"
    );
    assert!(
        stdout.contains("DKIM keypair preserved"),
        "missing DKIM-preserved hint; stdout={stdout}",
    );

    // Config rewritten on disk.
    let cfg = std::fs::read_to_string(tmp.path().join("config.toml")).unwrap();
    assert!(!cfg.contains("\"b.com\""), "config still has b.com: {cfg}");
    assert!(cfg.contains("\"a.com\""), "config lost a.com: {cfg}");

    // DKIM keypair preserved on disk.
    let dkim_b = tmp.path().join("dkim").join("b.com");
    assert!(
        dkim_b.join("private.key").is_file(),
        "DKIM private.key must be preserved after remove",
    );
    assert!(
        dkim_b.join("public.key").is_file(),
        "DKIM public.key must be preserved after remove",
    );

    // Hot-reload: RCPT to a b.com address now rejected with 5.7.x.
    let resp = smtp_rcpt_status(port, "sender@example.com", "info@b.com");
    assert!(
        !resp.starts_with("250"),
        "RCPT to removed domain must NOT be accepted post-remove (got: {resp})",
    );

    shutdown(&mut child);
}

/// Default refusal: b.com still has mailboxes; remove without
/// `--force` exits non-zero, prints the numbered blocker list, and
/// suggests `--force`. State on disk is unchanged.
#[test]
fn remove_blocked_lists_mailboxes_and_suggests_force() {
    if skip_if_not_root() {
        return;
    }
    let tmp = TempDir::new().unwrap();
    setup_two_domain_env(tmp.path(), &["info", "alice", "support"]);
    let port = find_free_port();
    let mut child = start_serve(tmp.path(), port);
    wait_for_listener(port);

    let out = run_domains_remove(tmp.path(), "b.com", false);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success(),
        "blocked remove must exit non-zero; stdout={stdout} stderr={stderr}",
    );
    // All three blocker FQDNs must appear in the printed list.
    for fqdn in ["info@b.com", "alice@b.com", "support@b.com"] {
        assert!(
            stdout.contains(fqdn),
            "missing blocker {fqdn} in stdout: {stdout}"
        );
    }
    assert!(
        stdout.contains("--force"),
        "missing --force suggestion: stdout={stdout}",
    );

    // Config on disk unchanged.
    let cfg = std::fs::read_to_string(tmp.path().join("config.toml")).unwrap();
    assert!(cfg.contains("\"b.com\""), "config lost b.com: {cfg}");
    assert!(
        cfg.contains("\"info@b.com\""),
        "config lost a b.com mailbox: {cfg}"
    );

    shutdown(&mut child);
}

/// Last-domain hard-block: the single-domain install refuses both
/// `aimx domains remove a.com` and `... --force a.com`, and the
/// error message points operators at `aimx uninstall`.
#[test]
fn remove_last_domain_hard_blocks_even_with_force() {
    if skip_if_not_root() {
        return;
    }
    let tmp = TempDir::new().unwrap();
    setup_single_domain_env(tmp.path());
    let port = find_free_port();
    let mut child = start_serve(tmp.path(), port);
    wait_for_listener(port);

    for force in [false, true] {
        let out = run_domains_remove(tmp.path(), "a.com", force);
        let stdout = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);
        let combined = format!("stdout={stdout}\nstderr={stderr}");
        assert!(
            !out.status.success(),
            "last-domain hard-block must reject (force={force}); {combined}"
        );
        assert!(
            combined.contains("last configured domain"),
            "last-domain wording missing (force={force}); {combined}"
        );
        assert!(
            combined.contains("aimx uninstall"),
            "aimx uninstall hint missing (force={force}); {combined}"
        );
    }

    // Config on disk unchanged.
    let cfg = std::fs::read_to_string(tmp.path().join("config.toml")).unwrap();
    assert!(cfg.contains("\"a.com\""));

    shutdown(&mut child);
}

/// `--force` cascade: three mailboxes on b.com (with seeded mail),
/// run remove with --force, assert config dropped b.com + every
/// mailbox, storage tree gone, DKIM keypair preserved on disk.
#[test]
fn remove_force_cascade_wipes_mailboxes_and_keeps_dkim() {
    if skip_if_not_root() {
        return;
    }
    let tmp = TempDir::new().unwrap();
    setup_two_domain_env(tmp.path(), &["info", "alice", "support"]);
    for local in ["info", "alice", "support"] {
        seed_mailbox_message(tmp.path(), "b.com", local);
    }
    // Also seed a.com so we can verify it survives the cascade.
    seed_mailbox_message(tmp.path(), "a.com", "info");

    let port = find_free_port();
    let mut child = start_serve(tmp.path(), port);
    wait_for_listener(port);

    let out = run_domains_remove(tmp.path(), "b.com", true);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "force cascade must succeed: stdout={stdout} stderr={stderr}",
    );
    assert!(stdout.contains("Removed domain (cascade)"));
    assert!(stdout.contains("info@b.com"));
    assert!(stdout.contains("alice@b.com"));
    assert!(stdout.contains("support@b.com"));
    assert!(stdout.contains("Storage tree removed"));
    assert!(stdout.contains("DKIM keypair preserved"));

    // Config dropped b.com and all its mailboxes.
    let cfg = std::fs::read_to_string(tmp.path().join("config.toml")).unwrap();
    assert!(!cfg.contains("\"b.com\""), "config still has b.com: {cfg}");
    assert!(
        !cfg.contains("@b.com"),
        "config still has b.com mailboxes: {cfg}"
    );

    // b.com storage tree gone.
    assert!(!tmp.path().join("b.com").exists());

    // DKIM keypair preserved on disk.
    let dkim_b = tmp.path().join("dkim").join("b.com");
    assert!(dkim_b.join("private.key").is_file());
    assert!(dkim_b.join("public.key").is_file());

    // a.com mailbox storage untouched.
    let a_stub = tmp
        .path()
        .join("a.com")
        .join("inbox")
        .join("info")
        .join("2026-05-23-stub.md");
    assert!(a_stub.is_file(), "a.com message must survive b.com cascade",);

    // Hot-reload: SMTP RCPT to a.com still accepted, b.com rejected.
    let resp_a = smtp_rcpt_status(port, "ext@example.com", "info@a.com");
    assert!(
        resp_a.starts_with("250"),
        "a.com RCPT must still be accepted post-cascade: {resp_a}"
    );
    let resp_b = smtp_rcpt_status(port, "ext@example.com", "info@b.com");
    assert!(
        !resp_b.starts_with("250"),
        "b.com RCPT must be rejected post-cascade: {resp_b}"
    );

    shutdown(&mut child);
}

/// Concurrent-ingest stress: while a background thread hammers SMTP
/// RCPT to a.com (the surviving domain), invoke
/// `domains remove b.com --force` on the main thread. Both must
/// complete within a reasonable time without blocking each other and
/// without deadlocking.
///
/// This pins the lock-hierarchy invariant: the per-mailbox locks the
/// cascade acquires are scoped to b.com mailboxes, so ingest into
/// `info@a.com` (a different mailbox key) must not contend with the
/// cascade.
#[test]
fn remove_force_does_not_block_concurrent_ingest_on_surviving_domain() {
    if skip_if_not_root() {
        return;
    }
    let tmp = TempDir::new().unwrap();
    setup_two_domain_env(tmp.path(), &["info"]);
    seed_mailbox_message(tmp.path(), "b.com", "info");

    let port = find_free_port();
    let mut child = start_serve(tmp.path(), port);
    wait_for_listener(port);

    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let stop_clone = std::sync::Arc::clone(&stop);
    let port_for_thread = port;
    // Spawn the ingest stressor — hammers RCPT TO info@a.com in a
    // tight loop until told to stop. We count accepted RCPTs as a
    // sanity signal.
    let handle = std::thread::spawn(move || {
        let mut accepted: u32 = 0;
        while !stop_clone.load(std::sync::atomic::Ordering::Relaxed) {
            let resp = smtp_rcpt_status(port_for_thread, "ext@example.com", "info@a.com");
            if resp.starts_with("250") {
                accepted += 1;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        accepted
    });

    // Give the stressor a moment to ramp up.
    std::thread::sleep(Duration::from_millis(100));

    // Run the cascade with a generous timeout. If the lock hierarchy
    // is wrong this either deadlocks (we'd hit our outer harness
    // timeout) or blocks long enough that the test surfaces the bug.
    let cascade_started = Instant::now();
    let out = run_domains_remove(tmp.path(), "b.com", true);
    let cascade_duration = cascade_started.elapsed();

    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    let accepted = handle.join().expect("stressor thread did not join");

    assert!(
        out.status.success(),
        "cascade must succeed under concurrent ingest; stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    // Whatever the actual wall-clock is, it must fit comfortably
    // inside the timeout that catches a deadlock — 10 s is plenty
    // for an empty-ish install on a CI runner.
    assert!(
        cascade_duration < Duration::from_secs(10),
        "cascade took too long ({cascade_duration:?}) — likely lock contention",
    );
    // The stressor should have managed at least one accepted RCPT
    // during the cascade window — if every attempt during the
    // cascade were blocked, the inversion would be obvious.
    assert!(
        accepted > 0,
        "ingest stressor must have completed at least one RCPT during cascade",
    );

    // Post-cascade: a.com survives, b.com is gone.
    assert!(tmp.path().join("a.com").is_dir());
    assert!(!tmp.path().join("b.com").exists());

    shutdown(&mut child);
}

/// Daemon-stopped fallback: non-root must hard-error with the
/// canonical hint.
#[test]
fn remove_daemon_stopped_non_root_hard_errors() {
    if unsafe { libc::geteuid() } == 0 {
        eprintln!("skipping non-root fallback test: running as root");
        return;
    }
    let tmp = TempDir::new().unwrap();
    setup_two_domain_env(tmp.path(), &[]);

    // Daemon never started.
    let out = run_domains_remove(tmp.path(), "b.com", false);
    assert!(!out.status.success(), "non-root must hard-error");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("daemon must be running"),
        "expected canonical hint; got: {stderr}"
    );
}

/// Daemon-stopped fallback: root falls back to direct config edit.
#[test]
fn remove_daemon_stopped_root_falls_back_to_direct_edit() {
    if skip_if_not_root() {
        return;
    }
    let tmp = TempDir::new().unwrap();
    setup_two_domain_env(tmp.path(), &[]);

    // Do NOT start the daemon. Root should fall back to direct edit.
    let out = run_domains_remove(tmp.path(), "b.com", false);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "root fallback must succeed: stdout={stdout} stderr={stderr}",
    );

    let cfg = std::fs::read_to_string(tmp.path().join("config.toml")).unwrap();
    assert!(!cfg.contains("\"b.com\""), "config still has b.com: {cfg}");

    // DKIM keypair preserved on disk.
    let dkim_b = tmp.path().join("dkim").join("b.com");
    assert!(dkim_b.join("private.key").is_file());

    // CLI surfaced the restart hint.
    let combined = format!("stdout={stdout} stderr={stderr}");
    assert!(
        combined.contains("restart") || combined.contains("daemon"),
        "missing restart hint: {combined}"
    );
}
