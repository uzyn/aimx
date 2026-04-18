use crate::setup::SystemOps;
use crate::term;
use std::io::{self, BufRead, Write};

pub fn run(yes: bool, sys: &dyn SystemOps) -> Result<(), Box<dyn std::error::Error>> {
    if !sys.check_root() {
        return Err("`aimx uninstall` requires root. Run with: sudo aimx uninstall".into());
    }

    let config_dir = crate::config::config_dir();
    let data_dir = resolve_data_dir_for_display();

    println!("\n{}", term::header("[UNINSTALL]"));
    println!("This will stop the aimx daemon and remove its service file.");
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

    println!("\n{}\n", term::success_banner("Uninstall complete."));
    println!(
        "To wipe config and data manually: sudo rm -rf {} {}",
        config_dir.display(),
        data_dir
    );
    Ok(())
}

fn resolve_data_dir_for_display() -> String {
    crate::config::Config::load_resolved()
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
    }

    impl MockSys {
        fn new(is_root: bool) -> Self {
            Self {
                is_root,
                uninstall_calls: RefCell::new(0),
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
            Ok(PathBuf::from("/usr/local/bin/aimx"))
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
