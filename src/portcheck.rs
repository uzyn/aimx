use crate::config::Config;
use crate::setup::{self, DEFAULT_VERIFY_HOST, NetworkOps, Port25Status, SystemOps};
use crate::term;

pub fn run(verify_host: Option<&str>) -> Result<(), Box<dyn std::error::Error>> {
    run_portcheck(verify_host, &setup::RealSystemOps)
}

pub(crate) fn run_portcheck(
    verify_host: Option<&str>,
    sys: &dyn SystemOps,
) -> Result<(), Box<dyn std::error::Error>> {
    if !sys.check_root() {
        return Err("`aimx portcheck` requires root. Run with: sudo aimx portcheck".into());
    }

    let config = Config::load_resolved_ignore_warnings().ok();

    let host = resolve_verify_host(verify_host, config.as_ref(), DEFAULT_VERIFY_HOST);
    let net = setup::RealNetworkOps::from_verify_host(host)?;

    let port25 = detect_port25(sys);

    if matches!(port25, Port25Status::Free) {
        return with_temp_smtp_listener(|| run_with_net(&net, &Port25Status::Free));
    }

    run_with_net(&net, &port25)
}

/// Bind a minimal SMTP listener on `0.0.0.0:25` for the duration of `f`,
/// then tear it down. Used by the preflight probe during `aimx setup`
/// (before aimx.service is installed) and by `aimx portcheck` (when nothing
/// is holding the port). The listener speaks just enough SMTP (220 banner,
/// EHLO/HELO, QUIT) for the remote `/probe` endpoint to confirm the port
/// is reachable.
pub(crate) fn with_temp_smtp_listener<F, R>(f: F) -> Result<R, Box<dyn std::error::Error>>
where
    F: FnOnce() -> Result<R, Box<dyn std::error::Error>>,
{
    let rt =
        tokio::runtime::Runtime::new().map_err(|e| format!("Failed to create runtime: {e}"))?;

    let listener = match rt.block_on(tokio::net::TcpListener::bind("0.0.0.0:25")) {
        Ok(l) => l,
        Err(e) => {
            return Err(format!(
                "Cannot bind port 25 for preflight: {e}\n\
                 Run with sudo or ensure port 25 is available."
            )
            .into());
        }
    };

    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let _guard = rt.enter();
    let handle = rt.spawn(run_temp_smtp_listener(listener, shutdown_rx));

    let result = f();

    let _ = shutdown_tx.send(true);
    rt.block_on(async {
        let _ = handle.await;
    });

    result
}

async fn run_temp_smtp_listener(
    listener: tokio::net::TcpListener,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
) {
    loop {
        tokio::select! {
            result = listener.accept() => {
                if let Ok((stream, _)) = result {
                    tokio::spawn(handle_temp_smtp_connection(stream));
                }
            }
            _ = shutdown.changed() => break,
        }
    }
}

async fn handle_temp_smtp_connection(stream: tokio::net::TcpStream) {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    let (reader, mut writer) = tokio::io::split(stream);
    let mut reader = BufReader::new(reader);

    if writer
        .write_all(b"220 localhost ESMTP aimx\r\n")
        .await
        .is_err()
    {
        return;
    }
    if writer.flush().await.is_err() {
        return;
    }

    let mut line = String::new();
    loop {
        line.clear();
        match tokio::time::timeout(
            std::time::Duration::from_secs(30),
            reader.read_line(&mut line),
        )
        .await
        {
            Ok(Ok(0)) | Ok(Err(_)) | Err(_) => break,
            Ok(Ok(_)) => {}
        }

        let upper = line.trim().to_ascii_uppercase();
        let resp: &[u8] = if upper.starts_with("EHLO") || upper.starts_with("HELO") {
            b"250 localhost\r\n"
        } else if upper.starts_with("QUIT") {
            let _ = writer.write_all(b"221 Bye\r\n").await;
            break;
        } else {
            b"502 Not implemented\r\n"
        };
        if writer.write_all(resp).await.is_err() {
            break;
        }
        let _ = writer.flush().await;
    }
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

fn detect_port25(sys: &dyn SystemOps) -> Port25Status {
    sys.check_port25_occupancy().unwrap_or(Port25Status::Free)
}

pub fn run_with_net(
    net: &dyn NetworkOps,
    port25: &Port25Status,
) -> Result<(), Box<dyn std::error::Error>> {
    println!(
        "{}\n",
        term::header("aimx portcheck - Port 25 connectivity check")
    );

    let mut all_pass = true;

    // Check 1: Outbound port 25
    print!("  Outbound port 25... ");
    std::io::Write::flush(&mut std::io::stdout())?;
    match setup::check_outbound(net) {
        setup::PreflightResult::Pass(_) => println!("{}", term::pass_badge()),
        setup::PreflightResult::Fail(msg) => {
            println!("{}", term::fail_badge());
            eprintln!("  {msg}");
            all_pass = false;
        }
    }

    match port25 {
        Port25Status::Aimx | Port25Status::Free => {
            print!("  Inbound port 25... ");
            std::io::Write::flush(&mut std::io::stdout())?;
            match setup::check_inbound(net) {
                setup::PreflightResult::Pass(_) => println!("{}", term::pass_badge()),
                setup::PreflightResult::Fail(msg) => {
                    println!("{}", term::fail_badge());
                    eprintln!("  {msg}");
                    all_pass = false;
                }
            }

            println!();
            if all_pass {
                println!(
                    "{}",
                    term::success(
                        "All checks passed. Port 25 is reachable. Your system is good for aimx setup."
                    )
                );
                println!("Run `sudo aimx setup` to begin.");
                Ok(())
            } else {
                Err("Some checks failed. See details above.".into())
            }
        }

        Port25Status::OtherProcess(name) => Err(format!(
            "Port 25 is occupied by `{name}`.\n\
             Stop or uninstall the process and run `sudo aimx portcheck` again to check."
        )
        .into()),
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
        fn get_server_ips(
            &self,
        ) -> Result<
            (Option<std::net::Ipv4Addr>, Option<std::net::Ipv6Addr>),
            Box<dyn std::error::Error>,
        > {
            Ok((Some("1.2.3.4".parse().unwrap()), None))
        }
        fn resolve_mx(&self, _domain: &str) -> Result<Vec<String>, Box<dyn std::error::Error>> {
            Ok(vec![])
        }
        fn resolve_a(&self, _domain: &str) -> Result<Vec<IpAddr>, Box<dyn std::error::Error>> {
            Ok(vec![])
        }
        fn resolve_aaaa(&self, _domain: &str) -> Result<Vec<IpAddr>, Box<dyn std::error::Error>> {
            Ok(vec![])
        }
        fn resolve_txt(&self, _domain: &str) -> Result<Vec<String>, Box<dyn std::error::Error>> {
            Ok(vec![])
        }
    }

    struct MockSystemOps {
        is_root: bool,
    }

    impl SystemOps for MockSystemOps {
        fn write_file(
            &self,
            _path: &std::path::Path,
            _content: &str,
        ) -> Result<(), Box<dyn std::error::Error>> {
            Ok(())
        }
        fn file_exists(&self, _path: &std::path::Path) -> bool {
            false
        }
        fn restart_service(&self, _service: &str) -> Result<(), Box<dyn std::error::Error>> {
            Ok(())
        }
        fn is_service_running(&self, _service: &str) -> bool {
            false
        }
        fn generate_tls_cert(
            &self,
            _cert_dir: &std::path::Path,
            _domain: &str,
        ) -> Result<(), Box<dyn std::error::Error>> {
            Ok(())
        }
        fn get_aimx_binary_path(&self) -> Result<std::path::PathBuf, Box<dyn std::error::Error>> {
            Ok(std::path::PathBuf::from("/usr/local/bin/aimx"))
        }
        fn check_root(&self) -> bool {
            self.is_root
        }
        fn check_port25_occupancy(&self) -> Result<Port25Status, Box<dyn std::error::Error>> {
            Ok(Port25Status::Free)
        }
        fn install_service_file(
            &self,
            _data_dir: &std::path::Path,
        ) -> Result<(), Box<dyn std::error::Error>> {
            Ok(())
        }
        fn uninstall_service_file(&self) -> Result<(), Box<dyn std::error::Error>> {
            Ok(())
        }
        fn wait_for_service_ready(&self) -> bool {
            true
        }
        fn with_temp_smtp_listener(
            &self,
            f: &mut dyn FnMut() -> Result<(), Box<dyn std::error::Error>>,
        ) -> Result<(), Box<dyn std::error::Error>> {
            f()
        }
    }

    #[test]
    fn portcheck_requires_root() {
        let sys = MockSystemOps { is_root: false };
        let result = run_portcheck(Some("https://check.example.com"), &sys);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("requires root"),
            "Expected root error, got: {err}"
        );
    }

    // --- aimx running tests ---

    #[test]
    fn portcheck_aimx_all_pass() {
        let net = MockNetworkOps::default();
        assert!(run_with_net(&net, &Port25Status::Aimx).is_ok());
        assert!(
            net.ehlo_called.get(),
            "should use EHLO probe when aimx is running"
        );
    }

    #[test]
    fn portcheck_aimx_outbound_fail() {
        let net = MockNetworkOps {
            outbound: false,
            ..Default::default()
        };
        assert!(run_with_net(&net, &Port25Status::Aimx).is_err());
    }

    #[test]
    fn portcheck_aimx_inbound_ehlo_fail() {
        let net = MockNetworkOps {
            inbound: false,
            ..Default::default()
        };
        assert!(run_with_net(&net, &Port25Status::Aimx).is_err());
    }

    // --- OtherProcess tests ---

    #[test]
    fn portcheck_other_process_fails_with_name() {
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

    // --- Free (temp server) tests ---

    #[test]
    fn portcheck_free_all_pass() {
        let net = MockNetworkOps::default();
        assert!(run_with_net(&net, &Port25Status::Free).is_ok());
        assert!(
            net.ehlo_called.get(),
            "should use EHLO probe when temp server is running"
        );
    }

    #[test]
    fn portcheck_free_inbound_fail() {
        let net = MockNetworkOps {
            inbound: false,
            ..Default::default()
        };
        assert!(run_with_net(&net, &Port25Status::Free).is_err());
    }

    // --- verify_host resolution tests ---

    #[test]
    fn config_without_verify_address_parses() {
        let toml_str = "domain = \"test.com\"\n[mailboxes]\n";
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
