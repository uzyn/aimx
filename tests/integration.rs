use assert_cmd::Command;
use predicates::prelude::*;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Command as StdCommand, Stdio};
use tempfile::TempDir;

fn setup_test_env(tmp: &Path) -> String {
    let config_content = format!(
        "domain: agent.example.com\ndata_dir: {}\nmailboxes:\n  catchall:\n    address: \"*@agent.example.com\"\n  alice:\n    address: alice@agent.example.com\n",
        tmp.display()
    );
    std::fs::create_dir_all(tmp.join("catchall")).unwrap();
    std::fs::create_dir_all(tmp.join("alice")).unwrap();
    let config_path = tmp.join("config.yaml");
    std::fs::write(&config_path, &config_content).unwrap();
    config_path.to_string_lossy().to_string()
}

fn read_frontmatter(md_path: &Path) -> serde_yaml::Value {
    let content = std::fs::read_to_string(md_path).unwrap();
    let parts: Vec<&str> = content.splitn(3, "---").collect();
    assert!(
        parts.len() >= 3,
        "Markdown file missing frontmatter delimiters"
    );
    serde_yaml::from_str(parts[1].trim()).unwrap()
}

fn get_yaml_str<'a>(map: &'a serde_yaml::Mapping, key: &str) -> &'a str {
    map.get(&serde_yaml::Value::String(key.to_string()))
        .and_then(|v| v.as_str())
        .unwrap_or("")
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
        .stdout(predicate::str::contains("preflight"))
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

    Command::cargo_bin("aimx")
        .unwrap()
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
    let map = parsed.as_mapping().unwrap();
    assert_eq!(get_yaml_str(map, "from"), "Alice <alice@example.com>");
    assert_eq!(get_yaml_str(map, "subject"), "Plain text test");
    assert_eq!(get_yaml_str(map, "message_id"), "plain-001@example.com");
    assert_eq!(get_yaml_str(map, "mailbox"), "catchall");
    assert_eq!(
        map.get(&serde_yaml::Value::String("read".to_string()))
            .unwrap(),
        &serde_yaml::Value::Bool(false)
    );

    let content = std::fs::read_to_string(&md_files[0]).unwrap();
    assert!(content.contains("This is a plain text email for testing."));
}

#[test]
fn ingest_html_fixture_full_pipeline() {
    let tmp = TempDir::new().unwrap();
    setup_test_env(tmp.path());
    let eml = std::fs::read("tests/fixtures/html_only.eml").unwrap();

    Command::cargo_bin("aimx")
        .unwrap()
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
    let map = parsed.as_mapping().unwrap();
    assert_eq!(get_yaml_str(map, "subject"), "HTML only test");

    let content = std::fs::read_to_string(&md_files[0]).unwrap();
    assert!(content.contains("Hello from HTML"));
    assert!(!content.contains("<html>"));
}

#[test]
fn ingest_multipart_fixture_full_pipeline() {
    let tmp = TempDir::new().unwrap();
    setup_test_env(tmp.path());
    let eml = std::fs::read("tests/fixtures/multipart.eml").unwrap();

    Command::cargo_bin("aimx")
        .unwrap()
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

    Command::cargo_bin("aimx")
        .unwrap()
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
    let map = parsed.as_mapping().unwrap();
    let attachments = map
        .get(&serde_yaml::Value::String("attachments".to_string()))
        .unwrap()
        .as_sequence()
        .unwrap();
    assert_eq!(attachments.len(), 1);
    let att = attachments[0].as_mapping().unwrap();
    assert_eq!(
        att.get(&serde_yaml::Value::String("filename".to_string()))
            .unwrap()
            .as_str()
            .unwrap(),
        "readme.txt"
    );
}

#[test]
fn ingest_routes_to_named_mailbox() {
    let tmp = TempDir::new().unwrap();
    setup_test_env(tmp.path());
    let eml = std::fs::read("tests/fixtures/plain.eml").unwrap();

    Command::cargo_bin("aimx")
        .unwrap()
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

    Command::cargo_bin("aimx")
        .unwrap()
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

    Command::cargo_bin("aimx")
        .unwrap()
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
    setup_test_env(tmp.path());

    Command::cargo_bin("aimx")
        .unwrap()
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
    assert!(public_pem.contains("BEGIN RSA PUBLIC KEY"));
}

#[test]
fn dkim_keygen_no_overwrite() {
    let tmp = TempDir::new().unwrap();
    setup_test_env(tmp.path());

    Command::cargo_bin("aimx")
        .unwrap()
        .arg("--data-dir")
        .arg(tmp.path())
        .arg("dkim-keygen")
        .assert()
        .success();

    Command::cargo_bin("aimx")
        .unwrap()
        .arg("--data-dir")
        .arg(tmp.path())
        .arg("dkim-keygen")
        .assert()
        .failure()
        .stderr(predicate::str::contains("already exist"));
}

#[test]
fn dkim_keygen_force_overwrite() {
    let tmp = TempDir::new().unwrap();
    setup_test_env(tmp.path());

    Command::cargo_bin("aimx")
        .unwrap()
        .arg("--data-dir")
        .arg(tmp.path())
        .arg("dkim-keygen")
        .assert()
        .success();

    let original = std::fs::read_to_string(tmp.path().join("dkim/private.key")).unwrap();

    Command::cargo_bin("aimx")
        .unwrap()
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
        "---\nid: {id}\nmessage_id: \"<{id}@test.com>\"\nfrom: {from}\nto: alice@test.com\nsubject: {subject}\ndate: '2025-06-01T12:00:00Z'\nin_reply_to: ''\nreferences: ''\nattachments: []\nmailbox: alice\nread: {read}\n---\n\nBody of {id}.\n"
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
    assert!(content.contains("read: true"));

    let resp = client.call_tool(
        "email_mark_unread",
        serde_json::json!({"mailbox": "alice", "id": "2025-06-01-001"}),
    );
    let text = get_tool_text(&resp);
    assert!(text.contains("marked as unread"));

    let content = std::fs::read_to_string(tmp.path().join("alice/2025-06-01-001.md")).unwrap();
    assert!(content.contains("read: false"));

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
        r#"domain: agent.example.com
data_dir: {}
mailboxes:
  catchall:
    address: "*@agent.example.com"
    on_receive:
      - type: cmd
        command: 'touch {}'
"#,
        tmp.display(),
        trigger_marker.display()
    );
    std::fs::create_dir_all(tmp.join("catchall")).unwrap();
    let config_path = tmp.join("config.yaml");
    std::fs::write(&config_path, &config_content).unwrap();
    config_path.to_string_lossy().to_string()
}

#[test]
fn ingest_trigger_executes_on_delivery() {
    let tmp = TempDir::new().unwrap();
    let marker = tmp.path().join("trigger.marker");
    setup_test_env_with_triggers(tmp.path(), &marker);
    let eml = std::fs::read("tests/fixtures/plain.eml").unwrap();

    Command::cargo_bin("aimx")
        .unwrap()
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
        r#"domain: agent.example.com
data_dir: {}
mailboxes:
  catchall:
    address: "*@agent.example.com"
    on_receive:
      - type: cmd
        command: 'false'
"#,
        tmp.path().display()
    );
    std::fs::create_dir_all(tmp.path().join("catchall")).unwrap();
    std::fs::write(tmp.path().join("config.yaml"), &config_content).unwrap();

    let eml = std::fs::read("tests/fixtures/plain.eml").unwrap();

    Command::cargo_bin("aimx")
        .unwrap()
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
        r#"domain: agent.example.com
data_dir: {}
mailboxes:
  catchall:
    address: "*@agent.example.com"
    trust: verified
    on_receive:
      - type: cmd
        command: 'touch {}'
"#,
        tmp.path().display(),
        marker.display()
    );
    std::fs::create_dir_all(tmp.path().join("catchall")).unwrap();
    std::fs::write(tmp.path().join("config.yaml"), &config_content).unwrap();

    let eml = std::fs::read("tests/fixtures/plain.eml").unwrap();

    Command::cargo_bin("aimx")
        .unwrap()
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
        r#"domain: agent.example.com
data_dir: {}
mailboxes:
  catchall:
    address: "*@agent.example.com"
    trust: none
    on_receive:
      - type: cmd
        command: 'touch {}'
"#,
        tmp.path().display(),
        marker.display()
    );
    std::fs::create_dir_all(tmp.path().join("catchall")).unwrap();
    std::fs::write(tmp.path().join("config.yaml"), &config_content).unwrap();

    let eml = std::fs::read("tests/fixtures/plain.eml").unwrap();

    Command::cargo_bin("aimx")
        .unwrap()
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

    Command::cargo_bin("aimx")
        .unwrap()
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
    let map = parsed.as_mapping().unwrap();

    let dkim = get_yaml_str(map, "dkim");
    assert!(
        dkim == "none" || dkim == "pass" || dkim == "fail",
        "dkim should be pass|fail|none, got: {dkim}"
    );

    let spf = get_yaml_str(map, "spf");
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
        r#"domain: agent.example.com
data_dir: {}
mailboxes:
  catchall:
    address: "*@agent.example.com"
    trust: verified
    trusted_senders:
      - "*@example.com"
    on_receive:
      - type: cmd
        command: 'touch {}'
"#,
        tmp.path().display(),
        marker.display()
    );
    std::fs::create_dir_all(tmp.path().join("catchall")).unwrap();
    std::fs::write(tmp.path().join("config.yaml"), &config_content).unwrap();

    let eml = std::fs::read("tests/fixtures/plain.eml").unwrap();

    Command::cargo_bin("aimx")
        .unwrap()
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
