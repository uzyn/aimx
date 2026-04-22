//! Sprint 8 S8-4: end-to-end happy path for the agent flow.
//!
//! Runs the full `aimx setup` (non-interactive, via direct config
//! layout) → `aimx agent-setup` (with a fake `claude` binary on a
//! controlled `$PATH`) → ingest of a trusted `.eml` → assertion that
//! the hook fired under the matching uid (via a sentinel file the
//! fake binary writes) → `aimx agent-cleanup --full` → assertion that
//! plugin files and the registered template are gone.
//!
//! Gated on `AIMX_INTEGRATION_SUDO=1` + root, matching `isolation.rs`.
//! A dedicated `aimx-it-agentflow` system user is created up front and
//! torn down via a `Drop` guard. CI runs this under sudo; developer
//! machines skip it by default.
//!
//! The fake `claude` is a tiny shell script written into a per-test
//! `PATH` dir; it writes its own uid and the incoming stdin to a
//! sentinel file under the tempdir so the test can assert the hook
//! fired under the expected user.

#![cfg(unix)]

use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const USER: &str = "aimx-it-agentflow";

struct UserTeardown;

impl Drop for UserTeardown {
    fn drop(&mut self) {
        let _ = Command::new("userdel").arg(USER).status();
    }
}

fn useradd_system(name: &str) {
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

fn aimx_binary_path() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_aimx"))
}

fn chown(path: &Path, uid: u32, gid: u32) {
    unsafe {
        let c = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).unwrap();
        let rc = libc::chown(c.as_ptr(), uid, gid);
        assert!(
            rc == 0,
            "chown({}) failed: {}",
            path.display(),
            std::io::Error::last_os_error()
        );
    }
}

fn chmod(path: &Path, mode: u32) {
    let perms = std::fs::Permissions::from_mode(mode);
    std::fs::set_permissions(path, perms).unwrap();
}

fn wait_for_socket(path: &Path, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if UnixStream::connect(path).is_ok() {
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    panic!(
        "aimx.sock did not appear / connect at {} within {:?}",
        path.display(),
        timeout
    );
}

#[test]
#[ignore]
fn end_to_end_agent_flow_installs_fires_and_cleans_up() {
    if std::env::var_os("AIMX_INTEGRATION_SUDO").is_none() {
        eprintln!("AIMX_INTEGRATION_SUDO is not set; skipping (requires root + useradd).");
        return;
    }
    assert_eq!(
        unsafe { libc::geteuid() },
        0,
        "e2e_agent_flow must run as root"
    );

    let _guard = UserTeardown;
    useradd_system(USER);
    let user_uid = uid_of(USER);
    let user_gid = user_uid;

    // Fresh tempdir datadir + config dir. Parent must be 0755 so the
    // test user can traverse.
    let tmp_root = std::env::temp_dir().join(format!("aimx-it-agentflow-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp_root);
    std::fs::create_dir_all(&tmp_root).unwrap();
    chmod(&tmp_root, 0o755);

    let config_dir = tmp_root.join("etc");
    let data_dir = tmp_root.join("data");
    let runtime_dir = tmp_root.join("run");
    let home_dir = tmp_root.join("home");
    let path_dir = tmp_root.join("bin");
    let sentinel_dir = tmp_root.join("sentinel");

    for d in [
        &config_dir,
        &data_dir,
        &runtime_dir,
        &home_dir,
        &path_dir,
        &sentinel_dir,
    ] {
        std::fs::create_dir_all(d).unwrap();
        chmod(d, 0o755);
    }

    // Sentinel dir needs to be writable by the test user since the fake
    // `claude` binary drops privilege to $USER before running.
    chown(&sentinel_dir, user_uid, user_gid);
    chmod(&sentinel_dir, 0o755);

    // Write minimal config: one mailbox owned by our test user, plus the
    // required catchall so Config::load validates.
    let config_path = config_dir.join("config.toml");
    let config_content = format!(
        "domain = \"it.example.com\"\n\
         trust = \"verified\"\n\
         trusted_senders = [\"*@example.com\"]\n\n\
         [mailboxes.catchall]\n\
         address = \"*@it.example.com\"\n\
         owner = \"aimx-catchall\"\n\n\
         [mailboxes.{USER}]\n\
         address = \"{USER}@it.example.com\"\n\
         owner = \"{USER}\"\n",
    );
    std::fs::write(&config_path, &config_content).unwrap();
    chmod(&config_path, 0o640);

    // Catchall user must exist for Config::load to not warn (it's
    // a reserved name so `getpwnam` isn't actually required, but
    // ingest writes will fail if the user is missing — create it if
    // absent, leave it alone if it already exists).
    let _ = Command::new("useradd")
        .arg("--system")
        .arg("--no-create-home")
        .arg("--shell")
        .arg("/usr/sbin/nologin")
        .arg("aimx-catchall")
        .status();

    // DKIM keypair.
    let keygen_status = Command::new(aimx_binary_path())
        .env("AIMX_CONFIG_DIR", &config_dir)
        .env("AIMX_DATA_DIR", &data_dir)
        .arg("dkim-keygen")
        .arg("--force")
        .status()
        .expect("dkim-keygen failed to spawn");
    assert!(keygen_status.success(), "dkim-keygen failed");

    // Pre-create the mailbox storage dirs with the correct ownership so
    // ingest can write immediately.
    let inbox_mbx = data_dir.join("inbox").join(USER);
    let sent_mbx = data_dir.join("sent").join(USER);
    std::fs::create_dir_all(&inbox_mbx).unwrap();
    std::fs::create_dir_all(&sent_mbx).unwrap();
    chown(&inbox_mbx, user_uid, user_gid);
    chown(&sent_mbx, user_uid, user_gid);
    chmod(&inbox_mbx, 0o700);
    chmod(&sent_mbx, 0o700);

    let inbox_catchall = data_dir.join("inbox").join("catchall");
    std::fs::create_dir_all(&inbox_catchall).unwrap();
    let catchall_uid = uid_of("aimx-catchall");
    chown(&inbox_catchall, catchall_uid, catchall_uid);
    chmod(&inbox_catchall, 0o700);

    // Fake `claude` binary. Writes its uid + stdin to the sentinel
    // file. Using $USER ensures only our test user can rewrite the
    // sentinel after privilege drop.
    let fake_claude = path_dir.join("claude");
    let sentinel = sentinel_dir.join("hook-fired.txt");
    let script = format!(
        "#!/bin/sh\n\
         echo \"uid=$(id -u) args=$*\" > {sentinel}\n\
         cat >> {sentinel}\n",
        sentinel = sentinel.display()
    );
    std::fs::write(&fake_claude, script).unwrap();
    chmod(&fake_claude, 0o755);
    // Make the fake binary + its parent traversable by the test user.
    chmod(&path_dir, 0o755);

    // The user's $HOME (used by `agent-setup` for plugin install).
    let user_home = home_dir.join(USER);
    std::fs::create_dir_all(&user_home).unwrap();
    chown(&user_home, user_uid, user_gid);
    chmod(&user_home, 0o755);

    // Start the daemon. Bind to a random port to avoid clashing with a
    // locally running aimx. `AIMX_SANDBOX_FORCE_FALLBACK=1` skips
    // systemd-run and uses the direct setresuid fallback so this test
    // works on minimal CI images.
    let port = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    let mut daemon = Command::new(aimx_binary_path())
        .env("AIMX_CONFIG_DIR", &config_dir)
        .env("AIMX_DATA_DIR", &data_dir)
        .env("AIMX_RUNTIME_DIR", &runtime_dir)
        .env("AIMX_SANDBOX_FORCE_FALLBACK", "1")
        .arg("--data-dir")
        .arg(&data_dir)
        .arg("serve")
        .arg("--bind")
        .arg(format!("127.0.0.1:{port}"))
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn aimx serve");

    let sock = runtime_dir.join("aimx.sock");
    wait_for_socket(&sock, Duration::from_secs(30));

    // Cleanup runs both daemon stop + tempdir removal on scope exit.
    struct Teardown(std::process::Child, PathBuf);
    impl Drop for Teardown {
        fn drop(&mut self) {
            unsafe {
                libc::kill(self.0.id() as libc::pid_t, libc::SIGTERM);
            }
            let _ = self.0.wait();
            let _ = std::fs::remove_dir_all(&self.1);
        }
    }
    let daemon_handle = Teardown(
        std::mem::replace(&mut daemon, Command::new("true").spawn().unwrap()),
        tmp_root.clone(),
    );

    // Run `aimx agent-setup claude-code` as the test user. Feed it a
    // curated `$PATH` containing only the fake claude so the probe
    // resolves deterministically, and a `$HOME` under tmp.
    let agent_setup_out = Command::new("runuser")
        .arg("-u")
        .arg(USER)
        .arg("--")
        .arg(aimx_binary_path())
        .env("AIMX_CONFIG_DIR", &config_dir)
        .env("AIMX_DATA_DIR", &data_dir)
        .env("AIMX_RUNTIME_DIR", &runtime_dir)
        .env("HOME", &user_home)
        .env("PATH", &path_dir)
        .arg("agent-setup")
        .arg("claude-code")
        .arg("--force")
        .output()
        .expect("failed to run aimx agent-setup");
    assert!(
        agent_setup_out.status.success(),
        "agent-setup failed: stdout={:?} stderr={:?}",
        String::from_utf8_lossy(&agent_setup_out.stdout),
        String::from_utf8_lossy(&agent_setup_out.stderr)
    );
    let setup_stdout = String::from_utf8_lossy(&agent_setup_out.stdout);
    let template_name = format!("invoke-claude-{USER}");
    assert!(
        setup_stdout.contains(&template_name),
        "agent-setup stdout should mention {template_name}: {setup_stdout}"
    );

    // Plugin files landed under $HOME/.claude/plugins/aimx.
    let plugin_dir = user_home.join(".claude/plugins/aimx");
    assert!(
        plugin_dir.exists(),
        "plugin dir {} should exist after agent-setup",
        plugin_dir.display()
    );

    // Wire an on_receive hook binding the template to our mailbox.
    // Goes through the daemon UDS (`HOOK-CREATE`); the template is
    // registered and the mailbox is owned by the caller, so the
    // authz check passes.
    let hook_create_out = Command::new("runuser")
        .arg("-u")
        .arg(USER)
        .arg("--")
        .arg(aimx_binary_path())
        .env("AIMX_CONFIG_DIR", &config_dir)
        .env("AIMX_DATA_DIR", &data_dir)
        .env("AIMX_RUNTIME_DIR", &runtime_dir)
        .arg("hooks")
        .arg("create")
        .arg("--mailbox")
        .arg(USER)
        .arg("--event")
        .arg("on_receive")
        .arg("--template")
        .arg(&template_name)
        .arg("--param")
        .arg("prompt=process this")
        .output()
        .expect("failed to run aimx hooks create");
    assert!(
        hook_create_out.status.success(),
        "hooks create failed: stdout={:?} stderr={:?}",
        String::from_utf8_lossy(&hook_create_out.stdout),
        String::from_utf8_lossy(&hook_create_out.stderr)
    );

    // Ingest a trusted `.eml` — trust = verified + allowlist covers
    // *@example.com, so the hook fires.
    let eml = format!(
        "From: sender@example.com\r\n\
         To: {USER}@it.example.com\r\n\
         Subject: hi\r\n\
         Message-ID: <e2e-agent-flow@example.com>\r\n\
         Date: Thu, 01 Jan 2026 12:00:00 +0000\r\n\
         \r\n\
         body\r\n"
    );
    let mut ingest = Command::new(aimx_binary_path())
        .env("AIMX_CONFIG_DIR", &config_dir)
        .env("AIMX_DATA_DIR", &data_dir)
        .env("AIMX_RUNTIME_DIR", &runtime_dir)
        .env("AIMX_SANDBOX_FORCE_FALLBACK", "1")
        .arg("--data-dir")
        .arg(&data_dir)
        .arg("ingest")
        .arg(format!("{USER}@it.example.com"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn aimx ingest");
    ingest
        .stdin
        .as_mut()
        .unwrap()
        .write_all(eml.as_bytes())
        .unwrap();
    let ingest_status = ingest.wait_with_output().expect("ingest wait failed");
    assert!(
        ingest_status.status.success(),
        "ingest failed: stdout={:?} stderr={:?}",
        String::from_utf8_lossy(&ingest_status.stdout),
        String::from_utf8_lossy(&ingest_status.stderr)
    );

    // Wait for the sentinel file to appear (the hook spawns detached).
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline {
        if sentinel.exists() {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(
        sentinel.exists(),
        "hook sentinel never appeared at {}",
        sentinel.display()
    );
    let sentinel_body = std::fs::read_to_string(&sentinel).unwrap();
    assert!(
        sentinel_body.contains(&format!("uid={user_uid}")),
        "hook did not fire under uid {user_uid}: {sentinel_body}"
    );

    // Now run `aimx agent-cleanup claude-code --full --yes` as the
    // test user. Should remove the template via UDS and the plugin
    // dir under $HOME.
    let cleanup_out = Command::new("runuser")
        .arg("-u")
        .arg(USER)
        .arg("--")
        .arg(aimx_binary_path())
        .env("AIMX_CONFIG_DIR", &config_dir)
        .env("AIMX_DATA_DIR", &data_dir)
        .env("AIMX_RUNTIME_DIR", &runtime_dir)
        .env("HOME", &user_home)
        .arg("agent-cleanup")
        .arg("claude-code")
        .arg("--full")
        .arg("--yes")
        .output()
        .expect("failed to run aimx agent-cleanup");
    assert!(
        cleanup_out.status.success(),
        "agent-cleanup failed: stdout={:?} stderr={:?}",
        String::from_utf8_lossy(&cleanup_out.stdout),
        String::from_utf8_lossy(&cleanup_out.stderr)
    );

    assert!(
        !plugin_dir.exists(),
        "plugin dir {} should be gone after --full cleanup",
        plugin_dir.display()
    );

    // Template should be gone from the daemon's in-memory config. Try
    // re-creating a hook against it; the daemon should refuse with
    // `unknown-template`.
    let post_cleanup = Command::new("runuser")
        .arg("-u")
        .arg(USER)
        .arg("--")
        .arg(aimx_binary_path())
        .env("AIMX_CONFIG_DIR", &config_dir)
        .env("AIMX_DATA_DIR", &data_dir)
        .env("AIMX_RUNTIME_DIR", &runtime_dir)
        .arg("hooks")
        .arg("create")
        .arg("--mailbox")
        .arg(USER)
        .arg("--event")
        .arg("on_receive")
        .arg("--template")
        .arg(&template_name)
        .arg("--param")
        .arg("prompt=should fail")
        .arg("--name")
        .arg("post-cleanup-check")
        .output()
        .expect("failed to run hooks create for post-cleanup check");
    assert!(
        !post_cleanup.status.success(),
        "template should be gone after cleanup; hook_create unexpectedly succeeded"
    );

    drop(daemon_handle);
}
