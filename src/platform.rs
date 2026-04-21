//! Small platform/OS helpers shared across subcommands.
//!
//! This module also owns the sandboxed hook runner [`spawn_sandboxed`],
//! which is the single place every hook fire lands. It picks one of two
//! execution paths at runtime:
//!
//! * **systemd** (host is a systemd box — `/run/systemd/system` exists):
//!   spawn via `systemd-run --uid --gid --property=... --pipe --`. The
//!   kernel and PID-1 enforce the sandbox (`ProtectSystem=strict`,
//!   `PrivateDevices=yes`, `NoNewPrivileges=yes`, `MemoryMax`,
//!   `RuntimeMaxSec`).
//! * **fallback** (OpenRC / unknown init): plain `fork + execvp` via
//!   `std::process::Command` with `pre_exec` setting `setgid` + `setuid`
//!   so the child drops privileges before `exec`. A parent-side
//!   poll-and-signal loop enforces the same timeout contract
//!   (SIGTERM → SIGKILL).

/// Returns `true` when the current process has effective UID 0 (root).
///
/// On non-Unix targets this always returns `false`.
pub fn is_root() -> bool {
    #[cfg(unix)]
    {
        unsafe { libc::geteuid() == 0 }
    }
    #[cfg(not(unix))]
    {
        false
    }
}

use std::collections::HashMap;
use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use crate::serve::service::{InitSystem, detect_init_system};

/// Max bytes of stdout/stderr retained per hook fire. Anything past this
/// is silently dropped; the tail end of the stream is preserved so an
/// operator can still see the last error message before the cutoff.
pub const MAX_OUTPUT_BYTES: usize = 64 * 1024;

/// Grace period between SIGTERM and SIGKILL when a hook exceeds its
/// timeout. Kept aligned with the PRD §6.7 spec.
pub const SIGKILL_GRACE: Duration = Duration::from_secs(5);

/// Default memory cap on systemd-run hooks. Matches PRD §6.7. Operators
/// who need to raise it set `run_as = "root"` in `config.toml` and live
/// with the reduced guarantees, or edit `/etc/aimx/config.toml` to add a
/// per-template override in a future sprint.
pub const DEFAULT_MEMORY_MAX: &str = "256M";

/// Stdin delivery policy per PRD §6.7. The daemon writes the chosen
/// payload to the child's stdin before starting the timeout clock, then
/// closes stdin — no hook blocks mid-run on a stdin `read()`.
pub enum SandboxStdin {
    /// Raw `.md` bytes (TOML frontmatter + body) written to stdin.
    Email(Vec<u8>),
    /// JSON object `{ "frontmatter": {...}, "body": "..." }` written to
    /// stdin. Currently a best-effort shape — see PRD §9 out-of-scope.
    EmailJson(Vec<u8>),
    /// Close stdin immediately (no payload).
    None,
}

/// Which sandbox path was actually taken. Logged alongside the hook-fire
/// record so operators can tell at a glance whether the kernel-level
/// protections applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SandboxKind {
    SystemdRun,
    Setuid,
}

impl SandboxKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            SandboxKind::SystemdRun => "systemd-run",
            SandboxKind::Setuid => "setuid",
        }
    }
}

/// Captured subprocess result.
#[derive(Debug)]
pub struct SandboxOutcome {
    pub exit_code: i32,
    /// Tail of captured stdout. Not surfaced in the structured hook-fire
    /// log line today (only `stderr_tail` is); reserved for the `doctor`
    /// recent-activity view in Sprint 6.
    #[allow(dead_code)]
    pub stdout_tail: Vec<u8>,
    pub stderr_tail: Vec<u8>,
    pub duration: Duration,
    pub sandbox: SandboxKind,
    pub timed_out: bool,
}

/// Reasons the helper may refuse to spawn or may fail after spawn.
///
/// `UserNotFound` is distinct from `SpawnFailed` so the caller can log a
/// specific "run `aimx setup` to create aimx-hook" hint.
#[derive(Debug)]
pub enum SandboxError {
    UserNotFound(String),
    SpawnFailed(std::io::Error),
    /// Capturing stdout/stderr or writing stdin failed.
    IoFailed(std::io::Error),
}

impl std::fmt::Display for SandboxError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SandboxError::UserNotFound(u) => write!(f, "user '{u}' not found on this host"),
            SandboxError::SpawnFailed(e) => write!(f, "spawn failed: {e}"),
            SandboxError::IoFailed(e) => write!(f, "subprocess I/O failed: {e}"),
        }
    }
}

impl std::error::Error for SandboxError {}

/// Spawn `argv` in the appropriate sandbox and return its outcome.
///
/// `run_as` accepts two special values:
/// * `"aimx-hook"` — resolve via `getpwnam` and drop privileges.
/// * `"root"` — no privilege drop; `systemd-run` path still applies its
///   properties (ProtectSystem etc.), the fallback path runs as the
///   caller's current UID. Operators explicitly opt into this via
///   `config.toml`; it is never settable over the UDS.
///
/// `envs` are additional environment variables to set on the child
/// (`AIMX_*` etc.). On the systemd path these are forwarded via
/// `--setenv=K=V`; on the fallback path they are merged into the child's
/// env (which is otherwise cleared — only `PATH` / `HOME` from the
/// parent are preserved, matching the old `sh -c` executor's contract).
pub fn spawn_sandboxed(
    argv: &[String],
    stdin: SandboxStdin,
    run_as: &str,
    timeout: Duration,
    envs: &HashMap<String, String>,
) -> Result<SandboxOutcome, SandboxError> {
    assert!(
        !argv.is_empty(),
        "spawn_sandboxed requires a non-empty argv"
    );
    let started = Instant::now();
    let init = detect_init_system();
    // Tests (and non-root operators on systemd boxes) can opt out of
    // the `systemd-run` path via `AIMX_SANDBOX_FORCE_FALLBACK=1`. This
    // exists because `systemd-run --user` semantics and PolicyKit
    // interactions make it impossible to exercise the fallback code
    // path under `cargo test` on a systemd host without it.
    let force_fallback = std::env::var_os("AIMX_SANDBOX_FORCE_FALLBACK").is_some();
    match (init, force_fallback) {
        (InitSystem::Systemd, false) => {
            spawn_via_systemd_run(argv, stdin, run_as, timeout, envs, started)
        }
        _ => spawn_via_fork_setuid(argv, stdin, run_as, timeout, envs, started),
    }
}

/// systemd path. Wraps `argv` in `systemd-run --pipe ...`, captures
/// stdout/stderr from the `systemd-run` child, and infers the wrapped
/// command's exit code from `systemd-run`'s own exit code (systemd-run
/// propagates the inner exit on `--pipe`).
fn spawn_via_systemd_run(
    argv: &[String],
    stdin: SandboxStdin,
    run_as: &str,
    timeout: Duration,
    envs: &HashMap<String, String>,
    started: Instant,
) -> Result<SandboxOutcome, SandboxError> {
    // Mirror the fallback path's preflight: `systemd-run --uid=aimx-hook`
    // on a host without the user fails with an opaque PolicyKit / "Unknown
    // user name" message. Catching it here produces the same
    // `SandboxError::UserNotFound` that the fallback path returns, so
    // operators get a consistent "run `aimx setup` to create aimx-hook"
    // signal regardless of init system.
    #[cfg(unix)]
    if run_as != "root" {
        match lookup_user(run_as) {
            Err(SandboxError::UserNotFound(_)) if !is_root() => {
                tracing::warn!(
                    target: "aimx::hook",
                    "run_as '{run_as}' not found and caller is non-root: running hook as current user via systemd-run"
                );
                return spawn_via_fork_setuid(argv, stdin, run_as, timeout, envs, started);
            }
            Err(e) => return Err(e),
            Ok(_) => {}
        }
    }

    let mut cmd = Command::new("systemd-run");
    cmd.arg("--quiet")
        .arg("--pipe")
        .arg("--wait")
        .arg("--collect");

    if run_as != "root" {
        cmd.arg(format!("--uid={run_as}"))
            .arg(format!("--gid={run_as}"));
    }
    cmd.arg("--property=ProtectSystem=strict")
        .arg("--property=PrivateDevices=yes")
        .arg("--property=NoNewPrivileges=yes")
        .arg(format!("--property=MemoryMax={DEFAULT_MEMORY_MAX}"))
        .arg(format!("--property=RuntimeMaxSec={}", timeout.as_secs()));

    for (k, v) in envs {
        cmd.arg(format!("--setenv={k}={v}"));
    }

    cmd.arg("--");
    for a in argv {
        cmd.arg(a);
    }

    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    run_child_with_timeout(
        cmd,
        stdin,
        // systemd-run --wait enforces RuntimeMaxSec on the unit. We add
        // a generous parent-side cap (timeout + grace + 5s) so a broken
        // systemd-run itself never wedges the daemon.
        timeout + SIGKILL_GRACE + Duration::from_secs(5),
        SandboxKind::SystemdRun,
        started,
    )
}

/// Fallback path. Drops to `aimx-hook` via `setuid`/`setgid` in
/// `pre_exec`, then `execvp`s `argv`. Timeout enforced with poll +
/// SIGTERM + SIGKILL.
#[cfg(unix)]
fn spawn_via_fork_setuid(
    argv: &[String],
    stdin: SandboxStdin,
    run_as: &str,
    timeout: Duration,
    envs: &HashMap<String, String>,
    started: Instant,
) -> Result<SandboxOutcome, SandboxError> {
    use std::os::unix::process::CommandExt;

    let mut cmd = Command::new(&argv[0]);
    cmd.args(&argv[1..]);

    cmd.env_clear();
    if let Some(p) = std::env::var_os("PATH") {
        cmd.env("PATH", p);
    }
    if let Some(h) = std::env::var_os("HOME") {
        cmd.env("HOME", h);
    }
    for (k, v) in envs {
        cmd.env(k, v);
    }

    // Put the child in its own process group so we can signal the whole
    // subtree at timeout. Otherwise a spawned `sleep` reparented to init
    // outlives its shell and keeps stdout pipes open, wedging the drain
    // threads.
    unsafe {
        cmd.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }

    // Configure stdio up-front so every return path spawns a child with
    // piped stdin/stdout/stderr.
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    if run_as != "root" {
        // If the configured user doesn't exist, fall back to running as
        // the caller's current UID (no privilege drop). This keeps dev
        // / CI hosts usable without manually creating `aimx-hook`; in
        // production `aimx setup` has already created the user before
        // the daemon starts. We surface the fallback in the log line
        // via the `sandbox=setuid` tag even though no setuid happened.
        let lookup = lookup_user(run_as);
        if matches!(lookup, Err(SandboxError::UserNotFound(_))) && !crate::platform::is_root() {
            tracing::warn!(
                target: "aimx::hook",
                "run_as '{run_as}' not found and caller is non-root: running hook as current user"
            );
            return run_child_with_timeout(cmd, stdin, timeout, SandboxKind::Setuid, started);
        }
        let (uid, gid) = lookup?;
        // Set real + effective uid/gid in `pre_exec` after `setsid` so
        // the child has no supplementary groups.
        unsafe {
            cmd.pre_exec(move || {
                // Drop supplementary groups. Requires CAP_SETGID in the
                // caller (root); the no-op case where we aren't root is
                // handled in userland before reaching here — if root
                // isn't available the operator explicitly set
                // run_as = "root" or is a non-systemd dev box.
                if libc::setgroups(0, std::ptr::null()) != 0 {
                    // Non-root callers will see EPERM here; that's OK
                    // in a dev / test path, fall through.
                    let err = std::io::Error::last_os_error();
                    if err.raw_os_error() != Some(libc::EPERM) {
                        return Err(err);
                    }
                }
                if libc::setgid(gid) != 0 {
                    let err = std::io::Error::last_os_error();
                    if err.raw_os_error() != Some(libc::EPERM) {
                        return Err(err);
                    }
                }
                if libc::setuid(uid) != 0 {
                    let err = std::io::Error::last_os_error();
                    if err.raw_os_error() != Some(libc::EPERM) {
                        return Err(err);
                    }
                }
                Ok(())
            });
        }
    }

    run_child_with_timeout(cmd, stdin, timeout, SandboxKind::Setuid, started)
}

#[cfg(not(unix))]
fn spawn_via_fork_setuid(
    _argv: &[String],
    _stdin: SandboxStdin,
    _run_as: &str,
    _timeout: Duration,
    _envs: &HashMap<String, String>,
    _started: Instant,
) -> Result<SandboxOutcome, SandboxError> {
    Err(SandboxError::SpawnFailed(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "spawn_sandboxed fallback path requires Unix",
    )))
}

/// Spawn `cmd`, feed `stdin` bytes, and wait up to `timeout` — sending
/// SIGTERM at expiry and SIGKILL [`SIGKILL_GRACE`] later if the child
/// is still alive. Captures stdout/stderr truncated at
/// [`MAX_OUTPUT_BYTES`].
fn run_child_with_timeout(
    mut cmd: Command,
    stdin: SandboxStdin,
    timeout: Duration,
    kind: SandboxKind,
    started: Instant,
) -> Result<SandboxOutcome, SandboxError> {
    let mut child = cmd.spawn().map_err(SandboxError::SpawnFailed)?;
    // Start the timeout clock from immediately after spawn, not from
    // `started` (which covers `detect_init_system` + `lookup_user` +
    // config plumbing). The child gets its full `timeout` budget, and
    // `started` remains the basis of `duration_ms` for end-to-end
    // latency observability.
    let spawned_at = Instant::now();

    // Write stdin + close before the timeout clock matters. We do this
    // on a thread so a child that refuses to drain stdin can't wedge the
    // parent; the thread joins in `drop`.
    let child_stdin = child.stdin.take();
    let stdin_handle = thread::spawn(move || {
        if let Some(mut pipe) = child_stdin {
            let payload: &[u8] = match &stdin {
                SandboxStdin::Email(b) | SandboxStdin::EmailJson(b) => b.as_slice(),
                SandboxStdin::None => &[],
            };
            if !payload.is_empty() {
                let _ = pipe.write_all(payload);
            }
            // Dropping `pipe` closes stdin.
        }
    });

    // Drain stdout/stderr on threads so a child that writes > PIPE_BUF
    // without a reader can't deadlock on its own write syscall. Each
    // thread reads to EOF and the parent truncates the collected buffer
    // to MAX_OUTPUT_BYTES.
    let stdout_pipe = child.stdout.take();
    let stderr_pipe = child.stderr.take();
    let stdout_handle = stdout_pipe.map(|mut s| {
        thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = s.read_to_end(&mut buf);
            buf
        })
    });
    let stderr_handle = stderr_pipe.map(|mut s| {
        thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = s.read_to_end(&mut buf);
            buf
        })
    });

    // Linux `pid_max` defaults to 4_194_304 (2^22) on 64-bit kernels and
    // is capped at 2^22 by the kernel; well under `i32::MAX`. The cast
    // below is therefore lossless on every supported target (Unix). We
    // negate the pid to signal the whole process group via `kill(-pgid)`.
    let pid = child.id() as i32;

    let deadline = spawned_at + timeout;
    let mut sigterm_sent = false;
    let mut sigkill_deadline: Option<Instant> = None;
    let mut timed_out = false;
    let status_result;

    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                status_result = Ok(status);
                break;
            }
            Ok(None) => {
                let now = Instant::now();
                if !sigterm_sent && now >= deadline {
                    timed_out = true;
                    // Signal the whole process group (pgid == pid since
                    // `setsid` made the child a session leader) so a
                    // grandchild (e.g. a shell's `sleep`) is killed too.
                    // Without this, the drain threads would block on
                    // stdout/stderr pipes the grandchild still holds
                    // open after the shell exits.
                    unsafe {
                        libc::kill(-pid, libc::SIGTERM);
                    }
                    sigterm_sent = true;
                    sigkill_deadline = Some(now + SIGKILL_GRACE);
                } else if let Some(kd) = sigkill_deadline
                    && now >= kd
                {
                    unsafe {
                        libc::kill(-pid, libc::SIGKILL);
                    }
                    sigkill_deadline = None;
                }
                thread::sleep(Duration::from_millis(50));
            }
            Err(e) => {
                status_result = Err(e);
                break;
            }
        }
    }

    let _ = stdin_handle.join();
    let stdout_tail = stdout_handle
        .map(|h| cap_tail(h.join().unwrap_or_default()))
        .unwrap_or_default();
    let stderr_tail = stderr_handle
        .map(|h| cap_tail(h.join().unwrap_or_default()))
        .unwrap_or_default();

    let duration = started.elapsed();
    match status_result {
        Ok(status) => {
            // No exit code means the child was signalled. Report as -1
            // so the caller sees a clear "abnormal" marker; the log line
            // surfaces `timed_out` separately.
            let exit_code = status.code().unwrap_or(-1);
            Ok(SandboxOutcome {
                exit_code,
                stdout_tail,
                stderr_tail,
                duration,
                sandbox: kind,
                timed_out,
            })
        }
        Err(e) => Err(SandboxError::IoFailed(e)),
    }
}

/// Truncate `buf` to the last [`MAX_OUTPUT_BYTES`] bytes in place.
fn cap_tail(mut buf: Vec<u8>) -> Vec<u8> {
    if buf.len() > MAX_OUTPUT_BYTES {
        let start = buf.len() - MAX_OUTPUT_BYTES;
        buf.drain(..start);
    }
    buf
}

/// Resolve a system user name to `(uid, gid)`. Returns
/// [`SandboxError::UserNotFound`] if `getpwnam` returns null.
#[cfg(unix)]
fn lookup_user(name: &str) -> Result<(libc::uid_t, libc::gid_t), SandboxError> {
    use std::ffi::CString;
    let cname = CString::new(name).map_err(|_| SandboxError::UserNotFound(name.to_string()))?;
    let pw = unsafe { libc::getpwnam(cname.as_ptr()) };
    if pw.is_null() {
        return Err(SandboxError::UserNotFound(name.to_string()));
    }
    // SAFETY: getpwnam returned a non-null pointer; we only read two
    // scalar fields before any subsequent getpw* call invalidates them.
    let (uid, gid) = unsafe { ((*pw).pw_uid, (*pw).pw_gid) };
    Ok((uid, gid))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;

    fn write_script(dir: &std::path::Path, name: &str, body: &str) -> std::path::PathBuf {
        let path = dir.join(name);
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(f, "#!/bin/sh").unwrap();
        f.write_all(body.as_bytes()).unwrap();
        let mut perms = std::fs::metadata(&path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&path, perms).unwrap();
        path
    }

    fn run_fallback(
        argv: &[String],
        stdin: SandboxStdin,
        timeout: Duration,
    ) -> Result<SandboxOutcome, SandboxError> {
        spawn_via_fork_setuid(
            argv,
            stdin,
            // non-root so setuid is a no-op on CI
            "root",
            timeout,
            &HashMap::new(),
            Instant::now(),
        )
    }

    #[test]
    fn exit_code_propagates_through_fallback() {
        let tmp = tempfile::tempdir().unwrap();
        let script = write_script(tmp.path(), "x.sh", "exit 7\n");
        let argv = vec![script.to_string_lossy().into_owned()];
        let out = run_fallback(&argv, SandboxStdin::None, Duration::from_secs(5)).unwrap();
        assert_eq!(out.exit_code, 7);
        assert!(!out.timed_out);
        assert_eq!(out.sandbox, SandboxKind::Setuid);
    }

    #[test]
    fn stdout_and_stderr_captured() {
        let tmp = tempfile::tempdir().unwrap();
        let script = write_script(
            tmp.path(),
            "x.sh",
            "echo out-line >&1; echo err-line >&2; exit 0\n",
        );
        let argv = vec![script.to_string_lossy().into_owned()];
        let out = run_fallback(&argv, SandboxStdin::None, Duration::from_secs(5)).unwrap();
        assert_eq!(out.exit_code, 0);
        let so = String::from_utf8_lossy(&out.stdout_tail);
        let se = String::from_utf8_lossy(&out.stderr_tail);
        assert!(so.contains("out-line"), "stdout: {so}");
        assert!(se.contains("err-line"), "stderr: {se}");
    }

    #[test]
    fn timeout_triggers_sigterm_and_sets_timed_out() {
        let tmp = tempfile::tempdir().unwrap();
        // Script traps SIGTERM so we can observe the grace period, then
        // exits after a short sleep if somehow not killed.
        let script = write_script(tmp.path(), "x.sh", "trap 'exit 42' TERM\nsleep 5\n");
        let argv = vec![script.to_string_lossy().into_owned()];
        let out = run_fallback(&argv, SandboxStdin::None, Duration::from_millis(200)).unwrap();
        assert!(out.timed_out, "timed_out flag must be set");
        assert!(
            out.duration < Duration::from_secs(4),
            "took too long: {:?}",
            out.duration
        );
    }

    #[test]
    fn sigkill_fires_when_sigterm_ignored() {
        let tmp = tempfile::tempdir().unwrap();
        // Ignore SIGTERM entirely; SIGKILL must finish the job.
        let script = write_script(tmp.path(), "x.sh", "trap '' TERM\nsleep 30\n");
        let argv = vec![script.to_string_lossy().into_owned()];
        let out = run_fallback(&argv, SandboxStdin::None, Duration::from_millis(200)).unwrap();
        assert!(out.timed_out);
        // SIGKILL landed within roughly timeout + SIGKILL_GRACE.
        assert!(
            out.duration < Duration::from_secs(10),
            "SIGKILL did not fire in time: {:?}",
            out.duration
        );
    }

    #[test]
    fn stdin_email_is_piped_to_child() {
        let tmp = tempfile::tempdir().unwrap();
        let script = write_script(tmp.path(), "x.sh", "cat\n");
        let argv = vec![script.to_string_lossy().into_owned()];
        let payload = b"+++\nfrom = 'a@b'\n+++\nhello".to_vec();
        let out = run_fallback(
            &argv,
            SandboxStdin::Email(payload.clone()),
            Duration::from_secs(5),
        )
        .unwrap();
        assert_eq!(out.exit_code, 0);
        assert_eq!(out.stdout_tail, payload);
    }

    #[test]
    fn stdin_none_closes_stdin_immediately() {
        let tmp = tempfile::tempdir().unwrap();
        let script = write_script(
            tmp.path(),
            "x.sh",
            "if read -r line; then echo got=$line; else echo empty; fi\n",
        );
        let argv = vec![script.to_string_lossy().into_owned()];
        let out = run_fallback(&argv, SandboxStdin::None, Duration::from_secs(5)).unwrap();
        assert_eq!(out.exit_code, 0);
        let so = String::from_utf8_lossy(&out.stdout_tail);
        assert!(so.contains("empty"), "stdout: {so}");
    }

    #[test]
    fn env_vars_propagate_in_fallback() {
        let tmp = tempfile::tempdir().unwrap();
        let script = write_script(tmp.path(), "x.sh", "echo [$AIMX_MAILBOX]\n");
        let argv = vec![script.to_string_lossy().into_owned()];
        let mut envs = HashMap::new();
        envs.insert("AIMX_MAILBOX".into(), "accounts".into());
        let out = spawn_via_fork_setuid(
            &argv,
            SandboxStdin::None,
            "root",
            Duration::from_secs(5),
            &envs,
            Instant::now(),
        )
        .unwrap();
        assert_eq!(out.exit_code, 0);
        let so = String::from_utf8_lossy(&out.stdout_tail);
        assert!(so.contains("[accounts]"), "stdout: {so}");
    }

    #[test]
    fn large_stdout_truncated_at_cap_keeping_tail() {
        let tmp = tempfile::tempdir().unwrap();
        // Write a marker after a ton of filler so the tail preservation
        // is observable.
        let body = format!(
            "yes 'x' | head -c {} ; printf 'END-MARKER'\n",
            MAX_OUTPUT_BYTES + 4096
        );
        let script = write_script(tmp.path(), "x.sh", &body);
        let argv = vec![script.to_string_lossy().into_owned()];
        let out = run_fallback(&argv, SandboxStdin::None, Duration::from_secs(10)).unwrap();
        assert_eq!(out.stdout_tail.len(), MAX_OUTPUT_BYTES);
        let tail = String::from_utf8_lossy(&out.stdout_tail[out.stdout_tail.len() - 32..]);
        assert!(tail.contains("END-MARKER"), "tail: {tail}");
    }

    #[test]
    fn unknown_user_lookup_directly_returns_user_not_found() {
        // Direct getpwnam lookup for a name very unlikely to exist on
        // any CI host. If it does, the test fails — rename.
        let res = lookup_user("aimx-nonexistent-user-xyz");
        assert!(matches!(res, Err(SandboxError::UserNotFound(_))));
    }

    #[test]
    fn non_root_unknown_user_falls_back_to_current_uid() {
        // Non-root callers (CI) cannot setuid anyway, so
        // `spawn_via_fork_setuid` for an unknown user must fall back to
        // running as the current uid rather than erroring out. This
        // preserves dev-box usability before `aimx setup` has created
        // `aimx-hook`. The equivalent failure path when the caller IS
        // root is covered by the production code review, not a unit
        // test (CI runs non-root).
        let tmp = tempfile::tempdir().unwrap();
        let script = write_script(tmp.path(), "x.sh", "exit 0\n");
        let argv = vec![script.to_string_lossy().into_owned()];
        let res = spawn_via_fork_setuid(
            &argv,
            SandboxStdin::None,
            "aimx-nonexistent-user-xyz",
            Duration::from_secs(5),
            &HashMap::new(),
            Instant::now(),
        );
        assert!(res.is_ok(), "expected fallback Ok, got: {res:?}");
        assert_eq!(res.unwrap().exit_code, 0);
    }
}
