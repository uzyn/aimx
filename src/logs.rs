//! `aimx logs` — tail or follow the aimx service log.
//!
//! Wraps `journalctl -u aimx -n <N>` on systemd and a best-effort
//! `/var/log/aimx/*.log` / `/var/log/messages` read on OpenRC. The
//! [`SystemOps::tail_service_logs`] / [`SystemOps::follow_service_logs`]
//! trait methods own the actual systemd/OpenRC dispatch so this command
//! is testable without spawning `journalctl`.

use crate::setup::{RealSystemOps, SystemOps};
use crate::term;

/// Default `--lines` value. 50 is enough to spot recent activity without
/// flooding the terminal.
pub const DEFAULT_LINES: usize = 50;

/// Service unit name for the daemon. Kept here (and in `doctor.rs`) so
/// the two callers that share the log tail stay in sync.
pub const SERVICE_UNIT: &str = "aimx";

pub fn run(lines: Option<usize>, follow: bool) -> Result<(), Box<dyn std::error::Error>> {
    run_with_ops(lines, follow, &RealSystemOps)
}

pub fn run_with_ops<S: SystemOps>(
    lines: Option<usize>,
    follow: bool,
    sys: &S,
) -> Result<(), Box<dyn std::error::Error>> {
    if follow {
        // `--follow` replaces the current process with `journalctl -f`
        // (or the OpenRC equivalent), so on success this call does not
        // return. On failure we fall through and print the error.
        return sys.follow_service_logs(SERVICE_UNIT);
    }

    let n = lines.unwrap_or(DEFAULT_LINES);
    match sys.tail_service_logs(SERVICE_UNIT, n) {
        Ok(out) => {
            print!("{out}");
            if !out.ends_with('\n') {
                println!();
            }
            Ok(())
        }
        Err(e) => {
            eprintln!(
                "{} {e}\n  {}",
                term::warn("warning:"),
                term::dim(
                    "If you are on systemd, run `journalctl -u aimx` directly. \
                     On OpenRC, check /var/log/messages or your syslog config."
                ),
            );
            Err(e)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::setup::Port25Status;
    use std::cell::Cell;
    use std::path::{Path, PathBuf};

    /// Minimal `SystemOps` mock that only services the log methods. Every
    /// other method panics — the logs command must not touch unrelated
    /// SystemOps behaviour.
    struct FakeLogsOps {
        canned: String,
        fail: bool,
        follow_called: Cell<bool>,
        last_n: Cell<usize>,
    }

    impl FakeLogsOps {
        fn new(canned: &str) -> Self {
            Self {
                canned: canned.to_string(),
                fail: false,
                follow_called: Cell::new(false),
                last_n: Cell::new(0),
            }
        }
        fn failing() -> Self {
            Self {
                canned: String::new(),
                fail: true,
                follow_called: Cell::new(false),
                last_n: Cell::new(0),
            }
        }
    }

    impl SystemOps for FakeLogsOps {
        fn write_file(
            &self,
            _path: &Path,
            _content: &str,
        ) -> Result<(), Box<dyn std::error::Error>> {
            unreachable!("logs::run must not touch write_file")
        }
        fn file_exists(&self, _path: &Path) -> bool {
            unreachable!("logs::run must not touch file_exists")
        }
        fn restart_service(&self, _service: &str) -> Result<(), Box<dyn std::error::Error>> {
            unreachable!("logs::run must not touch restart_service")
        }
        fn is_service_running(&self, _service: &str) -> bool {
            unreachable!("logs::run must not touch is_service_running")
        }
        fn generate_tls_cert(
            &self,
            _cert_dir: &Path,
            _domain: &str,
        ) -> Result<(), Box<dyn std::error::Error>> {
            unreachable!("logs::run must not touch generate_tls_cert")
        }
        fn get_aimx_binary_path(&self) -> Result<PathBuf, Box<dyn std::error::Error>> {
            unreachable!("logs::run must not touch get_aimx_binary_path")
        }
        fn check_root(&self) -> bool {
            unreachable!("logs::run must not touch check_root")
        }
        fn check_port25_occupancy(&self) -> Result<Port25Status, Box<dyn std::error::Error>> {
            unreachable!("logs::run must not touch check_port25_occupancy")
        }
        fn install_service_file(&self, _data_dir: &Path) -> Result<(), Box<dyn std::error::Error>> {
            unreachable!("logs::run must not touch install_service_file")
        }
        fn uninstall_service_file(&self) -> Result<(), Box<dyn std::error::Error>> {
            unreachable!("logs::run must not touch uninstall_service_file")
        }
        fn wait_for_service_ready(&self) -> bool {
            unreachable!("logs::run must not touch wait_for_service_ready")
        }
        fn tail_service_logs(
            &self,
            unit: &str,
            n: usize,
        ) -> Result<String, Box<dyn std::error::Error>> {
            assert_eq!(unit, SERVICE_UNIT, "logs must query the aimx unit");
            self.last_n.set(n);
            if self.fail {
                Err("no journal available".into())
            } else {
                Ok(self.canned.clone())
            }
        }
        fn follow_service_logs(&self, unit: &str) -> Result<(), Box<dyn std::error::Error>> {
            assert_eq!(unit, SERVICE_UNIT);
            self.follow_called.set(true);
            Ok(())
        }
    }

    #[test]
    fn run_uses_default_lines_when_none_supplied() {
        let ops = FakeLogsOps::new("line one\nline two\n");
        run_with_ops(None, false, &ops).unwrap();
        assert_eq!(ops.last_n.get(), DEFAULT_LINES);
    }

    #[test]
    fn run_passes_explicit_line_count() {
        let ops = FakeLogsOps::new("line\n");
        run_with_ops(Some(7), false, &ops).unwrap();
        assert_eq!(ops.last_n.get(), 7);
    }

    #[test]
    fn run_follow_dispatches_to_follow_service_logs() {
        let ops = FakeLogsOps::new("");
        run_with_ops(None, true, &ops).unwrap();
        assert!(
            ops.follow_called.get(),
            "follow=true must call follow_service_logs"
        );
        assert_eq!(
            ops.last_n.get(),
            0,
            "follow path must not invoke tail_service_logs"
        );
    }

    #[test]
    fn run_propagates_tail_errors() {
        let ops = FakeLogsOps::failing();
        let result = run_with_ops(Some(10), false, &ops);
        assert!(
            result.is_err(),
            "tail failure must surface as a non-zero exit"
        );
    }
}
