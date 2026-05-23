//! End-to-end integration tests for the `DOMAIN-LIST` and
//! `DOMAIN-ADD` UDS verbs and the `aimx domains` CLI.
//!
//! Setup mirrors `tests/multi_domain.rs`: spin up `aimx serve` against
//! a two-domain v2 install, exercise the CLI as a separate subprocess,
//! and assert the expected wire / on-disk effects.
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

/// `DOMAIN-CRUD` is root-only (Action::DomainCrud). Tests that route
/// through the daemon need to bail out on a non-root local run.
fn skip_if_not_root() -> bool {
    if unsafe { libc::geteuid() } == 0 {
        return false;
    }
    eprintln!("skipping DOMAIN-* UDS test: requires root; DOMAIN-CRUD is root-only");
    true
}

/// One-shot pre-generated DKIM keypair shared across tests in this
/// file. Avoids re-running `aimx dkim-keygen` (~200ms) per test.
static DD_DKIM_CACHE: LazyLock<TempDir> = LazyLock::new(|| {
    let cache = TempDir::new().expect("create DKIM cache");
    // Seed a config so `aimx dkim-keygen` (no --domain) defaults to
    // the cached single-domain install.
    let config = format!(
        "domain = \"dd-cache.example.com\"\ndata_dir = \"{}\"\n\n[mailboxes.catchall]\naddress = \"*@dd-cache.example.com\"\nowner = \"aimx-catchall\"\n",
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
    // `aimx dkim-keygen` (no `--domain`) writes under
    // `<dkim_dir>/<default_domain>/` — the cache config sets the
    // default domain to `dd-cache.example.com`, so the source path
    // is nested under that subdir.
    let cache_dkim = DD_DKIM_CACHE
        .path()
        .join("dkim")
        .join("dd-cache.example.com");
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

/// Provision a single-domain v2 install (a.com only) under `tmp`.
/// Tests then add a second domain via the CLI and assert the result.
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
        let dir = tmp.join("a.com").join(folder).join("info");
        std::fs::create_dir_all(&dir).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(tmp.join("a.com"), std::fs::Permissions::from_mode(0o755))
            .unwrap();
    }
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

/// Run `aimx domains list` against the running daemon and return the
/// full stdout. The CLI exits non-zero only on real errors; pre-add
/// the test asserts the table contains the expected domain.
fn run_domains_list(tmp: &Path) -> std::process::Output {
    let runtime = tmp.join("run");
    StdCommand::new(aimx_binary_path())
        .env("AIMX_CONFIG_DIR", tmp)
        .env("AIMX_RUNTIME_DIR", &runtime)
        .env("NO_COLOR", "1")
        .arg("--data-dir")
        .arg(tmp)
        .arg("domains")
        .arg("list")
        .output()
        .expect("failed to spawn aimx domains list")
}

fn run_domains_add(tmp: &Path, domain: &str, selector: Option<&str>) -> std::process::Output {
    let runtime = tmp.join("run");
    let mut cmd = StdCommand::new(aimx_binary_path());
    cmd.env("AIMX_CONFIG_DIR", tmp)
        .env("AIMX_RUNTIME_DIR", &runtime)
        .env("NO_COLOR", "1")
        .arg("--data-dir")
        .arg(tmp)
        .arg("domains")
        .arg("add")
        .arg(domain)
        .arg("--no-dns-check");
    if let Some(s) = selector {
        cmd.arg("--selector").arg(s);
    }
    cmd.output().expect("failed to spawn aimx domains add")
}

#[test]
fn domains_list_returns_initial_domain() {
    if skip_if_not_root() {
        return;
    }
    let tmp = TempDir::new().unwrap();
    setup_single_domain_env(tmp.path());
    let port = find_free_port();
    let mut child = start_serve(tmp.path(), port);
    wait_for_listener(port);

    let out = run_domains_list(tmp.path());
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "domains list failed: stdout={stdout} stderr={stderr}"
    );
    assert!(stdout.contains("a.com"), "stdout: {stdout}");
    assert!(stdout.contains("DOMAIN"), "header missing: {stdout}");

    shutdown(&mut child);
}

/// Full end-to-end: add b.com to a single-domain install, assert
/// config is rewritten, DKIM keypair lands on disk, daemon hot-reloads
/// (the new domain shows in `aimx domains list` AND SMTP RCPT to
/// `@b.com` is accepted immediately).
#[test]
fn domains_add_full_flow_hot_reloads_smtp_and_config() {
    if skip_if_not_root() {
        return;
    }
    let tmp = TempDir::new().unwrap();
    setup_single_domain_env(tmp.path());
    let port = find_free_port();
    let mut child = start_serve(tmp.path(), port);
    wait_for_listener(port);

    // Pre-condition: a.com only.
    let pre = run_domains_list(tmp.path());
    let pre_stdout = String::from_utf8_lossy(&pre.stdout);
    assert!(pre_stdout.contains("a.com"), "pre stdout: {pre_stdout}");
    assert!(!pre_stdout.contains("b.com"), "pre stdout: {pre_stdout}");

    // Add b.com.
    let add = run_domains_add(tmp.path(), "b.com", Some("s2025"));
    let add_stdout = String::from_utf8_lossy(&add.stdout);
    let add_stderr = String::from_utf8_lossy(&add.stderr);
    assert!(
        add.status.success(),
        "domains add failed: stdout={add_stdout} stderr={add_stderr}"
    );

    // DKIM keypair exists at the per-domain layout.
    let dkim_b = tmp.path().join("dkim").join("b.com");
    assert!(
        dkim_b.join("private.key").is_file(),
        "DKIM private.key missing under {}",
        dkim_b.display()
    );
    assert!(
        dkim_b.join("public.key").is_file(),
        "DKIM public.key missing under {}",
        dkim_b.display()
    );

    // Config rewritten on disk with both domains and a `[domain."b.com"]`
    // sub-table carrying the selector override.
    let config_text = std::fs::read_to_string(tmp.path().join("config.toml")).unwrap();
    assert!(
        config_text.contains("domains") && config_text.contains("\"b.com\""),
        "config.toml missing b.com: {config_text}"
    );
    assert!(
        config_text.contains("s2025"),
        "config.toml missing selector override: {config_text}"
    );

    // Hot-reload: the same running daemon now lists b.com.
    let post = run_domains_list(tmp.path());
    let post_stdout = String::from_utf8_lossy(&post.stdout);
    assert!(
        post_stdout.contains("b.com"),
        "post stdout missing b.com: {post_stdout}"
    );

    // SMTP RCPT to `@b.com` is accepted by the same running daemon.
    // We send to the per-domain catchall lookup — without an explicit
    // mailbox on b.com, the recipient address must still pass the
    // domain check (rejection would happen later on no-mailbox, not at
    // domain-check time). `recipient_domain_matches_any` is the only
    // gate at RCPT time; mailbox resolution happens at DATA / dispatch.
    let resp = smtp_rcpt_status(port, "sender@example.com", "info@b.com");
    // The daemon may accept the RCPT (domain valid) and reject later;
    // what matters is that the RCPT is NOT rejected with 550 5.7.1
    // ("relay not permitted") on a domain miss. Accepting 250 (any
    // mailbox match path) or 550 5.1.1 (no such user) both prove the
    // domain was recognized.
    assert!(
        resp.starts_with("250") || resp.contains("5.1.1"),
        "RCPT to @b.com must be domain-recognized (got: {resp})"
    );

    shutdown(&mut child);
}

#[test]
fn domains_add_duplicate_returns_error_and_leaves_state_unchanged() {
    if skip_if_not_root() {
        return;
    }
    let tmp = TempDir::new().unwrap();
    setup_single_domain_env(tmp.path());
    let port = find_free_port();
    let mut child = start_serve(tmp.path(), port);
    wait_for_listener(port);

    let out = run_domains_add(tmp.path(), "a.com", None);
    let combined = format!(
        "stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    assert!(
        !out.status.success(),
        "duplicate add must fail; got: {combined}"
    );
    assert!(
        combined.contains("already configured"),
        "expected duplicate-add reason; got: {combined}"
    );

    shutdown(&mut child);
}

#[test]
fn dkim_keygen_with_domain_flag_writes_under_per_domain_dir() {
    // Doesn't need root — `aimx dkim-keygen` writes to <dkim_dir>
    // resolved from `AIMX_CONFIG_DIR`, so a tempdir-rooted run works
    // for any uid.
    let tmp = TempDir::new().unwrap();
    setup_single_domain_env(tmp.path());

    let out = StdCommand::new(aimx_binary_path())
        .env("AIMX_CONFIG_DIR", tmp.path())
        .arg("--data-dir")
        .arg(tmp.path())
        .arg("dkim-keygen")
        .arg("--domain")
        .arg("a.com")
        .arg("--selector")
        .arg("test-selector")
        .arg("--force")
        .output()
        .expect("dkim-keygen");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "dkim-keygen --domain failed: stdout={stdout} stderr={stderr}"
    );

    let key = tmp.path().join("dkim").join("a.com").join("private.key");
    assert!(key.is_file(), "expected key at {}", key.display());
}

#[test]
fn dkim_keygen_with_unknown_domain_refuses() {
    let tmp = TempDir::new().unwrap();
    setup_single_domain_env(tmp.path());

    let out = StdCommand::new(aimx_binary_path())
        .env("AIMX_CONFIG_DIR", tmp.path())
        .arg("--data-dir")
        .arg(tmp.path())
        .arg("dkim-keygen")
        .arg("--domain")
        .arg("never-added.example")
        .output()
        .expect("dkim-keygen");
    assert!(
        !out.status.success(),
        "dkim-keygen must refuse unknown domain"
    );
    let combined = format!(
        "stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    assert!(
        combined.contains("not in `domains") || combined.contains("aimx domains add"),
        "error must mention the missing-domain hint; got: {combined}"
    );
}

#[test]
fn dkim_keygen_without_domain_uses_legacy_root_path() {
    // The `aimx dkim-keygen` invocation without `--domain` keeps the
    // legacy un-namespaced output path (`<dkim_dir>/{private,public}.key`)
    // so existing single-domain scripts continue to work. The daemon's
    // startup loader falls back to that path for the default domain
    // when no per-domain key exists.
    let tmp = TempDir::new().unwrap();
    setup_single_domain_env(tmp.path());

    // Remove the pre-installed per-domain key so we can distinguish
    // legacy-path vs per-domain-path output.
    let _ = std::fs::remove_dir_all(tmp.path().join("dkim").join("a.com"));

    let out = StdCommand::new(aimx_binary_path())
        .env("AIMX_CONFIG_DIR", tmp.path())
        .arg("--data-dir")
        .arg(tmp.path())
        .arg("dkim-keygen")
        .output()
        .expect("dkim-keygen");
    assert!(
        out.status.success(),
        "dkim-keygen without --domain must succeed for back-compat"
    );
    // Legacy un-namespaced root: `<dkim_dir>/private.key`.
    let key = tmp.path().join("dkim").join("private.key");
    assert!(
        key.is_file(),
        "expected legacy un-namespaced key at {}",
        key.display()
    );
}

/// Daemon-stopped fallback: non-root caller hard-errors with the
/// canonical hint; root caller writes config directly.
#[test]
fn domains_add_daemon_stopped_non_root_hard_errors() {
    if unsafe { libc::geteuid() } == 0 {
        eprintln!("skipping non-root fallback test: running as root");
        return;
    }
    let tmp = TempDir::new().unwrap();
    setup_single_domain_env(tmp.path());

    // Daemon never started: socket missing.
    let out = run_domains_add(tmp.path(), "b.com", None);
    assert!(!out.status.success(), "non-root must hard-error");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("daemon must be running"),
        "expected canonical hint; got: {stderr}"
    );
}

#[test]
fn domains_add_daemon_stopped_root_falls_back_to_direct_edit() {
    if skip_if_not_root() {
        return;
    }
    let tmp = TempDir::new().unwrap();
    setup_single_domain_env(tmp.path());

    // Do NOT start the daemon. Root caller should fall back to direct
    // config edit + DKIM keygen.
    let out = run_domains_add(tmp.path(), "b.com", None);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "root fallback must succeed: stdout={stdout} stderr={stderr}"
    );

    // Config + DKIM landed on disk.
    let config = std::fs::read_to_string(tmp.path().join("config.toml")).unwrap();
    assert!(
        config.contains("\"b.com\""),
        "config missing b.com after fallback: {config}"
    );
    assert!(
        tmp.path()
            .join("dkim")
            .join("b.com")
            .join("private.key")
            .is_file(),
        "DKIM key missing after fallback"
    );

    // The CLI must surface a "restart daemon" hint so the operator
    // knows the running serve (if it were started) wouldn't pick up
    // the change automatically.
    let combined = format!("stdout={stdout} stderr={stderr}");
    assert!(
        combined.contains("restart") || combined.contains("daemon"),
        "missing restart hint; got: {combined}"
    );
}
