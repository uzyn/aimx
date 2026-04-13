use crate::config::Config;
use crate::setup::{self, DEFAULT_VERIFY_HOST, NetworkOps, Port25Status};
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

    let port25 = detect_port25();
    run_with_net(&net, &port25)
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

fn detect_port25() -> Port25Status {
    let sys = setup::RealSystemOps;
    setup::SystemOps::check_port25_occupancy(&sys).unwrap_or(Port25Status::Free)
}

pub fn run_with_net(
    net: &dyn NetworkOps,
    port25: &Port25Status,
) -> Result<(), Box<dyn std::error::Error>> {
    println!("aimx verify - Port 25 connectivity check\n");

    let mut all_pass = true;

    // Check 1: Outbound port 25
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

    match port25 {
        Port25Status::Aimx => {
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

            println!();
            if all_pass {
                println!("All checks passed. Port 25 is reachable. SMTP server is up.");
                Ok(())
            } else {
                Err("Some checks failed. See details above.".into())
            }
        }

        Port25Status::OtherProcess(name) => Err(format!(
            "Port 25 is occupied by `{name}`.\n\
             Stop the process and run `sudo aimx setup` to configure aimx."
        )
        .into()),

        Port25Status::Free => {
            println!();
            println!(
                "aimx serve is not running. Start it with:\n\
                 \n\
                 \tsudo aimx setup\n\
                 \n\
                 or if already set up:\n\
                 \n\
                 \tsudo systemctl start aimx"
            );
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::setup::DEFAULT_CHECK_SERVICE_SMTP_ADDR;
    use std::net::IpAddr;

    struct MockNetworkOps {
        outbound: bool,
        inbound: bool,
        ehlo_called: std::cell::Cell<bool>,
    }

    impl Default for MockNetworkOps {
        fn default() -> Self {
            Self {
                outbound: true,
                inbound: true,
                ehlo_called: std::cell::Cell::new(false),
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
        fn check_ptr_record(&self) -> Result<Option<String>, Box<dyn std::error::Error>> {
            Ok(None)
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

    // --- aimx running tests ---

    #[test]
    fn verify_aimx_all_pass() {
        let net = MockNetworkOps::default();
        assert!(run_with_net(&net, &Port25Status::Aimx).is_ok());
        assert!(
            net.ehlo_called.get(),
            "should use EHLO probe when aimx is running"
        );
    }

    #[test]
    fn verify_aimx_outbound_fail() {
        let net = MockNetworkOps {
            outbound: false,
            ..Default::default()
        };
        assert!(run_with_net(&net, &Port25Status::Aimx).is_err());
    }

    #[test]
    fn verify_aimx_inbound_ehlo_fail() {
        let net = MockNetworkOps {
            inbound: false,
            ..Default::default()
        };
        assert!(run_with_net(&net, &Port25Status::Aimx).is_err());
    }

    // --- OtherProcess tests ---

    #[test]
    fn verify_other_process_fails_with_name() {
        let net = MockNetworkOps::default();
        let err = run_with_net(&net, &Port25Status::OtherProcess("postfix".into()))
            .unwrap_err()
            .to_string();
        assert!(err.contains("postfix"), "error should name the process");
        assert!(
            err.contains("Port 25 is occupied"),
            "error should mention port 25 is occupied"
        );
    }

    // --- Free (aimx not running) tests ---

    #[test]
    fn verify_free_advises_start() {
        let net = MockNetworkOps::default();
        let result = run_with_net(&net, &Port25Status::Free);
        assert!(result.is_ok());
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
