use crate::config::{Config, MailboxConfig};
use crate::dkim;
use crate::verify;
use chrono::Utc;
use std::collections::HashMap;
use std::io::{self, Write};
use std::net::IpAddr;
use std::path::{Path, PathBuf};

pub trait SystemOps {
    fn is_package_installed(&self, package: &str) -> bool;
    fn install_package(&self, package: &str) -> Result<(), Box<dyn std::error::Error>>;
    fn write_file(&self, path: &Path, content: &str) -> Result<(), Box<dyn std::error::Error>>;
    fn read_file(&self, path: &Path) -> Result<String, Box<dyn std::error::Error>>;
    fn file_exists(&self, path: &Path) -> bool;
    fn restart_service(&self, service: &str) -> Result<(), Box<dyn std::error::Error>>;
    fn generate_tls_cert(
        &self,
        cert_dir: &Path,
        domain: &str,
    ) -> Result<(), Box<dyn std::error::Error>>;
    fn get_aimx_binary_path(&self) -> Result<PathBuf, Box<dyn std::error::Error>>;
}

pub trait NetworkOps {
    fn check_outbound_port25(&self) -> Result<bool, Box<dyn std::error::Error>>;
    fn check_inbound_port25(&self) -> Result<bool, Box<dyn std::error::Error>>;
    fn check_ptr_record(&self) -> Result<Option<String>, Box<dyn std::error::Error>>;
    fn get_server_ip(&self) -> Result<IpAddr, Box<dyn std::error::Error>>;
    fn resolve_mx(&self, domain: &str) -> Result<Vec<String>, Box<dyn std::error::Error>>;
    fn resolve_a(&self, domain: &str) -> Result<Vec<IpAddr>, Box<dyn std::error::Error>>;
    fn resolve_txt(&self, domain: &str) -> Result<Vec<String>, Box<dyn std::error::Error>>;
}

pub trait VerifyRunner {
    fn run_verify(&self, data_dir: Option<&Path>) -> Result<(), Box<dyn std::error::Error>>;
}

pub struct RealVerifyRunner;

impl VerifyRunner for RealVerifyRunner {
    fn run_verify(&self, data_dir: Option<&Path>) -> Result<(), Box<dyn std::error::Error>> {
        verify::run(data_dir)
    }
}

pub struct RealSystemOps;

impl SystemOps for RealSystemOps {
    fn is_package_installed(&self, package: &str) -> bool {
        std::process::Command::new("dpkg")
            .args(["-s", package])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    }

    fn install_package(&self, package: &str) -> Result<(), Box<dyn std::error::Error>> {
        let status = std::process::Command::new("sudo")
            .args(["apt-get", "install", "-y", package])
            .status()?;
        if !status.success() {
            return Err(format!("Failed to install {package}").into());
        }
        Ok(())
    }

    fn write_file(&self, path: &Path, content: &str) -> Result<(), Box<dyn std::error::Error>> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, content)?;
        Ok(())
    }

    fn read_file(&self, path: &Path) -> Result<String, Box<dyn std::error::Error>> {
        Ok(std::fs::read_to_string(path)?)
    }

    fn file_exists(&self, path: &Path) -> bool {
        path.exists()
    }

    fn restart_service(&self, service: &str) -> Result<(), Box<dyn std::error::Error>> {
        let status = std::process::Command::new("sudo")
            .args(["systemctl", "restart", service])
            .status()?;
        if !status.success() {
            return Err(format!("Failed to restart {service}").into());
        }
        Ok(())
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
        std::env::current_exe().map_err(|e| e.into())
    }
}

pub struct RealNetworkOps;

impl NetworkOps for RealNetworkOps {
    fn check_outbound_port25(&self) -> Result<bool, Box<dyn std::error::Error>> {
        use std::net::{TcpStream, ToSocketAddrs};
        use std::time::Duration;

        let target = "gmail-smtp-in.l.google.com:25";
        let addrs: Vec<_> = target.to_socket_addrs()?.collect();
        if addrs.is_empty() {
            return Ok(false);
        }

        match TcpStream::connect_timeout(&addrs[0], Duration::from_secs(10)) {
            Ok(_) => Ok(true),
            Err(_) => Ok(false),
        }
    }

    fn check_inbound_port25(&self) -> Result<bool, Box<dyn std::error::Error>> {
        let resp = std::process::Command::new("curl")
            .args(["-s", "-m", "15", "https://check.aimx.email/probe"])
            .output();

        match resp {
            Ok(output) if output.status.success() => {
                let body = String::from_utf8_lossy(&output.stdout);
                Ok(body.contains("\"reachable\":true") || body.contains("\"reachable\": true"))
            }
            _ => Ok(false),
        }
    }

    fn check_ptr_record(&self) -> Result<Option<String>, Box<dyn std::error::Error>> {
        let ip = self.get_server_ip()?;
        let output = std::process::Command::new("dig")
            .args(["+short", "-x", &ip.to_string()])
            .output()?;
        let result = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if result.is_empty() || result == "." {
            Ok(None)
        } else {
            Ok(Some(result))
        }
    }

    fn get_server_ip(&self) -> Result<IpAddr, Box<dyn std::error::Error>> {
        let output = std::process::Command::new("hostname").arg("-I").output()?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        let ip_str = stdout
            .split_whitespace()
            .next()
            .ok_or("Could not determine server IP")?;
        Ok(ip_str.parse()?)
    }

    fn resolve_mx(&self, domain: &str) -> Result<Vec<String>, Box<dyn std::error::Error>> {
        let output = std::process::Command::new("dig")
            .args(["+short", "MX", domain])
            .output()?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        let records: Vec<String> = stdout
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| l.trim().to_string())
            .collect();
        Ok(records)
    }

    fn resolve_a(&self, domain: &str) -> Result<Vec<IpAddr>, Box<dyn std::error::Error>> {
        let output = std::process::Command::new("dig")
            .args(["+short", "A", domain])
            .output()?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        let addrs: Vec<IpAddr> = stdout
            .lines()
            .filter_map(|l| l.trim().parse().ok())
            .collect();
        Ok(addrs)
    }

    fn resolve_txt(&self, domain: &str) -> Result<Vec<String>, Box<dyn std::error::Error>> {
        let output = std::process::Command::new("dig")
            .args(["+short", "TXT", domain])
            .output()?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        let records: Vec<String> = stdout
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| l.trim().trim_matches('"').to_string())
            .collect();
        Ok(records)
    }
}

pub const COMPATIBLE_PROVIDERS: &[&str] = &[
    "Hetzner Cloud",
    "OVH / OVHcloud",
    "BuyVM (Frantech)",
    "Vultr (on request)",
    "DigitalOcean (on request)",
    "Linode/Akamai (on request)",
];

#[derive(Debug, PartialEq)]
pub enum PreflightResult {
    Pass,
    Fail(String),
    Warn(String),
}

pub fn check_outbound(net: &dyn NetworkOps) -> PreflightResult {
    match net.check_outbound_port25() {
        Ok(true) => PreflightResult::Pass,
        Ok(false) => PreflightResult::Fail(
            "Outbound port 25 is blocked. Your VPS provider may restrict SMTP traffic.".into(),
        ),
        Err(e) => PreflightResult::Fail(format!("Outbound port 25 check failed: {e}")),
    }
}

pub fn check_inbound(net: &dyn NetworkOps) -> PreflightResult {
    match net.check_inbound_port25() {
        Ok(true) => PreflightResult::Pass,
        Ok(false) => PreflightResult::Fail(
            "Inbound port 25 is not reachable. Check your firewall and VPS provider settings."
                .into(),
        ),
        Err(e) => PreflightResult::Fail(format!("Inbound port 25 check failed: {e}")),
    }
}

pub fn check_ptr(net: &dyn NetworkOps) -> PreflightResult {
    match net.check_ptr_record() {
        Ok(Some(ptr)) => {
            println!("  PTR record: {ptr}");
            PreflightResult::Pass
        }
        Ok(None) => PreflightResult::Warn(
            "No PTR (reverse DNS) record found. Set a PTR record at your VPS provider \
             pointing to your domain. This improves deliverability but is not required."
                .into(),
        ),
        Err(e) => PreflightResult::Warn(format!("PTR record check failed: {e}")),
    }
}

pub fn run_preflight(net: &dyn NetworkOps) -> Result<bool, Box<dyn std::error::Error>> {
    println!("Running preflight checks...\n");

    let mut all_pass = true;

    print!("  Outbound port 25... ");
    io::stdout().flush()?;
    match check_outbound(net) {
        PreflightResult::Pass => println!("PASS"),
        PreflightResult::Fail(msg) => {
            println!("FAIL");
            eprintln!("\n  {msg}");
            eprintln!("\n  Compatible VPS providers with port 25 open:");
            for p in COMPATIBLE_PROVIDERS {
                eprintln!("    - {p}");
            }
            all_pass = false;
        }
        PreflightResult::Warn(msg) => println!("WARN: {msg}"),
    }

    print!("  Inbound port 25... ");
    io::stdout().flush()?;
    match check_inbound(net) {
        PreflightResult::Pass => println!("PASS"),
        PreflightResult::Fail(msg) => {
            println!("FAIL");
            eprintln!("\n  {msg}");
            all_pass = false;
        }
        PreflightResult::Warn(msg) => println!("WARN: {msg}"),
    }

    print!("  PTR record... ");
    io::stdout().flush()?;
    match check_ptr(net) {
        PreflightResult::Pass => println!("PASS"),
        PreflightResult::Fail(msg) => {
            println!("FAIL: {msg}");
            all_pass = false;
        }
        PreflightResult::Warn(msg) => println!("WARN\n  {msg}"),
    }

    println!();

    if !all_pass {
        eprintln!("Preflight checks failed. Please resolve the issues above before proceeding.");
    } else {
        println!("All preflight checks passed.");
    }

    Ok(all_pass)
}

pub fn generate_smtpd_conf(domain: &str, aimx_binary: &str) -> String {
    format!(
        r#"# Generated by aimx setup for {domain}
# Backed up original config before overwriting

pki {domain} cert "/etc/ssl/aimx/cert.pem"
pki {domain} key "/etc/ssl/aimx/key.pem"

listen on 0.0.0.0 tls pki {domain}
listen on :: tls pki {domain}

action "deliver" mda "{aimx_binary} ingest %{{rcpt}}"
action "relay" relay

match from any for domain "{domain}" action "deliver"
match for any action "relay"
"#
    )
}

pub fn configure_opensmtpd(
    sys: &dyn SystemOps,
    domain: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    if !sys.is_package_installed("opensmtpd") {
        println!("Installing OpenSMTPD...");
        sys.install_package("opensmtpd")?;
        println!("OpenSMTPD installed.");
    } else {
        println!("OpenSMTPD is already installed.");
    }

    let cert_dir = Path::new("/etc/ssl/aimx");
    if !sys.file_exists(&cert_dir.join("cert.pem")) {
        println!("Generating self-signed TLS certificate...");
        sys.generate_tls_cert(cert_dir, domain)?;
        println!("TLS certificate generated in /etc/ssl/aimx/");
    } else {
        println!("TLS certificate already exists.");
    }

    let smtpd_conf = Path::new("/etc/smtpd.conf");
    if sys.file_exists(smtpd_conf) {
        let timestamp = Utc::now().format("%Y%m%d%H%M%S");
        let backup = PathBuf::from(format!("/etc/smtpd.conf.bak.{timestamp}"));
        let existing = sys.read_file(smtpd_conf)?;
        sys.write_file(&backup, &existing)?;
        println!("Backed up existing smtpd.conf to {}", backup.display());
    }

    let aimx_binary = sys
        .get_aimx_binary_path()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| "/usr/local/bin/aimx".to_string());

    let conf = generate_smtpd_conf(domain, &aimx_binary);
    sys.write_file(smtpd_conf, &conf)?;
    println!("Wrote smtpd.conf for {domain}");

    println!("Restarting OpenSMTPD...");
    sys.restart_service("opensmtpd")?;
    println!("OpenSMTPD restarted.");

    Ok(())
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
    dkim_value: &str,
    dkim_selector: &str,
) -> Vec<DnsRecord> {
    vec![
        DnsRecord {
            record_type: "A".into(),
            name: domain.into(),
            value: server_ip.into(),
        },
        DnsRecord {
            record_type: "MX".into(),
            name: domain.into(),
            value: format!("10 {domain}."),
        },
        DnsRecord {
            record_type: "TXT".into(),
            name: domain.into(),
            value: format!("v=spf1 ip4:{server_ip} -all"),
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
        DnsRecord {
            record_type: "PTR".into(),
            name: server_ip.into(),
            value: format!("{domain}."),
        },
    ]
}

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

pub fn display_dns_guidance(domain: &str, server_ip: &str, dkim_value: &str, dkim_selector: &str) {
    let records = generate_dns_records(domain, server_ip, dkim_value, dkim_selector);
    println!("\nAdd the following DNS records at your domain registrar:\n");
    println!("  TYPE NAME                                          VALUE");
    println!("  ---- --------------------------------------------- -----");
    print!("{}", format_dns_records(&records));
    println!("\nNote: The PTR record is set at your VPS provider, not your domain registrar.");
}

#[derive(Debug, PartialEq)]
pub enum DnsVerifyResult {
    Pass,
    Fail(String),
    Missing(String),
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

pub fn verify_spf(net: &dyn NetworkOps, domain: &str, expected_ip: &str) -> DnsVerifyResult {
    match net.resolve_txt(domain) {
        Ok(records) => {
            let spf: Vec<&String> = records.iter().filter(|r| r.starts_with("v=spf1")).collect();
            if spf.is_empty() {
                return DnsVerifyResult::Missing("No SPF record found".into());
            }
            if spf.iter().any(|r| r.contains(expected_ip)) {
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

pub fn verify_dkim(net: &dyn NetworkOps, domain: &str, selector: &str) -> DnsVerifyResult {
    let dkim_domain = format!("{selector}._domainkey.{domain}");
    match net.resolve_txt(&dkim_domain) {
        Ok(records) => {
            let dkim: Vec<&String> = records.iter().filter(|r| r.contains("v=DKIM1")).collect();
            if dkim.is_empty() {
                DnsVerifyResult::Missing("No DKIM record found".into())
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
                DnsVerifyResult::Missing("No DMARC record found".into())
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
    dkim_selector: &str,
) -> Vec<(String, DnsVerifyResult)> {
    let ip_str = server_ip.to_string();
    vec![
        ("MX".into(), verify_mx(net, domain)),
        ("A".into(), verify_a(net, domain, server_ip)),
        ("SPF".into(), verify_spf(net, domain, &ip_str)),
        ("DKIM".into(), verify_dkim(net, domain, dkim_selector)),
        ("DMARC".into(), verify_dmarc(net, domain)),
    ]
}

pub fn display_dns_verification(results: &[(String, DnsVerifyResult)]) -> bool {
    let mut all_pass = true;
    println!("\nDNS Verification:\n");
    for (name, result) in results {
        match result {
            DnsVerifyResult::Pass => println!("  {name}: PASS"),
            DnsVerifyResult::Fail(msg) => {
                println!("  {name}: FAIL - {msg}");
                all_pass = false;
            }
            DnsVerifyResult::Missing(msg) => {
                println!("  {name}: MISSING - {msg}");
                all_pass = false;
            }
        }
    }
    println!();
    all_pass
}

pub fn mcp_config_snippet(data_dir: &Path) -> String {
    let aimx_path = "/usr/local/bin/aimx";
    let data_dir_str = data_dir.to_string_lossy();

    let args = if data_dir_str == "/var/lib/aimx" {
        r#"["mcp"]"#.to_string()
    } else {
        format!(r#"["--data-dir", "{data_dir_str}", "mcp"]"#)
    };

    format!(
        r#"{{
  "mcpServers": {{
    "email": {{
      "command": "{aimx_path}",
      "args": {args}
    }}
  }}
}}"#
    )
}

pub fn gmail_whitelist_instructions(domain: &str) -> String {
    format!(
        r#"To prevent emails from {domain} landing in spam:

  1. Open Gmail Settings > Filters and Blocked Addresses
  2. Click "Create a new filter"
  3. In the "From" field, enter: *@{domain}
  4. Click "Create filter"
  5. Check "Never send it to Spam"
  6. Click "Create filter"

Alternatively, just reply to an email from {domain} — Gmail will learn it's not spam."#
    )
}

pub fn finalize_setup(
    data_dir: &Path,
    domain: &str,
    dkim_selector: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    std::fs::create_dir_all(data_dir)?;

    let config_path = Config::config_path(data_dir);
    let config = if config_path.exists() {
        let mut cfg = Config::load(&config_path)?;
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
            cfg.save(&config_path)?;
        }
        if !cfg.mailboxes.contains_key("catchall") {
            cfg.mailboxes.insert(
                "catchall".to_string(),
                MailboxConfig {
                    address: format!("*@{domain}"),
                    on_receive: vec![],
                    trust: "none".to_string(),
                    trusted_senders: vec![],
                },
            );
            cfg.save(&config_path)?;
        }
        cfg
    } else {
        let mut mailboxes = HashMap::new();
        mailboxes.insert(
            "catchall".to_string(),
            MailboxConfig {
                address: format!("*@{domain}"),
                on_receive: vec![],
                trust: "none".to_string(),
                trusted_senders: vec![],
            },
        );
        let cfg = Config {
            domain: domain.to_string(),
            data_dir: data_dir.to_path_buf(),
            dkim_selector: dkim_selector.to_string(),
            mailboxes,
        };
        cfg.save(&config_path)?;
        cfg
    };

    let catchall_dir = data_dir.join("catchall");
    std::fs::create_dir_all(&catchall_dir)?;

    let dkim_private = data_dir.join("dkim/private.key");
    if !dkim_private.exists() {
        println!("Generating DKIM keypair...");
        dkim::generate_keypair(data_dir, false)?;
        println!("DKIM keypair generated.");
    } else {
        println!("DKIM keypair already exists.");
    }

    println!("\nSetup complete for {domain}!\n");

    println!("MCP configuration for Claude Code (~/.claude/settings.json):\n");
    println!("{}\n", mcp_config_snippet(&config.data_dir));

    println!("{}\n", gmail_whitelist_instructions(domain));

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

pub fn run_setup(
    domain: &str,
    data_dir: Option<&Path>,
    sys: &dyn SystemOps,
    net: &dyn NetworkOps,
) -> Result<(), Box<dyn std::error::Error>> {
    run_setup_with_verify(domain, data_dir, sys, net, &RealVerifyRunner)
}

pub fn run_setup_with_verify(
    domain: &str,
    data_dir: Option<&Path>,
    sys: &dyn SystemOps,
    net: &dyn NetworkOps,
    verify_runner: &dyn VerifyRunner,
) -> Result<(), Box<dyn std::error::Error>> {
    validate_domain(domain)?;

    println!("aimx setup for {domain}\n");

    let passed = run_preflight(net)?;
    if !passed {
        return Err("Preflight checks failed. Fix the issues above and try again.".into());
    }

    let data_dir = data_dir.unwrap_or(Path::new("/var/lib/aimx"));
    std::fs::create_dir_all(data_dir)?;

    let config_path = Config::config_path(data_dir);
    let dkim_selector = if config_path.exists() {
        Config::load(&config_path)
            .map(|c| c.dkim_selector)
            .unwrap_or_else(|_| "dkim".to_string())
    } else {
        "dkim".to_string()
    };

    finalize_setup(data_dir, domain, &dkim_selector)?;

    configure_opensmtpd(sys, domain)?;

    let server_ip = net.get_server_ip()?;
    let dkim_value = dkim::dns_record_value(data_dir)?;

    display_dns_guidance(domain, &server_ip.to_string(), &dkim_value, &dkim_selector);

    println!("After adding DNS records, press Enter to verify...");
    io::stdout().flush()?;
    let mut input = String::new();
    io::stdin().read_line(&mut input)?;

    let results = verify_all_dns(net, domain, &server_ip, &dkim_selector);
    let all_pass = display_dns_verification(&results);

    if all_pass {
        println!("All DNS records verified. Your email server is ready!");

        println!("\nWould you like to run end-to-end email verification? (y/N) ");
        io::stdout().flush()?;
        let mut verify_input = String::new();
        io::stdin().read_line(&mut verify_input)?;

        if verify_input.trim().eq_ignore_ascii_case("y") {
            match verify_runner.run_verify(Some(data_dir)) {
                Ok(()) => {
                    println!("End-to-end email verification passed!");
                }
                Err(e) => {
                    println!("End-to-end verification did not pass: {e}");
                    println!("This is expected if DNS records are still propagating.");
                    println!("Run `aimx verify` later to retry.");
                }
            }
        } else {
            println!("Skipping end-to-end verification.");
            println!("Run `aimx verify` later to test the full pipeline.");
        }
    } else {
        println!("Some DNS records are not yet correct.");
        println!("DNS propagation can take up to 48 hours.");
        println!("Run `aimx preflight` later to re-check.");
    }

    Ok(())
}

pub fn run_preflight_command(net: &dyn NetworkOps) -> Result<(), Box<dyn std::error::Error>> {
    let passed = run_preflight(net)?;
    if !passed {
        return Err("Preflight checks failed. Fix the issues above and try again.".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mailbox;
    use std::cell::RefCell;
    use std::collections::HashMap;
    use tempfile::TempDir;

    struct MockNetworkOps {
        outbound_port25: bool,
        inbound_port25: bool,
        ptr_record: Option<String>,
        server_ip: IpAddr,
        mx_records: HashMap<String, Vec<String>>,
        a_records: HashMap<String, Vec<IpAddr>>,
        txt_records: HashMap<String, Vec<String>>,
    }

    impl Default for MockNetworkOps {
        fn default() -> Self {
            Self {
                outbound_port25: true,
                inbound_port25: true,
                ptr_record: Some("mail.example.com.".into()),
                server_ip: "1.2.3.4".parse().unwrap(),
                mx_records: HashMap::new(),
                a_records: HashMap::new(),
                txt_records: HashMap::new(),
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
        fn check_ptr_record(&self) -> Result<Option<String>, Box<dyn std::error::Error>> {
            Ok(self.ptr_record.clone())
        }
        fn get_server_ip(&self) -> Result<IpAddr, Box<dyn std::error::Error>> {
            Ok(self.server_ip)
        }
        fn resolve_mx(&self, domain: &str) -> Result<Vec<String>, Box<dyn std::error::Error>> {
            Ok(self.mx_records.get(domain).cloned().unwrap_or_default())
        }
        fn resolve_a(&self, domain: &str) -> Result<Vec<IpAddr>, Box<dyn std::error::Error>> {
            Ok(self.a_records.get(domain).cloned().unwrap_or_default())
        }
        fn resolve_txt(&self, domain: &str) -> Result<Vec<String>, Box<dyn std::error::Error>> {
            Ok(self.txt_records.get(domain).cloned().unwrap_or_default())
        }
    }

    struct MockSystemOps {
        installed_packages: RefCell<Vec<String>>,
        written_files: RefCell<HashMap<PathBuf, String>>,
        existing_files: HashMap<PathBuf, String>,
        restarted_services: RefCell<Vec<String>>,
        package_installed: bool,
    }

    impl Default for MockSystemOps {
        fn default() -> Self {
            Self {
                installed_packages: RefCell::new(vec![]),
                written_files: RefCell::new(HashMap::new()),
                existing_files: HashMap::new(),
                restarted_services: RefCell::new(vec![]),
                package_installed: false,
            }
        }
    }

    impl SystemOps for MockSystemOps {
        fn is_package_installed(&self, _package: &str) -> bool {
            self.package_installed
        }
        fn install_package(&self, package: &str) -> Result<(), Box<dyn std::error::Error>> {
            self.installed_packages
                .borrow_mut()
                .push(package.to_string());
            Ok(())
        }
        fn write_file(&self, path: &Path, content: &str) -> Result<(), Box<dyn std::error::Error>> {
            self.written_files
                .borrow_mut()
                .insert(path.to_path_buf(), content.to_string());
            Ok(())
        }
        fn read_file(&self, path: &Path) -> Result<String, Box<dyn std::error::Error>> {
            self.existing_files
                .get(path)
                .cloned()
                .ok_or_else(|| format!("File not found: {}", path.display()).into())
        }
        fn file_exists(&self, path: &Path) -> bool {
            self.existing_files.contains_key(path)
        }
        fn restart_service(&self, service: &str) -> Result<(), Box<dyn std::error::Error>> {
            self.restarted_services
                .borrow_mut()
                .push(service.to_string());
            Ok(())
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
    }

    #[test]
    fn outbound_pass() {
        let net = MockNetworkOps {
            outbound_port25: true,
            ..Default::default()
        };
        assert_eq!(check_outbound(&net), PreflightResult::Pass);
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
        assert_eq!(check_inbound(&net), PreflightResult::Pass);
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
    fn ptr_pass() {
        let net = MockNetworkOps {
            ptr_record: Some("mail.example.com.".into()),
            ..Default::default()
        };
        assert_eq!(check_ptr(&net), PreflightResult::Pass);
    }

    #[test]
    fn ptr_warn_when_missing() {
        let net = MockNetworkOps {
            ptr_record: None,
            ..Default::default()
        };
        match check_ptr(&net) {
            PreflightResult::Warn(msg) => assert!(msg.contains("PTR")),
            other => panic!("Expected Warn, got {:?}", other),
        }
    }

    #[test]
    fn preflight_all_pass() {
        let net = MockNetworkOps::default();
        let result = run_preflight(&net).unwrap();
        assert!(result);
    }

    #[test]
    fn preflight_fails_on_outbound_blocked() {
        let net = MockNetworkOps {
            outbound_port25: false,
            ..Default::default()
        };
        let result = run_preflight(&net).unwrap();
        assert!(!result);
    }

    #[test]
    fn preflight_fails_on_inbound_blocked() {
        let net = MockNetworkOps {
            inbound_port25: false,
            ..Default::default()
        };
        let result = run_preflight(&net).unwrap();
        assert!(!result);
    }

    #[test]
    fn preflight_passes_with_ptr_warning() {
        let net = MockNetworkOps {
            ptr_record: None,
            ..Default::default()
        };
        let result = run_preflight(&net).unwrap();
        assert!(result);
    }

    #[test]
    fn smtpd_conf_generation() {
        let conf = generate_smtpd_conf("agent.example.com", "/usr/local/bin/aimx");
        assert!(conf.contains("pki agent.example.com cert"));
        assert!(conf.contains("pki agent.example.com key"));
        assert!(conf.contains("listen on 0.0.0.0 tls pki agent.example.com"));
        assert!(conf.contains("listen on :: tls pki agent.example.com"));
        assert!(conf.contains(r#"action "deliver" mda "/usr/local/bin/aimx ingest %{rcpt}""#));
        assert!(conf.contains(r#"action "relay" relay"#));
        assert!(conf.contains(r#"match from any for domain "agent.example.com" action "deliver""#));
        assert!(conf.contains(r#"match for any action "relay""#));
    }

    #[test]
    fn smtpd_conf_uses_correct_domain() {
        let conf = generate_smtpd_conf("mail.custom.org", "/usr/local/bin/aimx");
        assert!(conf.contains("mail.custom.org"));
        assert!(!conf.contains("agent.example.com"));
    }

    #[test]
    fn opensmtpd_installs_when_missing() {
        let sys = MockSystemOps::default();
        configure_opensmtpd(&sys, "example.com").unwrap();
        let installed = sys.installed_packages.borrow();
        assert!(installed.contains(&"opensmtpd".to_string()));
    }

    #[test]
    fn opensmtpd_skips_install_when_present() {
        let sys = MockSystemOps {
            package_installed: true,
            ..Default::default()
        };
        configure_opensmtpd(&sys, "example.com").unwrap();
        let installed = sys.installed_packages.borrow();
        assert!(!installed.contains(&"opensmtpd".to_string()));
    }

    #[test]
    fn opensmtpd_backs_up_existing_config_with_timestamp() {
        let mut existing = HashMap::new();
        existing.insert(PathBuf::from("/etc/smtpd.conf"), "old config".to_string());
        let sys = MockSystemOps {
            existing_files: existing,
            ..Default::default()
        };
        configure_opensmtpd(&sys, "example.com").unwrap();
        let written = sys.written_files.borrow();
        let backup_entry = written
            .iter()
            .find(|(k, _)| k.to_string_lossy().starts_with("/etc/smtpd.conf.bak."));
        assert!(backup_entry.is_some(), "timestamped backup should exist");
        let (path, content) = backup_entry.unwrap();
        assert_eq!(content, "old config");
        let filename = path.to_string_lossy();
        let timestamp = filename.strip_prefix("/etc/smtpd.conf.bak.").unwrap();
        assert_eq!(timestamp.len(), 14, "timestamp should be YYYYMMDDHHmmSS");
        assert!(timestamp.chars().all(|c| c.is_ascii_digit()));
    }

    #[test]
    fn opensmtpd_restarts_service() {
        let sys = MockSystemOps::default();
        configure_opensmtpd(&sys, "example.com").unwrap();
        let restarted = sys.restarted_services.borrow();
        assert!(restarted.contains(&"opensmtpd".to_string()));
    }

    #[test]
    fn dns_record_generation() {
        let records = generate_dns_records(
            "agent.example.com",
            "1.2.3.4",
            "v=DKIM1; k=rsa; p=ABC123",
            "dkim",
        );
        assert_eq!(records.len(), 6);

        assert_eq!(records[0].record_type, "A");
        assert_eq!(records[0].name, "agent.example.com");
        assert_eq!(records[0].value, "1.2.3.4");

        assert_eq!(records[1].record_type, "MX");
        assert_eq!(records[1].value, "10 agent.example.com.");

        assert_eq!(records[2].record_type, "TXT");
        assert!(records[2].value.contains("v=spf1"));
        assert!(records[2].value.contains("1.2.3.4"));

        assert_eq!(records[3].record_type, "TXT");
        assert_eq!(records[3].name, "dkim._domainkey.agent.example.com");
        assert!(records[3].value.contains("DKIM1"));

        assert_eq!(records[4].record_type, "TXT");
        assert_eq!(records[4].name, "_dmarc.agent.example.com");
        assert!(records[4].value.contains("v=DMARC1"));
        assert!(records[4].value.contains("p=reject"));

        assert_eq!(records[5].record_type, "PTR");
        assert_eq!(records[5].name, "1.2.3.4");
        assert_eq!(records[5].value, "agent.example.com.");
    }

    #[test]
    fn dns_record_formatting() {
        let records = generate_dns_records("test.com", "5.6.7.8", "v=DKIM1; k=rsa; p=XYZ", "dkim");
        let formatted = format_dns_records(&records);
        assert!(formatted.contains("A"));
        assert!(formatted.contains("MX"));
        assert!(formatted.contains("TXT"));
        assert!(formatted.contains("PTR"));
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
    fn verify_dkim_pass() {
        let mut net = MockNetworkOps::default();
        net.txt_records.insert(
            "dkim._domainkey.example.com".into(),
            vec!["v=DKIM1; k=rsa; p=ABC123".into()],
        );
        assert_eq!(
            verify_dkim(&net, "example.com", "dkim"),
            DnsVerifyResult::Pass
        );
    }

    #[test]
    fn verify_dkim_missing() {
        let net = MockNetworkOps::default();
        match verify_dkim(&net, "example.com", "dkim") {
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
    fn verify_all_dns_all_pass() {
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

        let results = verify_all_dns(&net, "example.com", &ip, "dkim");
        assert!(results.iter().all(|(_, r)| *r == DnsVerifyResult::Pass));
    }

    #[test]
    fn verify_all_dns_partial_fail() {
        let ip: IpAddr = "1.2.3.4".parse().unwrap();
        let mut net = MockNetworkOps::default();
        net.mx_records
            .insert("example.com".into(), vec!["10 example.com.".into()]);
        // A record missing, SPF missing, etc.

        let results = verify_all_dns(&net, "example.com", &ip, "dkim");
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
        assert!(display_dns_verification(&results));
    }

    #[test]
    fn display_dns_verification_with_failure() {
        let results = vec![
            ("MX".into(), DnsVerifyResult::Pass),
            ("A".into(), DnsVerifyResult::Fail("wrong IP".into())),
        ];
        assert!(!display_dns_verification(&results));
    }

    #[test]
    fn display_dns_verification_with_missing() {
        let results = vec![
            ("MX".into(), DnsVerifyResult::Pass),
            ("SPF".into(), DnsVerifyResult::Missing("No SPF".into())),
        ];
        assert!(!display_dns_verification(&results));
    }

    #[test]
    fn mcp_snippet_default_data_dir() {
        let snippet = mcp_config_snippet(Path::new("/var/lib/aimx"));
        assert!(snippet.contains("\"command\": \"/usr/local/bin/aimx\""));
        assert!(snippet.contains("\"args\": [\"mcp\"]"));
        assert!(!snippet.contains("--data-dir"));
    }

    #[test]
    fn mcp_snippet_custom_data_dir() {
        let snippet = mcp_config_snippet(Path::new("/custom/data"));
        assert!(snippet.contains("--data-dir"));
        assert!(snippet.contains("/custom/data"));
    }

    #[test]
    fn gmail_whitelist_has_domain() {
        let instructions = gmail_whitelist_instructions("agent.example.com");
        assert!(instructions.contains("agent.example.com"));
        assert!(instructions.contains("*@agent.example.com"));
        assert!(instructions.contains("Never send it to Spam"));
    }

    #[test]
    fn finalize_creates_data_dir_and_config() {
        let tmp = TempDir::new().unwrap();
        finalize_setup(tmp.path(), "test.example.com", "dkim").unwrap();

        assert!(Config::config_path(tmp.path()).exists());
        assert!(tmp.path().join("catchall").exists());
        assert!(tmp.path().join("dkim/private.key").exists());
        assert!(tmp.path().join("dkim/public.key").exists());

        let config = Config::load_from_data_dir(tmp.path()).unwrap();
        assert_eq!(config.domain, "test.example.com");
        assert!(config.mailboxes.contains_key("catchall"));
        assert_eq!(config.mailboxes["catchall"].address, "*@test.example.com");
    }

    #[test]
    fn finalize_is_idempotent() {
        let tmp = TempDir::new().unwrap();
        finalize_setup(tmp.path(), "test.example.com", "dkim").unwrap();

        let key1 = std::fs::read_to_string(tmp.path().join("dkim/private.key")).unwrap();

        finalize_setup(tmp.path(), "test.example.com", "dkim").unwrap();

        let key2 = std::fs::read_to_string(tmp.path().join("dkim/private.key")).unwrap();
        assert_eq!(key1, key2);

        let config = Config::load_from_data_dir(tmp.path()).unwrap();
        assert_eq!(config.domain, "test.example.com");
        assert!(config.mailboxes.contains_key("catchall"));
    }

    #[test]
    fn finalize_preserves_existing_mailboxes() {
        let tmp = TempDir::new().unwrap();
        finalize_setup(tmp.path(), "test.example.com", "dkim").unwrap();

        let config = Config::load_from_data_dir(tmp.path()).unwrap();
        mailbox::create_mailbox(&config, "alice").unwrap();

        finalize_setup(tmp.path(), "test.example.com", "dkim").unwrap();

        let config = Config::load_from_data_dir(tmp.path()).unwrap();
        assert!(config.mailboxes.contains_key("alice"));
        assert!(config.mailboxes.contains_key("catchall"));
    }

    #[test]
    fn finalize_updates_domain_if_changed() {
        let tmp = TempDir::new().unwrap();
        finalize_setup(tmp.path(), "old.example.com", "dkim").unwrap();

        finalize_setup(tmp.path(), "new.example.com", "dkim").unwrap();

        let config = Config::load_from_data_dir(tmp.path()).unwrap();
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

    #[test]
    fn preflight_command_returns_err_on_failure() {
        let net = MockNetworkOps {
            outbound_port25: false,
            ..Default::default()
        };
        let result = run_preflight_command(&net);
        assert!(result.is_err());
    }

    #[test]
    fn preflight_command_returns_ok_on_success() {
        let net = MockNetworkOps::default();
        let result = run_preflight_command(&net);
        assert!(result.is_ok());
    }

    struct MockVerifyRunner {
        called: RefCell<bool>,
        should_pass: bool,
    }

    impl MockVerifyRunner {
        fn new(should_pass: bool) -> Self {
            Self {
                called: RefCell::new(false),
                should_pass,
            }
        }

        fn was_called(&self) -> bool {
            *self.called.borrow()
        }
    }

    impl VerifyRunner for MockVerifyRunner {
        fn run_verify(&self, _data_dir: Option<&Path>) -> Result<(), Box<dyn std::error::Error>> {
            *self.called.borrow_mut() = true;
            if self.should_pass {
                Ok(())
            } else {
                Err("verify service unavailable".into())
            }
        }
    }

    #[test]
    fn verify_runner_trait_mock_pass() {
        let runner = MockVerifyRunner::new(true);
        assert!(runner.run_verify(None).is_ok());
        assert!(runner.was_called());
    }

    #[test]
    fn verify_runner_trait_mock_fail() {
        let runner = MockVerifyRunner::new(false);
        let result = runner.run_verify(None);
        assert!(result.is_err());
        assert!(runner.was_called());
    }

    #[test]
    fn setup_verify_step_available() {
        // Verify that run_setup_with_verify accepts a VerifyRunner trait object.
        // We can't fully test it here because it reads stdin, but this
        // compile-time check confirms the trait integration is wired up.
        let runner = MockVerifyRunner::new(true);
        let _: &dyn VerifyRunner = &runner;
        let _fn_ptr: fn(
            &str,
            Option<&std::path::Path>,
            &dyn SystemOps,
            &dyn NetworkOps,
            &dyn VerifyRunner,
        ) -> Result<(), Box<dyn std::error::Error>> = run_setup_with_verify;
        let _ = _fn_ptr;
    }
}
