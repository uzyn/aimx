//! End-to-end integration tests for multi-domain inbound SMTP intake.
//!
//! These tests spin up `aimx serve` against a two-domain config and
//! verify that:
//! - RCPT TO for each configured domain is accepted (regardless of
//!   which one is the default).
//! - Mail addressed to each domain lands under the correct per-domain
//!   inbox tree.
//! - RCPT TO for an unconfigured domain rejects with `550 5.7.1`.
//!
//! Tests share the cached DKIM keypair shape used by the main
//! integration suite — we install one keypair under each per-domain
//! DKIM dir so the daemon's startup loader populates both entries.

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

/// One-shot pre-generated DKIM keypair shared by every multi-domain
/// integration test. Avoids re-running `aimx dkim-keygen` (~200ms) per
/// test.
static MD_DKIM_CACHE: LazyLock<TempDir> = LazyLock::new(|| {
    let cache = TempDir::new().expect("create DKIM cache");
    let config = format!(
        "domain = \"md-cache.example.com\"\ndata_dir = \"{}\"\n\n[mailboxes.catchall]\naddress = \"*@md-cache.example.com\"\nowner = \"aimx-catchall\"\n",
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
    // `aimx dkim-keygen` (no `--domain`) writes the cache under
    // `<dkim_dir>/<default_domain>/` — the cache config sets the
    // default domain to `md-cache.example.com`.
    let cache_dkim = MD_DKIM_CACHE
        .path()
        .join("dkim")
        .join("md-cache.example.com");
    for name in ["private.key", "public.key"] {
        let src = cache_dkim.join(name);
        let dst = domain_dir.join(name);
        if src.exists() {
            std::fs::copy(&src, &dst).unwrap();
        }
    }
}

fn current_username() -> String {
    // Resolve the calling test's username so the mailbox `owner` matches
    // the running uid (the daemon's authz check is strict).
    unsafe {
        let uid = libc::geteuid();
        let pw = libc::getpwuid(uid);
        if pw.is_null() {
            return format!("uid{uid}");
        }
        let cstr = std::ffi::CStr::from_ptr((*pw).pw_name);
        cstr.to_string_lossy().to_string()
    }
}

/// Provision a canonical two-domain v2 install under `tmp` with
/// per-domain trust overrides. a.com sets `trust = "verified"` with a
/// catch-all trusted-senders pattern; b.com leaves both unset (so
/// effective trust falls back to global `"none"`). The same shared
/// DKIM cache + storage layout as `setup_two_domain_env` applies.
fn setup_two_domain_env_with_trust_overrides(tmp: &Path) {
    let owner = current_username();
    let cfg = format!(
        r#"domains = ["a.com", "b.com"]
data_dir = "{tmp_path}"
trust = "none"
trusted_senders = []

[domain."a.com"]
trust = "verified"
trusted_senders = ["*@example.com"]

[mailboxes."info@a.com"]
address = "info@a.com"
owner = "{owner}"

[mailboxes."support@b.com"]
address = "support@b.com"
owner = "{owner}"
"#,
        tmp_path = tmp.display(),
    );
    std::fs::write(tmp.join("config.toml"), cfg).unwrap();

    std::fs::write(tmp.join(".layout-version"), "2\n").unwrap();

    install_dkim_under(&tmp.join("dkim").join("a.com"));
    install_dkim_under(&tmp.join("dkim").join("b.com"));

    for (domain, local) in [("a.com", "info"), ("b.com", "support")] {
        for folder in ["inbox", "sent"] {
            let dir = tmp.join(domain).join(folder).join(local);
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
            std::fs::set_permissions(tmp.join(domain), std::fs::Permissions::from_mode(0o755))
                .unwrap();
        }
    }
}

/// Provision a canonical two-domain v2 install under `tmp`:
/// - `<tmp>/config.toml` carries `domains = ["a.com", "b.com"]` and one
///   FQDN-keyed mailbox per domain.
/// - `<tmp>/.layout-version` marker is pre-written so the daemon skips
///   the upgrade migration.
/// - `<tmp>/dkim/a.com/{private,public}.key` and
///   `<tmp>/dkim/b.com/{private,public}.key` are populated from the
///   shared cache.
/// - Per-mailbox inbox/sent dirs are created under each per-domain
///   storage root.
fn setup_two_domain_env(tmp: &Path) {
    let owner = current_username();
    let cfg = format!(
        r#"domains = ["a.com", "b.com"]
data_dir = "{tmp_path}"

[mailboxes."info@a.com"]
address = "info@a.com"
owner = "{owner}"

[mailboxes."support@b.com"]
address = "support@b.com"
owner = "{owner}"
"#,
        tmp_path = tmp.display(),
    );
    std::fs::write(tmp.join("config.toml"), cfg).unwrap();

    // Pre-write the v2 marker so the upgrade migration short-circuits.
    std::fs::write(tmp.join(".layout-version"), "2\n").unwrap();

    // Per-domain DKIM keys.
    install_dkim_under(&tmp.join("dkim").join("a.com"));
    install_dkim_under(&tmp.join("dkim").join("b.com"));

    // Per-domain storage trees with 0o700 inbox/sent dirs.
    for (domain, local) in [("a.com", "info"), ("b.com", "support")] {
        for folder in ["inbox", "sent"] {
            let dir = tmp.join(domain).join(folder).join(local);
            std::fs::create_dir_all(&dir).unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
            }
        }
        // Per-domain root must be 0o755 (the daemon's run_serve enforces
        // this on first start; pre-set it here so tests don't need to
        // wait).
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(tmp.join(domain), std::fs::Permissions::from_mode(0o755))
                .unwrap();
        }
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

fn smtp_send_email(port: u16, from: &str, rcpts: &[&str], data: &str) {
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

    for rcpt in rcpts {
        buf.clear();
        write!(writer, "RCPT TO:<{rcpt}>\r\n").unwrap();
        reader.read_line(&mut buf).unwrap();
        assert!(buf.starts_with("250"), "RCPT TO {rcpt}: {buf}");
    }

    buf.clear();
    write!(writer, "DATA\r\n").unwrap();
    reader.read_line(&mut buf).unwrap();
    assert!(buf.starts_with("354"), "DATA: {buf}");

    write!(writer, "{data}\r\n.\r\n").unwrap();
    buf.clear();
    reader.read_line(&mut buf).unwrap();
    assert!(buf.starts_with("250"), "DATA end: {buf}");

    let _ = write!(writer, "QUIT\r\n");
    let mut sink = String::new();
    let _ = reader.read_line(&mut sink);
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

#[test]
fn two_domain_smtp_intake_accepts_rcpt_for_each_domain() {
    let tmp = TempDir::new().unwrap();
    setup_two_domain_env(tmp.path());
    let port = find_free_port();
    let mut child = start_serve(tmp.path(), port);
    wait_for_listener(port);

    let resp_a = smtp_rcpt_status(port, "sender@example.com", "info@a.com");
    let resp_b = smtp_rcpt_status(port, "sender@example.com", "support@b.com");

    assert!(
        resp_a.starts_with("250"),
        "RCPT TO info@a.com must be accepted; got: {resp_a}"
    );
    assert!(
        resp_b.starts_with("250"),
        "RCPT TO support@b.com must be accepted; got: {resp_b}"
    );

    shutdown(&mut child);
}

#[test]
fn two_domain_smtp_intake_rejects_unconfigured_domain() {
    let tmp = TempDir::new().unwrap();
    setup_two_domain_env(tmp.path());
    let port = find_free_port();
    let mut child = start_serve(tmp.path(), port);
    wait_for_listener(port);

    let resp = smtp_rcpt_status(port, "sender@example.com", "alice@c.com");
    assert!(
        resp.starts_with("550"),
        "RCPT TO alice@c.com must be rejected; got: {resp}"
    );

    shutdown(&mut child);
}

#[test]
fn two_domain_smtp_intake_routes_to_per_domain_inbox() {
    let tmp = TempDir::new().unwrap();
    setup_two_domain_env(tmp.path());
    let port = find_free_port();
    let mut child = start_serve(tmp.path(), port);
    wait_for_listener(port);

    // Deliver one message to each domain.
    let email_a = "From: sender@example.com\r\nTo: info@a.com\r\nSubject: A-mail\r\nDate: Mon, 01 Jan 2024 00:00:00 +0000\r\nMessage-ID: <a-mail@example.com>\r\n\r\nHello A";
    let email_b = "From: sender@example.com\r\nTo: support@b.com\r\nSubject: B-mail\r\nDate: Mon, 01 Jan 2024 00:00:00 +0000\r\nMessage-ID: <b-mail@example.com>\r\n\r\nHello B";
    smtp_send_email(port, "sender@example.com", &["info@a.com"], email_a);
    smtp_send_email(port, "sender@example.com", &["support@b.com"], email_b);

    std::thread::sleep(Duration::from_millis(500));

    // a.com's inbox sees A-mail.
    let a_inbox = tmp.path().join("a.com").join("inbox").join("info");
    let a_entries: Vec<_> = std::fs::read_dir(&a_inbox)
        .expect("a.com inbox must exist")
        .filter_map(|e| e.ok())
        .collect();
    assert_eq!(
        a_entries.len(),
        1,
        "expected one email in a.com inbox; got {} under {}",
        a_entries.len(),
        a_inbox.display()
    );
    let a_content = std::fs::read_to_string(a_entries[0].path()).unwrap();
    assert!(
        a_content.contains("A-mail"),
        "a.com message must carry A-mail subject"
    );

    // b.com's inbox sees B-mail.
    let b_inbox = tmp.path().join("b.com").join("inbox").join("support");
    let b_entries: Vec<_> = std::fs::read_dir(&b_inbox)
        .expect("b.com inbox must exist")
        .filter_map(|e| e.ok())
        .collect();
    assert_eq!(
        b_entries.len(),
        1,
        "expected one email in b.com inbox; got {} under {}",
        b_entries.len(),
        b_inbox.display()
    );
    let b_content = std::fs::read_to_string(b_entries[0].path()).unwrap();
    assert!(
        b_content.contains("B-mail"),
        "b.com message must carry B-mail subject"
    );

    shutdown(&mut child);
}

/// Per-domain trust overrides land in the inbound frontmatter.
///
/// a.com has `[domain."a.com"] trust = "verified"` plus a matching
/// trusted-senders pattern; b.com inherits the global `trust = "none"`.
/// Inbound from `sender@example.com` to each domain produces:
/// - `trusted = "true"` on a.com (sender matches `*@example.com` and
///   the verified policy fires).
/// - `trusted = "none"` on b.com (global default policy `none` — no
///   evaluation).
#[test]
fn two_domain_per_domain_trust_overrides_land_in_frontmatter() {
    let tmp = TempDir::new().unwrap();
    setup_two_domain_env_with_trust_overrides(tmp.path());
    let port = find_free_port();
    let mut child = start_serve(tmp.path(), port);
    wait_for_listener(port);

    let email_a = "From: sender@example.com\r\nTo: info@a.com\r\nSubject: A-trust\r\nDate: Mon, 01 Jan 2024 00:00:00 +0000\r\nMessage-ID: <a-trust@example.com>\r\n\r\nHello A";
    let email_b = "From: sender@example.com\r\nTo: support@b.com\r\nSubject: B-trust\r\nDate: Mon, 01 Jan 2024 00:00:00 +0000\r\nMessage-ID: <b-trust@example.com>\r\n\r\nHello B";
    smtp_send_email(port, "sender@example.com", &["info@a.com"], email_a);
    smtp_send_email(port, "sender@example.com", &["support@b.com"], email_b);

    std::thread::sleep(Duration::from_millis(500));

    let a_inbox = tmp.path().join("a.com").join("inbox").join("info");
    let a_entries: Vec<_> = std::fs::read_dir(&a_inbox)
        .expect("a.com inbox must exist")
        .filter_map(|e| e.ok())
        .collect();
    assert_eq!(a_entries.len(), 1);
    let a_content = std::fs::read_to_string(a_entries[0].path()).unwrap();
    // a.com inherits per-domain `trust = "verified"` + matching senders.
    // DKIM will be "none" or "fail" (no DKIM signature on the inbound
    // stub email), so the result will be "false" rather than "true" --
    // the important assertion is that an evaluation HAPPENED (not
    // "none") because the per-domain trust policy fired.
    assert!(
        a_content.contains("trusted = \"true\"") || a_content.contains("trusted = \"false\""),
        "a.com per-domain trust = verified must trigger evaluation; got:\n{a_content}"
    );

    let b_inbox = tmp.path().join("b.com").join("inbox").join("support");
    let b_entries: Vec<_> = std::fs::read_dir(&b_inbox)
        .expect("b.com inbox must exist")
        .filter_map(|e| e.ok())
        .collect();
    assert_eq!(b_entries.len(), 1);
    let b_content = std::fs::read_to_string(b_entries[0].path()).unwrap();
    assert!(
        b_content.contains("trusted = \"none\""),
        "b.com falls through to global trust = none; got:\n{b_content}"
    );

    shutdown(&mut child);
}
