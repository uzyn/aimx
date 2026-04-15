use assert_cmd::Command;
use predicates::prelude::*;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Command as StdCommand, Stdio};
use tempfile::TempDir;
use wait_timeout::ChildExt;

fn setup_test_env(tmp: &Path) -> String {
    let config_content = format!(
        "domain = \"agent.example.com\"\ndata_dir = \"{}\"\n\n[mailboxes.catchall]\naddress = \"*@agent.example.com\"\n\n[mailboxes.alice]\naddress = \"alice@agent.example.com\"\n",
        tmp.display()
    );
    std::fs::create_dir_all(tmp.join("catchall")).unwrap();
    std::fs::create_dir_all(tmp.join("alice")).unwrap();
    let config_path = tmp.join("config.toml");
    std::fs::write(&config_path, &config_content).unwrap();
    // Sprint 34: `aimx serve` loads the DKIM private key at startup and
    // refuses to start if it's missing. Every integration test that spawns
    // `aimx serve` as a subprocess needs a DKIM key on disk inside the
    // AIMX_CONFIG_DIR tempdir. Generate via the `dkim-keygen` subcommand
    // through the binary path to keep this helper dependency-free.
    let dkim_dir = tmp.join("dkim");
    if !dkim_dir.join("private.key").exists() {
        let status = StdCommand::new(aimx_binary_path())
            .env("AIMX_CONFIG_DIR", tmp)
            .arg("dkim-keygen")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .expect("Failed to run aimx dkim-keygen in test setup");
        assert!(
            status.success(),
            "aimx dkim-keygen exited non-zero in test setup"
        );
    }
    config_path.to_string_lossy().to_string()
}

/// Build an `aimx` Command pre-wired with `AIMX_CONFIG_DIR` pointed at the
/// test's tempdir. v0.2 (Sprint 33) moved config out of the storage
/// directory, so integration tests must override both the storage path
/// (`--data-dir` / `AIMX_DATA_DIR`) and the config lookup via this env var.
fn aimx_cmd(tmp: &Path) -> Command {
    let mut cmd = Command::cargo_bin("aimx").unwrap();
    cmd.env("AIMX_CONFIG_DIR", tmp);
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
    std::fs::read_dir(dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|ext| ext == "md"))
        .collect()
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
        .stdout(predicate::str::contains("mailbox"))
        .stdout(predicate::str::contains("mcp"))
        .stdout(predicate::str::contains("setup"))
        .stdout(predicate::str::contains("status"))
        .stdout(predicate::str::contains("serve"))
        .stdout(predicate::str::contains("verify"))
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

    let md_files = find_md_files(&tmp.path().join("catchall"));
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

    let md_files = find_md_files(&tmp.path().join("catchall"));
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

    let md_files = find_md_files(&tmp.path().join("catchall"));
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

    let md_files = find_md_files(&tmp.path().join("catchall"));
    assert_eq!(md_files.len(), 1);

    let att_path = tmp.path().join("catchall/attachments/readme.txt");
    assert!(att_path.exists());
    let att_content = std::fs::read_to_string(&att_path).unwrap();
    assert!(att_content.contains("This is the content of the attached file."));

    let parsed = read_frontmatter(&md_files[0]);
    let table = parsed.as_table().unwrap();
    let attachments = table.get("attachments").unwrap().as_array().unwrap();
    assert_eq!(attachments.len(), 1);
    let att = attachments[0].as_table().unwrap();
    assert_eq!(att.get("filename").unwrap().as_str().unwrap(), "readme.txt");
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

    let alice_files = find_md_files(&tmp.path().join("alice"));
    assert_eq!(alice_files.len(), 1);

    let catchall_files = find_md_files(&tmp.path().join("catchall"));
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

    let catchall_files = find_md_files(&tmp.path().join("catchall"));
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

    let md_files = find_md_files(&tmp.path().join("catchall"));
    assert_eq!(md_files.len(), 1);
}

#[test]
fn dkim_keygen_end_to_end() {
    let tmp = TempDir::new().unwrap();
    // Write config.toml but skip DKIM key generation — the `dkim-keygen`
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
        let mut child = StdCommand::new(aimx_binary_path())
            .env("AIMX_CONFIG_DIR", data_dir)
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
    assert_eq!(tools.len(), 9);

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
    assert!(tmp.path().join("support").is_dir());

    let resp = client.call_tool("mailbox_list", serde_json::json!({}));
    let text = get_tool_text(&resp);
    assert!(text.contains("catchall"));
    assert!(text.contains("support"));
    assert!(text.contains("alice"));

    let resp = client.call_tool("mailbox_delete", serde_json::json!({"name": "support"}));
    let text = get_tool_text(&resp);
    assert!(text.contains("deleted"));
    assert!(!tmp.path().join("support").exists());

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

    let alice_dir = tmp.path().join("alice");
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

#[test]
fn mcp_email_mark_read_unread() {
    let tmp = TempDir::new().unwrap();
    setup_test_env(tmp.path());

    let alice_dir = tmp.path().join("alice");
    create_email_file(
        &alice_dir,
        "2025-06-01-001",
        "sender@example.com",
        "Hello",
        false,
    );

    let mut client = McpClient::spawn(tmp.path());
    client.initialize();

    let resp = client.call_tool(
        "email_mark_read",
        serde_json::json!({"mailbox": "alice", "id": "2025-06-01-001"}),
    );
    let text = get_tool_text(&resp);
    assert!(text.contains("marked as read"));

    let content = std::fs::read_to_string(tmp.path().join("alice/2025-06-01-001.md")).unwrap();
    assert!(content.contains("read = true"));

    let resp = client.call_tool(
        "email_mark_unread",
        serde_json::json!({"mailbox": "alice", "id": "2025-06-01-001"}),
    );
    let text = get_tool_text(&resp);
    assert!(text.contains("marked as unread"));

    let content = std::fs::read_to_string(tmp.path().join("alice/2025-06-01-001.md")).unwrap();
    assert!(content.contains("read = false"));

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

[[mailboxes.catchall.on_receive]]
type = "cmd"
command = "touch {}"
"#,
        tmp.display(),
        trigger_marker.display()
    );
    std::fs::create_dir_all(tmp.join("catchall")).unwrap();
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

    let md_files = find_md_files(&tmp.path().join("catchall"));
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

[[mailboxes.catchall.on_receive]]
type = "cmd"
command = "false"
"#,
        tmp.path().display()
    );
    std::fs::create_dir_all(tmp.path().join("catchall")).unwrap();
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

    let md_files = find_md_files(&tmp.path().join("catchall"));
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

[[mailboxes.catchall.on_receive]]
type = "cmd"
command = "touch {}"
"#,
        tmp.path().display(),
        marker.display()
    );
    std::fs::create_dir_all(tmp.path().join("catchall")).unwrap();
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

    let md_files = find_md_files(&tmp.path().join("catchall"));
    assert_eq!(md_files.len(), 1, "Email should still be saved");
    assert!(
        !marker.exists(),
        "Trigger should NOT fire for unsigned email with trust=verified"
    );
}

#[test]
fn ingest_trust_none_allows_unsigned_trigger() {
    let tmp = TempDir::new().unwrap();
    let marker = tmp.path().join("triggered");
    let config_content = format!(
        r#"domain = "agent.example.com"
data_dir = "{}"

[mailboxes.catchall]
address = "*@agent.example.com"
trust = "none"

[[mailboxes.catchall.on_receive]]
type = "cmd"
command = "touch {}"
"#,
        tmp.path().display(),
        marker.display()
    );
    std::fs::create_dir_all(tmp.path().join("catchall")).unwrap();
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

    let md_files = find_md_files(&tmp.path().join("catchall"));
    assert_eq!(md_files.len(), 1);
    assert!(
        marker.exists(),
        "Trigger should fire with trust=none even for unsigned email"
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

    let md_files = find_md_files(&tmp.path().join("catchall"));
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
    let tmp = TempDir::new().unwrap();
    let marker = tmp.path().join("triggered");
    let config_content = format!(
        r#"domain = "agent.example.com"
data_dir = "{}"

[mailboxes.catchall]
address = "*@agent.example.com"
trust = "verified"
trusted_senders = ["*@example.com"]

[[mailboxes.catchall.on_receive]]
type = "cmd"
command = "touch {}"
"#,
        tmp.path().display(),
        marker.display()
    );
    std::fs::create_dir_all(tmp.path().join("catchall")).unwrap();
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

    let md_files = find_md_files(&tmp.path().join("catchall"));
    assert_eq!(md_files.len(), 1);
    assert!(
        marker.exists(),
        "Trigger should fire for trusted sender even with trust=verified"
    );
}

/// S31-2: end-to-end channel-recipe test.
///
/// Drives the full ingest -> channel-rule-match -> templated shell command
/// path with an assert-able one-liner that writes `{filepath}` and
/// `{subject}` into a marker file. A second rule exits non-zero to prove
/// that trigger failure does NOT block delivery. This is the smoke test
/// protecting every recipe in `book/channel-recipes.md` from regressions.
#[test]
fn channel_recipe_end_to_end_with_templated_args() {
    let tmp = TempDir::new().unwrap();
    let marker = tmp.path().join("recipe.marker");
    let config_content = format!(
        r#"domain = "agent.example.com"
data_dir = "{data_dir}"

[mailboxes.catchall]
address = "*@agent.example.com"

[[mailboxes.catchall.on_receive]]
type = "cmd"
command = 'printf "filepath=%s\nsubject=%s\n" {{filepath}} {{subject}} > {marker}'

[[mailboxes.catchall.on_receive]]
type = "cmd"
command = "false"
"#,
        data_dir = tmp.path().display(),
        marker = marker.display()
    );
    std::fs::create_dir_all(tmp.path().join("catchall")).unwrap();
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

    let md_files = find_md_files(&tmp.path().join("catchall"));
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
        "Marker should contain the expanded {{filepath}} value; got: {contents}"
    );
    assert!(
        contents.contains("subject=Plain text test"),
        "Marker should contain the expanded {{subject}} value; got: {contents}"
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
fn status_shows_domain_and_mailboxes() {
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
        .arg("status")
        .assert()
        .success()
        .stdout(predicate::str::contains("agent.example.com"))
        .stdout(predicate::str::contains("catchall"))
        .stdout(predicate::str::contains("alice"))
        .stdout(predicate::str::contains("MAILBOX"));
}

#[test]
fn status_help_works() {
    Command::cargo_bin("aimx")
        .unwrap()
        .args(["status", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("status"));
}

#[test]
fn verify_help_works() {
    Command::cargo_bin("aimx")
        .unwrap()
        .args(["verify", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("verify"));
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
        if started.elapsed() > std::time::Duration::from_secs(10) {
            child.kill().unwrap();
            panic!("aimx serve did not start within 10s");
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

    let alice_dir = tmp.path().join("alice");
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
        if started.elapsed() > std::time::Duration::from_secs(10) {
            child.kill().unwrap();
            panic!("aimx serve did not start within 10s");
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

    let alice_files = find_md_files(&tmp.path().join("alice"));
    let catchall_files = find_md_files(&tmp.path().join("catchall"));
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
        if started.elapsed() > std::time::Duration::from_secs(10) {
            child.kill().unwrap();
            panic!("aimx serve did not start within 10s");
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
    std::fs::create_dir_all(tmp.join("catchall")).unwrap();
    std::fs::create_dir_all(tmp.join("alice")).unwrap();
    std::fs::create_dir_all(tmp.join("bob")).unwrap();
    let config_path = tmp.join("config.toml");
    std::fs::write(&config_path, &config_content).unwrap();
    // Sprint 34: `aimx serve` requires the DKIM private key at startup.
    let dkim_dir = tmp.join("dkim");
    if !dkim_dir.join("private.key").exists() {
        let status = StdCommand::new(aimx_binary_path())
            .env("AIMX_CONFIG_DIR", tmp)
            .arg("dkim-keygen")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .expect("Failed to run aimx dkim-keygen in test setup");
        assert!(
            status.success(),
            "aimx dkim-keygen exited non-zero in test setup"
        );
    }
    config_path.to_string_lossy().to_string()
}

fn start_serve(tmp: &Path, port: u16) -> std::process::Child {
    let runtime = tmp.join("run");
    std::fs::create_dir_all(&runtime).ok();
    let mut child = StdCommand::new(aimx_binary_path())
        .env("AIMX_CONFIG_DIR", tmp)
        .env("AIMX_RUNTIME_DIR", &runtime)
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
        if started.elapsed() > std::time::Duration::from_secs(10) {
            child.kill().unwrap();
            panic!("aimx serve did not start within 10s");
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

    let alice_dir = tmp.path().join("alice");
    let md_files = find_md_files(&alice_dir);
    assert_eq!(md_files.len(), 1, "Expected 1 email in alice mailbox");

    let att_path = alice_dir.join("attachments").join("report.txt");
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
    assert_eq!(get_toml_str(att, "path"), "attachments/report.txt");
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

    let alice_dir = tmp.path().join("alice");
    let md_files = find_md_files(&alice_dir);
    assert_eq!(md_files.len(), 1, "Expected 1 email in alice mailbox");

    let att_dir = alice_dir.join("attachments");
    assert!(
        att_dir.join("notes.txt").exists(),
        "notes.txt should exist on disk"
    );
    assert!(
        att_dir.join("data.csv").exists(),
        "data.csv should exist on disk"
    );
    assert!(
        att_dir.join("image.png").exists(),
        "image.png should exist on disk"
    );

    let notes = std::fs::read_to_string(att_dir.join("notes.txt")).unwrap();
    assert!(notes.contains("Meeting notes"));

    let csv = std::fs::read_to_string(att_dir.join("data.csv")).unwrap();
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

    let alice_dir = tmp.path().join("alice");
    let catchall_dir = tmp.path().join("catchall");
    assert_eq!(find_md_files(&alice_dir).len(), 1);
    assert_eq!(find_md_files(&catchall_dir).len(), 1);

    let alice_att = alice_dir.join("attachments").join("shared.txt");
    let catchall_att = catchall_dir.join("attachments").join("shared.txt");
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

    let alice_files = find_md_files(&tmp.path().join("alice"));
    let bob_files = find_md_files(&tmp.path().join("bob"));
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

    // No BCC header — bob is BCC'd via envelope only
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

    let alice_files = find_md_files(&tmp.path().join("alice"));
    let bob_files = find_md_files(&tmp.path().join("bob"));
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

    // BCC address should not appear as a header in the stored email
    let bob_content = std::fs::read_to_string(&bob_files[0]).unwrap();
    assert!(
        !bob_content.contains("Bcc:")
            && !bob_content.contains("bcc:")
            && !bob_content.contains("BCC:"),
        "BCC header line should not be in stored email"
    );
    assert!(
        !bob_content.contains("bob@agent.example.com"),
        "BCC recipient address should not appear in stored email"
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

    let alice_files = find_md_files(&tmp.path().join("alice"));
    let bob_files = find_md_files(&tmp.path().join("bob"));
    let catchall_files = find_md_files(&tmp.path().join("catchall"));
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
// Sprint 34 — UDS send listener integration tests.
//
// These tests spawn `aimx serve` as a subprocess (same pattern as the
// `serve_e2e_*` tests above) and drive the `/run/aimx/send.sock` UDS
// listener with a raw Unix-socket client. `AIMX_RUNTIME_DIR` is overridden
// to a tempdir so the socket lives inside the test sandbox; the binary
// under test creates it with mode `0o666`. We never exercise the real MX
// delivery path — the `ERR DOMAIN` and `ERR MALFORMED` responses prove the
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

    let sock = tmp.path().join("run").join("send.sock");
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

    let sock = tmp.path().join("run").join("send.sock");
    assert!(
        wait_for_socket(&sock, std::time::Duration::from_secs(5)),
        "UDS send socket never appeared"
    );

    // Submit an AIMX/1 SEND with a From: domain that does not match the
    // configured primary domain (`agent.example.com`). This must be
    // rejected with `ERR DOMAIN` before any MX lookup happens, proving
    // both the wire parser and the handler wiring end-to-end.
    let body = b"From: alice@not-the-domain.example\r\n\
                 To: user@gmail.com\r\n\
                 Subject: Hi\r\n\
                 Date: Thu, 01 Jan 2025 12:00:00 +0000\r\n\
                 Message-ID: <integ-abc@not-the-domain.example>\r\n\
                 \r\n\
                 hello\r\n";
    let header = format!(
        "AIMX/1 SEND\nFrom-Mailbox: alice\nContent-Length: {}\n\n",
        body.len()
    );

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

    let sock = tmp.path().join("run").join("send.sock");
    assert!(
        wait_for_socket(&sock, std::time::Duration::from_secs(5)),
        "UDS send socket never appeared"
    );

    // Wrong leading line — must be rejected with `ERR MALFORMED`.
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

    let sock = tmp.path().join("run").join("send.sock");
    assert!(
        wait_for_socket(&sock, std::time::Duration::from_secs(5)),
        "UDS send socket never appeared"
    );

    // Clean shutdown — the listener removes the socket file so the next
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
        "send.sock should be removed on clean shutdown but still exists at {}",
        sock.display()
    );
}
