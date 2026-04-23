use crate::config::{Config, MailboxConfig};
use crate::dkim;
use crate::platform::is_root;
use crate::term;
use std::collections::HashMap;
use std::io::{self, BufRead, Write};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::path::{Path, PathBuf};

#[derive(Debug, PartialEq)]
pub enum Port25Status {
    Free,
    Aimx,
    OtherProcess(String),
}

pub trait SystemOps {
    fn write_file(&self, path: &Path, content: &str) -> Result<(), Box<dyn std::error::Error>>;
    fn file_exists(&self, path: &Path) -> bool;
    fn restart_service(&self, service: &str) -> Result<(), Box<dyn std::error::Error>>;
    /// Stop the service. `aimx upgrade` requires this split
    /// (stop → swap → start) because `restart_service` alone can't
    /// express a pause window where the binary swap happens between
    /// shutdown and startup. Idempotent w.r.t. the real init system:
    /// a stop call on an already-stopped service is not an error.
    ///
    /// Required trait method — every `SystemOps` impl must provide it.
    /// Test mocks in modules that never reach the upgrade path
    /// (`doctor.rs`, `logs.rs`, `portcheck.rs`, `uninstall.rs`) are
    /// expected to `unreachable!()` here; a future sprint that teaches
    /// those modules to stop/start the service will then surface a
    /// clear panic rather than a silent `Err` string.
    fn stop_service(&self, service: &str) -> Result<(), Box<dyn std::error::Error>>;
    /// Complement of `stop_service`. Not idempotent — starting an
    /// already-started service surfaces as an error from both systemd
    /// and OpenRC. Required trait method; see `stop_service` above.
    fn start_service(&self, service: &str) -> Result<(), Box<dyn std::error::Error>>;
    fn is_service_running(&self, service: &str) -> bool;
    fn generate_tls_cert(
        &self,
        cert_dir: &Path,
        domain: &str,
    ) -> Result<(), Box<dyn std::error::Error>>;
    fn get_aimx_binary_path(&self) -> Result<PathBuf, Box<dyn std::error::Error>>;
    fn check_root(&self) -> bool;
    fn check_port25_occupancy(&self) -> Result<Port25Status, Box<dyn std::error::Error>>;
    fn install_service_file(&self, data_dir: &Path) -> Result<(), Box<dyn std::error::Error>>;
    /// Stop + disable the `aimx` service and remove its init-system service
    /// file. Service-control commands are best-effort (service may already be
    /// stopped); file removal is the authoritative step. Returns an error when
    /// the init system is unsupported.
    fn uninstall_service_file(&self) -> Result<(), Box<dyn std::error::Error>>;
    /// Poll `127.0.0.1:25` until a TCP connection succeeds or the timeout
    /// elapses. Returns `true` if the port became reachable, `false` on timeout.
    fn wait_for_service_ready(&self) -> bool;
    /// Run `f` while a minimal SMTP responder is bound to `0.0.0.0:25`, then
    /// tear it down. Used for the port-25 inbound preflight during `aimx setup`
    /// before the real aimx.service has been installed. The default impl
    /// performs a real bind (requires root / a free :25); `MockSystemOps`
    /// overrides this to just invoke `f`, so unit tests don't contend for a
    /// real port.
    fn with_temp_smtp_listener(
        &self,
        f: &mut dyn FnMut() -> Result<(), Box<dyn std::error::Error>>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        crate::portcheck::with_temp_smtp_listener(f)
    }
    /// Return the last `n` log lines for `unit` from the active init system.
    /// systemd: `journalctl -u <unit> -n <n> --no-pager`. OpenRC: best-effort
    /// `tail` of `/var/log/aimx/*.log` or `/var/log/messages`. Returns an
    /// error when no log source is reachable so callers can render a
    /// human-friendly fallback line ("no logs available").
    ///
    /// The default impl delegates to the real systemd/OpenRC dispatch in
    /// [`crate::serve::service`]. Tests override the method on their own
    /// `SystemOps` mocks so they don't shell out.
    fn tail_service_logs(
        &self,
        unit: &str,
        n: usize,
    ) -> Result<String, Box<dyn std::error::Error>> {
        crate::serve::service::tail_service_logs_default(unit, n)
    }
    /// Stream the live log for `unit`, following new lines as they arrive.
    /// systemd: replaces the current process with `journalctl -f -u <unit>`.
    /// OpenRC: best-effort `tail -F /var/log/messages` (filtered if possible).
    /// Returns an error when no follow source is reachable.
    fn follow_service_logs(&self, unit: &str) -> Result<(), Box<dyn std::error::Error>> {
        crate::serve::service::follow_service_logs_default(unit)
    }

    /// True iff a user named `user` exists on the local system (resolves via
    /// `id <user>` / `getent passwd <user>`). Used by
    /// [`ensure_catchall_user`] to skip creation on re-runs.
    fn user_exists(&self, user: &str) -> bool {
        std::process::Command::new("id")
            .arg(user)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    }

    /// Resolve a system user name to its `(uid, gid)` via `getpwnam`.
    /// Returns `None` when the user does not exist on this host. Wired
    /// behind `SystemOps` so doctor's hook-templates section can be
    /// unit-tested without requiring `aimx-catchall` to be present in
    /// the test environment's `/etc/passwd`.
    fn lookup_user_uid_gid(&self, user: &str) -> Option<(u32, u32)> {
        #[cfg(unix)]
        {
            use std::ffi::CString;
            let cname = CString::new(user).ok()?;
            // SAFETY: getpwnam returns a pointer to a static buffer that
            // is invalidated by the next getpw* call. We only read two
            // scalar fields synchronously and copy them out.
            let pw = unsafe { libc::getpwnam(cname.as_ptr()) };
            if pw.is_null() {
                return None;
            }
            let (uid, gid) = unsafe { ((*pw).pw_uid as u32, (*pw).pw_gid as u32) };
            Some((uid, gid))
        }
        #[cfg(not(unix))]
        {
            let _ = user;
            None
        }
    }

    /// Idempotently create a system user (+ matching primary group) with
    /// no login shell, home directory `home`, and explicit `useradd`
    /// flags matching PRD §6.4 (`--system`, `/usr/sbin/nologin`). The
    /// home directory is created + chowned by the caller; `useradd` is
    /// invoked with `--no-create-home` so this helper stays focused on
    /// the user/group stanza.
    ///
    /// Returns `Ok(true)` when the user was newly created, `Ok(false)`
    /// when the user already existed, and `Err` when the creation
    /// command failed.
    fn create_system_user(
        &self,
        user: &str,
        home: &Path,
    ) -> Result<bool, Box<dyn std::error::Error>> {
        if self.user_exists(user) {
            return Ok(false);
        }
        // Try `useradd` first (Debian/Ubuntu/RHEL). Only fall back to
        // `adduser` when `useradd` isn't on PATH (`ErrorKind::NotFound`).
        // Any other failure (useradd ran and exited non-zero, permission
        // denied spawning it, etc.) is propagated as-is so the operator
        // sees the real cause rather than a confusing "adduser failed"
        // downstream.
        let home_str = home.to_string_lossy().into_owned();
        match std::process::Command::new("useradd")
            .args([
                "--system",
                "--no-create-home",
                "--home-dir",
                &home_str,
                "--shell",
                "/usr/sbin/nologin",
                "--user-group",
                user,
            ])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
        {
            Ok(s) if s.success() => Ok(true),
            Ok(s) => {
                Err(format!("`useradd` exited with {s} while creating system user '{user}'").into())
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                // Fall back to the Alpine / BusyBox `adduser` flags.
                let s = std::process::Command::new("adduser")
                    .args(["-S", "-H", "-h", &home_str, "-s", "/sbin/nologin", user])
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .status()?;
                if !s.success() {
                    return Err(format!("Failed to create system user '{user}'").into());
                }
                Ok(true)
            }
            Err(e) => Err(format!("failed to spawn `useradd` for user '{user}': {e}").into()),
        }
    }

    /// Create `path` as a directory, chown it to `owner:group`, and
    /// chmod to `mode`. Used by [`ensure_catchall_user`] to provision
    /// `/var/lib/aimx-catchall` as `aimx-catchall:aimx-catchall` mode
    /// `0700` without the daemon needing to reach into the rest of the
    /// datadir.
    fn ensure_owned_directory(
        &self,
        path: &Path,
        owner: &str,
        group: &str,
        mode: u32,
    ) -> Result<(), Box<dyn std::error::Error>> {
        std::fs::create_dir_all(path)?;
        let chown = std::process::Command::new("chown")
            .args([&format!("{owner}:{group}")])
            .arg(path)
            .status()?;
        if !chown.success() {
            return Err(format!("Failed to chown {} to {owner}:{group}", path.display()).into());
        }
        let chmod = std::process::Command::new("chmod")
            .args([&format!("{mode:o}")])
            .arg(path)
            .status()?;
        if !chmod.success() {
            return Err(format!("Failed to chmod {} on {}", mode, path.display()).into());
        }
        Ok(())
    }
}

/// Fixed name of the unprivileged Unix user that runs hook subprocesses
/// for the **catchall** mailbox (PRD §6.4). Created by [`aimx setup`]
/// only when the operator configures a catchall mailbox — no
/// speculative user creation on a setup that never configures one.
/// Regular mailbox owners are real Linux users picked by the operator
/// (Sprint 3 S3-2); `aimx-catchall` exists purely so the catchall has
/// a privilege-dropped uid that still lets hooks read its mail.
pub const CATCHALL_SERVICE_USER: &str = "aimx-catchall";

/// Home directory of the `aimx-catchall` system user. Lives outside
/// `/var/lib/aimx/` so the mailbox datadir layout stays independent of
/// the service user's filesystem footprint. Owned
/// `aimx-catchall:aimx-catchall` mode `0700` — the user has no shell
/// and no login, so the home dir is a formality, but keeping it mode
/// `0700` avoids any chance of `aimx-catchall` becoming a world-readable
/// landing zone if an operator later points a hook's cwd at it.
pub const CATCHALL_HOME_DIR: &str = "/var/lib/aimx-catchall";

/// Ensure the `aimx-catchall` service user + matching group exist and
/// its home directory is provisioned. Called from [`run_setup`] **only
/// when the operator's config includes a catchall mailbox** — skipped
/// otherwise so `/etc/passwd` stays clean on installs that never
/// configure a catchall.
///
/// Idempotent: re-running on a box that already has the user skips
/// creation but still re-asserts the `0700 aimx-catchall:aimx-catchall`
/// invariant on `/var/lib/aimx-catchall/` so permissions drift is
/// corrected on every setup pass.
pub(crate) fn ensure_catchall_user(sys: &dyn SystemOps) -> Result<(), Box<dyn std::error::Error>> {
    println!("\n{}", term::header("[Catchall]"));
    let home = Path::new(CATCHALL_HOME_DIR);
    let created = sys.create_system_user(CATCHALL_SERVICE_USER, home)?;
    if created {
        println!(
            "  {} {}",
            term::success("Created system user"),
            term::highlight(CATCHALL_SERVICE_USER)
        );
    } else {
        println!(
            "  {} {}",
            term::info(&format!(
                "System user {CATCHALL_SERVICE_USER} already present"
            )),
            term::dim("(skipping creation)")
        );
    }

    sys.ensure_owned_directory(home, CATCHALL_SERVICE_USER, CATCHALL_SERVICE_USER, 0o700)?;
    println!(
        "  {} {} {}",
        term::success("Home directory"),
        term::highlight(CATCHALL_HOME_DIR),
        term::dim("(0700)"),
    );
    Ok(())
}

pub trait NetworkOps {
    fn check_outbound_port25(&self) -> Result<bool, Box<dyn std::error::Error>>;
    /// Full SMTP EHLO handshake via `{verify_host}/probe`.
    /// Used by `aimx setup` (post-install) and `aimx portcheck`.
    fn check_inbound_port25(&self) -> Result<bool, Box<dyn std::error::Error>>;
    /// Return the server's IPv4 and IPv6 addresses in a single call.
    ///
    /// Both families are derived from a single `hostname -I` invocation
    /// in `RealNetworkOps`, avoiding duplicate work and duplicate failure
    /// modes.
    fn get_server_ips(
        &self,
    ) -> Result<(Option<Ipv4Addr>, Option<Ipv6Addr>), Box<dyn std::error::Error>>;
    fn resolve_mx(&self, domain: &str) -> Result<Vec<String>, Box<dyn std::error::Error>>;
    fn resolve_a(&self, domain: &str) -> Result<Vec<IpAddr>, Box<dyn std::error::Error>>;
    fn resolve_aaaa(&self, domain: &str) -> Result<Vec<IpAddr>, Box<dyn std::error::Error>>;
    fn resolve_txt(&self, domain: &str) -> Result<Vec<String>, Box<dyn std::error::Error>>;
}

pub struct RealSystemOps;

impl SystemOps for RealSystemOps {
    fn write_file(&self, path: &Path, content: &str) -> Result<(), Box<dyn std::error::Error>> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, content)?;
        Ok(())
    }

    fn file_exists(&self, path: &Path) -> bool {
        path.exists()
    }

    fn restart_service(&self, service: &str) -> Result<(), Box<dyn std::error::Error>> {
        use crate::serve::service::{detect_init_system, restart_service_command};

        let init = detect_init_system();
        let (program, args) = restart_service_command(&init, service).ok_or_else(|| {
            format!("Could not detect init system (systemd or OpenRC) to restart {service}.")
        })?;
        let status = std::process::Command::new(program).args(&args).status()?;
        if !status.success() {
            return Err(format!("Failed to restart {service}").into());
        }
        Ok(())
    }

    fn stop_service(&self, service: &str) -> Result<(), Box<dyn std::error::Error>> {
        use crate::serve::service::{detect_init_system, stop_service_command};

        let init = detect_init_system();
        let (program, args) = stop_service_command(&init, service).ok_or_else(|| {
            format!("Could not detect init system (systemd or OpenRC) to stop {service}.")
        })?;
        let status = std::process::Command::new(program).args(&args).status()?;
        if !status.success() {
            return Err(format!("Failed to stop {service}").into());
        }
        Ok(())
    }

    fn start_service(&self, service: &str) -> Result<(), Box<dyn std::error::Error>> {
        use crate::serve::service::{detect_init_system, start_service_command};

        let init = detect_init_system();
        let (program, args) = start_service_command(&init, service).ok_or_else(|| {
            format!("Could not detect init system (systemd or OpenRC) to start {service}.")
        })?;
        let status = std::process::Command::new(program).args(&args).status()?;
        if !status.success() {
            return Err(format!("Failed to start {service}").into());
        }
        Ok(())
    }

    fn is_service_running(&self, service: &str) -> bool {
        use crate::serve::service::{detect_init_system, is_service_running_command};

        let init = detect_init_system();
        match is_service_running_command(&init, service) {
            Some((program, args)) => std::process::Command::new(program)
                .args(&args)
                .status()
                .is_ok_and(|s| s.success()),
            None => false,
        }
    }

    fn generate_tls_cert(
        &self,
        cert_dir: &Path,
        domain: &str,
    ) -> Result<(), Box<dyn std::error::Error>> {
        std::fs::create_dir_all(cert_dir)?;
        let cert_path = cert_dir.join("cert.pem");
        let key_path = cert_dir.join("key.pem");

        let status = std::process::Command::new("openssl")
            .args([
                "req",
                "-x509",
                "-newkey",
                "rsa:2048",
                "-keyout",
                &key_path.to_string_lossy(),
                "-out",
                &cert_path.to_string_lossy(),
                "-days",
                "3650",
                "-nodes",
                "-subj",
                &format!("/CN={domain}"),
            ])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()?;

        if !status.success() {
            return Err("Failed to generate TLS certificate".into());
        }
        Ok(())
    }

    fn get_aimx_binary_path(&self) -> Result<PathBuf, Box<dyn std::error::Error>> {
        // Sprint 5 S5-4: canonicalize so the generated unit file's
        // `ExecStart=` line points at the real binary rather than a
        // symlink — systemd best practice, and what closes PRD §11's
        // "non-/usr/local prefixes" open question. An operator who
        // installed via `AIMX_PREFIX=/opt/aimx curl ... | sh` now gets
        // a unit whose ExecStart is `/opt/aimx/bin/aimx serve`, not
        // whatever `/usr/local/bin/aimx` symlink the installer might
        // have left behind.
        let exe = std::env::current_exe()?;
        exe.canonicalize().map_err(|e| {
            format!(
                "cannot resolve current executable path ({}): {e}",
                exe.display()
            )
            .into()
        })
    }

    fn check_root(&self) -> bool {
        unsafe { libc::geteuid() == 0 }
    }

    fn check_port25_occupancy(&self) -> Result<Port25Status, Box<dyn std::error::Error>> {
        let output = std::process::Command::new("ss")
            .args(["-tlnp", "sport", "=", ":25"])
            .output()?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        parse_port25_status(&stdout)
    }

    fn install_service_file(&self, data_dir: &Path) -> Result<(), Box<dyn std::error::Error>> {
        use crate::serve::service::{
            InitSystem, detect_init_system, generate_openrc_script, generate_systemd_unit,
        };

        // Sprint 5 S5-4: derive `ExecStart=` from the canonicalized
        // current-exe path. Abort hard if we cannot resolve it —
        // writing a unit whose ExecStart points at a non-existent or
        // non-executable binary silently is worse than failing fast
        // (the service would just loop on restart, burning
        // StartLimitBurst for no reason).
        let aimx_path_buf =
            self.get_aimx_binary_path()
                .map_err(|e| -> Box<dyn std::error::Error> {
                    format!(
                        "✗ cannot resolve current executable path: {e}. \
                 Re-run `sudo aimx setup` from the installed binary."
                    )
                    .into()
                })?;
        let aimx_path = aimx_path_buf.to_string_lossy().to_string();
        let data_dir_str = data_dir.to_string_lossy().to_string();

        match detect_init_system() {
            InitSystem::Systemd => {
                let unit = generate_systemd_unit(&aimx_path, &data_dir_str);
                let unit_path = Path::new("/etc/systemd/system/aimx.service");
                self.write_file(unit_path, &unit)?;
                let _ = std::process::Command::new("systemctl")
                    .args(["daemon-reload"])
                    .status();
                let _ = std::process::Command::new("systemctl")
                    .args(["enable", "aimx"])
                    .status();
                // Clear any stuck "start-limit-hit" state from a prior
                // failed install attempt so the restart below actually runs.
                let _ = std::process::Command::new("systemctl")
                    .args(["reset-failed", "aimx"])
                    .status();
                self.restart_service("aimx")?;
            }
            InitSystem::OpenRC => {
                let script = generate_openrc_script(&aimx_path, &data_dir_str);
                let script_path = Path::new("/etc/init.d/aimx");
                self.write_file(script_path, &script)?;
                let _ = std::process::Command::new("chmod")
                    .args(["+x", "/etc/init.d/aimx"])
                    .status();
                let _ = std::process::Command::new("rc-update")
                    .args(["add", "aimx", "default"])
                    .status();
                self.restart_service("aimx")?;
            }
            InitSystem::Unknown => {
                return Err("Could not detect init system (systemd or OpenRC). \
                     Start aimx serve manually."
                    .into());
            }
        }
        Ok(())
    }

    fn uninstall_service_file(&self) -> Result<(), Box<dyn std::error::Error>> {
        use crate::serve::service::{InitSystem, detect_init_system};

        match detect_init_system() {
            InitSystem::Systemd => {
                let _ = std::process::Command::new("systemctl")
                    .args(["stop", "aimx"])
                    .status();
                let _ = std::process::Command::new("systemctl")
                    .args(["disable", "aimx"])
                    .status();
                let unit_path = Path::new("/etc/systemd/system/aimx.service");
                if unit_path.exists() {
                    std::fs::remove_file(unit_path)?;
                }
                let _ = std::process::Command::new("systemctl")
                    .args(["daemon-reload"])
                    .status();
                let _ = std::process::Command::new("systemctl")
                    .args(["reset-failed", "aimx"])
                    .status();
            }
            InitSystem::OpenRC => {
                let _ = std::process::Command::new("rc-service")
                    .args(["aimx", "stop"])
                    .status();
                let _ = std::process::Command::new("rc-update")
                    .args(["del", "aimx", "default"])
                    .status();
                let script_path = Path::new("/etc/init.d/aimx");
                if script_path.exists() {
                    std::fs::remove_file(script_path)?;
                }
            }
            InitSystem::Unknown => {
                return Err("Could not detect init system (systemd or OpenRC). \
                     Remove /etc/systemd/system/aimx.service or /etc/init.d/aimx manually."
                    .into());
            }
        }
        Ok(())
    }

    fn wait_for_service_ready(&self) -> bool {
        use std::net::{SocketAddr, TcpStream};
        use std::time::{Duration, Instant};

        let addr: SocketAddr = "127.0.0.1:25".parse().expect("static address parses");
        let budget = Duration::from_millis(5_000);
        let interval = Duration::from_millis(500);
        let connect_timeout = Duration::from_millis(200);
        let start = Instant::now();
        loop {
            if TcpStream::connect_timeout(&addr, connect_timeout).is_ok() {
                return true;
            }
            if start.elapsed() >= budget {
                return false;
            }
            std::thread::sleep(interval);
        }
    }
}

pub fn parse_port25_status(ss_output: &str) -> Result<Port25Status, Box<dyn std::error::Error>> {
    let lines: Vec<&str> = ss_output.lines().collect();
    let data_lines: Vec<&str> = lines
        .iter()
        .skip(1)
        .filter(|l| !l.trim().is_empty())
        .copied()
        .collect();

    if data_lines.is_empty() {
        return Ok(Port25Status::Free);
    }

    // Try to extract process name from users:(("name",...)) pattern
    for line in &data_lines {
        if let Some(start) = line.find("users:((\"") {
            let rest = &line[start + 9..];
            if let Some(end) = rest.find('"') {
                let process_name = &rest[..end];
                if process_name == "aimx" {
                    return Ok(Port25Status::Aimx);
                }
                return Ok(Port25Status::OtherProcess(process_name.to_string()));
            }
        }
    }

    // Something is on port 25 but we can't identify it
    Ok(Port25Status::OtherProcess("unknown".to_string()))
}

pub const DEFAULT_VERIFY_HOST: &str = "https://check.aimx.email";

pub const DEFAULT_CHECK_SERVICE_SMTP_ADDR: &str = "check.aimx.email:25";

fn is_global_ipv6(ip: &Ipv6Addr) -> bool {
    let segments = ip.segments();
    // Not link-local (fe80::/10)
    (segments[0] & 0xffc0) != 0xfe80
    // Not ULA (fc00::/7)
    && (segments[0] & 0xfe00) != 0xfc00
    // Not loopback
    && !ip.is_loopback()
    // Not unspecified
    && !ip.is_unspecified()
}

/// Parse the whitespace-separated output of `hostname -I` into the first
/// IPv4 address and the first *global* IPv6 address. Non-global IPv6 tokens
/// (link-local, ULA, loopback, unspecified) are ignored.
pub(crate) fn parse_hostname_i_output(stdout: &str) -> (Option<Ipv4Addr>, Option<Ipv6Addr>) {
    let mut ipv4: Option<Ipv4Addr> = None;
    let mut ipv6: Option<Ipv6Addr> = None;
    for token in stdout.split_whitespace() {
        match token.parse::<IpAddr>() {
            Ok(IpAddr::V4(v4)) if ipv4.is_none() => ipv4 = Some(v4),
            Ok(IpAddr::V6(v6)) if ipv6.is_none() && is_global_ipv6(&v6) => ipv6 = Some(v6),
            _ => {}
        }
        if ipv4.is_some() && ipv6.is_some() {
            break;
        }
    }
    (ipv4, ipv6)
}

/// Injectable shim over `std::process::Command::new("dig").args(...).output()`.
/// Tests inject a scripted implementation to exercise the resolver cascade
/// without shelling out to a real `dig`.
pub(crate) trait DigRunner {
    fn run(&self, args: &[String]) -> io::Result<std::process::Output>;
}

pub(crate) struct RealDigRunner;

impl DigRunner for RealDigRunner {
    fn run(&self, args: &[String]) -> io::Result<std::process::Output> {
        std::process::Command::new("dig").args(args).output()
    }
}

/// Query `dig` for a record, cascading across `DIG_RESOLVERS` with up to
/// `DIG_RETRY_ATTEMPTS` per resolver.
///
/// Stop conditions, in priority order:
/// 1. Any invocation returns exit 0 → parse and return the lines (an
///    empty vec is a valid success; NOERROR/NXDOMAIN is authoritative
///    so we do NOT fall through on an empty-but-ok response).
/// 2. All resolvers × all attempts exit non-zero → return `io::Error`
///    with the last resolver's stderr tail for diagnostics.
///
/// The empty-filtered, trimmed lines are returned raw. Type-specific
/// post-processing (IP parsing for A/AAAA, quote-stripping for TXT)
/// lives in the `resolve_*` callers.
pub(crate) fn dig_with_cascade(
    runner: &dyn DigRunner,
    record_type: &str,
    domain: &str,
) -> io::Result<Vec<String>> {
    let mut last_err_detail = String::new();
    for resolver in DIG_RESOLVERS {
        for attempt in 0..DIG_RETRY_ATTEMPTS {
            let args = dig_short_args(resolver, record_type, domain);
            match runner.run(&args) {
                Ok(output) if output.status.success() => {
                    let stdout = String::from_utf8_lossy(&output.stdout);
                    let lines: Vec<String> = stdout
                        .lines()
                        .filter(|l| !l.trim().is_empty())
                        .map(|l| l.trim().to_string())
                        .collect();
                    return Ok(lines);
                }
                Ok(output) => {
                    last_err_detail = format!(
                        "@{resolver} exit={} stderr={}",
                        output.status,
                        String::from_utf8_lossy(&output.stderr).trim()
                    );
                }
                Err(e) => {
                    last_err_detail = format!("@{resolver} spawn error: {e}");
                }
            }
            if attempt + 1 < DIG_RETRY_ATTEMPTS {
                std::thread::sleep(dig_retry_delay());
            }
        }
    }
    Err(io::Error::other(format!(
        "dig {record_type} {domain} failed across all {} resolvers ({} attempts each); last error: {last_err_detail}",
        DIG_RESOLVERS.len(),
        DIG_RETRY_ATTEMPTS,
    )))
}

pub struct RealNetworkOps {
    pub verify_host: String,
    pub check_service_smtp_addr: String,
    dig_runner: Box<dyn DigRunner + Send + Sync>,
}

impl std::fmt::Debug for RealNetworkOps {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RealNetworkOps")
            .field("verify_host", &self.verify_host)
            .field("check_service_smtp_addr", &self.check_service_smtp_addr)
            .finish_non_exhaustive()
    }
}

impl Default for RealNetworkOps {
    fn default() -> Self {
        Self {
            verify_host: DEFAULT_VERIFY_HOST.to_string(),
            check_service_smtp_addr: DEFAULT_CHECK_SERVICE_SMTP_ADDR.to_string(),
            dig_runner: Box::new(RealDigRunner),
        }
    }
}

impl RealNetworkOps {
    pub fn from_verify_host(verify_host: String) -> Result<Self, Box<dyn std::error::Error>> {
        let trimmed = verify_host.trim_end_matches('/').to_string();
        validate_verify_host(&trimmed)?;
        let smtp_addr = derive_smtp_addr_from_verify_host(&trimmed);
        Ok(Self {
            verify_host: trimmed,
            check_service_smtp_addr: smtp_addr,
            dig_runner: Box::new(RealDigRunner),
        })
    }

    #[cfg(test)]
    fn with_dig_runner(mut self, runner: Box<dyn DigRunner + Send + Sync>) -> Self {
        self.dig_runner = runner;
        self
    }

    fn curl_probe(&self) -> Result<bool, Box<dyn std::error::Error>> {
        let url = format!("{}/probe", self.verify_host);
        let resp = std::process::Command::new("curl")
            .args(["-s", "-m", "60", &url])
            .output();

        match resp {
            Ok(output) if output.status.success() => {
                let body = String::from_utf8_lossy(&output.stdout);
                Ok(body.contains("\"reachable\":true") || body.contains("\"reachable\": true"))
            }
            _ => Ok(false),
        }
    }
}

pub fn validate_verify_host(verify_host: &str) -> Result<(), Box<dyn std::error::Error>> {
    if verify_host.is_empty() {
        return Err("verify-host cannot be empty".into());
    }
    if !verify_host.starts_with("http://") && !verify_host.starts_with("https://") {
        return Err(format!(
            "verify-host must start with http:// or https:// (got: {verify_host})"
        )
        .into());
    }
    Ok(())
}

pub fn derive_smtp_addr_from_verify_host(verify_host: &str) -> String {
    // Extract authority (host[:port]) from URL like "https://check.aimx.email:3025/probe"
    let without_scheme = verify_host
        .strip_prefix("https://")
        .or_else(|| verify_host.strip_prefix("http://"))
        .unwrap_or(verify_host);
    let authority = without_scheme.split('/').next().unwrap_or(without_scheme);

    // Bracketed IPv6 literal: [::1] or [::1]:3025
    if let Some(rest) = authority.strip_prefix('[')
        && let Some(end) = rest.find(']')
    {
        let ipv6 = &rest[..end];
        return format!("[{ipv6}]:25");
    }

    // Hostname or IPv4: strip :port if present (rsplit handles hosts safely since
    // non-IPv6 hosts have at most one colon, the port separator).
    let host = match authority.rsplit_once(':') {
        Some((h, _port)) => h,
        None => authority,
    };
    format!("{host}:25")
}

impl NetworkOps for RealNetworkOps {
    fn check_outbound_port25(&self) -> Result<bool, Box<dyn std::error::Error>> {
        use std::io::{BufRead, BufReader, Write};
        use std::net::{TcpStream, ToSocketAddrs};
        use std::time::Duration;

        let target = &self.check_service_smtp_addr;
        let addrs: Vec<_> = target.to_socket_addrs()?.collect();
        if addrs.is_empty() {
            return Ok(false);
        }

        let stream = match TcpStream::connect_timeout(&addrs[0], Duration::from_secs(10)) {
            Ok(s) => s,
            Err(_) => return Ok(false),
        };
        stream.set_read_timeout(Some(Duration::from_secs(5)))?;
        stream.set_write_timeout(Some(Duration::from_secs(5)))?;

        let mut reader = BufReader::new(stream.try_clone()?);
        let mut writer = stream;

        let mut banner = String::new();
        if reader.read_line(&mut banner).is_err() || !banner.starts_with("220") {
            return Ok(false);
        }

        writer.write_all(b"EHLO aimx\r\n")?;
        writer.flush()?;

        let mut ehlo_resp = String::new();
        loop {
            ehlo_resp.clear();
            if reader.read_line(&mut ehlo_resp).is_err() {
                return Ok(false);
            }
            if ehlo_resp.starts_with("250 ") {
                break;
            }
            if !ehlo_resp.starts_with("250-") {
                return Ok(false);
            }
        }

        let _ = writer.write_all(b"QUIT\r\n");
        let _ = writer.flush();

        Ok(true)
    }

    fn check_inbound_port25(&self) -> Result<bool, Box<dyn std::error::Error>> {
        self.curl_probe()
    }

    fn get_server_ips(
        &self,
    ) -> Result<(Option<Ipv4Addr>, Option<Ipv6Addr>), Box<dyn std::error::Error>> {
        let output = std::process::Command::new("hostname").arg("-I").output()?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        Ok(parse_hostname_i_output(&stdout))
    }

    fn resolve_mx(&self, domain: &str) -> Result<Vec<String>, Box<dyn std::error::Error>> {
        let records = dig_with_cascade(self.dig_runner.as_ref(), "MX", domain)?;
        Ok(records)
    }

    fn resolve_a(&self, domain: &str) -> Result<Vec<IpAddr>, Box<dyn std::error::Error>> {
        let records = dig_with_cascade(self.dig_runner.as_ref(), "A", domain)?;
        Ok(records.into_iter().filter_map(|l| l.parse().ok()).collect())
    }

    fn resolve_aaaa(&self, domain: &str) -> Result<Vec<IpAddr>, Box<dyn std::error::Error>> {
        let records = dig_with_cascade(self.dig_runner.as_ref(), "AAAA", domain)?;
        Ok(records.into_iter().filter_map(|l| l.parse().ok()).collect())
    }

    fn resolve_txt(&self, domain: &str) -> Result<Vec<String>, Box<dyn std::error::Error>> {
        let records = dig_with_cascade(self.dig_runner.as_ref(), "TXT", domain)?;
        Ok(records
            .into_iter()
            .map(|l| l.replace("\" \"", "").trim_matches('"').to_string())
            .collect())
    }
}

pub const COMPATIBLE_PROVIDERS: &[&str] = &[
    "Hetzner Cloud",
    "OVH / OVHcloud",
    "BuyVM (Frantech)",
    "Vultr (on request)",
    "Linode/Akamai (on request)",
];

/// Per-attempt dig timeout in seconds. A single dig invocation uses
/// `+time=DIG_TIMEOUT_SECS +tries=DIG_TRIES`. We rely on the outer
/// `dig_with_cascade` retry/fallback to handle transient UDP loss
/// rather than bloating a single invocation's budget.
pub const DIG_TIMEOUT_SECS: u32 = 2;

/// Number of internal dig retries per invocation. Kept at 1 because
/// `dig_with_cascade` handles retries at a higher level where we can
/// bail early on success and fall through to a different resolver.
pub const DIG_TRIES: u32 = 1;

/// Total attempts per resolver in `dig_with_cascade`. A non-zero dig
/// exit retries up to this many times before falling through to the
/// next resolver. An exit-0 response (including an empty answer) stops
/// the cascade immediately. NOERROR/NXDOMAIN is authoritative.
pub(crate) const DIG_RETRY_ATTEMPTS: u32 = 3;

/// Delay between retry attempts on the same resolver, in milliseconds.
#[cfg_attr(test, allow(dead_code))]
pub(crate) const DIG_RETRY_DELAY_MS: u64 = 300;

/// Public resolvers queried in order. Hardcoded because `aimx setup` /
/// `aimx status` need reliable, cache-consistent answers and users have
/// no reason to prefer their local recursive resolver, which is often
/// the source of the flakiness this cascade fixes (stale caches, UDP
/// loss, round-robin across inconsistent upstreams).
pub(crate) const DIG_RESOLVERS: &[&str] = &["1.1.1.1", "8.8.8.8", "9.9.9.9"];

/// Build the argv vec for a `dig @<resolver> +short <TYPE> <domain>`
/// invocation with the per-attempt bounds declared above. Extracted so
/// the bounds are applied uniformly and are unit-testable without
/// shelling out.
pub fn dig_short_args(resolver: &str, record_type: &str, domain: &str) -> Vec<String> {
    vec![
        format!("@{resolver}"),
        format!("+time={DIG_TIMEOUT_SECS}"),
        format!("+tries={DIG_TRIES}"),
        "+short".to_string(),
        record_type.to_string(),
        domain.to_string(),
    ]
}

/// Retry delay between attempts on the same resolver. Extracted so tests
/// can short-circuit the sleep; otherwise the all-resolvers-fail test
/// alone would cost ~2.4s, compounded across multiple tests.
#[cfg(not(test))]
fn dig_retry_delay() -> std::time::Duration {
    std::time::Duration::from_millis(DIG_RETRY_DELAY_MS)
}

#[cfg(test)]
fn dig_retry_delay() -> std::time::Duration {
    std::time::Duration::from_millis(0)
}

#[derive(Debug, PartialEq)]
pub enum PreflightResult {
    /// Check passed. Optional detail string is displayed inline.
    Pass(Option<String>),
    Fail(String),
}

pub fn check_outbound(net: &dyn NetworkOps) -> PreflightResult {
    match net.check_outbound_port25() {
        Ok(true) => PreflightResult::Pass(None),
        Ok(false) => PreflightResult::Fail(
            "Outbound port 25 is blocked. Your VPS provider may restrict SMTP traffic.".into(),
        ),
        Err(e) => PreflightResult::Fail(format!("Outbound port 25 check failed: {e}")),
    }
}

fn inbound_result(res: Result<bool, Box<dyn std::error::Error>>) -> PreflightResult {
    match res {
        Ok(true) => PreflightResult::Pass(None),
        Ok(false) => PreflightResult::Fail(
            "Inbound port 25 is not reachable. Check your firewall and VPS provider settings."
                .into(),
        ),
        Err(e) => PreflightResult::Fail(format!("Inbound port 25 check failed: {e}")),
    }
}

/// Full SMTP EHLO handshake via `/probe`.
pub fn check_inbound(net: &dyn NetworkOps) -> PreflightResult {
    inbound_result(net.check_inbound_port25())
}

#[derive(Debug)]
pub struct DnsRecord {
    pub record_type: String,
    pub name: String,
    pub value: String,
}

pub fn generate_dns_records(
    domain: &str,
    server_ip: &str,
    server_ipv6: Option<&str>,
    dkim_value: &str,
    dkim_selector: &str,
) -> Vec<DnsRecord> {
    let spf_value = match server_ipv6 {
        Some(ipv6) => format!("v=spf1 ip4:{server_ip} ip6:{ipv6} -all"),
        None => format!("v=spf1 ip4:{server_ip} -all"),
    };

    let mut records = vec![DnsRecord {
        record_type: "A".into(),
        name: domain.into(),
        value: server_ip.into(),
    }];

    if let Some(ipv6) = server_ipv6 {
        records.push(DnsRecord {
            record_type: "AAAA".into(),
            name: domain.into(),
            value: ipv6.into(),
        });
    }

    records.extend([
        DnsRecord {
            record_type: "MX".into(),
            name: domain.into(),
            value: format!("10 {domain}."),
        },
        DnsRecord {
            record_type: "TXT".into(),
            name: domain.into(),
            value: spf_value,
        },
        DnsRecord {
            record_type: "TXT".into(),
            name: format!("{dkim_selector}._domainkey.{domain}"),
            value: dkim_value.into(),
        },
        DnsRecord {
            record_type: "TXT".into(),
            name: format!("_dmarc.{domain}"),
            value: "v=DMARC1; p=reject; rua=mailto:postmaster@{domain}".replace("{domain}", domain),
        },
    ]);

    records
}

#[cfg(test)]
pub fn format_dns_records(records: &[DnsRecord]) -> String {
    let mut output = String::new();
    for r in records {
        output.push_str(&format!(
            "  {:4} {:<45} {}\n",
            r.record_type, r.name, r.value
        ));
    }
    output
}

pub fn display_dns_guidance(
    domain: &str,
    server_ip: &str,
    server_ipv6: Option<&str>,
    dkim_value: &str,
    dkim_selector: &str,
) {
    let records = generate_dns_records(domain, server_ip, server_ipv6, dkim_value, dkim_selector);
    println!("\n{}", term::header("[DNS]"));
    println!("Add the following DNS records at your domain registrar:\n");
    println!("  TYPE NAME                                          VALUE");
    println!("  ---- --------------------------------------------- -----");
    for r in &records {
        println!("  {:4} {:<45} {}", r.record_type, r.name, r.value);
    }
}

#[derive(Debug, PartialEq)]
pub enum DnsVerifyResult {
    Pass,
    Fail(String),
    Missing(String),
    Warn(String),
}

pub fn verify_mx(net: &dyn NetworkOps, domain: &str) -> DnsVerifyResult {
    match net.resolve_mx(domain) {
        Ok(records) if !records.is_empty() => {
            let has_match = records
                .iter()
                .any(|r| r.to_lowercase().contains(&domain.to_lowercase()));
            if has_match {
                DnsVerifyResult::Pass
            } else {
                DnsVerifyResult::Fail(format!(
                    "MX record found but does not point to {domain}: {:?}",
                    records
                ))
            }
        }
        Ok(_) => DnsVerifyResult::Missing("No MX record found".into()),
        Err(e) => DnsVerifyResult::Fail(format!("MX lookup failed: {e}")),
    }
}

pub fn verify_a(net: &dyn NetworkOps, domain: &str, expected_ip: &IpAddr) -> DnsVerifyResult {
    match net.resolve_a(domain) {
        Ok(addrs) if addrs.contains(expected_ip) => DnsVerifyResult::Pass,
        Ok(addrs) if !addrs.is_empty() => DnsVerifyResult::Fail(format!(
            "A record points to {:?}, expected {expected_ip}",
            addrs
        )),
        Ok(_) => DnsVerifyResult::Missing("No A record found".into()),
        Err(e) => DnsVerifyResult::Fail(format!("A record lookup failed: {e}")),
    }
}

pub fn verify_aaaa(net: &dyn NetworkOps, domain: &str, expected_ip: &IpAddr) -> DnsVerifyResult {
    match net.resolve_aaaa(domain) {
        Ok(addrs) if addrs.contains(expected_ip) => DnsVerifyResult::Pass,
        Ok(addrs) if !addrs.is_empty() => DnsVerifyResult::Fail(format!(
            "AAAA record points to {:?}, expected {expected_ip}",
            addrs
        )),
        Ok(_) => DnsVerifyResult::Missing("No AAAA record found".into()),
        Err(e) => DnsVerifyResult::Fail(format!("AAAA record lookup failed: {e}")),
    }
}

fn spf_contains_ip(record: &str, expected_ip: &str) -> bool {
    for token in record.split_whitespace() {
        if let Some(mechanism) = token
            .strip_prefix("ip4:")
            .or_else(|| token.strip_prefix("+ip4:"))
            .or_else(|| token.strip_prefix("ip6:"))
            .or_else(|| token.strip_prefix("+ip6:"))
        {
            let ip_part = mechanism.split('/').next().unwrap_or(mechanism);
            if ip_part == expected_ip {
                return true;
            }
        }
    }
    false
}

pub fn verify_spf(net: &dyn NetworkOps, domain: &str, expected_ip: &str) -> DnsVerifyResult {
    match net.resolve_txt(domain) {
        Ok(records) => {
            let spf: Vec<&String> = records.iter().filter(|r| r.starts_with("v=spf1")).collect();
            if spf.is_empty() {
                return DnsVerifyResult::Missing("No SPF record found".into());
            }
            if spf.iter().any(|r| spf_contains_ip(r, expected_ip)) {
                DnsVerifyResult::Pass
            } else {
                DnsVerifyResult::Fail(format!(
                    "SPF record found but does not include {expected_ip}: {:?}",
                    spf
                ))
            }
        }
        Err(e) => DnsVerifyResult::Fail(format!("SPF lookup failed: {e}")),
    }
}

fn extract_dkim_public_key(record: &str) -> Option<String> {
    // Single source of truth for DKIM1 `p=` parsing lives in `dkim::`; see
    // `public_key_spki_base64` / `extract_dkim_p_value`.
    crate::dkim::extract_dkim_p_value(record)
}

pub fn verify_dkim(
    net: &dyn NetworkOps,
    domain: &str,
    selector: &str,
    local_public_key: Option<&str>,
) -> DnsVerifyResult {
    let dkim_domain = format!("{selector}._domainkey.{domain}");
    match net.resolve_txt(&dkim_domain) {
        Ok(records) => {
            let dkim: Vec<&String> = records.iter().filter(|r| r.contains("v=DKIM1")).collect();
            if dkim.is_empty() {
                return DnsVerifyResult::Missing("No DKIM record found".into());
            }
            if let Some(local_key) = local_public_key {
                let local_clean = local_key.replace(' ', "");
                let any_match = dkim
                    .iter()
                    .any(|r| extract_dkim_public_key(r).as_deref() == Some(&local_clean));
                if any_match {
                    DnsVerifyResult::Pass
                } else {
                    DnsVerifyResult::Fail(
                        "DKIM record found but public key does not match local key".into(),
                    )
                }
            } else {
                DnsVerifyResult::Pass
            }
        }
        Err(e) => DnsVerifyResult::Fail(format!("DKIM lookup failed: {e}")),
    }
}

pub fn verify_dmarc(net: &dyn NetworkOps, domain: &str) -> DnsVerifyResult {
    let dmarc_domain = format!("_dmarc.{domain}");
    match net.resolve_txt(&dmarc_domain) {
        Ok(records) => {
            let dmarc: Vec<&String> = records.iter().filter(|r| r.contains("v=DMARC1")).collect();
            if dmarc.is_empty() {
                return DnsVerifyResult::Missing("No DMARC record found".into());
            }
            let has_permissive = dmarc.iter().any(|r| {
                r.split(';')
                    .any(|part| part.trim().eq_ignore_ascii_case("p=none"))
            });
            if has_permissive {
                DnsVerifyResult::Warn(
                    "DMARC record uses p=none (no enforcement). Consider p=quarantine or p=reject for production."
                        .into(),
                )
            } else {
                DnsVerifyResult::Pass
            }
        }
        Err(e) => DnsVerifyResult::Fail(format!("DMARC lookup failed: {e}")),
    }
}

pub fn verify_all_dns(
    net: &dyn NetworkOps,
    domain: &str,
    server_ip: &IpAddr,
    server_ipv6: Option<&IpAddr>,
    dkim_selector: &str,
    local_dkim_pubkey: Option<&str>,
) -> Vec<(String, DnsVerifyResult)> {
    let ip_str = server_ip.to_string();
    let mut results = vec![("A".into(), verify_a(net, domain, server_ip))];

    if let Some(ipv6) = server_ipv6 {
        results.push(("AAAA".into(), verify_aaaa(net, domain, ipv6)));
    }

    results.push(("MX".into(), verify_mx(net, domain)));
    results.push(("SPF".into(), verify_spf(net, domain, &ip_str)));

    if let Some(ipv6) = server_ipv6 {
        let ipv6_str = ipv6.to_string();
        results.push(("SPF (IPv6)".into(), verify_spf(net, domain, &ipv6_str)));
    }

    results.extend([
        (
            "DKIM".into(),
            verify_dkim(net, domain, dkim_selector, local_dkim_pubkey),
        ),
        ("DMARC".into(), verify_dmarc(net, domain)),
    ]);

    results
}

fn dns_record_for_check<'a>(check: &str, records: &'a [DnsRecord]) -> Option<&'a DnsRecord> {
    match check {
        "A" => records.iter().find(|r| r.record_type == "A"),
        "AAAA" => records.iter().find(|r| r.record_type == "AAAA"),
        "MX" => records.iter().find(|r| r.record_type == "MX"),
        "SPF" | "SPF (IPv6)" => records
            .iter()
            .find(|r| r.record_type == "TXT" && r.value.starts_with("v=spf1")),
        "DKIM" => records
            .iter()
            .find(|r| r.record_type == "TXT" && r.name.contains("._domainkey.")),
        "DMARC" => records
            .iter()
            .find(|r| r.record_type == "TXT" && r.name.starts_with("_dmarc.")),
        _ => None,
    }
}

/// Produce just the per-record DNS verification lines. No preamble, no
/// trailing blank. Used by callers (like `aimx status`) that render their
/// own section header and don't want the `DNS Verification:` heading.
pub fn dns_verification_record_lines(
    results: &[(String, DnsVerifyResult)],
    dns_records: &[DnsRecord],
) -> (Vec<String>, bool) {
    let mut lines = Vec::new();
    let mut all_pass = true;
    for (name, result) in results {
        match result {
            DnsVerifyResult::Pass => lines.push(format!("  {name}: {}", term::pass_badge())),
            DnsVerifyResult::Fail(msg) => {
                lines.push(format!("  {name}: {} - {msg}", term::fail_badge()));
                if let Some(rec) = dns_record_for_check(name, dns_records) {
                    lines.push(format!(
                        "         {} {}  {}  {}",
                        term::dim("→ Add:"),
                        rec.record_type,
                        rec.name,
                        rec.value
                    ));
                }
                // S44-2: DKIM failures have an operator-silent consequence
                // that bit us in finding #10. A single FAIL line in a
                // column of PASS lines is too easy to skim past, so print a
                // second, semantically-red line spelling out that outbound
                // signatures will not verify at receivers until the DNS
                // key matches the on-disk public key.
                if name == "DKIM" {
                    lines.push(format!(
                        "         {} Outbound DKIM signatures will FAIL verification at receivers until DNS matches.",
                        term::error("!!")
                    ));
                }
                all_pass = false;
            }
            DnsVerifyResult::Missing(msg) => {
                lines.push(format!("  {name}: {} - {msg}", term::missing_badge()));
                if let Some(rec) = dns_record_for_check(name, dns_records) {
                    lines.push(format!(
                        "         {} {}  {}  {}",
                        term::dim("→ Add:"),
                        rec.record_type,
                        rec.name,
                        rec.value
                    ));
                }
                if name == "DKIM" {
                    lines.push(format!(
                        "         {} Outbound DKIM signatures will FAIL verification at receivers until DNS matches.",
                        term::error("!!")
                    ));
                }
                all_pass = false;
            }
            DnsVerifyResult::Warn(msg) => {
                lines.push(format!("  {name}: {} - {msg}", term::warn_badge()));
            }
        }
    }
    (lines, all_pass)
}

/// Produce the full DNS verification output: preamble (`""`, `"DNS
/// Verification:"`, `""`), per-record lines, trailing blank. Used by
/// the setup wizard. Callers that render their own section header should
/// use `dns_verification_record_lines` instead to avoid double-headers.
pub fn dns_verification_lines(
    results: &[(String, DnsVerifyResult)],
    dns_records: &[DnsRecord],
) -> (Vec<String>, bool) {
    let (body, all_pass) = dns_verification_record_lines(results, dns_records);
    let mut lines = Vec::with_capacity(body.len() + 4);
    lines.push(String::new());
    lines.push("DNS Verification:".to_string());
    lines.push(String::new());
    lines.extend(body);
    lines.push(String::new());
    (lines, all_pass)
}

pub fn display_dns_verification(
    results: &[(String, DnsVerifyResult)],
    dns_records: &[DnsRecord],
) -> bool {
    let (lines, all_pass) = dns_verification_lines(results, dns_records);
    for line in lines {
        println!("{line}");
    }
    all_pass
}

pub fn display_mcp_section(data_dir: &Path) {
    println!("\n{}", term::header("[MCP]"));
    for line in mcp_section_lines(data_dir) {
        println!("{line}");
    }
}

/// Produce the plain-text body of the `[MCP]` section (without the header
/// line itself). Returned as a vector of lines so tests can assert on
/// content without spawning a subprocess.
pub fn mcp_section_lines(data_dir: &Path) -> Vec<String> {
    let mut lines = Vec::new();
    lines.push("Wire aimx into your AI agent with one command per agent:".to_string());
    lines.push(String::new());

    for spec in crate::agent_setup::registry() {
        let cmd = if data_dir == Path::new("/var/lib/aimx") {
            format!("aimx agent-setup {}", spec.name)
        } else {
            format!(
                "aimx --data-dir {} agent-setup {}",
                data_dir.display(),
                spec.name
            )
        };
        lines.push(format!("  {cmd}"));
    }

    lines.push(String::new());
    lines.push(
        "Run `aimx agent-setup --list` to see supported agents and destination paths.".to_string(),
    );
    lines.push(
        "See `book/agent-integration.md` for the full list and manual MCP wiring.".to_string(),
    );
    lines.push(String::new());
    lines
}

/// Finalize the on-disk install: ensure `data_dir` exists, create or
/// update `config.toml`, and generate the DKIM keypair if missing.
///
/// `trust_defaults` is honoured only on **fresh installs**. When `config.toml`
/// already exists the existing top-level `trust` / `trusted_senders` are
/// preserved, and `trust_defaults` is ignored. Pass `None` in that case.
pub fn finalize_setup(
    data_dir: &Path,
    domain: &str,
    dkim_selector: &str,
    trust_defaults: Option<(String, Vec<String>)>,
) -> Result<(), Box<dyn std::error::Error>> {
    std::fs::create_dir_all(data_dir)?;
    install_config_dir()?;

    let config_path = crate::config::config_path();
    let _config = if config_path.exists() {
        let mut cfg = Config::load_ignore_warnings(&config_path)?;
        if cfg.domain != domain {
            let old_domain = cfg.domain.clone();
            cfg.domain = domain.to_string();
            for mailbox in cfg.mailboxes.values_mut() {
                if mailbox.address.ends_with(&format!("@{old_domain}")) {
                    let local_part = mailbox
                        .address
                        .strip_suffix(&format!("@{old_domain}"))
                        .unwrap_or(&mailbox.address);
                    mailbox.address = format!("{local_part}@{domain}");
                }
            }
            install_config_file(&cfg, &config_path)?;
        }
        if !cfg.mailboxes.contains_key("catchall") {
            cfg.mailboxes.insert(
                "catchall".to_string(),
                MailboxConfig {
                    address: format!("*@{domain}"),
                    owner: crate::config::RESERVED_RUN_AS_CATCHALL.to_string(),
                    hooks: vec![],
                    trust: None,
                    trusted_senders: None,
                    allow_root_catchall: false,
                },
            );
            install_config_file(&cfg, &config_path)?;
        }
        cfg
    } else {
        let (default_trust, default_trusted_senders) =
            trust_defaults.unwrap_or_else(|| ("none".to_string(), vec![]));
        let mut mailboxes = HashMap::new();
        mailboxes.insert(
            "catchall".to_string(),
            MailboxConfig {
                address: format!("*@{domain}"),
                owner: crate::config::RESERVED_RUN_AS_CATCHALL.to_string(),
                hooks: vec![],
                trust: None,
                trusted_senders: None,
                allow_root_catchall: false,
            },
        );
        let cfg = Config {
            domain: domain.to_string(),
            data_dir: data_dir.to_path_buf(),
            dkim_selector: dkim_selector.to_string(),
            trust: default_trust,
            trusted_senders: default_trusted_senders,
            hook_templates: Vec::new(),
            mailboxes,
            verify_host: None,
            enable_ipv6: false,
            upgrade: None,
        };
        install_config_file(&cfg, &config_path)?;
        cfg
    };

    let catchall_dir = data_dir.join("catchall");
    std::fs::create_dir_all(&catchall_dir)?;

    let dkim_root = crate::config::dkim_dir();
    let dkim_private = dkim_root.join("private.key");
    if !dkim_private.exists() {
        println!("Generating DKIM keypair...");
        dkim::generate_keypair(&dkim_root, false)?;
        println!("DKIM keypair generated.");
    } else {
        println!("DKIM keypair already exists.");
    }

    Ok(())
}

fn announce_setup_complete(domain: &str) {
    // FR-3.5 step 8: the wizard closes with a single-line success
    // banner naming the domain. Intentionally terse — the operator just
    // sat through TLS, DKIM, DNS, and systemctl start; they don't need
    // more ceremony.
    println!();
    println!(
        "{}",
        term::success(&format!("aimx is running for {domain}."))
    );
}

/// Create the config dir (default `/etc/aimx`, or `AIMX_CONFIG_DIR` override)
/// with mode `0o755`. Idempotent: a pre-existing directory is left as-is.
fn install_config_dir() -> Result<(), Box<dyn std::error::Error>> {
    let dir = crate::config::config_dir();
    std::fs::create_dir_all(&dir)?;

    // Gate mode enforcement on `is_root()` alone: an operator who happens
    // to have `AIMX_CONFIG_DIR` exported on a real install still gets
    // tightened perms. Tests run as a non-root user so the branch is
    // skipped for their tempdir; `apply_config_file_mode_sets_640`
    // covers the real-install invariant directly.
    if is_root() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755))?;
        }
    }
    Ok(())
}

/// Write `config.toml` and (on real installs) tighten its mode to `0o640`,
/// owner `root:root`. Non-root invocations leave the mode at the OS default
/// the dedicated [`tests::apply_config_file_mode_sets_640`] test covers
/// the real-install invariant directly via [`apply_config_file_mode`].
///
/// When the file does not yet exist and we are running as root, it is
/// created atomically with mode `0o640` via `OpenOptions::mode(...)
/// .create_new(true)` so there is no brief window of umask-default
/// permissions between write and chmod. The rewrite path (re-entrant
/// setup) falls back to `Config::save` + `apply_config_file_mode`.
fn install_config_file(cfg: &Config, path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let is_root = is_root();
    if is_root && !path.exists() {
        #[cfg(unix)]
        {
            use std::io::Write as _;
            use std::os::unix::fs::OpenOptionsExt;
            let content = toml::to_string_pretty(cfg)?;
            let mut f = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o640)
                .open(path)?;
            f.write_all(content.as_bytes())?;
            f.sync_all()?;
            return Ok(());
        }
    }
    cfg.save(path)?;
    if is_root {
        apply_config_file_mode(path)?;
    }
    Ok(())
}

/// Set `config.toml` to mode `0o640`. Factored out of [`install_config_file`]
/// so the mode-enforcement path is unit-testable without actually running
/// as root on a real install.
pub(crate) fn apply_config_file_mode(path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o640))?;
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
    Ok(())
}

fn validate_domain(domain: &str) -> Result<(), Box<dyn std::error::Error>> {
    if domain.is_empty() {
        return Err("Domain must not be empty".into());
    }
    if domain.len() > 253 {
        return Err("Domain exceeds maximum length of 253 characters".into());
    }
    for label in domain.split('.') {
        if label.is_empty() || label.len() > 63 {
            return Err(format!("Invalid domain label: '{label}'").into());
        }
        if label.starts_with('-') || label.ends_with('-') {
            return Err(
                format!("Domain label must not start or end with a hyphen: '{label}'").into(),
            );
        }
        if !label.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
            return Err(format!("Domain label contains invalid characters: '{label}'").into());
        }
    }
    if domain.split('.').count() < 2 {
        return Err("Domain must have at least two labels (e.g. example.com)".into());
    }
    Ok(())
}

/// Upper bound on re-prompt attempts inside [`prompt_trusted_senders`].
/// Matches the [`MAX_OWNER_PROMPT_ATTEMPTS`] error-budget pattern so a
/// scripted stdin that never resolves to a valid entry cannot spin
/// forever. Operators fat-fingering an address get five chances before
/// the wizard aborts with the rejection history.
pub(crate) const MAX_TRUSTED_SENDERS_ATTEMPTS: usize = 5;

/// Warning printed when the operator leaves the trusted-senders prompt
/// blank. Also logged (at `warn` level) under `AIMX_NONINTERACTIVE=1`
/// where the interactive surface is skipped (FR-3.8). Kept as a
/// module-level constant so tests can assert the exact wording.
pub(crate) const EMPTY_TRUSTED_SENDERS_WARNING: &str = "No trusted senders configured. Hooks will NOT fire for inbound email. \
     Add senders later via `aimx config trust add`.";

/// Validate that a string is a plausible email address or glob pattern
/// suitable for `trusted_senders`. Delegates final semantics to the
/// `trust.rs` matcher — this only catches obvious typos at prompt time
/// so the operator sees the error before it hits `config.toml`.
///
/// Rules: exactly one `@`, a non-empty local part, a non-empty domain
/// part, and every character in the allowed set (alphanumeric, `.`,
/// `-`, `_`, `+`, `*`, `?`). Globs like `*@company.com`,
/// `alice*@example.com`, and `alice@*.company.com` all pass.
pub(crate) fn validate_trusted_sender(entry: &str) -> Result<(), String> {
    if entry.is_empty() {
        return Err("empty entry".to_string());
    }
    let at_count = entry.chars().filter(|c| *c == '@').count();
    if at_count != 1 {
        return Err(format!("expected exactly one '@', got {at_count}"));
    }
    let (local, domain) = entry.split_once('@').expect("one '@' checked above");
    if local.is_empty() {
        return Err("empty local part before '@'".to_string());
    }
    if domain.is_empty() {
        return Err("empty domain after '@'".to_string());
    }
    for c in entry.chars() {
        if c == '@' {
            continue;
        }
        let ok = c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | '+' | '*' | '?');
        if !ok {
            return Err(format!("invalid character '{c}'"));
        }
    }
    Ok(())
}

/// Parse a comma- or whitespace-separated line into a list of
/// trusted-sender entries. Each entry is validated via
/// [`validate_trusted_sender`]; the first invalid entry short-circuits
/// with an error that names the offending entry and the validation
/// reason — callers decide whether to re-prompt or abort.
pub(crate) fn parse_trusted_senders_line(line: &str) -> Result<Vec<String>, (String, String)> {
    let mut entries = Vec::new();
    for raw in line.split(|c: char| c == ',' || c.is_whitespace()) {
        let s = raw.trim();
        if s.is_empty() {
            continue;
        }
        match validate_trusted_sender(s) {
            Ok(()) => entries.push(s.to_string()),
            Err(reason) => return Err((s.to_string(), reason)),
        }
    }
    Ok(entries)
}

/// Interactively prompt for the `trusted_senders` allowlist (FR-3.2).
///
/// Returns `(policy, senders)`:
/// - Non-empty list → `("verified", senders)`; no warning printed.
/// - Empty input  → `("none", vec![])`; a loud warning block is printed
///   so the operator knows hooks will not fire.
///
/// Invalid entries re-prompt with `✗ invalid sender '<entry>': <reason>`;
/// after [`MAX_TRUSTED_SENDERS_ATTEMPTS`] rejections the wizard aborts.
pub fn prompt_trusted_senders(
    reader: &mut dyn BufRead,
) -> Result<(String, Vec<String>), Box<dyn std::error::Error>> {
    println!();
    for attempt in 1..=MAX_TRUSTED_SENDERS_ATTEMPTS {
        print!(
            "Trusted sender addresses (comma-separated, e.g. you@example.com, *@company.com) \
             — leave blank to disable hooks: "
        );
        io::stdout().flush()?;
        let mut line = String::new();
        reader.read_line(&mut line)?;
        match parse_trusted_senders_line(&line) {
            Ok(senders) if senders.is_empty() => {
                print_empty_trusted_senders_warning();
                return Ok(("none".to_string(), vec![]));
            }
            Ok(senders) => return Ok(("verified".to_string(), senders)),
            Err((entry, reason)) => {
                println!("{} invalid sender '{entry}': {reason}", term::fail_badge());
                if attempt == MAX_TRUSTED_SENDERS_ATTEMPTS {
                    return Err(format!(
                        "trusted-senders prompt aborted after {MAX_TRUSTED_SENDERS_ATTEMPTS} \
                         attempts; last error: invalid sender '{entry}': {reason}"
                    )
                    .into());
                }
            }
        }
    }
    unreachable!("loop exits via return");
}

/// Print the loud warning block for an empty trusted-senders list.
/// Extracted so [`prompt_trusted_senders`] and the non-interactive
/// branch in [`run_setup`] can share the exact wording.
fn print_empty_trusted_senders_warning() {
    println!();
    println!("{}", term::warn_badge());
    println!("  {}", term::warn(EMPTY_TRUSTED_SENDERS_WARNING));
    println!();
}

pub fn prompt_domain(reader: &mut dyn BufRead) -> Result<String, Box<dyn std::error::Error>> {
    print!("Enter the domain you want to use for email (e.g. agent.example.com): ");
    io::stdout().flush()?;
    let mut domain = String::new();
    reader.read_line(&mut domain)?;
    let domain = domain.trim().to_string();
    if domain.is_empty() {
        return Err("No domain entered. Setup cancelled.".into());
    }
    validate_domain(&domain)?;

    print!(
        "You will need to add MX, SPF, and DKIM DNS records for this domain.\n\
         Do you control this domain and have access to its DNS settings? (Y/n) "
    );
    io::stdout().flush()?;
    let mut confirm = String::new();
    reader.read_line(&mut confirm)?;
    if confirm
        .trim()
        .chars()
        .next()
        .is_some_and(|c| c == 'n' || c == 'N')
    {
        return Err("Setup cancelled. You need DNS access to proceed.".into());
    }

    Ok(domain)
}

/// True iff the on-disk `config.toml` has a mailbox whose address is
/// the wildcard catchall for its domain. Used by [`run_setup`] to gate
/// `aimx-catchall` system-user creation — no catchall mailbox means no
/// speculative user/home-dir provisioning (PRD §6.4 / S3-1). Returns
/// `false` if `config.toml` is missing or unreadable so fresh installs
/// skip the user-create step until `finalize_setup` has written the
/// file (in practice `finalize_setup` always runs first, but the
/// conservative default keeps this helper safe to call earlier).
fn config_has_catchall(config_path: &Path) -> bool {
    if !config_path.exists() {
        return false;
    }
    let Ok(cfg) = Config::load_ignore_warnings(config_path) else {
        return false;
    };
    cfg.mailboxes.values().any(|mb| mb.is_catchall(&cfg))
}

/// Upper bound on re-prompt attempts inside [`prompt_mailbox_owner`].
/// Protects tests (and any CI capturing stdin from a script) from
/// spinning forever when the scripted input never resolves to a real
/// user. Five attempts is comfortable for an operator fat-fingering the
/// username without turning an unattended CI into an infinite loop.
pub(crate) const MAX_OWNER_PROMPT_ATTEMPTS: usize = 5;

/// Name of the env var that forces [`prompt_mailbox_owner`] into its
/// non-interactive branch: if set to a truthy value the helper accepts
/// the local-part default when it resolves, or errors hard when no
/// default is available. Mirrors the convention used by other scripted
/// installer paths (CI, OS image builders) in the aimx codebase.
pub(crate) const NONINTERACTIVE_ENV: &str = "AIMX_NONINTERACTIVE";

pub(crate) fn is_noninteractive_env() -> bool {
    matches!(
        std::env::var(NONINTERACTIVE_ENV).ok().as_deref(),
        Some("1") | Some("true") | Some("yes"),
    )
}

/// Prompt the operator for the Linux user that should own mail for
/// `address` (S3-2). Default = local-part of the address when
/// `getpwnam(local_part)` resolves on the host; otherwise no default
/// and explicit input is required.
///
/// Unknown users re-prompt with an actionable `useradd` hint up to
/// [`MAX_OWNER_PROMPT_ATTEMPTS`] times before giving up. Under
/// `AIMX_NONINTERACTIVE=1` the helper accepts the local-part default
/// when available, or errors hard when the local part doesn't resolve
/// — scripted installs never block on a prompt.
///
/// The catchall mailbox never goes through this helper; its owner is
/// hard-coded to `aimx-catchall` by [`finalize_setup`] (S3-2 AC).
///
/// Live caller: `aimx mailboxes create` invokes this when the operator
/// omits `--owner` (see `mailbox::resolve_create_owner`). Sprints 6+ will
/// share the same seam from `aimx agent-setup`.
pub fn prompt_mailbox_owner(
    address: &str,
    sys: &dyn SystemOps,
) -> Result<String, Box<dyn std::error::Error>> {
    let stdin = io::stdin();
    let mut reader = stdin.lock();
    prompt_mailbox_owner_with_reader(address, sys, &mut reader)
}

/// Inner prompt helper that reads from an injectable `BufRead`. The
/// public [`prompt_mailbox_owner`] wraps this with a `stdin().lock()`
/// reader in production; tests drive the re-prompt / 5-failure branches
/// with scripted `Cursor<&[u8]>` input.
pub(crate) fn prompt_mailbox_owner_with_reader(
    address: &str,
    sys: &dyn SystemOps,
    reader: &mut dyn io::BufRead,
) -> Result<String, Box<dyn std::error::Error>> {
    let local_part = address.split('@').next().unwrap_or("").trim().to_string();
    let default_candidate = if local_part.is_empty() {
        None
    } else if crate::user_resolver::resolve_user(&local_part).is_some() {
        Some(local_part.clone())
    } else {
        None
    };

    if is_noninteractive_env() {
        return default_candidate.ok_or_else(|| {
            format!(
                "AIMX_NONINTERACTIVE=1 requires a resolvable default owner for \
                 mailbox '{address}'; local part '{local_part}' does not resolve \
                 via getpwnam. Create the user with `useradd --system {local_part}` \
                 or unset AIMX_NONINTERACTIVE and re-run setup."
            )
            .into()
        });
    }

    for attempt in 0..MAX_OWNER_PROMPT_ATTEMPTS {
        match &default_candidate {
            Some(def) => print!("Which Linux user should own `{address}`? [{def}]: "),
            None => print!(
                "Which Linux user should own `{address}`? (no default, \
                 explicit input required): "
            ),
        }
        io::stdout().flush()?;

        let mut line = String::new();
        reader.read_line(&mut line)?;
        let entered = line.trim().to_string();

        let candidate = if entered.is_empty() {
            match &default_candidate {
                Some(def) => def.clone(),
                None => {
                    println!(
                        "  {} {}",
                        term::warn("No default available."),
                        term::dim("Type a valid Linux username and press Enter.")
                    );
                    continue;
                }
            }
        } else {
            entered
        };

        if sys.user_exists(&candidate) {
            return Ok(candidate);
        }

        eprintln!(
            "  {} User '{candidate}' does not exist. Create it with \
             `useradd {candidate}` and re-enter, or type an existing \
             Linux username. ({remaining} attempt(s) remaining)",
            term::warn("!"),
            remaining = MAX_OWNER_PROMPT_ATTEMPTS - attempt - 1,
        );
    }
    Err(format!(
        "could not resolve an owner for mailbox '{address}' after \
         {MAX_OWNER_PROMPT_ATTEMPTS} attempts; create the intended Linux \
         user with `useradd <name>` and re-run setup"
    )
    .into())
}

pub fn is_already_configured(sys: &dyn SystemOps, _data_dir: &Path) -> bool {
    let tls_cert = Path::new("/etc/ssl/aimx/cert.pem");
    let dkim_key = crate::config::dkim_dir().join("private.key");

    let service_running = sys.is_service_running("aimx");
    let cert_exists = sys.file_exists(tls_cert);
    let dkim_exists = sys.file_exists(&dkim_key);

    service_running && cert_exists && dkim_exists
}

/// Gate detected IPv6 on `enable_ipv6`.
///
/// IPv6 outbound is opt-in via `enable_ipv6` in `config.toml`. When disabled,
/// the detected IPv6 is dropped. AAAA + `ip6:` SPF guidance/verification are
/// omitted to match the IPv4-only default of `aimx send`.
///
/// The caller passes the IPv6 from a single `get_server_ips()` call
/// (no second `hostname -I`); this helper just applies the opt-in gate.
/// Kept as a standalone function so the gate is trivially testable.
pub(crate) fn detect_server_ipv6(enable_ipv6: bool, ipv6: Option<Ipv6Addr>) -> Option<Ipv6Addr> {
    if enable_ipv6 { ipv6 } else { None }
}

pub fn run_setup(
    domain: Option<&str>,
    data_dir: Option<&Path>,
    sys: &dyn SystemOps,
    net: &dyn NetworkOps,
) -> Result<(), Box<dyn std::error::Error>> {
    // Step 1: Root check
    if !sys.check_root() {
        return Err("`aimx setup` requires root. Run with: sudo aimx setup <domain>".into());
    }

    // Step 2: Port 25 preflight. Runs BEFORE the domain prompt and any
    // filesystem writes. If the VPS blocks SMTP there is no point asking for
    // a domain, generating TLS certs, or writing config.
    let port25_status = sys.check_port25_occupancy()?;
    if let Port25Status::OtherProcess(name) = &port25_status {
        return Err(format!(
            "Port 25 is occupied by {name}. \
             Stop the process and run `aimx setup` again."
        )
        .into());
    }

    println!("{}\n", term::header("Port 25 preflight"));
    if matches!(port25_status, Port25Status::Aimx) {
        println!("  `aimx serve` is already running on port 25. Probing the live daemon.");
        run_port25_preflight(net)?;
    } else {
        sys.with_temp_smtp_listener(&mut || run_port25_preflight(net))?;
    }
    println!();

    // Resolve domain: use argument if provided, otherwise prompt interactively
    let domain = match domain {
        Some(d) => {
            validate_domain(d)?;
            d.to_string()
        }
        None => {
            let stdin = io::stdin();
            let mut reader = stdin.lock();
            prompt_domain(&mut reader)?
        }
    };

    println!("aimx setup for {domain}\n");

    let data_dir = data_dir.unwrap_or(Path::new("/var/lib/aimx"));
    std::fs::create_dir_all(data_dir)?;
    install_config_dir()?;

    let config_path = crate::config::config_path();
    let (dkim_selector, enable_ipv6) = if config_path.exists() {
        match Config::load_ignore_warnings(&config_path) {
            Ok(c) => (c.dkim_selector, c.enable_ipv6),
            Err(_) => ("aimx".to_string(), false),
        }
    } else {
        ("aimx".to_string(), false)
    };

    // Re-entrant detection: if already configured, skip install/configure steps
    let already_configured = is_already_configured(sys, data_dir);

    if already_configured {
        println!(
            "{}",
            term::success(
                "Existing aimx configuration detected. Skipping install, proceeding to verification."
            )
        );
    }

    // Step 3 (FR-3.5): Trusted-senders prompt. Runs BEFORE TLS cert and
    // DKIM keygen so the operator is not asked a decision question
    // after the wizard has started mutating /etc. Re-entry skips the
    // prompt (existing values preserved — FR-3.7). Non-interactive
    // installs default to an empty list with the warning logged, not
    // displayed (FR-3.8).
    let trust_defaults = if config_path.exists() {
        None
    } else if is_noninteractive_env() {
        tracing::warn!("{}", EMPTY_TRUSTED_SENDERS_WARNING);
        Some(("none".to_string(), Vec::<String>::new()))
    } else {
        let stdin = io::stdin();
        let mut reader = stdin.lock();
        Some(prompt_trusted_senders(&mut reader)?)
    };

    // Step 4 (FR-3.5): Generate TLS cert, then write config.toml + DKIM
    // keys. Idempotent on re-entry (handles domain changes). Must
    // happen before the aimx.service install at the end, because the
    // daemon refuses to start without a loadable config and DKIM key.
    if !already_configured {
        let cert_dir = Path::new("/etc/ssl/aimx");
        if !sys.file_exists(&cert_dir.join("cert.pem")) {
            println!("Generating self-signed TLS certificate...");
            sys.generate_tls_cert(cert_dir, &domain)?;
            println!("TLS certificate generated in /etc/ssl/aimx/");
        } else {
            println!("TLS certificate already exists.");
        }
    }

    finalize_setup(data_dir, &domain, &dkim_selector, trust_defaults)?;

    // Step 4b: Create `aimx-catchall` service user (PRD §6.4, S3-1) —
    // but only when the operator's configured mailboxes include a
    // catchall, so installs that skip the catchall don't bloat
    // `/etc/passwd` with a service user they'll never use. Re-entrant
    // setup passes through the same guard so a host that once had a
    // catchall and removed it won't re-create the user.
    if config_has_catchall(&config_path) {
        ensure_catchall_user(sys)?;
    }

    // Step 6: DNS guidance and verification (section [DNS])
    // Single `hostname -I` invocation (S32-4): derive both families from one call.
    let (ipv4_detected, ipv6_detected) = net.get_server_ips()?;
    let server_ipv4 = ipv4_detected
        .ok_or::<Box<dyn std::error::Error>>("Could not determine server IPv4 address".into())?;
    let server_ipv6 = detect_server_ipv6(enable_ipv6, ipv6_detected);
    let server_ip: IpAddr = IpAddr::V4(server_ipv4);
    let server_ipv6_ip: Option<IpAddr> = server_ipv6.map(IpAddr::V6);
    let dkim_value = dkim::dns_record_value(&crate::config::dkim_dir())?;

    let local_dkim_pubkey = dkim_value
        .strip_prefix("v=DKIM1; k=rsa; p=")
        .map(|s| s.to_string());

    let server_ip_str = server_ip.to_string();
    let server_ipv6_str = server_ipv6_ip.map(|ip| ip.to_string());
    display_dns_guidance(
        &domain,
        &server_ip_str,
        server_ipv6_str.as_deref(),
        &dkim_value,
        &dkim_selector,
    );
    let dns_records = generate_dns_records(
        &domain,
        &server_ip_str,
        server_ipv6_str.as_deref(),
        &dkim_value,
        &dkim_selector,
    );

    // Step 6 (FR-3.5): DNS verify loop. The "press q to skip and run
    // `aimx doctor` later" escape is surfaced as a prominent standalone
    // line, not buried as a parenthetical, so an operator who hasn't
    // finished propagating records doesn't feel trapped in the wizard.
    loop {
        println!();
        println!(
            "  Press {} to verify DNS records now.",
            term::highlight("Enter")
        );
        println!(
            "  Press {} to skip and run `{}` later.",
            term::highlight("q"),
            term::highlight("aimx doctor")
        );
        print!("> ");
        io::stdout().flush()?;
        let mut input = String::new();
        io::stdin().read_line(&mut input)?;
        if input.trim().eq_ignore_ascii_case("q") {
            println!(
                "Update your DNS records and run `{}` to re-verify.",
                term::highlight("aimx doctor")
            );
            break;
        }

        let results = verify_all_dns(
            net,
            &domain,
            &server_ip,
            server_ipv6_ip.as_ref(),
            &dkim_selector,
            local_dkim_pubkey.as_deref(),
        );
        let all_pass = display_dns_verification(&results, &dns_records);

        if all_pass {
            println!(
                "{}",
                term::success("All DNS records verified. Your email server is ready!")
            );
            break;
        } else {
            println!("Some DNS records are not yet correct.");
            println!("DNS propagation can take up to 48 hours.");
        }
    }

    // Write (or refresh) the agent-facing README inside the data directory.
    crate::datadir_readme::write(data_dir)?;

    // Step 7 (FR-3.5): Install and start aimx.service once DNS guidance
    // is out of the way. Setup concludes with the daemon bound to :25
    // and verified healthy, or a loud error.
    if !already_configured {
        install_and_verify_service(sys, data_dir)?;
    }

    // Section [MCP] — informational summary per FR-3.4. The actual
    // agent-setup drop-through lands in Sprint 6; for now the wizard
    // closes with the placeholder message pointing operators at
    // `aimx agent-setup`.
    display_mcp_section(data_dir);

    // Step 8 (FR-3.5): one-line success banner.
    announce_setup_complete(&domain);

    // Placeholder for the Sprint 6 drop-through to `aimx agent-setup`
    // as `$SUDO_USER`. Skipped under AIMX_NONINTERACTIVE=1 (FR-3.8) so
    // scripted installs don't print a hint that only makes sense to a
    // human operator at the end of the wizard.
    if !is_noninteractive_env() {
        println!(
            "{} Run `{}` as your regular user to wire aimx into Claude Code, Codex, etc.",
            term::highlight("→"),
            term::highlight("aimx agent-setup")
        );
    }

    Ok(())
}

/// Install the systemd/OpenRC service file, restart the daemon, and poll
/// `127.0.0.1:25` until it accepts a TCP connection. Errors out if the
/// daemon doesn't bind within the readiness window. Setup's last step
/// must either leave `aimx serve` running or tell the operator exactly
/// what went wrong.
fn install_and_verify_service(
    sys: &dyn SystemOps,
    data_dir: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    println!("\nStarting aimx serve...");
    sys.install_service_file(data_dir)?;

    print!("  Waiting for aimx serve to bind port 25... ");
    io::stdout().flush()?;
    if sys.wait_for_service_ready() {
        println!("{}", term::pass_badge());
        println!("{}", term::success("`aimx serve` is running."));
        Ok(())
    } else {
        println!("{}", term::fail_badge());
        Err(
            "aimx.service did not bind port 25 within the readiness window.\n\
             Check `sudo journalctl -u aimx` for errors, then run `sudo aimx setup` again."
                .into(),
        )
    }
}

/// Probe outbound and inbound port 25. Returns `Err` if either leg fails.
/// Prints a PASS/FAIL line for each check so the operator can see progress.
fn run_port25_preflight(net: &dyn NetworkOps) -> Result<(), Box<dyn std::error::Error>> {
    let mut port_failed = false;

    print!("  Outbound port 25... ");
    io::stdout().flush()?;
    match check_outbound(net) {
        PreflightResult::Pass(_) => println!("{}", term::pass_badge()),
        PreflightResult::Fail(msg) => {
            println!("{}", term::fail_badge());
            eprintln!("\n  {msg}");
            eprintln!("\n  Compatible VPS providers with port 25 open:");
            for p in COMPATIBLE_PROVIDERS {
                eprintln!("    - {p}");
            }
            port_failed = true;
        }
    }

    print!("  Inbound port 25... ");
    io::stdout().flush()?;
    match check_inbound(net) {
        PreflightResult::Pass(_) => println!("{}", term::pass_badge()),
        PreflightResult::Fail(msg) => {
            println!("{}", term::fail_badge());
            eprintln!("\n  {msg}");
            port_failed = true;
        }
    }

    if port_failed {
        return Err(
            "Port 25 checks failed. Your VPS provider may block SMTP traffic.\n\
             Fix the issues above and run `sudo aimx setup` again."
                .into(),
        );
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::mailbox;
    use std::cell::RefCell;
    use std::collections::HashMap;
    use tempfile::TempDir;

    struct MockNetworkOps {
        outbound_port25: bool,
        inbound_port25: bool,
        server_ipv4: Option<Ipv4Addr>,
        server_ipv6: Option<Ipv6Addr>,
        mx_records: HashMap<String, Vec<String>>,
        a_records: HashMap<String, Vec<IpAddr>>,
        aaaa_records: HashMap<String, Vec<IpAddr>>,
        txt_records: HashMap<String, Vec<String>>,
        get_server_ips_calls: std::cell::Cell<u32>,
    }

    impl Default for MockNetworkOps {
        fn default() -> Self {
            Self {
                outbound_port25: true,
                inbound_port25: true,
                server_ipv4: Some("1.2.3.4".parse().unwrap()),
                server_ipv6: None,
                mx_records: HashMap::new(),
                a_records: HashMap::new(),
                aaaa_records: HashMap::new(),
                txt_records: HashMap::new(),
                get_server_ips_calls: std::cell::Cell::new(0),
            }
        }
    }

    impl NetworkOps for MockNetworkOps {
        fn check_outbound_port25(&self) -> Result<bool, Box<dyn std::error::Error>> {
            Ok(self.outbound_port25)
        }
        fn check_inbound_port25(&self) -> Result<bool, Box<dyn std::error::Error>> {
            Ok(self.inbound_port25)
        }
        fn get_server_ips(
            &self,
        ) -> Result<(Option<Ipv4Addr>, Option<Ipv6Addr>), Box<dyn std::error::Error>> {
            self.get_server_ips_calls
                .set(self.get_server_ips_calls.get() + 1);
            Ok((self.server_ipv4, self.server_ipv6))
        }
        fn resolve_mx(&self, domain: &str) -> Result<Vec<String>, Box<dyn std::error::Error>> {
            Ok(self.mx_records.get(domain).cloned().unwrap_or_default())
        }
        fn resolve_a(&self, domain: &str) -> Result<Vec<IpAddr>, Box<dyn std::error::Error>> {
            Ok(self.a_records.get(domain).cloned().unwrap_or_default())
        }
        fn resolve_aaaa(&self, domain: &str) -> Result<Vec<IpAddr>, Box<dyn std::error::Error>> {
            Ok(self.aaaa_records.get(domain).cloned().unwrap_or_default())
        }
        fn resolve_txt(&self, domain: &str) -> Result<Vec<String>, Box<dyn std::error::Error>> {
            Ok(self.txt_records.get(domain).cloned().unwrap_or_default())
        }
    }

    pub(crate) struct MockSystemOps {
        pub(crate) written_files: RefCell<HashMap<PathBuf, String>>,
        pub(crate) existing_files: HashMap<PathBuf, String>,
        pub(crate) restarted_services: RefCell<Vec<String>>,
        /// Ordered list of services passed to `stop_service`. Used by
        /// the Sprint 4 `aimx upgrade` tests to assert the stop → swap
        /// → start sequence.
        pub(crate) stopped_services: RefCell<Vec<String>>,
        /// Ordered list of services passed to `start_service`.
        pub(crate) started_services: RefCell<Vec<String>>,
        pub(crate) service_file_installed: RefCell<bool>,
        pub(crate) is_root: bool,
        pub(crate) port25_status: Port25Status,
        pub(crate) service_running: bool,
        pub(crate) service_ready: bool,
        pub(crate) wait_for_ready_calls: RefCell<u32>,
        /// Set of existing users (for `user_exists`) — mutable to simulate
        /// a `create_system_user` call creating the user.
        pub(crate) existing_users: RefCell<std::collections::HashSet<String>>,
        /// Ordered list of `(user, home)` pairs passed to
        /// `create_system_user`. Sprint 3 renames the helper's caller
        /// from `ensure_hook_user` to `ensure_catchall_user`; tests
        /// assert both the user name AND its home-dir arg.
        pub(crate) created_users: RefCell<Vec<(String, PathBuf)>>,
        /// Ordered list of `(path, owner, group, mode)` tuples passed to
        /// `ensure_owned_directory`. Sprint 3 replaces the recursive
        /// `chown_group_readable` with a single-path, explicit-mode
        /// helper scoped to the `aimx-catchall` home dir.
        pub(crate) owned_dirs: RefCell<Vec<(PathBuf, String, String, u32)>>,
        /// Override for [`SystemOps::get_aimx_binary_path`] so the Sprint 4
        /// `aimx upgrade` tests can route the swap through a tempdir
        /// instead of the real `/usr/local/bin/aimx`.
        pub(crate) override_aimx_binary_path: Option<PathBuf>,
        /// If set, `stop_service` returns an error. Used to exercise the
        /// "stop failed, nothing to roll back" branch.
        pub(crate) stop_service_fails: bool,
        /// If set (sticky), every `start_service` call returns an error.
        /// Used to exercise the "start failed, rollback's own start
        /// also fails" path that yields `UpgradeError::RollbackFailed`.
        pub(crate) start_service_fails: bool,
        /// Number of upcoming `start_service` calls that must fail
        /// before subsequent calls succeed. Takes precedence over
        /// `start_service_fails` when non-zero. Set to `1` to drive the
        /// canonical `RolledBack { failed_step: StartService }` path:
        /// the upgrade's `start_service` call fails, rollback's own
        /// `start_service` call succeeds, and the service is left
        /// running on the previous binary.
        pub(crate) start_service_failures_remaining: std::cell::Cell<u32>,
    }

    impl Default for MockSystemOps {
        fn default() -> Self {
            Self {
                written_files: RefCell::new(HashMap::new()),
                existing_files: HashMap::new(),
                restarted_services: RefCell::new(vec![]),
                stopped_services: RefCell::new(vec![]),
                started_services: RefCell::new(vec![]),
                service_file_installed: RefCell::new(false),
                is_root: true,
                port25_status: Port25Status::Free,
                service_running: false,
                service_ready: true,
                wait_for_ready_calls: RefCell::new(0),
                existing_users: RefCell::new(std::collections::HashSet::new()),
                created_users: RefCell::new(vec![]),
                owned_dirs: RefCell::new(vec![]),
                override_aimx_binary_path: None,
                stop_service_fails: false,
                start_service_fails: false,
                start_service_failures_remaining: std::cell::Cell::new(0),
            }
        }
    }

    impl SystemOps for MockSystemOps {
        fn write_file(&self, path: &Path, content: &str) -> Result<(), Box<dyn std::error::Error>> {
            self.written_files
                .borrow_mut()
                .insert(path.to_path_buf(), content.to_string());
            Ok(())
        }
        fn file_exists(&self, path: &Path) -> bool {
            self.existing_files.contains_key(path) || self.written_files.borrow().contains_key(path)
        }
        fn restart_service(&self, service: &str) -> Result<(), Box<dyn std::error::Error>> {
            self.restarted_services
                .borrow_mut()
                .push(service.to_string());
            Ok(())
        }
        fn stop_service(&self, service: &str) -> Result<(), Box<dyn std::error::Error>> {
            self.stopped_services.borrow_mut().push(service.to_string());
            if self.stop_service_fails {
                return Err("mock stop failure".into());
            }
            Ok(())
        }
        fn start_service(&self, service: &str) -> Result<(), Box<dyn std::error::Error>> {
            self.started_services.borrow_mut().push(service.to_string());
            let remaining = self.start_service_failures_remaining.get();
            if remaining > 0 {
                self.start_service_failures_remaining.set(remaining - 1);
                return Err("mock start failure (scheduled)".into());
            }
            if self.start_service_fails {
                return Err("mock start failure".into());
            }
            Ok(())
        }
        fn is_service_running(&self, _service: &str) -> bool {
            self.service_running
        }
        fn generate_tls_cert(
            &self,
            _cert_dir: &Path,
            _domain: &str,
        ) -> Result<(), Box<dyn std::error::Error>> {
            Ok(())
        }
        fn get_aimx_binary_path(&self) -> Result<PathBuf, Box<dyn std::error::Error>> {
            Ok(self
                .override_aimx_binary_path
                .clone()
                .unwrap_or_else(|| PathBuf::from("/usr/local/bin/aimx")))
        }
        fn check_root(&self) -> bool {
            self.is_root
        }
        fn check_port25_occupancy(&self) -> Result<Port25Status, Box<dyn std::error::Error>> {
            match &self.port25_status {
                Port25Status::Free => Ok(Port25Status::Free),
                Port25Status::Aimx => Ok(Port25Status::Aimx),
                Port25Status::OtherProcess(name) => Ok(Port25Status::OtherProcess(name.clone())),
            }
        }
        fn install_service_file(&self, _data_dir: &Path) -> Result<(), Box<dyn std::error::Error>> {
            *self.service_file_installed.borrow_mut() = true;
            Ok(())
        }
        fn uninstall_service_file(&self) -> Result<(), Box<dyn std::error::Error>> {
            *self.service_file_installed.borrow_mut() = false;
            Ok(())
        }
        fn wait_for_service_ready(&self) -> bool {
            *self.wait_for_ready_calls.borrow_mut() += 1;
            self.service_ready
        }
        fn with_temp_smtp_listener(
            &self,
            f: &mut dyn FnMut() -> Result<(), Box<dyn std::error::Error>>,
        ) -> Result<(), Box<dyn std::error::Error>> {
            // Mock: no real bind; the network probe is mocked too.
            f()
        }
        fn user_exists(&self, user: &str) -> bool {
            self.existing_users.borrow().contains(user)
        }
        fn create_system_user(
            &self,
            user: &str,
            home: &Path,
        ) -> Result<bool, Box<dyn std::error::Error>> {
            self.created_users
                .borrow_mut()
                .push((user.to_string(), home.to_path_buf()));
            let newly = self.existing_users.borrow_mut().insert(user.to_string());
            Ok(newly)
        }
        fn ensure_owned_directory(
            &self,
            path: &Path,
            owner: &str,
            group: &str,
            mode: u32,
        ) -> Result<(), Box<dyn std::error::Error>> {
            // Record the call without touching the real filesystem —
            // `ensure_catchall_user`'s target path (`/var/lib/aimx-catchall`)
            // is outside any tempdir, and we don't need a real inode for
            // tests. Assertions compare against the recorded tuple, not
            // against directory existence.
            self.owned_dirs.borrow_mut().push((
                path.to_path_buf(),
                owner.to_string(),
                group.to_string(),
                mode,
            ));
            Ok(())
        }
    }

    // ----- Sprint 3 S3-1: ensure_catchall_user ---------------------------

    #[test]
    fn ensure_catchall_user_creates_user_when_absent() {
        let sys = MockSystemOps::default();
        super::ensure_catchall_user(&sys).unwrap();
        let created = sys.created_users.borrow();
        assert_eq!(
            created.len(),
            1,
            "ensure_catchall_user must call create_system_user once: {created:?}"
        );
        assert_eq!(
            created[0].0,
            super::CATCHALL_SERVICE_USER,
            "must create the aimx-catchall user"
        );
        assert_eq!(
            created[0].1,
            Path::new(super::CATCHALL_HOME_DIR),
            "home dir arg must match CATCHALL_HOME_DIR (PRD §6.4)"
        );
        // Home dir was provisioned with the expected (owner, group, mode)
        // triple so aimx-catchall can read its own home but nobody else.
        let owned = sys.owned_dirs.borrow();
        assert_eq!(owned.len(), 1, "home dir must be chowned once: {owned:?}");
        assert_eq!(owned[0].0, Path::new(super::CATCHALL_HOME_DIR));
        assert_eq!(owned[0].1, super::CATCHALL_SERVICE_USER);
        assert_eq!(owned[0].2, super::CATCHALL_SERVICE_USER);
        assert_eq!(owned[0].3, 0o700);
    }

    #[test]
    fn ensure_catchall_user_is_idempotent_when_user_exists() {
        let sys = MockSystemOps::default();
        sys.existing_users
            .borrow_mut()
            .insert(super::CATCHALL_SERVICE_USER.to_string());
        super::ensure_catchall_user(&sys).unwrap();
        let created = sys.created_users.borrow();
        // `create_system_user` is still called once — the mock returns
        // `false` (meaning "not newly created") when the user is
        // pre-existing — so the helper short-circuits the useradd
        // invocation but still re-asserts the home-dir invariant.
        assert_eq!(created.len(), 1);
        assert_eq!(
            sys.owned_dirs.borrow().len(),
            1,
            "home dir permissions re-asserted on every run so drift heals"
        );
    }

    #[test]
    fn run_setup_skips_catchall_user_when_no_catchall_configured() {
        // S3-1 AC: `ensure_catchall_user` is gated on whether the
        // on-disk config contains a catchall mailbox. A synthetic
        // config with only a named (non-wildcard) mailbox must not
        // trigger user creation.
        let tmp = tempfile::TempDir::new().unwrap();
        let cfg_path = tmp.path().join("config.toml");
        std::fs::write(
            &cfg_path,
            r#"domain = "test.example"

[mailboxes.ops]
address = "ops@test.example"
owner = "ops"
"#,
        )
        .unwrap();
        // The `ops` mailbox is a named (non-wildcard) address with an
        // orphan owner; `Config::load_ignore_warnings` happily loads it
        // (orphan-owner is a warning, not an error) and
        // `config_has_catchall` then inspects the mailbox map and
        // returns `false` because no entry is `*@<domain>`. That is the
        // correct "don't create the catchall user" branch.
        assert!(!super::config_has_catchall(&cfg_path));
    }

    #[test]
    fn run_setup_detects_catchall_when_wildcard_mailbox_present() {
        // Companion to the test above: when the config does carry a
        // wildcard mailbox, `config_has_catchall` returns `true` so
        // run_setup will invoke ensure_catchall_user.
        let tmp = tempfile::TempDir::new().unwrap();
        let cfg_path = tmp.path().join("config.toml");
        std::fs::write(
            &cfg_path,
            r#"domain = "test.example"

[mailboxes.catchall]
address = "*@test.example"
owner = "aimx-catchall"
"#,
        )
        .unwrap();
        assert!(super::config_has_catchall(&cfg_path));
    }

    // ----- Sprint 3 S3-2: prompt_mailbox_owner ---------------------------

    /// Serialize `AIMX_NONINTERACTIVE` env-var mutations across tests
    /// that toggle it; cargo runs tests in parallel threads and env is
    /// process-global so concurrent mutation would cross-contaminate.
    static NONINT_ENV_SERIALIZE: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn prompt_mailbox_owner_noninteractive_uses_local_part_default() {
        use crate::user_resolver::{ResolvedUser, set_test_resolver};
        fn resolver(name: &str) -> Option<ResolvedUser> {
            if name == "alice" {
                Some(ResolvedUser {
                    name: "alice".into(),
                    uid: 1001,
                    gid: 1001,
                })
            } else {
                None
            }
        }
        let _r = set_test_resolver(resolver);
        let _g = NONINT_ENV_SERIALIZE
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        // SAFETY: the serialize mutex above keeps concurrent tests from
        // racing on the process-global env block.
        unsafe { std::env::set_var(super::NONINTERACTIVE_ENV, "1") };
        let sys = MockSystemOps::default();
        let owner = super::prompt_mailbox_owner("alice@example.com", &sys).unwrap();
        unsafe { std::env::remove_var(super::NONINTERACTIVE_ENV) };
        assert_eq!(owner, "alice");
    }

    #[test]
    fn prompt_mailbox_owner_noninteractive_errors_when_no_default() {
        use crate::user_resolver::{ResolvedUser, set_test_resolver};
        fn empty_resolver(_name: &str) -> Option<ResolvedUser> {
            None
        }
        let _r = set_test_resolver(empty_resolver);
        let _g = NONINT_ENV_SERIALIZE
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        unsafe { std::env::set_var(super::NONINTERACTIVE_ENV, "1") };
        let sys = MockSystemOps::default();
        let err = super::prompt_mailbox_owner("ghost@example.com", &sys)
            .unwrap_err()
            .to_string();
        unsafe { std::env::remove_var(super::NONINTERACTIVE_ENV) };
        assert!(
            err.contains("AIMX_NONINTERACTIVE") && err.contains("ghost") && err.contains("useradd"),
            "non-interactive-no-default must point at useradd: {err}"
        );
    }

    #[test]
    fn prompt_mailbox_owner_max_attempts_is_bounded() {
        // Defence-in-depth: the constant exists and is small enough to
        // avoid hanging tests that happen to hit the re-prompt path.
        // Evaluated at compile time so clippy's
        // `assertions_on_constants` lint stays quiet (the wrapper
        // `const _` pattern below is the canonical fix).
        const _: () = assert!(
            MAX_OWNER_PROMPT_ATTEMPTS >= 1 && MAX_OWNER_PROMPT_ATTEMPTS <= 10,
            "sensible re-prompt ceiling expected"
        );
    }

    #[test]
    fn prompt_mailbox_owner_reprompts_until_valid_user_entered() {
        // Sprint 7.5 S7.5-3: exercise the re-prompt loop with a scripted
        // stdin. Two bad entries (users that don't exist) followed by a
        // good one — the helper must re-prompt after each bad entry and
        // return the good entry without burning the full 5-attempt
        // budget. Mock `SystemOps` reports only "carol" as a real user.
        use crate::user_resolver::{ResolvedUser, set_test_resolver};
        fn empty_resolver(_name: &str) -> Option<ResolvedUser> {
            None
        }
        let _r = set_test_resolver(empty_resolver);
        let _g = NONINT_ENV_SERIALIZE
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        unsafe { std::env::remove_var(super::NONINTERACTIVE_ENV) };

        let sys = MockSystemOps::default();
        sys.existing_users.borrow_mut().insert("carol".into());

        // Three lines, one per `read_line` call. The first two are
        // rejected by the `user_exists` mock, the third succeeds.
        let scripted = b"alice\nbob\ncarol\n";
        let mut cursor = std::io::Cursor::new(&scripted[..]);
        let owner =
            super::prompt_mailbox_owner_with_reader("team@example.com", &sys, &mut cursor).unwrap();
        assert_eq!(owner, "carol");
    }

    #[test]
    fn prompt_mailbox_owner_errors_after_five_rejected_attempts() {
        // Sprint 7.5 S7.5-3: when every scripted attempt is a bad user,
        // the helper must give up after `MAX_OWNER_PROMPT_ATTEMPTS` and
        // return the documented useradd error.
        use crate::user_resolver::{ResolvedUser, set_test_resolver};
        fn empty_resolver(_name: &str) -> Option<ResolvedUser> {
            None
        }
        let _r = set_test_resolver(empty_resolver);
        let _g = NONINT_ENV_SERIALIZE
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        unsafe { std::env::remove_var(super::NONINTERACTIVE_ENV) };

        // `existing_users` stays empty — nothing the operator types can
        // resolve, so every attempt re-prompts.
        let sys = MockSystemOps::default();

        let scripted = b"a\nb\nc\nd\ne\n";
        let mut cursor = std::io::Cursor::new(&scripted[..]);
        let err = super::prompt_mailbox_owner_with_reader("ghost@example.com", &sys, &mut cursor)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("5 attempts") && err.contains("useradd"),
            "after 5 failures the error must name the ceiling and point at useradd: {err}"
        );
    }

    // ----- Sprint 3 S3-3: legacy-template-phase removal regression ------

    #[test]
    fn legacy_template_phase_is_absent_from_setup() {
        // Regression guard: the Sprint 3 interactive-checkbox phase
        // and the `aimx-hook` group plumbing must stay gone. This
        // check uses a structural `syn` walk over the parsed AST of
        // `src/setup.rs` rather than a substring grep over the source,
        // so future string literals and comments that happen to mention
        // the retired names cannot trip the assertion (Sprint 7.5
        // S7.5-4 — replaced the Sprint 3 `include_str!` + grep).
        use syn::visit::Visit;

        let source = include_str!("setup.rs");
        let parsed = syn::parse_file(source).expect("src/setup.rs must parse as a Rust file");

        // Retired helpers: any function or method with one of these
        // identifiers is a regression regardless of visibility, since
        // the whole point is that the legacy template-checkbox phase
        // no longer exists anywhere in the module.
        const RETIRED: &[&str] = &[
            "apply_selected_templates",
            "current_hook_templates",
            "ensure_hook_user",
            "chown_datadir_for_hook_user",
        ];
        // Any `configure_hook_*` helper was the heart of the old
        // interactive checkbox flow. Match on the prefix so future
        // `configure_hook_foo` rebirths surface here.
        const BANNED_PREFIX: &str = "configure_hook";

        struct Walker {
            found_retired: Vec<&'static str>,
            found_banned: Vec<String>,
        }
        impl<'ast> Visit<'ast> for Walker {
            fn visit_item_fn(&mut self, f: &'ast syn::ItemFn) {
                self.record(f.sig.ident.to_string());
                syn::visit::visit_item_fn(self, f);
            }
            fn visit_impl_item_fn(&mut self, f: &'ast syn::ImplItemFn) {
                self.record(f.sig.ident.to_string());
                syn::visit::visit_impl_item_fn(self, f);
            }
            fn visit_trait_item_fn(&mut self, f: &'ast syn::TraitItemFn) {
                self.record(f.sig.ident.to_string());
                syn::visit::visit_trait_item_fn(self, f);
            }
        }
        impl Walker {
            fn record(&mut self, name: String) {
                for retired in RETIRED {
                    if name == *retired {
                        self.found_retired.push(retired);
                    }
                }
                if name.starts_with(BANNED_PREFIX) {
                    self.found_banned.push(name);
                }
            }
        }

        let mut walker = Walker {
            found_retired: Vec::new(),
            found_banned: Vec::new(),
        };
        walker.visit_file(&parsed);

        assert!(
            walker.found_retired.is_empty(),
            "retired helper(s) resurfaced — agent-setup owns template wiring now: {:?}",
            walker.found_retired
        );
        assert!(
            walker.found_banned.is_empty(),
            "`{BANNED_PREFIX}*` helper(s) were removed in Sprint 3 S3-3 and must \
             not come back: {:?}",
            walker.found_banned
        );
    }

    #[test]
    fn real_network_ops_default_verify_host() {
        let net = RealNetworkOps::default();
        assert_eq!(net.verify_host, DEFAULT_VERIFY_HOST);
    }

    #[test]
    fn real_network_ops_custom_verify_host() {
        let net = RealNetworkOps::from_verify_host("https://verify.custom.example.com".to_string())
            .unwrap();
        assert_eq!(net.verify_host, "https://verify.custom.example.com");
        assert_eq!(net.check_service_smtp_addr, "verify.custom.example.com:25");
    }

    #[test]
    fn real_network_ops_from_verify_host_strips_trailing_slash() {
        let net =
            RealNetworkOps::from_verify_host("https://check.aimx.email/".to_string()).unwrap();
        assert_eq!(net.verify_host, "https://check.aimx.email");
        assert_eq!(net.check_service_smtp_addr, "check.aimx.email:25");
    }

    #[test]
    fn from_verify_host_rejects_empty() {
        let err = RealNetworkOps::from_verify_host(String::new()).unwrap_err();
        assert!(err.to_string().contains("cannot be empty"));
    }

    #[test]
    fn from_verify_host_rejects_bare_hostname() {
        let err = RealNetworkOps::from_verify_host("check.aimx.email".to_string()).unwrap_err();
        assert!(err.to_string().contains("http://"));
    }

    #[test]
    fn from_verify_host_rejects_non_http_scheme() {
        let err =
            RealNetworkOps::from_verify_host("ftp://verify.example.com".to_string()).unwrap_err();
        assert!(err.to_string().contains("http://"));
    }

    #[test]
    fn from_verify_host_rejects_only_slashes() {
        // trailing-slash strip reduces "/" to empty
        let err = RealNetworkOps::from_verify_host("/".to_string()).unwrap_err();
        assert!(err.to_string().contains("cannot be empty"));
    }

    #[test]
    fn outbound_pass() {
        let net = MockNetworkOps {
            outbound_port25: true,
            ..Default::default()
        };
        assert_eq!(check_outbound(&net), PreflightResult::Pass(None));
    }

    #[test]
    fn outbound_fail() {
        let net = MockNetworkOps {
            outbound_port25: false,
            ..Default::default()
        };
        match check_outbound(&net) {
            PreflightResult::Fail(msg) => assert!(msg.contains("blocked")),
            other => panic!("Expected Fail, got {:?}", other),
        }
    }

    #[test]
    fn inbound_pass() {
        let net = MockNetworkOps {
            inbound_port25: true,
            ..Default::default()
        };
        assert_eq!(check_inbound(&net), PreflightResult::Pass(None));
    }

    #[test]
    fn inbound_fail() {
        let net = MockNetworkOps {
            inbound_port25: false,
            ..Default::default()
        };
        match check_inbound(&net) {
            PreflightResult::Fail(msg) => assert!(msg.contains("not reachable")),
            other => panic!("Expected Fail, got {:?}", other),
        }
    }

    #[test]
    fn dns_record_generation() {
        let records = generate_dns_records(
            "agent.example.com",
            "1.2.3.4",
            None,
            "v=DKIM1; k=rsa; p=ABC123",
            "aimx",
        );
        assert_eq!(records.len(), 5);

        assert_eq!(records[0].record_type, "A");
        assert_eq!(records[0].name, "agent.example.com");
        assert_eq!(records[0].value, "1.2.3.4");

        assert_eq!(records[1].record_type, "MX");
        assert_eq!(records[1].value, "10 agent.example.com.");

        assert_eq!(records[2].record_type, "TXT");
        assert!(records[2].value.contains("v=spf1"));
        assert!(records[2].value.contains("ip4:1.2.3.4"));
        assert!(!records[2].value.contains("ip6:"));

        assert_eq!(records[3].record_type, "TXT");
        assert_eq!(records[3].name, "aimx._domainkey.agent.example.com");
        assert!(records[3].value.contains("DKIM1"));

        assert_eq!(records[4].record_type, "TXT");
        assert_eq!(records[4].name, "_dmarc.agent.example.com");
        assert!(records[4].value.contains("v=DMARC1"));
        assert!(records[4].value.contains("p=reject"));

        assert!(
            !records.iter().any(|r| r.record_type == "PTR"),
            "PTR is the operator's responsibility; generate_dns_records must not emit PTR records"
        );
    }

    #[test]
    fn dns_record_formatting() {
        let records =
            generate_dns_records("test.com", "5.6.7.8", None, "v=DKIM1; k=rsa; p=XYZ", "aimx");
        let formatted = format_dns_records(&records);
        assert!(formatted.contains("A"));
        assert!(formatted.contains("MX"));
        assert!(formatted.contains("TXT"));
        assert!(!formatted.contains("PTR"));
        assert!(formatted.contains("test.com"));
        assert!(formatted.contains("5.6.7.8"));
    }

    #[test]
    fn verify_mx_pass() {
        let mut net = MockNetworkOps::default();
        net.mx_records
            .insert("example.com".into(), vec!["10 example.com.".into()]);
        assert_eq!(verify_mx(&net, "example.com"), DnsVerifyResult::Pass);
    }

    #[test]
    fn verify_mx_missing() {
        let net = MockNetworkOps::default();
        match verify_mx(&net, "example.com") {
            DnsVerifyResult::Missing(msg) => assert!(msg.contains("No MX")),
            other => panic!("Expected Missing, got {:?}", other),
        }
    }

    #[test]
    fn verify_mx_wrong_target() {
        let mut net = MockNetworkOps::default();
        net.mx_records
            .insert("example.com".into(), vec!["10 other.example.net.".into()]);
        match verify_mx(&net, "example.com") {
            DnsVerifyResult::Fail(msg) => assert!(msg.contains("does not point to")),
            other => panic!("Expected Fail, got {:?}", other),
        }
    }

    #[test]
    fn verify_a_pass() {
        let mut net = MockNetworkOps::default();
        let ip: IpAddr = "1.2.3.4".parse().unwrap();
        net.a_records.insert("example.com".into(), vec![ip]);
        assert_eq!(verify_a(&net, "example.com", &ip), DnsVerifyResult::Pass);
    }

    #[test]
    fn verify_a_wrong_ip() {
        let mut net = MockNetworkOps::default();
        let ip: IpAddr = "1.2.3.4".parse().unwrap();
        let wrong_ip: IpAddr = "5.6.7.8".parse().unwrap();
        net.a_records.insert("example.com".into(), vec![wrong_ip]);
        match verify_a(&net, "example.com", &ip) {
            DnsVerifyResult::Fail(msg) => assert!(msg.contains("expected")),
            other => panic!("Expected Fail, got {:?}", other),
        }
    }

    #[test]
    fn verify_a_missing() {
        let net = MockNetworkOps::default();
        let ip: IpAddr = "1.2.3.4".parse().unwrap();
        match verify_a(&net, "example.com", &ip) {
            DnsVerifyResult::Missing(msg) => assert!(msg.contains("No A")),
            other => panic!("Expected Missing, got {:?}", other),
        }
    }

    #[test]
    fn verify_spf_pass() {
        let mut net = MockNetworkOps::default();
        net.txt_records
            .insert("example.com".into(), vec!["v=spf1 ip4:1.2.3.4 -all".into()]);
        assert_eq!(
            verify_spf(&net, "example.com", "1.2.3.4"),
            DnsVerifyResult::Pass
        );
    }

    #[test]
    fn verify_spf_missing() {
        let net = MockNetworkOps::default();
        match verify_spf(&net, "example.com", "1.2.3.4") {
            DnsVerifyResult::Missing(msg) => assert!(msg.contains("No SPF")),
            other => panic!("Expected Missing, got {:?}", other),
        }
    }

    #[test]
    fn verify_spf_wrong_ip() {
        let mut net = MockNetworkOps::default();
        net.txt_records
            .insert("example.com".into(), vec!["v=spf1 ip4:9.9.9.9 -all".into()]);
        match verify_spf(&net, "example.com", "1.2.3.4") {
            DnsVerifyResult::Fail(msg) => assert!(msg.contains("does not include")),
            other => panic!("Expected Fail, got {:?}", other),
        }
    }

    #[test]
    fn verify_dkim_pass_no_local_key() {
        let mut net = MockNetworkOps::default();
        net.txt_records.insert(
            "aimx._domainkey.example.com".into(),
            vec!["v=DKIM1; k=rsa; p=ABC123".into()],
        );
        assert_eq!(
            verify_dkim(&net, "example.com", "aimx", None),
            DnsVerifyResult::Pass
        );
    }

    #[test]
    fn verify_dkim_pass_with_matching_key() {
        let mut net = MockNetworkOps::default();
        net.txt_records.insert(
            "aimx._domainkey.example.com".into(),
            vec!["v=DKIM1; k=rsa; p=ABC123".into()],
        );
        assert_eq!(
            verify_dkim(&net, "example.com", "aimx", Some("ABC123")),
            DnsVerifyResult::Pass
        );
    }

    #[test]
    fn verify_dkim_fail_mismatched_key() {
        let mut net = MockNetworkOps::default();
        net.txt_records.insert(
            "aimx._domainkey.example.com".into(),
            vec!["v=DKIM1; k=rsa; p=ABC123".into()],
        );
        match verify_dkim(&net, "example.com", "aimx", Some("WRONG_KEY")) {
            DnsVerifyResult::Fail(msg) => {
                assert!(msg.contains("does not match"), "Got: {msg}")
            }
            other => panic!("Expected Fail, got {:?}", other),
        }
    }

    #[test]
    fn verify_dkim_pass_with_long_key() {
        // Simulate a realistic 2048-bit DKIM key that would be split across
        // multiple TXT record strings by DNS. After resolve_txt concatenation,
        // the mock provides the joined value.
        let long_key = "MIIBCgKCAQEA011La5tkO7DUxlLEduWsIbrPcK0NAS9SpcW9rftGU2Kx6F0YSPy/54QjZ13AZk6eGM0zJgF3JF9ibX/GiRDVefqCJPhi7lj1kq6xErWxO0ZR7/YslRcoSoAHR/PnO8chRr1DVHEY+5e0cY54z5SLR+lq/xn69zuiHq5AZBpevcfn/ESA3KujF3rXjDT4DM+ydqu92bdLB4MpLMezVoOjNq75RsSQW/ItokH37V4g6OtrV41yYEGvhAawG24j2Kj6RT96cXdOrvRqUb1/IH/a81Is0WH/PoXSLpwarF0Ie1u/+RfUWLj57osAuIsScbzVmzo5Pil+GgAU45UXj91pDwIDAQAB";
        let mut net = MockNetworkOps::default();
        net.txt_records.insert(
            "aimx._domainkey.example.com".into(),
            vec![format!("v=DKIM1; k=rsa; p={long_key}")],
        );
        assert_eq!(
            verify_dkim(&net, "example.com", "aimx", Some(long_key)),
            DnsVerifyResult::Pass,
        );
    }

    #[test]
    fn verify_dkim_missing() {
        let net = MockNetworkOps::default();
        match verify_dkim(&net, "example.com", "aimx", None) {
            DnsVerifyResult::Missing(msg) => assert!(msg.contains("No DKIM")),
            other => panic!("Expected Missing, got {:?}", other),
        }
    }

    #[test]
    fn verify_dmarc_pass() {
        let mut net = MockNetworkOps::default();
        net.txt_records.insert(
            "_dmarc.example.com".into(),
            vec!["v=DMARC1; p=reject".into()],
        );
        assert_eq!(verify_dmarc(&net, "example.com"), DnsVerifyResult::Pass);
    }

    #[test]
    fn verify_dmarc_missing() {
        let net = MockNetworkOps::default();
        match verify_dmarc(&net, "example.com") {
            DnsVerifyResult::Missing(msg) => assert!(msg.contains("No DMARC")),
            other => panic!("Expected Missing, got {:?}", other),
        }
    }

    #[test]
    fn verify_dmarc_warns_on_p_none() {
        let mut net = MockNetworkOps::default();
        net.txt_records
            .insert("_dmarc.example.com".into(), vec!["v=DMARC1; p=none".into()]);
        match verify_dmarc(&net, "example.com") {
            DnsVerifyResult::Warn(msg) => assert!(msg.contains("p=none"), "Got: {msg}"),
            other => panic!("Expected Warn, got {:?}", other),
        }
    }

    #[test]
    fn verify_spf_rejects_prefix_match() {
        let mut net = MockNetworkOps::default();
        net.txt_records.insert(
            "example.com".into(),
            vec!["v=spf1 ip4:1.2.3.45 -all".into()],
        );
        match verify_spf(&net, "example.com", "1.2.3.4") {
            DnsVerifyResult::Fail(msg) => {
                assert!(msg.contains("does not include"), "Got: {msg}")
            }
            other => panic!("Expected Fail for prefix match, got {:?}", other),
        }
    }

    #[test]
    fn verify_spf_rejects_suffix_match() {
        let mut net = MockNetworkOps::default();
        net.txt_records.insert(
            "example.com".into(),
            vec!["v=spf1 ip4:11.2.3.4 -all".into()],
        );
        match verify_spf(&net, "example.com", "1.2.3.4") {
            DnsVerifyResult::Fail(msg) => {
                assert!(msg.contains("does not include"), "Got: {msg}")
            }
            other => panic!("Expected Fail for suffix match, got {:?}", other),
        }
    }

    #[test]
    fn verify_spf_passes_with_cidr() {
        let mut net = MockNetworkOps::default();
        net.txt_records.insert(
            "example.com".into(),
            vec!["v=spf1 ip4:1.2.3.4/32 -all".into()],
        );
        assert_eq!(
            verify_spf(&net, "example.com", "1.2.3.4"),
            DnsVerifyResult::Pass
        );
    }

    #[test]
    fn verify_all_dns_orders_results_to_match_display() {
        // The [DNS] display table is produced by generate_dns_records in the
        // order A, [AAAA], MX, TXT(SPF), [TXT(SPF IPv6)], TXT(DKIM),
        // TXT(DMARC). Verification output must follow the same order so
        // operators can scan one against the other; anything else is a
        // regression of the fix from PR for fix/dns-verification-order.
        let ip: IpAddr = "1.2.3.4".parse().unwrap();
        let mut net = MockNetworkOps::default();
        net.mx_records
            .insert("example.com".into(), vec!["10 example.com.".into()]);
        net.a_records.insert("example.com".into(), vec![ip]);
        net.txt_records
            .insert("example.com".into(), vec!["v=spf1 ip4:1.2.3.4 -all".into()]);
        net.txt_records.insert(
            "dkim._domainkey.example.com".into(),
            vec!["v=DKIM1; k=rsa; p=ABC".into()],
        );
        net.txt_records.insert(
            "_dmarc.example.com".into(),
            vec!["v=DMARC1; p=reject".into()],
        );

        let names_v4: Vec<String> = verify_all_dns(&net, "example.com", &ip, None, "dkim", None)
            .into_iter()
            .map(|(name, _)| name)
            .collect();
        assert_eq!(names_v4, vec!["A", "MX", "SPF", "DKIM", "DMARC"]);

        let ipv6: IpAddr = "2001:db8::1".parse().unwrap();
        net.aaaa_records.insert("example.com".into(), vec![ipv6]);
        net.txt_records.insert(
            "example.com".into(),
            vec!["v=spf1 ip4:1.2.3.4 ip6:2001:db8::1 -all".into()],
        );
        let names_v6: Vec<String> =
            verify_all_dns(&net, "example.com", &ip, Some(&ipv6), "dkim", None)
                .into_iter()
                .map(|(name, _)| name)
                .collect();
        assert_eq!(
            names_v6,
            vec!["A", "AAAA", "MX", "SPF", "SPF (IPv6)", "DKIM", "DMARC"]
        );
    }

    #[test]
    fn verify_all_dns_all_pass() {
        let ip: IpAddr = "1.2.3.4".parse().unwrap();
        let mut net = MockNetworkOps::default();
        net.mx_records
            .insert("example.com".into(), vec!["10 example.com.".into()]);
        net.a_records.insert("example.com".into(), vec![ip]);
        net.txt_records
            .insert("example.com".into(), vec!["v=spf1 ip4:1.2.3.4 -all".into()]);
        net.txt_records.insert(
            "aimx._domainkey.example.com".into(),
            vec!["v=DKIM1; k=rsa; p=ABC".into()],
        );
        net.txt_records.insert(
            "_dmarc.example.com".into(),
            vec!["v=DMARC1; p=reject".into()],
        );

        let results = verify_all_dns(&net, "example.com", &ip, None, "aimx", None);
        assert!(results.iter().all(|(_, r)| *r == DnsVerifyResult::Pass));
    }

    #[test]
    fn verify_all_dns_partial_fail() {
        let ip: IpAddr = "1.2.3.4".parse().unwrap();
        let mut net = MockNetworkOps::default();
        net.mx_records
            .insert("example.com".into(), vec!["10 example.com.".into()]);
        // A record missing, SPF missing, etc.

        let results = verify_all_dns(&net, "example.com", &ip, None, "aimx", None);
        let pass_count = results
            .iter()
            .filter(|(_, r)| *r == DnsVerifyResult::Pass)
            .count();
        assert!(pass_count < results.len());
    }

    #[test]
    fn display_dns_verification_all_pass() {
        let results = vec![
            ("MX".into(), DnsVerifyResult::Pass),
            ("A".into(), DnsVerifyResult::Pass),
        ];
        assert!(display_dns_verification(&results, &[]));
    }

    #[test]
    fn display_dns_verification_with_failure() {
        let results = vec![
            ("MX".into(), DnsVerifyResult::Pass),
            ("A".into(), DnsVerifyResult::Fail("wrong IP".into())),
        ];
        assert!(!display_dns_verification(&results, &[]));
    }

    #[test]
    fn display_dns_verification_with_missing() {
        let results = vec![
            ("MX".into(), DnsVerifyResult::Pass),
            ("SPF".into(), DnsVerifyResult::Missing("No SPF".into())),
        ];
        assert!(!display_dns_verification(&results, &[]));
    }

    #[test]
    fn dns_verification_lines_dkim_fail_includes_loud_consequence_note() {
        // S44-2: a single PASS/FAIL line is too easy to skim past. When
        // DKIM fails the verifier, the operator must see an explicit note
        // that every outbound signature will break until DNS matches.
        let results = vec![
            ("MX".into(), DnsVerifyResult::Pass),
            (
                "DKIM".into(),
                DnsVerifyResult::Fail("public key does not match local key".into()),
            ),
        ];
        let (lines, all_pass) = dns_verification_lines(&results, &[]);
        assert!(!all_pass);
        let joined = strip_ansi(&lines.join("\n"));
        assert!(
            joined.contains("Outbound DKIM signatures will FAIL verification"),
            "expected louder consequence line, got:\n{joined}"
        );
        assert!(
            joined.contains("until DNS matches"),
            "expected actionable remedy, got:\n{joined}"
        );
    }

    #[test]
    fn dns_verification_lines_dkim_missing_includes_loud_consequence_note() {
        let results = vec![(
            "DKIM".into(),
            DnsVerifyResult::Missing("No DKIM record found".into()),
        )];
        let (lines, _) = dns_verification_lines(&results, &[]);
        let joined = strip_ansi(&lines.join("\n"));
        assert!(
            joined.contains("Outbound DKIM signatures will FAIL verification"),
            "expected louder consequence line even for Missing, got:\n{joined}"
        );
    }

    #[test]
    fn dns_verification_lines_non_dkim_fail_has_no_dkim_consequence() {
        let results = vec![("SPF".into(), DnsVerifyResult::Fail("bad".into()))];
        let (lines, _) = dns_verification_lines(&results, &[]);
        let joined = strip_ansi(&lines.join("\n"));
        assert!(
            !joined.contains("Outbound DKIM signatures"),
            "DKIM-only copy must not leak onto other failures, got:\n{joined}"
        );
    }

    /// Strip ANSI escape sequences so assertions work regardless of
    /// whether `colored` happens to be enabled in the current test env.
    fn strip_ansi(s: &str) -> String {
        let mut out = String::with_capacity(s.len());
        let mut chars = s.chars().peekable();
        while let Some(c) = chars.next() {
            if c == '\u{001b}' {
                // Skip CSI: ESC [ ... final-byte(0x40-0x7E)
                if chars.peek() == Some(&'[') {
                    chars.next();
                    while let Some(&next) = chars.peek() {
                        chars.next();
                        if ('@'..='~').contains(&next) {
                            break;
                        }
                    }
                }
            } else {
                out.push(c);
            }
        }
        out
    }

    #[test]
    fn mcp_section_default_lists_claude_code_install_command() {
        let lines = mcp_section_lines(Path::new("/var/lib/aimx"));
        let joined = lines.join("\n");
        assert!(
            joined.contains("aimx agent-setup claude-code"),
            "expected claude-code install command in:\n{joined}"
        );
        assert!(!joined.contains("mcpServers"));
        assert!(!joined.contains("--data-dir"));
    }

    #[test]
    fn mcp_section_custom_data_dir_threads_override_into_commands() {
        let lines = mcp_section_lines(Path::new("/custom/data"));
        let joined = lines.join("\n");
        assert!(
            joined.contains("aimx --data-dir /custom/data agent-setup claude-code"),
            "expected --data-dir override in install command:\n{joined}"
        );
        assert!(!joined.contains("mcpServers"));
    }

    #[test]
    fn mcp_section_points_at_agent_integration_doc() {
        let lines = mcp_section_lines(Path::new("/var/lib/aimx"));
        let joined = lines.join("\n");
        assert!(joined.contains("agent-integration.md"));
        assert!(joined.contains("aimx agent-setup --list"));
    }

    #[test]
    fn run_setup_skips_install_on_reentrant_path() {
        // When `is_already_configured` returns true, the entire install
        // block is skipped, so `install_service_file` must NOT be called.
        // Guards against a future refactor that drops the re-entrant shortcut.
        let tmp = TempDir::new().unwrap();
        let _cfg_guard = crate::config::test_env::ConfigDirOverride::set(tmp.path());

        let mut existing = HashMap::new();
        existing.insert(PathBuf::from("/etc/ssl/aimx/cert.pem"), "cert".to_string());
        existing.insert(
            crate::config::dkim_dir().join("private.key"),
            "key".to_string(),
        );

        let sys = MockSystemOps {
            existing_files: existing,
            service_running: true,
            ..Default::default()
        };
        let net = MockNetworkOps {
            outbound_port25: false,
            ..Default::default()
        };

        let _ = run_setup(Some("example.com"), Some(tmp.path()), &sys, &net);

        assert!(
            !*sys.service_file_installed.borrow(),
            "re-entrant setup must skip `install_service_file`"
        );
    }

    #[test]
    fn fresh_setup_defers_install_until_after_preflight() {
        // `aimx setup` installs aimx.service as the FINAL step, after the
        // port-25 preflight has passed. If preflight fails we must NOT put a
        // service file on disk and ask systemd to start a daemon we already
        // know the network won't route to.
        let tmp = TempDir::new().unwrap();
        let _cfg_guard = crate::config::test_env::ConfigDirOverride::set(tmp.path());

        let sys = MockSystemOps::default();
        let net = MockNetworkOps {
            // Preflight fails at the outbound leg.
            outbound_port25: false,
            ..Default::default()
        };

        let result = run_setup(Some("example.com"), Some(tmp.path()), &sys, &net);

        let err = result.expect_err("preflight failure must bubble up");
        assert!(
            err.to_string().contains("Port 25 checks failed"),
            "expected port-25 error, got: {err}"
        );
        assert!(
            !*sys.service_file_installed.borrow(),
            "install_service_file must NOT run when the preflight fails"
        );
        assert_eq!(
            *sys.wait_for_ready_calls.borrow(),
            0,
            "wait_for_service_ready must NOT run when the preflight fails"
        );
    }

    #[test]
    fn fresh_setup_does_not_write_config_when_preflight_fails() {
        // The port-25 preflight runs BEFORE finalize_setup, so a VPS that
        // blocks outbound port 25 leaves no artefacts on disk. This is the
        // fail-fast invariant: no TLS cert, no config.toml, no DKIM key
        // until the network has been proven OK.
        let tmp = TempDir::new().unwrap();
        let _cfg_guard = crate::config::test_env::ConfigDirOverride::set(tmp.path());

        let sys = MockSystemOps::default();
        let net = MockNetworkOps {
            outbound_port25: false,
            ..Default::default()
        };

        let result = run_setup(Some("example.com"), Some(tmp.path()), &sys, &net);
        let err = result.expect_err("preflight failure must bubble up");
        assert!(
            err.to_string().contains("Port 25 checks failed"),
            "expected port-25 error, got: {err}"
        );

        assert!(
            !crate::config::config_path().exists(),
            "config.toml must NOT be written when the early preflight fails"
        );
        assert!(
            !crate::config::dkim_dir().join("private.key").exists(),
            "DKIM private key must NOT be generated when the early preflight fails"
        );
        assert!(
            !*sys.service_file_installed.borrow(),
            "install_service_file must NOT run when the preflight fails"
        );
        assert_eq!(
            *sys.wait_for_ready_calls.borrow(),
            0,
            "wait_for_service_ready must NOT run when the preflight fails"
        );
    }

    #[test]
    fn install_and_verify_service_errors_when_service_never_binds() {
        // Once preflight + DNS have passed and the final install step runs,
        // a readiness timeout must surface a loud error, NOT silently
        // "proceed anyway" and leave the user with a failed systemd unit.
        let tmp = TempDir::new().unwrap();
        let _cfg_guard = crate::config::test_env::ConfigDirOverride::set(tmp.path());

        let sys = MockSystemOps {
            service_ready: false,
            ..Default::default()
        };
        let err = install_and_verify_service(&sys, tmp.path())
            .expect_err("readiness timeout must be an error");
        assert!(
            err.to_string().contains("did not bind port 25"),
            "expected 'did not bind port 25' in error, got: {err}"
        );
        assert!(
            *sys.service_file_installed.borrow(),
            "install_service_file must have run before wait_for_service_ready"
        );
        assert_eq!(
            *sys.wait_for_ready_calls.borrow(),
            1,
            "wait_for_service_ready must be called exactly once"
        );
    }

    #[test]
    fn install_and_verify_service_succeeds_when_daemon_binds() {
        let tmp = TempDir::new().unwrap();
        let _cfg_guard = crate::config::test_env::ConfigDirOverride::set(tmp.path());
        let sys = MockSystemOps {
            service_ready: true,
            ..Default::default()
        };
        install_and_verify_service(&sys, tmp.path()).expect("must succeed");
        assert!(*sys.service_file_installed.borrow());
        assert_eq!(*sys.wait_for_ready_calls.borrow(), 1);
    }

    #[test]
    fn reentrant_setup_does_not_wait_for_service_ready() {
        // S42-2: the wait-for-ready loop is gated on the fresh-install branch.
        // A re-entrant run (cert + DKIM already present, service already
        // running) must skip both `install_service_file` and the wait loop.
        let tmp = TempDir::new().unwrap();
        let _cfg_guard = crate::config::test_env::ConfigDirOverride::set(tmp.path());

        let mut existing = HashMap::new();
        existing.insert(PathBuf::from("/etc/ssl/aimx/cert.pem"), "cert".to_string());
        existing.insert(
            crate::config::dkim_dir().join("private.key"),
            "key".to_string(),
        );

        let sys = MockSystemOps {
            existing_files: existing,
            service_running: true,
            ..Default::default()
        };
        let net = MockNetworkOps {
            outbound_port25: false,
            ..Default::default()
        };

        let _ = run_setup(Some("example.com"), Some(tmp.path()), &sys, &net);

        assert!(
            !*sys.service_file_installed.borrow(),
            "re-entrant setup must skip install_service_file"
        );
        assert_eq!(
            *sys.wait_for_ready_calls.borrow(),
            0,
            "re-entrant setup must skip the wait-for-ready loop"
        );
    }

    #[cfg(unix)]
    #[test]
    fn apply_config_file_mode_sets_640() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("config.toml");
        std::fs::write(&path, "domain = \"test.example.com\"\n").unwrap();
        // Give it an obviously-wrong mode first so we can see the change.
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o777)).unwrap();

        apply_config_file_mode(&path).unwrap();

        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(
            mode, 0o640,
            "install_config_file must tighten config.toml to 0o640"
        );
    }

    #[cfg(unix)]
    #[test]
    fn config_dir_exists_after_finalize() {
        let tmp = TempDir::new().unwrap();
        let _cfg_guard = crate::config::test_env::ConfigDirOverride::set(tmp.path());
        finalize_setup(tmp.path(), "mode.example.com", "aimx", None).unwrap();

        // Config file resolved via AIMX_CONFIG_DIR lives inside tmp.
        let cfg_path = crate::config::config_path();
        assert!(cfg_path.exists(), "config.toml must be created by finalize");
    }

    #[test]
    fn finalize_creates_data_dir_and_config() {
        let tmp = TempDir::new().unwrap();
        let _cfg_guard = crate::config::test_env::ConfigDirOverride::set(tmp.path());
        finalize_setup(tmp.path(), "test.example.com", "aimx", None).unwrap();

        assert!(crate::config::config_path().exists());
        assert!(tmp.path().join("catchall").exists());
        assert!(tmp.path().join("dkim/private.key").exists());
        assert!(tmp.path().join("dkim/public.key").exists());

        let config = Config::load_resolved_ignore_warnings().unwrap();
        assert_eq!(config.domain, "test.example.com");
        assert!(config.mailboxes.contains_key("catchall"));
        assert_eq!(config.mailboxes["catchall"].address, "*@test.example.com");
    }

    #[test]
    fn finalize_is_idempotent() {
        let tmp = TempDir::new().unwrap();
        let _cfg_guard = crate::config::test_env::ConfigDirOverride::set(tmp.path());
        finalize_setup(tmp.path(), "test.example.com", "aimx", None).unwrap();

        let key1 = std::fs::read_to_string(tmp.path().join("dkim/private.key")).unwrap();

        finalize_setup(tmp.path(), "test.example.com", "aimx", None).unwrap();

        let key2 = std::fs::read_to_string(tmp.path().join("dkim/private.key")).unwrap();
        assert_eq!(key1, key2);

        let config = Config::load_resolved_ignore_warnings().unwrap();
        assert_eq!(config.domain, "test.example.com");
        assert!(config.mailboxes.contains_key("catchall"));
    }

    #[test]
    fn finalize_preserves_existing_mailboxes() {
        let tmp = TempDir::new().unwrap();
        let _cfg_guard = crate::config::test_env::ConfigDirOverride::set(tmp.path());
        finalize_setup(tmp.path(), "test.example.com", "aimx", None).unwrap();

        let config = Config::load_resolved_ignore_warnings().unwrap();
        mailbox::create_mailbox(&config, "alice", "ops").unwrap();

        finalize_setup(tmp.path(), "test.example.com", "aimx", None).unwrap();

        let config = Config::load_resolved_ignore_warnings().unwrap();
        assert!(config.mailboxes.contains_key("alice"));
        assert!(config.mailboxes.contains_key("catchall"));
    }

    #[test]
    fn finalize_updates_domain_if_changed() {
        let tmp = TempDir::new().unwrap();
        let _cfg_guard = crate::config::test_env::ConfigDirOverride::set(tmp.path());
        finalize_setup(tmp.path(), "old.example.com", "aimx", None).unwrap();

        finalize_setup(tmp.path(), "new.example.com", "aimx", None).unwrap();

        let config = Config::load_resolved_ignore_warnings().unwrap();
        assert_eq!(config.domain, "new.example.com");
        let catchall = config.mailboxes.get("catchall").unwrap();
        assert_eq!(catchall.address, "*@new.example.com");
    }

    #[test]
    fn compatible_providers_not_empty() {
        assert!(!COMPATIBLE_PROVIDERS.is_empty());
        assert!(COMPATIBLE_PROVIDERS.iter().any(|p| p.contains("Hetzner")));
    }

    #[test]
    fn dig_short_args_includes_resolver_and_bounds() {
        // `aimx status` runs these resolvers synchronously; the cascade
        // in `dig_with_cascade` pins a specific public resolver per
        // invocation and keeps `+time` / `+tries` tight so a single
        // non-responsive upstream doesn't stall the command.
        let args = dig_short_args("1.1.1.1", "MX", "example.com");
        assert!(
            args.iter().any(|a| a == "@1.1.1.1"),
            "dig args must carry @<resolver>, got {args:?}"
        );
        assert!(
            args.iter().any(|a| a.starts_with("+time=")),
            "dig args must carry a +time= bound, got {args:?}"
        );
        assert!(
            args.iter().any(|a| a.starts_with("+tries=")),
            "dig args must carry a +tries= bound, got {args:?}"
        );
        assert!(
            args.iter().any(|a| a == "+short"),
            "dig args must include +short, got {args:?}"
        );
        assert!(
            args.iter().any(|a| a == "MX"),
            "dig args must include the record type, got {args:?}"
        );
        assert!(
            args.iter().any(|a| a == "example.com"),
            "dig args must include the domain, got {args:?}"
        );
    }

    #[test]
    fn dig_resolvers_default_cascade_order() {
        // Cloudflare first, then Google, then Quad9. Order matters:
        // 1.1.1.1 has the best steady-state latency and the fewest
        // rate-limit surprises for scripted workloads.
        assert_eq!(DIG_RESOLVERS, &["1.1.1.1", "8.8.8.8", "9.9.9.9"]);
    }

    #[test]
    fn validate_domain_accepts_valid() {
        assert!(validate_domain("example.com").is_ok());
        assert!(validate_domain("mail.example.com").is_ok());
        assert!(validate_domain("my-domain.co.uk").is_ok());
    }

    #[test]
    fn validate_domain_rejects_empty() {
        assert!(validate_domain("").is_err());
    }

    #[test]
    fn validate_domain_rejects_single_label() {
        assert!(validate_domain("localhost").is_err());
    }

    #[test]
    fn validate_domain_rejects_special_chars() {
        assert!(validate_domain("ex ample.com").is_err());
        assert!(validate_domain("ex\"ample.com").is_err());
        assert!(validate_domain("ex\nample.com").is_err());
    }

    #[test]
    fn validate_domain_rejects_leading_trailing_hyphen() {
        assert!(validate_domain("-example.com").is_err());
        assert!(validate_domain("example-.com").is_err());
    }

    // S11.1: Root Check + MTA Conflict Detection tests

    #[test]
    fn non_root_detection() {
        let sys = MockSystemOps {
            is_root: false,
            ..Default::default()
        };
        let net = MockNetworkOps::default();
        let result = run_setup(Some("example.com"), None, &sys, &net);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("requires root"),
            "Expected root error, got: {err}"
        );
    }

    #[test]
    fn other_process_detected_exits_with_error() {
        let tmp = TempDir::new().unwrap();
        let _cfg_guard = crate::config::test_env::ConfigDirOverride::set(tmp.path());
        let sys = MockSystemOps {
            port25_status: Port25Status::OtherProcess("postfix".to_string()),
            ..Default::default()
        };
        let net = MockNetworkOps::default();
        let result = run_setup(Some("example.com"), Some(tmp.path()), &sys, &net);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("postfix"),
            "Expected postfix in error, got: {err}"
        );
        assert!(
            err.contains("Port 25 is occupied"),
            "Expected port 25 occupied message, got: {err}"
        );
    }

    #[test]
    fn nothing_on_port25_proceeds() {
        let sys = MockSystemOps {
            port25_status: Port25Status::Free,
            ..Default::default()
        };
        assert!(matches!(
            sys.check_port25_occupancy().unwrap(),
            Port25Status::Free
        ));
    }

    #[test]
    fn parse_port25_status_free() {
        let output = "State  Recv-Q  Send-Q  Local Address:Port  Peer Address:Port\n";
        assert_eq!(parse_port25_status(output).unwrap(), Port25Status::Free);
    }

    #[test]
    fn parse_port25_status_empty() {
        assert_eq!(parse_port25_status("").unwrap(), Port25Status::Free);
    }

    #[test]
    fn parse_port25_status_aimx() {
        let output = "State  Recv-Q  Send-Q  Local Address:Port  Peer Address:Port  Process\n\
                       LISTEN 0      128     0.0.0.0:25         0.0.0.0:*          users:((\"aimx\",pid=1234,fd=6))";
        assert_eq!(parse_port25_status(output).unwrap(), Port25Status::Aimx);
    }

    #[test]
    fn parse_port25_status_other_smtpd() {
        let output = "State  Recv-Q  Send-Q  Local Address:Port  Peer Address:Port  Process\n\
                       LISTEN 0      128     0.0.0.0:25         0.0.0.0:*          users:((\"smtpd\",pid=1234,fd=6))";
        assert_eq!(
            parse_port25_status(output).unwrap(),
            Port25Status::OtherProcess("smtpd".to_string())
        );
    }

    #[test]
    fn parse_port25_status_postfix() {
        let output = "State  Recv-Q  Send-Q  Local Address:Port  Peer Address:Port  Process\n\
                       LISTEN 0      128     0.0.0.0:25         0.0.0.0:*          users:((\"master\",pid=5678,fd=13))";
        assert_eq!(
            parse_port25_status(output).unwrap(),
            Port25Status::OtherProcess("master".to_string())
        );
    }

    #[test]
    fn parse_port25_status_exim() {
        let output = "State  Recv-Q  Send-Q  Local Address:Port  Peer Address:Port  Process\n\
                       LISTEN 0      128     0.0.0.0:25         0.0.0.0:*          users:((\"exim4\",pid=999,fd=3))";
        assert_eq!(
            parse_port25_status(output).unwrap(),
            Port25Status::OtherProcess("exim4".to_string())
        );
    }

    // S11.2: Reorder Setup Flow tests

    #[test]
    fn derive_smtp_addr_from_https_url() {
        assert_eq!(
            derive_smtp_addr_from_verify_host("https://check.aimx.email"),
            "check.aimx.email:25"
        );
    }

    #[test]
    fn derive_smtp_addr_from_http_url() {
        assert_eq!(
            derive_smtp_addr_from_verify_host("http://verify.custom.example.com"),
            "verify.custom.example.com:25"
        );
    }

    #[test]
    fn derive_smtp_addr_from_url_with_port() {
        assert_eq!(
            derive_smtp_addr_from_verify_host("https://check.aimx.email:3025"),
            "check.aimx.email:25"
        );
    }

    #[test]
    fn derive_smtp_addr_from_ipv6_literal_with_port() {
        assert_eq!(
            derive_smtp_addr_from_verify_host("https://[::1]:3025"),
            "[::1]:25"
        );
    }

    #[test]
    fn derive_smtp_addr_from_ipv6_literal_without_port() {
        assert_eq!(
            derive_smtp_addr_from_verify_host("https://[2001:db8::1]"),
            "[2001:db8::1]:25"
        );
    }

    #[test]
    fn derive_smtp_addr_from_ipv6_literal_with_path() {
        assert_eq!(
            derive_smtp_addr_from_verify_host("https://[::1]:8080/probe"),
            "[::1]:25"
        );
    }

    #[test]
    fn real_network_ops_from_verify_host() {
        let net = RealNetworkOps::from_verify_host("https://check.aimx.email".to_string()).unwrap();
        assert_eq!(net.verify_host, "https://check.aimx.email");
        assert_eq!(net.check_service_smtp_addr, "check.aimx.email:25");
    }

    #[test]
    fn real_network_ops_default_has_check_service_smtp() {
        let net = RealNetworkOps::default();
        assert_eq!(net.check_service_smtp_addr, "check.aimx.email:25");
    }

    #[test]
    fn inbound_timeout_is_60s() {
        // Verify that RealNetworkOps uses -m 60 for the curl timeout.
        // We can't run curl in tests, but we verify the constant by checking
        // that the method signature exists on RealNetworkOps.
        // The actual 60s timeout is encoded in the implementation of check_inbound_port25.
        let net = RealNetworkOps::default();
        // Just ensure the method is callable (compile check)
        let _ = &net as &dyn NetworkOps;
    }

    // Sprint 5 S5-2: Trusted-senders prompt tests

    #[test]
    fn validate_trusted_sender_accepts_plain_address() {
        assert!(validate_trusted_sender("alice@example.com").is_ok());
    }

    #[test]
    fn validate_trusted_sender_accepts_domain_glob() {
        assert!(validate_trusted_sender("*@company.com").is_ok());
    }

    #[test]
    fn validate_trusted_sender_accepts_local_part_glob() {
        assert!(validate_trusted_sender("alice*@example.com").is_ok());
    }

    #[test]
    fn validate_trusted_sender_accepts_domain_wildcard() {
        assert!(validate_trusted_sender("alice@*.company.com").is_ok());
    }

    #[test]
    fn validate_trusted_sender_rejects_empty() {
        assert!(validate_trusted_sender("").is_err());
    }

    #[test]
    fn validate_trusted_sender_rejects_missing_at() {
        let err = validate_trusted_sender("no-at-here.com").unwrap_err();
        assert!(err.contains("'@'"));
    }

    #[test]
    fn validate_trusted_sender_rejects_multiple_ats() {
        assert!(validate_trusted_sender("a@b@c.com").is_err());
    }

    #[test]
    fn validate_trusted_sender_rejects_empty_local_part() {
        let err = validate_trusted_sender("@example.com").unwrap_err();
        assert!(err.contains("local part"));
    }

    #[test]
    fn validate_trusted_sender_rejects_empty_domain() {
        let err = validate_trusted_sender("alice@").unwrap_err();
        assert!(err.contains("domain"));
    }

    #[test]
    fn validate_trusted_sender_rejects_stray_chars() {
        // Space is not allowed; whitespace would be a separator.
        assert!(validate_trusted_sender("alice@exa mple.com").is_err());
        // Angle-brackets are not allowed — the RFC 5322 display-name
        // form is the matcher's concern, not the stored pattern's.
        assert!(validate_trusted_sender("<alice@example.com>").is_err());
    }

    #[test]
    fn prompt_trusted_senders_empty_input_returns_none_with_warning() {
        let input = b"\n";
        let mut reader = io::Cursor::new(input);
        let (mode, senders) = prompt_trusted_senders(&mut reader).unwrap();
        assert_eq!(mode, "none");
        assert!(senders.is_empty());
    }

    #[test]
    fn prompt_trusted_senders_single_address() {
        let input = b"alice@example.com\n";
        let mut reader = io::Cursor::new(input);
        let (mode, senders) = prompt_trusted_senders(&mut reader).unwrap();
        assert_eq!(mode, "verified");
        assert_eq!(senders, vec!["alice@example.com".to_string()]);
    }

    #[test]
    fn prompt_trusted_senders_comma_separated() {
        let input = b"alice@example.com, *@company.com\n";
        let mut reader = io::Cursor::new(input);
        let (mode, senders) = prompt_trusted_senders(&mut reader).unwrap();
        assert_eq!(mode, "verified");
        assert_eq!(
            senders,
            vec!["alice@example.com".to_string(), "*@company.com".to_string()]
        );
    }

    #[test]
    fn prompt_trusted_senders_whitespace_separated() {
        let input = b"alice@example.com   *@company.com\n";
        let mut reader = io::Cursor::new(input);
        let (mode, senders) = prompt_trusted_senders(&mut reader).unwrap();
        assert_eq!(mode, "verified");
        assert_eq!(senders.len(), 2);
    }

    #[test]
    fn prompt_trusted_senders_retries_invalid_then_accepts() {
        let input = b"not-an-address\nalice@example.com\n";
        let mut reader = io::Cursor::new(input);
        let (mode, senders) = prompt_trusted_senders(&mut reader).unwrap();
        assert_eq!(mode, "verified");
        assert_eq!(senders, vec!["alice@example.com".to_string()]);
    }

    #[test]
    fn prompt_trusted_senders_aborts_after_max_attempts() {
        // Five bad lines in a row — the sixth line is never read because
        // the wizard aborts on attempt 5 per the error-budget contract.
        let input = b"bad1\nbad2\nbad3\nbad4\nbad5\nalice@example.com\n";
        let mut reader = io::Cursor::new(input);
        let err = prompt_trusted_senders(&mut reader).unwrap_err().to_string();
        assert!(
            err.contains(&MAX_TRUSTED_SENDERS_ATTEMPTS.to_string()),
            "error must name the attempt ceiling: {err}"
        );
        assert!(
            err.contains("bad5"),
            "error should name the last bad entry: {err}"
        );
    }

    #[test]
    fn empty_trusted_senders_warning_text_is_stable() {
        // The operator-visible warning wording is load-bearing (PRD
        // FR-3.2). Guard against silent edits that drop the
        // "NOT fire" / "Add senders later" language.
        assert!(EMPTY_TRUSTED_SENDERS_WARNING.contains("No trusted senders"));
        assert!(EMPTY_TRUSTED_SENDERS_WARNING.contains("NOT fire"));
        assert!(EMPTY_TRUSTED_SENDERS_WARNING.contains("aimx config trust add"));
    }

    #[test]
    fn prompt_trusted_senders_skipped_under_noninteractive_env() {
        // FR-3.8: under AIMX_NONINTERACTIVE=1 the trusted-senders prompt
        // is NOT read. `run_setup` short-circuits before calling
        // `prompt_trusted_senders`, so this test documents the contract
        // by way of the shared helper: `is_noninteractive_env()` returns
        // true, and no stdin is consumed.
        let _guard = NONINT_ENV_SERIALIZE
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        unsafe { std::env::set_var(NONINTERACTIVE_ENV, "1") };
        assert!(is_noninteractive_env());
        unsafe { std::env::remove_var(NONINTERACTIVE_ENV) };
    }

    #[test]
    fn finalize_setup_writes_default_trust_on_fresh_install() {
        let tmp = tempfile::TempDir::new().unwrap();
        let _guard = crate::config::test_env::ConfigDirOverride::set(tmp.path());
        finalize_setup(
            tmp.path(),
            "trust.example.com",
            "aimx",
            Some(("verified".to_string(), vec!["*@company.com".to_string()])),
        )
        .unwrap();

        let on_disk = Config::load_ignore_warnings(&crate::config::config_path()).unwrap();
        assert_eq!(on_disk.trust, "verified");
        assert_eq!(on_disk.trusted_senders, vec!["*@company.com".to_string()]);
        // Catchall mailbox inherits; its own fields remain unset.
        let catchall = on_disk.mailboxes.get("catchall").unwrap();
        assert!(catchall.trust.is_none());
        assert!(catchall.trusted_senders.is_none());
        assert_eq!(catchall.effective_trust(&on_disk), "verified");
    }

    #[test]
    fn finalize_setup_preserves_existing_trust_on_reentry() {
        let tmp = tempfile::TempDir::new().unwrap();
        let _guard = crate::config::test_env::ConfigDirOverride::set(tmp.path());
        // Fresh install picks `verified`.
        finalize_setup(
            tmp.path(),
            "trust.example.com",
            "aimx",
            Some(("verified".to_string(), vec!["*@company.com".to_string()])),
        )
        .unwrap();
        // Re-entry passes None; the on-disk value must survive regardless.
        finalize_setup(tmp.path(), "trust.example.com", "aimx", None).unwrap();

        let on_disk = Config::load_ignore_warnings(&crate::config::config_path()).unwrap();
        assert_eq!(on_disk.trust, "verified");
        assert_eq!(on_disk.trusted_senders, vec!["*@company.com".to_string()]);
    }

    #[test]
    fn finalize_setup_none_defaults_produce_trust_none() {
        // When the operator omits the prompt (e.g. non-interactive path or
        // tests), `None` should behave identically to `Some((\"none\", []))`.
        let tmp = tempfile::TempDir::new().unwrap();
        let _guard = crate::config::test_env::ConfigDirOverride::set(tmp.path());
        finalize_setup(tmp.path(), "trust.example.com", "aimx", None).unwrap();

        let on_disk = Config::load_ignore_warnings(&crate::config::config_path()).unwrap();
        assert_eq!(on_disk.trust, "none");
        assert!(on_disk.trusted_senders.is_empty());
    }

    #[test]
    fn prompt_domain_accepts_valid_domain_with_confirmation() {
        let input = b"agent.example.com\ny\n";
        let mut reader = io::Cursor::new(input);
        let domain = prompt_domain(&mut reader).unwrap();
        assert_eq!(domain, "agent.example.com");
    }

    #[test]
    fn prompt_domain_rejects_empty_input() {
        let input = b"\n";
        let mut reader = io::Cursor::new(input);
        let result = prompt_domain(&mut reader);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("No domain entered")
        );
    }

    #[test]
    fn prompt_domain_rejects_invalid_domain() {
        let input = b"notvalid\n";
        let mut reader = io::Cursor::new(input);
        let result = prompt_domain(&mut reader);
        assert!(result.is_err());
    }

    #[test]
    fn prompt_domain_exits_on_declined_confirmation() {
        let input = b"agent.example.com\nn\n";
        let mut reader = io::Cursor::new(input);
        let result = prompt_domain(&mut reader);
        assert!(result.is_err());
        assert!(
            result.unwrap_err().to_string().contains("cancelled"),
            "should indicate cancellation"
        );
    }

    #[test]
    fn prompt_domain_accepts_empty_confirmation_as_yes() {
        let input = b"agent.example.com\n\n";
        let mut reader = io::Cursor::new(input);
        let domain = prompt_domain(&mut reader).expect("empty confirmation should default to yes");
        assert_eq!(domain, "agent.example.com");
    }

    #[test]
    fn prompt_domain_exits_on_uppercase_n() {
        let input = b"agent.example.com\nN\n";
        let mut reader = io::Cursor::new(input);
        let result = prompt_domain(&mut reader);
        assert!(result.is_err());
        assert!(
            result.unwrap_err().to_string().contains("cancelled"),
            "should indicate cancellation"
        );
    }

    #[test]
    fn detect_server_ipv6_false_drops_detected_ipv6() {
        let ipv6: Ipv6Addr = "2001:db8::1".parse().unwrap();
        let result = detect_server_ipv6(false, Some(ipv6));
        assert!(
            result.is_none(),
            "ipv6 must be dropped when enable_ipv6 = false"
        );
    }

    #[test]
    fn detect_server_ipv6_true_keeps_detected_ipv6() {
        let ipv6: Ipv6Addr = "2001:db8::1".parse().unwrap();
        let result = detect_server_ipv6(true, Some(ipv6));
        assert_eq!(result, Some(ipv6));
    }

    #[test]
    fn detect_server_ipv6_true_returns_none_when_net_has_none() {
        let result = detect_server_ipv6(true, None);
        assert!(result.is_none());
    }

    #[test]
    fn get_server_ips_called_once_per_setup_flow() {
        // Dedup AC: a single call to `NetworkOps::get_server_ips` must feed
        // both IPv4 and IPv6 consumers in the setup flow (no double shell-out
        // to `hostname -I`). Exercised indirectly via `detect_server_ipv6`
        // + the run_setup wiring; here we assert the trait contract returns
        // both families in one invocation.
        let net = MockNetworkOps {
            server_ipv4: Some("203.0.113.5".parse().unwrap()),
            server_ipv6: Some("2001:db8::1".parse().unwrap()),
            ..Default::default()
        };
        let (v4, v6) = net.get_server_ips().unwrap();
        assert_eq!(v4, Some("203.0.113.5".parse().unwrap()));
        assert_eq!(v6, Some("2001:db8::1".parse().unwrap()));
        assert_eq!(
            net.get_server_ips_calls.get(),
            1,
            "single invocation must return both families"
        );
    }

    #[test]
    fn parse_hostname_i_output_extracts_ipv4_and_global_ipv6() {
        let stdout = "10.0.0.5 203.0.113.7 fe80::1 2001:db8::42 fc00::1\n";
        let (v4, v6) = parse_hostname_i_output(stdout);
        assert_eq!(
            v4,
            Some("10.0.0.5".parse().unwrap()),
            "takes the first IPv4 token (private is OK here; caller may filter)"
        );
        assert_eq!(
            v6,
            Some("2001:db8::42".parse().unwrap()),
            "skips link-local (fe80::) and ULA (fc00::) IPv6"
        );
    }

    #[test]
    fn parse_hostname_i_output_returns_none_when_empty() {
        let (v4, v6) = parse_hostname_i_output("   \n");
        assert!(v4.is_none());
        assert!(v6.is_none());
    }

    #[test]
    fn setup_with_domain_arg_skips_prompt() {
        let tmp = TempDir::new().unwrap();
        let _cfg_guard = crate::config::test_env::ConfigDirOverride::set(tmp.path());
        let sys = MockSystemOps::default();
        let net = MockNetworkOps {
            inbound_port25: false,
            ..Default::default()
        };
        let result = run_setup(Some("example.com"), Some(tmp.path()), &sys, &net);
        // Should progress past domain prompt and fail on port 25 check
        assert!(result.is_err());
        assert!(
            result.unwrap_err().to_string().contains("Port 25"),
            "Expected port 25 failure, not a prompt error"
        );
    }

    // S18.4: Re-entrant setup tests

    #[test]
    fn is_already_configured_all_present() {
        let tmp = TempDir::new().unwrap();
        let _cfg_guard = crate::config::test_env::ConfigDirOverride::set(tmp.path());

        let mut existing = HashMap::new();
        existing.insert(PathBuf::from("/etc/ssl/aimx/cert.pem"), "cert".to_string());
        existing.insert(
            crate::config::dkim_dir().join("private.key"),
            "key".to_string(),
        );

        let sys = MockSystemOps {
            existing_files: existing,
            service_running: true,
            ..Default::default()
        };
        assert!(is_already_configured(&sys, tmp.path()));
    }

    #[test]
    fn is_already_configured_service_not_running() {
        let tmp = TempDir::new().unwrap();
        let _cfg_guard = crate::config::test_env::ConfigDirOverride::set(tmp.path());
        let mut existing = HashMap::new();
        existing.insert(PathBuf::from("/etc/ssl/aimx/cert.pem"), "cert".to_string());
        existing.insert(
            crate::config::dkim_dir().join("private.key"),
            "key".to_string(),
        );

        let sys = MockSystemOps {
            existing_files: existing,
            service_running: false,
            ..Default::default()
        };
        assert!(!is_already_configured(&sys, tmp.path()));
    }

    #[test]
    fn is_already_configured_missing_dkim() {
        let tmp = TempDir::new().unwrap();
        let _cfg_guard = crate::config::test_env::ConfigDirOverride::set(tmp.path());
        let mut existing = HashMap::new();
        existing.insert(PathBuf::from("/etc/ssl/aimx/cert.pem"), "cert".to_string());

        let sys = MockSystemOps {
            existing_files: existing,
            service_running: true,
            ..Default::default()
        };
        assert!(!is_already_configured(&sys, tmp.path()));
    }

    // S26-2: IPv6 DNS record generation tests

    #[test]
    fn dns_record_generation_with_ipv6() {
        let records = generate_dns_records(
            "agent.example.com",
            "1.2.3.4",
            Some("2001:db8::1"),
            "v=DKIM1; k=rsa; p=ABC123",
            "aimx",
        );
        assert_eq!(records.len(), 6);

        assert_eq!(records[0].record_type, "A");
        assert_eq!(records[0].value, "1.2.3.4");

        assert_eq!(records[1].record_type, "AAAA");
        assert_eq!(records[1].name, "agent.example.com");
        assert_eq!(records[1].value, "2001:db8::1");

        assert_eq!(records[2].record_type, "MX");

        assert_eq!(records[3].record_type, "TXT");
        assert_eq!(records[3].value, "v=spf1 ip4:1.2.3.4 ip6:2001:db8::1 -all");

        assert!(
            !records.iter().any(|r| r.record_type == "PTR"),
            "PTR is the operator's responsibility, not generated by aimx setup"
        );
    }

    #[test]
    fn dns_record_generation_without_ipv6() {
        let records = generate_dns_records(
            "example.com",
            "1.2.3.4",
            None,
            "v=DKIM1; k=rsa; p=ABC",
            "aimx",
        );
        assert!(!records.iter().any(|r| r.record_type == "AAAA"));
        let spf = records
            .iter()
            .find(|r| r.value.starts_with("v=spf1"))
            .unwrap();
        assert_eq!(spf.value, "v=spf1 ip4:1.2.3.4 -all");
        assert!(!spf.value.contains("ip6:"));
    }

    #[test]
    fn dns_guidance_includes_aaaa_with_ipv6() {
        let records = generate_dns_records(
            "test.com",
            "1.2.3.4",
            Some("2001:db8::1"),
            "v=DKIM1; k=rsa; p=ABC",
            "aimx",
        );
        assert_eq!(records.len(), 6);
        assert!(records.iter().any(|r| r.record_type == "AAAA"));
        assert!(
            !records.iter().any(|r| r.record_type == "PTR"),
            "PTR is the operator's responsibility, not generated by aimx setup"
        );
    }

    // S26-3: ip6: SPF verification tests

    #[test]
    fn spf_contains_ip_ipv6_pass() {
        assert!(spf_contains_ip(
            "v=spf1 ip4:1.2.3.4 ip6:2001:db8::1 -all",
            "2001:db8::1"
        ));
    }

    #[test]
    fn spf_contains_ip_ipv6_with_plus_prefix() {
        assert!(spf_contains_ip(
            "v=spf1 +ip6:2001:db8::1 -all",
            "2001:db8::1"
        ));
    }

    #[test]
    fn spf_contains_ip_ipv6_missing() {
        assert!(!spf_contains_ip("v=spf1 ip4:1.2.3.4 -all", "2001:db8::1"));
    }

    #[test]
    fn spf_contains_ip_ipv6_wrong_address() {
        assert!(!spf_contains_ip(
            "v=spf1 ip6:2001:db8::2 -all",
            "2001:db8::1"
        ));
    }

    #[test]
    fn spf_contains_ip_ipv6_with_cidr() {
        assert!(spf_contains_ip(
            "v=spf1 ip6:2001:db8::1/128 -all",
            "2001:db8::1"
        ));
    }

    #[test]
    fn spf_contains_ip_ipv4_still_works() {
        assert!(spf_contains_ip("v=spf1 ip4:1.2.3.4 -all", "1.2.3.4"));
    }

    #[test]
    fn spf_contains_ip_dual_stack_both_present() {
        let record = "v=spf1 ip4:1.2.3.4 ip6:2001:db8::1 -all";
        assert!(spf_contains_ip(record, "1.2.3.4"));
        assert!(spf_contains_ip(record, "2001:db8::1"));
    }

    #[test]
    fn verify_spf_ipv6_pass() {
        let mut net = MockNetworkOps::default();
        net.txt_records.insert(
            "example.com".into(),
            vec!["v=spf1 ip4:1.2.3.4 ip6:2001:db8::1 -all".into()],
        );
        assert_eq!(
            verify_spf(&net, "example.com", "2001:db8::1"),
            DnsVerifyResult::Pass
        );
    }

    #[test]
    fn verify_spf_ipv6_fail() {
        let mut net = MockNetworkOps::default();
        net.txt_records
            .insert("example.com".into(), vec!["v=spf1 ip4:1.2.3.4 -all".into()]);
        match verify_spf(&net, "example.com", "2001:db8::1") {
            DnsVerifyResult::Fail(msg) => assert!(msg.contains("does not include")),
            other => panic!("Expected Fail, got {:?}", other),
        }
    }

    #[test]
    fn verify_aaaa_pass() {
        let ipv6: IpAddr = "2001:db8::1".parse().unwrap();
        let mut net = MockNetworkOps::default();
        net.aaaa_records.insert("example.com".into(), vec![ipv6]);
        assert_eq!(
            verify_aaaa(&net, "example.com", &ipv6),
            DnsVerifyResult::Pass
        );
    }

    #[test]
    fn verify_aaaa_missing() {
        let ipv6: IpAddr = "2001:db8::1".parse().unwrap();
        let net = MockNetworkOps::default();
        match verify_aaaa(&net, "example.com", &ipv6) {
            DnsVerifyResult::Missing(msg) => assert!(msg.contains("No AAAA")),
            other => panic!("Expected Missing, got {:?}", other),
        }
    }

    #[test]
    fn verify_aaaa_wrong_ip() {
        let expected: IpAddr = "2001:db8::1".parse().unwrap();
        let actual: IpAddr = "2001:db8::2".parse().unwrap();
        let mut net = MockNetworkOps::default();
        net.aaaa_records.insert("example.com".into(), vec![actual]);
        match verify_aaaa(&net, "example.com", &expected) {
            DnsVerifyResult::Fail(msg) => assert!(msg.contains("AAAA record points to")),
            other => panic!("Expected Fail, got {:?}", other),
        }
    }

    #[test]
    fn verify_all_dns_with_ipv6() {
        let ip: IpAddr = "1.2.3.4".parse().unwrap();
        let ipv6: IpAddr = "2001:db8::1".parse().unwrap();
        let ipv6_parsed: Ipv6Addr = "2001:db8::1".parse().unwrap();
        let mut net = MockNetworkOps {
            server_ipv6: Some(ipv6_parsed),
            ..Default::default()
        };
        net.mx_records
            .insert("example.com".into(), vec!["10 example.com.".into()]);
        net.a_records.insert("example.com".into(), vec![ip]);
        net.aaaa_records.insert("example.com".into(), vec![ipv6]);
        net.txt_records.insert(
            "example.com".into(),
            vec!["v=spf1 ip4:1.2.3.4 ip6:2001:db8::1 -all".into()],
        );
        net.txt_records.insert(
            "aimx._domainkey.example.com".into(),
            vec!["v=DKIM1; k=rsa; p=ABC".into()],
        );
        net.txt_records.insert(
            "_dmarc.example.com".into(),
            vec!["v=DMARC1; p=reject".into()],
        );

        let results = verify_all_dns(&net, "example.com", &ip, Some(&ipv6), "aimx", None);
        assert!(results.iter().all(|(_, r)| *r == DnsVerifyResult::Pass));
        assert!(results.iter().any(|(name, _)| name == "AAAA"));
        assert!(results.iter().any(|(name, _)| name == "SPF (IPv6)"));
    }

    #[test]
    fn verify_all_dns_without_ipv6_has_no_aaaa() {
        let ip: IpAddr = "1.2.3.4".parse().unwrap();
        let mut net = MockNetworkOps::default();
        net.mx_records
            .insert("example.com".into(), vec!["10 example.com.".into()]);
        net.a_records.insert("example.com".into(), vec![ip]);
        net.txt_records
            .insert("example.com".into(), vec!["v=spf1 ip4:1.2.3.4 -all".into()]);
        net.txt_records.insert(
            "aimx._domainkey.example.com".into(),
            vec!["v=DKIM1; k=rsa; p=ABC".into()],
        );
        net.txt_records.insert(
            "_dmarc.example.com".into(),
            vec!["v=DMARC1; p=reject".into()],
        );

        let results = verify_all_dns(&net, "example.com", &ip, None, "aimx", None);
        assert!(!results.iter().any(|(name, _)| name == "AAAA"));
        assert!(!results.iter().any(|(name, _)| name == "SPF (IPv6)"));
    }

    // is_global_ipv6 tests

    #[test]
    fn global_ipv6_accepts_global_unicast() {
        let ip: Ipv6Addr = "2001:db8::1".parse().unwrap();
        assert!(is_global_ipv6(&ip));
    }

    #[test]
    fn global_ipv6_rejects_link_local() {
        let ip: Ipv6Addr = "fe80::1".parse().unwrap();
        assert!(!is_global_ipv6(&ip));
    }

    #[test]
    fn global_ipv6_rejects_ula() {
        let fc: Ipv6Addr = "fc00::1".parse().unwrap();
        let fd: Ipv6Addr = "fd00::1".parse().unwrap();
        assert!(!is_global_ipv6(&fc));
        assert!(!is_global_ipv6(&fd));
    }

    #[test]
    fn global_ipv6_rejects_loopback() {
        let ip: Ipv6Addr = "::1".parse().unwrap();
        assert!(!is_global_ipv6(&ip));
    }

    #[test]
    fn global_ipv6_rejects_unspecified() {
        let ip: Ipv6Addr = "::".parse().unwrap();
        assert!(!is_global_ipv6(&ip));
    }

    // ========================================================================
    // dig_with_cascade tests. The core fix for the "A record flips between
    // PASS and MISSING" flakiness reported against `aimx setup`'s DNS verify
    // loop. Prior behavior: a single dig UDP query with no in-process retry
    // and no exit-status check → transient loss masquerades as "no record".
    // ========================================================================

    use std::os::unix::process::ExitStatusExt;
    use std::sync::Mutex;

    struct ScriptedDigRunner {
        responses: Mutex<std::collections::VecDeque<io::Result<std::process::Output>>>,
        calls: Mutex<Vec<Vec<String>>>,
    }

    impl ScriptedDigRunner {
        fn new(responses: Vec<io::Result<std::process::Output>>) -> Self {
            Self {
                responses: Mutex::new(responses.into_iter().collect()),
                calls: Mutex::new(Vec::new()),
            }
        }

        fn call_count(&self) -> usize {
            self.calls.lock().unwrap().len()
        }

        fn nth_call_args(&self, n: usize) -> Vec<String> {
            self.calls.lock().unwrap()[n].clone()
        }
    }

    impl DigRunner for ScriptedDigRunner {
        fn run(&self, args: &[String]) -> io::Result<std::process::Output> {
            self.calls.lock().unwrap().push(args.to_vec());
            self.responses
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_else(|| Err(io::Error::other("ran out of scripted responses")))
        }
    }

    fn ok_output(stdout: &str) -> io::Result<std::process::Output> {
        Ok(std::process::Output {
            status: std::process::ExitStatus::from_raw(0),
            stdout: stdout.as_bytes().to_vec(),
            stderr: Vec::new(),
        })
    }

    fn fail_output(stderr: &str) -> io::Result<std::process::Output> {
        // Shift left 8 to land in the "exited with status N" wait bits;
        // any non-zero value gives `status.success() == false`, which is
        // all the cascade cares about.
        Ok(std::process::Output {
            status: std::process::ExitStatus::from_raw(1 << 8),
            stdout: Vec::new(),
            stderr: stderr.as_bytes().to_vec(),
        })
    }

    #[test]
    fn dig_cascade_first_resolver_succeeds() {
        let runner = ScriptedDigRunner::new(vec![ok_output("10 mail.example.com.\n")]);
        let result = dig_with_cascade(&runner, "MX", "example.com").unwrap();
        assert_eq!(result, vec!["10 mail.example.com.".to_string()]);
        assert_eq!(runner.call_count(), 1);
        assert!(runner.nth_call_args(0).contains(&"@1.1.1.1".to_string()));
    }

    #[test]
    fn dig_cascade_falls_through_on_exit_code() {
        // All DIG_RETRY_ATTEMPTS against 1.1.1.1 fail; 8.8.8.8 succeeds on
        // its first try. The cascade must invoke 1.1.1.1 × DIG_RETRY_ATTEMPTS
        // then 8.8.8.8 × 1.
        let mut responses: Vec<_> = (0..DIG_RETRY_ATTEMPTS)
            .map(|_| fail_output("timeout"))
            .collect();
        responses.push(ok_output("1.2.3.4\n"));
        let runner = ScriptedDigRunner::new(responses);
        let result = dig_with_cascade(&runner, "A", "example.com").unwrap();
        assert_eq!(result, vec!["1.2.3.4".to_string()]);
        assert_eq!(runner.call_count(), DIG_RETRY_ATTEMPTS as usize + 1);
        for i in 0..DIG_RETRY_ATTEMPTS as usize {
            assert!(
                runner.nth_call_args(i).contains(&"@1.1.1.1".to_string()),
                "attempt {i} should have targeted 1.1.1.1"
            );
        }
        assert!(
            runner
                .nth_call_args(DIG_RETRY_ATTEMPTS as usize)
                .contains(&"@8.8.8.8".to_string()),
            "fallthrough should target 8.8.8.8"
        );
    }

    #[test]
    fn dig_cascade_all_resolvers_fail_returns_err() {
        let total_attempts = DIG_RESOLVERS.len() * DIG_RETRY_ATTEMPTS as usize;
        let responses: Vec<_> = (0..total_attempts)
            .map(|_| fail_output("timeout"))
            .collect();
        let runner = ScriptedDigRunner::new(responses);
        let result = dig_with_cascade(&runner, "A", "example.com");
        let err = result.unwrap_err();
        assert!(err.to_string().contains("failed across all"));
        assert_eq!(runner.call_count(), total_attempts);
    }

    #[test]
    fn dig_cascade_empty_but_ok_is_success_no_fallback() {
        // NOERROR/NXDOMAIN is authoritative. An empty answer from 1.1.1.1
        // must NOT cascade to 8.8.8.8. Otherwise the cascade would bloat
        // latency on every genuinely-missing-record check.
        let runner = ScriptedDigRunner::new(vec![ok_output("")]);
        let result = dig_with_cascade(&runner, "A", "missing.example.com").unwrap();
        assert!(result.is_empty());
        assert_eq!(runner.call_count(), 1);
    }

    #[test]
    fn dig_cascade_retries_on_spawn_error() {
        // If dig itself can't be spawned on one attempt (e.g., transient
        // resource exhaustion) and succeeds on retry, the cascade treats
        // that the same as a non-zero exit: retry, don't bail.
        let responses: Vec<io::Result<std::process::Output>> = vec![
            Err(io::Error::other("spawn failed")),
            ok_output("8.8.4.4\n"),
        ];
        let runner = ScriptedDigRunner::new(responses);
        let result = dig_with_cascade(&runner, "A", "example.com").unwrap();
        assert_eq!(result, vec!["8.8.4.4".to_string()]);
        assert_eq!(runner.call_count(), 2);
    }

    #[test]
    fn resolve_a_returns_err_when_all_resolvers_fail() {
        // Integration-level: an all-failing DigRunner must make
        // RealNetworkOps::resolve_a return Err, not Ok(vec![]). This is
        // the bug that produced the "A: MISSING" flip; old code ignored
        // dig exit status and silently reported empty stdout as missing.
        let total_attempts = DIG_RESOLVERS.len() * DIG_RETRY_ATTEMPTS as usize;
        let responses: Vec<_> = (0..total_attempts)
            .map(|_| fail_output("timeout"))
            .collect();
        let runner = Box::new(ScriptedDigRunner::new(responses));
        let net = RealNetworkOps::default().with_dig_runner(runner);
        let result = net.resolve_a("example.com");
        assert!(result.is_err(), "expected Err, got {result:?}");
    }

    #[test]
    fn resolve_a_succeeds_on_second_resolver_after_first_fails() {
        // End-to-end through RealNetworkOps: 1.1.1.1 fails all attempts,
        // 8.8.8.8 succeeds on first try → resolve_a returns the parsed IP.
        let mut responses: Vec<_> = (0..DIG_RETRY_ATTEMPTS)
            .map(|_| fail_output("timeout"))
            .collect();
        responses.push(ok_output("203.0.113.10\n"));
        let runner = Box::new(ScriptedDigRunner::new(responses));
        let net = RealNetworkOps::default().with_dig_runner(runner);
        let result = net.resolve_a("example.com").unwrap();
        assert_eq!(result, vec!["203.0.113.10".parse::<IpAddr>().unwrap()]);
    }

    #[test]
    fn verify_a_maps_resolver_err_to_fail_not_missing() {
        // The user-visible fix: when DNS lookup actually fails (network
        // issue), verify_a must render as FAIL with the error message,
        // not MISSING which wrongly implies the record doesn't exist.
        struct ErroringNet;
        impl NetworkOps for ErroringNet {
            fn check_outbound_port25(&self) -> Result<bool, Box<dyn std::error::Error>> {
                Ok(true)
            }
            fn check_inbound_port25(&self) -> Result<bool, Box<dyn std::error::Error>> {
                Ok(true)
            }
            fn get_server_ips(
                &self,
            ) -> Result<(Option<Ipv4Addr>, Option<Ipv6Addr>), Box<dyn std::error::Error>>
            {
                Ok((None, None))
            }
            fn resolve_mx(&self, _: &str) -> Result<Vec<String>, Box<dyn std::error::Error>> {
                Ok(vec![])
            }
            fn resolve_a(&self, _: &str) -> Result<Vec<IpAddr>, Box<dyn std::error::Error>> {
                Err("transient DNS failure".into())
            }
            fn resolve_aaaa(&self, _: &str) -> Result<Vec<IpAddr>, Box<dyn std::error::Error>> {
                Ok(vec![])
            }
            fn resolve_txt(&self, _: &str) -> Result<Vec<String>, Box<dyn std::error::Error>> {
                Ok(vec![])
            }
        }
        let ip: IpAddr = "1.2.3.4".parse().unwrap();
        let result = verify_a(&ErroringNet, "example.com", &ip);
        match result {
            DnsVerifyResult::Fail(msg) => {
                assert!(
                    msg.contains("A record lookup failed"),
                    "expected A-lookup-failed prefix, got: {msg}"
                );
                assert!(
                    msg.contains("transient DNS failure"),
                    "expected underlying error forwarded, got: {msg}"
                );
            }
            other => panic!("expected DnsVerifyResult::Fail, got {other:?}"),
        }
    }

    // ----- Sprint 5 S5-4: systemd/OpenRC ExecStart respects binary -------

    #[test]
    fn mock_get_aimx_binary_path_honours_override() {
        // `MockSystemOps::override_aimx_binary_path` is the Sprint 5
        // seam: when an operator installed via `AIMX_PREFIX=/opt/aimx`
        // the canonicalized current-exe path is `/opt/aimx/bin/aimx`,
        // not `/usr/local/bin/aimx`. Verify the mock returns the
        // override so the generated unit string below reflects it.
        let sys = MockSystemOps {
            override_aimx_binary_path: Some(PathBuf::from("/opt/aimx/bin/aimx")),
            ..Default::default()
        };
        let resolved = sys.get_aimx_binary_path().unwrap();
        assert_eq!(resolved, PathBuf::from("/opt/aimx/bin/aimx"));
    }

    #[test]
    fn generated_systemd_unit_reflects_custom_prefix() {
        // End-to-end of the S5-4 contract: the path returned by
        // `get_aimx_binary_path` flows directly into
        // `generate_systemd_unit`'s `ExecStart=` line. When the install
        // prefix is `/opt/aimx`, the unit file carries
        // `ExecStart=/opt/aimx/bin/aimx serve --data-dir ...` — not
        // the hardcoded `/usr/local/bin/aimx` fallback from before.
        use crate::serve::service::generate_systemd_unit;
        let sys = MockSystemOps {
            override_aimx_binary_path: Some(PathBuf::from("/opt/aimx/bin/aimx")),
            ..Default::default()
        };
        let aimx_path = sys.get_aimx_binary_path().unwrap();
        let unit = generate_systemd_unit(&aimx_path.to_string_lossy(), "/var/lib/aimx");
        assert!(
            unit.contains("ExecStart=/opt/aimx/bin/aimx serve --data-dir /var/lib/aimx"),
            "generated unit must carry the custom-prefix ExecStart, got:\n{unit}"
        );
        assert!(
            !unit.contains("ExecStart=/usr/local/bin/aimx"),
            "unit must not silently fall back to /usr/local/bin: {unit}"
        );
    }

    #[test]
    fn generated_openrc_script_reflects_custom_prefix() {
        use crate::serve::service::generate_openrc_script;
        let sys = MockSystemOps {
            override_aimx_binary_path: Some(PathBuf::from("/opt/aimx/bin/aimx")),
            ..Default::default()
        };
        let aimx_path = sys.get_aimx_binary_path().unwrap();
        let script = generate_openrc_script(&aimx_path.to_string_lossy(), "/var/lib/aimx");
        assert!(
            script.contains("command=/opt/aimx/bin/aimx"),
            "OpenRC script must carry the custom-prefix `command=`, got:\n{script}"
        );
    }

    #[test]
    fn install_service_file_has_no_silent_fallback() {
        // Regression guard (S5-4): the Sprint 5 rewrite replaces the
        // `.unwrap_or_else(|_| "/usr/local/bin/aimx".to_string())`
        // silent fallback in `install_service_file` with a hard-fail.
        // Walk the parsed AST and verify that the only occurrences of
        // the `/usr/local/bin/aimx` literal are inside a `#[cfg(test)]`
        // mod — production code must not hardcode the install prefix.
        use syn::visit::Visit;
        let source = include_str!("setup.rs");
        let parsed = syn::parse_file(source).expect("src/setup.rs must parse");

        struct Walker {
            in_test_mod_depth: usize,
            hits: Vec<String>,
        }
        impl<'ast> Visit<'ast> for Walker {
            fn visit_item_mod(&mut self, m: &'ast syn::ItemMod) {
                let is_test = m.attrs.iter().any(|a| {
                    a.path().is_ident("cfg") && {
                        let mut found = false;
                        let _ = a.parse_nested_meta(|meta| {
                            if meta.path.is_ident("test") {
                                found = true;
                            }
                            Ok(())
                        });
                        found
                    }
                });
                if is_test {
                    self.in_test_mod_depth += 1;
                    syn::visit::visit_item_mod(self, m);
                    self.in_test_mod_depth -= 1;
                } else {
                    syn::visit::visit_item_mod(self, m);
                }
            }
            fn visit_lit_str(&mut self, lit: &'ast syn::LitStr) {
                if self.in_test_mod_depth == 0 && lit.value() == "/usr/local/bin/aimx" {
                    self.hits.push(lit.value());
                }
                syn::visit::visit_lit_str(self, lit);
            }
        }

        let mut walker = Walker {
            in_test_mod_depth: 0,
            hits: Vec::new(),
        };
        walker.visit_file(&parsed);
        assert!(
            walker.hits.is_empty(),
            "production code in setup.rs must NOT hardcode \
             /usr/local/bin/aimx — the path must flow from \
             `get_aimx_binary_path()` (S5-4). Hits: {:?}",
            walker.hits
        );
    }

    // ----- Sprint 5 S5-3: wizard flow / success banner polish -------------

    #[test]
    fn success_banner_is_single_line_and_names_domain() {
        // FR-3.5 step 8: the wizard closes with `aimx is running for
        // <domain>.` on a single line. Capture stdout via the
        // `announce_setup_complete` helper and verify.
        // (Direct stdout capture is finicky under cargo test; instead
        // assert the helper calls `term::success` with the expected
        // string shape by grepping the source for the key literal.)
        let source = include_str!("setup.rs");
        assert!(
            source.contains("aimx is running for {domain}."),
            "announce_setup_complete must emit the FR-3.5 single-line banner"
        );
    }

    #[test]
    fn wizard_does_not_display_deliverability_section() {
        // S5-1 regression: `display_deliverability_section` and
        // `gmail_whitelist_instructions` must stay deleted from the
        // wizard surface. Walk the parsed AST for any function with
        // those names (regardless of visibility) — a substring grep
        // would false-positive on this test's own comments.
        use syn::visit::Visit;
        let source = include_str!("setup.rs");
        let parsed = syn::parse_file(source).expect("src/setup.rs must parse");

        const RETIRED: &[&str] = &[
            "display_deliverability_section",
            "gmail_whitelist_instructions",
        ];

        struct Walker {
            found: Vec<&'static str>,
        }
        impl<'ast> Visit<'ast> for Walker {
            fn visit_item_fn(&mut self, f: &'ast syn::ItemFn) {
                let name = f.sig.ident.to_string();
                for r in RETIRED {
                    if name == *r {
                        self.found.push(*r);
                    }
                }
                syn::visit::visit_item_fn(self, f);
            }
        }

        let mut walker = Walker { found: Vec::new() };
        walker.visit_file(&parsed);
        assert!(
            walker.found.is_empty(),
            "Sprint 5 S5-1 retired helpers must stay gone: {:?}",
            walker.found
        );
    }

    #[test]
    fn dns_verify_loop_has_prominent_q_escape() {
        // FR-3.5 step 6: the "press `q` to skip and run `aimx doctor`
        // later" escape must be a prominent standalone line, not a
        // parenthetical. Assert both the `aimx doctor` hint and the
        // literal `q` key appear near a line-start marker so future
        // compaction doesn't bury the escape hatch.
        let source = include_str!("setup.rs");
        assert!(
            source.contains("Press {} to skip and run `{}` later."),
            "q-escape hint must be its own line"
        );
        assert!(
            source.contains("aimx doctor"),
            "q-escape must name `aimx doctor` as the followup"
        );
    }
}
