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

    run_with_net(&net)
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

pub fn run_with_net(net: &dyn NetworkOps) -> Result<(), Box<dyn std::error::Error>> {
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

    // `aimx verify` is a post-setup sanity check — the user already has a
    // working mail server, so the full EHLO handshake via `/probe` is the
    // correct validation.
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
        /// Result for `check_inbound_port25` (EHLO via `/probe`). This is the
        /// variant `aimx verify` should be using.
        inbound: bool,
        /// Result for `check_inbound_reachable` (plain TCP via `/reach`).
        /// `aimx verify` must NOT call this; set to the opposite of `inbound`
        /// in tests that verify `aimx verify` uses the EHLO variant.
        inbound_reachable: bool,
        ptr: Option<String>,
        /// Track whether `check_inbound_reachable` was called during the test
        /// — used to assert `aimx verify` never touches the `/reach` path.
        reach_called: std::cell::Cell<bool>,
    }

    impl Default for MockNetworkOps {
        fn default() -> Self {
            Self {
                outbound: true,
                inbound: true,
                inbound_reachable: true,
                ptr: Some("mail.example.com.".into()),
                reach_called: std::cell::Cell::new(false),
            }
        }
    }

    impl NetworkOps for MockNetworkOps {
        fn check_outbound_port25(&self) -> Result<bool, Box<dyn std::error::Error>> {
            Ok(self.outbound)
        }
        fn check_inbound_port25(&self) -> Result<bool, Box<dyn std::error::Error>> {
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

    #[test]
    fn verify_all_pass() {
        let net = MockNetworkOps::default();
        assert!(run_with_net(&net).is_ok());
    }

    #[test]
    fn verify_outbound_fail() {
        let net = MockNetworkOps {
            outbound: false,
            ..Default::default()
        };
        assert!(run_with_net(&net).is_err());
    }

    #[test]
    fn verify_inbound_fail() {
        let net = MockNetworkOps {
            inbound: false,
            ..Default::default()
        };
        assert!(run_with_net(&net).is_err());
    }

    #[test]
    fn verify_ptr_warn_still_passes() {
        let net = MockNetworkOps {
            ptr: None,
            ..Default::default()
        };
        assert!(run_with_net(&net).is_ok());
    }

    #[test]
    fn verify_all_fail() {
        let net = MockNetworkOps {
            outbound: false,
            inbound: false,
            ptr: None,
            ..Default::default()
        };
        assert!(run_with_net(&net).is_err());
    }

    /// Regression test for S13.1: `aimx verify` must continue to use the
    /// EHLO-based `/probe` path (not the plain-TCP `/reach` path). If the
    /// EHLO path says `inbound: false` but the reach path says
    /// `inbound_reachable: true`, `aimx verify` should still fail.
    #[test]
    fn verify_uses_ehlo_not_reach() {
        let net = MockNetworkOps {
            inbound: false,
            inbound_reachable: true,
            ..Default::default()
        };
        assert!(run_with_net(&net).is_err());
        assert!(
            !net.reach_called.get(),
            "aimx verify must not call check_inbound_reachable (the /reach endpoint)"
        );
    }

    #[test]
    fn config_without_verify_address_parses() {
        let toml_str = "domain = \"test.com\"\n[mailboxes]\n";
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(config.domain, "test.com");
    }

    #[test]
    fn config_with_legacy_verify_address_parses() {
        // serde should ignore unknown fields (verify_address was removed)
        // This works because Config does NOT have #[serde(deny_unknown_fields)]
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
