use assert_cmd::Command;
use predicates::prelude::*;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Command as StdCommand, Stdio};
use std::sync::LazyLock;
use tempfile::TempDir;
use wait_timeout::ChildExt;

/// Process-scoped cache of a pre-generated 2048-bit RSA DKIM keypair.
/// Shared read-only across all integration tests to avoid re-running
/// `aimx dkim-keygen` (~200ms each) for every test that spawns `aimx serve`.
static DKIM_CACHE: LazyLock<TempDir> = LazyLock::new(|| {
    let cache = TempDir::new().expect("create DKIM cache tempdir");
    // dkim-keygen needs a parseable config.toml (it loads Config at startup).
    let config_content = format!(
        "domain = \"cache.example.com\"\ndata_dir = \"{}\"\n\n[mailboxes.catchall]\naddress = \"*@cache.example.com\"\nowner = \"aimx-catchall\"\n",
        cache.path().display()
    );
    std::fs::write(cache.path().join("config.toml"), config_content)
        .expect("write cache config.toml");
    let status = StdCommand::new(aimx_binary_path())
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

/// Copy the cached DKIM keypair into `tmp/dkim/`.
fn install_cached_dkim_keys(tmp: &Path) {
    let dkim_dir = tmp.join("dkim");
    if dkim_dir.join("private.key").exists() {
        return;
    }
    std::fs::create_dir_all(&dkim_dir).unwrap();
    let cache_dkim = DKIM_CACHE.path().join("dkim");
    for name in ["private.key", "public.key"] {
        let src = cache_dkim.join(name);
        let dst = dkim_dir.join(name);
        if src.exists() {
            std::fs::copy(&src, &dst).unwrap();
        }
    }
}

fn setup_test_env(tmp: &Path) -> String {
    // The UDS enforces per-mailbox ownership. Tests that drive MCP /
    // UDS as the current user need alice's owner to match the running
    // uid so the authz check accepts.
    //
    // `aimx doctor` validates mailbox storage ownership. The fixture
    // uses the current test-runner's username
    // for BOTH mailboxes (including the catchall) and creates all four
    // storage dirs so `MAILBOX-DIR-OWNER-DRIFT` / `MAILBOX-DIR-MISSING`
    // do not fire for fixture reasons. Tests that specifically need a
    // catchall owned by `aimx-catchall` must override the config
    // themselves.
    let owner = current_username();
    let config_content = format!(
        "domain = \"agent.example.com\"\ndata_dir = \"{}\"\n\n[mailboxes.catchall]\naddress = \"*@agent.example.com\"\nowner = \"{owner}\"\n\n[mailboxes.alice]\naddress = \"alice@agent.example.com\"\nowner = \"{owner}\"\n",
        tmp.display()
    );
    for sub in [
        "inbox/catchall",
        "sent/catchall",
        "inbox/alice",
        "sent/alice",
    ] {
        let dir = tmp.join(sub);
        std::fs::create_dir_all(&dir).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
    }
    let config_path = tmp.join("config.toml");
    std::fs::write(&config_path, &config_content).unwrap();
    install_cached_dkim_keys(tmp);
    config_path.to_string_lossy().to_string()
}

/// Build an `aimx` Command pre-wired with `AIMX_CONFIG_DIR` pointed at the
/// test's tempdir. Config and storage live in different roots, so
/// integration tests must override both the storage path (`--data-dir`
/// / `AIMX_DATA_DIR`) and the config lookup via this env var.
fn aimx_cmd(tmp: &Path) -> Command {
    let mut cmd = Command::cargo_bin("aimx").unwrap();
    cmd.env("AIMX_CONFIG_DIR", tmp);
    // Integration tests fire hooks via the sandboxed executor. On a
    // systemd host the default path shells out to `systemd-run`, which
    // refuses interactive auth for non-root users and makes every
    // hook-firing test fail. Force the fallback path so tests exercise
    // the same observable surface (exit code, stderr capture, env vars).
    cmd.env("AIMX_SANDBOX_FORCE_FALLBACK", "1");
    cmd
}

fn read_frontmatter(md_path: &Path) -> toml::Value {
    let content = std::fs::read_to_string(md_path).unwrap();
    let parts: Vec<&str> = content.splitn(3, "+++").collect();
    assert!(
        parts.len() >= 3,
        "Markdown file missing frontmatter delimiters"
    );
    toml::from_str(parts[1].trim()).unwrap()
}

fn get_toml_str<'a>(table: &'a toml::Table, key: &str) -> &'a str {
    table.get(key).and_then(|v| v.as_str()).unwrap_or("")
}

fn find_md_files(dir: &Path) -> Vec<std::path::PathBuf> {
    let mut out: Vec<std::path::PathBuf> = Vec::new();
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return out,
    };
    for entry in entries.filter_map(|e| e.ok()) {
        let path = entry.path();
        if path.is_dir() {
            // Bundle directory: `<stem>/<stem>.md`.
            if let Some(stem) = path.file_name().and_then(|f| f.to_str()) {
                let md = path.join(format!("{stem}.md"));
                if md.exists() {
                    out.push(md);
                }
            }
        } else if path.extension().is_some_and(|ext| ext == "md") {
            out.push(path);
        }
    }
    out.sort();
    out
}

/// Resolve the inbox directory for a mailbox under a test tempdir.
fn inbox(tmp: &Path, name: &str) -> std::path::PathBuf {
    tmp.join("inbox").join(name)
}

/// Search every bundle directory under `mailbox_dir` for an attachment
/// named `name`. Returns the first match; tests only create one email
/// with attachments per setup.
fn find_attachment(mailbox_dir: &Path, name: &str) -> Option<std::path::PathBuf> {
    let entries = std::fs::read_dir(mailbox_dir).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            let candidate = path.join(name);
            if candidate.exists() {
                return Some(candidate);
            }
        }
    }
    None
}

#[test]
fn help_shows_subcommands() {
    Command::cargo_bin("aimx")
        .unwrap()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("Operations (as current user)"))
        .stdout(predicate::str::contains("Server administration"))
        .stdout(predicate::str::contains("send"))
        .stdout(predicate::str::contains("mailboxes"))
        .stdout(predicate::str::contains("mcp"))
        .stdout(predicate::str::contains("setup"))
        .stdout(predicate::str::contains("doctor"))
        .stdout(predicate::str::contains("serve"))
        .stdout(predicate::str::contains("portcheck"))
        .stdout(predicate::str::contains("dkim-keygen"))
        // `ingest` is wired (called by `aimx serve` over stdin) but hidden
        // from top-level --help so the user-facing command list stays clean.
        .stdout(predicate::str::contains("ingest").not());
}

#[test]
fn help_shows_data_dir_option() {
    Command::cargo_bin("aimx")
        .unwrap()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("--data-dir"))
        .stdout(predicate::str::contains("AIMX_DATA_DIR"));
}

#[test]
fn ingest_plain_fixture_full_pipeline() {
    let tmp = TempDir::new().unwrap();
    setup_test_env(tmp.path());
    let eml = std::fs::read("tests/fixtures/plain.eml").unwrap();

    aimx_cmd(tmp.path())
        .arg("--data-dir")
        .arg(tmp.path())
        .arg("ingest")
        .arg("catchall@agent.example.com")
        .write_stdin(eml)
        .assert()
        .success();

    let md_files = find_md_files(&inbox(tmp.path(), "catchall"));
    assert_eq!(md_files.len(), 1);

    let parsed = read_frontmatter(&md_files[0]);
    let table = parsed.as_table().unwrap();
    assert_eq!(get_toml_str(table, "from"), "Alice <alice@example.com>");
    assert_eq!(get_toml_str(table, "subject"), "Plain text test");
    assert_eq!(get_toml_str(table, "message_id"), "plain-001@example.com");
    assert_eq!(get_toml_str(table, "mailbox"), "catchall");
    assert_eq!(table.get("read").unwrap(), &toml::Value::Boolean(false));

    let content = std::fs::read_to_string(&md_files[0]).unwrap();
    assert!(content.contains("This is a plain text email for testing."));
}

#[test]
fn ingest_emits_tracing_logs_on_stderr() {
    // Smoke test for the logging refactor: the stdin `aimx ingest` path
    // now installs a tracing subscriber on entry, so stderr must carry
    // the `aimx::ingest` / `aimx::trust` structured records.
    let tmp = TempDir::new().unwrap();
    setup_test_env(tmp.path());
    let eml = std::fs::read("tests/fixtures/plain.eml").unwrap();

    aimx_cmd(tmp.path())
        .arg("--data-dir")
        .arg(tmp.path())
        .arg("ingest")
        .arg("catchall@agent.example.com")
        .write_stdin(eml)
        .assert()
        .success()
        .stderr(predicate::str::contains("aimx::ingest"))
        .stderr(predicate::str::contains("received email"))
        .stderr(predicate::str::contains("stored email"))
        .stderr(predicate::str::contains("aimx::trust"));
}

#[test]
fn ingest_html_fixture_full_pipeline() {
    let tmp = TempDir::new().unwrap();
    setup_test_env(tmp.path());
    let eml = std::fs::read("tests/fixtures/html_only.eml").unwrap();

    aimx_cmd(tmp.path())
        .arg("--data-dir")
        .arg(tmp.path())
        .arg("ingest")
        .arg("catchall@agent.example.com")
        .write_stdin(eml)
        .assert()
        .success();

    let md_files = find_md_files(&inbox(tmp.path(), "catchall"));
    assert_eq!(md_files.len(), 1);

    let parsed = read_frontmatter(&md_files[0]);
    let table = parsed.as_table().unwrap();
    assert_eq!(get_toml_str(table, "subject"), "HTML only test");

    let content = std::fs::read_to_string(&md_files[0]).unwrap();
    assert!(content.contains("Hello from HTML"));
    assert!(!content.contains("<html>"));
}

#[test]
fn ingest_multipart_fixture_full_pipeline() {
    let tmp = TempDir::new().unwrap();
    setup_test_env(tmp.path());
    let eml = std::fs::read("tests/fixtures/multipart.eml").unwrap();

    aimx_cmd(tmp.path())
        .arg("--data-dir")
        .arg(tmp.path())
        .arg("ingest")
        .arg("catchall@agent.example.com")
        .write_stdin(eml)
        .assert()
        .success();

    let md_files = find_md_files(&inbox(tmp.path(), "catchall"));
    assert_eq!(md_files.len(), 1);

    let content = std::fs::read_to_string(&md_files[0]).unwrap();
    assert!(content.contains("This is the plain text version."));
    assert!(!content.contains("<html>"));
}

#[test]
fn ingest_attachment_fixture_full_pipeline() {
    let tmp = TempDir::new().unwrap();
    setup_test_env(tmp.path());
    let eml = std::fs::read("tests/fixtures/with_attachment.eml").unwrap();

    aimx_cmd(tmp.path())
        .arg("--data-dir")
        .arg(tmp.path())
        .arg("ingest")
        .arg("catchall@agent.example.com")
        .write_stdin(eml)
        .assert()
        .success();

    let md_files = find_md_files(&inbox(tmp.path(), "catchall"));
    assert_eq!(md_files.len(), 1);

    let att_path = find_attachment(&inbox(tmp.path(), "catchall"), "readme.txt")
        .expect("readme.txt attachment missing from bundle");
    assert!(att_path.exists());
    let att_content = std::fs::read_to_string(&att_path).unwrap();
    assert!(att_content.contains("This is the content of the attached file."));

    let parsed = read_frontmatter(&md_files[0]);
    let table = parsed.as_table().unwrap();
    let attachments = table.get("attachments").unwrap().as_array().unwrap();
    assert_eq!(attachments.len(), 1);
    let att = attachments[0].as_table().unwrap();
    assert_eq!(att.get("filename").unwrap().as_str().unwrap(), "readme.txt");
    // Bundle-relative path: attachment sits beside the `.md` with no
    // `attachments/` prefix.
    assert_eq!(att.get("path").unwrap().as_str().unwrap(), "readme.txt");
}

#[test]
fn ingest_routes_to_named_mailbox() {
    let tmp = TempDir::new().unwrap();
    setup_test_env(tmp.path());
    let eml = std::fs::read("tests/fixtures/plain.eml").unwrap();

    aimx_cmd(tmp.path())
        .arg("--data-dir")
        .arg(tmp.path())
        .arg("ingest")
        .arg("alice@agent.example.com")
        .write_stdin(eml)
        .assert()
        .success();

    let alice_files = find_md_files(&inbox(tmp.path(), "alice"));
    assert_eq!(alice_files.len(), 1);

    let catchall_files = find_md_files(&inbox(tmp.path(), "catchall"));
    assert_eq!(catchall_files.len(), 0);
}

#[test]
fn ingest_unknown_routes_to_catchall() {
    let tmp = TempDir::new().unwrap();
    setup_test_env(tmp.path());
    let eml = std::fs::read("tests/fixtures/plain.eml").unwrap();

    aimx_cmd(tmp.path())
        .arg("--data-dir")
        .arg(tmp.path())
        .arg("ingest")
        .arg("unknown@agent.example.com")
        .write_stdin(eml)
        .assert()
        .success();

    let catchall_files = find_md_files(&inbox(tmp.path(), "catchall"));
    assert_eq!(catchall_files.len(), 1);
}

#[test]
fn ingest_via_env_var() {
    let tmp = TempDir::new().unwrap();
    setup_test_env(tmp.path());
    let eml = std::fs::read("tests/fixtures/plain.eml").unwrap();

    aimx_cmd(tmp.path())
        .env("AIMX_DATA_DIR", tmp.path())
        .arg("ingest")
        .arg("catchall@agent.example.com")
        .write_stdin(eml)
        .assert()
        .success();

    let md_files = find_md_files(&inbox(tmp.path(), "catchall"));
    assert_eq!(md_files.len(), 1);
}

#[test]
fn dkim_keygen_end_to_end() {
    let tmp = TempDir::new().unwrap();
    // Write config.toml but skip DKIM key generation. The `dkim-keygen`
    // command itself is under test and must start from a clean slate.
    let config_content = format!(
        "domain = \"agent.example.com\"\ndata_dir = \"{}\"\n\n[mailboxes.catchall]\naddress = \"*@agent.example.com\"\nowner = \"aimx-catchall\"\n",
        tmp.path().display()
    );
    std::fs::write(tmp.path().join("config.toml"), &config_content).unwrap();

    aimx_cmd(tmp.path())
        .arg("--data-dir")
        .arg(tmp.path())
        .arg("dkim-keygen")
        .assert()
        .success()
        .stdout(predicate::str::contains("DKIM keypair generated"))
        .stdout(predicate::str::contains("_domainkey"));

    assert!(tmp.path().join("dkim/private.key").exists());
    assert!(tmp.path().join("dkim/public.key").exists());

    let private_pem = std::fs::read_to_string(tmp.path().join("dkim/private.key")).unwrap();
    assert!(private_pem.contains("BEGIN RSA PRIVATE KEY"));

    let public_pem = std::fs::read_to_string(tmp.path().join("dkim/public.key")).unwrap();
    assert!(public_pem.contains("BEGIN PUBLIC KEY"));
}

#[test]
fn dkim_keygen_no_overwrite() {
    let tmp = TempDir::new().unwrap();
    let config_content = format!(
        "domain = \"agent.example.com\"\ndata_dir = \"{}\"\n\n[mailboxes.catchall]\naddress = \"*@agent.example.com\"\nowner = \"aimx-catchall\"\n",
        tmp.path().display()
    );
    std::fs::write(tmp.path().join("config.toml"), &config_content).unwrap();

    aimx_cmd(tmp.path())
        .arg("--data-dir")
        .arg(tmp.path())
        .arg("dkim-keygen")
        .assert()
        .success();

    aimx_cmd(tmp.path())
        .arg("--data-dir")
        .arg(tmp.path())
        .arg("dkim-keygen")
        .assert()
        .success()
        .stderr(predicate::str::contains("already exist"))
        .stderr(predicate::str::contains("Warning:"));
}

#[test]
fn dkim_keygen_force_overwrite() {
    let tmp = TempDir::new().unwrap();
    let config_content = format!(
        "domain = \"agent.example.com\"\ndata_dir = \"{}\"\n\n[mailboxes.catchall]\naddress = \"*@agent.example.com\"\nowner = \"aimx-catchall\"\n",
        tmp.path().display()
    );
    std::fs::write(tmp.path().join("config.toml"), &config_content).unwrap();

    aimx_cmd(tmp.path())
        .arg("--data-dir")
        .arg(tmp.path())
        .arg("dkim-keygen")
        .assert()
        .success();

    let original = std::fs::read_to_string(tmp.path().join("dkim/private.key")).unwrap();

    aimx_cmd(tmp.path())
        .arg("--data-dir")
        .arg(tmp.path())
        .arg("dkim-keygen")
        .arg("--force")
        .assert()
        .success();

    let new = std::fs::read_to_string(tmp.path().join("dkim/private.key")).unwrap();
    assert_ne!(original, new);
}

#[cfg(unix)]
#[test]
fn dkim_keygen_permission_denied_error_mentions_path_and_override() {
    use std::os::unix::fs::PermissionsExt;

    // Skip when running as root; chmod 0o500 is bypassed by CAP_DAC_OVERRIDE.
    if unsafe { libc::geteuid() } == 0 {
        eprintln!("skipping: test must run as non-root");
        return;
    }

    // `AIMX_CONFIG_DIR` points at a read-only directory. `aimx dkim-keygen`
    // then tries to create `<ro>/dkim/` and hits PermissionDenied. The error
    // message must name the target path and suggest `sudo` or `AIMX_CONFIG_DIR`.
    let tmp = TempDir::new().unwrap();
    let config_dir = tmp.path().join("ro-config");
    std::fs::create_dir_all(&config_dir).unwrap();
    let config_content = format!(
        "domain = \"agent.example.com\"\ndata_dir = \"{}\"\n\n[mailboxes.catchall]\naddress = \"*@agent.example.com\"\nowner = \"aimx-catchall\"\n",
        tmp.path().display()
    );
    std::fs::write(config_dir.join("config.toml"), &config_content).unwrap();
    // Remove write permission so `create_dir_all(<ro-config>/dkim)` inside
    // `generate_keypair` fails with PermissionDenied.
    std::fs::set_permissions(&config_dir, std::fs::Permissions::from_mode(0o500)).unwrap();

    let output = Command::cargo_bin("aimx")
        .unwrap()
        .env("AIMX_CONFIG_DIR", &config_dir)
        .arg("dkim-keygen")
        .output()
        .expect("spawn aimx");

    // Restore permissions for TempDir cleanup.
    let _ = std::fs::set_permissions(&config_dir, std::fs::Permissions::from_mode(0o755));

    assert!(
        !output.status.success(),
        "dkim-keygen must fail on read-only config dir"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    let combined = format!("{stderr}{}", String::from_utf8_lossy(&output.stdout));
    let dkim_path = config_dir.join("dkim");
    assert!(
        combined.contains(&dkim_path.display().to_string())
            || combined.contains(&config_dir.display().to_string()),
        "error must mention the attempted path; got: {combined}"
    );
    assert!(
        combined.contains("sudo") || combined.contains("AIMX_CONFIG_DIR"),
        "error must suggest sudo or AIMX_CONFIG_DIR; got: {combined}"
    );
}

fn aimx_binary_path() -> std::path::PathBuf {
    assert_cmd::cargo::cargo_bin("aimx")
}

/// `MAILBOX-CREATE` / `MAILBOX-DELETE` over UDS are root-only.
/// Tests that exercise the CLI via UDS need to bail out
/// when not running as root so a casual `cargo test` doesn't surface
/// EACCES as a pretend bug. The CI `integration-isolation` job sets
/// `AIMX_INTEGRATION_SUDO=1` and runs under sudo, so these tests
/// execute there; non-root local runs skip them with a single stderr
/// line so the cause is obvious.
fn skip_if_mailbox_crud_not_root() -> bool {
    if unsafe { libc::geteuid() } == 0 {
        return false;
    }
    eprintln!("skipping mailbox-CRUD UDS test: requires root; MAILBOX-CRUD is root-only");
    true
}

/// Resolve a non-root Linux username suitable for use as a test mailbox
/// `owner`. Used by tests that create mailboxes in a tmpdir:
/// the daemon chowns the new mailbox dirs to the configured owner's
/// uid, so on a non-root CI runner the owner must be the tester's own
/// username (chown-to-self is a zero-effect syscall every user can
/// issue).
///
/// `owner = "root"` is forbidden on non-catchall mailboxes, so when
/// the test process runs as root (root-gated CI step under `sudo`)
/// we must pick a non-root username. Prefer `SUDO_USER`
/// — the invoking non-root user — when it points at a real passwd
/// entry (the normal CI path, where `sudo` preserves the env). Fall
/// back to `nobody` otherwise (always present on Linux, matches the
/// owner regex, and resolves via `getpwnam` → no hard reject; an
/// `OrphanMailboxOwner` warning at worst). Under authz the test CLI
/// runs as root and takes the `RootBypass` path, so the owner uid
/// doesn't need to match the caller.
fn current_username() -> String {
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

struct McpClient {
    child: std::process::Child,
    stdin: std::process::ChildStdin,
    reader: BufReader<std::process::ChildStdout>,
    id: i64,
    /// Background-drained stderr from the MCP subprocess. The drain
    /// thread reads to EOF into a shared buffer so a child that prints
    /// a startup error before dying still has its diagnostic captured.
    /// Surfaced by `send_request` whenever a JSON-RPC read returns EOF
    /// (the original failure mode silently ate the child's stderr).
    stderr_buf: std::sync::Arc<std::sync::Mutex<Vec<u8>>>,
    stderr_drain: Option<std::thread::JoinHandle<()>>,
}

#[allow(dead_code)]
fn spawn_stderr_drain(
    mut stderr: std::process::ChildStderr,
) -> (
    std::sync::Arc<std::sync::Mutex<Vec<u8>>>,
    std::thread::JoinHandle<()>,
) {
    let buf = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let buf_w = std::sync::Arc::clone(&buf);
    let handle = std::thread::spawn(move || {
        use std::io::Read as _;
        let mut chunk = [0u8; 4096];
        loop {
            match stderr.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if let Ok(mut g) = buf_w.lock() {
                        g.extend_from_slice(&chunk[..n]);
                    }
                }
            }
        }
    });
    (buf, handle)
}

impl McpClient {
    fn spawn(data_dir: &Path) -> Self {
        // MCP mailbox create/delete try the daemon's UDS socket first.
        // Tests that don't spawn their own daemon must point
        // AIMX_RUNTIME_DIR at an empty tempdir so the socket isn't found
        // (otherwise the test would speak to whatever production daemon
        // happens to be running on the CI/dev host).
        let runtime = data_dir.join("run");
        std::fs::create_dir_all(&runtime).ok();
        let mut child = StdCommand::new(aimx_binary_path())
            .env("AIMX_CONFIG_DIR", data_dir)
            .env("AIMX_RUNTIME_DIR", &runtime)
            .arg("--data-dir")
            .arg(data_dir)
            .arg("mcp")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("Failed to spawn aimx mcp");

        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let stderr = child.stderr.take().unwrap();
        let reader = BufReader::new(stdout);
        let (stderr_buf, stderr_drain) = spawn_stderr_drain(stderr);

        Self {
            child,
            stdin,
            reader,
            id: 0,
            stderr_buf,
            stderr_drain: Some(stderr_drain),
        }
    }

    fn send_request(&mut self, method: &str, params: serde_json::Value) -> serde_json::Value {
        self.id += 1;
        let request = serde_json::json!({
            "jsonrpc": "2.0",
            "id": self.id,
            "method": method,
            "params": params
        });

        let msg = serde_json::to_string(&request).unwrap();
        writeln!(self.stdin, "{msg}").unwrap();
        self.stdin.flush().unwrap();

        let mut line = String::new();
        let n = self.reader.read_line(&mut line).unwrap_or_else(|e| {
            panic!(
                "MCP read failed for method {method:?}: {e}{}",
                self.format_dead_child_diag()
            )
        });
        if n == 0 {
            // EOF — the MCP subprocess closed stdout. Almost always
            // means it died at startup or while parsing the request.
            // Surface the captured stderr + child status so future CI
            // failures land with a real diagnostic instead of the
            // opaque `Error("EOF while parsing a value", ...)` panic
            // that this helper used to produce.
            panic!(
                "MCP subprocess closed stdout (EOF) before responding to {method:?}{}",
                self.format_dead_child_diag()
            );
        }
        serde_json::from_str(line.trim()).unwrap_or_else(|e| {
            panic!(
                "MCP returned non-JSON for {method:?}: {e}; raw line {:?}{}",
                line,
                self.format_dead_child_diag()
            )
        })
    }

    /// Wait briefly for the MCP child to exit (so its stderr drain
    /// thread reaches EOF), then format a diagnostic block carrying the
    /// captured stderr + exit status. Used by `send_request` when the
    /// child dies mid-handshake.
    fn format_dead_child_diag(&mut self) -> String {
        // Give the child up to 1s to exit so the drain thread captures
        // any final stderr lines. We don't kill it — many failure
        // modes already exited; for the rare hung case the test
        // harness's outer timeout still reaps.
        let started = std::time::Instant::now();
        let mut status = None;
        while started.elapsed() < std::time::Duration::from_secs(1) {
            match self.child.try_wait() {
                Ok(Some(s)) => {
                    status = Some(s);
                    break;
                }
                Ok(None) => std::thread::sleep(std::time::Duration::from_millis(50)),
                Err(_) => break,
            }
        }
        if let Some(h) = self.stderr_drain.take() {
            let _ = h.join();
        }
        let stderr = self
            .stderr_buf
            .lock()
            .map(|g| String::from_utf8_lossy(&g).into_owned())
            .unwrap_or_default();
        let status_str = match status {
            Some(s) => format!("{s}"),
            None => "still running".to_string(),
        };
        format!(
            "\n  child status: {status_str}\n  child stderr:\n----\n{}\n----",
            stderr.trim_end()
        )
    }

    fn send_notification(&mut self, method: &str, params: serde_json::Value) {
        let notification = serde_json::json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params
        });

        let msg = serde_json::to_string(&notification).unwrap();
        writeln!(self.stdin, "{msg}").unwrap();
        self.stdin.flush().unwrap();
    }

    fn initialize(&mut self) -> serde_json::Value {
        let resp = self.send_request(
            "initialize",
            serde_json::json!({
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": {
                    "name": "test-client",
                    "version": "0.1.0"
                }
            }),
        );

        self.send_notification("notifications/initialized", serde_json::json!({}));

        resp
    }

    fn call_tool(&mut self, name: &str, arguments: serde_json::Value) -> serde_json::Value {
        self.send_request(
            "tools/call",
            serde_json::json!({
                "name": name,
                "arguments": arguments
            }),
        )
    }

    fn list_tools(&mut self) -> serde_json::Value {
        self.send_request("tools/list", serde_json::json!({}))
    }

    fn shutdown(mut self) {
        drop(self.stdin);
        let _ = self.child.wait();
        if let Some(h) = self.stderr_drain.take() {
            let _ = h.join();
        }
    }
}

fn get_tool_text(response: &serde_json::Value) -> String {
    response["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or("")
        .to_string()
}

fn is_tool_error(response: &serde_json::Value) -> bool {
    response["result"]["isError"].as_bool().unwrap_or(false)
}

#[test]
fn mcp_initialize_handshake() {
    let tmp = TempDir::new().unwrap();
    setup_test_env(tmp.path());

    let mut client = McpClient::spawn(tmp.path());
    let resp = client.initialize();

    assert!(resp["result"]["serverInfo"].is_object());
    assert!(resp["result"]["capabilities"]["tools"].is_object());

    client.shutdown();
}

#[test]
fn mcp_list_tools() {
    let tmp = TempDir::new().unwrap();
    setup_test_env(tmp.path());

    let mut client = McpClient::spawn(tmp.path());
    client.initialize();

    let resp = client.list_tools();
    let tools = resp["result"]["tools"].as_array().unwrap();
    // S2-3 / S2-4 reintroduced `mailbox_create` and `mailbox_delete` as
    // owner-gated MCP tools (the daemon synthesizes the owner from
    // SO_PEERCRED, so an agent can only operate on mailboxes owned by
    // its own uid). Surface: 7 mail tools + 3 hook tools + 2 mailbox
    // CRUD tools + `mailbox_list` = 12.
    assert_eq!(tools.len(), 12);

    let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert!(names.contains(&"mailbox_list"));
    assert!(names.contains(&"mailbox_create"));
    assert!(names.contains(&"mailbox_delete"));
    assert!(names.contains(&"email_list"));
    assert!(names.contains(&"email_read"));
    assert!(names.contains(&"email_mark_read"));
    assert!(names.contains(&"email_mark_unread"));
    assert!(names.contains(&"email_send"));
    assert!(names.contains(&"email_reply"));
    assert!(names.contains(&"hook_create"));
    assert!(names.contains(&"hook_list"));
    assert!(names.contains(&"hook_delete"));
    // hook_list_templates was deleted alongside hook templates.
    assert!(!names.contains(&"hook_list_templates"));

    client.shutdown();
}

#[cfg(unix)]
#[test]
fn mcp_mailbox_list_returns_caller_owned() {
    // `mailbox_list` is now a thin UDS client: it ships
    // `AIMX/1 MAILBOX-LIST`, the daemon resolves the caller via
    // `SO_PEERCRED`, and the response is a JSON array of mailboxes
    // the caller owns. The shared fixture sets every mailbox owner
    // to the test runner's username so a non-root `cargo test`
    // sees both alice and catchall through the daemon.
    let tmp = TempDir::new().unwrap();
    setup_test_env(tmp.path());

    let port = find_free_port();
    let daemon = start_serve(tmp.path(), port);
    let sock = tmp.path().join("run").join("aimx.sock");
    assert!(
        wait_for_socket(&sock, std::time::Duration::from_secs(5)),
        "UDS send socket never appeared"
    );

    let runtime = tmp.path().join("run");
    let mut child = StdCommand::new(aimx_binary_path())
        .env("AIMX_CONFIG_DIR", tmp.path())
        .env("AIMX_RUNTIME_DIR", &runtime)
        .arg("--data-dir")
        .arg(tmp.path())
        .arg("mcp")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("Failed to spawn aimx mcp");
    let stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    let (stderr_buf, stderr_drain) = spawn_stderr_drain(stderr);
    let mut client = McpClient {
        child,
        stdin,
        reader: BufReader::new(stdout),
        id: 0,
        stderr_buf,
        stderr_drain: Some(stderr_drain),
    };
    client.initialize();

    let resp = client.call_tool("mailbox_list", serde_json::json!({}));
    let text = get_tool_text(&resp);
    let rows: serde_json::Value =
        serde_json::from_str(&text).unwrap_or_else(|e| panic!("response not JSON: {text}: {e}"));
    let arr = rows.as_array().expect("expected JSON array");
    let names: Vec<&str> = arr
        .iter()
        .filter_map(|row| row.get("name").and_then(|v| v.as_str()))
        .collect();
    assert!(names.contains(&"alice"), "expected alice in {names:?}");
    assert!(
        names.contains(&"catchall"),
        "expected catchall in {names:?}"
    );

    let alice = arr
        .iter()
        .find(|row| row.get("name").and_then(|v| v.as_str()) == Some("alice"))
        .unwrap();
    assert!(
        alice
            .get("inbox_path")
            .and_then(|v| v.as_str())
            .is_some_and(|p| p.ends_with("/inbox/alice")),
        "alice row missing inbox_path: {alice}"
    );
    assert!(
        alice
            .get("sent_path")
            .and_then(|v| v.as_str())
            .is_some_and(|p| p.ends_with("/sent/alice")),
        "alice row missing sent_path: {alice}"
    );
    assert_eq!(
        alice.get("registered"),
        Some(&serde_json::Value::Bool(true))
    );

    client.shutdown();
    stop_serve(daemon);
}

/// Without a running daemon, `mailbox_list` surfaces the canonical
/// "aimx daemon not running" message that `email_send` and
/// `hook_create` already use.
#[cfg(unix)]
#[test]
fn mcp_mailbox_list_without_daemon_reports_missing_socket() {
    let tmp = TempDir::new().unwrap();
    setup_test_env(tmp.path());

    let runtime = tmp.path().join("run");
    std::fs::create_dir_all(&runtime).ok();

    let mut child = StdCommand::new(aimx_binary_path())
        .env("AIMX_CONFIG_DIR", tmp.path())
        .env("AIMX_RUNTIME_DIR", &runtime)
        .arg("--data-dir")
        .arg(tmp.path())
        .arg("mcp")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("Failed to spawn aimx mcp");
    let stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    let (stderr_buf, stderr_drain) = spawn_stderr_drain(stderr);
    let mut client = McpClient {
        child,
        stdin,
        reader: BufReader::new(stdout),
        id: 0,
        stderr_buf,
        stderr_drain: Some(stderr_drain),
    };
    client.initialize();

    let resp = client.call_tool("mailbox_list", serde_json::json!({}));
    let text = get_tool_text(&resp);
    assert!(
        text.contains("aimx daemon not running"),
        "expected canonical missing-daemon message, got: {text}"
    );

    client.shutdown();
}

#[test]
fn mcp_mailbox_create_tool_is_exposed() {
    // S2-3 re-added `mailbox_create` to the MCP surface. The tool
    // is now declared and dispatchable; with no daemon listening it
    // surfaces a "daemon not running" tool error rather than a
    // framework-level "unknown tool" rejection. This test guards
    // against the tool silently disappearing again — the previous
    // shape (`tool_no_longer_exposed`) would have continued to pass
    // even after re-add because both paths return `isError`.
    let tmp = TempDir::new().unwrap();
    setup_test_env(tmp.path());

    let mut client = McpClient::spawn(tmp.path());
    client.initialize();

    let resp = client.call_tool("mailbox_create", serde_json::json!({"name": "support"}));
    let tool_text = resp["result"]["content"][0]["text"].as_str().unwrap_or("");
    assert!(
        tool_text.contains("daemon not running") || tool_text.contains("daemon"),
        "expected daemon-not-running text in tool error, got: {resp}"
    );
    // Framework-level "unknown tool" would land in `error.code` or
    // `error.message`; assert it didn't take that path.
    assert!(
        resp.get("error").is_none(),
        "framework error means the tool is missing from the surface: {resp}"
    );

    client.shutdown();
}

#[test]
fn mcp_mailbox_delete_tool_is_exposed() {
    let tmp = TempDir::new().unwrap();
    setup_test_env(tmp.path());

    let mut client = McpClient::spawn(tmp.path());
    client.initialize();

    let resp = client.call_tool("mailbox_delete", serde_json::json!({"name": "support"}));
    let tool_text = resp["result"]["content"][0]["text"].as_str().unwrap_or("");
    assert!(
        tool_text.contains("daemon not running") || tool_text.contains("daemon"),
        "expected daemon-not-running text in tool error, got: {resp}"
    );
    assert!(
        resp.get("error").is_none(),
        "framework error means the tool is missing from the surface: {resp}"
    );

    client.shutdown();
}

fn create_email_file(dir: &Path, id: &str, from: &str, subject: &str, read: bool) {
    std::fs::create_dir_all(dir).unwrap();
    let content = format!(
        "+++\nid = \"{id}\"\nmessage_id = \"<{id}@test.com>\"\nfrom = \"{from}\"\nto = \"alice@test.com\"\nsubject = \"{subject}\"\ndate = \"2025-06-01T12:00:00Z\"\nin_reply_to = \"\"\nreferences = \"\"\nattachments = []\nmailbox = \"alice\"\nread = {read}\ndkim = \"none\"\nspf = \"none\"\n+++\n\nBody of {id}.\n"
    );
    std::fs::write(dir.join(format!("{id}.md")), content).unwrap();
}

#[test]
fn mcp_email_list_and_read() {
    let tmp = TempDir::new().unwrap();
    setup_test_env(tmp.path());

    let alice_dir = inbox(tmp.path(), "alice");
    create_email_file(
        &alice_dir,
        "2025-06-01-001",
        "sender@example.com",
        "Hello",
        false,
    );
    create_email_file(
        &alice_dir,
        "2025-06-01-002",
        "other@example.com",
        "World",
        true,
    );

    // The MCP `email_*` tools route through the daemon's `MAILBOX-LIST`
    // for path resolution so the non-root MCP process never reads
    // root-owned `/etc/aimx/config.toml`. Tests must spawn a daemon
    // even for read-only tools.
    let port = find_free_port();
    let daemon = start_serve(tmp.path(), port);
    let sock = tmp.path().join("run").join("aimx.sock");
    assert!(
        wait_for_socket(&sock, std::time::Duration::from_secs(5)),
        "UDS socket never appeared"
    );

    let mut client = McpClient::spawn(tmp.path());
    client.initialize();

    let resp = client.call_tool("email_list", serde_json::json!({"mailbox": "alice"}));
    let text = get_tool_text(&resp);
    let rows: serde_json::Value = serde_json::from_str(&text).expect("email_list returns JSON");
    let rows = rows.as_array().expect("array");
    assert_eq!(rows.len(), 2);
    // Descending by filename — newest first.
    assert_eq!(rows[0]["id"], "2025-06-01-002");
    assert_eq!(rows[1]["id"], "2025-06-01-001");
    assert_eq!(rows[1]["from"], "sender@example.com");
    assert_eq!(rows[1]["read"], false);
    assert_eq!(rows[0]["read"], true);

    // Client-side filter — the old `unread: true` server-side filter is
    // gone; agents now page and filter on `read == false` themselves.
    let unread: Vec<&serde_json::Value> = rows
        .iter()
        .filter(|r| r["read"].as_bool() == Some(false))
        .collect();
    assert_eq!(unread.len(), 1);
    assert_eq!(unread[0]["id"], "2025-06-01-001");

    let resp = client.call_tool(
        "email_read",
        serde_json::json!({"mailbox": "alice", "id": "2025-06-01-001"}),
    );
    let text = get_tool_text(&resp);
    assert!(text.contains("Body of 2025-06-01-001"));

    client.shutdown();
    stop_serve(daemon);
}

#[test]
fn mcp_email_list_pagination() {
    // The new `limit`/`offset` shape returns a JSON page sorted
    // descending by filename. 5 messages with `limit=2, offset=1`
    // returns rows 4 and 3 in that order.
    let tmp = TempDir::new().unwrap();
    setup_test_env(tmp.path());

    let alice_dir = inbox(tmp.path(), "alice");
    for i in 1..=5 {
        let id = format!("2025-06-0{i}-001");
        create_email_file(&alice_dir, &id, "s@example.com", "S", false);
    }

    let port = find_free_port();
    let daemon = start_serve(tmp.path(), port);
    let sock = tmp.path().join("run").join("aimx.sock");
    assert!(
        wait_for_socket(&sock, std::time::Duration::from_secs(5)),
        "UDS socket never appeared"
    );

    let mut client = McpClient::spawn(tmp.path());
    client.initialize();

    let resp = client.call_tool(
        "email_list",
        serde_json::json!({"mailbox": "alice", "limit": 2, "offset": 1}),
    );
    let text = get_tool_text(&resp);
    let rows: serde_json::Value = serde_json::from_str(&text).expect("JSON");
    let rows = rows.as_array().expect("array");
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0]["id"], "2025-06-04-001");
    assert_eq!(rows[1]["id"], "2025-06-03-001");

    client.shutdown();
    stop_serve(daemon);
}

#[test]
fn mcp_email_list_empty_mailbox_returns_empty_json_array() {
    let tmp = TempDir::new().unwrap();
    setup_test_env(tmp.path());

    let port = find_free_port();
    let daemon = start_serve(tmp.path(), port);
    let sock = tmp.path().join("run").join("aimx.sock");
    assert!(
        wait_for_socket(&sock, std::time::Duration::from_secs(5)),
        "UDS socket never appeared"
    );

    let mut client = McpClient::spawn(tmp.path());
    client.initialize();

    let resp = client.call_tool("email_list", serde_json::json!({"mailbox": "alice"}));
    let text = get_tool_text(&resp);
    assert_eq!(text, "[]");

    client.shutdown();
    stop_serve(daemon);
}

#[test]
fn mcp_email_read_nonexistent_error() {
    let tmp = TempDir::new().unwrap();
    setup_test_env(tmp.path());

    let port = find_free_port();
    let daemon = start_serve(tmp.path(), port);
    let sock = tmp.path().join("run").join("aimx.sock");
    assert!(
        wait_for_socket(&sock, std::time::Duration::from_secs(5)),
        "UDS socket never appeared"
    );

    let mut client = McpClient::spawn(tmp.path());
    client.initialize();

    let resp = client.call_tool(
        "email_read",
        serde_json::json!({"mailbox": "alice", "id": "nonexistent"}),
    );
    assert!(is_tool_error(&resp));
    let text = get_tool_text(&resp);
    assert!(text.contains("not found"), "Got: {text}");

    client.shutdown();
    stop_serve(daemon);
}

#[test]
fn mcp_email_list_nonexistent_mailbox_error() {
    let tmp = TempDir::new().unwrap();
    setup_test_env(tmp.path());

    let port = find_free_port();
    let daemon = start_serve(tmp.path(), port);
    let sock = tmp.path().join("run").join("aimx.sock");
    assert!(
        wait_for_socket(&sock, std::time::Duration::from_secs(5)),
        "UDS socket never appeared"
    );

    let mut client = McpClient::spawn(tmp.path());
    client.initialize();

    let resp = client.call_tool("email_list", serde_json::json!({"mailbox": "nonexistent"}));
    assert!(is_tool_error(&resp));
    let text = get_tool_text(&resp);
    assert!(text.contains("does not exist"), "Got: {text}");

    client.shutdown();
    stop_serve(daemon);
}

#[cfg(unix)]
#[test]
fn mcp_email_mark_read_unread() {
    // MCP's email_mark_read / email_mark_unread tools route through
    // `aimx serve` over UDS so they work without write access to the
    // root-owned mailbox files. The test spawns the daemon first and
    // points both the daemon and the MCP client at the same runtime dir.
    let tmp = TempDir::new().unwrap();
    setup_test_env(tmp.path());

    let alice_dir = inbox(tmp.path(), "alice");
    create_email_file(
        &alice_dir,
        "2025-06-01-001",
        "sender@example.com",
        "Hello",
        false,
    );

    let port = find_free_port();
    let daemon = start_serve(tmp.path(), port);
    let sock = tmp.path().join("run").join("aimx.sock");
    assert!(
        wait_for_socket(&sock, std::time::Duration::from_secs(5)),
        "UDS send socket never appeared"
    );

    let runtime = tmp.path().join("run");
    let mut child = StdCommand::new(aimx_binary_path())
        .env("AIMX_CONFIG_DIR", tmp.path())
        .env("AIMX_RUNTIME_DIR", &runtime)
        .arg("--data-dir")
        .arg(tmp.path())
        .arg("mcp")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("Failed to spawn aimx mcp");
    let stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    let (stderr_buf, stderr_drain) = spawn_stderr_drain(stderr);
    let mut client = McpClient {
        child,
        stdin,
        reader: BufReader::new(stdout),
        id: 0,
        stderr_buf,
        stderr_drain: Some(stderr_drain),
    };
    client.initialize();

    let resp = client.call_tool(
        "email_mark_read",
        serde_json::json!({"mailbox": "alice", "id": "2025-06-01-001"}),
    );
    let text = get_tool_text(&resp);
    assert!(
        text.contains("marked as read"),
        "expected 'marked as read' in response; got: {text}"
    );

    let content =
        std::fs::read_to_string(inbox(tmp.path(), "alice").join("2025-06-01-001.md")).unwrap();
    assert!(content.contains("read = true"));

    let resp = client.call_tool(
        "email_mark_unread",
        serde_json::json!({"mailbox": "alice", "id": "2025-06-01-001"}),
    );
    let text = get_tool_text(&resp);
    assert!(
        text.contains("marked as unread"),
        "expected 'marked as unread' in response; got: {text}"
    );

    let content =
        std::fs::read_to_string(inbox(tmp.path(), "alice").join("2025-06-01-001.md")).unwrap();
    assert!(content.contains("read = false"));

    client.shutdown();
    stop_serve(daemon);
}

/// When the daemon is not running, email_mark_read returns a helpful
/// error pointing the operator at `systemctl start aimx`.
#[cfg(unix)]
#[test]
fn mcp_email_mark_without_daemon_reports_missing_socket() {
    let tmp = TempDir::new().unwrap();
    setup_test_env(tmp.path());

    let alice_dir = inbox(tmp.path(), "alice");
    create_email_file(
        &alice_dir,
        "2025-06-01-001",
        "sender@example.com",
        "Hello",
        false,
    );

    // Point AIMX_RUNTIME_DIR at an empty dir; the UDS socket will not exist.
    let runtime = tmp.path().join("run");
    std::fs::create_dir_all(&runtime).ok();

    let mut child = StdCommand::new(aimx_binary_path())
        .env("AIMX_CONFIG_DIR", tmp.path())
        .env("AIMX_RUNTIME_DIR", &runtime)
        .arg("--data-dir")
        .arg(tmp.path())
        .arg("mcp")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("Failed to spawn aimx mcp");
    let stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    let (stderr_buf, stderr_drain) = spawn_stderr_drain(stderr);
    let mut client = McpClient {
        child,
        stdin,
        reader: BufReader::new(stdout),
        id: 0,
        stderr_buf,
        stderr_drain: Some(stderr_drain),
    };
    client.initialize();

    let resp = client.call_tool(
        "email_mark_read",
        serde_json::json!({"mailbox": "alice", "id": "2025-06-01-001"}),
    );
    assert!(
        is_tool_error(&resp),
        "expected a tool error when daemon absent, got: {resp:?}"
    );
    let text = get_tool_text(&resp);
    assert!(
        text.contains("aimx daemon not running"),
        "expected daemon-not-running hint, got: {text}"
    );

    client.shutdown();
}

#[test]
fn mcp_email_send_missing_mailbox_error() {
    let tmp = TempDir::new().unwrap();
    setup_test_env(tmp.path());

    let port = find_free_port();
    let daemon = start_serve(tmp.path(), port);
    let sock = tmp.path().join("run").join("aimx.sock");
    assert!(
        wait_for_socket(&sock, std::time::Duration::from_secs(5)),
        "UDS socket never appeared"
    );

    let mut client = McpClient::spawn(tmp.path());
    client.initialize();

    let resp = client.call_tool(
        "email_send",
        serde_json::json!({
            "from_mailbox": "nonexistent",
            "to": "user@example.com",
            "subject": "Test",
            "body": "Hello"
        }),
    );
    assert!(is_tool_error(&resp));
    let text = get_tool_text(&resp);
    assert!(text.contains("does not exist"), "Got: {text}");

    client.shutdown();
    stop_serve(daemon);
}

#[test]
fn mcp_email_reply_nonexistent_email_error() {
    let tmp = TempDir::new().unwrap();
    setup_test_env(tmp.path());

    let port = find_free_port();
    let daemon = start_serve(tmp.path(), port);
    let sock = tmp.path().join("run").join("aimx.sock");
    assert!(
        wait_for_socket(&sock, std::time::Duration::from_secs(5)),
        "UDS socket never appeared"
    );

    let mut client = McpClient::spawn(tmp.path());
    client.initialize();

    let resp = client.call_tool(
        "email_reply",
        serde_json::json!({
            "mailbox": "alice",
            "id": "nonexistent",
            "body": "Reply text"
        }),
    );
    assert!(is_tool_error(&resp));
    let text = get_tool_text(&resp);
    assert!(text.contains("not found"), "Got: {text}");

    client.shutdown();
    stop_serve(daemon);
}

#[test]
fn mcp_clean_exit_on_stdin_close() {
    let tmp = TempDir::new().unwrap();
    setup_test_env(tmp.path());

    let mut client = McpClient::spawn(tmp.path());
    client.initialize();

    drop(client.stdin);
    let status = client.child.wait().unwrap();
    assert!(status.success() || status.code() == Some(0));
}

#[test]
fn ingest_frontmatter_contains_dkim_spf() {
    let tmp = TempDir::new().unwrap();
    setup_test_env(tmp.path());
    let eml = std::fs::read("tests/fixtures/plain.eml").unwrap();

    aimx_cmd(tmp.path())
        .arg("--data-dir")
        .arg(tmp.path())
        .arg("ingest")
        .arg("catchall@agent.example.com")
        .write_stdin(eml)
        .assert()
        .success();

    let md_files = find_md_files(&inbox(tmp.path(), "catchall"));
    assert_eq!(md_files.len(), 1);

    let parsed = read_frontmatter(&md_files[0]);
    let table = parsed.as_table().unwrap();

    let dkim = get_toml_str(table, "dkim");
    assert!(
        dkim == "none" || dkim == "pass" || dkim == "fail",
        "dkim should be pass|fail|none, got: {dkim}"
    );

    let spf = get_toml_str(table, "spf");
    assert!(
        spf == "none" || spf == "pass" || spf == "fail",
        "spf should be pass|fail|none, got: {spf}"
    );
}

#[test]
fn setup_help_hides_domain_arg() {
    // The `<domain>` positional is hidden in --help: the wizard prompts
    // for the domain interactively and the bare `aimx setup` is the
    // documented entry point. The arg is retained as a backward-compat
    // input for scripts that already supply it (and the parse path is
    // exercised below by `setup_with_explicit_domain_still_parses`).
    Command::cargo_bin("aimx")
        .unwrap()
        .args(["setup", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("DOMAIN").not());
}

#[test]
fn setup_with_explicit_domain_still_parses() {
    // Backward-compat: `aimx setup <domain>` must still parse even
    // though the positional is hidden from --help. We check that clap
    // doesn't reject the args with a usage error before the root
    // check fires (which is the next, documented failure).
    if unsafe { libc::geteuid() } == 0 {
        eprintln!("Skipping: running as root");
        return;
    }
    Command::cargo_bin("aimx")
        .unwrap()
        .args(["setup", "agent.example.com"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("requires root"));
}

#[test]
fn setup_without_domain_proceeds_to_root_check() {
    // This test verifies non-root behavior; skip when running as root
    // (e.g. Alpine/Fedora CI containers) since setup proceeds past root check
    if unsafe { libc::geteuid() } == 0 {
        eprintln!("Skipping: running as root");
        return;
    }
    Command::cargo_bin("aimx")
        .unwrap()
        .arg("setup")
        .assert()
        .failure()
        .stderr(predicate::str::contains("requires root"));
}

#[test]
fn doctor_shows_domain_and_mailboxes() {
    let tmp = TempDir::new().unwrap();
    setup_test_env(tmp.path());

    let eml = b"From: sender@example.com\r\nTo: catchall@agent.example.com\r\nSubject: Test\r\nMessage-ID: <status-test@example.com>\r\n\r\nBody\r\n";
    aimx_cmd(tmp.path())
        .arg("--data-dir")
        .arg(tmp.path())
        .arg("ingest")
        .arg("catchall@agent.example.com")
        .write_stdin(eml.to_vec())
        .assert()
        .success();

    // `aimx doctor` exits non-zero when the Checks
    // section surfaces any `FAIL`-severity finding. `setup_test_env`
    // owns both mailboxes as the test runner and creates all four
    // storage dirs at mode 0700, so doctor should succeed.
    let assert = aimx_cmd(tmp.path())
        .arg("--data-dir")
        .arg(tmp.path())
        .arg("doctor")
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout).to_string();
    assert!(
        stdout.contains("agent.example.com"),
        "doctor output must contain domain, got:\n{stdout}"
    );
    assert!(
        stdout.contains("catchall"),
        "doctor output must contain catchall mailbox, got:\n{stdout}"
    );
    assert!(
        stdout.contains("alice"),
        "doctor output must contain alice mailbox, got:\n{stdout}"
    );
    assert!(
        stdout.contains("Mailbox"),
        "doctor output must contain Mailbox header, got:\n{stdout}"
    );
}

#[test]
fn logs_help_advertises_lines_and_follow_flags() {
    Command::cargo_bin("aimx")
        .unwrap()
        .args(["logs", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--lines"))
        .stdout(predicate::str::contains("--follow"));
}

#[test]
fn logs_subcommand_is_advertised_in_top_level_help() {
    Command::cargo_bin("aimx")
        .unwrap()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("logs"));
}

#[test]
fn doctor_renders_logs_pointer_section() {
    // doctor no longer tails the journal (too noisy in practice).
    // It now prints a `Logs` section with a one-line hint telling the
    // operator how to view logs via `aimx logs`.
    let tmp = TempDir::new().unwrap();
    setup_test_env(tmp.path());

    // With the fixture cleaned up (both mailboxes owned by
    // the test runner, all four storage dirs present at 0700), doctor
    // succeeds and the Logs section renders in the happy-path report.
    let assert = aimx_cmd(tmp.path())
        .arg("--data-dir")
        .arg(tmp.path())
        .arg("doctor")
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout).to_string();
    assert!(
        stdout.contains("Logs"),
        "doctor output must contain a 'Logs' header, got:\n{stdout}"
    );
    assert!(
        stdout.contains("aimx logs"),
        "doctor output must point the operator at `aimx logs`, got:\n{stdout}"
    );
    assert!(
        !stdout.contains("Recent logs"),
        "doctor must NOT render the old 'Recent logs' tail section, got:\n{stdout}"
    );
}

#[test]
fn doctor_help_works() {
    Command::cargo_bin("aimx")
        .unwrap()
        .args(["doctor", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("doctor"));
}

#[test]
fn mailboxes_and_mailbox_alias_produce_identical_output() {
    // `mailboxes` is the canonical subcommand name; the singular
    // `mailbox` is retained as a clap alias for muscle memory. Both must
    // produce byte-identical output for `list`.
    let tmp = TempDir::new().unwrap();
    setup_test_env(tmp.path());

    let plural = aimx_cmd(tmp.path())
        .arg("--data-dir")
        .arg(tmp.path())
        .arg("mailboxes")
        .arg("list")
        .assert()
        .success();
    let singular = aimx_cmd(tmp.path())
        .arg("--data-dir")
        .arg(tmp.path())
        .arg("mailbox")
        .arg("list")
        .assert()
        .success();

    let plural_out = String::from_utf8_lossy(&plural.get_output().stdout).to_string();
    let singular_out = String::from_utf8_lossy(&singular.get_output().stdout).to_string();
    assert_eq!(
        plural_out, singular_out,
        "`aimx mailboxes list` and `aimx mailbox list` must produce identical output"
    );
}

#[test]
fn status_subcommand_no_longer_exists() {
    // Clean rename: `aimx status` must produce a clap "unrecognized
    // subcommand" error. No alias was kept.
    let assert = Command::cargo_bin("aimx")
        .unwrap()
        .arg("status")
        .assert()
        .failure();
    let stderr = String::from_utf8_lossy(&assert.get_output().stderr).to_string();
    assert!(
        stderr.contains("unrecognized subcommand")
            || stderr.contains("invalid")
            || stderr.contains("error"),
        "expected clap error for removed `status` subcommand, got stderr: {stderr}"
    );
}

#[test]
fn portcheck_help_works() {
    Command::cargo_bin("aimx")
        .unwrap()
        .args(["portcheck", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("port 25"));
}

#[test]
fn serve_help_works() {
    Command::cargo_bin("aimx")
        .unwrap()
        .args(["serve", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("serve"))
        .stdout(predicate::str::contains("--bind"))
        .stdout(predicate::str::contains("--tls-cert"))
        .stdout(predicate::str::contains("--tls-key"));
}

fn find_free_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.local_addr().unwrap().port()
}

fn smtp_send_email(port: u16, from: &str, rcpts: &[&str], data: &str) {
    use std::io::{BufRead as _, Write as _};
    let stream = std::net::TcpStream::connect(format!("127.0.0.1:{port}")).unwrap();
    stream
        .set_read_timeout(Some(std::time::Duration::from_secs(10)))
        .unwrap();
    let mut reader = std::io::BufReader::new(stream.try_clone().unwrap());
    let mut writer = stream;

    let mut buf = String::new();
    reader.read_line(&mut buf).unwrap();
    assert!(buf.starts_with("220"), "Expected banner, got: {buf}");

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

    for rcpt in rcpts {
        buf.clear();
        write!(writer, "RCPT TO:<{rcpt}>\r\n").unwrap();
        reader.read_line(&mut buf).unwrap();
        assert!(buf.starts_with("250"), "RCPT TO failed: {buf}");
    }

    buf.clear();
    write!(writer, "DATA\r\n").unwrap();
    reader.read_line(&mut buf).unwrap();
    assert!(buf.starts_with("354"), "DATA failed: {buf}");

    write!(writer, "{data}\r\n.\r\n").unwrap();
    buf.clear();
    reader.read_line(&mut buf).unwrap();
    assert!(buf.starts_with("250"), "DATA end failed: {buf}");

    write!(writer, "QUIT\r\n").unwrap();
    buf.clear();
    let _ = reader.read_line(&mut buf);
}

#[test]
fn serve_e2e_receive_email_and_shutdown() {
    let tmp = TempDir::new().unwrap();
    setup_test_env(tmp.path());
    let port = find_free_port();

    let runtime = tmp.path().join("run");
    std::fs::create_dir_all(&runtime).ok();
    let mut child = StdCommand::new(aimx_binary_path())
        .env("AIMX_CONFIG_DIR", tmp.path())
        .env("AIMX_RUNTIME_DIR", &runtime)
        .arg("--data-dir")
        .arg(tmp.path())
        .arg("serve")
        .arg("--bind")
        .arg(format!("127.0.0.1:{port}"))
        .stderr(Stdio::piped())
        .spawn()
        .expect("Failed to spawn aimx serve");

    // Wait for server to be ready
    let started = std::time::Instant::now();
    loop {
        if started.elapsed() > std::time::Duration::from_secs(30) {
            child.kill().unwrap();
            panic!("aimx serve did not start within 30s");
        }
        if std::net::TcpStream::connect(format!("127.0.0.1:{port}")).is_ok() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }

    let email_data = "From: sender@example.com\r\nTo: alice@agent.example.com\r\nSubject: E2E Test\r\nDate: Mon, 01 Jan 2024 00:00:00 +0000\r\nMessage-ID: <e2e-test@example.com>\r\n\r\nHello from the e2e test";

    smtp_send_email(
        port,
        "sender@example.com",
        &["alice@agent.example.com"],
        email_data,
    );

    // Allow ingest to complete
    std::thread::sleep(std::time::Duration::from_millis(500));

    let alice_dir = inbox(tmp.path(), "alice");
    let md_files = find_md_files(&alice_dir);
    assert_eq!(md_files.len(), 1, "Expected 1 email in alice mailbox");

    let content = std::fs::read_to_string(&md_files[0]).unwrap();
    assert!(content.contains("subject = \"E2E Test\""));
    assert!(content.contains("Hello from the e2e test"));

    // Send SIGTERM
    unsafe {
        libc::kill(child.id() as libc::pid_t, libc::SIGTERM);
    }

    let status = child
        .wait_timeout(std::time::Duration::from_secs(10))
        .unwrap();
    assert!(
        status.is_some(),
        "aimx serve should exit within 10s of SIGTERM"
    );
    let status = status.unwrap();
    assert!(status.success(), "aimx serve should exit cleanly: {status}");
}

#[test]
fn serve_e2e_multi_recipient() {
    let tmp = TempDir::new().unwrap();
    setup_test_env(tmp.path());
    let port = find_free_port();

    let runtime = tmp.path().join("run");
    std::fs::create_dir_all(&runtime).ok();
    let mut child = StdCommand::new(aimx_binary_path())
        .env("AIMX_CONFIG_DIR", tmp.path())
        .env("AIMX_RUNTIME_DIR", &runtime)
        .arg("--data-dir")
        .arg(tmp.path())
        .arg("serve")
        .arg("--bind")
        .arg(format!("127.0.0.1:{port}"))
        .stderr(Stdio::piped())
        .spawn()
        .expect("Failed to spawn aimx serve");

    let started = std::time::Instant::now();
    loop {
        if started.elapsed() > std::time::Duration::from_secs(30) {
            child.kill().unwrap();
            panic!("aimx serve did not start within 30s");
        }
        if std::net::TcpStream::connect(format!("127.0.0.1:{port}")).is_ok() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }

    let email_data = "From: sender@example.com\r\nTo: alice@agent.example.com, catchall@agent.example.com\r\nSubject: Multi RCPT\r\nDate: Mon, 01 Jan 2024 00:00:00 +0000\r\nMessage-ID: <multi-rcpt@example.com>\r\n\r\nMulti recipient test";

    smtp_send_email(
        port,
        "sender@example.com",
        &["alice@agent.example.com", "catchall@agent.example.com"],
        email_data,
    );

    std::thread::sleep(std::time::Duration::from_millis(500));

    let alice_files = find_md_files(&inbox(tmp.path(), "alice"));
    let catchall_files = find_md_files(&inbox(tmp.path(), "catchall"));
    assert_eq!(alice_files.len(), 1, "Expected 1 email in alice mailbox");
    assert_eq!(
        catchall_files.len(),
        1,
        "Expected 1 email in catchall mailbox"
    );

    unsafe {
        libc::kill(child.id() as libc::pid_t, libc::SIGTERM);
    }
    let _ = child.wait_timeout(std::time::Duration::from_secs(10));
}

#[test]
fn serve_e2e_connection_refused_after_shutdown() {
    let tmp = TempDir::new().unwrap();
    setup_test_env(tmp.path());
    let port = find_free_port();

    let runtime = tmp.path().join("run");
    std::fs::create_dir_all(&runtime).ok();
    let mut child = StdCommand::new(aimx_binary_path())
        .env("AIMX_CONFIG_DIR", tmp.path())
        .env("AIMX_RUNTIME_DIR", &runtime)
        .arg("--data-dir")
        .arg(tmp.path())
        .arg("serve")
        .arg("--bind")
        .arg(format!("127.0.0.1:{port}"))
        .stderr(Stdio::piped())
        .spawn()
        .expect("Failed to spawn aimx serve");

    let started = std::time::Instant::now();
    loop {
        if started.elapsed() > std::time::Duration::from_secs(30) {
            child.kill().unwrap();
            panic!("aimx serve did not start within 30s");
        }
        if std::net::TcpStream::connect(format!("127.0.0.1:{port}")).is_ok() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }

    // Send SIGTERM and wait for exit
    unsafe {
        libc::kill(child.id() as libc::pid_t, libc::SIGTERM);
    }
    let status = child
        .wait_timeout(std::time::Duration::from_secs(10))
        .unwrap();
    assert!(status.is_some(), "aimx serve should exit within 10s");

    // Connection should be refused after shutdown
    std::thread::sleep(std::time::Duration::from_millis(200));
    let result = std::net::TcpStream::connect(format!("127.0.0.1:{port}"));
    assert!(
        result.is_err(),
        "Connection should be refused after shutdown"
    );
}

fn setup_test_env_with_bob(tmp: &Path) -> String {
    let owner = current_username();
    let config_content = format!(
        "domain = \"agent.example.com\"\ndata_dir = \"{}\"\n\n[mailboxes.catchall]\naddress = \"*@agent.example.com\"\nowner = \"aimx-catchall\"\n\n[mailboxes.alice]\naddress = \"alice@agent.example.com\"\nowner = \"{owner}\"\n\n[mailboxes.bob]\naddress = \"bob@agent.example.com\"\nowner = \"{owner}\"\n",
        tmp.display()
    );
    std::fs::create_dir_all(tmp.join("inbox").join("catchall")).unwrap();
    std::fs::create_dir_all(tmp.join("inbox").join("alice")).unwrap();
    std::fs::create_dir_all(tmp.join("inbox").join("bob")).unwrap();
    std::fs::create_dir_all(tmp.join("sent").join("alice")).unwrap();
    std::fs::create_dir_all(tmp.join("sent").join("bob")).unwrap();
    let config_path = tmp.join("config.toml");
    std::fs::write(&config_path, &config_content).unwrap();
    install_cached_dkim_keys(tmp);
    config_path.to_string_lossy().to_string()
}

fn start_serve(tmp: &Path, port: u16) -> std::process::Child {
    let runtime = tmp.join("run");
    std::fs::create_dir_all(&runtime).ok();
    let mut child = StdCommand::new(aimx_binary_path())
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
        .expect("Failed to spawn aimx serve");

    let started = std::time::Instant::now();
    loop {
        if started.elapsed() > std::time::Duration::from_secs(30) {
            // Kill the daemon, then drain its stderr so the panic
            // message carries whatever the daemon logged before it
            // failed to bind. Without this, timeouts surface as the
            // bare "did not start within 30s" panic with zero
            // diagnostic.
            let _ = child.kill();
            let mut stderr_buf = String::new();
            if let Some(mut err) = child.stderr.take() {
                use std::io::Read as _;
                let _ = err.read_to_string(&mut stderr_buf);
            }
            panic!(
                "aimx serve did not start within 30s on port {port}; stderr: {}",
                stderr_buf.trim()
            );
        }
        if std::net::TcpStream::connect(format!("127.0.0.1:{port}")).is_ok() {
            break;
        }
        // Early-exit detection: if the daemon died before binding, no
        // amount of polling on `port` will succeed. Surface its stderr
        // immediately instead of waiting the full 30s.
        if let Ok(Some(status)) = child.try_wait() {
            let mut stderr_buf = String::new();
            if let Some(mut err) = child.stderr.take() {
                use std::io::Read as _;
                let _ = err.read_to_string(&mut stderr_buf);
            }
            panic!(
                "aimx serve exited early with {status:?} before binding port {port}; stderr: {}",
                stderr_buf.trim()
            );
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    child
}

fn stop_serve(mut child: std::process::Child) {
    unsafe {
        libc::kill(child.id() as libc::pid_t, libc::SIGTERM);
    }
    let _ = child.wait_timeout(std::time::Duration::from_secs(10));
}

#[test]
fn serve_e2e_single_attachment() {
    let tmp = TempDir::new().unwrap();
    setup_test_env(tmp.path());
    let port = find_free_port();
    let child = start_serve(tmp.path(), port);

    let email_data = concat!(
        "From: sender@example.com\r\n",
        "To: alice@agent.example.com\r\n",
        "Subject: Single Attachment\r\n",
        "Date: Mon, 01 Jan 2024 00:00:00 +0000\r\n",
        "Message-ID: <att-single@example.com>\r\n",
        "MIME-Version: 1.0\r\n",
        "Content-Type: multipart/mixed; boundary=\"boundary1\"\r\n",
        "\r\n",
        "--boundary1\r\n",
        "Content-Type: text/plain; charset=utf-8\r\n",
        "\r\n",
        "Please find attached.\r\n",
        "--boundary1\r\n",
        "Content-Type: text/plain; name=\"report.txt\"\r\n",
        "Content-Disposition: attachment; filename=\"report.txt\"\r\n",
        "\r\n",
        "Quarterly results here.\r\n",
        "--boundary1--",
    );

    smtp_send_email(
        port,
        "sender@example.com",
        &["alice@agent.example.com"],
        email_data,
    );
    std::thread::sleep(std::time::Duration::from_millis(500));

    let alice_dir = inbox(tmp.path(), "alice");
    let md_files = find_md_files(&alice_dir);
    assert_eq!(md_files.len(), 1, "Expected 1 email in alice mailbox");

    let att_path =
        find_attachment(&alice_dir, "report.txt").expect("report.txt missing from bundle");
    assert!(att_path.exists(), "Attachment file should exist on disk");
    let att_content = std::fs::read_to_string(&att_path).unwrap();
    assert!(
        att_content.contains("Quarterly results"),
        "Attachment content mismatch"
    );

    let fm = read_frontmatter(&md_files[0]);
    let table = fm.as_table().unwrap();
    let attachments = table.get("attachments").unwrap().as_array().unwrap();
    assert_eq!(attachments.len(), 1, "Expected 1 attachment in frontmatter");
    let att = attachments[0].as_table().unwrap();
    assert_eq!(get_toml_str(att, "filename"), "report.txt");
    assert_eq!(get_toml_str(att, "path"), "report.txt");
    assert!(att.get("size").unwrap().as_integer().unwrap() > 0);

    let content = std::fs::read_to_string(&md_files[0]).unwrap();
    assert!(content.contains("Please find attached."));

    stop_serve(child);
}

#[test]
fn serve_e2e_multiple_attachments() {
    let tmp = TempDir::new().unwrap();
    setup_test_env(tmp.path());
    let port = find_free_port();
    let child = start_serve(tmp.path(), port);

    let email_data = concat!(
        "From: sender@example.com\r\n",
        "To: alice@agent.example.com\r\n",
        "Subject: Multiple Attachments\r\n",
        "Date: Mon, 01 Jan 2024 00:00:00 +0000\r\n",
        "Message-ID: <att-multi@example.com>\r\n",
        "MIME-Version: 1.0\r\n",
        "Content-Type: multipart/mixed; boundary=\"boundary2\"\r\n",
        "\r\n",
        "--boundary2\r\n",
        "Content-Type: text/plain; charset=utf-8\r\n",
        "\r\n",
        "Multiple files attached.\r\n",
        "--boundary2\r\n",
        "Content-Type: text/plain; name=\"notes.txt\"\r\n",
        "Content-Disposition: attachment; filename=\"notes.txt\"\r\n",
        "\r\n",
        "Meeting notes from Monday.\r\n",
        "--boundary2\r\n",
        "Content-Type: text/csv; name=\"data.csv\"\r\n",
        "Content-Disposition: attachment; filename=\"data.csv\"\r\n",
        "\r\n",
        "name,value\r\nalpha,1\r\nbeta,2\r\n",
        "--boundary2\r\n",
        "Content-Type: application/octet-stream; name=\"image.png\"\r\n",
        "Content-Disposition: attachment; filename=\"image.png\"\r\n",
        "\r\n",
        "FAKE PNG CONTENT FOR TESTING\r\n",
        "--boundary2--",
    );

    smtp_send_email(
        port,
        "sender@example.com",
        &["alice@agent.example.com"],
        email_data,
    );
    std::thread::sleep(std::time::Duration::from_millis(500));

    let alice_dir = inbox(tmp.path(), "alice");
    let md_files = find_md_files(&alice_dir);
    assert_eq!(md_files.len(), 1, "Expected 1 email in alice mailbox");

    let notes_path = find_attachment(&alice_dir, "notes.txt").expect("notes.txt missing");
    let csv_path = find_attachment(&alice_dir, "data.csv").expect("data.csv missing");
    let image_path = find_attachment(&alice_dir, "image.png").expect("image.png missing");
    assert!(notes_path.exists());
    assert!(csv_path.exists());
    assert!(image_path.exists());

    let notes = std::fs::read_to_string(&notes_path).unwrap();
    assert!(notes.contains("Meeting notes"));

    let csv = std::fs::read_to_string(&csv_path).unwrap();
    assert!(csv.contains("alpha,1"));

    let fm = read_frontmatter(&md_files[0]);
    let table = fm.as_table().unwrap();
    let attachments = table.get("attachments").unwrap().as_array().unwrap();
    assert_eq!(
        attachments.len(),
        3,
        "Expected 3 attachments in frontmatter"
    );

    let filenames: Vec<&str> = attachments
        .iter()
        .map(|a| get_toml_str(a.as_table().unwrap(), "filename"))
        .collect();
    assert!(filenames.contains(&"notes.txt"));
    assert!(filenames.contains(&"data.csv"));
    assert!(filenames.contains(&"image.png"));

    let content = std::fs::read_to_string(&md_files[0]).unwrap();
    assert!(content.contains("Multiple files attached."));

    stop_serve(child);
}

#[test]
fn serve_e2e_attachment_multi_recipient() {
    let tmp = TempDir::new().unwrap();
    setup_test_env(tmp.path());
    let port = find_free_port();
    let child = start_serve(tmp.path(), port);

    let email_data = concat!(
        "From: sender@example.com\r\n",
        "To: alice@agent.example.com, catchall@agent.example.com\r\n",
        "Subject: Shared Attachment\r\n",
        "Date: Mon, 01 Jan 2024 00:00:00 +0000\r\n",
        "Message-ID: <att-shared@example.com>\r\n",
        "MIME-Version: 1.0\r\n",
        "Content-Type: multipart/mixed; boundary=\"boundary3\"\r\n",
        "\r\n",
        "--boundary3\r\n",
        "Content-Type: text/plain; charset=utf-8\r\n",
        "\r\n",
        "Shared attachment.\r\n",
        "--boundary3\r\n",
        "Content-Type: text/plain; name=\"shared.txt\"\r\n",
        "Content-Disposition: attachment; filename=\"shared.txt\"\r\n",
        "\r\n",
        "This file goes to both.\r\n",
        "--boundary3--",
    );

    smtp_send_email(
        port,
        "sender@example.com",
        &["alice@agent.example.com", "catchall@agent.example.com"],
        email_data,
    );
    std::thread::sleep(std::time::Duration::from_millis(500));

    let alice_dir = inbox(tmp.path(), "alice");
    let catchall_dir = inbox(tmp.path(), "catchall");
    assert_eq!(find_md_files(&alice_dir).len(), 1);
    assert_eq!(find_md_files(&catchall_dir).len(), 1);

    let alice_att =
        find_attachment(&alice_dir, "shared.txt").expect("alice bundle missing shared.txt");
    let catchall_att =
        find_attachment(&catchall_dir, "shared.txt").expect("catchall bundle missing shared.txt");
    assert!(alice_att.exists(), "alice should have attachment");
    assert!(catchall_att.exists(), "catchall should have attachment");

    let alice_content = std::fs::read_to_string(&alice_att).unwrap();
    let catchall_content = std::fs::read_to_string(&catchall_att).unwrap();
    assert!(alice_content.contains("This file goes to both."));
    assert!(catchall_content.contains("This file goes to both."));

    stop_serve(child);
}

#[test]
fn serve_e2e_cc_recipients() {
    let tmp = TempDir::new().unwrap();
    setup_test_env_with_bob(tmp.path());
    let port = find_free_port();
    let child = start_serve(tmp.path(), port);

    let email_data = concat!(
        "From: sender@example.com\r\n",
        "To: alice@agent.example.com\r\n",
        "CC: bob@agent.example.com\r\n",
        "Subject: CC Test\r\n",
        "Date: Mon, 01 Jan 2024 00:00:00 +0000\r\n",
        "Message-ID: <cc-test@example.com>\r\n",
        "\r\n",
        "Testing CC delivery",
    );

    smtp_send_email(
        port,
        "sender@example.com",
        &["alice@agent.example.com", "bob@agent.example.com"],
        email_data,
    );
    std::thread::sleep(std::time::Duration::from_millis(500));

    let alice_files = find_md_files(&inbox(tmp.path(), "alice"));
    let bob_files = find_md_files(&inbox(tmp.path(), "bob"));
    assert_eq!(alice_files.len(), 1, "Expected 1 email in alice mailbox");
    assert_eq!(bob_files.len(), 1, "Expected 1 email in bob mailbox");

    let alice_fm = read_frontmatter(&alice_files[0]);
    let bob_fm = read_frontmatter(&bob_files[0]);
    let alice_table = alice_fm.as_table().unwrap();
    let bob_table = bob_fm.as_table().unwrap();

    assert_eq!(get_toml_str(alice_table, "subject"), "CC Test");
    assert_eq!(get_toml_str(bob_table, "subject"), "CC Test");
    assert_eq!(get_toml_str(alice_table, "mailbox"), "alice");
    assert_eq!(get_toml_str(bob_table, "mailbox"), "bob");

    let alice_content = std::fs::read_to_string(&alice_files[0]).unwrap();
    let bob_content = std::fs::read_to_string(&bob_files[0]).unwrap();
    assert!(alice_content.contains("Testing CC delivery"));
    assert!(bob_content.contains("Testing CC delivery"));

    stop_serve(child);
}

#[test]
fn serve_e2e_bcc_recipients() {
    let tmp = TempDir::new().unwrap();
    setup_test_env_with_bob(tmp.path());
    let port = find_free_port();
    let child = start_serve(tmp.path(), port);

    // No BCC header; bob is BCC'd via envelope only
    let email_data = concat!(
        "From: sender@example.com\r\n",
        "To: alice@agent.example.com\r\n",
        "Subject: BCC Test\r\n",
        "Date: Mon, 01 Jan 2024 00:00:00 +0000\r\n",
        "Message-ID: <bcc-test@example.com>\r\n",
        "\r\n",
        "Testing BCC delivery",
    );

    smtp_send_email(
        port,
        "sender@example.com",
        &["alice@agent.example.com", "bob@agent.example.com"],
        email_data,
    );
    std::thread::sleep(std::time::Duration::from_millis(500));

    let alice_files = find_md_files(&inbox(tmp.path(), "alice"));
    let bob_files = find_md_files(&inbox(tmp.path(), "bob"));
    assert_eq!(alice_files.len(), 1, "Expected 1 email in alice mailbox");
    assert_eq!(bob_files.len(), 1, "Expected 1 email in bob (BCC) mailbox");

    let alice_fm = read_frontmatter(&alice_files[0]);
    let bob_fm = read_frontmatter(&bob_files[0]);
    let alice_table = alice_fm.as_table().unwrap();
    let bob_table = bob_fm.as_table().unwrap();

    assert_eq!(get_toml_str(alice_table, "subject"), "BCC Test");
    assert_eq!(get_toml_str(bob_table, "subject"), "BCC Test");
    assert_eq!(get_toml_str(alice_table, "mailbox"), "alice");
    assert_eq!(get_toml_str(bob_table, "mailbox"), "bob");

    // BCC address should not appear as a Bcc: header in the stored email
    let bob_content = std::fs::read_to_string(&bob_files[0]).unwrap();
    assert!(
        !bob_content.contains("Bcc:")
            && !bob_content.contains("bcc:")
            && !bob_content.contains("BCC:"),
        "BCC header line should not be in stored email"
    );
    // delivered_to carries the actual RCPT TO (envelope recipient),
    // which for BCC is the BCC address.
    assert_eq!(
        get_toml_str(bob_table, "delivered_to"),
        "bob@agent.example.com",
        "delivered_to should be the envelope recipient (BCC address)"
    );
    assert_eq!(
        get_toml_str(bob_table, "to"),
        "alice@agent.example.com",
        "To field should be the To header, not the envelope recipient"
    );

    stop_serve(child);
}

#[test]
fn serve_e2e_to_cc_bcc_combined() {
    let tmp = TempDir::new().unwrap();
    setup_test_env_with_bob(tmp.path());
    let port = find_free_port();
    let child = start_serve(tmp.path(), port);

    // To: alice, CC: bob, BCC: catchall (catchall not in headers)
    let email_data = concat!(
        "From: sender@example.com\r\n",
        "To: alice@agent.example.com\r\n",
        "CC: bob@agent.example.com\r\n",
        "Subject: All Recipients Test\r\n",
        "Date: Mon, 01 Jan 2024 00:00:00 +0000\r\n",
        "Message-ID: <all-rcpt@example.com>\r\n",
        "\r\n",
        "Testing all recipient types",
    );

    smtp_send_email(
        port,
        "sender@example.com",
        &[
            "alice@agent.example.com",
            "bob@agent.example.com",
            "catchall@agent.example.com",
        ],
        email_data,
    );
    std::thread::sleep(std::time::Duration::from_millis(500));

    let alice_files = find_md_files(&inbox(tmp.path(), "alice"));
    let bob_files = find_md_files(&inbox(tmp.path(), "bob"));
    let catchall_files = find_md_files(&inbox(tmp.path(), "catchall"));
    assert_eq!(alice_files.len(), 1, "Expected 1 email in alice (To)");
    assert_eq!(bob_files.len(), 1, "Expected 1 email in bob (CC)");
    assert_eq!(
        catchall_files.len(),
        1,
        "Expected 1 email in catchall (BCC)"
    );

    for (files, expected_mailbox) in [
        (&alice_files, "alice"),
        (&bob_files, "bob"),
        (&catchall_files, "catchall"),
    ] {
        let fm = read_frontmatter(&files[0]);
        let table = fm.as_table().unwrap();
        assert_eq!(get_toml_str(table, "subject"), "All Recipients Test");
        assert_eq!(get_toml_str(table, "mailbox"), expected_mailbox);
        assert_eq!(
            get_toml_str(table, "to"),
            "alice@agent.example.com",
            "To field should be from header, not envelope"
        );

        let content = std::fs::read_to_string(&files[0]).unwrap();
        assert!(content.contains("Testing all recipient types"));
    }

    stop_serve(child);
}

// ---------------------------------------------------------------------------
// UDS send listener integration tests.
//
// These tests spawn `aimx serve` as a subprocess (same pattern as the
// `serve_e2e_*` tests above) and drive the `/run/aimx/aimx.sock` UDS
// listener with a raw Unix-socket client. `AIMX_RUNTIME_DIR` is overridden
// to a tempdir so the socket lives inside the test sandbox; the binary
// under test creates it with mode `0o666`. We never exercise the real MX
// delivery path; the `ERR DOMAIN` and `ERR MALFORMED` responses prove the
// framing and handler wiring are intact without reaching the network.
// ---------------------------------------------------------------------------

#[cfg(unix)]
fn wait_for_socket(path: &Path, timeout: std::time::Duration) -> bool {
    let started = std::time::Instant::now();
    while started.elapsed() < timeout {
        if path.exists() {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    false
}

#[cfg(unix)]
#[test]
fn serve_creates_send_socket_with_world_writable_mode() {
    use std::os::unix::fs::PermissionsExt;

    let tmp = TempDir::new().unwrap();
    setup_test_env(tmp.path());
    let port = find_free_port();
    let child = start_serve(tmp.path(), port);

    let sock = tmp.path().join("run").join("aimx.sock");
    assert!(
        wait_for_socket(&sock, std::time::Duration::from_secs(5)),
        "UDS send socket {} never appeared",
        sock.display()
    );

    let mode = std::fs::metadata(&sock).unwrap().permissions().mode() & 0o777;
    assert_eq!(
        mode, 0o666,
        "UDS send socket must be world-writable (0o666); got {mode:o}"
    );

    stop_serve(child);
}

#[cfg(unix)]
#[test]
fn uds_send_listener_accepts_and_rejects_domain_mismatch() {
    use std::io::Read as _;
    use std::os::unix::net::UnixStream;

    let tmp = TempDir::new().unwrap();
    setup_test_env(tmp.path());
    let port = find_free_port();
    let child = start_serve(tmp.path(), port);

    let sock = tmp.path().join("run").join("aimx.sock");
    assert!(
        wait_for_socket(&sock, std::time::Duration::from_secs(5)),
        "UDS send socket never appeared"
    );

    // Submit an AIMX/1 SEND with a From: domain that does not match the
    // configured primary domain (`agent.example.com`). This must be
    // rejected with `ERR DOMAIN` before any MX lookup happens, proving
    // both the wire parser and the handler wiring end-to-end.
    //
    // There is no `From-Mailbox:` header; the daemon parses `From:`
    // out of the body and resolves the mailbox itself.
    let body = b"From: alice@not-the-domain.example\r\n\
                 To: user@gmail.com\r\n\
                 Subject: Hi\r\n\
                 Date: Thu, 01 Jan 2025 12:00:00 +0000\r\n\
                 Message-ID: <integ-abc@not-the-domain.example>\r\n\
                 \r\n\
                 hello\r\n";
    let header = format!("AIMX/1 SEND\nContent-Length: {}\n\n", body.len());

    let mut client = UnixStream::connect(&sock).expect("connect UDS");
    client.write_all(header.as_bytes()).unwrap();
    client.write_all(body).unwrap();
    // Signal "no more bytes coming" so the server can return the response.
    client
        .shutdown(std::net::Shutdown::Write)
        .expect("shutdown write");

    let mut resp = String::new();
    client.read_to_string(&mut resp).expect("read response");

    assert!(
        resp.starts_with("AIMX/1 ERR DOMAIN"),
        "expected ERR DOMAIN, got {resp:?}"
    );
    assert!(resp.ends_with('\n'), "response must be LF-terminated");

    stop_serve(child);
}

#[cfg(unix)]
#[test]
fn uds_send_listener_rejects_malformed_request() {
    use std::io::Read as _;
    use std::os::unix::net::UnixStream;

    let tmp = TempDir::new().unwrap();
    setup_test_env(tmp.path());
    let port = find_free_port();
    let child = start_serve(tmp.path(), port);

    let sock = tmp.path().join("run").join("aimx.sock");
    assert!(
        wait_for_socket(&sock, std::time::Duration::from_secs(5)),
        "UDS send socket never appeared"
    );

    // Wrong leading line must be rejected with `ERR MALFORMED`.
    let mut client = UnixStream::connect(&sock).expect("connect UDS");
    client
        .write_all(b"GET / HTTP/1.1\r\nHost: x\r\n\r\n")
        .unwrap();
    client
        .shutdown(std::net::Shutdown::Write)
        .expect("shutdown write");

    let mut resp = String::new();
    client.read_to_string(&mut resp).expect("read response");
    assert!(
        resp.starts_with("AIMX/1 ERR MALFORMED"),
        "expected ERR MALFORMED, got {resp:?}"
    );

    stop_serve(child);
}

#[cfg(unix)]
#[test]
fn uds_send_listener_cleaned_up_after_sigterm() {
    let tmp = TempDir::new().unwrap();
    setup_test_env(tmp.path());
    let port = find_free_port();
    let child = start_serve(tmp.path(), port);

    let sock = tmp.path().join("run").join("aimx.sock");
    assert!(
        wait_for_socket(&sock, std::time::Duration::from_secs(5)),
        "UDS send socket never appeared"
    );

    // Clean shutdown: the listener removes the socket file so the next
    // start does not trip the stale-socket retry path.
    stop_serve(child);

    let started = std::time::Instant::now();
    while started.elapsed() < std::time::Duration::from_secs(5) {
        if !sock.exists() {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    panic!(
        "aimx.sock should be removed on clean shutdown but still exists at {}",
        sock.display()
    );
}

// ---------------------------------------------------------------------------
// `aimx send` thin UDS client end-to-end.
//
// Spawns `aimx serve` with `AIMX_TEST_MAIL_DROP` pointing at a tempfile so
// the daemon's outbound MX transport is replaced with a file-drop capture.
// Then invokes `aimx send` via `assert_cmd` and asserts:
//   * client exited 0
//   * daemon logs include peer_uid=/peer_pid= for the accepted send
//   * the captured (signed) message carries a DKIM-Signature header that
//     verifies against the test public key using the relaxed-canonicalization
//     helper.
// ---------------------------------------------------------------------------

#[cfg(unix)]
fn start_serve_with_mail_drop(
    tmp: &Path,
    port: u16,
    mail_drop: &Path,
) -> (std::process::Child, std::path::PathBuf) {
    let runtime = tmp.join("run");
    std::fs::create_dir_all(&runtime).ok();
    let mut child = StdCommand::new(aimx_binary_path())
        .env("AIMX_CONFIG_DIR", tmp)
        .env("AIMX_RUNTIME_DIR", &runtime)
        .env("AIMX_TEST_MAIL_DROP", mail_drop)
        .env("AIMX_SANDBOX_FORCE_FALLBACK", "1")
        .arg("--data-dir")
        .arg(tmp)
        .arg("serve")
        .arg("--bind")
        .arg(format!("127.0.0.1:{port}"))
        .stderr(Stdio::piped())
        .spawn()
        .expect("Failed to spawn aimx serve");

    let started = std::time::Instant::now();
    loop {
        if started.elapsed() > std::time::Duration::from_secs(30) {
            child.kill().unwrap();
            panic!("aimx serve did not start within 30s");
        }
        if std::net::TcpStream::connect(format!("127.0.0.1:{port}")).is_ok() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }

    let sock = runtime.join("aimx.sock");
    assert!(
        wait_for_socket(&sock, std::time::Duration::from_secs(5)),
        "UDS send socket never appeared"
    );

    (child, sock)
}

#[cfg(unix)]
fn extract_dkim_bh(signed: &[u8]) -> String {
    let signed_str = String::from_utf8_lossy(signed).to_string();
    let mut dkim_header = String::new();
    let mut in_dkim = false;
    for line in signed_str.lines() {
        if line.starts_with("DKIM-Signature:") {
            in_dkim = true;
            dkim_header.push_str(line);
        } else if in_dkim && (line.starts_with('\t') || line.starts_with(' ')) {
            dkim_header.push_str(line);
        } else if in_dkim {
            break;
        }
    }
    let bh_start = dkim_header.find("bh=").expect("bh= not found");
    let bh_value = &dkim_header[bh_start + 3..];
    let bh_end = bh_value.find(';').unwrap_or(bh_value.len());
    bh_value[..bh_end].replace([' ', '\t'], "")
}

#[cfg(unix)]
fn compute_relaxed_body_hash(signed: &[u8]) -> String {
    use base64::Engine;
    use sha2::{Digest, Sha256};

    let signed_str = String::from_utf8_lossy(signed);
    let body_start = signed_str.find("\r\n\r\n").expect("No body separator") + 4;
    let body = &signed[body_start..];

    let body_str = String::from_utf8_lossy(body);
    let mut canonical_body = String::new();
    for line in body_str.split("\r\n") {
        let trimmed = line.split_whitespace().collect::<Vec<_>>().join(" ");
        let trimmed = trimmed.trim_end();
        canonical_body.push_str(trimmed);
        canonical_body.push_str("\r\n");
    }
    while canonical_body.ends_with("\r\n\r\n") {
        canonical_body.truncate(canonical_body.len() - 2);
    }

    let mut hasher = Sha256::new();
    hasher.update(canonical_body.as_bytes());
    base64::engine::general_purpose::STANDARD.encode(hasher.finalize())
}

#[cfg(unix)]
#[test]
fn send_uds_end_to_end_delivers_signed_message() {
    let tmp = TempDir::new().unwrap();
    setup_test_env(tmp.path());
    let port = find_free_port();

    let mail_drop = tmp.path().join("outbound.log");
    let (mut child, _sock) = start_serve_with_mail_drop(tmp.path(), port, &mail_drop);

    // Read daemon stderr concurrently so the test can later assert the
    // peer_uid/peer_pid trace lines emitted by the UDS accept loop.
    let stderr = child.stderr.take().expect("daemon stderr must be piped");
    let captured_stderr: std::sync::Arc<std::sync::Mutex<String>> =
        std::sync::Arc::new(std::sync::Mutex::new(String::new()));
    let captured_clone = std::sync::Arc::clone(&captured_stderr);
    let reader_thread = std::thread::spawn(move || {
        let mut reader = BufReader::new(stderr);
        let mut line = String::new();
        while let Ok(n) = reader.read_line(&mut line) {
            if n == 0 {
                break;
            }
            captured_clone.lock().unwrap().push_str(&line);
            line.clear();
        }
    });

    let runtime = tmp.path().join("run");
    let output = Command::cargo_bin("aimx")
        .unwrap()
        .env("AIMX_CONFIG_DIR", tmp.path())
        .env("AIMX_RUNTIME_DIR", &runtime)
        .arg("--data-dir")
        .arg(tmp.path())
        .arg("send")
        .arg("--from")
        .arg("alice@agent.example.com")
        .arg("--to")
        .arg("recipient@example.com")
        .arg("--subject")
        .arg("End-to-end UDS send")
        .arg("--body")
        .arg("Hello from the end-to-end test.")
        .output()
        .expect("aimx send failed to run");

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr_out = String::from_utf8_lossy(&output.stderr).to_string();

    assert!(
        output.status.success(),
        "aimx send should exit 0; status={:?}, stdout={stdout}, stderr={stderr_out}",
        output.status
    );
    assert!(
        stderr_out.contains("Email sent.") && stderr_out.contains("Message-ID:"),
        "stderr should contain 'Email sent.' and 'Message-ID:', got {stderr_out}"
    );
    assert!(
        !stdout.trim().is_empty(),
        "stdout should echo the Message-ID"
    );

    // Give the daemon a moment to flush its stderr and the file-drop write.
    std::thread::sleep(std::time::Duration::from_millis(200));

    let signed = std::fs::read(&mail_drop).expect("mail drop file missing");
    assert!(
        signed.starts_with(b"----- AIMX TEST DROP -----\n"),
        "mail drop should begin with the sentinel header"
    );
    let payload = &signed[b"----- AIMX TEST DROP -----\n".len()..];
    let payload_str = String::from_utf8_lossy(payload);

    assert!(
        payload_str.contains("DKIM-Signature:"),
        "captured message must contain DKIM-Signature; got:\n{payload_str}"
    );
    assert!(
        payload_str.contains("From: alice@agent.example.com"),
        "captured message must echo the original From header"
    );
    assert!(
        payload_str.contains("Sent from AIMX.") && payload_str.contains("https://aimx.email"),
        "default signature must be appended to the delivered body when config omits it; got:\n{payload_str}"
    );

    // Cryptographic DKIM body-hash verification using relaxed
    // canonicalization.
    let signed_header = extract_dkim_bh(payload);
    let computed = compute_relaxed_body_hash(payload);
    assert_eq!(
        signed_header, computed,
        "DKIM body hash must verify: signed={signed_header}, computed={computed}"
    );

    // Stop the daemon cleanly and drain the stderr reader.
    stop_serve(child);
    let _ = reader_thread.join();

    let logs = captured_stderr.lock().unwrap();
    assert!(
        logs.contains("[send] accepted: peer_uid="),
        "daemon should log peer_uid for accepted UDS sends; logs:\n{logs}"
    );
    assert!(
        logs.contains("peer_pid="),
        "daemon should log peer_pid for accepted UDS sends; logs:\n{logs}"
    );
}

#[cfg(unix)]
fn drop_payload(path: &Path) -> Vec<u8> {
    let signed = std::fs::read(path).expect("mail drop file missing");
    let sentinel = b"----- AIMX TEST DROP -----\n";
    assert!(signed.starts_with(sentinel));
    signed[sentinel.len()..].to_vec()
}

/// Bare Markdown send via the CLI client → daemon assembles
/// multipart/alternative on the wire (text part = Markdown source verbatim,
/// HTML part = rendered HTML), DKIM body-hash verifies against the new
/// shape, and the sent record persists the Markdown source as the body
/// (no `.html` sibling).
#[cfg(unix)]
#[test]
fn send_uds_end_to_end_emits_multipart_alternative_for_markdown_body() {
    let tmp = TempDir::new().unwrap();
    setup_test_env(tmp.path());
    let port = find_free_port();

    let mail_drop = tmp.path().join("outbound.log");
    let (child, _sock) = start_serve_with_mail_drop(tmp.path(), port, &mail_drop);

    let runtime = tmp.path().join("run");
    let output = Command::cargo_bin("aimx")
        .unwrap()
        .env("AIMX_CONFIG_DIR", tmp.path())
        .env("AIMX_RUNTIME_DIR", &runtime)
        .arg("--data-dir")
        .arg(tmp.path())
        .arg("send")
        .arg("--from")
        .arg("alice@agent.example.com")
        .arg("--to")
        .arg("recipient@example.com")
        .arg("--subject")
        .arg("Markdown render check")
        .arg("--body")
        .arg("# Hello\n\nWorld")
        .output()
        .expect("aimx send failed to run");

    assert!(
        output.status.success(),
        "aimx send should exit 0: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    std::thread::sleep(std::time::Duration::from_millis(200));

    let payload = drop_payload(&mail_drop);
    let payload_str = String::from_utf8_lossy(&payload);

    // Wire shape: multipart/alternative with text + html parts.
    assert!(
        payload_str.contains("Content-Type: multipart/alternative"),
        "wire must be multipart/alternative for the default Markdown path; got:\n{payload_str}"
    );
    assert!(
        payload_str.contains("Content-Type: text/plain"),
        "text/plain part missing"
    );
    assert!(
        payload_str.contains("Content-Type: text/html"),
        "text/html part missing"
    );
    // The text part contains the Markdown source verbatim (modulo
    // CRLF normalization and the appended default signature).
    assert!(
        payload_str.contains("# Hello"),
        "text part must carry the Markdown source verbatim:\n{payload_str}"
    );
    // The HTML part contains rendered HTML for `# Hello` and `World`.
    assert!(
        payload_str.contains("<h1") && payload_str.contains(">Hello</h1>"),
        "html part missing rendered <h1>Hello</h1>:\n{payload_str}"
    );
    assert!(
        payload_str.contains(">World</p>") || payload_str.contains("World"),
        "html part must include the body paragraph"
    );

    // DKIM body-hash verification using the same relaxed-canonicalization
    // helper as the legacy plain-text end-to-end test. The new multipart
    // shape must still verify byte-for-byte.
    let signed_header = extract_dkim_bh(&payload);
    let computed = compute_relaxed_body_hash(&payload);
    assert_eq!(
        signed_header, computed,
        "DKIM body hash must verify on multipart/alternative wire: signed={signed_header}, computed={computed}"
    );

    // The sent record exists at sent/alice/<stem>.md and its body
    // contains the Markdown source verbatim (no `.html` sibling, no
    // rendered HTML in the persisted body).
    let sent_dir = tmp.path().join("sent").join("alice");
    let entries: Vec<_> = std::fs::read_dir(&sent_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .collect();
    assert_eq!(entries.len(), 1, "exactly one sent record expected");
    let sent_path = entries[0].path();
    let sent_content = std::fs::read_to_string(&sent_path).unwrap();
    // Body equals the Markdown source verbatim.
    assert!(
        sent_content.contains("# Hello"),
        "sent record body should preserve the Markdown source verbatim"
    );
    // The audit-trail field declares the wire shape: default Markdown
    // path stamps `outbound_format = "markdown"`.
    assert!(
        sent_content.contains("outbound_format = \"markdown\""),
        "sent record must declare outbound_format = \"markdown\":\n{sent_content}"
    );
    // No `.html` sibling appears.
    let any_html_sibling = std::fs::read_dir(&sent_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .any(|e| {
            e.path()
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("html"))
        });
    assert!(
        !any_html_sibling,
        "no .html sibling file should be written next to the sent record"
    );

    stop_serve(child);
}

/// Markdown send with one attachment must emit a nested
/// multipart/mixed wrapping multipart/alternative, AND the resulting
/// DKIM-Signature must still verify against the canonical body bytes.
#[cfg(unix)]
#[test]
fn send_uds_end_to_end_dkim_verifies_with_attachment() {
    let tmp = TempDir::new().unwrap();
    setup_test_env(tmp.path());
    let port = find_free_port();

    let attachment = tmp.path().join("note.txt");
    std::fs::write(&attachment, b"attached payload").unwrap();

    let mail_drop = tmp.path().join("outbound.log");
    let (child, _sock) = start_serve_with_mail_drop(tmp.path(), port, &mail_drop);

    let runtime = tmp.path().join("run");
    let output = Command::cargo_bin("aimx")
        .unwrap()
        .env("AIMX_CONFIG_DIR", tmp.path())
        .env("AIMX_RUNTIME_DIR", &runtime)
        .arg("--data-dir")
        .arg(tmp.path())
        .arg("send")
        .arg("--from")
        .arg("alice@agent.example.com")
        .arg("--to")
        .arg("recipient@example.com")
        .arg("--subject")
        .arg("Markdown plus attachment")
        .arg("--body")
        .arg("## Report\n\nDetails inline.")
        .arg("--attachment")
        .arg(&attachment)
        .output()
        .expect("aimx send failed to run");

    assert!(
        output.status.success(),
        "aimx send with attachment should exit 0: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    std::thread::sleep(std::time::Duration::from_millis(200));

    let payload = drop_payload(&mail_drop);
    let payload_str = String::from_utf8_lossy(&payload);

    assert!(
        payload_str.contains("Content-Type: multipart/mixed"),
        "outer Content-Type must be multipart/mixed when attachments are present:\n{payload_str}"
    );
    assert!(
        payload_str.contains("Content-Type: multipart/alternative"),
        "inner Content-Type must be multipart/alternative:\n{payload_str}"
    );
    assert!(
        payload_str.contains("filename=\"note.txt\""),
        "attachment must survive daemon-side reassembly"
    );
    assert!(
        payload_str.contains("<h2") && payload_str.contains(">Report</h2>"),
        "html part missing rendered heading"
    );

    // DKIM body-hash must verify against the new nested wire shape.
    let signed_header = extract_dkim_bh(&payload);
    let computed = compute_relaxed_body_hash(&payload);
    assert_eq!(
        signed_header, computed,
        "DKIM body hash must verify on nested mixed+alternative wire: signed={signed_header}, computed={computed}"
    );

    stop_serve(child);
}

/// `aimx send --text-only` end-to-end:
/// - captured wire is single-part `text/plain`, body verbatim, no
///   multipart structure, no rendered HTML.
/// - the sent record stores the body verbatim.
/// - DKIM body-hash verifies against the single-part wire shape.
#[cfg(unix)]
#[test]
fn send_uds_end_to_end_text_only_emits_single_part_text_plain() {
    let tmp = TempDir::new().unwrap();
    setup_test_env(tmp.path());
    let port = find_free_port();

    let mail_drop = tmp.path().join("outbound.log");
    let (child, _sock) = start_serve_with_mail_drop(tmp.path(), port, &mail_drop);

    let runtime = tmp.path().join("run");
    let output = Command::cargo_bin("aimx")
        .unwrap()
        .env("AIMX_CONFIG_DIR", tmp.path())
        .env("AIMX_RUNTIME_DIR", &runtime)
        .arg("--data-dir")
        .arg(tmp.path())
        .arg("send")
        .arg("--from")
        .arg("alice@agent.example.com")
        .arg("--to")
        .arg("recipient@example.com")
        .arg("--subject")
        .arg("OTP delivery")
        .arg("--body")
        .arg("Your code: 9999")
        .arg("--text-only")
        .output()
        .expect("aimx send failed to run");

    assert!(
        output.status.success(),
        "aimx send --text-only should exit 0: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    std::thread::sleep(std::time::Duration::from_millis(200));

    let payload = drop_payload(&mail_drop);
    let payload_str = String::from_utf8_lossy(&payload);

    assert!(
        payload_str.contains("Content-Type: text/plain"),
        "text-only wire must carry text/plain: {payload_str}"
    );
    assert!(
        !payload_str.contains("multipart/"),
        "text-only wire must be single-part: {payload_str}"
    );
    assert!(
        !payload_str.contains("Content-Type: text/html"),
        "text-only wire must not include a text/html part: {payload_str}"
    );
    assert!(
        payload_str.contains("Your code: 9999"),
        "body must reach the wire verbatim: {payload_str}"
    );
    // No default signature appended (operator-supplied content).
    assert!(
        !payload_str.contains("Sent from AIMX."),
        "text-only path must not append the default signature: {payload_str}"
    );

    // DKIM body-hash must verify against the single-part wire shape.
    let signed_header = extract_dkim_bh(&payload);
    let computed = compute_relaxed_body_hash(&payload);
    assert_eq!(
        signed_header, computed,
        "DKIM body hash must verify on text-only wire: signed={signed_header}, computed={computed}"
    );

    // Sent record exists and carries the body verbatim. The audit
    // trail declares `outbound_format = "text"` so an operator
    // browsing `sent/` can tell at a glance the recipient saw plain
    // text — distinct from a default Markdown send (`"markdown"`).
    let sent_dir = tmp.path().join("sent").join("alice");
    let entries: Vec<_> = std::fs::read_dir(&sent_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .collect();
    assert_eq!(entries.len(), 1, "exactly one sent record");
    let sent_content = std::fs::read_to_string(entries[0].path()).unwrap();
    assert!(
        sent_content.contains("Your code: 9999"),
        "sent record body must carry the plain text verbatim"
    );
    assert!(
        sent_content.contains("outbound_format = \"text\""),
        "sent record must declare outbound_format = \"text\" on --text-only path:\n{sent_content}"
    );

    stop_serve(child);
}

/// `aimx send --html-body` end-to-end:
/// - captured wire is `multipart/alternative` with the supplied HTML
///   in the text/html part verbatim (not rendered by comrak).
/// - the sent record stores the `--body` text part (the custom HTML
///   is NOT persisted — the operator's template is the source of truth).
/// - DKIM body-hash verifies against the alternative wire shape.
#[cfg(unix)]
#[test]
fn send_uds_end_to_end_html_body_uses_supplied_html_verbatim() {
    let tmp = TempDir::new().unwrap();
    setup_test_env(tmp.path());
    let port = find_free_port();

    let mail_drop = tmp.path().join("outbound.log");
    let (child, _sock) = start_serve_with_mail_drop(tmp.path(), port, &mail_drop);

    let runtime = tmp.path().join("run");
    let output = Command::cargo_bin("aimx")
        .unwrap()
        .env("AIMX_CONFIG_DIR", tmp.path())
        .env("AIMX_RUNTIME_DIR", &runtime)
        .arg("--data-dir")
        .arg(tmp.path())
        .arg("send")
        .arg("--from")
        .arg("alice@agent.example.com")
        .arg("--to")
        .arg("recipient@example.com")
        .arg("--subject")
        .arg("Custom HTML layout")
        .arg("--body")
        .arg("fallback text")
        .arg("--html-body")
        .arg("<p>custom <b>html</b></p>")
        .output()
        .expect("aimx send failed to run");

    assert!(
        output.status.success(),
        "aimx send --html-body should exit 0: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    std::thread::sleep(std::time::Duration::from_millis(200));

    let payload = drop_payload(&mail_drop);
    let payload_str = String::from_utf8_lossy(&payload);

    assert!(
        payload_str.contains("Content-Type: multipart/alternative"),
        "html-body wire must be multipart/alternative: {payload_str}"
    );
    assert!(
        payload_str.contains("Content-Type: text/plain"),
        "text part missing"
    );
    assert!(
        payload_str.contains("Content-Type: text/html"),
        "text/html part missing"
    );
    // Text part: the --body text appears verbatim.
    assert!(
        payload_str.contains("fallback text"),
        "text part must carry --body verbatim: {payload_str}"
    );
    // HTML part: the operator's HTML is verbatim — no inline `style="`
    // attributes the renderer would have added.
    let html_idx = payload_str
        .find("Content-Type: text/html")
        .expect("text/html part missing");
    let html_section = &payload_str[html_idx..];
    assert!(
        html_section.contains("<p>custom <b>html</b></p>"),
        "html part must carry --html-body verbatim: {html_section}"
    );
    assert!(
        !html_section.contains("style=\""),
        "renderer must not be invoked on --html-body path: {html_section}"
    );
    // No default signature appended on the html-body branch.
    assert!(
        !payload_str.contains("Sent from AIMX."),
        "html-body path must not append the default signature: {payload_str}"
    );

    // DKIM body-hash verifies on the alternative wire shape.
    let signed_header = extract_dkim_bh(&payload);
    let computed = compute_relaxed_body_hash(&payload);
    assert_eq!(
        signed_header, computed,
        "DKIM body hash must verify on html-body alternative wire: signed={signed_header}, computed={computed}"
    );

    // Sent record stores the --body text, not the --html-body content.
    // The "custom HTML is not stored" invariant is verified at the
    // integration layer here.
    let sent_dir = tmp.path().join("sent").join("alice");
    let entries: Vec<_> = std::fs::read_dir(&sent_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .collect();
    assert_eq!(entries.len(), 1, "exactly one sent record");
    let sent_path = entries[0].path();
    let sent_content = std::fs::read_to_string(&sent_path).unwrap();
    assert!(
        sent_content.contains("fallback text"),
        "sent record body must carry the --body text"
    );
    // Audit-trail field declares the wire shape: `--html-body` path
    // stamps `outbound_format = "html"` so an operator browsing
    // `sent/` knows the recipient saw an operator-supplied HTML
    // template (and that re-rendering the stored Markdown body would
    // NOT reproduce the recipient's view).
    assert!(
        sent_content.contains("outbound_format = \"html\""),
        "sent record must declare outbound_format = \"html\" on --html-body path:\n{sent_content}"
    );

    // No `.html` sibling appears next to the sent record.
    let any_html_sibling = std::fs::read_dir(&sent_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .any(|e| {
            e.path()
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("html"))
        });
    assert!(
        !any_html_sibling,
        "no .html sibling file should be written next to the sent record"
    );

    stop_serve(child);
}

/// MCP `email_send` with `text_only=true` ships a single-part
/// `text/plain` message verbatim — no Markdown rendering, no
/// multipart wrapper. End-to-end: MCP stdio client → daemon UDS →
/// file-drop transport → captured wire bytes. Mirrors the CLI-side
/// `send_uds_end_to_end_text_only_emits_single_part_text_plain` so a
/// regression in either layer surfaces independently.
#[cfg(unix)]
#[test]
fn mcp_email_send_text_only_emits_single_part_text_plain() {
    let tmp = TempDir::new().unwrap();
    setup_test_env(tmp.path());
    let port = find_free_port();
    let mail_drop = tmp.path().join("outbound.log");
    let (child, _sock) = start_serve_with_mail_drop(tmp.path(), port, &mail_drop);

    let mut client = McpClient::spawn(tmp.path());
    client.initialize();

    let resp = client.call_tool(
        "email_send",
        serde_json::json!({
            "from_mailbox": "alice",
            "to": "recipient@example.com",
            "subject": "OTP delivery",
            "body": "Your code: 9999",
            "text_only": true,
        }),
    );
    assert!(!is_tool_error(&resp), "email_send should succeed: {resp:?}");

    std::thread::sleep(std::time::Duration::from_millis(200));

    let payload = drop_payload(&mail_drop);
    let payload_str = String::from_utf8_lossy(&payload);
    assert!(
        payload_str.contains("Content-Type: text/plain"),
        "MCP text_only must produce text/plain wire: {payload_str}"
    );
    assert!(
        !payload_str.contains("multipart/"),
        "MCP text_only must produce single-part wire: {payload_str}"
    );
    assert!(
        !payload_str.contains("Content-Type: text/html"),
        "MCP text_only must not produce a text/html part: {payload_str}"
    );
    assert!(
        payload_str.contains("Your code: 9999"),
        "body must reach the wire verbatim: {payload_str}"
    );

    client.shutdown();
    stop_serve(child);
}

/// MCP `email_send` with `html_body` produces a `multipart/alternative`
/// where the operator's HTML appears in the `text/html` part verbatim
/// (no inline `style="..."` attributes the renderer would have added).
/// The custom HTML is NOT persisted to the sent record — only the
/// `body` text part is stored; the operator's template is the source
/// of truth elsewhere.
#[cfg(unix)]
#[test]
fn mcp_email_send_html_body_uses_supplied_html_verbatim() {
    let tmp = TempDir::new().unwrap();
    setup_test_env(tmp.path());
    let port = find_free_port();
    let mail_drop = tmp.path().join("outbound.log");
    let (child, _sock) = start_serve_with_mail_drop(tmp.path(), port, &mail_drop);

    let mut client = McpClient::spawn(tmp.path());
    client.initialize();

    let resp = client.call_tool(
        "email_send",
        serde_json::json!({
            "from_mailbox": "alice",
            "to": "recipient@example.com",
            "subject": "Custom HTML layout",
            "body": "fallback text",
            "html_body": "<p>custom <b>html</b></p>",
        }),
    );
    assert!(!is_tool_error(&resp), "email_send should succeed: {resp:?}");

    std::thread::sleep(std::time::Duration::from_millis(200));

    let payload = drop_payload(&mail_drop);
    let payload_str = String::from_utf8_lossy(&payload);
    assert!(
        payload_str.contains("Content-Type: multipart/alternative"),
        "html_body wire must be multipart/alternative: {payload_str}"
    );
    assert!(
        payload_str.contains("Content-Type: text/plain"),
        "text part missing"
    );
    assert!(
        payload_str.contains("Content-Type: text/html"),
        "text/html part missing"
    );
    assert!(
        payload_str.contains("fallback text"),
        "text part must carry body verbatim: {payload_str}"
    );
    let html_idx = payload_str
        .find("Content-Type: text/html")
        .expect("text/html part missing");
    let html_section = &payload_str[html_idx..];
    assert!(
        html_section.contains("<p>custom <b>html</b></p>"),
        "html part must carry html_body verbatim: {html_section}"
    );
    assert!(
        !html_section.contains("style=\""),
        "renderer must not be invoked on html_body MCP path: {html_section}"
    );

    client.shutdown();
    stop_serve(child);
}

/// MCP `email_send` with both `text_only=true` and `html_body` set is
/// rejected server-side before the UDS is opened. The error wording
/// matches the codec's canonical message so operators see the same
/// string regardless of which layer (clap, MCP, codec) fired the check.
#[cfg(unix)]
#[test]
fn mcp_email_send_text_only_plus_html_body_rejected() {
    let tmp = TempDir::new().unwrap();
    setup_test_env(tmp.path());
    let port = find_free_port();
    let daemon = start_serve(tmp.path(), port);
    let sock = tmp.path().join("run").join("aimx.sock");
    assert!(
        wait_for_socket(&sock, std::time::Duration::from_secs(5)),
        "UDS socket never appeared"
    );

    let mut client = McpClient::spawn(tmp.path());
    client.initialize();

    let resp = client.call_tool(
        "email_send",
        serde_json::json!({
            "from_mailbox": "alice",
            "to": "recipient@example.com",
            "subject": "conflict",
            "body": "fallback",
            "text_only": true,
            "html_body": "<p>x</p>",
        }),
    );
    assert!(
        is_tool_error(&resp),
        "MCP must reject text_only + html_body before opening UDS: {resp:?}"
    );
    let text = get_tool_text(&resp);
    assert!(
        text.contains("--text-only") && text.contains("--html-body"),
        "MCP rejection must name both flags (matching the codec wording): {text}"
    );
    assert!(
        text.contains("mutually exclusive"),
        "MCP rejection must use the canonical 'mutually exclusive' wording: {text}"
    );

    client.shutdown();
    stop_serve(daemon);
}

/// MCP `email_reply` with both `text_only=true` and `html_body` set is
/// rejected server-side with the same canonical wording as
/// `email_send`. The reply path has its own validation call site so a
/// regression on one tool does not silently fix itself by leaning on
/// the other tool's check.
#[cfg(unix)]
#[test]
fn mcp_email_reply_text_only_plus_html_body_rejected() {
    let tmp = TempDir::new().unwrap();
    setup_test_env(tmp.path());

    let port = find_free_port();
    let daemon = start_serve(tmp.path(), port);
    let sock = tmp.path().join("run").join("aimx.sock");
    assert!(
        wait_for_socket(&sock, std::time::Duration::from_secs(5)),
        "UDS socket never appeared"
    );

    let mut client = McpClient::spawn(tmp.path());
    client.initialize();

    // The id and mailbox need not exist — mutual-exclusion is the
    // very first check in `email_reply`, ahead of `validate_email_id`
    // and `lookup_mailbox_row`. The "mutually exclusive" wording in
    // the response proves the check fired first (no UDS open, no
    // mailbox lookup, no parent-message read).
    let resp = client.call_tool(
        "email_reply",
        serde_json::json!({
            "mailbox": "alice",
            "id": "2099-12-31-235959-nope",
            "body": "fallback",
            "text_only": true,
            "html_body": "<p>x</p>",
        }),
    );
    assert!(
        is_tool_error(&resp),
        "email_reply must reject text_only + html_body: {resp:?}"
    );
    let text = get_tool_text(&resp);
    assert!(
        text.contains("mutually exclusive"),
        "reply rejection must use the canonical 'mutually exclusive' wording: {text}"
    );

    client.shutdown();
    stop_serve(daemon);
}

/// `aimx send --text-only --html-body ...` is rejected at the CLI
/// layer by clap — the daemon never sees the request and the operator
/// gets the conflict explained immediately.
#[cfg(unix)]
#[test]
fn send_text_only_and_html_body_combination_rejected_at_cli() {
    let tmp = TempDir::new().unwrap();
    setup_test_env(tmp.path());

    let runtime = tmp.path().join("run");
    let output = Command::cargo_bin("aimx")
        .unwrap()
        .env("AIMX_CONFIG_DIR", tmp.path())
        .env("AIMX_RUNTIME_DIR", &runtime)
        .arg("--data-dir")
        .arg(tmp.path())
        .arg("send")
        .arg("--from")
        .arg("alice@agent.example.com")
        .arg("--to")
        .arg("recipient@example.com")
        .arg("--subject")
        .arg("conflict")
        .arg("--body")
        .arg("fallback")
        .arg("--text-only")
        .arg("--html-body")
        .arg("<p>x</p>")
        .output()
        .expect("aimx send failed to launch");

    assert!(
        !output.status.success(),
        "clap must reject --text-only + --html-body before any UDS round-trip"
    );
    let stderr_text = String::from_utf8_lossy(&output.stderr);
    assert!(
        (stderr_text.contains("--text-only") || stderr_text.contains("text_only"))
            && (stderr_text.contains("--html-body") || stderr_text.contains("html_body")),
        "clap error must name both flags: {stderr_text}"
    );
}

/// End-to-end `after_send` hook test. Replaces the default
/// `setup_test_env` config with a mailbox that carries an `after_send` hook
/// writing a sentinel file containing `$AIMX_SEND_STATUS`. After a send
/// round-trip the sentinel must exist and carry `delivered`.
#[cfg(unix)]
#[test]
fn after_send_hook_fires_with_delivered_status() {
    let tmp = TempDir::new().unwrap();
    setup_test_env(tmp.path());

    // Overwrite config.toml with an after_send hook on `alice`.
    let sentinel = tmp.path().join("after_send.sentinel");
    let owner = current_username();
    let config = format!(
        r#"domain = "agent.example.com"
data_dir = "{data_dir}"

[mailboxes.catchall]
address = "*@agent.example.com"
owner = "aimx-catchall"

[mailboxes.alice]
address = "alice@agent.example.com"
owner = "{owner}"

[[mailboxes.alice.hooks]]
name = "aftersendhk1"
event = "after_send"
cmd = ["/bin/sh", "-c", 'printf "status=%s to=%s\n" "$AIMX_SEND_STATUS" "$AIMX_TO" > {sentinel}']
"#,
        data_dir = tmp.path().display(),
        sentinel = sentinel.display(),
    );
    std::fs::write(tmp.path().join("config.toml"), &config).unwrap();

    let port = find_free_port();
    let mail_drop = tmp.path().join("outbound.log");
    let (child, _sock) = start_serve_with_mail_drop(tmp.path(), port, &mail_drop);

    let runtime = tmp.path().join("run");
    let output = Command::cargo_bin("aimx")
        .unwrap()
        .env("AIMX_CONFIG_DIR", tmp.path())
        .env("AIMX_RUNTIME_DIR", &runtime)
        .arg("--data-dir")
        .arg(tmp.path())
        .arg("send")
        .arg("--from")
        .arg("alice@agent.example.com")
        .arg("--to")
        .arg("recipient@example.com")
        .arg("--subject")
        .arg("hook test")
        .arg("--body")
        .arg("body")
        .output()
        .expect("aimx send failed to run");
    assert!(output.status.success(), "aimx send should succeed");

    stop_serve(child);

    // Daemon awaits the subprocess before replying, so the sentinel is
    // already written by the time `aimx send` returns. Read directly.
    assert!(
        sentinel.exists(),
        "after_send sentinel should exist at {}",
        sentinel.display()
    );
    let content = std::fs::read_to_string(&sentinel).unwrap();
    assert!(
        content.contains("status=delivered"),
        "AIMX_SEND_STATUS should be 'delivered'; got: {content}"
    );
    assert!(
        content.contains("to=recipient@example.com"),
        "AIMX_TO should be the recipient; got: {content}"
    );
}

#[test]
fn serve_e2e_stale_readme_refreshed_at_startup() {
    let tmp = TempDir::new().unwrap();
    setup_test_env(tmp.path());
    let port = find_free_port();

    // Plant a stale README with an outdated version comment.
    let readme_path = tmp.path().join("README.md");
    std::fs::write(
        &readme_path,
        "<!-- aimx-readme-version: 0 -->\nstale content\n",
    )
    .unwrap();
    let before = std::fs::read_to_string(&readme_path).unwrap();
    assert!(before.contains("stale content"));

    let child = start_serve(tmp.path(), port);

    // By the time start_serve returns the TCP listener is bound, which is
    // *after* refresh_if_outdated runs in serve startup.  The README should
    // now contain the current template, not the stale content.
    let after = std::fs::read_to_string(&readme_path).unwrap();
    assert!(
        after.starts_with("<!-- aimx-readme-version: 7 -->"),
        "README should start with current version comment after serve startup; got: {}",
        after.lines().next().unwrap_or("<empty>")
    );
    assert!(
        !after.contains("stale content"),
        "stale content should be replaced after serve startup"
    );

    stop_serve(child);
}

#[cfg(unix)]
#[test]
fn send_without_daemon_reports_missing_socket() {
    let tmp = TempDir::new().unwrap();
    setup_test_env(tmp.path());

    let runtime = tmp.path().join("run");
    std::fs::create_dir_all(&runtime).ok();
    // No `aimx serve` spawned; the UDS will not exist.

    let output = Command::cargo_bin("aimx")
        .unwrap()
        .env("AIMX_CONFIG_DIR", tmp.path())
        .env("AIMX_RUNTIME_DIR", &runtime)
        .arg("--data-dir")
        .arg(tmp.path())
        .arg("send")
        .arg("--from")
        .arg("alice@agent.example.com")
        .arg("--to")
        .arg("recipient@example.com")
        .arg("--subject")
        .arg("nope")
        .arg("--body")
        .arg("nope")
        .output()
        .expect("aimx send failed to run");

    assert!(
        !output.status.success(),
        "aimx send must fail when daemon is not running"
    );
    assert_eq!(
        output.status.code(),
        Some(2),
        "missing-socket exit code must be 2"
    );
    let stderr_out = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr_out.contains("aimx daemon not running"),
        "stderr must carry the daemon-not-running message; got: {stderr_out}"
    );
}

// ---------------------------------------------------------------------------
// MCP write ops via daemon.
//
// `email_mark_read` / `email_mark_unread` route through `aimx serve` over
// the UDS. These tests spawn the daemon + MCP as sibling subprocesses and
// exercise concurrency: an inbound SMTP delivery racing an MCP MARK-READ
// call on the same mailbox must leave both files intact.
// ---------------------------------------------------------------------------

#[cfg(unix)]
#[test]
fn mcp_mark_read_concurrent_with_inbound_ingest() {
    let tmp = TempDir::new().unwrap();
    setup_test_env(tmp.path());

    // Pre-seed one email so MARK-READ has a target.
    let alice_dir = inbox(tmp.path(), "alice");
    create_email_file(
        &alice_dir,
        "2025-06-01-001",
        "sender@example.com",
        "Hello",
        false,
    );

    let port = find_free_port();
    let daemon = start_serve(tmp.path(), port);

    let runtime = tmp.path().join("run");
    let sock = runtime.join("aimx.sock");
    assert!(
        wait_for_socket(&sock, std::time::Duration::from_secs(5)),
        "UDS send socket never appeared"
    );

    // Kick off an inbound SMTP transaction in parallel with the MARK-READ.
    let smtp_handle = std::thread::spawn(move || {
        use std::io::{Read, Write};
        use std::net::TcpStream;
        let mut stream = TcpStream::connect(format!("127.0.0.1:{port}")).unwrap();
        stream
            .set_read_timeout(Some(std::time::Duration::from_secs(5)))
            .unwrap();

        let mut reader = std::io::BufReader::new(stream.try_clone().unwrap());
        let mut line = String::new();
        reader.read_line(&mut line).unwrap(); // 220 greeting

        let send = |stream: &mut TcpStream, s: &str| {
            stream.write_all(s.as_bytes()).unwrap();
        };
        let readl = |reader: &mut std::io::BufReader<TcpStream>| {
            let mut l = String::new();
            reader.read_line(&mut l).unwrap();
            l
        };

        send(&mut stream, "EHLO test\r\n");
        // Drain multi-line EHLO response.
        loop {
            let l = readl(&mut reader);
            if l.len() > 3 && l.as_bytes()[3] != b'-' {
                break;
            }
        }
        send(&mut stream, "MAIL FROM:<other@example.com>\r\n");
        readl(&mut reader);
        send(&mut stream, "RCPT TO:<alice@agent.example.com>\r\n");
        readl(&mut reader);
        send(&mut stream, "DATA\r\n");
        readl(&mut reader);
        let msg = concat!(
            "From: other@example.com\r\n",
            "To: alice@agent.example.com\r\n",
            "Subject: Concurrent ingest\r\n",
            "Date: Mon, 01 Jan 2024 00:00:00 +0000\r\n",
            "Message-ID: <concurrent-ingest@example.com>\r\n",
            "\r\n",
            "inbound body\r\n",
            ".\r\n",
        );
        send(&mut stream, msg);
        readl(&mut reader);
        send(&mut stream, "QUIT\r\n");
        let _ = readl(&mut reader);
        // Drain remaining bytes
        let mut buf = Vec::new();
        let _ = reader.get_mut().read_to_end(&mut buf);
    });

    // Fire the MARK-READ via MCP concurrently.
    let mark_handle = {
        let tmp_path = tmp.path().to_path_buf();
        let runtime = runtime.clone();
        std::thread::spawn(move || {
            let mut child = StdCommand::new(aimx_binary_path())
                .env("AIMX_CONFIG_DIR", &tmp_path)
                .env("AIMX_RUNTIME_DIR", &runtime)
                .arg("--data-dir")
                .arg(&tmp_path)
                .arg("mcp")
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .expect("Failed to spawn aimx mcp");
            let stdin = child.stdin.take().unwrap();
            let stdout = child.stdout.take().unwrap();
            let stderr = child.stderr.take().unwrap();
            let (stderr_buf, stderr_drain) = spawn_stderr_drain(stderr);
            let mut client = McpClient {
                child,
                stdin,
                reader: BufReader::new(stdout),
                id: 0,
                stderr_buf,
                stderr_drain: Some(stderr_drain),
            };
            client.initialize();
            let resp = client.call_tool(
                "email_mark_read",
                serde_json::json!({"mailbox": "alice", "id": "2025-06-01-001"}),
            );
            let text = get_tool_text(&resp);
            assert!(
                text.contains("marked as read"),
                "expected 'marked as read' in MCP response; got: {text}"
            );
            client.shutdown();
        })
    };

    smtp_handle.join().unwrap();
    mark_handle.join().unwrap();

    // Verify both files exist and are internally consistent.
    let seed_content =
        std::fs::read_to_string(alice_dir.join("2025-06-01-001.md")).expect("seed email readable");
    assert!(
        seed_content.contains("read = true"),
        "seed email should have been marked read: {seed_content}"
    );
    assert!(
        seed_content.starts_with("+++"),
        "seed email must retain valid frontmatter delimiters"
    );

    // Inbound ingest should have produced a second .md file in the same
    // mailbox. Find it and assert its frontmatter parses cleanly.
    let entries: Vec<_> = find_md_files(&alice_dir);
    assert!(
        entries.len() >= 2,
        "expected >=2 .md files after concurrent ingest + MARK-READ, got {}",
        entries.len()
    );
    for md in &entries {
        let content = std::fs::read_to_string(md).unwrap();
        assert!(
            content.starts_with("+++"),
            "every .md must retain valid frontmatter delimiters after concurrent access: {}",
            md.display()
        );
        let fm = read_frontmatter(md);
        let _ = fm.as_table().expect("frontmatter must parse to table");
    }

    stop_serve(daemon);
}

// ---------------------------------------------------------------------------
// Mailbox CRUD via UDS (daemon hot-swaps Arc<Config>). These integration
// tests exercise the end-to-end flow:
//   - `aimx mailbox create foo` against a running daemon → inbound SMTP to
//     `foo@<domain>` routes to `inbox/foo/`, not catchall, with no restart.
//   - `aimx mailbox create foo` against a stopped daemon → falls back to
//     direct on-disk edit + prints the restart-hint banner.
//   - `aimx mailbox delete foo` refuses when the mailbox still has files,
//     then succeeds after the files are removed.
// ---------------------------------------------------------------------------

#[cfg(unix)]
#[test]
#[ignore = "requires root; MAILBOX-CRUD is root-only; run via the CI mailbox-crud-root step or AIMX_INTEGRATION_SUDO=1 sudo"]
fn mailbox_create_via_uds_hotswaps_config_and_routes_new_mail() {
    if skip_if_mailbox_crud_not_root() {
        return;
    }
    let tmp = TempDir::new().unwrap();
    setup_test_env(tmp.path());

    let port = find_free_port();
    let daemon = start_serve(tmp.path(), port);
    let sock = tmp.path().join("run").join("aimx.sock");
    assert!(
        wait_for_socket(&sock, std::time::Duration::from_secs(5)),
        "UDS send socket never appeared"
    );

    // Create a fresh mailbox via the CLI, which should route through UDS
    // and succeed without printing the restart hint.
    let assert = aimx_cmd(tmp.path())
        .env("AIMX_RUNTIME_DIR", tmp.path().join("run"))
        .arg("--data-dir")
        .arg(tmp.path())
        .arg("mailbox")
        .arg("create")
        .arg("eve")
        .arg("--owner")
        .arg(current_username())
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout).to_string();
    assert!(
        stdout.contains("Mailbox 'eve' created"),
        "expected success message, got: {stdout}"
    );
    assert!(
        !stdout.contains("Restart the daemon"),
        "UDS path must suppress the restart-hint banner: {stdout}"
    );

    // The daemon should already see the new mailbox in its in-memory
    // Config. Send a fresh inbound SMTP message addressed to
    // `eve@agent.example.com` and verify it lands in `inbox/eve/` rather
    // than in catchall.
    let email = concat!(
        "From: sender@example.com\r\n",
        "To: eve@agent.example.com\r\n",
        "Subject: Hi Eve\r\n",
        "Date: Mon, 01 Jan 2024 00:00:00 +0000\r\n",
        "Message-ID: <eve-hot-swap@example.com>\r\n",
        "\r\n",
        "hello eve\r\n",
    );
    smtp_send_email(
        port,
        "sender@example.com",
        &["eve@agent.example.com"],
        email,
    );
    std::thread::sleep(std::time::Duration::from_millis(500));

    let eve_dir = inbox(tmp.path(), "eve");
    let md_files = find_md_files(&eve_dir);
    assert!(
        !md_files.is_empty(),
        "new mailbox 'eve' must receive the inbound message without restart"
    );

    // Catchall must be empty (aside from any pre-existing content from
    // setup_test_env, which creates the dir but not any messages).
    let catchall = inbox(tmp.path(), "catchall");
    let catchall_md = find_md_files(&catchall);
    assert!(
        catchall_md.is_empty(),
        "catchall must not receive eve's mail once the live-swap applied: \
         catchall contents = {catchall_md:?}"
    );

    // config.toml on disk reflects the new mailbox stanza.
    let config_text = std::fs::read_to_string(tmp.path().join("config.toml")).unwrap();
    assert!(
        config_text.contains("[mailboxes.eve]"),
        "config.toml should contain the new stanza: {config_text}"
    );

    stop_serve(daemon);
}

/// Non-root + missing socket: per S2-1 the CLI must NOT silently fall
/// back to the direct config.toml edit (which would fail with a
/// confusing perm error). It exits with the dedicated socket-missing
/// code (`EXIT_SOCKET_MISSING = 2`) and prints the actionable hint
/// naming both remediations.
#[cfg(unix)]
#[test]
fn mailbox_create_without_daemon_non_root_exits_with_socket_missing_hint() {
    if unsafe { libc::geteuid() } == 0 {
        eprintln!(
            "skipping: this test exercises the non-root socket-missing branch; \
             run as a non-root uid"
        );
        return;
    }
    let tmp = TempDir::new().unwrap();
    setup_test_env(tmp.path());

    let runtime = tmp.path().join("run");
    std::fs::create_dir_all(&runtime).ok();

    let assert = aimx_cmd(tmp.path())
        .env("AIMX_RUNTIME_DIR", &runtime)
        .arg("--data-dir")
        .arg(tmp.path())
        .arg("mailbox")
        .arg("create")
        .arg("eve")
        .arg("--owner")
        .arg(current_username())
        .assert()
        .code(2);
    let stderr = String::from_utf8_lossy(&assert.get_output().stderr).to_string();
    assert!(
        stderr.contains("daemon must be running"),
        "expected socket-missing hint on stderr, got: {stderr}"
    );
    assert!(
        stderr.contains("aimx serve") && stderr.contains("sudo"),
        "hint must name both remediations (start daemon / use sudo): {stderr}"
    );

    // No fallback wrote the stanza.
    let config_text = std::fs::read_to_string(tmp.path().join("config.toml")).unwrap();
    assert!(
        !config_text.contains("[mailboxes.eve]"),
        "non-root socket-missing path must NOT fall back to a direct write: {config_text}"
    );
}

#[cfg(unix)]
#[test]
#[ignore = "requires root; MAILBOX-CRUD is root-only; run via the CI mailbox-crud-root step or AIMX_INTEGRATION_SUDO=1 sudo"]
fn mailbox_delete_via_uds_refuses_nonempty_and_succeeds_after_cleanup() {
    if skip_if_mailbox_crud_not_root() {
        return;
    }
    let tmp = TempDir::new().unwrap();
    setup_test_env(tmp.path());

    let port = find_free_port();
    let daemon = start_serve(tmp.path(), port);
    let sock = tmp.path().join("run").join("aimx.sock");
    assert!(wait_for_socket(&sock, std::time::Duration::from_secs(5)));

    // Create a mailbox via UDS.
    aimx_cmd(tmp.path())
        .env("AIMX_RUNTIME_DIR", tmp.path().join("run"))
        .arg("--data-dir")
        .arg(tmp.path())
        .arg("mailbox")
        .arg("create")
        .arg("qux")
        .arg("--owner")
        .arg(current_username())
        .assert()
        .success();

    // Drop a file in the new mailbox so delete is refused.
    let qux_inbox = inbox(tmp.path(), "qux");
    std::fs::write(qux_inbox.join("2025-01-01-120000-held.md"), "content").unwrap();

    let assert = aimx_cmd(tmp.path())
        .env("AIMX_RUNTIME_DIR", tmp.path().join("run"))
        .arg("--data-dir")
        .arg(tmp.path())
        .arg("mailbox")
        .arg("delete")
        .arg("--yes")
        .arg("qux")
        .assert()
        .failure();
    let stderr = String::from_utf8_lossy(&assert.get_output().stderr).to_string();
    assert!(
        stderr.contains("NONEMPTY") && stderr.contains("qux"),
        "delete must be refused with NONEMPTY error, got stderr: {stderr}"
    );

    // The stanza must still be there.
    let config_text = std::fs::read_to_string(tmp.path().join("config.toml")).unwrap();
    assert!(config_text.contains("[mailboxes.qux]"));

    // Remove the file and retry; delete now succeeds, stanza is gone,
    // subsequent mail addressed to qux@domain falls through to catchall.
    std::fs::remove_file(qux_inbox.join("2025-01-01-120000-held.md")).unwrap();

    aimx_cmd(tmp.path())
        .env("AIMX_RUNTIME_DIR", tmp.path().join("run"))
        .arg("--data-dir")
        .arg(tmp.path())
        .arg("mailbox")
        .arg("delete")
        .arg("--yes")
        .arg("qux")
        .assert()
        .success();

    let config_text = std::fs::read_to_string(tmp.path().join("config.toml")).unwrap();
    assert!(
        !config_text.contains("[mailboxes.qux]"),
        "stanza should be removed after successful delete: {config_text}"
    );

    // Inbound to qux@... now falls through to catchall because the daemon
    // already picked up the swap.
    let email = concat!(
        "From: sender@example.com\r\n",
        "To: qux@agent.example.com\r\n",
        "Subject: Fallthrough\r\n",
        "Date: Mon, 01 Jan 2024 00:00:00 +0000\r\n",
        "Message-ID: <qux-gone@example.com>\r\n",
        "\r\n",
        "gone\r\n",
    );
    smtp_send_email(
        port,
        "sender@example.com",
        &["qux@agent.example.com"],
        email,
    );
    std::thread::sleep(std::time::Duration::from_millis(500));

    let catchall_md = find_md_files(&inbox(tmp.path(), "catchall"));
    assert!(
        !catchall_md.is_empty(),
        "mail to a deleted mailbox must fall through to catchall after the swap"
    );

    stop_serve(daemon);
}

// ---------------------------------------------------------------------------
// `aimx mailboxes delete --force` (CLI-only wipe + delete)
// ---------------------------------------------------------------------------

#[cfg(unix)]
#[test]
#[ignore = "requires root; MAILBOX-CRUD is root-only; run via the CI mailbox-crud-root step or AIMX_INTEGRATION_SUDO=1 sudo"]
fn mailbox_delete_force_yes_wipes_contents_and_succeeds() {
    if skip_if_mailbox_crud_not_root() {
        return;
    }
    let tmp = TempDir::new().unwrap();
    setup_test_env(tmp.path());

    let port = find_free_port();
    let daemon = start_serve(tmp.path(), port);
    let sock = tmp.path().join("run").join("aimx.sock");
    assert!(wait_for_socket(&sock, std::time::Duration::from_secs(5)));

    // Create a mailbox and ingest one message into it so a plain delete
    // would be refused with NONEMPTY.
    aimx_cmd(tmp.path())
        .env("AIMX_RUNTIME_DIR", tmp.path().join("run"))
        .arg("--data-dir")
        .arg(tmp.path())
        .arg("mailboxes")
        .arg("create")
        .arg("zed")
        .arg("--owner")
        .arg(current_username())
        .assert()
        .success();
    let zed_inbox = inbox(tmp.path(), "zed");
    std::fs::write(zed_inbox.join("2025-04-01-120000-held.md"), "content").unwrap();

    // Force-delete with `--yes` skips the prompt and proceeds.
    aimx_cmd(tmp.path())
        .env("AIMX_RUNTIME_DIR", tmp.path().join("run"))
        .arg("--data-dir")
        .arg(tmp.path())
        .arg("mailboxes")
        .arg("delete")
        .arg("--force")
        .arg("--yes")
        .arg("zed")
        .assert()
        .success();

    // Stanza is gone.
    let config_text = std::fs::read_to_string(tmp.path().join("config.toml")).unwrap();
    assert!(
        !config_text.contains("[mailboxes.zed]"),
        "stanza should be removed after force-delete: {config_text}"
    );
    // Inbox dir is empty (the daemon leaves the empty dir on disk per S46).
    let leftover: Vec<_> = std::fs::read_dir(&zed_inbox)
        .map(|r| r.filter_map(|e| e.ok()).collect())
        .unwrap_or_default();
    assert!(
        leftover.is_empty(),
        "inbox dir must be empty after --force wipe (got {leftover:?})"
    );

    stop_serve(daemon);
}

#[cfg(unix)]
#[test]
#[ignore = "requires root; MAILBOX-CRUD is root-only; run via the CI mailbox-crud-root step or AIMX_INTEGRATION_SUDO=1 sudo"]
fn mailbox_delete_force_without_yes_prompts_and_aborts_on_n() {
    if skip_if_mailbox_crud_not_root() {
        return;
    }
    let tmp = TempDir::new().unwrap();
    setup_test_env(tmp.path());

    let port = find_free_port();
    let daemon = start_serve(tmp.path(), port);
    let sock = tmp.path().join("run").join("aimx.sock");
    assert!(wait_for_socket(&sock, std::time::Duration::from_secs(5)));

    aimx_cmd(tmp.path())
        .env("AIMX_RUNTIME_DIR", tmp.path().join("run"))
        .arg("--data-dir")
        .arg(tmp.path())
        .arg("mailboxes")
        .arg("create")
        .arg("yon")
        .arg("--owner")
        .arg(current_username())
        .assert()
        .success();
    let yon_inbox = inbox(tmp.path(), "yon");
    std::fs::write(yon_inbox.join("2025-04-01-130000-keep.md"), "stay").unwrap();

    // Pipe `n\n` on stdin; the prompt must abort the delete and leave
    // the file in place.
    let assert = aimx_cmd(tmp.path())
        .env("AIMX_RUNTIME_DIR", tmp.path().join("run"))
        .arg("--data-dir")
        .arg(tmp.path())
        .arg("mailboxes")
        .arg("delete")
        .arg("--force")
        .arg("yon")
        .write_stdin("n\n")
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout).to_string();
    assert!(
        stdout.contains("Cancelled."),
        "abort path must print Cancelled, got: {stdout}"
    );
    assert!(
        stdout.contains("inbox/yon/: 1 file"),
        "prompt must show per-directory file counts with grammatical plural, got: {stdout}"
    );
    assert!(
        !stdout.contains("inbox/yon/: 1 files"),
        "prompt must not use the ungrammatical `1 files` form, got: {stdout}"
    );

    // File still there.
    assert!(
        yon_inbox.join("2025-04-01-130000-keep.md").is_file(),
        "abort must leave the email on disk"
    );
    // Stanza still present.
    let config_text = std::fs::read_to_string(tmp.path().join("config.toml")).unwrap();
    assert!(config_text.contains("[mailboxes.yon]"));

    stop_serve(daemon);
}

/// `aimx mailboxes delete --force catchall` must refuse client-side
/// (regardless of caller uid) before any wipe or daemon submission so
/// the catchall slot is never accidentally torn down. S2-2 keeps the
/// catchall refusal at the very top of the `--force` branch — it fires
/// even if no daemon is running and even for a non-root caller.
#[cfg(unix)]
#[test]
fn mailbox_delete_force_refuses_catchall() {
    let tmp = TempDir::new().unwrap();
    setup_test_env(tmp.path());

    let runtime = tmp.path().join("run");
    std::fs::create_dir_all(&runtime).ok();

    let assert = aimx_cmd(tmp.path())
        .env("AIMX_RUNTIME_DIR", &runtime)
        .arg("--data-dir")
        .arg(tmp.path())
        .arg("mailboxes")
        .arg("delete")
        .arg("--force")
        .arg("--yes")
        .arg("catchall")
        .assert()
        .failure();
    let stderr = String::from_utf8_lossy(&assert.get_output().stderr).to_string();
    assert!(
        stderr.contains("catchall"),
        "catchall refusal must surface verbatim, got stderr: {stderr}"
    );
}

/// S2-6 / S2-2: end-to-end non-root mailbox CRUD via daemon UDS. Runs
/// without `AIMX_INTEGRATION_SUDO=1` — this test is the canonical
/// regression guard against re-introducing a root gate on the
/// MAILBOX-CREATE / MAILBOX-DELETE path. With `aimx serve` running and
/// the test runner's uid (which is also the configured mailbox owner
/// in `setup_test_env`), creating `task-mb` and then force-deleting it
/// must succeed without sudo. Skips automatically if the runner happens
/// to be root (CI's root-only step exercises a different branch).
#[cfg(unix)]
#[test]
fn mailbox_create_delete_force_e2e_as_non_root_user() {
    if unsafe { libc::geteuid() } == 0 {
        eprintln!(
            "skipping: this test pins the non-root happy path; \
             root creates run via the dedicated sudo-lane test"
        );
        return;
    }
    let tmp = TempDir::new().unwrap();
    setup_test_env(tmp.path());

    let port = find_free_port();
    let daemon = start_serve(tmp.path(), port);
    let sock = tmp.path().join("run").join("aimx.sock");
    assert!(
        wait_for_socket(&sock, std::time::Duration::from_secs(5)),
        "UDS socket never appeared after `aimx serve` start"
    );

    // CREATE — non-root, no sudo, no AIMX_TEST_SKIP_AUTHZ_CHECK.
    // Pass `--owner <runner>` so `prompt_mailbox_owner` is skipped
    // entirely (it would otherwise loop 5 times under non-TTY stdin
    // because the local part `task-mb` does not resolve via getpwnam).
    // Owner == caller, so the soft-warning path also stays silent —
    // exactly the agent-friendly happy path we want to pin.
    let runner = current_username();
    let create = aimx_cmd(tmp.path())
        .env("AIMX_RUNTIME_DIR", tmp.path().join("run"))
        .arg("--data-dir")
        .arg(tmp.path())
        .arg("mailboxes")
        .arg("create")
        .arg("task-mb")
        .arg("--owner")
        .arg(&runner)
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&create.get_output().stdout).to_string();
    assert!(
        stdout.contains("Mailbox 'task-mb' created"),
        "CREATE must succeed for non-root caller via daemon UDS, got: {stdout}"
    );
    assert!(
        !stdout.contains("Restart the daemon"),
        "UDS path must NOT print the restart-hint banner: {stdout}"
    );

    // config.toml on disk reflects the new mailbox stanza.
    let config_text = std::fs::read_to_string(tmp.path().join("config.toml")).unwrap();
    assert!(
        config_text.contains("[mailboxes.task-mb]"),
        "config.toml should contain the new stanza: {config_text}"
    );
    // The owner field must be the runner's username (synthesized by
    // the daemon from SO_PEERCRED — never client-supplied).
    assert!(
        config_text.contains(&format!("owner = \"{runner}\"")),
        "owner must be the runner's username: {config_text}"
    );

    // On-disk owner check: the inbox dir must be owned by the runner's
    // uid (the daemon chowns to the resolved owner).
    let inbox_dir = tmp.path().join("inbox").join("task-mb");
    assert!(inbox_dir.is_dir(), "inbox/task-mb/ must exist on disk");
    use std::os::unix::fs::MetadataExt;
    let meta = std::fs::metadata(&inbox_dir).unwrap();
    let runner_uid = unsafe { libc::geteuid() };
    assert_eq!(
        meta.uid(),
        runner_uid,
        "inbox/task-mb/ must be owned by the runner uid"
    );

    // DELETE --force --yes — should wipe and succeed via daemon UDS.
    // Confirms S2-2: the `--force` flag works for the mailbox owner
    // without sudo. Mailbox is empty on creation, but exercising the
    // force path also covers the wipe-then-submit ordering.
    let delete = aimx_cmd(tmp.path())
        .env("AIMX_RUNTIME_DIR", tmp.path().join("run"))
        .arg("--data-dir")
        .arg(tmp.path())
        .arg("mailboxes")
        .arg("delete")
        .arg("--force")
        .arg("--yes")
        .arg("task-mb")
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&delete.get_output().stdout).to_string();
    assert!(
        stdout.contains("Mailbox 'task-mb' deleted"),
        "DELETE --force --yes must succeed for non-root owner, got: {stdout}"
    );

    // config.toml must no longer reference task-mb.
    let config_text_after = std::fs::read_to_string(tmp.path().join("config.toml")).unwrap();
    assert!(
        !config_text_after.contains("[mailboxes.task-mb]"),
        "config.toml must no longer contain task-mb stanza: {config_text_after}"
    );

    stop_serve(daemon);
}

/// S2-1 soft-warning: a non-root caller passing `--owner <other>` gets
/// a stderr line clarifying that the daemon will discard the value and
/// bind ownership to the caller. The mailbox is still created (the
/// daemon synthesizes the correct owner) — the warning is purely UX.
#[cfg(unix)]
#[test]
fn mailbox_create_owner_flag_warns_for_non_root_callers() {
    if unsafe { libc::geteuid() } == 0 {
        eprintln!("skipping: warning fires only for non-root callers");
        return;
    }
    let tmp = TempDir::new().unwrap();
    setup_test_env(tmp.path());

    let port = find_free_port();
    let daemon = start_serve(tmp.path(), port);
    let sock = tmp.path().join("run").join("aimx.sock");
    assert!(
        wait_for_socket(&sock, std::time::Duration::from_secs(5)),
        "UDS socket never appeared"
    );

    // Pick an owner string different from the runner. `nobody` is
    // present on every Linux box and resolves via getpwnam, which
    // satisfies the CLI's pre-flight `--owner` check before the
    // daemon's SO_PEERCRED override kicks in.
    let assert = aimx_cmd(tmp.path())
        .env("AIMX_RUNTIME_DIR", tmp.path().join("run"))
        .arg("--data-dir")
        .arg(tmp.path())
        .arg("mailboxes")
        .arg("create")
        .arg("warned-mb")
        .arg("--owner")
        .arg("nobody")
        .assert()
        .success();

    let stderr = String::from_utf8_lossy(&assert.get_output().stderr).to_string();
    let runner = current_username();
    assert!(
        stderr.contains("--owner ignored"),
        "soft warning must fire when non-root passes --owner <other>, stderr: {stderr}"
    );
    assert!(
        stderr.contains(&runner),
        "warning must name the actual caller (`{runner}`), stderr: {stderr}"
    );

    // The daemon ignored the owner flag — the resulting mailbox is
    // owned by the runner, not `nobody`.
    let config_text = std::fs::read_to_string(tmp.path().join("config.toml")).unwrap();
    assert!(
        config_text.contains(&format!("owner = \"{runner}\"")),
        "owner must be the runner, NOT the wire-supplied value: {config_text}"
    );

    stop_serve(daemon);
}

/// Cycle 2 regression guard for Blocker 1 / Blocker 3 of the Sprint 2
/// review: the CLI must reach `mailbox::run` even when the caller
/// cannot read `/etc/aimx/config.toml`.
///
/// In production the config is `0640 root:root`, so a non-root caller
/// cannot read it. The previous shape — `dispatch()` `?`-propagating
/// `Config::load_resolved_with_data_dir(...)` — surfaced the EACCES
/// as a bare `Permission denied (os error 13)` before
/// `mailbox::run` ever saw the request. This test simulates the
/// production permission model by chmod-ing the config file to `0000`
/// (no permissions for any user, including the test runner). Either
/// the create succeeds via UDS (daemon-up branch) or it exits with
/// the canonical socket-missing hint (daemon-down branch). The bare
/// EACCES surface that broke Cycle 1 must NOT reappear.
///
/// Runs without sudo so it lives in the standard CI lane and catches
/// the regression on every PR.
#[cfg(unix)]
#[test]
fn mailbox_create_with_unreadable_config_does_not_surface_eacces() {
    if unsafe { libc::geteuid() } == 0 {
        eprintln!("skipping: chmod 0000 has no effect on root");
        return;
    }
    let tmp = TempDir::new().unwrap();
    setup_test_env(tmp.path());
    let port = find_free_port();
    let daemon = start_serve(tmp.path(), port);
    let sock = tmp.path().join("run").join("aimx.sock");
    assert!(
        wait_for_socket(&sock, std::time::Duration::from_secs(5)),
        "UDS socket never appeared"
    );

    // Chmod the config to 0000. The daemon already loaded its
    // `Arc<Config>` snapshot at startup, so it keeps serving from
    // memory; the CLI subprocess inherits the test runner's uid and
    // therefore cannot read the file.
    use std::os::unix::fs::PermissionsExt;
    let config_path = tmp.path().join("config.toml");
    std::fs::set_permissions(&config_path, std::fs::Permissions::from_mode(0o000)).unwrap();

    let runner = current_username();
    let create = aimx_cmd(tmp.path())
        .env("AIMX_RUNTIME_DIR", tmp.path().join("run"))
        .arg("--data-dir")
        .arg(tmp.path())
        .arg("mailboxes")
        .arg("create")
        .arg("perm-mb")
        .arg("--owner")
        .arg(&runner)
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&create.get_output().stdout).to_string();
    let stderr = String::from_utf8_lossy(&create.get_output().stderr).to_string();
    assert!(
        stdout.contains("Mailbox 'perm-mb' created"),
        "CREATE must succeed via daemon UDS without reading local config; \
         stdout: {stdout}, stderr: {stderr}"
    );
    // The bare-EACCES surface that broke Cycle 1 must NOT reappear.
    assert!(
        !stderr.contains("Permission denied (os error 13)"),
        "config-read EACCES must never surface to the operator; stderr: {stderr}"
    );

    // Restore perms so the test cleanup can read the file.
    std::fs::set_permissions(&config_path, std::fs::Permissions::from_mode(0o640)).unwrap();
    stop_serve(daemon);
}

/// Companion to the create-side regression test: `aimx mailboxes
/// list` must also work without local config-read access. The
/// non-root list path falls back to `MAILBOX-LIST` over UDS, which
/// the daemon resolves via SO_PEERCRED — no config read on the
/// client side at all.
#[cfg(unix)]
#[test]
fn mailbox_list_with_unreadable_config_falls_back_to_uds() {
    if unsafe { libc::geteuid() } == 0 {
        eprintln!("skipping: chmod 0000 has no effect on root");
        return;
    }
    let tmp = TempDir::new().unwrap();
    setup_test_env(tmp.path());
    let port = find_free_port();
    let daemon = start_serve(tmp.path(), port);
    let sock = tmp.path().join("run").join("aimx.sock");
    assert!(
        wait_for_socket(&sock, std::time::Duration::from_secs(5)),
        "UDS socket never appeared"
    );

    use std::os::unix::fs::PermissionsExt;
    let config_path = tmp.path().join("config.toml");
    std::fs::set_permissions(&config_path, std::fs::Permissions::from_mode(0o000)).unwrap();

    let list = aimx_cmd(tmp.path())
        .env("AIMX_RUNTIME_DIR", tmp.path().join("run"))
        .arg("--data-dir")
        .arg(tmp.path())
        .arg("mailboxes")
        .arg("list")
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&list.get_output().stdout).to_string();
    let stderr = String::from_utf8_lossy(&list.get_output().stderr).to_string();
    assert!(
        stdout.contains("alice") || stdout.contains("MAILBOX"),
        "LIST must succeed via daemon UDS; stdout: {stdout}, stderr: {stderr}"
    );
    assert!(
        !stderr.contains("Permission denied (os error 13)"),
        "config-read EACCES must never surface to the operator; stderr: {stderr}"
    );

    std::fs::set_permissions(&config_path, std::fs::Permissions::from_mode(0o640)).unwrap();
    stop_serve(daemon);
}

// ---------------------------------------------------------------------------
// Per-tool production-perm regression tests for the 9 MCP tools that
// previously fell through `self.load_config()` and broke on a real
// install (`/etc/aimx/config.toml` is `0640 root:root`; the non-root
// MCP process would always fail with `Permission denied (os error
// 13)`). Every tool now routes through the daemon's `MAILBOX-LIST` /
// `HOOK-LIST` verbs, and `AimxMcpServer::load_config` was deleted from
// `src/mcp.rs` so production code cannot accidentally reintroduce the
// bug class.
//
// Each test follows the same shape: spawn the daemon (which loaded
// `Arc<Config>` at startup so it keeps serving from memory), `chmod
// 0000` on the on-disk config so any client-side direct read would
// fail with EACCES, then invoke the tool and assert (a) it works and
// (b) no EACCES surfaces from a config-load path.
// ---------------------------------------------------------------------------

/// Regression test: `email_list` must not surface EACCES on a config
/// with `0000` perms — the tool now derives `inbox_path` / `sent_path`
/// from the daemon's `MAILBOX-LIST` row, never reads `config.toml`.
#[cfg(unix)]
#[test]
fn mcp_email_list_with_unreadable_config_does_not_surface_eacces() {
    if unsafe { libc::geteuid() } == 0 {
        eprintln!("skipping: chmod 0000 has no effect on root");
        return;
    }
    let tmp = TempDir::new().unwrap();
    setup_test_env(tmp.path());
    let port = find_free_port();
    let daemon = start_serve(tmp.path(), port);
    let sock = tmp.path().join("run").join("aimx.sock");
    assert!(
        wait_for_socket(&sock, std::time::Duration::from_secs(5)),
        "UDS socket never appeared"
    );

    use std::os::unix::fs::PermissionsExt;
    let config_path = tmp.path().join("config.toml");
    std::fs::set_permissions(&config_path, std::fs::Permissions::from_mode(0o000)).unwrap();

    let mut client = McpClient::spawn(tmp.path());
    client.initialize();

    let resp = client.call_tool("email_list", serde_json::json!({"mailbox": "alice"}));
    let text = get_tool_text(&resp);
    assert!(
        !text.contains("Permission denied"),
        "email_list must not surface EACCES on `0000` config; got: {text}"
    );
    // Empty mailbox returns the literal `[]` string.
    assert_eq!(text, "[]", "{text}");

    client.shutdown();
    std::fs::set_permissions(&config_path, std::fs::Permissions::from_mode(0o640)).unwrap();
    stop_serve(daemon);
}

/// Regression test: `email_read` against `0000` config — same shape.
#[cfg(unix)]
#[test]
fn mcp_email_read_with_unreadable_config_does_not_surface_eacces() {
    if unsafe { libc::geteuid() } == 0 {
        eprintln!("skipping: chmod 0000 has no effect on root");
        return;
    }
    let tmp = TempDir::new().unwrap();
    setup_test_env(tmp.path());
    let alice_dir = inbox(tmp.path(), "alice");
    create_email_file(&alice_dir, "2025-06-01-001", "s@example.com", "Hi", false);

    let port = find_free_port();
    let daemon = start_serve(tmp.path(), port);
    let sock = tmp.path().join("run").join("aimx.sock");
    assert!(
        wait_for_socket(&sock, std::time::Duration::from_secs(5)),
        "UDS socket never appeared"
    );

    use std::os::unix::fs::PermissionsExt;
    let config_path = tmp.path().join("config.toml");
    std::fs::set_permissions(&config_path, std::fs::Permissions::from_mode(0o000)).unwrap();

    let mut client = McpClient::spawn(tmp.path());
    client.initialize();

    let resp = client.call_tool(
        "email_read",
        serde_json::json!({"mailbox": "alice", "id": "2025-06-01-001"}),
    );
    let text = get_tool_text(&resp);
    assert!(
        !text.contains("Permission denied"),
        "email_read must not surface EACCES; got: {text}"
    );
    assert!(
        text.contains("Body of 2025-06-01-001"),
        "email_read must return body; got: {text}"
    );

    client.shutdown();
    std::fs::set_permissions(&config_path, std::fs::Permissions::from_mode(0o640)).unwrap();
    stop_serve(daemon);
}

/// Regression test: `email_mark_read` against `0000` config.
#[cfg(unix)]
#[test]
fn mcp_email_mark_read_with_unreadable_config_does_not_surface_eacces() {
    if unsafe { libc::geteuid() } == 0 {
        eprintln!("skipping: chmod 0000 has no effect on root");
        return;
    }
    let tmp = TempDir::new().unwrap();
    setup_test_env(tmp.path());
    let alice_dir = inbox(tmp.path(), "alice");
    create_email_file(&alice_dir, "2025-06-01-001", "s@example.com", "Hi", false);

    let port = find_free_port();
    let daemon = start_serve(tmp.path(), port);
    let sock = tmp.path().join("run").join("aimx.sock");
    assert!(
        wait_for_socket(&sock, std::time::Duration::from_secs(5)),
        "UDS socket never appeared"
    );

    use std::os::unix::fs::PermissionsExt;
    let config_path = tmp.path().join("config.toml");
    std::fs::set_permissions(&config_path, std::fs::Permissions::from_mode(0o000)).unwrap();

    let mut client = McpClient::spawn(tmp.path());
    client.initialize();

    let resp = client.call_tool(
        "email_mark_read",
        serde_json::json!({"mailbox": "alice", "id": "2025-06-01-001"}),
    );
    let text = get_tool_text(&resp);
    assert!(
        !text.contains("Permission denied"),
        "email_mark_read must not surface EACCES; got: {text}"
    );

    client.shutdown();
    std::fs::set_permissions(&config_path, std::fs::Permissions::from_mode(0o640)).unwrap();
    stop_serve(daemon);
}

/// Regression test: `email_mark_unread` against `0000` config.
#[cfg(unix)]
#[test]
fn mcp_email_mark_unread_with_unreadable_config_does_not_surface_eacces() {
    if unsafe { libc::geteuid() } == 0 {
        eprintln!("skipping: chmod 0000 has no effect on root");
        return;
    }
    let tmp = TempDir::new().unwrap();
    setup_test_env(tmp.path());
    let alice_dir = inbox(tmp.path(), "alice");
    create_email_file(&alice_dir, "2025-06-01-001", "s@example.com", "Hi", true);

    let port = find_free_port();
    let daemon = start_serve(tmp.path(), port);
    let sock = tmp.path().join("run").join("aimx.sock");
    assert!(
        wait_for_socket(&sock, std::time::Duration::from_secs(5)),
        "UDS socket never appeared"
    );

    use std::os::unix::fs::PermissionsExt;
    let config_path = tmp.path().join("config.toml");
    std::fs::set_permissions(&config_path, std::fs::Permissions::from_mode(0o000)).unwrap();

    let mut client = McpClient::spawn(tmp.path());
    client.initialize();

    let resp = client.call_tool(
        "email_mark_unread",
        serde_json::json!({"mailbox": "alice", "id": "2025-06-01-001"}),
    );
    let text = get_tool_text(&resp);
    assert!(
        !text.contains("Permission denied"),
        "email_mark_unread must not surface EACCES; got: {text}"
    );

    client.shutdown();
    std::fs::set_permissions(&config_path, std::fs::Permissions::from_mode(0o640)).unwrap();
    stop_serve(daemon);
}

/// Regression test: `email_send` against `0000` config — the tool now
/// derives the from-address from the daemon's `MAILBOX-LIST` row, no
/// `config.toml` read.
#[cfg(unix)]
#[test]
fn mcp_email_send_with_unreadable_config_does_not_surface_eacces() {
    if unsafe { libc::geteuid() } == 0 {
        eprintln!("skipping: chmod 0000 has no effect on root");
        return;
    }
    let tmp = TempDir::new().unwrap();
    setup_test_env(tmp.path());

    let port = find_free_port();
    let daemon = start_serve(tmp.path(), port);
    let sock = tmp.path().join("run").join("aimx.sock");
    assert!(
        wait_for_socket(&sock, std::time::Duration::from_secs(5)),
        "UDS socket never appeared"
    );

    use std::os::unix::fs::PermissionsExt;
    let config_path = tmp.path().join("config.toml");
    std::fs::set_permissions(&config_path, std::fs::Permissions::from_mode(0o000)).unwrap();

    let mut client = McpClient::spawn(tmp.path());
    client.initialize();

    // Send to a nonexistent recipient — we don't care whether the SMTP
    // delivery itself succeeds, only that a config-EACCES does not
    // surface from the MCP tool entry point.
    let resp = client.call_tool(
        "email_send",
        serde_json::json!({
            "from_mailbox": "alice",
            "to": "nobody@invalid.example.invalid",
            "subject": "perm test",
            "body": "x"
        }),
    );
    let text = get_tool_text(&resp);
    assert!(
        !text.contains("Permission denied"),
        "email_send must not surface EACCES; got: {text}"
    );

    client.shutdown();
    std::fs::set_permissions(&config_path, std::fs::Permissions::from_mode(0o640)).unwrap();
    stop_serve(daemon);
}

/// Regression test: `email_reply` against `0000` config.
#[cfg(unix)]
#[test]
fn mcp_email_reply_with_unreadable_config_does_not_surface_eacces() {
    if unsafe { libc::geteuid() } == 0 {
        eprintln!("skipping: chmod 0000 has no effect on root");
        return;
    }
    let tmp = TempDir::new().unwrap();
    setup_test_env(tmp.path());
    let alice_dir = inbox(tmp.path(), "alice");
    create_email_file(&alice_dir, "2025-06-01-001", "s@example.com", "Hi", false);

    let port = find_free_port();
    let daemon = start_serve(tmp.path(), port);
    let sock = tmp.path().join("run").join("aimx.sock");
    assert!(
        wait_for_socket(&sock, std::time::Duration::from_secs(5)),
        "UDS socket never appeared"
    );

    use std::os::unix::fs::PermissionsExt;
    let config_path = tmp.path().join("config.toml");
    std::fs::set_permissions(&config_path, std::fs::Permissions::from_mode(0o000)).unwrap();

    let mut client = McpClient::spawn(tmp.path());
    client.initialize();

    let resp = client.call_tool(
        "email_reply",
        serde_json::json!({
            "mailbox": "alice",
            "id": "2025-06-01-001",
            "body": "reply body"
        }),
    );
    let text = get_tool_text(&resp);
    assert!(
        !text.contains("Permission denied"),
        "email_reply must not surface EACCES; got: {text}"
    );

    client.shutdown();
    std::fs::set_permissions(&config_path, std::fs::Permissions::from_mode(0o640)).unwrap();
    stop_serve(daemon);
}

/// Regression test: `hook_create` against `0000` config — the tool now
/// pre-flights via `MAILBOX-LIST` over UDS, no `config.toml` read.
#[cfg(unix)]
#[test]
fn mcp_hook_create_with_unreadable_config_does_not_surface_eacces() {
    if unsafe { libc::geteuid() } == 0 {
        eprintln!("skipping: chmod 0000 has no effect on root");
        return;
    }
    let tmp = TempDir::new().unwrap();
    setup_test_env(tmp.path());

    let port = find_free_port();
    let daemon = start_serve(tmp.path(), port);
    let sock = tmp.path().join("run").join("aimx.sock");
    assert!(
        wait_for_socket(&sock, std::time::Duration::from_secs(5)),
        "UDS socket never appeared"
    );

    use std::os::unix::fs::PermissionsExt;
    let config_path = tmp.path().join("config.toml");
    std::fs::set_permissions(&config_path, std::fs::Permissions::from_mode(0o000)).unwrap();

    let mut client = McpClient::spawn(tmp.path());
    client.initialize();

    let resp = client.call_tool(
        "hook_create",
        serde_json::json!({
            "mailbox": "alice",
            "event": "on_receive",
            "cmd": ["/bin/true"]
        }),
    );
    let text = get_tool_text(&resp);
    assert!(
        !text.contains("Permission denied"),
        "hook_create must not surface EACCES; got: {text}"
    );

    client.shutdown();
    std::fs::set_permissions(&config_path, std::fs::Permissions::from_mode(0o640)).unwrap();
    stop_serve(daemon);
}

/// Regression test: `hook_list` against `0000` config — routes through
/// the new `HOOK-LIST` UDS verb, no `config.toml` read.
#[cfg(unix)]
#[test]
fn mcp_hook_list_with_unreadable_config_does_not_surface_eacces() {
    if unsafe { libc::geteuid() } == 0 {
        eprintln!("skipping: chmod 0000 has no effect on root");
        return;
    }
    let tmp = TempDir::new().unwrap();
    setup_test_env(tmp.path());

    let port = find_free_port();
    let daemon = start_serve(tmp.path(), port);
    let sock = tmp.path().join("run").join("aimx.sock");
    assert!(
        wait_for_socket(&sock, std::time::Duration::from_secs(5)),
        "UDS socket never appeared"
    );

    use std::os::unix::fs::PermissionsExt;
    let config_path = tmp.path().join("config.toml");
    std::fs::set_permissions(&config_path, std::fs::Permissions::from_mode(0o000)).unwrap();

    let mut client = McpClient::spawn(tmp.path());
    client.initialize();

    let resp = client.call_tool("hook_list", serde_json::json!({}));
    let text = get_tool_text(&resp);
    assert!(
        !text.contains("Permission denied"),
        "hook_list must not surface EACCES; got: {text}"
    );
    assert_eq!(text, "[]", "{text}");

    client.shutdown();
    std::fs::set_permissions(&config_path, std::fs::Permissions::from_mode(0o640)).unwrap();
    stop_serve(daemon);
}

/// Regression test: `hook_delete` against `0000` config — the tool is
/// a thin pass-through to `HOOK-DELETE`, no `config.toml` read.
#[cfg(unix)]
#[test]
fn mcp_hook_delete_with_unreadable_config_does_not_surface_eacces() {
    if unsafe { libc::geteuid() } == 0 {
        eprintln!("skipping: chmod 0000 has no effect on root");
        return;
    }
    let tmp = TempDir::new().unwrap();
    setup_test_env(tmp.path());

    let port = find_free_port();
    let daemon = start_serve(tmp.path(), port);
    let sock = tmp.path().join("run").join("aimx.sock");
    assert!(
        wait_for_socket(&sock, std::time::Duration::from_secs(5)),
        "UDS socket never appeared"
    );

    use std::os::unix::fs::PermissionsExt;
    let config_path = tmp.path().join("config.toml");
    std::fs::set_permissions(&config_path, std::fs::Permissions::from_mode(0o000)).unwrap();

    let mut client = McpClient::spawn(tmp.path());
    client.initialize();

    let resp = client.call_tool("hook_delete", serde_json::json!({"name": "no_such_hook"}));
    let text = get_tool_text(&resp);
    assert!(
        !text.contains("Permission denied"),
        "hook_delete must not surface EACCES; got: {text}"
    );
    // The daemon-side handler will canonically return ENOENT for a
    // hook name it doesn't know.
    assert!(
        text.contains("not found") || text.contains("ENOENT"),
        "hook_delete error should be opaque not-found: {text}"
    );

    client.shutdown();
    std::fs::set_permissions(&config_path, std::fs::Permissions::from_mode(0o640)).unwrap();
    stop_serve(daemon);
}

/// Cycle 2 regression guard for Blocker 2: the MCP `mailbox_create`
/// tool must work end-to-end against a real daemon for a non-root
/// caller. Cycle 1 shipped the tool but the wire-protocol parser
/// rejected the `owner: None` request with `[MALFORMED] missing
/// required header: Owner` — a regression that 1021 unit tests
/// missed because none of them reached the wire layer for this code
/// path. The fix relaxes the parser to make `Owner:` optional; this
/// test pins the agent-friendly surface so a future re-tightening
/// fails CI.
#[cfg(unix)]
#[test]
fn mcp_mailbox_create_against_running_daemon_succeeds_for_non_root() {
    if unsafe { libc::geteuid() } == 0 {
        eprintln!("skipping: this test pins the non-root MCP path");
        return;
    }
    let tmp = TempDir::new().unwrap();
    setup_test_env(tmp.path());
    let port = find_free_port();
    let daemon = start_serve(tmp.path(), port);
    let sock = tmp.path().join("run").join("aimx.sock");
    assert!(
        wait_for_socket(&sock, std::time::Duration::from_secs(5)),
        "UDS socket never appeared"
    );

    let mut client = McpClient::spawn(tmp.path());
    client.initialize();

    let resp = client.call_tool("mailbox_create", serde_json::json!({"name": "agent-mb"}));
    let tool_text = resp["result"]["content"][0]["text"].as_str().unwrap_or("");
    // The protocol-level rejection used to surface as
    // `[MALFORMED] missing required header: Owner` — must NOT recur.
    assert!(
        !tool_text.contains("MALFORMED"),
        "MAILBOX-CREATE without Owner must not be rejected by the parser; got: {tool_text}"
    );
    // Success path: the tool returns the new mailbox's full address.
    assert!(
        tool_text.contains("agent-mb@agent.example.com")
            || tool_text.contains("Mailbox 'agent-mb' created"),
        "expected success message, got: {tool_text}"
    );

    // Daemon hot-swapped the config. The on-disk stanza names the
    // runner as the owner because the daemon synthesizes from
    // SO_PEERCRED, not from any client-supplied value.
    let config_text = std::fs::read_to_string(tmp.path().join("config.toml")).unwrap();
    let runner = current_username();
    assert!(
        config_text.contains("[mailboxes.agent-mb]"),
        "config.toml should carry the new stanza: {config_text}"
    );
    assert!(
        config_text.contains(&format!("owner = \"{runner}\"")),
        "owner must be the runner's username (SO_PEERCRED-bound): {config_text}"
    );

    client.shutdown();
    stop_serve(daemon);
}

// ---------------------------------------------------------------------------
// DKIM startup check wired into `run_serve`. These tests exercise the
// full daemon against a canned resolver override so the check runs
// through the real code path (not just the evaluator unit tests) and we
// can assert the startup log rendering and that the listeners still bind
// after a non-`Match` outcome.
// ---------------------------------------------------------------------------

#[cfg(unix)]
fn start_serve_with_env(tmp: &Path, port: u16, extra_env: &[(&str, &str)]) -> std::process::Child {
    let runtime = tmp.join("run");
    std::fs::create_dir_all(&runtime).ok();
    let mut cmd = StdCommand::new(aimx_binary_path());
    cmd.env("AIMX_CONFIG_DIR", tmp)
        .env("AIMX_RUNTIME_DIR", &runtime)
        .env("AIMX_SANDBOX_FORCE_FALLBACK", "1");
    for (k, v) in extra_env {
        cmd.env(k, v);
    }
    let mut child = cmd
        .arg("--data-dir")
        .arg(tmp)
        .arg("serve")
        .arg("--bind")
        .arg(format!("127.0.0.1:{port}"))
        .stderr(Stdio::piped())
        .spawn()
        .expect("Failed to spawn aimx serve");

    let started = std::time::Instant::now();
    loop {
        if started.elapsed() > std::time::Duration::from_secs(30) {
            child.kill().unwrap();
            panic!("aimx serve did not start within 30s");
        }
        if std::net::TcpStream::connect(format!("127.0.0.1:{port}")).is_ok() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    child
}

/// Collect the stderr a spawned `aimx serve` has buffered so far. We send
/// SIGTERM, wait for exit, then drain stderr; the startup-warning log
/// lines are well before the shutdown banner, so they are always captured
/// in full.
#[cfg(unix)]
fn stop_serve_capture_stderr(mut child: std::process::Child) -> String {
    use std::io::Read as _;
    unsafe {
        libc::kill(child.id() as libc::pid_t, libc::SIGTERM);
    }
    let _ = child.wait_timeout(std::time::Duration::from_secs(10));
    let mut buf = String::new();
    if let Some(mut stderr) = child.stderr.take() {
        let _ = stderr.read_to_string(&mut buf);
    }
    buf
}

/// Strip ANSI escape sequences so the substring assertions below don't
/// break when `term::warn`/`term::error` decorate output for a TTY.
fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' && chars.peek() == Some(&'[') {
            chars.next();
            while let Some(&nc) = chars.peek() {
                chars.next();
                if nc.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

#[cfg(unix)]
#[test]
fn dkim_startup_check_mismatch_logs_warning_and_binds_listeners() {
    // `AIMX_TEST_DKIM_RESOLVER_OVERRIDE=ok:...` short-circuits the real
    // DNS lookup so the startup check sees a canned `p=` value that does
    // not match the on-disk public key. The daemon must log a multi-line
    // ERROR warning and still bind both the SMTP and UDS listeners.
    let tmp = TempDir::new().unwrap();
    setup_test_env(tmp.path());

    let port = find_free_port();
    let child = start_serve_with_env(
        tmp.path(),
        port,
        &[(
            "AIMX_TEST_DKIM_RESOLVER_OVERRIDE",
            "ok:v=DKIM1; k=rsa; p=COMPLETELY-DIFFERENT-KEY",
        )],
    );

    // The TCP listener is already accepting connections (start_serve_with_env
    // waits for that). Confirm the UDS listener bound too.
    let sock = tmp.path().join("run").join("aimx.sock");
    assert!(
        wait_for_socket(&sock, std::time::Duration::from_secs(5)),
        "UDS send socket never appeared despite DKIM mismatch (should be non-fatal)"
    );

    let stderr = strip_ansi(&stop_serve_capture_stderr(child));
    assert!(
        stderr.contains("ERROR:") && stderr.contains("DKIM key mismatch"),
        "expected mismatch ERROR in stderr, got:\n{stderr}"
    );
    assert!(
        stderr.contains("aimx setup"),
        "mismatch warning must tell operator how to fix: {stderr}"
    );
}

#[cfg(unix)]
#[test]
fn dkim_startup_check_resolve_error_logs_warning_and_binds_listeners() {
    // `AIMX_TEST_DKIM_RESOLVER_OVERRIDE=err:...` forces the resolver to
    // return an error, simulating NXDOMAIN / timeout / offline DNS. The
    // daemon must log a `warn`-level message but never treat this as
    // fatal. DNS may not have propagated yet after a fresh setup.
    let tmp = TempDir::new().unwrap();
    setup_test_env(tmp.path());

    let port = find_free_port();
    let child = start_serve_with_env(
        tmp.path(),
        port,
        &[("AIMX_TEST_DKIM_RESOLVER_OVERRIDE", "err:simulated NXDOMAIN")],
    );

    let sock = tmp.path().join("run").join("aimx.sock");
    assert!(
        wait_for_socket(&sock, std::time::Duration::from_secs(5)),
        "UDS send socket never appeared after ResolveError (should be non-fatal)"
    );

    let stderr = strip_ansi(&stop_serve_capture_stderr(child));
    assert!(
        stderr.contains("Warning:") && stderr.contains("DKIM DNS sanity check skipped"),
        "expected resolve-error warn in stderr, got:\n{stderr}"
    );
    assert!(
        stderr.contains("simulated NXDOMAIN"),
        "resolve error message must surface the underlying DNS error: {stderr}"
    );
}

// ---------------------------------------------------------------------------
// Unified per-mailbox write lock covering inbound ingest, MARK-*, and
// MAILBOX-*. These integration tests stress the lock boundary:
//   1. Concurrent ingest bursts + MARK-READ on the same mailbox: no torn
//      writes, every `.md` file has a clean `+++ ... +++` frontmatter and
//      parses as TOML.
//   2. Concurrent MAILBOX-CREATE + ingest addressed to the new mailbox:
//      the two locks (outer per-mailbox, inner CONFIG_WRITE_LOCK) must not
//      deadlock, the config write lands before the ingest routes, and the
//      inbound message ends up in the new mailbox (or catchall, but never
//      corrupt) with the daemon still healthy.
// ---------------------------------------------------------------------------

#[cfg(unix)]
#[test]
fn concurrent_ingest_burst_and_mark_same_mailbox_no_torn_writes() {
    // Fire N inbound messages concurrently with M MARK-READ calls
    // against the same mailbox. With the unified per-mailbox
    // `tokio::sync::Mutex<()>` shared between ingest and the MARK
    // handler, every `.md` file on disk must end with a clean
    // frontmatter block.
    let tmp = TempDir::new().unwrap();
    setup_test_env(tmp.path());

    let alice_dir = inbox(tmp.path(), "alice");
    // Pre-seed two emails so MARK-READ has stable targets while new
    // inbound ingests are landing in the same directory.
    for id in ["2025-06-01-seed1", "2025-06-01-seed2"] {
        create_email_file(&alice_dir, id, "sender@example.com", "Pre-seed", false);
    }

    let port = find_free_port();
    let daemon = start_serve(tmp.path(), port);

    let runtime = tmp.path().join("run");
    let sock = runtime.join("aimx.sock");
    assert!(
        wait_for_socket(&sock, std::time::Duration::from_secs(5)),
        "UDS send socket never appeared"
    );

    // Fire 6 concurrent inbound SMTP transactions.
    let mut smtp_handles = Vec::new();
    for i in 0..6 {
        smtp_handles.push(std::thread::spawn(move || {
            let email = format!(
                "From: other@example.com\r\n\
                 To: alice@agent.example.com\r\n\
                 Subject: Burst {i}\r\n\
                 Date: Mon, 01 Jan 2024 00:00:00 +0000\r\n\
                 Message-ID: <burst-{i}@example.com>\r\n\
                 \r\n\
                 burst body {i}\r\n",
            );
            smtp_send_email(
                port,
                "other@example.com",
                &["alice@agent.example.com"],
                &email,
            );
        }));
    }

    // Fire concurrent MARK-READ calls on the seeded files via MCP.
    let mut mark_handles = Vec::new();
    for id in ["2025-06-01-seed1", "2025-06-01-seed2"] {
        let tmp_path = tmp.path().to_path_buf();
        let runtime = runtime.clone();
        let id = id.to_string();
        mark_handles.push(std::thread::spawn(move || {
            let mut child = StdCommand::new(aimx_binary_path())
                .env("AIMX_CONFIG_DIR", &tmp_path)
                .env("AIMX_RUNTIME_DIR", &runtime)
                .arg("--data-dir")
                .arg(&tmp_path)
                .arg("mcp")
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .expect("Failed to spawn aimx mcp");
            let stdin = child.stdin.take().unwrap();
            let stdout = child.stdout.take().unwrap();
            let stderr = child.stderr.take().unwrap();
            let (stderr_buf, stderr_drain) = spawn_stderr_drain(stderr);
            let mut client = McpClient {
                child,
                stdin,
                reader: BufReader::new(stdout),
                id: 0,
                stderr_buf,
                stderr_drain: Some(stderr_drain),
            };
            client.initialize();
            let resp = client.call_tool(
                "email_mark_read",
                serde_json::json!({"mailbox": "alice", "id": id}),
            );
            let text = get_tool_text(&resp);
            assert!(text.contains("marked as read"), "{text}");
            client.shutdown();
        }));
    }

    for h in smtp_handles {
        h.join().unwrap();
    }
    for h in mark_handles {
        h.join().unwrap();
    }

    // All .md files in the mailbox must have intact frontmatter.
    let mds = find_md_files(&alice_dir);
    assert!(
        mds.len() >= 8,
        "expected >=8 .md files (2 seed + 6 burst); got {}",
        mds.len()
    );
    for md in &mds {
        let content = std::fs::read_to_string(md).unwrap();
        assert!(
            content.starts_with("+++"),
            "torn write detected in {}: content did not start with '+++'",
            md.display()
        );
        let parts: Vec<&str> = content.splitn(3, "+++").collect();
        assert_eq!(
            parts.len(),
            3,
            "torn write in {}: expected 3 +++ parts, got {}",
            md.display(),
            parts.len()
        );
        // Frontmatter must parse as TOML; a half-written file would
        // almost certainly produce a parse error.
        let _parsed: toml::Value = toml::from_str(parts[1].trim()).unwrap_or_else(|e| {
            panic!(
                "frontmatter in {} failed to parse as TOML: {e}\n{}",
                md.display(),
                parts[1]
            )
        });
    }

    // Both seeded files were successfully marked read; MARK-READ did
    // not get corrupted by a racing ingest.
    for id in ["2025-06-01-seed1", "2025-06-01-seed2"] {
        let content = std::fs::read_to_string(alice_dir.join(format!("{id}.md"))).unwrap();
        assert!(
            content.contains("read = true"),
            "MARK-READ did not persist for {id}: {content}"
        );
    }

    stop_serve(daemon);
}

#[cfg(unix)]
#[test]
#[ignore = "requires root; MAILBOX-CRUD is root-only; run via the CI mailbox-crud-root step or AIMX_INTEGRATION_SUDO=1 sudo"]
fn concurrent_mailbox_create_and_ingest_does_not_deadlock() {
    if skip_if_mailbox_crud_not_root() {
        return;
    }
    // MAILBOX-CREATE takes the outer per-mailbox lock, then the inner
    // process-wide CONFIG_WRITE_LOCK (see `crate::mailbox_locks`).
    // Inbound ingest to the same mailbox takes only the outer lock.
    // This test races the two to confirm (a) no deadlock occurs and
    // (b) the daemon is still responsive afterwards; the message
    // lands somewhere consistent (either the new mailbox if the
    // create completed first, or catchall if ingest ran first).
    let tmp = TempDir::new().unwrap();
    setup_test_env(tmp.path());

    let port = find_free_port();
    let daemon = start_serve(tmp.path(), port);
    let runtime = tmp.path().join("run");
    let sock = runtime.join("aimx.sock");
    assert!(
        wait_for_socket(&sock, std::time::Duration::from_secs(5)),
        "UDS send socket never appeared"
    );

    // Kick off the two operations concurrently and cap the total wait
    // because a deadlock would manifest as a join that never returns.
    let create_handle = {
        let tmp_path = tmp.path().to_path_buf();
        let runtime = runtime.clone();
        std::thread::spawn(move || {
            let status = StdCommand::new(aimx_binary_path())
                .env("AIMX_CONFIG_DIR", &tmp_path)
                .env("AIMX_RUNTIME_DIR", &runtime)
                .arg("--data-dir")
                .arg(&tmp_path)
                .arg("mailbox")
                .arg("create")
                .arg("newton")
                .arg("--owner")
                .arg(current_username())
                .status()
                .expect("mailbox create did not complete");
            assert!(status.success(), "mailbox create failed: {status:?}");
        })
    };

    let ingest_handle = std::thread::spawn(move || {
        let email = concat!(
            "From: sender@example.com\r\n",
            "To: newton@agent.example.com\r\n",
            "Subject: Newton race\r\n",
            "Date: Mon, 01 Jan 2024 00:00:00 +0000\r\n",
            "Message-ID: <newton-race@example.com>\r\n",
            "\r\n",
            "race body\r\n",
        );
        smtp_send_email(
            port,
            "sender@example.com",
            &["newton@agent.example.com"],
            email,
        );
    });

    // Guard against deadlock: require both threads to finish within a
    // bounded wall-clock budget. We use a watchdog thread to panic if
    // the sum exceeds 20s; tests normally complete in <1s. The flag
    // lets us dismiss the watchdog promptly on success instead of
    // leaving a detached thread sleeping for 20s then panicking in the
    // background.
    let watchdog_cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let watchdog = {
        let cancel = std::sync::Arc::clone(&watchdog_cancel);
        std::thread::spawn(move || {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
            while std::time::Instant::now() < deadline {
                if cancel.load(std::sync::atomic::Ordering::Acquire) {
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            if cancel.load(std::sync::atomic::Ordering::Acquire) {
                return;
            }
            // If this runs, the joins below are still pending → deadlock.
            panic!("Deadlock watchdog fired: MAILBOX-CREATE + ingest did not converge");
        })
    };

    create_handle.join().unwrap();
    ingest_handle.join().unwrap();
    // Successful joins reached here; dismiss the watchdog promptly so
    // it exits rather than lingering in the background.
    watchdog_cancel.store(true, std::sync::atomic::Ordering::Release);
    watchdog.join().unwrap();

    // config.toml reflects the create.
    let config_text = std::fs::read_to_string(tmp.path().join("config.toml")).unwrap();
    assert!(
        config_text.contains("[mailboxes.newton]"),
        "mailbox create must land on disk: {config_text}"
    );

    // The message went to either newton (if create won) or catchall
    // (if ingest won), but never disappeared and never produced a
    // torn file.
    std::thread::sleep(std::time::Duration::from_millis(500));
    let newton = find_md_files(&inbox(tmp.path(), "newton"));
    let catchall = find_md_files(&inbox(tmp.path(), "catchall"));
    assert!(
        newton.len() + catchall.len() >= 1,
        "ingest lost the message: newton={} catchall={}",
        newton.len(),
        catchall.len()
    );
    for md in newton.iter().chain(catchall.iter()) {
        let content = std::fs::read_to_string(md).unwrap();
        assert!(
            content.starts_with("+++"),
            "torn write in {}: does not start with '+++'",
            md.display()
        );
        let parts: Vec<&str> = content.splitn(3, "+++").collect();
        assert_eq!(parts.len(), 3, "{}", md.display());
        let _: toml::Value = toml::from_str(parts[1].trim())
            .unwrap_or_else(|e| panic!("{} failed to parse: {e}", md.display()));
    }

    // Daemon still responsive after the race. Send a follow-up
    // message and confirm it lands (the deadlock canary).
    let followup = concat!(
        "From: sender@example.com\r\n",
        "To: alice@agent.example.com\r\n",
        "Subject: Post-race\r\n",
        "Date: Mon, 01 Jan 2024 00:00:00 +0000\r\n",
        "Message-ID: <post-race@example.com>\r\n",
        "\r\n",
        "still alive\r\n",
    );
    smtp_send_email(
        port,
        "sender@example.com",
        &["alice@agent.example.com"],
        followup,
    );
    std::thread::sleep(std::time::Duration::from_millis(500));
    let alice = find_md_files(&inbox(tmp.path(), "alice"));
    assert!(
        alice.iter().any(|p| {
            std::fs::read_to_string(p)
                .map(|c| c.contains("Post-race"))
                .unwrap_or(false)
        }),
        "daemon unresponsive after the race; follow-up message never landed"
    );

    stop_serve(daemon);
}

// ---------------------------------------------------------------------
// `aimx mailboxes show` + `aimx hooks` CLI
// ---------------------------------------------------------------------

/// `aimx mailboxes show <name>` surfaces trust, senders, hooks,
/// and counts for a configured mailbox. Verify the happy path and the
/// singular `mailbox show` alias.
#[test]
fn mailboxes_show_prints_trust_senders_hooks_and_counts() {
    let tmp = TempDir::new().unwrap();
    let config_content = format!(
        r#"domain = "agent.example.com"
data_dir = "{}"
trust = "none"

[mailboxes.catchall]
address = "*@agent.example.com"
owner = "aimx-catchall"

[mailboxes.support]
address = "support@agent.example.com"
owner = "ops"
trust = "verified"
trusted_senders = ["*@company.com", "boss@example.com"]

[[mailboxes.support.hooks]]
name = "inbound_urgent"
event = "on_receive"
cmd = ["/bin/echo", "inbound"]

[[mailboxes.support.hooks]]
name = "outbound_notify"
event = "after_send"
cmd = ["/bin/echo", "outbound"]
"#,
        tmp.path().display()
    );
    std::fs::create_dir_all(tmp.path().join("inbox").join("support")).unwrap();
    std::fs::create_dir_all(tmp.path().join("sent").join("support")).unwrap();
    std::fs::write(tmp.path().join("config.toml"), &config_content).unwrap();
    install_cached_dkim_keys(tmp.path());

    let plural = aimx_cmd(tmp.path())
        .arg("--data-dir")
        .arg(tmp.path())
        .args(["mailboxes", "show", "support"])
        .assert()
        .success();
    let out = String::from_utf8_lossy(&plural.get_output().stdout).to_string();

    for expected in [
        "support@agent.example.com",
        "verified",
        "*@company.com",
        "boss@example.com",
        "inbound_urgent",
        "outbound_notify",
        "on_receive",
        "after_send",
        "inbox:",
        "sent:",
    ] {
        assert!(
            out.contains(expected),
            "missing {expected:?} in output: {out}"
        );
    }

    // Singular alias must work too.
    aimx_cmd(tmp.path())
        .arg("--data-dir")
        .arg(tmp.path())
        .args(["mailbox", "show", "support"])
        .assert()
        .success();
}

#[test]
fn mailboxes_show_unknown_mailbox_errors() {
    let tmp = TempDir::new().unwrap();
    setup_test_env(tmp.path());
    let assert = aimx_cmd(tmp.path())
        .arg("--data-dir")
        .arg(tmp.path())
        .args(["mailboxes", "show", "ghost"])
        .assert()
        .failure();
    let stderr = String::from_utf8_lossy(&assert.get_output().stderr).to_string();
    assert!(
        stderr.contains("does not exist"),
        "expected 'does not exist' error, got: {stderr}"
    );
}

/// Build an `aimx` Command pre-wired with both `AIMX_CONFIG_DIR` and
/// `AIMX_RUNTIME_DIR` pointed at the test's tempdir. Using a per-test
/// runtime dir isolates the UDS socket path so the CLI falls back to the
/// direct-edit path even when a real `aimx serve` is running on the host
/// (e.g. on developer machines or CI boxes where the daemon is
/// installed). Without this isolation, `aimx hooks create` would hit the
/// host daemon's `/run/aimx/aimx.sock` and fail with `PROTOCOL unknown
/// verb` when the host daemon is on an older build.
fn aimx_cmd_isolated(tmp: &Path) -> Command {
    let runtime = tmp.join("run");
    std::fs::create_dir_all(&runtime).ok();
    let mut cmd = Command::cargo_bin("aimx").unwrap();
    cmd.env("AIMX_CONFIG_DIR", tmp);
    cmd.env("AIMX_RUNTIME_DIR", &runtime);
    cmd.env("AIMX_SANDBOX_FORCE_FALLBACK", "1");
    // `aimx hooks create --cmd` is root-only. CI runs
    // non-root, so tests set this test-only escape hatch to exercise
    // the direct-write + SIGHUP path on behalf of the fake-root
    // operator. Production systemd units never pass this env var.
    cmd.env("AIMX_TEST_SKIP_AUTHZ_CHECK", "1");
    cmd
}

/// `aimx hooks create` + `aimx hooks list` roundtrip.
/// Daemon is not running (AIMX_RUNTIME_DIR points at an empty dir), so
/// the CLI falls back to direct config.toml edit and prints a restart
/// hint. That path covers the full flag validation and the on-disk
/// write.
#[test]
fn hooks_create_and_list_roundtrip_direct_edit() {
    let tmp = TempDir::new().unwrap();
    setup_test_env(tmp.path());

    let create = aimx_cmd_isolated(tmp.path())
        .arg("--data-dir")
        .arg(tmp.path())
        .args([
            "hooks",
            "create",
            "--mailbox",
            "alice",
            "--event",
            "on_receive",
            "--cmd",
            r#"["/bin/echo", "hi"]"#,
            "--name",
            "alice_greeter",
        ])
        .assert()
        .success();
    let create_out = String::from_utf8_lossy(&create.get_output().stdout).to_string();
    assert!(
        create_out.contains("Hook created"),
        "create output: {create_out}"
    );
    assert!(
        create_out.contains("alice_greeter"),
        "create output should echo the hook name: {create_out}"
    );
    // A restart hint is expected on the socket-missing fallback path.
    assert!(
        create_out.contains("restart")
            || create_out.contains("Hint")
            || create_out.contains("next start")
            || create_out.contains("Note:"),
        "expected restart hint on socket-missing fallback: {create_out}"
    );

    let list = aimx_cmd_isolated(tmp.path())
        .arg("--data-dir")
        .arg(tmp.path())
        .args(["hooks", "list"])
        .assert()
        .success();
    let list_out = String::from_utf8_lossy(&list.get_output().stdout).to_string();
    assert!(list_out.contains("alice"), "list output: {list_out}");
    assert!(list_out.contains("on_receive"), "list output: {list_out}");
    assert!(
        list_out.contains("alice_greeter"),
        "list output: {list_out}"
    );
}

/// `aimx hooks create --timeout-secs 5` round-trips through the
/// direct-edit fallback into `config.toml` and shows up in
/// `aimx hooks list`. The `--stdin` flag is gone; the email is always
/// piped to hooks.
#[test]
fn hooks_create_with_timeout_flag_persists_to_config() {
    let tmp = TempDir::new().unwrap();
    setup_test_env(tmp.path());

    let create = aimx_cmd_isolated(tmp.path())
        .arg("--data-dir")
        .arg(tmp.path())
        .args([
            "hooks",
            "create",
            "--mailbox",
            "alice",
            "--event",
            "on_receive",
            "--cmd",
            r#"["/bin/echo", "noinput"]"#,
            "--name",
            "timeouthook",
            "--timeout-secs",
            "5",
        ])
        .assert()
        .success();
    let out = String::from_utf8_lossy(&create.get_output().stdout).to_string();
    assert!(out.contains("Hook created"), "{out}");

    let toml_contents = std::fs::read_to_string(tmp.path().join("config.toml")).unwrap();
    assert!(
        !toml_contents.contains("stdin"),
        "stdin field must not appear in config.toml: {toml_contents}"
    );
    assert!(
        toml_contents.contains("timeout_secs = 5"),
        "timeout_secs override missing from config.toml: {toml_contents}"
    );

    let list = aimx_cmd_isolated(tmp.path())
        .arg("--data-dir")
        .arg(tmp.path())
        .args(["hooks", "list"])
        .assert()
        .success();
    let list_out = String::from_utf8_lossy(&list.get_output().stdout).to_string();
    assert!(list_out.contains("timeouthook"), "{list_out}");
    assert!(list_out.contains("5"), "{list_out}");
}

/// `aimx hooks create --timeout-secs 700` is rejected because the value
/// exceeds the schema cap of 600s.
#[test]
fn hooks_create_rejects_timeout_secs_over_max() {
    let tmp = TempDir::new().unwrap();
    setup_test_env(tmp.path());

    let attempt = aimx_cmd_isolated(tmp.path())
        .arg("--data-dir")
        .arg(tmp.path())
        .args([
            "hooks",
            "create",
            "--mailbox",
            "alice",
            "--event",
            "on_receive",
            "--cmd",
            r#"["/bin/true"]"#,
            "--name",
            "toolong",
            "--timeout-secs",
            "700",
        ])
        .assert()
        .failure();
    let stderr = String::from_utf8_lossy(&attempt.get_output().stderr).to_string();
    assert!(
        stderr.contains("timeout_secs") || stderr.contains("600"),
        "expected timeout_secs rejection: {stderr}"
    );
}

#[test]
fn hooks_create_anonymous_prints_derived_name() {
    let tmp = TempDir::new().unwrap();
    setup_test_env(tmp.path());

    let create = aimx_cmd_isolated(tmp.path())
        .arg("--data-dir")
        .arg(tmp.path())
        .args([
            "hooks",
            "create",
            "--mailbox",
            "alice",
            "--event",
            "on_receive",
            "--cmd",
            r#"["/bin/echo", "anon"]"#,
        ])
        .assert()
        .success();
    let out = String::from_utf8_lossy(&create.get_output().stdout).to_string();
    assert!(out.contains("Hook created"), "{out}");

    // The on-disk config must not have a `name =` entry.
    let toml_contents = std::fs::read_to_string(tmp.path().join("config.toml")).unwrap();
    let parsed: toml::Value = toml::from_str(&toml_contents).unwrap();
    let hooks = parsed
        .get("mailboxes")
        .and_then(|m| m.get("alice"))
        .and_then(|a| a.get("hooks"))
        .and_then(|h| h.as_array())
        .unwrap();
    assert_eq!(hooks.len(), 1);
    assert!(
        hooks[0].get("name").is_none(),
        "anonymous hook must not write name = ..., got: {toml_contents}"
    );
}

#[test]
fn hooks_alias_works() {
    let tmp = TempDir::new().unwrap();
    setup_test_env(tmp.path());
    aimx_cmd_isolated(tmp.path())
        .arg("--data-dir")
        .arg(tmp.path())
        .args(["hook", "list"])
        .assert()
        .success();
}

#[test]
fn hooks_create_rejects_invalid_name() {
    let tmp = TempDir::new().unwrap();
    setup_test_env(tmp.path());

    let assert = aimx_cmd_isolated(tmp.path())
        .arg("--data-dir")
        .arg(tmp.path())
        .args([
            "hooks",
            "create",
            "--mailbox",
            "alice",
            "--event",
            "on_receive",
            "--cmd",
            r#"["/bin/echo", "hi"]"#,
            "--name",
            "bad name!",
        ])
        .assert()
        .failure();
    let stderr = String::from_utf8_lossy(&assert.get_output().stderr).to_string();
    assert!(
        stderr.contains("--name") || stderr.contains("hook name"),
        "expected hook-name validation error: {stderr}"
    );
}

#[test]
fn hooks_create_rejects_unknown_event_at_parse_time() {
    let tmp = TempDir::new().unwrap();
    setup_test_env(tmp.path());

    let assert = aimx_cmd_isolated(tmp.path())
        .arg("--data-dir")
        .arg(tmp.path())
        .args([
            "hooks",
            "create",
            "--mailbox",
            "alice",
            "--event",
            "nope",
            "--cmd",
            r#"["/bin/echo", "hi"]"#,
        ])
        .assert()
        .failure();
    let stderr = String::from_utf8_lossy(&assert.get_output().stderr).to_string();
    assert!(
        stderr.contains("nope") || stderr.contains("invalid value"),
        "expected clap value-parse error: {stderr}"
    );
}

#[test]
fn hooks_delete_prompts_and_removes_via_direct_edit() {
    let tmp = TempDir::new().unwrap();
    setup_test_env(tmp.path());

    let create = aimx_cmd_isolated(tmp.path())
        .arg("--data-dir")
        .arg(tmp.path())
        .args([
            "hooks",
            "create",
            "--mailbox",
            "alice",
            "--event",
            "on_receive",
            "--cmd",
            r#"["/bin/echo", "hi"]"#,
            "--name",
            "delete_me",
        ])
        .assert()
        .success();
    let create_out = String::from_utf8_lossy(&create.get_output().stdout).to_string();
    assert!(create_out.contains("delete_me"), "{create_out}");

    aimx_cmd_isolated(tmp.path())
        .arg("--data-dir")
        .arg(tmp.path())
        .args(["hooks", "delete", "delete_me", "--yes"])
        .assert()
        .success();

    let list = aimx_cmd_isolated(tmp.path())
        .arg("--data-dir")
        .arg(tmp.path())
        .args(["hooks", "list"])
        .assert()
        .success();
    let out = String::from_utf8_lossy(&list.get_output().stdout).to_string();
    assert!(!out.contains("delete_me"), "hook should be gone: {out}");
}

#[test]
fn hooks_delete_unknown_name_errors() {
    let tmp = TempDir::new().unwrap();
    setup_test_env(tmp.path());
    let assert = aimx_cmd_isolated(tmp.path())
        .arg("--data-dir")
        .arg(tmp.path())
        .args(["hooks", "delete", "does_not_exist", "--yes"])
        .assert()
        .failure();
    let stderr = String::from_utf8_lossy(&assert.get_output().stderr).to_string();
    assert!(stderr.contains("not found"), "stderr: {stderr}");
}

// ---------------------------------------------------------------------
// UDS HOOK-CREATE / HOOK-DELETE end-to-end
// ---------------------------------------------------------------------

/// Spin up `aimx serve`, issue `aimx hooks create --cmd`, confirm the
/// CLI routed through the daemon's UDS HOOK-CREATE verb so `config.toml`
/// hot-swaps without a restart. The success path prints the
/// `(live via daemon)` marker and no `Hint:` restart banner.
#[test]
fn hooks_raw_cmd_sighup_hot_swaps_config() {
    let tmp = TempDir::new().unwrap();
    setup_test_env(tmp.path());
    let port = find_free_port();
    let daemon = start_serve(tmp.path(), port);

    let runtime = tmp.path().join("run");
    let create = Command::cargo_bin("aimx")
        .unwrap()
        .env("AIMX_CONFIG_DIR", tmp.path())
        .env("AIMX_RUNTIME_DIR", &runtime)
        .env("AIMX_TEST_SKIP_AUTHZ_CHECK", "1")
        .arg("--data-dir")
        .arg(tmp.path())
        .args([
            "hooks",
            "create",
            "--mailbox",
            "alice",
            "--event",
            "on_receive",
            "--cmd",
            r#"["/bin/echo", "via-daemon"]"#,
        ])
        .assert()
        .success();
    let create_out = String::from_utf8_lossy(&create.get_output().stdout).to_string();
    assert!(
        create_out.contains("Hook created"),
        "create output: {create_out}"
    );
    // The CLI now routes through the daemon's UDS HOOK-CREATE verb —
    // the daemon atomically rewrites config.toml and hot-swaps the
    // in-memory Config so SIGHUP is not required. Positive signal:
    // stdout carries the `(live via daemon)` marker. Negative
    // signal: no `Hint:` restart banner (the daemon-down fallback
    // path would have produced one).
    assert!(
        create_out.contains("live via daemon"),
        "daemon-success should print the live-via-daemon marker: {create_out}"
    );
    assert!(
        !create_out.contains("Hint:"),
        "daemon-success should not print restart hint: {create_out}"
    );

    // The daemon writes config.toml atomically when handling
    // HOOK-CREATE. The new hook argv must appear there.
    let content = std::fs::read_to_string(tmp.path().join("config.toml")).unwrap();
    assert!(
        content.contains("via-daemon"),
        "config.toml should contain new hook: {content}"
    );

    stop_serve(daemon);
}

/// Mirror of `hooks_create_anonymous_prints_derived_name` but over the
/// UDS/daemon path: confirms the CLI prints the derived 12-hex-char name
/// returned by the daemon in `submit_hook_create_via_daemon`, and that
/// the daemon did NOT write a `name =` line to `config.toml`.
#[test]
fn hooks_create_anonymous_prints_derived_name_via_daemon() {
    let tmp = TempDir::new().unwrap();
    setup_test_env(tmp.path());
    let port = find_free_port();
    let daemon = start_serve(tmp.path(), port);

    let runtime = tmp.path().join("run");
    let create = Command::cargo_bin("aimx")
        .unwrap()
        .env("AIMX_CONFIG_DIR", tmp.path())
        .env("AIMX_RUNTIME_DIR", &runtime)
        .env("AIMX_TEST_SKIP_AUTHZ_CHECK", "1")
        .arg("--data-dir")
        .arg(tmp.path())
        .args([
            "hooks",
            "create",
            "--mailbox",
            "alice",
            "--event",
            "on_receive",
            "--cmd",
            r#"["/bin/echo", "daemon-anon"]"#,
        ])
        .assert()
        .success();
    let out = String::from_utf8_lossy(&create.get_output().stdout).to_string();
    assert!(out.contains("Hook created"), "{out}");
    // When the daemon is up, SIGHUP succeeds and the
    // CLI prints "Reload:" rather than the socket-missing "Hint:"
    // restart banner.
    assert!(
        !out.contains("Hint:"),
        "daemon-success should not print restart hint: {out}"
    );

    // Compute the expected derived name (mirrors `derive_hook_name` in
    // src/hook.rs) and assert it was printed by the CLI. Argv elements
    // are joined by 0x1F so `["/bin/echo", "daemon-anon"]` hashes
    // distinctly from a string fused on whitespace.
    let expected = {
        use sha2::{Digest, Sha256};
        let mut hasher = Sha256::new();
        hasher.update(b"on_receive");
        hasher.update([0x1F]);
        hasher.update(b"/bin/echo");
        hasher.update([0x1F]);
        hasher.update(b"daemon-anon");
        hasher.update([0x1F]);
        hasher.update([0u8]); // fire_on_untrusted = false
        let digest = hasher.finalize();
        let mut s = String::with_capacity(12);
        for b in digest.iter().take(6) {
            s.push_str(&format!("{b:02x}"));
        }
        s
    };
    assert_eq!(expected.len(), 12);
    assert!(
        out.contains(&expected),
        "expected derived name '{expected}' in CLI output: {out}"
    );

    // The daemon-rewritten config must not have a `name =` entry.
    let content = std::fs::read_to_string(tmp.path().join("config.toml")).unwrap();
    let parsed: toml::Value = toml::from_str(&content).unwrap();
    let hooks = parsed
        .get("mailboxes")
        .and_then(|m| m.get("alice"))
        .and_then(|a| a.get("hooks"))
        .and_then(|h| h.as_array())
        .unwrap();
    assert_eq!(hooks.len(), 1);
    assert!(
        hooks[0].get("name").is_none(),
        "anonymous hook must not write name = ..., got: {content}"
    );

    stop_serve(daemon);
}

// ---------------------------------------------------------------------
// MCP hook tools (hook_create, hook_list, hook_delete)
// ---------------------------------------------------------------------

/// `hook_create` without a running daemon returns a precise
/// socket-missing error rather than panicking. Exercised against the
/// new MCP signature (`mailbox`, `event`, `cmd: Vec<String>`).
#[test]
fn mcp_hook_create_without_daemon_reports_missing_socket() {
    let tmp = TempDir::new().unwrap();
    setup_test_env(tmp.path());

    let mut client = McpClient::spawn(tmp.path());
    client.initialize();

    let resp = client.call_tool(
        "hook_create",
        serde_json::json!({
            "mailbox": "alice",
            "event": "on_receive",
            "cmd": ["/bin/echo", "hi"]
        }),
    );
    assert!(
        is_tool_error(&resp),
        "expected error when daemon missing: {resp}"
    );
    let text = get_tool_text(&resp);
    assert!(
        text.contains("daemon not running"),
        "expected socket-missing message: {text}"
    );

    client.shutdown();
}

/// `hook_create` against a mailbox the caller does not own returns
/// the canonical "not authorized" error from the auth predicate. The
/// daemon is not even started: the MCP-side pre-flight check rejects
/// before any wire I/O.
#[test]
fn mcp_hook_create_unowned_mailbox_returns_not_authorized() {
    // Skip this assertion when running the test runner as root (CI
    // tier 1 runs in containers as root). The auth predicate
    // unconditionally allows root, so the unowned-mailbox negative
    // path can only fire as a non-root caller.
    let is_root = unsafe { libc::geteuid() == 0 };
    if is_root {
        eprintln!("skipping unowned-mailbox negative-auth test under root caller");
        return;
    }

    // Plant a mailbox owned by `nobody` (uid !=
    // current_euid() on every supported Linux deployment) so the
    // current test user is *not* the owner.
    let tmp = TempDir::new().unwrap();
    let owner = current_username();
    let config = format!(
        "domain = \"agent.example.com\"\ndata_dir = \"{tmp_dir}\"\n\n\
         [mailboxes.catchall]\naddress = \"*@agent.example.com\"\nowner = \"{owner}\"\n\n\
         [mailboxes.alice]\naddress = \"alice@agent.example.com\"\nowner = \"{owner}\"\n\n\
         [mailboxes.foreign]\naddress = \"foreign@agent.example.com\"\nowner = \"nobody\"\n",
        tmp_dir = tmp.path().display()
    );
    for sub in [
        "inbox/catchall",
        "sent/catchall",
        "inbox/alice",
        "sent/alice",
        "inbox/foreign",
        "sent/foreign",
    ] {
        let dir = tmp.path().join(sub);
        std::fs::create_dir_all(&dir).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
    }
    std::fs::write(tmp.path().join("config.toml"), &config).unwrap();
    install_cached_dkim_keys(tmp.path());

    let port = find_free_port();
    let daemon = start_serve(tmp.path(), port);
    let sock = tmp.path().join("run").join("aimx.sock");
    assert!(
        wait_for_socket(&sock, std::time::Duration::from_secs(5)),
        "UDS socket never appeared"
    );

    let mut client = McpClient::spawn(tmp.path());
    client.initialize();

    let resp = client.call_tool(
        "hook_create",
        serde_json::json!({
            "mailbox": "foreign",
            "event": "on_receive",
            "cmd": ["/bin/echo", "hi"]
        }),
    );
    assert!(is_tool_error(&resp), "{resp}");
    let text = get_tool_text(&resp);
    // NFR2 opacity: a hook_create on a mailbox the caller doesn't own
    // surfaces as the same opaque "does not exist" error a missing
    // mailbox would, because the daemon's MAILBOX-LIST is filtered
    // by SO_PEERCRED. The MCP-side pre-flight cannot distinguish
    // "doesn't exist" from "exists but unowned" client-side.
    assert!(
        text.contains("does not exist"),
        "expected opaque does-not-exist error, got: {text}"
    );

    client.shutdown();
    stop_serve(daemon);
}

/// `hook_delete` against a hook that does not exist returns the
/// canonical "not found" error from the daemon's HOOK-DELETE handler.
/// The MCP tool is now a thin pass-through (no client-side
/// `load_config` pre-flight), so the daemon must be running.
#[test]
fn mcp_hook_delete_unknown_hook_returns_not_found() {
    let tmp = TempDir::new().unwrap();
    setup_test_env(tmp.path());

    let port = find_free_port();
    let daemon = start_serve(tmp.path(), port);
    let sock = tmp.path().join("run").join("aimx.sock");
    assert!(
        wait_for_socket(&sock, std::time::Duration::from_secs(5)),
        "UDS socket never appeared"
    );

    let mut client = McpClient::spawn(tmp.path());
    client.initialize();

    let resp = client.call_tool("hook_delete", serde_json::json!({"name": "no_such_hook"}));
    assert!(is_tool_error(&resp), "{resp}");
    let text = get_tool_text(&resp);
    assert!(
        text.contains("not found") || text.contains("ENOENT"),
        "{text}"
    );

    client.shutdown();
    stop_serve(daemon);
}

/// `hook_delete` against a hook that lives on a mailbox the caller
/// does not own surfaces as `not found` — same shape as `hook_delete`
/// for a name that doesn't exist anywhere. The MCP error must NOT
/// leak the foreign mailbox name through a "caller does not own
/// mailbox 'X'" message.
#[test]
fn mcp_hook_delete_unowned_hook_returns_not_found_not_unauthorized() {
    let is_root = unsafe { libc::geteuid() == 0 };
    if is_root {
        eprintln!("skipping unowned-hook negative test under root caller");
        return;
    }

    let tmp = TempDir::new().unwrap();
    let owner = current_username();
    let config = format!(
        "domain = \"agent.example.com\"\ndata_dir = \"{tmp_dir}\"\n\n\
         [mailboxes.catchall]\naddress = \"*@agent.example.com\"\nowner = \"{owner}\"\n\n\
         [mailboxes.alice]\naddress = \"alice@agent.example.com\"\nowner = \"{owner}\"\n\n\
         [mailboxes.othersmbx]\naddress = \"other@agent.example.com\"\nowner = \"nobody\"\n\n\
         [[mailboxes.othersmbx.hooks]]\nname = \"hidden_hook\"\nevent = \"on_receive\"\n\
         cmd = [\"/bin/true\"]\n",
        tmp_dir = tmp.path().display()
    );
    for sub in [
        "inbox/catchall",
        "sent/catchall",
        "inbox/alice",
        "sent/alice",
        "inbox/othersmbx",
        "sent/othersmbx",
    ] {
        let dir = tmp.path().join(sub);
        std::fs::create_dir_all(&dir).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
    }
    std::fs::write(tmp.path().join("config.toml"), &config).unwrap();
    install_cached_dkim_keys(tmp.path());

    let port = find_free_port();
    let daemon = start_serve(tmp.path(), port);
    let sock = tmp.path().join("run").join("aimx.sock");
    assert!(
        wait_for_socket(&sock, std::time::Duration::from_secs(5)),
        "UDS socket never appeared"
    );

    let mut client = McpClient::spawn(tmp.path());
    client.initialize();

    let resp = client.call_tool("hook_delete", serde_json::json!({"name": "hidden_hook"}));
    assert!(is_tool_error(&resp), "{resp}");
    let text = get_tool_text(&resp);
    assert!(
        text.contains("not found") || text.contains("ENOENT"),
        "expected `not found` (or ENOENT): {text}"
    );
    // Crucially, the foreign mailbox name must not appear in the error
    // shown to a non-owner caller.
    assert!(
        !text.contains("othersmbx"),
        "MCP error must not leak unowned mailbox name: {text}"
    );
    assert!(
        !text.contains("not authorized"),
        "MCP error must collapse `not authorized` to `not found`: {text}"
    );

    client.shutdown();
    stop_serve(daemon);
}

/// `hook_list` with no hooks configured returns `[]`. Exercises the
/// new MCP `hook_list` tool's empty-output shape end-to-end through
/// the new `HOOK-LIST` UDS verb.
#[test]
fn mcp_hook_list_empty_returns_empty_array() {
    let tmp = TempDir::new().unwrap();
    setup_test_env(tmp.path());

    let port = find_free_port();
    let daemon = start_serve(tmp.path(), port);
    let sock = tmp.path().join("run").join("aimx.sock");
    assert!(
        wait_for_socket(&sock, std::time::Duration::from_secs(5)),
        "UDS socket never appeared"
    );

    let mut client = McpClient::spawn(tmp.path());
    client.initialize();

    let resp = client.call_tool("hook_list", serde_json::json!({}));
    let text = get_tool_text(&resp);
    assert_eq!(text, "[]", "{text}");

    client.shutdown();
    stop_serve(daemon);
}

// ---------------------------------------------------------------------
// Per-mailbox chown on ingest + state rewrites.
// The non-root form of these tests verifies mode (file permission bits)
// without requiring an actual chown syscall — the tester's own uid is
// already the mailbox owner (via `testowner` resolver shim), so the
// chown syscall is a no-op from the kernel's perspective. Root-gated
// cross-user isolation lives in `tests/isolation.rs`.
// ---------------------------------------------------------------------

/// Ingest an email and verify the `.md` file exists. When the ingest
/// runs as root (CI `integration-isolation` job or local sudo run), the
/// chown+chmod land cleanly and the file's mode is asserted to be
/// `0o600`. When the ingest runs as a regular user and the configured
/// `owner = "ops"` doesn't match, `chown_as_owner` fails with
/// PermissionDenied; the warning is logged but the file still exists
/// (and inherits umask) — this test only validates that the ingest
/// itself succeeds end-to-end even when chown fails, so the failure
/// mode is graceful (PRD §6.3: chown failure is non-fatal because the
/// containing dir is already `0o700 <owner>:<owner>` on a properly
/// provisioned host).
#[cfg(unix)]
#[test]
fn ingest_succeeds_and_chown_failure_is_nonfatal() {
    use std::os::unix::fs::MetadataExt;

    let tmp = TempDir::new().unwrap();
    setup_test_env(tmp.path());

    let eml = b"From: sender@example.com\r\n\
                 To: alice@agent.example.com\r\n\
                 Subject: mode test\r\n\
                 Message-ID: <mode-test@example.com>\r\n\
                 Date: Thu, 01 Jan 2026 12:00:00 +0000\r\n\
                 \r\n\
                 body\r\n";
    let mut ingest = StdCommand::new(aimx_binary_path())
        .env("AIMX_CONFIG_DIR", tmp.path())
        .env("AIMX_DATA_DIR", tmp.path())
        .arg("ingest")
        .arg("alice@agent.example.com")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("aimx ingest failed to spawn");
    ingest
        .stdin
        .as_mut()
        .unwrap()
        .write_all(eml)
        .expect("write stdin");
    let out = ingest.wait_with_output().expect("ingest wait");
    assert!(
        out.status.success(),
        "ingest must succeed even when chown fails; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let alice_inbox = tmp.path().join("inbox").join("alice");
    let md_files = find_md_files(&alice_inbox);
    assert_eq!(md_files.len(), 1, "exactly one delivered email");
    let meta = std::fs::metadata(&md_files[0]).unwrap();
    let is_root = unsafe { libc::geteuid() == 0 };
    if is_root {
        // Root: chown to root succeeds; chmod sets mode to 0o600.
        assert_eq!(
            meta.mode() & 0o777,
            0o600,
            "root ingest must produce 0o600 .md files via post-persist chown"
        );
    }
    // Non-root: chown fails silently and the file stays at umask default.
    // The test only asserts success (no panic) on the non-root path;
    // the real isolation assertion lives in tests/isolation.rs which
    // creates real users for both alice and bob.
}

/// `aimx --version` must render
/// `AIMX (AI Mail Exchange) version <tag> (<git-sha>) <target-triple> built <date>`
/// — all four metadata fields present on a single line behind the banner.
#[test]
fn aimx_version_renders_full_metadata() {
    let output = Command::cargo_bin("aimx")
        .unwrap()
        .arg("--version")
        .output()
        .expect("run aimx --version");
    assert!(output.status.success(), "aimx --version exited non-zero");

    let stdout = String::from_utf8(output.stdout).expect("stdout is utf-8");
    let line = stdout
        .lines()
        .next()
        .expect("at least one output line")
        .to_string();

    // Output must be exactly
    // `AIMX (AI Mail Exchange) version <tag> (<sha>) <target> built <date>`.
    // An earlier revision of this test stripped an optional leading `aimx `
    // and would have silently accepted a duplicated banner from clap's
    // default `ArgAction::Version`; we assert the exact shape here so that
    // regression fails loudly.
    const BANNER: &str = "AIMX (AI Mail Exchange) version ";
    assert!(
        line.starts_with(BANNER),
        "version line missing banner prefix: {line:?}"
    );
    let rest = &line[BANNER.len()..];
    // Reject `AIMX (AI Mail Exchange) version AIMX ...` — the duplicate-banner
    // regression that clap's default `--version` would produce.
    assert!(
        !rest.starts_with("AIMX "),
        "duplicate banner reintroduced: {line:?}"
    );
    // First token after the banner must be the tag.
    let tag = rest
        .split(' ')
        .next()
        .unwrap_or_else(|| panic!("version line missing tag token: {line:?}"));
    assert!(!tag.is_empty(), "tag token must be non-empty: {line:?}");
    // Tags are bare SemVer — the baked
    // `RELEASE_TAG` goes through `build.rs::strip_legacy_v_prefix`, so a
    // leading `v` on this token would signal a regression in either
    // `build.rs` or the release tagging convention. Reject it loudly.
    assert!(
        !tag.starts_with('v'),
        "tag token must not carry a leading `v` (bare SemVer): {line:?}"
    );
    // The tag must either begin with a digit (bare SemVer like `0.1.0`,
    // `0.0.0-fixture`, `0.0.0-fixture-12-gabcdef1-dirty`) or equal `dev`
    // (the build.rs fallback when `git describe` finds no tag).
    assert!(
        tag == "dev" || tag.chars().next().is_some_and(|c| c.is_ascii_digit()),
        "tag token must be bare SemVer or `dev`: {tag:?} in {line:?}"
    );

    // Remainder must match: `<tag> (<hex-sha>) <target> built <YYYY-MM-DD>`.
    // Parenthesised SHA.
    let open = rest
        .find(" (")
        .unwrap_or_else(|| panic!("version line missing ` (<sha>)` segment: {line:?}"));
    let close_rel = rest[open..]
        .find(") ")
        .unwrap_or_else(|| panic!("version line missing `) ` after sha: {line:?}"));
    let sha = &rest[open + 2..open + close_rel];
    assert!(
        !sha.is_empty() && sha.chars().all(|c| c.is_ascii_hexdigit()),
        "git sha segment not hex: {sha:?} in {line:?}"
    );

    let after_sha = &rest[open + close_rel + 2..];
    let built_idx = after_sha
        .find(" built ")
        .unwrap_or_else(|| panic!("version line missing ` built ` trailer: {line:?}"));
    let target = &after_sha[..built_idx];
    assert!(
        target == "unknown" || target.matches('-').count() >= 2,
        "target triple looks wrong: {target:?}"
    );

    let date = &after_sha[built_idx + " built ".len()..];
    assert_eq!(date.len(), 10, "build date not YYYY-MM-DD: {date:?}");
    assert_eq!(
        &date[4..5],
        "-",
        "build date missing first hyphen: {date:?}"
    );
    assert_eq!(
        &date[7..8],
        "-",
        "build date missing second hyphen: {date:?}"
    );
    assert!(date[..4].chars().all(|c| c.is_ascii_digit()));
    assert!(date[5..7].chars().all(|c| c.is_ascii_digit()));
    assert!(date[8..10].chars().all(|c| c.is_ascii_digit()));
}

// ===== aimx agents <command> CLI surface =================================

/// `aimx agents list` is the canonical wiring-state dump. Must succeed
/// and emit at least one of the registered agent names.
#[test]
fn agents_list_works() {
    Command::cargo_bin("aimx")
        .unwrap()
        .args(["agents", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("claude-code"));
}

#[test]
fn agents_setup_list_works() {
    Command::cargo_bin("aimx")
        .unwrap()
        .args(["agents", "setup", "--list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("claude-code"));
}

/// NanoClaw was added as the eighth supported agent. A separate assertion
/// (rather than folding it into `agents_setup_list_works`) keeps the failure
/// message specific if NanoClaw is ever accidentally dropped from
/// `registry()`. Also confirms the `$NANOCLAW_HOME` template surfaces in
/// the printed destination so users see the env var without reading the
/// guide.
#[test]
fn agents_setup_list_includes_nanoclaw() {
    Command::cargo_bin("aimx")
        .unwrap()
        .args(["agents", "setup", "--list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("nanoclaw"))
        .stdout(predicate::str::contains("NanoClaw"))
        .stdout(predicate::str::contains("nanoclaw/skills/aimx"));
}

/// `aimx agents remove <unknown>` must surface an explicit "Unknown agent"
/// error rather than a clap usage hint or a silent no-op.
#[test]
fn agents_remove_unknown_agent_errors_clearly() {
    Command::cargo_bin("aimx")
        .unwrap()
        .args(["agents", "remove", "nonesuch"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("Unknown agent"))
        .stderr(predicate::str::contains("nonesuch"));
}

/// `aimx agent-setup` was a legacy hyphenated alias retired alongside
/// the canonical `aimx agents setup` migration. It must now error with
/// clap's standard "unrecognized subcommand" message instead of silently
/// dispatching, so scripts pinned to the old form fail loudly and
/// surface in CI / journalctl.
#[test]
fn aimx_agent_setup_legacy_form_errors() {
    Command::cargo_bin("aimx")
        .unwrap()
        .args(["agent-setup", "--list"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("unrecognized subcommand"))
        .stderr(predicate::str::contains("agent-setup"));
}

/// `aimx agent setup` (singular noun) was a clap alias retired alongside
/// the canonical `aimx agents setup` migration. It must now error with
/// clap's standard "unrecognized subcommand" message rather than silently
/// dispatching to `agents`.
#[test]
fn aimx_agent_singular_form_errors() {
    Command::cargo_bin("aimx")
        .unwrap()
        .args(["agent", "setup", "--list"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("unrecognized subcommand"))
        .stderr(predicate::str::contains("agent"));
}

/// End-to-end: a booted `aimx serve` answers the `AIMX/1 VERSION` verb
/// over the UDS with a JSON body that matches the binary's compile-time
/// `aimx --version` output. This is the contract `aimx doctor` relies
/// on for drift detection.
#[cfg(unix)]
#[test]
fn uds_version_verb_returns_running_daemon_metadata() {
    use std::io::{Read as _, Write as _};
    use std::os::unix::net::UnixStream;

    let tmp = TempDir::new().unwrap();
    setup_test_env(tmp.path());
    let port = find_free_port();
    let child = start_serve(tmp.path(), port);

    let sock = tmp.path().join("run").join("aimx.sock");
    assert!(
        wait_for_socket(&sock, std::time::Duration::from_secs(5)),
        "UDS send socket never appeared"
    );

    let mut stream = UnixStream::connect(&sock).expect("connect to aimx.sock");
    stream
        .write_all(b"AIMX/1 VERSION\n\n")
        .expect("write VERSION request");
    stream
        .shutdown(std::net::Shutdown::Write)
        .expect("half-close write side");

    let mut buf = Vec::with_capacity(512);
    stream.read_to_end(&mut buf).expect("read response");

    let text = std::str::from_utf8(&buf).expect("response is utf8");
    assert!(
        text.starts_with("AIMX/1 OK\n"),
        "unexpected status line: {text}",
    );
    let header_end = buf
        .windows(2)
        .position(|w| w == b"\n\n")
        .expect("response has header/body separator");
    let body = &buf[header_end + 2..];
    let body_str = std::str::from_utf8(body).expect("body is utf8");

    // The JSON body must carry every field of `VersionResponse`. We
    // do not pull in serde_json here just to parse — pattern-matching
    // on substrings is enough to lock the wire shape.
    for needle in [
        "\"tag\":",
        "\"git_hash\":",
        "\"target\":",
        "\"build_date\":",
    ] {
        assert!(
            body_str.contains(needle),
            "response missing field {needle:?}: {body_str}",
        );
    }

    // The reported tag must match what the test binary itself prints
    // for `aimx --version` — in test builds this is whatever
    // `crate::version::release_tag()` resolves to. Cross-check by
    // running the bin and parsing the token immediately after the
    // literal "version" keyword in the banner
    // (`AIMX (AI Mail Exchange) version <tag> (<sha>) <target> built <date>`).
    let v_out = std::process::Command::new(aimx_binary_path())
        .arg("--version")
        .output()
        .expect("aimx --version");
    let v_stdout = String::from_utf8(v_out.stdout).unwrap();
    let mut tokens = v_stdout.split_whitespace();
    for tok in tokens.by_ref() {
        if tok == "version" {
            break;
        }
    }
    let local_tag = tokens
        .next()
        .expect("aimx --version emits a tag after the \"version\" keyword");
    assert!(
        body_str.contains(local_tag),
        "daemon tag missing local {local_tag:?} in {body_str}",
    );

    stop_serve(child);
}

// ---------------------------------------------------------------------------
// End-to-end production-perm smoke tests.
//
// Every MCP tool must work against a `chmod 0600 root:root` config +
// running `aimx serve`. The bug class these guard against: the
// non-root MCP process trying (and failing with EACCES) to read the
// root-owned `/etc/aimx/config.toml`. The structural guard (the
// `load_config` deletion) prevents the most-direct reintroduction;
// these run-time tests catch any structurally-different reintroduction
// (e.g. a future tool that grabs a path via a different code path).
//
// The tests are gated on:
//   - `cfg(unix)` (uid model only meaningful on Unix)
//   - `#[ignore]` (only run when the test binary is invoked with
//     `--ignored`)
//   - `AIMX_INTEGRATION_SUDO=1` env var (defense-in-depth so a casual
//     `cargo test -- --ignored` skips them)
//   - `geteuid() == 0` (the daemon must be started by root to chown the
//     config to `root:root` and bind `/run/aimx/`).
//
// They reuse the `aimx-test-alice` system user provisioned by the
// `mailbox-dir-perms-isolation` CI lane and spawn `aimx mcp` under
// `runuser -u aimx-test-alice` so the MCP process really is non-root.
// One full-cycle test per category (mailbox / email / hook) collectively
// exercises all 12 MCP tools.
// ---------------------------------------------------------------------------

#[cfg(unix)]
const PRODPERM_USER: &str = "aimx-test-alice";

#[cfg(unix)]
fn prodperm_skip() -> bool {
    if std::env::var_os("AIMX_INTEGRATION_SUDO").is_none() {
        eprintln!("skipping: production-perm smoke requires AIMX_INTEGRATION_SUDO=1 + sudo");
        return true;
    }
    if unsafe { libc::geteuid() } != 0 {
        eprintln!("skipping: production-perm smoke must run as root (sudo lane)");
        return true;
    }
    let id = StdCommand::new("id")
        .arg(PRODPERM_USER)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !id {
        eprintln!(
            "skipping: production-perm smoke requires {PRODPERM_USER} \
             system user (CI lane provisions this)"
        );
        return true;
    }
    false
}

#[cfg(unix)]
fn prodperm_uid_of(name: &str) -> u32 {
    let output = StdCommand::new("id")
        .arg("-u")
        .arg(name)
        .output()
        .expect("failed to run `id -u`");
    assert!(output.status.success(), "id -u {name} failed");
    String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse::<u32>()
        .expect("id -u returned non-numeric")
}

/// Build a tempdir with a production-shape layout: `config.toml`
/// chowned to `root:root` with mode `0600`, mailbox storage owned by
/// `aimx-test-alice`. Returns the tmpdir guard so callers can keep it
/// alive for the test duration.
#[cfg(unix)]
fn prodperm_setup_env(tmp: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let alice_uid = prodperm_uid_of(PRODPERM_USER);

    // Top-level dirs must be traversable by alice so `aimx mcp` (run
    // under her uid) can reach config + storage paths it has rights to
    // (the config file is intentionally unreadable; the directory
    // itself must allow `x`).
    std::fs::set_permissions(tmp, std::fs::Permissions::from_mode(0o755)).unwrap();

    let config_content = format!(
        "domain = \"agent.example.com\"\ndata_dir = \"{}\"\n\n[mailboxes.catchall]\naddress = \"*@agent.example.com\"\nowner = \"aimx-catchall\"\n\n[mailboxes.alice]\naddress = \"alice@agent.example.com\"\nowner = \"{PRODPERM_USER}\"\n",
        tmp.display()
    );
    let config_path = tmp.join("config.toml");
    std::fs::write(&config_path, config_content).unwrap();
    // Mirror the production install: `0600 root:root`.
    std::fs::set_permissions(&config_path, std::fs::Permissions::from_mode(0o600)).unwrap();
    let cstr = std::ffi::CString::new(config_path.as_os_str().as_encoded_bytes()).unwrap();
    let chown_rc = unsafe { libc::chown(cstr.as_ptr(), 0, 0) };
    assert_eq!(chown_rc, 0, "chown root:root config.toml failed");

    // The catchall system user must exist so the config validator
    // doesn't surface a hard-fail on the orphan owner.
    let _ = StdCommand::new("useradd")
        .arg("--system")
        .arg("--no-create-home")
        .arg("--shell")
        .arg("/usr/sbin/nologin")
        .arg("aimx-catchall")
        .status();

    for sub in [
        "inbox/catchall",
        "sent/catchall",
        "inbox/alice",
        "sent/alice",
    ] {
        let dir = tmp.join(sub);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        let cstr = std::ffi::CString::new(dir.as_os_str().as_encoded_bytes()).unwrap();
        // alice owns `inbox/alice/` + `sent/alice/`; the catchall dirs
        // are owned by `aimx-catchall` to mirror production.
        let owner_uid = if sub.contains("alice") {
            alice_uid
        } else {
            prodperm_uid_of("aimx-catchall")
        };
        unsafe {
            libc::chown(cstr.as_ptr(), owner_uid, owner_uid);
        }
    }

    install_cached_dkim_keys(tmp);
    // DKIM private.key is `0600`; the daemon (root) reads it directly,
    // so leaving it owned by the test runner is fine. Storage dirs
    // already covered above.
    let runtime = tmp.join("run");
    std::fs::create_dir_all(&runtime).unwrap();
    std::fs::set_permissions(&runtime, std::fs::Permissions::from_mode(0o755)).unwrap();

    // Copy the `aimx` binary into the tempdir so `aimx-test-alice` can
    // exec it. The binary's canonical location is somewhere under
    // `target/debug/`, which on most CI/dev hosts sits beneath a home
    // directory whose mode is `0o750` — the alice uid (not in the
    // owner's group) cannot traverse it, and `runuser -u alice -- /path`
    // exits with `Permission denied` before the MCP child even starts.
    // The tempdir itself is `0o755` and lives under `/tmp` (always
    // world-traversable), so the copy is reachable from any uid.
    prodperm_copy_aimx_binary(tmp);
}

/// Path inside the prodperm tempdir where we stash a world-traversable
/// copy of the aimx binary. Used by `prodperm_spawn_mcp` to spawn `aimx
/// mcp` as the alice uid without depending on the host's `$HOME` mode.
#[cfg(unix)]
fn prodperm_aimx_binary(tmp: &Path) -> std::path::PathBuf {
    tmp.join("aimx")
}

#[cfg(unix)]
fn prodperm_copy_aimx_binary(tmp: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let src = aimx_binary_path();
    let dst = prodperm_aimx_binary(tmp);
    std::fs::copy(&src, &dst).unwrap_or_else(|e| {
        panic!(
            "failed to copy aimx binary {} -> {}: {e}",
            src.display(),
            dst.display()
        )
    });
    // `0o755` so any uid (including `aimx-test-alice`) can exec it.
    std::fs::set_permissions(&dst, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// Spawn `aimx mcp` under `runuser -u aimx-test-alice` so the resulting
/// process really is non-root and any client-side `config.toml` read
/// will fail with EACCES (the bug class we are guarding against).
///
/// Uses the tempdir-local copy of the `aimx` binary planted by
/// `prodperm_setup_env` (`/tmp/.../aimx`) rather than the original under
/// `target/debug/`, which on most CI/dev hosts sits beneath a `0o750`
/// `$HOME` and can't be exec'd by the alice uid. The runtime dir +
/// config dir env vars survive the `runuser` env reset (verified on
/// Ubuntu's `util-linux` `runuser`: only `HOME`/`USER`/`MAIL`/`LOGNAME`
/// are rewritten), so the MCP child finds the daemon socket via
/// `AIMX_RUNTIME_DIR` exactly the same way the test's daemon binds it.
#[cfg(unix)]
fn prodperm_spawn_mcp(tmp: &Path) -> McpClient {
    let runtime = tmp.join("run");
    let bin = prodperm_aimx_binary(tmp);
    let mut child = StdCommand::new("runuser")
        .arg("-u")
        .arg(PRODPERM_USER)
        .arg("--")
        .arg(&bin)
        .env("AIMX_CONFIG_DIR", tmp)
        .env("AIMX_RUNTIME_DIR", &runtime)
        .arg("--data-dir")
        .arg(tmp)
        .arg("mcp")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn aimx mcp under runuser");

    let stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    let reader = BufReader::new(stdout);
    let (stderr_buf, stderr_drain) = spawn_stderr_drain(stderr);

    McpClient {
        child,
        stdin,
        reader,
        id: 0,
        stderr_buf,
        stderr_drain: Some(stderr_drain),
    }
}

#[cfg(unix)]
fn prodperm_assert_no_eacces(text: &str, tool: &str) {
    assert!(
        !text.contains("Permission denied"),
        "{tool}: must not surface EACCES on production-perm config; got: {text}"
    );
    assert!(
        !text.contains("os error 13"),
        "{tool}: must not surface EACCES on production-perm config; got: {text}"
    );
}

/// Write a simple email file as `aimx-test-alice` so the inbox carries
/// content the MCP tools can read / mark / reply to. Done from root via
/// `runuser` so the on-disk uid matches `inbox/alice/`'s owner.
#[cfg(unix)]
fn prodperm_seed_email(tmp: &Path, id: &str) {
    // Match the production frontmatter writer: optional fields with no
    // value (`in_reply_to`, `references`) are omitted entirely rather
    // than written as empty strings, so deserializers see `None` rather
    // than `Some("")`.
    let body = format!(
        "+++\nid = \"{id}\"\nmessage_id = \"<{id}@test.com>\"\nfrom = \"sender@example.com\"\nto = \"alice@agent.example.com\"\nsubject = \"Hello {id}\"\ndate = \"2025-06-01T12:00:00Z\"\nattachments = []\nmailbox = \"alice\"\nread = false\ndkim = \"none\"\nspf = \"none\"\n+++\n\nbody {id}\n"
    );
    let path = tmp.join("inbox").join("alice").join(format!("{id}.md"));
    std::fs::write(&path, body).unwrap();
    let cstr = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).unwrap();
    let alice_uid = prodperm_uid_of(PRODPERM_USER);
    unsafe {
        libc::chown(cstr.as_ptr(), alice_uid, alice_uid);
    }
}

/// `mailbox_list_full_cycle` — covers `mailbox_list`, `mailbox_create`,
/// and `mailbox_delete` against a `chmod 0600 root:root` config.
#[cfg(unix)]
#[test]
#[ignore = "production-perm smoke; requires root + AIMX_INTEGRATION_SUDO=1"]
fn mailbox_list_full_cycle_against_root_owned_config() {
    if prodperm_skip() {
        return;
    }
    let tmp = TempDir::new().unwrap();
    prodperm_setup_env(tmp.path());
    let port = find_free_port();
    let daemon = start_serve(tmp.path(), port);
    let sock = tmp.path().join("run").join("aimx.sock");
    assert!(
        wait_for_socket(&sock, std::time::Duration::from_secs(5)),
        "UDS socket never appeared"
    );
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&sock, std::fs::Permissions::from_mode(0o666)).unwrap();

    let mut client = prodperm_spawn_mcp(tmp.path());
    client.initialize();

    // mailbox_list
    let resp = client.call_tool("mailbox_list", serde_json::json!({}));
    let text = get_tool_text(&resp);
    prodperm_assert_no_eacces(&text, "mailbox_list");
    assert!(
        text.contains("alice"),
        "mailbox_list should show alice's mailbox: {text}"
    );

    // mailbox_create — alice (uid bound by SO_PEERCRED) creates a
    // new mailbox; the daemon must hot-swap the config.
    let resp = client.call_tool("mailbox_create", serde_json::json!({"name": "alice-extra"}));
    let text = get_tool_text(&resp);
    prodperm_assert_no_eacces(&text, "mailbox_create");
    assert!(
        text.contains("alice-extra") || text.contains("created"),
        "mailbox_create should succeed: {text}"
    );

    // mailbox_delete — same caller deletes what they just created.
    let resp = client.call_tool("mailbox_delete", serde_json::json!({"name": "alice-extra"}));
    let text = get_tool_text(&resp);
    prodperm_assert_no_eacces(&text, "mailbox_delete");
    assert!(
        text.contains("alice-extra") || text.contains("deleted"),
        "mailbox_delete should succeed: {text}"
    );

    client.shutdown();
    stop_serve(daemon);
}

/// `email_list_full_cycle` — covers `email_list`, `email_read`,
/// `email_mark_read`, `email_mark_unread`, `email_send`, `email_reply`
/// against a `chmod 0600 root:root` config.
#[cfg(unix)]
#[test]
#[ignore = "production-perm smoke; requires root + AIMX_INTEGRATION_SUDO=1"]
fn email_list_full_cycle_against_root_owned_config() {
    if prodperm_skip() {
        return;
    }
    let tmp = TempDir::new().unwrap();
    prodperm_setup_env(tmp.path());
    prodperm_seed_email(tmp.path(), "2025-06-01-001");

    let port = find_free_port();
    // The daemon must use the file-drop transport so `email_send` does
    // not block on real MX delivery. `AIMX_TEST_MAIL_DROP` must point
    // at a *file* path — `FileDropTransport::send` opens it with
    // `OpenOptions::create(true).append(true)` and surfaces `Is a
    // directory (os error 21)` if a directory is passed instead.
    let mail_drop = tmp.path().join("outbound.log");
    let daemon = start_serve_with_env(
        tmp.path(),
        port,
        &[("AIMX_TEST_MAIL_DROP", mail_drop.to_str().unwrap())],
    );
    let sock = tmp.path().join("run").join("aimx.sock");
    assert!(
        wait_for_socket(&sock, std::time::Duration::from_secs(5)),
        "UDS socket never appeared"
    );
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&sock, std::fs::Permissions::from_mode(0o666)).unwrap();

    let mut client = prodperm_spawn_mcp(tmp.path());
    client.initialize();

    // email_list
    let resp = client.call_tool("email_list", serde_json::json!({"mailbox": "alice"}));
    let text = get_tool_text(&resp);
    prodperm_assert_no_eacces(&text, "email_list");
    assert!(
        text.contains("2025-06-01-001"),
        "email_list should include the seeded id: {text}"
    );

    // email_read
    let resp = client.call_tool(
        "email_read",
        serde_json::json!({"mailbox": "alice", "id": "2025-06-01-001"}),
    );
    let text = get_tool_text(&resp);
    prodperm_assert_no_eacces(&text, "email_read");
    assert!(
        text.contains("body 2025-06-01-001"),
        "email_read should return body: {text}"
    );

    // email_mark_read
    let resp = client.call_tool(
        "email_mark_read",
        serde_json::json!({"mailbox": "alice", "id": "2025-06-01-001"}),
    );
    let text = get_tool_text(&resp);
    prodperm_assert_no_eacces(&text, "email_mark_read");

    // email_mark_unread
    let resp = client.call_tool(
        "email_mark_unread",
        serde_json::json!({"mailbox": "alice", "id": "2025-06-01-001"}),
    );
    let text = get_tool_text(&resp);
    prodperm_assert_no_eacces(&text, "email_mark_unread");

    // email_send — recipient is intentionally unrouteable; the
    // file-drop transport short-circuits delivery so we are only
    // testing that the MCP tool reaches the daemon and the daemon's
    // sent-copy write succeeds without surfacing config EACCES.
    let resp = client.call_tool(
        "email_send",
        serde_json::json!({
            "from_mailbox": "alice",
            "to": "rcpt@invalid.example.invalid",
            "subject": "perm test",
            "body": "x"
        }),
    );
    let send_text = get_tool_text(&resp);
    prodperm_assert_no_eacces(&send_text, "email_send");

    // The daemon must have written a sent-copy `.md` under
    // `sent/alice/`. Without this assertion a future regression where
    // the sent-copy persistence silently no-ops would only be caught
    // by the EACCES guard, which would not fire.
    let sent_dir = tmp.path().join("sent").join("alice");
    let sent_md_count = std::fs::read_dir(&sent_dir)
        .expect("sent/alice/ must exist")
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.path()
                .extension()
                .and_then(|s| s.to_str())
                .map(|s| s == "md")
                .unwrap_or(false)
        })
        .count();
    assert!(
        sent_md_count >= 1,
        "email_send should have written a sent-copy .md under {} (daemon response: {send_text}); found {sent_md_count}",
        sent_dir.display()
    );

    // email_reply — replies to the seeded message.
    let resp = client.call_tool(
        "email_reply",
        serde_json::json!({
            "mailbox": "alice",
            "id": "2025-06-01-001",
            "body": "reply body"
        }),
    );
    let text = get_tool_text(&resp);
    prodperm_assert_no_eacces(&text, "email_reply");

    client.shutdown();
    stop_serve(daemon);
}

/// `hook_list_full_cycle` — covers `hook_list`, `hook_create`, and
/// `hook_delete` against a `chmod 0600 root:root` config.
#[cfg(unix)]
#[test]
#[ignore = "production-perm smoke; requires root + AIMX_INTEGRATION_SUDO=1"]
fn hook_list_full_cycle_against_root_owned_config() {
    if prodperm_skip() {
        return;
    }
    let tmp = TempDir::new().unwrap();
    prodperm_setup_env(tmp.path());
    let port = find_free_port();
    let daemon = start_serve(tmp.path(), port);
    let sock = tmp.path().join("run").join("aimx.sock");
    assert!(
        wait_for_socket(&sock, std::time::Duration::from_secs(5)),
        "UDS socket never appeared"
    );
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&sock, std::fs::Permissions::from_mode(0o666)).unwrap();

    let mut client = prodperm_spawn_mcp(tmp.path());
    client.initialize();

    // hook_list (initially empty for alice)
    let resp = client.call_tool("hook_list", serde_json::json!({}));
    let text = get_tool_text(&resp);
    prodperm_assert_no_eacces(&text, "hook_list");
    assert_eq!(text, "[]", "hook_list initially empty: {text}");

    // hook_create
    let resp = client.call_tool(
        "hook_create",
        serde_json::json!({
            "mailbox": "alice",
            "event": "on_receive",
            "cmd": ["/bin/true"],
            "name": "prod_perm_hook"
        }),
    );
    let text = get_tool_text(&resp);
    prodperm_assert_no_eacces(&text, "hook_create");

    // Re-list to confirm it's there.
    let resp = client.call_tool("hook_list", serde_json::json!({}));
    let text = get_tool_text(&resp);
    prodperm_assert_no_eacces(&text, "hook_list");
    assert!(
        text.contains("prod_perm_hook"),
        "hook_list should show the new hook: {text}"
    );

    // hook_delete
    let resp = client.call_tool("hook_delete", serde_json::json!({"name": "prod_perm_hook"}));
    let text = get_tool_text(&resp);
    prodperm_assert_no_eacces(&text, "hook_delete");

    client.shutdown();
    stop_serve(daemon);
}
