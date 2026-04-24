use crate::setup::SystemOps;
use crate::term;
use std::io::{self, BufRead, Write};

pub fn run(yes: bool, sys: &dyn SystemOps) -> Result<(), Box<dyn std::error::Error>> {
    if !sys.check_root() {
        return Err("`aimx uninstall` requires root. Run with: sudo aimx uninstall".into());
    }

    let config_dir = crate::config::config_dir();
    let data_dir = resolve_data_dir_for_display();

    // Resolve the binary path up front so the confirmation prompt can name
    // the exact path we are about to delete. The removal itself happens
    // after `uninstall_service_file` below; reusing this same value keeps
    // the prompt and the action in sync.
    let binary_path = sys.get_aimx_binary_path().ok();
    let binary_path_hint = binary_path
        .as_ref()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "the installed aimx binary".to_string());

    println!("\n{}", term::header("Uninstall"));
    println!(
        "This will stop the aimx daemon, remove its service file, and delete {binary_path_hint}."
    );
    println!(
        "Config ({}) and mailbox data ({}) will be kept.",
        config_dir.display(),
        data_dir
    );

    if !yes {
        let stdin = io::stdin();
        let mut reader = stdin.lock();
        confirm(&mut reader, "Proceed with uninstall? (y/N) ")?;
    }

    sys.uninstall_service_file()?;

    // Remove the installed binary too. Without this, a subsequent
    // `install.sh` run sees the leftover executable, reads its version,
    // and refuses to "downgrade" to the newly-fetched tag. Linux permits
    // unlinking a running executable — the kernel keeps the inode mapped
    // until this process exits — so the self-delete here is safe.
    //
    // Note: `get_aimx_binary_path` canonicalises via `current_exe`, so if
    // the operator installed via an intervening symlink (e.g. a distro
    // package at `/usr/bin/aimx -> /opt/aimx/bin/aimx`), the real target is
    // removed but the symlink is left dangling. `install.sh`'s `[ -x … ]`
    // check evaluates false on a dangling symlink so reinstall still
    // proceeds; the operator may want to `rm` the stale symlink manually.
    let binary_removed = match binary_path {
        Some(path) => match sys.remove_file(&path) {
            Ok(()) => Some(path),
            Err(e) => {
                println!(
                    "{} could not remove binary at {}: {e}",
                    term::warn_mark(),
                    path.display()
                );
                println!("Remove it manually with: sudo rm {}", path.display());
                None
            }
        },
        None => {
            println!(
                "{} could not resolve the aimx binary path; remove it manually if needed.",
                term::warn_mark()
            );
            None
        }
    };

    println!("\n{}\n", term::success_banner("Uninstall complete."));
    if let Some(path) = &binary_removed {
        println!("Removed binary: {}", path.display());
    }
    println!(
        "To wipe config and data manually: sudo rm -rf {} {}",
        config_dir.display(),
        data_dir
    );
    Ok(())
}

fn resolve_data_dir_for_display() -> String {
    crate::config::Config::load_resolved_ignore_warnings()
        .map(|c| c.data_dir.display().to_string())
        .unwrap_or_else(|_| "/var/lib/aimx".to_string())
}

pub fn confirm(reader: &mut dyn BufRead, prompt: &str) -> Result<(), Box<dyn std::error::Error>> {
    print!("{prompt}");
    io::stdout().flush()?;
    let mut line = String::new();
    reader.read_line(&mut line)?;
    if !line.trim().eq_ignore_ascii_case("y") {
        return Err("Uninstall cancelled.".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::setup::Port25Status;
    use std::cell::RefCell;
    use std::path::{Path, PathBuf};

    struct MockSys {
        is_root: bool,
        uninstall_calls: RefCell<u32>,
        removed_files: RefCell<Vec<PathBuf>>,
        binary_path: PathBuf,
        remove_file_fails: bool,
        binary_path_fails: bool,
    }

    impl MockSys {
        fn new(is_root: bool) -> Self {
            Self {
                is_root,
                uninstall_calls: RefCell::new(0),
                removed_files: RefCell::new(vec![]),
                binary_path: PathBuf::from("/usr/local/bin/aimx"),
                remove_file_fails: false,
                binary_path_fails: false,
            }
        }
    }

    impl SystemOps for MockSys {
        fn write_file(&self, _p: &Path, _c: &str) -> Result<(), Box<dyn std::error::Error>> {
            Ok(())
        }
        fn file_exists(&self, _p: &Path) -> bool {
            false
        }
        fn restart_service(&self, _s: &str) -> Result<(), Box<dyn std::error::Error>> {
            Ok(())
        }
        fn stop_service(&self, _s: &str) -> Result<(), Box<dyn std::error::Error>> {
            unreachable!("uninstall::run must not touch stop_service")
        }
        fn start_service(&self, _s: &str) -> Result<(), Box<dyn std::error::Error>> {
            unreachable!("uninstall::run must not touch start_service")
        }
        fn is_service_running(&self, _s: &str) -> bool {
            false
        }
        fn generate_tls_cert(
            &self,
            _cert_dir: &Path,
            _domain: &str,
        ) -> Result<(), Box<dyn std::error::Error>> {
            Ok(())
        }
        fn get_aimx_binary_path(&self) -> Result<PathBuf, Box<dyn std::error::Error>> {
            if self.binary_path_fails {
                return Err("mock binary-path failure".into());
            }
            Ok(self.binary_path.clone())
        }
        fn check_root(&self) -> bool {
            self.is_root
        }
        fn check_port25_occupancy(&self) -> Result<Port25Status, Box<dyn std::error::Error>> {
            Ok(Port25Status::Free)
        }
        fn install_service_file(&self, _d: &Path) -> Result<(), Box<dyn std::error::Error>> {
            Ok(())
        }
        fn uninstall_service_file(&self) -> Result<(), Box<dyn std::error::Error>> {
            *self.uninstall_calls.borrow_mut() += 1;
            Ok(())
        }
        fn remove_file(&self, path: &Path) -> Result<(), Box<dyn std::error::Error>> {
            if self.remove_file_fails {
                return Err("mock remove_file failure".into());
            }
            self.removed_files.borrow_mut().push(path.to_path_buf());
            Ok(())
        }
        fn wait_for_service_ready(&self) -> bool {
            true
        }
    }

    #[test]
    fn non_root_invocation_is_rejected() {
        let sys = MockSys::new(false);
        let err = run(true, &sys).unwrap_err();
        assert!(err.to_string().contains("requires root"));
        assert_eq!(*sys.uninstall_calls.borrow(), 0);
    }

    #[test]
    fn yes_flag_skips_prompt_and_uninstalls() {
        let sys = MockSys::new(true);
        run(true, &sys).expect("uninstall should succeed");
        assert_eq!(*sys.uninstall_calls.borrow(), 1);
        let removed = sys.removed_files.borrow();
        assert_eq!(
            removed.as_slice(),
            &[PathBuf::from("/usr/local/bin/aimx")],
            "uninstall must delete the installed binary so install.sh sees a clean slate"
        );
    }

    #[test]
    fn remove_file_failure_is_non_fatal() {
        let mut sys = MockSys::new(true);
        sys.remove_file_fails = true;
        run(true, &sys).expect("uninstall should succeed even if binary removal fails");
        assert_eq!(*sys.uninstall_calls.borrow(), 1);
        assert!(
            sys.removed_files.borrow().is_empty(),
            "failing remove_file must not record a successful deletion"
        );
    }

    #[test]
    fn binary_path_failure_is_non_fatal() {
        let mut sys = MockSys::new(true);
        sys.binary_path_fails = true;
        run(true, &sys).expect("uninstall should succeed even if binary path resolution fails");
        assert_eq!(*sys.uninstall_calls.borrow(), 1);
        assert!(sys.removed_files.borrow().is_empty());
    }

    #[test]
    fn confirm_accepts_y() {
        let mut input = b"y\n" as &[u8];
        confirm(&mut input, "? ").expect("y should be accepted");
    }

    #[test]
    fn confirm_accepts_uppercase_y() {
        let mut input = b"Y\n" as &[u8];
        confirm(&mut input, "? ").expect("Y should be accepted");
    }

    #[test]
    fn confirm_rejects_no() {
        let mut input = b"n\n" as &[u8];
        let err = confirm(&mut input, "? ").unwrap_err();
        assert!(err.to_string().contains("cancelled"));
    }

    #[test]
    fn confirm_rejects_empty() {
        let mut input = b"\n" as &[u8];
        let err = confirm(&mut input, "? ").unwrap_err();
        assert!(err.to_string().contains("cancelled"));
    }
}
