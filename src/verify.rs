use crate::config::Config;
use crate::setup::{self, DEFAULT_PROBE_URL, NetworkOps};
use std::path::Path;

pub fn run(data_dir: Option<&Path>) -> Result<(), Box<dyn std::error::Error>> {
    let config = match data_dir {
        Some(dir) => Config::load_from_data_dir(dir).ok(),
        None => Config::load_default().ok(),
    };

    let probe_url = config
        .as_ref()
        .and_then(|c| c.probe_url.clone())
        .unwrap_or_else(|| DEFAULT_PROBE_URL.to_string());

    let net = setup::RealNetworkOps::from_probe_url(probe_url);

    run_with_net(&net)
}

pub fn run_with_net(net: &dyn NetworkOps) -> Result<(), Box<dyn std::error::Error>> {
    println!("aimx verify - Port 25 connectivity check\n");

    let mut all_pass = true;

    print!("  Outbound port 25... ");
    std::io::Write::flush(&mut std::io::stdout())?;
    match setup::check_outbound(net) {
        setup::PreflightResult::Pass => println!("PASS"),
        setup::PreflightResult::Fail(msg) => {
            println!("FAIL");
            eprintln!("  {msg}");
            all_pass = false;
        }
        setup::PreflightResult::Warn(msg) => println!("WARN: {msg}"),
    }

    print!("  Inbound port 25 (EHLO probe)... ");
    std::io::Write::flush(&mut std::io::stdout())?;
    match setup::check_inbound(net) {
        setup::PreflightResult::Pass => println!("PASS"),
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
        setup::PreflightResult::Pass => println!("PASS"),
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
        inbound: bool,
        ptr: Option<String>,
    }

    impl NetworkOps for MockNetworkOps {
        fn check_outbound_port25(&self) -> Result<bool, Box<dyn std::error::Error>> {
            Ok(self.outbound)
        }
        fn check_inbound_port25(&self) -> Result<bool, Box<dyn std::error::Error>> {
            Ok(self.inbound)
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
        let net = MockNetworkOps {
            outbound: true,
            inbound: true,
            ptr: Some("mail.example.com.".into()),
        };
        assert!(run_with_net(&net).is_ok());
    }

    #[test]
    fn verify_outbound_fail() {
        let net = MockNetworkOps {
            outbound: false,
            inbound: true,
            ptr: Some("mail.example.com.".into()),
        };
        assert!(run_with_net(&net).is_err());
    }

    #[test]
    fn verify_inbound_fail() {
        let net = MockNetworkOps {
            outbound: true,
            inbound: false,
            ptr: Some("mail.example.com.".into()),
        };
        assert!(run_with_net(&net).is_err());
    }

    #[test]
    fn verify_ptr_warn_still_passes() {
        let net = MockNetworkOps {
            outbound: true,
            inbound: true,
            ptr: None,
        };
        assert!(run_with_net(&net).is_ok());
    }

    #[test]
    fn verify_all_fail() {
        let net = MockNetworkOps {
            outbound: false,
            inbound: false,
            ptr: None,
        };
        assert!(run_with_net(&net).is_err());
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
}
