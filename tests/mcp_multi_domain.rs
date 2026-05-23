//! End-to-end MCP integration tests for multi-domain installs.
//!
//! These tests spin up `aimx serve` against a two-domain config and
//! drive the MCP server (`aimx mcp`) over stdio. They pin two
//! behaviours the multi-domain track promised:
//!
//! - `mailbox_list` returns FQDN-shaped names (`info@a.com`,
//!   `support@b.com`), never bare local-parts. Agents disambiguate
//!   identical local-parts across domains via the `@<domain>` suffix.
//! - `email_list` accepts both bare local-parts and the FQDN form
//!   against the same mailbox, with bare local-parts resolving to
//!   `domains[0]`.
//!
//! A single-domain regression test mirrors the same shape against a
//! one-domain install so the MCP response is uniform regardless of
//! domain count.

#![cfg(unix)]

use std::io::{BufRead, BufReader, Write};
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

/// One-shot pre-generated DKIM keypair shared by every test in this
/// file. Mirrors the cache used by `tests/multi_domain.rs` so we don't
/// re-run `aimx dkim-keygen` (~200ms) per test.
static MCP_MD_DKIM_CACHE: LazyLock<TempDir> = LazyLock::new(|| {
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
    let cache_dkim = MCP_MD_DKIM_CACHE
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

fn wait_for_socket(path: &Path, timeout: Duration) -> bool {
    let started = Instant::now();
    while started.elapsed() < timeout {
        if path.exists() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
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

/// Provision a two-domain v2 install under `tmp` with one mailbox per
/// domain (`info@a.com`, `info@b.com`) — identical local-parts so the
/// FQDN suffix is what makes them distinct.
fn setup_two_domain_env(tmp: &Path) {
    let owner = current_username();
    let cfg = format!(
        r#"domains = ["a.com", "b.com"]
data_dir = "{tmp_path}"

[mailboxes."info@a.com"]
address = "info@a.com"
owner = "{owner}"

[mailboxes."info@b.com"]
address = "info@b.com"
owner = "{owner}"
"#,
        tmp_path = tmp.display(),
    );
    std::fs::write(tmp.join("config.toml"), cfg).unwrap();
    std::fs::write(tmp.join(".layout-version"), "2\n").unwrap();

    install_dkim_under(&tmp.join("dkim").join("a.com"));
    install_dkim_under(&tmp.join("dkim").join("b.com"));

    for domain in ["a.com", "b.com"] {
        for folder in ["inbox", "sent"] {
            let dir = tmp.join(domain).join(folder).join("info");
            std::fs::create_dir_all(&dir).unwrap();
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(tmp.join(domain), std::fs::Permissions::from_mode(0o755)).unwrap();
    }
}

/// Provision a single-domain v2 install with one mailbox under
/// `info@example.com`. Used by the single-domain regression test to
/// confirm the MCP response shape is uniform across domain counts.
fn setup_single_domain_env(tmp: &Path) {
    let owner = current_username();
    let cfg = format!(
        r#"domains = ["example.com"]
data_dir = "{tmp_path}"

[mailboxes."info@example.com"]
address = "info@example.com"
owner = "{owner}"
"#,
        tmp_path = tmp.display(),
    );
    std::fs::write(tmp.join("config.toml"), cfg).unwrap();
    std::fs::write(tmp.join(".layout-version"), "2\n").unwrap();

    install_dkim_under(&tmp.join("dkim").join("example.com"));

    for folder in ["inbox", "sent"] {
        let dir = tmp.join("example.com").join(folder).join("info");
        std::fs::create_dir_all(&dir).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(
        tmp.join("example.com"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
}

/// Minimal MCP client speaking the JSON-RPC 2.0 newline-framed dialect
/// over stdio. Sufficient for `initialize` + `tools/call`; mirrors the
/// shape of the one in `tests/integration.rs` but lives here so this
/// file is self-contained.
struct McpClient {
    child: std::process::Child,
    stdin: std::process::ChildStdin,
    reader: BufReader<std::process::ChildStdout>,
    id: i64,
}

impl McpClient {
    fn spawn(tmp: &Path) -> Self {
        let runtime = tmp.join("run");
        std::fs::create_dir_all(&runtime).ok();
        let mut child = StdCommand::new(aimx_binary_path())
            .env("AIMX_CONFIG_DIR", tmp)
            .env("AIMX_RUNTIME_DIR", &runtime)
            .arg("--data-dir")
            .arg(tmp)
            .arg("mcp")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("failed to spawn aimx mcp");
        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        Self {
            child,
            stdin,
            reader: BufReader::new(stdout),
            id: 0,
        }
    }

    fn send_request(&mut self, method: &str, params: serde_json::Value) -> serde_json::Value {
        self.id += 1;
        let req = serde_json::json!({
            "jsonrpc": "2.0",
            "id": self.id,
            "method": method,
            "params": params,
        });
        writeln!(self.stdin, "{}", serde_json::to_string(&req).unwrap()).unwrap();
        self.stdin.flush().unwrap();
        let mut line = String::new();
        let n = self.reader.read_line(&mut line).unwrap();
        if n == 0 {
            panic!("MCP closed stdout before responding to {method:?}");
        }
        serde_json::from_str(line.trim())
            .unwrap_or_else(|e| panic!("MCP non-JSON for {method:?}: {e} (raw: {line:?})"))
    }

    fn send_notification(&mut self, method: &str, params: serde_json::Value) {
        let n = serde_json::json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        });
        writeln!(self.stdin, "{}", serde_json::to_string(&n).unwrap()).unwrap();
        self.stdin.flush().unwrap();
    }

    fn initialize(&mut self) {
        let _ = self.send_request(
            "initialize",
            serde_json::json!({
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": {"name": "mcp-md-test", "version": "0.1"},
            }),
        );
        self.send_notification("notifications/initialized", serde_json::json!({}));
    }

    fn call_tool(&mut self, name: &str, args: serde_json::Value) -> serde_json::Value {
        self.send_request(
            "tools/call",
            serde_json::json!({"name": name, "arguments": args}),
        )
    }

    fn shutdown(mut self) {
        drop(self.stdin);
        let _ = self.child.wait_timeout(Duration::from_secs(5));
        if let Some(mut c) = Some(self.child) {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

fn tool_text(response: &serde_json::Value) -> String {
    response["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or("")
        .to_string()
}

fn is_tool_error(response: &serde_json::Value) -> bool {
    response["result"]["isError"].as_bool().unwrap_or(false)
}

/// Two-domain install: `mailbox_list` returns FQDN-shaped names and
/// `email_list` accepts both `info` (resolves to `domains[0]`) and the
/// explicit `info@a.com` form for the same mailbox.
#[test]
fn two_domain_mcp_returns_fqdn_and_accepts_both_input_shapes() {
    let tmp = TempDir::new().unwrap();
    setup_two_domain_env(tmp.path());
    let port = find_free_port();
    let mut daemon = start_serve(tmp.path(), port);
    wait_for_listener(port);
    let sock = tmp.path().join("run").join("aimx.sock");
    assert!(
        wait_for_socket(&sock, Duration::from_secs(10)),
        "UDS socket {} never appeared",
        sock.display()
    );

    let mut client = McpClient::spawn(tmp.path());
    client.initialize();

    // mailbox_list: every row carries an FQDN-shaped name.
    let resp = client.call_tool("mailbox_list", serde_json::json!({}));
    let text = tool_text(&resp);
    let rows: serde_json::Value = serde_json::from_str(&text)
        .unwrap_or_else(|e| panic!("mailbox_list returned non-JSON: {text}: {e}"));
    let arr = rows.as_array().expect("expected JSON array");
    let names: Vec<&str> = arr
        .iter()
        .filter_map(|r| r.get("name").and_then(|v| v.as_str()))
        .collect();
    assert!(
        names.contains(&"info@a.com"),
        "expected info@a.com in {names:?}"
    );
    assert!(
        names.contains(&"info@b.com"),
        "expected info@b.com in {names:?}"
    );
    for row in arr {
        let name = row.get("name").and_then(|v| v.as_str()).unwrap_or("");
        assert!(
            name.contains('@'),
            "MCP mailbox_list row must carry FQDN, not bare local-part: {name}"
        );
    }

    // email_list against the bare local-part `info` resolves to
    // domains[0] (a.com). Returns an empty JSON array, not a tool error.
    let resp = client.call_tool("email_list", serde_json::json!({"mailbox": "info"}));
    assert!(
        !is_tool_error(&resp),
        "email_list with bare local-part must not error on multi-domain install: {resp}"
    );
    let text = tool_text(&resp);
    let _rows_local: serde_json::Value = serde_json::from_str(&text)
        .unwrap_or_else(|e| panic!("email_list (local-part) returned non-JSON: {text}: {e}"));

    // email_list against the FQDN `info@a.com` also succeeds against the
    // same underlying mailbox.
    let resp = client.call_tool("email_list", serde_json::json!({"mailbox": "info@a.com"}));
    assert!(
        !is_tool_error(&resp),
        "email_list with FQDN must succeed: {resp}"
    );
    let text = tool_text(&resp);
    let _rows_fqdn: serde_json::Value = serde_json::from_str(&text)
        .unwrap_or_else(|e| panic!("email_list (FQDN) returned non-JSON: {text}: {e}"));

    // And the explicit second-domain FQDN resolves independently.
    let resp = client.call_tool("email_list", serde_json::json!({"mailbox": "info@b.com"}));
    assert!(
        !is_tool_error(&resp),
        "email_list against second domain must succeed: {resp}"
    );

    client.shutdown();
    shutdown(&mut daemon);
}

/// Single-domain install: MCP response shape matches the multi-domain
/// shape — `mailbox_list` rows still carry FQDN-shaped names so agents
/// do not need to branch on domain count.
#[test]
fn single_domain_mcp_returns_fqdn_names() {
    let tmp = TempDir::new().unwrap();
    setup_single_domain_env(tmp.path());
    let port = find_free_port();
    let mut daemon = start_serve(tmp.path(), port);
    wait_for_listener(port);
    let sock = tmp.path().join("run").join("aimx.sock");
    assert!(
        wait_for_socket(&sock, Duration::from_secs(10)),
        "UDS socket {} never appeared",
        sock.display()
    );

    let mut client = McpClient::spawn(tmp.path());
    client.initialize();

    let resp = client.call_tool("mailbox_list", serde_json::json!({}));
    let text = tool_text(&resp);
    let rows: serde_json::Value = serde_json::from_str(&text)
        .unwrap_or_else(|e| panic!("mailbox_list returned non-JSON: {text}: {e}"));
    let arr = rows.as_array().expect("expected JSON array");

    let names: Vec<&str> = arr
        .iter()
        .filter_map(|r| r.get("name").and_then(|v| v.as_str()))
        .collect();
    assert!(
        names.contains(&"info@example.com"),
        "single-domain mailbox_list must carry FQDN-shaped name; got {names:?}"
    );
    for row in arr {
        let name = row.get("name").and_then(|v| v.as_str()).unwrap_or("");
        assert!(
            name.contains('@'),
            "single-domain MCP rows still carry FQDN (uniform shape across domain counts): {name}"
        );
    }

    client.shutdown();
    shutdown(&mut daemon);
}
