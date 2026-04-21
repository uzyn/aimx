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
        "domain = \"cache.example.com\"\ndata_dir = \"{}\"\n\n[mailboxes.catchall]\naddress = \"*@cache.example.com\"\n",
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
    let config_content = format!(
        "domain = \"agent.example.com\"\ndata_dir = \"{}\"\n\n[mailboxes.catchall]\naddress = \"*@agent.example.com\"\n\n[mailboxes.alice]\naddress = \"alice@agent.example.com\"\n",
        tmp.display()
    );
    std::fs::create_dir_all(tmp.join("inbox").join("catchall")).unwrap();
    std::fs::create_dir_all(tmp.join("inbox").join("alice")).unwrap();
    std::fs::create_dir_all(tmp.join("sent").join("alice")).unwrap();
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
            // Zola-style bundle: `<stem>/<stem>.md`.
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
        .stdout(predicate::str::contains("ingest"))
        .stdout(predicate::str::contains("send"))
        .stdout(predicate::str::contains("mailboxes"))
        .stdout(predicate::str::contains("mcp"))
        .stdout(predicate::str::contains("setup"))
        .stdout(predicate::str::contains("doctor"))
        .stdout(predicate::str::contains("serve"))
        .stdout(predicate::str::contains("portcheck"))
        .stdout(predicate::str::contains("dkim-keygen"));
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
        "domain = \"agent.example.com\"\ndata_dir = \"{}\"\n\n[mailboxes.catchall]\naddress = \"*@agent.example.com\"\n",
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
        "domain = \"agent.example.com\"\ndata_dir = \"{}\"\n\n[mailboxes.catchall]\naddress = \"*@agent.example.com\"\n",
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
        "domain = \"agent.example.com\"\ndata_dir = \"{}\"\n\n[mailboxes.catchall]\naddress = \"*@agent.example.com\"\n",
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
        "domain = \"agent.example.com\"\ndata_dir = \"{}\"\n\n[mailboxes.catchall]\naddress = \"*@agent.example.com\"\n",
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

struct McpClient {
    child: std::process::Child,
    stdin: std::process::ChildStdin,
    reader: BufReader<std::process::ChildStdout>,
    id: i64,
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
        let reader = BufReader::new(stdout);

        Self {
            child,
            stdin,
            reader,
            id: 0,
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
        self.reader.read_line(&mut line).unwrap();
        serde_json::from_str(line.trim()).unwrap()
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
    // 9 mail/mailbox tools + 4 hook tools (Sprint 5).
    assert_eq!(tools.len(), 13);

    let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert!(names.contains(&"mailbox_create"));
    assert!(names.contains(&"mailbox_list"));
    assert!(names.contains(&"mailbox_delete"));
    assert!(names.contains(&"email_list"));
    assert!(names.contains(&"email_read"));
    assert!(names.contains(&"email_mark_read"));
    assert!(names.contains(&"email_mark_unread"));
    assert!(names.contains(&"email_send"));
    assert!(names.contains(&"email_reply"));
    assert!(names.contains(&"hook_list_templates"));
    assert!(names.contains(&"hook_create"));
    assert!(names.contains(&"hook_list"));
    assert!(names.contains(&"hook_delete"));

    client.shutdown();
}

#[test]
fn mcp_mailbox_create_list_delete() {
    let tmp = TempDir::new().unwrap();
    setup_test_env(tmp.path());

    let mut client = McpClient::spawn(tmp.path());
    client.initialize();

    let resp = client.call_tool("mailbox_create", serde_json::json!({"name": "support"}));
    let text = get_tool_text(&resp);
    assert!(
        text.contains("created"),
        "Expected creation message, got: {text}"
    );
    assert!(inbox(tmp.path(), "support").is_dir());

    let resp = client.call_tool("mailbox_list", serde_json::json!({}));
    let text = get_tool_text(&resp);
    assert!(text.contains("catchall"));
    assert!(text.contains("support"));
    assert!(text.contains("alice"));

    let resp = client.call_tool("mailbox_delete", serde_json::json!({"name": "support"}));
    let text = get_tool_text(&resp);
    assert!(text.contains("deleted"));
    assert!(!inbox(tmp.path(), "support").exists());

    client.shutdown();
}

#[test]
fn mcp_mailbox_delete_catchall_error() {
    let tmp = TempDir::new().unwrap();
    setup_test_env(tmp.path());

    let mut client = McpClient::spawn(tmp.path());
    client.initialize();

    let resp = client.call_tool("mailbox_delete", serde_json::json!({"name": "catchall"}));
    assert!(is_tool_error(&resp));
    let text = get_tool_text(&resp);
    assert!(text.contains("Cannot delete"), "Got: {text}");

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

    let mut client = McpClient::spawn(tmp.path());
    client.initialize();

    let resp = client.call_tool("email_list", serde_json::json!({"mailbox": "alice"}));
    let text = get_tool_text(&resp);
    assert!(text.contains("2025-06-01-001"));
    assert!(text.contains("2025-06-01-002"));
    assert!(text.contains("sender@example.com"));

    let resp = client.call_tool(
        "email_list",
        serde_json::json!({"mailbox": "alice", "unread": true}),
    );
    let text = get_tool_text(&resp);
    assert!(text.contains("2025-06-01-001"));
    assert!(!text.contains("2025-06-01-002"));

    let resp = client.call_tool(
        "email_read",
        serde_json::json!({"mailbox": "alice", "id": "2025-06-01-001"}),
    );
    let text = get_tool_text(&resp);
    assert!(text.contains("Body of 2025-06-01-001"));

    client.shutdown();
}

#[test]
fn mcp_email_read_nonexistent_error() {
    let tmp = TempDir::new().unwrap();
    setup_test_env(tmp.path());

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
}

#[test]
fn mcp_email_list_nonexistent_mailbox_error() {
    let tmp = TempDir::new().unwrap();
    setup_test_env(tmp.path());

    let mut client = McpClient::spawn(tmp.path());
    client.initialize();

    let resp = client.call_tool("email_list", serde_json::json!({"mailbox": "nonexistent"}));
    assert!(is_tool_error(&resp));
    let text = get_tool_text(&resp);
    assert!(text.contains("does not exist"), "Got: {text}");

    client.shutdown();
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
    let mut client = McpClient {
        child,
        stdin,
        reader: BufReader::new(stdout),
        id: 0,
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
    let mut client = McpClient {
        child,
        stdin,
        reader: BufReader::new(stdout),
        id: 0,
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
}

#[test]
fn mcp_email_reply_nonexistent_email_error() {
    let tmp = TempDir::new().unwrap();
    setup_test_env(tmp.path());

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

fn setup_test_env_with_triggers(tmp: &Path, trigger_marker: &Path) -> String {
    let config_content = format!(
        r#"domain = "agent.example.com"
data_dir = "{}"

[mailboxes.catchall]
address = "*@agent.example.com"

[[mailboxes.catchall.hooks]]
name = "catchalltrig"
event = "on_receive"
cmd = "touch {}"
dangerously_support_untrusted = true
"#,
        tmp.display(),
        trigger_marker.display()
    );
    std::fs::create_dir_all(tmp.join("inbox").join("catchall")).unwrap();
    let config_path = tmp.join("config.toml");
    std::fs::write(&config_path, &config_content).unwrap();
    config_path.to_string_lossy().to_string()
}

#[test]
fn ingest_trigger_executes_on_delivery() {
    let tmp = TempDir::new().unwrap();
    let marker = tmp.path().join("trigger.marker");
    setup_test_env_with_triggers(tmp.path(), &marker);
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
    assert!(
        marker.exists(),
        "Trigger marker file should have been created"
    );
}

#[test]
fn ingest_failing_trigger_does_not_block_delivery() {
    let tmp = TempDir::new().unwrap();
    let config_content = format!(
        r#"domain = "agent.example.com"
data_dir = "{}"

[mailboxes.catchall]
address = "*@agent.example.com"

[[mailboxes.catchall.hooks]]
name = "failtrigger1"
event = "on_receive"
cmd = "false"
dangerously_support_untrusted = true
"#,
        tmp.path().display()
    );
    std::fs::create_dir_all(inbox(tmp.path(), "catchall")).unwrap();
    std::fs::write(tmp.path().join("config.toml"), &config_content).unwrap();

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
    assert_eq!(
        md_files.len(),
        1,
        "Email should be saved despite trigger failure"
    );
}

#[test]
fn ingest_trust_verified_blocks_unsigned_trigger() {
    let tmp = TempDir::new().unwrap();
    let marker = tmp.path().join("should_not_exist");
    let config_content = format!(
        r#"domain = "agent.example.com"
data_dir = "{}"

[mailboxes.catchall]
address = "*@agent.example.com"
trust = "verified"

[[mailboxes.catchall.hooks]]
name = "verifiedhook"
event = "on_receive"
cmd = "touch {}"
"#,
        tmp.path().display(),
        marker.display()
    );
    std::fs::create_dir_all(inbox(tmp.path(), "catchall")).unwrap();
    std::fs::write(tmp.path().join("config.toml"), &config_content).unwrap();

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
    assert_eq!(md_files.len(), 1, "Email should still be saved");
    assert!(
        !marker.exists(),
        "Trigger should NOT fire for unsigned email with trust=verified"
    );
}

#[test]
fn ingest_trust_none_allows_unsigned_trigger() {
    // S50-3: mailbox trust=none no longer fires hooks by default. The hook
    // must explicitly opt in via `dangerously_support_untrusted` to keep
    // the pre-Sprint-50 "fire on unsigned" behavior.
    let tmp = TempDir::new().unwrap();
    let marker = tmp.path().join("triggered");
    let config_content = format!(
        r#"domain = "agent.example.com"
data_dir = "{}"

[mailboxes.catchall]
address = "*@agent.example.com"
trust = "none"

[[mailboxes.catchall.hooks]]
name = "trustnonefir"
event = "on_receive"
cmd = "touch {}"
dangerously_support_untrusted = true
"#,
        tmp.path().display(),
        marker.display()
    );
    std::fs::create_dir_all(inbox(tmp.path(), "catchall")).unwrap();
    std::fs::write(tmp.path().join("config.toml"), &config_content).unwrap();

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
    assert!(
        marker.exists(),
        "Trigger should fire with trust=none even for unsigned email"
    );
}

/// Verifies the end-to-end global-trust inherit path: the top-level
/// `trust` + `trusted_senders` on `Config` apply to a mailbox that has
/// neither field set. On ingest the frontmatter's `trusted` value and the
/// hook gate (Sprint 50) must both reflect the inherited policy.
#[test]
fn ingest_inherits_global_trust_when_mailbox_has_no_override() {
    // S50-3: Sprint 50 inverts the hook gate. It now fires iff the
    // evaluated `trusted == "true"` OR the hook opts in explicitly.
    // Unsigned fixture means `trusted == "false"` even with allowlist, so
    // the hook must opt in or it won't fire. We keep the original intent
    // of this test (inheriting global trust into the mailbox row) by
    // asserting the frontmatter value and NOT asserting the marker exists.
    let tmp = TempDir::new().unwrap();
    let marker = tmp.path().join("triggered");
    let config_content = format!(
        r#"domain = "agent.example.com"
data_dir = "{}"
trust = "verified"
trusted_senders = ["*@example.com"]

[mailboxes.catchall]
address = "*@agent.example.com"

[[mailboxes.catchall.hooks]]
name = "inheritglobs"
event = "on_receive"
cmd = "touch {}"
"#,
        tmp.path().display(),
        marker.display()
    );
    std::fs::create_dir_all(inbox(tmp.path(), "catchall")).unwrap();
    std::fs::write(tmp.path().join("config.toml"), &config_content).unwrap();

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

    // S50-3: the hook gate now reads `trusted` from frontmatter. The
    // unsigned fixture produces `trusted = "false"` even with the global
    // allowlist covering the sender, so a default hook does NOT fire.
    assert!(
        !marker.exists(),
        "Default hook must not fire for trusted=false under Sprint 50 semantics"
    );

    // The strict `trusted` field evaluation requires BOTH allowlist AND
    // DKIM pass. Unsigned fixture → DKIM is not "pass" → trusted = "false".
    // This proves the inherit path wired through `trust::evaluate_trust`.
    let parsed = read_frontmatter(&md_files[0]);
    let table = parsed.as_table().unwrap();
    assert_eq!(
        get_toml_str(table, "trusted"),
        "false",
        "global verified + allowlisted sender + DKIM != pass → trusted=false"
    );
}

/// S50-3: per-mailbox `trust = "none"` override yields
/// `trusted = "none"` on the email, which Sprint 50 no longer treats as
/// "fire hooks by default." To preserve the original intent (mailbox
/// override beats global), the hook opts in explicitly.
#[test]
fn ingest_mailbox_trust_none_override_beats_global_verified() {
    let tmp = TempDir::new().unwrap();
    let marker = tmp.path().join("triggered");
    let config_content = format!(
        r#"domain = "agent.example.com"
data_dir = "{}"
trust = "verified"

[mailboxes.catchall]
address = "*@agent.example.com"
trust = "none"

[[mailboxes.catchall.hooks]]
name = "mbtrustover1"
event = "on_receive"
cmd = "touch {}"
dangerously_support_untrusted = true
"#,
        tmp.path().display(),
        marker.display()
    );
    std::fs::create_dir_all(inbox(tmp.path(), "catchall")).unwrap();
    std::fs::write(tmp.path().join("config.toml"), &config_content).unwrap();

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
    assert!(
        marker.exists(),
        "Trigger should fire because mailbox trust=none overrides global verified"
    );

    let parsed = read_frontmatter(&md_files[0]);
    let table = parsed.as_table().unwrap();
    assert_eq!(
        get_toml_str(table, "trusted"),
        "none",
        "mailbox trust=none override → no evaluation → trusted=none"
    );
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
fn ingest_trusted_sender_bypasses_dkim() {
    // S50-3: trusted_senders alone no longer yields `trusted = "true"`;
    // Sprint 50 requires allowlist AND DKIM pass for `trusted = "true"`.
    // To mirror the "bypass DKIM for trusted senders" affordance, the hook
    // opts in explicitly.
    let tmp = TempDir::new().unwrap();
    let marker = tmp.path().join("triggered");
    let config_content = format!(
        r#"domain = "agent.example.com"
data_dir = "{}"

[mailboxes.catchall]
address = "*@agent.example.com"
trust = "verified"
trusted_senders = ["*@example.com"]

[[mailboxes.catchall.hooks]]
name = "trustedsend1"
event = "on_receive"
cmd = "touch {}"
dangerously_support_untrusted = true
"#,
        tmp.path().display(),
        marker.display()
    );
    std::fs::create_dir_all(inbox(tmp.path(), "catchall")).unwrap();
    std::fs::write(tmp.path().join("config.toml"), &config_content).unwrap();

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
    assert!(
        marker.exists(),
        "Trigger should fire for trusted sender even with trust=verified"
    );
}

/// S31-2 / S44-1 / S50-*: end-to-end hook-recipe test.
///
/// Drives the full ingest -> hook-match -> templated shell command path with
/// an assert-able one-liner that writes `$AIMX_FILEPATH` and `$AIMX_SUBJECT`
/// into a marker file. A second hook exits non-zero to prove that hook
/// failure does NOT block delivery. Both hooks opt in via
/// `dangerously_support_untrusted` so they fire on the unsigned fixture.
#[test]
fn hook_recipe_end_to_end_with_templated_args() {
    let tmp = TempDir::new().unwrap();
    let marker = tmp.path().join("recipe.marker");
    let config_content = format!(
        r#"domain = "agent.example.com"
data_dir = "{data_dir}"

[mailboxes.catchall]
address = "*@agent.example.com"

[[mailboxes.catchall.hooks]]
name = "recipehook01"
event = "on_receive"
cmd = 'printf "filepath=%s\nsubject=%s\n" "$AIMX_FILEPATH" "$AIMX_SUBJECT" > {marker}'
dangerously_support_untrusted = true

[[mailboxes.catchall.hooks]]
name = "recipehook02"
event = "on_receive"
cmd = "false"
dangerously_support_untrusted = true
"#,
        data_dir = tmp.path().display(),
        marker = marker.display()
    );
    std::fs::create_dir_all(inbox(tmp.path(), "catchall")).unwrap();
    std::fs::write(tmp.path().join("config.toml"), &config_content).unwrap();

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
    assert_eq!(
        md_files.len(),
        1,
        "Email should be ingested even when a sibling trigger fails"
    );

    assert!(
        marker.exists(),
        "Channel-recipe trigger should have written the marker file"
    );

    let contents = std::fs::read_to_string(&marker).unwrap();
    let md_path = md_files[0].to_string_lossy().to_string();
    assert!(
        contents.contains(&format!("filepath={md_path}")),
        "Marker should contain the $AIMX_FILEPATH value; got: {contents}"
    );
    assert!(
        contents.contains("subject=Plain text test"),
        "Marker should contain the $AIMX_SUBJECT value; got: {contents}"
    );
}

#[test]
fn setup_help_shows_domain_arg() {
    Command::cargo_bin("aimx")
        .unwrap()
        .args(["setup", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("DOMAIN"));
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

    aimx_cmd(tmp.path())
        .arg("--data-dir")
        .arg(tmp.path())
        .arg("doctor")
        .assert()
        .success()
        .stdout(predicate::str::contains("agent.example.com"))
        .stdout(predicate::str::contains("catchall"))
        .stdout(predicate::str::contains("alice"))
        .stdout(predicate::str::contains("Mailbox"));
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
    // S48-7: `mailboxes` is the canonical subcommand name; the singular
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
    // S48-1 clean rename: `aimx status` must produce a clap "unrecognized
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
    let config_content = format!(
        "domain = \"agent.example.com\"\ndata_dir = \"{}\"\n\n[mailboxes.catchall]\naddress = \"*@agent.example.com\"\n\n[mailboxes.alice]\naddress = \"alice@agent.example.com\"\n\n[mailboxes.bob]\naddress = \"bob@agent.example.com\"\n",
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
    // which for BCC is the BCC address. This is correct per FR-13.
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

/// S50-4: end-to-end `after_send` hook test. Replaces the default
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
    let config = format!(
        r#"domain = "agent.example.com"
data_dir = "{data_dir}"

[mailboxes.catchall]
address = "*@agent.example.com"

[mailboxes.alice]
address = "alice@agent.example.com"

[[mailboxes.alice.hooks]]
name = "aftersendhk1"
event = "after_send"
cmd = 'printf "status=%s to=%s\n" "$AIMX_SEND_STATUS" "$AIMX_TO" > {sentinel}'
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
        after.starts_with("<!-- aimx-readme-version: 6 -->"),
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
            let mut client = McpClient {
                child,
                stdin,
                reader: BufReader::new(stdout),
                id: 0,
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
fn mailbox_create_via_uds_hotswaps_config_and_routes_new_mail() {
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

#[cfg(unix)]
#[test]
fn mailbox_create_without_daemon_falls_back_and_prints_restart_hint() {
    let tmp = TempDir::new().unwrap();
    setup_test_env(tmp.path());

    // Point at an empty runtime dir; no socket present, so UDS fails with
    // NotFound and the CLI falls back to direct on-disk edit.
    let runtime = tmp.path().join("run");
    std::fs::create_dir_all(&runtime).ok();

    let assert = aimx_cmd(tmp.path())
        .env("AIMX_RUNTIME_DIR", &runtime)
        .arg("--data-dir")
        .arg(tmp.path())
        .arg("mailbox")
        .arg("create")
        .arg("eve")
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout).to_string();
    assert!(
        stdout.contains("Mailbox 'eve' created"),
        "expected success message, got: {stdout}"
    );
    assert!(
        stdout.contains("Restart the daemon"),
        "fallback path must print the restart hint: {stdout}"
    );

    // Fallback wrote the stanza too.
    let config_text = std::fs::read_to_string(tmp.path().join("config.toml")).unwrap();
    assert!(config_text.contains("[mailboxes.eve]"));
    assert!(tmp.path().join("inbox").join("eve").is_dir());
}

#[cfg(unix)]
#[test]
fn mailbox_delete_via_uds_refuses_nonempty_and_succeeds_after_cleanup() {
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
// S48-5: `aimx mailboxes delete --force` (CLI-only wipe + delete)
// ---------------------------------------------------------------------------

#[cfg(unix)]
#[test]
fn mailbox_delete_force_yes_wipes_contents_and_succeeds() {
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
fn mailbox_delete_force_without_yes_prompts_and_aborts_on_n() {
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

#[cfg(unix)]
#[test]
fn mailbox_delete_force_refuses_catchall() {
    let tmp = TempDir::new().unwrap();
    setup_test_env(tmp.path());

    let assert = aimx_cmd(tmp.path())
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
            let mut client = McpClient {
                child,
                stdin,
                reader: BufReader::new(stdout),
                id: 0,
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
fn concurrent_mailbox_create_and_ingest_does_not_deadlock() {
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
// Sprint 51: `aimx mailboxes show` + `aimx hooks` CLI
// ---------------------------------------------------------------------

/// S51-1: `aimx mailboxes show <name>` surfaces trust, senders, hooks,
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

[mailboxes.support]
address = "support@agent.example.com"
trust = "verified"
trusted_senders = ["*@company.com", "boss@example.com"]

[[mailboxes.support.hooks]]
name = "inbound_urgent"
event = "on_receive"
cmd = "echo inbound"

[[mailboxes.support.hooks]]
name = "outbound_notify"
event = "after_send"
cmd = "echo outbound"
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
    // Sprint 3 S3-4 makes `aimx hooks create --cmd` root-only. CI runs
    // non-root, so tests set this test-only escape hatch to exercise
    // the direct-write + SIGHUP path on behalf of the fake-root
    // operator. Production systemd units never pass this env var.
    cmd.env("AIMX_TEST_SKIP_ROOT_CHECK", "1");
    cmd
}

/// S51-2: `aimx hooks create` + `aimx hooks list` roundtrip.
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
            "echo hi",
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
        create_out.contains("restart") || create_out.contains("Hint"),
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
            "echo anon",
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
            "echo hi",
            "--name",
            "bad name!",
        ])
        .assert()
        .failure();
    let stderr = String::from_utf8_lossy(&assert.get_output().stderr).to_string();
    assert!(stderr.contains("--name"), "expected --name error: {stderr}");
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
            "echo hi",
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
            "echo hi",
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
// S51-3: UDS HOOK-CREATE / HOOK-DELETE end-to-end
// ---------------------------------------------------------------------

/// S3-4: spin up `aimx serve`, issue `aimx hooks create --cmd`, confirm
/// the CLI wrote `config.toml` directly (raw-cmd bypasses UDS entirely
/// under Sprint 3) and SIGHUP'd the running daemon. The success path
/// prints a `Reload:` banner and no `Hint:` restart banner.
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
        .env("AIMX_TEST_SKIP_ROOT_CHECK", "1")
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
            "echo via-daemon",
        ])
        .assert()
        .success();
    let create_out = String::from_utf8_lossy(&create.get_output().stdout).to_string();
    assert!(
        create_out.contains("Hook created"),
        "create output: {create_out}"
    );
    // Sprint 3 S3-4: raw-cmd hooks write config.toml directly and
    // SIGHUP the daemon. Positive signal: stdout must carry the
    // `Reload:` banner (which only prints on SighupOutcome::Sent).
    // Negative signal: no `Hint:` restart banner (that would indicate
    // the SIGHUP path fell through to DaemonNotRunning).
    assert!(
        create_out.contains("Reload:"),
        "daemon-success should print Reload: banner: {create_out}"
    );
    assert!(
        !create_out.contains("Hint:"),
        "daemon-success should not print restart hint: {create_out}"
    );

    // On-disk config.toml should contain the new hook (CLI wrote it
    // directly — raw-cmd never traverses UDS under Sprint 3).
    let content = std::fs::read_to_string(tmp.path().join("config.toml")).unwrap();
    assert!(
        content.contains("echo via-daemon"),
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
        .env("AIMX_TEST_SKIP_ROOT_CHECK", "1")
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
            "echo daemon-anon",
        ])
        .assert()
        .success();
    let out = String::from_utf8_lossy(&create.get_output().stdout).to_string();
    assert!(out.contains("Hook created"), "{out}");
    // Sprint 3 S3-4: when the daemon is up, SIGHUP succeeds and the
    // CLI prints "Reload:" rather than the socket-missing "Hint:"
    // restart banner.
    assert!(
        !out.contains("Hint:"),
        "daemon-success should not print restart hint: {out}"
    );

    // Compute the expected derived name (mirrors `derive_hook_name` in
    // src/hook.rs) and assert it was printed by the CLI.
    let expected = {
        use sha2::{Digest, Sha256};
        let mut hasher = Sha256::new();
        hasher.update(b"on_receive");
        hasher.update([0x1F]);
        hasher.update(b"echo daemon-anon");
        hasher.update([0x1F]);
        hasher.update([0u8]); // dangerously_support_untrusted = false
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
// Sprint 5: MCP hook tools (hook_list_templates, hook_create, hook_list,
// hook_delete)
// ---------------------------------------------------------------------

/// Like `setup_test_env`, but also registers a single `invoke-claude`
/// template so the Sprint 5 MCP tool tests have something to bind to.
fn setup_test_env_with_template(tmp: &Path) -> String {
    let config_content = format!(
        "domain = \"agent.example.com\"\ndata_dir = \"{}\"\n\n\
         [[hook_template]]\n\
         name = \"invoke-claude\"\n\
         description = \"Test invoke-claude template\"\n\
         cmd = [\"/usr/bin/true\", \"{{prompt}}\"]\n\
         params = [\"prompt\"]\n\
         stdin = \"email\"\n\
         run_as = \"aimx-hook\"\n\
         timeout_secs = 60\n\
         allowed_events = [\"on_receive\", \"after_send\"]\n\n\
         [mailboxes.catchall]\n\
         address = \"*@agent.example.com\"\n\n\
         [mailboxes.alice]\n\
         address = \"alice@agent.example.com\"\n",
        tmp.display()
    );
    std::fs::create_dir_all(tmp.join("inbox").join("catchall")).unwrap();
    std::fs::create_dir_all(tmp.join("inbox").join("alice")).unwrap();
    std::fs::create_dir_all(tmp.join("sent").join("alice")).unwrap();
    let config_path = tmp.join("config.toml");
    std::fs::write(&config_path, &config_content).unwrap();
    install_cached_dkim_keys(tmp);
    config_path.to_string_lossy().to_string()
}

/// S5-1: `hook_list_templates` with zero templates returns `[]`.
#[test]
fn mcp_hook_list_templates_empty_returns_empty_array() {
    let tmp = TempDir::new().unwrap();
    setup_test_env(tmp.path());

    let mut client = McpClient::spawn(tmp.path());
    client.initialize();

    let resp = client.call_tool("hook_list_templates", serde_json::json!({}));
    let text = get_tool_text(&resp);
    assert_eq!(text, "[]", "empty config must return []: {text}");

    client.shutdown();
}

/// S5-1: with one template registered, `hook_list_templates` returns
/// the PRD-specified shape.
#[test]
fn mcp_hook_list_templates_returns_registered_template() {
    let tmp = TempDir::new().unwrap();
    setup_test_env_with_template(tmp.path());

    let mut client = McpClient::spawn(tmp.path());
    client.initialize();

    let resp = client.call_tool("hook_list_templates", serde_json::json!({}));
    let text = get_tool_text(&resp);
    let json: serde_json::Value = serde_json::from_str(&text).unwrap();
    let arr = json.as_array().unwrap();
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["name"], "invoke-claude");
    assert_eq!(arr[0]["description"], "Test invoke-claude template");
    assert_eq!(arr[0]["params"][0], "prompt");
    let events: Vec<&str> = arr[0]["allowed_events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert!(events.contains(&"on_receive"));
    assert!(events.contains(&"after_send"));
    // Internals must not leak.
    assert!(arr[0].get("cmd").is_none());
    assert!(arr[0].get("run_as").is_none());
    assert!(arr[0].get("timeout_secs").is_none());

    client.shutdown();
}

/// S5-2: `hook_create` without a running daemon returns a precise
/// socket-missing error rather than panicking.
#[test]
fn mcp_hook_create_without_daemon_reports_missing_socket() {
    let tmp = TempDir::new().unwrap();
    setup_test_env_with_template(tmp.path());

    let mut client = McpClient::spawn(tmp.path());
    client.initialize();

    let resp = client.call_tool(
        "hook_create",
        serde_json::json!({
            "mailbox": "alice",
            "event": "on_receive",
            "template": "invoke-claude",
            "params": {"prompt": "hi"}
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

/// S5-2: full round-trip against a live daemon. `hook_create` submits
/// via UDS, the daemon stamps `origin = "mcp"` in `config.toml`, and
/// the tool response includes the effective name + substituted argv.
#[test]
fn mcp_hook_create_end_to_end_against_daemon() {
    let tmp = TempDir::new().unwrap();
    setup_test_env_with_template(tmp.path());
    let port = find_free_port();
    let daemon = start_serve(tmp.path(), port);

    let mut client = McpClient::spawn(tmp.path());
    client.initialize();

    let resp = client.call_tool(
        "hook_create",
        serde_json::json!({
            "mailbox": "alice",
            "event": "on_receive",
            "template": "invoke-claude",
            "params": {"prompt": "You are an assistant"},
            "name": "mcp_test_hook"
        }),
    );
    assert!(!is_tool_error(&resp), "expected success, got {resp}");
    let text = get_tool_text(&resp);
    let json: serde_json::Value = serde_json::from_str(&text).expect(&text);
    assert_eq!(json["effective_name"], "mcp_test_hook");
    assert_eq!(json["substituted_argv"][0], "/usr/bin/true");
    assert_eq!(json["substituted_argv"][1], "You are an assistant");

    // config.toml must now carry the hook with origin = "mcp".
    let content = std::fs::read_to_string(tmp.path().join("config.toml")).unwrap();
    assert!(content.contains("name = \"mcp_test_hook\""), "{content}");
    assert!(content.contains("origin = \"mcp\""), "{content}");
    assert!(
        content.contains("template = \"invoke-claude\""),
        "{content}"
    );

    client.shutdown();
    stop_serve(daemon);
}

/// S5-2: the daemon's `unknown-template` error surfaces verbatim.
#[test]
fn mcp_hook_create_unknown_template_returns_error() {
    let tmp = TempDir::new().unwrap();
    setup_test_env_with_template(tmp.path());

    let mut client = McpClient::spawn(tmp.path());
    client.initialize();

    let resp = client.call_tool(
        "hook_create",
        serde_json::json!({
            "mailbox": "alice",
            "event": "on_receive",
            "template": "does-not-exist",
            "params": {}
        }),
    );
    assert!(is_tool_error(&resp));
    let text = get_tool_text(&resp);
    assert!(text.contains("Unknown template"), "{text}");
    assert!(text.contains("hook_list_templates"), "{text}");

    client.shutdown();
}

/// S5-2: missing required params fail at the pre-flight substitution
/// check on the MCP side (daemon-side re-validates).
#[test]
fn mcp_hook_create_missing_param_returns_error() {
    let tmp = TempDir::new().unwrap();
    setup_test_env_with_template(tmp.path());
    let port = find_free_port();
    let daemon = start_serve(tmp.path(), port);

    let mut client = McpClient::spawn(tmp.path());
    client.initialize();

    let resp = client.call_tool(
        "hook_create",
        serde_json::json!({
            "mailbox": "alice",
            "event": "on_receive",
            "template": "invoke-claude",
            "params": {}
        }),
    );
    assert!(is_tool_error(&resp), "expected error: {resp}");
    let text = get_tool_text(&resp);
    // The daemon returns `missing-param: ...`; MCP surfaces verbatim.
    assert!(text.contains("missing-param"), "{text}");

    client.shutdown();
    stop_serve(daemon);
}

/// S5-3: `hook_list` emits an empty array when no hooks are configured.
#[test]
fn mcp_hook_list_empty_returns_empty_array() {
    let tmp = TempDir::new().unwrap();
    setup_test_env_with_template(tmp.path());

    let mut client = McpClient::spawn(tmp.path());
    client.initialize();

    let resp = client.call_tool("hook_list", serde_json::json!({}));
    let text = get_tool_text(&resp);
    assert_eq!(text, "[]", "{text}");

    client.shutdown();
}

/// S5-3: origin-masking is enforced end-to-end. Operator-origin hooks
/// written to `config.toml` expose only name/mailbox/event/origin;
/// MCP-origin hooks (created via the daemon in this test) expose the
/// template + params too.
#[test]
fn mcp_hook_list_masks_operator_origin() {
    let tmp = TempDir::new().unwrap();
    setup_test_env_with_template(tmp.path());

    // Append an operator-origin hook to the config before spinning up.
    let config_path = tmp.path().join("config.toml");
    let existing = std::fs::read_to_string(&config_path).unwrap();
    let extra = "\n[[mailboxes.alice.hooks]]\nname = \"op_hook\"\n\
                 event = \"on_receive\"\ncmd = \"echo operator-secret\"\n\
                 origin = \"operator\"\n";
    std::fs::write(&config_path, format!("{existing}{extra}")).unwrap();

    let port = find_free_port();
    let daemon = start_serve(tmp.path(), port);

    let mut client = McpClient::spawn(tmp.path());
    client.initialize();

    // Create an MCP-origin hook alongside the operator one.
    let resp = client.call_tool(
        "hook_create",
        serde_json::json!({
            "mailbox": "alice",
            "event": "on_receive",
            "template": "invoke-claude",
            "params": {"prompt": "agent secret"},
            "name": "mcp_hook"
        }),
    );
    assert!(!is_tool_error(&resp), "{resp}");

    // Now query hook_list.
    let resp = client.call_tool("hook_list", serde_json::json!({}));
    let text = get_tool_text(&resp);
    let json: serde_json::Value = serde_json::from_str(&text).expect(&text);
    let rows = json.as_array().unwrap();
    assert_eq!(rows.len(), 2);

    let op = rows.iter().find(|r| r["name"] == "op_hook").unwrap();
    assert_eq!(op["origin"], "operator");
    assert_eq!(op["mailbox"], "alice");
    // Masking: the operator's cmd / template / params must not appear
    // in the MCP-facing row.
    assert!(op.get("template").is_none(), "{op}");
    assert!(op.get("params").is_none(), "{op}");
    assert!(op.get("cmd").is_none(), "{op}");
    // And operator's command text must not appear anywhere in the
    // serialized payload.
    assert!(
        !text.contains("operator-secret"),
        "operator cmd leaked via hook_list: {text}"
    );

    let mcp = rows.iter().find(|r| r["name"] == "mcp_hook").unwrap();
    assert_eq!(mcp["origin"], "mcp");
    assert_eq!(mcp["template"], "invoke-claude");
    assert_eq!(mcp["params"]["prompt"], "agent secret");

    client.shutdown();
    stop_serve(daemon);
}

/// S5-4: `hook_delete` on an MCP-origin hook succeeds; on an operator-
/// origin hook the daemon's `origin-protected` message surfaces verbatim.
#[test]
fn mcp_hook_delete_respects_origin_protection() {
    let tmp = TempDir::new().unwrap();
    setup_test_env_with_template(tmp.path());

    // Plant an operator-origin hook on disk before starting the daemon.
    let config_path = tmp.path().join("config.toml");
    let existing = std::fs::read_to_string(&config_path).unwrap();
    let extra = "\n[[mailboxes.alice.hooks]]\nname = \"op_hook\"\n\
                 event = \"on_receive\"\ncmd = \"echo operator\"\n\
                 origin = \"operator\"\n";
    std::fs::write(&config_path, format!("{existing}{extra}")).unwrap();

    let port = find_free_port();
    let daemon = start_serve(tmp.path(), port);

    let mut client = McpClient::spawn(tmp.path());
    client.initialize();

    // Create an MCP-origin hook to delete.
    let resp = client.call_tool(
        "hook_create",
        serde_json::json!({
            "mailbox": "alice",
            "event": "on_receive",
            "template": "invoke-claude",
            "params": {"prompt": "hi"},
            "name": "mcp_hook"
        }),
    );
    assert!(!is_tool_error(&resp), "{resp}");

    // Delete MCP-origin hook: success.
    let resp = client.call_tool("hook_delete", serde_json::json!({"name": "mcp_hook"}));
    assert!(!is_tool_error(&resp), "{resp}");
    let text = get_tool_text(&resp);
    assert!(text.contains("deleted"), "{text}");

    // Delete operator-origin hook: daemon returns origin-protected.
    let resp = client.call_tool("hook_delete", serde_json::json!({"name": "op_hook"}));
    assert!(is_tool_error(&resp), "{resp}");
    let text = get_tool_text(&resp);
    assert!(text.contains("origin-protected"), "{text}");
    assert!(text.contains("sudo aimx hooks delete"), "{text}");

    // The operator hook must still be present on disk.
    let content = std::fs::read_to_string(&config_path).unwrap();
    assert!(
        content.contains("name = \"op_hook\""),
        "operator hook must survive protected-delete attempt: {content}"
    );

    client.shutdown();
    stop_serve(daemon);
}

/// S5-4: `hook_delete` without a running daemon returns a precise
/// socket-missing error.
#[test]
fn mcp_hook_delete_without_daemon_reports_missing_socket() {
    let tmp = TempDir::new().unwrap();
    setup_test_env_with_template(tmp.path());

    let mut client = McpClient::spawn(tmp.path());
    client.initialize();

    let resp = client.call_tool("hook_delete", serde_json::json!({"name": "anything"}));
    assert!(is_tool_error(&resp));
    let text = get_tool_text(&resp);
    assert!(text.contains("daemon not running"), "{text}");

    client.shutdown();
}

// ---------------------------------------------------------------------
// Sprint 6: S6-1 — End-to-end MCP → hook fire → sandbox verify
// ---------------------------------------------------------------------

/// Write a tiny mock-curl shell script to `path` that records its argv
/// (one per line) plus the full stdin to two sibling files:
///
/// * `<path>.argv` — one argv entry per line, the first line is `argv[0]`.
/// * `<path>.stdin` — raw bytes piped on stdin.
/// * `<path>.uid` — output of `id -u` so the root-gated UID assertion
///   in S6-1 can verify the daemon actually dropped privileges.
///
/// Returns the path to the script. Marked +x by the caller.
#[cfg(unix)]
fn write_mock_curl_script(path: &Path) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let argv_log = path.with_extension("argv");
    let stdin_log = path.with_extension("stdin");
    let uid_log = path.with_extension("uid");
    let script = format!(
        "#!/bin/sh\n\
         # Mock curl for S6-1 hook-template e2e test. Records argv + stdin.\n\
         echo \"$0\" > '{argv}'\n\
         for a in \"$@\"; do echo \"$a\" >> '{argv}'; done\n\
         cat > '{stdin}'\n\
         id -u > '{uid}'\n\
         exit 0\n",
        argv = argv_log.display(),
        stdin = stdin_log.display(),
        uid = uid_log.display(),
    );
    std::fs::write(path, script).expect("write mock curl script");
    let mut perms = std::fs::metadata(path).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(path, perms).unwrap();
    path.to_path_buf()
}

/// S6-1 (PRD §7 + §9): full MCP → daemon → hook fire round-trip.
///
/// Flow:
/// 1. Spin up `aimx serve` with a `webhook` template whose `cmd[0]` is a
///    mock-curl shell script in the test's tempdir.
/// 2. Connect an MCP client and call `hook_list_templates`. Expect the
///    `webhook` template to appear (validates Sprint 5 surface against
///    the actual config).
/// 3. Call `hook_create(mailbox=alice, event=after_send, template=webhook,
///    params={url=https://example.com/hook})`. Expect success and an
///    `origin = "mcp"` row in `config.toml`.
/// 4. Trigger the hook by submitting an outbound message via `aimx send`.
///    `after_send` fires through the same sandboxed executor as
///    `on_receive` (PRD §6.7) — and unlike `on_receive` it has no trust
///    gate, so an MCP-origin hook can fire without `dangerously_*` opt-in.
///    On-receive trust gating is exercised by separate Sprint 1–3 tests.
/// 5. Assert the mock-curl recorded the substituted argv (URL ends up at
///    the right slot, `cmd[0]` is the mock binary verbatim) and the JSON
///    stdin (the webhook template's `stdin = "email_json"` mode wraps
///    the persisted `.md` body in a `{ "raw": ... }` envelope).
#[cfg(unix)]
#[test]
fn hook_templates_end_to_end_mcp_to_sandbox() {
    let tmp = TempDir::new().unwrap();

    // Set up the mock-curl binary in the test tempdir. The webhook
    // template's `cmd[0]` will point at this script directly.
    let mock_curl = tmp.path().join("mock_curl.sh");
    let mock_curl_path = write_mock_curl_script(&mock_curl);
    let argv_log = mock_curl.with_extension("argv");
    let stdin_log = mock_curl.with_extension("stdin");
    let uid_log = mock_curl.with_extension("uid");

    // Build a config carrying the webhook template (cmd[0] points at the
    // mock) plus an `alice` mailbox. We use the `after_send` event so the
    // hook fire path is exercised end-to-end without depending on a real
    // DKIM signature on the inbound side (Sprint 1's `evaluate_trust`
    // requires DKIM=pass for `trusted = true`, which the test fixtures
    // can't satisfy — see hook_substitute fuzz tests for substitution
    // edge cases that complement this end-to-end coverage).
    let webhook_url = "https://example.com/aimx-hook";
    let config_content = format!(
        r#"domain = "agent.example.com"
data_dir = "{data_dir}"

[[hook_template]]
name = "webhook"
description = "POST the email as JSON to a URL"
cmd = ["{mock}", "-sS", "-X", "POST", "-H", "Content-Type: application/json", "--data-binary", "@-", "{{url}}"]
params = ["url"]
stdin = "email_json"
run_as = "aimx-hook"
timeout_secs = 30
allowed_events = ["on_receive", "after_send"]

[mailboxes.catchall]
address = "*@agent.example.com"

[mailboxes.alice]
address = "alice@agent.example.com"
"#,
        data_dir = tmp.path().display(),
        mock = mock_curl_path.display(),
    );
    std::fs::create_dir_all(tmp.path().join("inbox").join("catchall")).unwrap();
    std::fs::create_dir_all(tmp.path().join("inbox").join("alice")).unwrap();
    std::fs::create_dir_all(tmp.path().join("sent").join("alice")).unwrap();
    std::fs::write(tmp.path().join("config.toml"), &config_content).unwrap();
    install_cached_dkim_keys(tmp.path());

    let port = find_free_port();
    let mail_drop = tmp.path().join("outbound.log");
    let (daemon, _sock) = start_serve_with_mail_drop(tmp.path(), port, &mail_drop);

    // ----- Step 1: hook_list_templates surfaces the configured template.
    let mut client = McpClient::spawn(tmp.path());
    client.initialize();

    let templates_resp = client.call_tool("hook_list_templates", serde_json::json!({}));
    let templates_text = get_tool_text(&templates_resp);
    let templates_json: serde_json::Value =
        serde_json::from_str(&templates_text).expect(&templates_text);
    let arr = templates_json
        .as_array()
        .expect("hook_list_templates must return array");
    assert!(
        arr.iter().any(|t| t["name"] == "webhook"),
        "webhook template must appear in hook_list_templates: {templates_text}"
    );

    // ----- Step 2: hook_create through MCP → UDS → daemon stamps origin.
    let create_resp = client.call_tool(
        "hook_create",
        serde_json::json!({
            "mailbox": "alice",
            "event": "after_send",
            "template": "webhook",
            "params": {"url": webhook_url},
            "name": "s61_e2e_hook"
        }),
    );
    assert!(
        !is_tool_error(&create_resp),
        "hook_create must succeed: {create_resp}"
    );
    let create_text = get_tool_text(&create_resp);
    let create_json: serde_json::Value = serde_json::from_str(&create_text).expect(&create_text);
    assert_eq!(create_json["effective_name"], "s61_e2e_hook");
    let argv_field = create_json["substituted_argv"]
        .as_array()
        .expect("substituted_argv must be array");
    // First entry is the mock-curl path; last entry must be the URL slot.
    assert_eq!(
        argv_field[0],
        serde_json::json!(mock_curl_path.to_string_lossy().to_string())
    );
    assert_eq!(
        argv_field[argv_field.len() - 1],
        serde_json::json!(webhook_url),
        "last argv slot must carry the substituted URL: {argv_field:?}"
    );

    // ----- Step 3: confirm the daemon stamped origin = "mcp" in config.
    let cfg_after = std::fs::read_to_string(tmp.path().join("config.toml")).unwrap();
    assert!(
        cfg_after.contains("origin = \"mcp\""),
        "daemon must stamp origin = \"mcp\" on UDS-created hook: {cfg_after}"
    );
    assert!(
        cfg_after.contains("template = \"webhook\""),
        "config.toml must reference the bound template name: {cfg_after}"
    );

    // ----- Step 4: trigger after_send via `aimx send`. The daemon awaits
    // hook subprocess completion before replying to the SEND verb, so by
    // the time `aimx send` returns the mock-curl will have written its
    // argv + stdin files.
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
        .arg("S6-1 e2e test")
        .arg("--body")
        .arg("Test body for end-to-end hook fire")
        .output()
        .expect("aimx send failed to spawn");
    assert!(
        output.status.success(),
        "aimx send must succeed; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    client.shutdown();
    let daemon_stderr = stop_serve_capture_stderr(daemon);

    // ----- Step 4b: structured hook-fire log line carries run_as = "aimx-hook"
    // and template = "webhook". The fallback executor logs at info level via
    // tracing, which `aimx serve` mirrors to stderr by default.
    let log_plain = strip_ansi(&daemon_stderr);
    assert!(
        log_plain.contains("template=webhook"),
        "daemon stderr must include `template=webhook` log field; got: {log_plain}"
    );
    assert!(
        log_plain.contains("run_as=aimx-hook"),
        "daemon stderr must include `run_as=aimx-hook` log field even on non-root \
         (the daemon attempts the drop and records the intent); got: {log_plain}"
    );

    // ----- Step 5: inspect the captured argv + stdin from mock-curl.
    assert!(
        argv_log.exists(),
        "mock-curl must have written argv log at {}",
        argv_log.display()
    );
    let argv_lines: Vec<String> = std::fs::read_to_string(&argv_log)
        .unwrap()
        .lines()
        .map(|s| s.to_string())
        .collect();
    // First line is `$0` (the script path itself); subsequent lines are
    // the argv entries the daemon passed in. The webhook template
    // declares 8 arguments after `cmd[0]` so we expect 9 lines total
    // (argv[0] + 8 args).
    assert_eq!(
        argv_lines.len(),
        9,
        "expected 9 argv entries (1 cmd + 8 args); got {argv_lines:?}"
    );
    assert_eq!(
        argv_lines[0],
        mock_curl_path.to_string_lossy(),
        "argv[0] must be the mock-curl path verbatim"
    );
    // Header argument must land verbatim — proves the daemon did NOT
    // shell-split substituted values across argv slots.
    assert!(
        argv_lines
            .iter()
            .any(|a| a == "Content-Type: application/json"),
        "argv must contain unsplit Content-Type header: {argv_lines:?}"
    );
    // Final URL slot must carry the substituted value.
    assert_eq!(
        argv_lines.last().unwrap(),
        webhook_url,
        "last argv slot must be the substituted URL: {argv_lines:?}"
    );

    // The stdin file must contain a JSON object (webhook template uses
    // `stdin = "email_json"`). The daemon wraps the persisted `.md`
    // payload as `{"raw": "..."}` per Sprint 2's stdin handling.
    assert!(
        stdin_log.exists(),
        "mock-curl must have captured stdin at {}",
        stdin_log.display()
    );
    let stdin_text = std::fs::read_to_string(&stdin_log).unwrap();
    let stdin_json: serde_json::Value = serde_json::from_str(&stdin_text).unwrap_or_else(|e| {
        panic!("stdin must be valid JSON for stdin=email_json mode (err {e}): {stdin_text}");
    });
    let raw = stdin_json
        .get("raw")
        .and_then(|v| v.as_str())
        .expect("email_json stdin must carry a `raw` key with the email markdown body");
    assert!(
        raw.contains("S6-1 e2e test")
            || raw.contains("Test body for end-to-end hook fire")
            || raw.contains("alice@agent.example.com"),
        "stdin payload must reflect the sent email content: {raw}"
    );

    // ----- Step 6: root-gated UID assertion (skipped on non-root CI).
    // When the test runs as root, the sandbox actually drops to the
    // `aimx-hook` UID; the mock-curl `id -u` output proves it. On
    // non-root CI the executor falls back to the current user (logged
    // as a WARN by `spawn_sandboxed`), and the assertion is skipped.
    let is_root = unsafe { libc::geteuid() == 0 };
    if is_root {
        let uid_str = std::fs::read_to_string(&uid_log)
            .expect("mock-curl uid log must exist when running as root");
        let uid: u32 = uid_str.trim().parse().expect("uid log must be numeric");
        // We don't hardcode the aimx-hook UID — the test box may have
        // it provisioned at any system UID. We just assert it's not 0
        // (root), proving the daemon's setuid drop fired.
        assert_ne!(
            uid, 0,
            "subprocess must NOT run as root when daemon dropped privileges to aimx-hook"
        );
    } else {
        // Non-root path: the UID file should still exist (mock-curl ran),
        // but its value is the current test user's UID.
        if uid_log.exists() {
            let uid_str = std::fs::read_to_string(&uid_log).unwrap();
            let uid: u32 = uid_str.trim().parse().unwrap_or(0);
            let current_uid = unsafe { libc::geteuid() };
            assert_eq!(
                uid, current_uid,
                "non-root path: subprocess should run as the current test user"
            );
        }
    }
}
