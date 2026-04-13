use crate::config::Config;
use crate::setup::{self, DEFAULT_VERIFY_HOST, NetworkOps};
use std::path::Path;

pub fn run(
    data_dir: Option<&Path>,
    verify_host: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    let config = match data_dir {
        Some(dir) => Config::load_from_data_dir(dir).ok(),
        None => Config::load_default().ok(),
    };

    let host = resolve_verify_host(verify_host, config.as_ref(), DEFAULT_VERIFY_HOST);
    let net = setup::RealNetworkOps::from_verify_host(host)?;

    let smtp_up = is_smtp_listening();
    run_with_net(&net, smtp_up)
}

pub(crate) fn resolve_verify_host(
    cli_override: Option<&str>,
    config: Option<&Config>,
    default: &str,
) -> String {
    if let Some(host) = cli_override {
        return host.to_string();
    }
    config
        .and_then(|c| c.verify_host.clone())
        .unwrap_or_else(|| default.to_string())
}

/// Check whether an SMTP server is already listening on localhost port 25.
fn is_smtp_listening() -> bool {
    use std::net::{TcpStream, ToSocketAddrs};
    use std::time::Duration;

    if let Ok(mut addrs) = "127.0.0.1:25".to_socket_addrs()
        && let Some(addr) = addrs.next()
    {
        return TcpStream::connect_timeout(&addr, Duration::from_secs(1)).is_ok();
    }
    false
}

/// Returns true if the current process is running as root.
fn is_root() -> bool {
    unsafe { libc::geteuid() == 0 }
}

pub fn run_with_net(net: &dyn NetworkOps, smtp_up: bool) -> Result<(), Box<dyn std::error::Error>> {
    println!("aimx verify - Port 25 connectivity check\n");

    let mut all_pass = true;

    print!("  Outbound port 25... ");
    std::io::Write::flush(&mut std::io::stdout())?;
    match setup::check_outbound(net) {
        setup::PreflightResult::Pass(_) => println!("PASS"),
        setup::PreflightResult::Fail(msg) => {
            println!("FAIL");
            eprintln!("  {msg}");
            all_pass = false;
        }
        setup::PreflightResult::Warn(msg) => println!("WARN: {msg}"),
    }

    if smtp_up {
        // OpenSMTPD (or another SMTP server) is running — full EHLO handshake.
        print!("  Inbound port 25 (EHLO probe)... ");
        std::io::Write::flush(&mut std::io::stdout())?;
        match setup::check_inbound(net) {
            setup::PreflightResult::Pass(_) => println!("PASS"),
            setup::PreflightResult::Fail(msg) => {
                println!("FAIL");
                eprintln!("  {msg}");
                all_pass = false;
            }
            setup::PreflightResult::Warn(msg) => println!("WARN: {msg}"),
        }
    } else {
        // No SMTP server yet — bind a temporary listener and do a plain TCP
        // reachability check. Requires root to bind port 25.
        if !is_root() {
            return Err(
                "No SMTP server detected on port 25. Root is required to bind a \
                 temporary listener for the inbound check.\n\
                 Run with: sudo aimx verify"
                    .into(),
            );
        }
        let _temp_listener = std::net::TcpListener::bind("0.0.0.0:25").ok();

        print!("  Inbound port 25 (TCP reach)... ");
        std::io::Write::flush(&mut std::io::stdout())?;
        match setup::check_inbound_tcp(net) {
            setup::PreflightResult::Pass(_) => println!("PASS"),
            setup::PreflightResult::Fail(msg) => {
                println!("FAIL");
                eprintln!("  {msg}");
                all_pass = false;
            }
            setup::PreflightResult::Warn(msg) => println!("WARN: {msg}"),
        }
    }

    print!("  PTR record... ");
    std::io::Write::flush(&mut std::io::stdout())?;
    match setup::check_ptr(net) {
        setup::PreflightResult::Pass(Some(ptr)) => println!("PASS ({ptr})"),
        setup::PreflightResult::Pass(None) => println!("PASS"),
        setup::PreflightResult::Fail(msg) => {
            println!("FAIL: {msg}");
            all_pass = false;
        }
        setup::PreflightResult::Warn(msg) => println!("WARN\n  {msg}"),
    }

    println!();

    if all_pass {
        println!("All checks passed. Port 25 is reachable.");
        Ok(())
    } else {
        Err("Some checks failed. See details above.".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::setup::DEFAULT_CHECK_SERVICE_SMTP_ADDR;
    use std::net::IpAddr;

    struct MockNetworkOps {
        outbound: bool,
        /// Result for `check_inbound_port25` (EHLO via `/probe`).
        inbound: bool,
        /// Result for `check_inbound_reachable` (plain TCP via `/reach`).
        inbound_reachable: bool,
        ptr: Option<String>,
        /// Track whether `check_inbound_port25` was called.
        ehlo_called: std::cell::Cell<bool>,
        /// Track whether `check_inbound_reachable` was called.
        reach_called: std::cell::Cell<bool>,
    }

    impl Default for MockNetworkOps {
        fn default() -> Self {
            Self {
                outbound: true,
                inbound: true,
                inbound_reachable: true,
                ptr: Some("mail.example.com.".into()),
                ehlo_called: std::cell::Cell::new(false),
                reach_called: std::cell::Cell::new(false),
            }
        }
    }

    impl NetworkOps for MockNetworkOps {
        fn check_outbound_port25(&self) -> Result<bool, Box<dyn std::error::Error>> {
            Ok(self.outbound)
        }
        fn check_inbound_port25(&self) -> Result<bool, Box<dyn std::error::Error>> {
            self.ehlo_called.set(true);
            Ok(self.inbound)
        }
        fn check_inbound_reachable(&self) -> Result<bool, Box<dyn std::error::Error>> {
            self.reach_called.set(true);
            Ok(self.inbound_reachable)
        }
        fn check_ptr_record(&self) -> Result<Option<String>, Box<dyn std::error::Error>> {
            Ok(self.ptr.clone())
        }
        fn get_server_ip(&self) -> Result<IpAddr, Box<dyn std::error::Error>> {
            Ok("1.2.3.4".parse().unwrap())
        }
        fn resolve_mx(&self, _domain: &str) -> Result<Vec<String>, Box<dyn std::error::Error>> {
            Ok(vec![])
        }
        fn resolve_a(&self, _domain: &str) -> Result<Vec<IpAddr>, Box<dyn std::error::Error>> {
            Ok(vec![])
        }
        fn resolve_txt(&self, _domain: &str) -> Result<Vec<String>, Box<dyn std::error::Error>> {
            Ok(vec![])
        }
    }

    // --- smtp_up=true (post-install) tests ---

    #[test]
    fn verify_all_pass_smtp_up() {
        let net = MockNetworkOps::default();
        assert!(run_with_net(&net, true).is_ok());
        assert!(net.ehlo_called.get(), "should use EHLO probe when smtp_up");
        assert!(
            !net.reach_called.get(),
            "should not use TCP reach when smtp_up"
        );
    }

    #[test]
    fn verify_outbound_fail() {
        let net = MockNetworkOps {
            outbound: false,
            ..Default::default()
        };
        assert!(run_with_net(&net, true).is_err());
    }

    #[test]
    fn verify_inbound_ehlo_fail() {
        let net = MockNetworkOps {
            inbound: false,
            ..Default::default()
        };
        assert!(run_with_net(&net, true).is_err());
    }

    #[test]
    fn verify_ptr_warn_still_passes() {
        let net = MockNetworkOps {
            ptr: None,
            ..Default::default()
        };
        assert!(run_with_net(&net, true).is_ok());
    }

    #[test]
    fn verify_all_fail() {
        let net = MockNetworkOps {
            outbound: false,
            inbound: false,
            ptr: None,
            ..Default::default()
        };
        assert!(run_with_net(&net, true).is_err());
    }

    /// When smtp_up=true, verify must use the EHLO `/probe` path, not `/reach`.
    #[test]
    fn verify_smtp_up_uses_ehlo_not_reach() {
        let net = MockNetworkOps {
            inbound: false,
            inbound_reachable: true,
            ..Default::default()
        };
        assert!(run_with_net(&net, true).is_err());
        assert!(
            !net.reach_called.get(),
            "aimx verify with smtp_up must not call check_inbound_reachable (/reach)"
        );
    }

    // --- smtp_up=false (pre-install / preflight) tests ---

    #[test]
    fn verify_preflight_all_pass() {
        let net = MockNetworkOps::default();
        // smtp_up=false path requires root to bind port 25; in tests we may or
        // may not be root. We can't reliably test the root-gated path in CI, so
        // only test if running as root.
        if !is_root() {
            // Non-root should get a clear error.
            let err = run_with_net(&net, false).unwrap_err();
            assert!(
                err.to_string().contains("sudo aimx verify"),
                "should advise running with sudo"
            );
            return;
        }
        assert!(run_with_net(&net, false).is_ok());
        assert!(
            net.reach_called.get(),
            "should use TCP reach when smtp not up"
        );
        assert!(
            !net.ehlo_called.get(),
            "should not use EHLO probe when smtp not up"
        );
    }

    #[test]
    fn verify_preflight_uses_reach_not_ehlo() {
        if !is_root() {
            return; // Can't test this path without root
        }
        let net = MockNetworkOps {
            inbound_reachable: false,
            inbound: true,
            ..Default::default()
        };
        assert!(run_with_net(&net, false).is_err());
        assert!(
            net.reach_called.get(),
            "preflight mode must call check_inbound_reachable (/reach)"
        );
        assert!(
            !net.ehlo_called.get(),
            "preflight mode must not call check_inbound_port25 (/probe)"
        );
    }

    // --- verify_host resolution tests ---

    #[test]
    fn config_without_verify_address_parses() {
        let toml_str = "domain = \"test.com\"\n[mailboxes]\n";
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(config.domain, "test.com");
    }

    #[test]
    fn config_with_legacy_verify_address_parses() {
        let toml_str = "domain = \"test.com\"\nverify_address = \"verify@old.com\"\n[mailboxes]\n";
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(config.domain, "test.com");
    }

    #[test]
    fn default_check_service_smtp_addr() {
        assert_eq!(DEFAULT_CHECK_SERVICE_SMTP_ADDR, "check.aimx.email:25");
    }

    fn cfg_with_verify_host(host: Option<&str>) -> Config {
        let toml_str = match host {
            Some(h) => format!("domain = \"test.com\"\nverify_host = \"{h}\"\n[mailboxes]\n"),
            None => "domain = \"test.com\"\n[mailboxes]\n".to_string(),
        };
        toml::from_str(&toml_str).unwrap()
    }

    #[test]
    fn resolve_host_prefers_cli_override_over_config_and_default() {
        let config = cfg_with_verify_host(Some("https://config.example.com"));
        let resolved = resolve_verify_host(
            Some("https://cli.example.com"),
            Some(&config),
            "https://default.example.com",
        );
        assert_eq!(resolved, "https://cli.example.com");
    }

    #[test]
    fn resolve_host_uses_config_when_no_cli_override() {
        let config = cfg_with_verify_host(Some("https://config.example.com"));
        let resolved = resolve_verify_host(None, Some(&config), "https://default.example.com");
        assert_eq!(resolved, "https://config.example.com");
    }

    #[test]
    fn resolve_host_falls_back_to_default_when_config_missing_field() {
        let config = cfg_with_verify_host(None);
        let resolved = resolve_verify_host(None, Some(&config), "https://default.example.com");
        assert_eq!(resolved, "https://default.example.com");
    }

    #[test]
    fn resolve_host_falls_back_to_default_when_no_config() {
        let resolved = resolve_verify_host(None, None, "https://default.example.com");
        assert_eq!(resolved, "https://default.example.com");
    }
}
