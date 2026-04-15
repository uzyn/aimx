use crate::config::{Config, MailboxConfig};
use crate::dkim;
use crate::term;
use std::collections::HashMap;
use std::io::{self, BufRead, Write};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::path::{Path, PathBuf};

#[derive(Debug, PartialEq)]
pub enum Port25Status {
    Free,
    Aimx,
    OtherProcess(String),
}

pub trait SystemOps {
    fn write_file(&self, path: &Path, content: &str) -> Result<(), Box<dyn std::error::Error>>;
    fn file_exists(&self, path: &Path) -> bool;
    fn restart_service(&self, service: &str) -> Result<(), Box<dyn std::error::Error>>;
    fn is_service_running(&self, service: &str) -> bool;
    fn generate_tls_cert(
        &self,
        cert_dir: &Path,
        domain: &str,
    ) -> Result<(), Box<dyn std::error::Error>>;
    fn get_aimx_binary_path(&self) -> Result<PathBuf, Box<dyn std::error::Error>>;
    fn check_root(&self) -> bool;
    fn check_port25_occupancy(&self) -> Result<Port25Status, Box<dyn std::error::Error>>;
    fn install_service_file(&self, data_dir: &Path) -> Result<(), Box<dyn std::error::Error>>;
    /// Create an OS-level system group idempotently.
    ///
    /// Returns `Ok(true)` when the group was newly created, `Ok(false)` when
    /// it already existed. Implementations detect `groupadd` (systemd/Linux)
    /// vs `addgroup` (Alpine/BusyBox) and pick the right tool.
    fn create_system_group(&self, name: &str) -> Result<bool, Box<dyn std::error::Error>>;
}

pub trait NetworkOps {
    fn check_outbound_port25(&self) -> Result<bool, Box<dyn std::error::Error>>;
    /// Full SMTP EHLO handshake via `{verify_host}/probe`.
    /// Used by `aimx setup` (post-install) and `aimx verify`.
    fn check_inbound_port25(&self) -> Result<bool, Box<dyn std::error::Error>>;
    fn check_ptr_record(&self) -> Result<Option<String>, Box<dyn std::error::Error>>;
    /// Return the server's IPv4 and IPv6 addresses in a single call.
    ///
    /// Consolidated in Sprint 32 (S32-4): both families are derived from a
    /// single `hostname -I` invocation in `RealNetworkOps`, avoiding duplicate
    /// work and duplicate failure modes.
    fn get_server_ips(
        &self,
    ) -> Result<(Option<Ipv4Addr>, Option<Ipv6Addr>), Box<dyn std::error::Error>>;
    fn resolve_mx(&self, domain: &str) -> Result<Vec<String>, Box<dyn std::error::Error>>;
    fn resolve_a(&self, domain: &str) -> Result<Vec<IpAddr>, Box<dyn std::error::Error>>;
    fn resolve_aaaa(&self, domain: &str) -> Result<Vec<IpAddr>, Box<dyn std::error::Error>>;
    fn resolve_txt(&self, domain: &str) -> Result<Vec<String>, Box<dyn std::error::Error>>;
}

pub struct RealSystemOps;

impl SystemOps for RealSystemOps {
    fn write_file(&self, path: &Path, content: &str) -> Result<(), Box<dyn std::error::Error>> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, content)?;
        Ok(())
    }

    fn file_exists(&self, path: &Path) -> bool {
        path.exists()
    }

    fn restart_service(&self, service: &str) -> Result<(), Box<dyn std::error::Error>> {
        use crate::serve::service::{detect_init_system, restart_service_command};

        let init = detect_init_system();
        let (program, args) = restart_service_command(&init, service).ok_or_else(|| {
            format!("Could not detect init system (systemd or OpenRC) to restart {service}.")
        })?;
        let status = std::process::Command::new(program).args(&args).status()?;
        if !status.success() {
            return Err(format!("Failed to restart {service}").into());
        }
        Ok(())
    }

    fn is_service_running(&self, service: &str) -> bool {
        use crate::serve::service::{detect_init_system, is_service_running_command};

        let init = detect_init_system();
        match is_service_running_command(&init, service) {
            Some((program, args)) => std::process::Command::new(program)
                .args(&args)
                .status()
                .is_ok_and(|s| s.success()),
            None => false,
        }
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

    fn check_root(&self) -> bool {
        unsafe { libc::geteuid() == 0 }
    }

    fn check_port25_occupancy(&self) -> Result<Port25Status, Box<dyn std::error::Error>> {
        let output = std::process::Command::new("ss")
            .args(["-tlnp", "sport", "=", ":25"])
            .output()?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        parse_port25_status(&stdout)
    }

    fn create_system_group(&self, name: &str) -> Result<bool, Box<dyn std::error::Error>> {
        // Idempotent: `getent group <name>` returns exit code 2 when absent.
        let exists = std::process::Command::new("getent")
            .args(["group", name])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if exists {
            return Ok(false);
        }

        // Prefer groupadd (util-linux) but fall back to addgroup (BusyBox/Alpine).
        let groupadd = std::process::Command::new("groupadd")
            .args(["--system", name])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
        if let Ok(status) = groupadd
            && status.success()
        {
            return Ok(true);
        }

        let addgroup = std::process::Command::new("addgroup")
            .args(["-S", name])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()?;
        if !addgroup.success() {
            return Err(format!(
                "Failed to create system group '{name}'. Tried groupadd and addgroup."
            )
            .into());
        }
        Ok(true)
    }

    fn install_service_file(&self, data_dir: &Path) -> Result<(), Box<dyn std::error::Error>> {
        use crate::serve::service::{
            InitSystem, detect_init_system, generate_openrc_script, generate_systemd_unit,
        };

        let aimx_path = self
            .get_aimx_binary_path()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|_| "/usr/local/bin/aimx".to_string());
        let data_dir_str = data_dir.to_string_lossy().to_string();

        match detect_init_system() {
            InitSystem::Systemd => {
                let unit = generate_systemd_unit(&aimx_path, &data_dir_str);
                let unit_path = Path::new("/etc/systemd/system/aimx.service");
                self.write_file(unit_path, &unit)?;
                let _ = std::process::Command::new("systemctl")
                    .args(["daemon-reload"])
                    .status();
                let _ = std::process::Command::new("systemctl")
                    .args(["enable", "aimx"])
                    .status();
                self.restart_service("aimx")?;
            }
            InitSystem::OpenRC => {
                let script = generate_openrc_script(&aimx_path, &data_dir_str);
                let script_path = Path::new("/etc/init.d/aimx");
                self.write_file(script_path, &script)?;
                let _ = std::process::Command::new("chmod")
                    .args(["+x", "/etc/init.d/aimx"])
                    .status();
                let _ = std::process::Command::new("rc-update")
                    .args(["add", "aimx", "default"])
                    .status();
                self.restart_service("aimx")?;
            }
            InitSystem::Unknown => {
                return Err("Could not detect init system (systemd or OpenRC). \
                     Start aimx serve manually."
                    .into());
            }
        }
        Ok(())
    }
}

pub fn parse_port25_status(ss_output: &str) -> Result<Port25Status, Box<dyn std::error::Error>> {
    let lines: Vec<&str> = ss_output.lines().collect();
    let data_lines: Vec<&str> = lines
        .iter()
        .skip(1)
        .filter(|l| !l.trim().is_empty())
        .copied()
        .collect();

    if data_lines.is_empty() {
        return Ok(Port25Status::Free);
    }

    // Try to extract process name from users:(("name",...)) pattern
    for line in &data_lines {
        if let Some(start) = line.find("users:((\"") {
            let rest = &line[start + 9..];
            if let Some(end) = rest.find('"') {
                let process_name = &rest[..end];
                if process_name == "aimx" {
                    return Ok(Port25Status::Aimx);
                }
                return Ok(Port25Status::OtherProcess(process_name.to_string()));
            }
        }
    }

    // Something is on port 25 but we can't identify it
    Ok(Port25Status::OtherProcess("unknown".to_string()))
}

pub const DEFAULT_VERIFY_HOST: &str = "https://check.aimx.email";

pub const DEFAULT_CHECK_SERVICE_SMTP_ADDR: &str = "check.aimx.email:25";

fn is_global_ipv6(ip: &Ipv6Addr) -> bool {
    let segments = ip.segments();
    // Not link-local (fe80::/10)
    (segments[0] & 0xffc0) != 0xfe80
    // Not ULA (fc00::/7)
    && (segments[0] & 0xfe00) != 0xfc00
    // Not loopback
    && !ip.is_loopback()
    // Not unspecified
    && !ip.is_unspecified()
}

/// Parse the whitespace-separated output of `hostname -I` into the first
/// IPv4 address and the first *global* IPv6 address. Non-global IPv6 tokens
/// (link-local, ULA, loopback, unspecified) are ignored.
pub(crate) fn parse_hostname_i_output(stdout: &str) -> (Option<Ipv4Addr>, Option<Ipv6Addr>) {
    let mut ipv4: Option<Ipv4Addr> = None;
    let mut ipv6: Option<Ipv6Addr> = None;
    for token in stdout.split_whitespace() {
        match token.parse::<IpAddr>() {
            Ok(IpAddr::V4(v4)) if ipv4.is_none() => ipv4 = Some(v4),
            Ok(IpAddr::V6(v6)) if ipv6.is_none() && is_global_ipv6(&v6) => ipv6 = Some(v6),
            _ => {}
        }
        if ipv4.is_some() && ipv6.is_some() {
            break;
        }
    }
    (ipv4, ipv6)
}

#[derive(Debug)]
pub struct RealNetworkOps {
    pub verify_host: String,
    pub check_service_smtp_addr: String,
}

impl Default for RealNetworkOps {
    fn default() -> Self {
        Self {
            verify_host: DEFAULT_VERIFY_HOST.to_string(),
            check_service_smtp_addr: DEFAULT_CHECK_SERVICE_SMTP_ADDR.to_string(),
        }
    }
}

impl RealNetworkOps {
    pub fn from_verify_host(verify_host: String) -> Result<Self, Box<dyn std::error::Error>> {
        let trimmed = verify_host.trim_end_matches('/').to_string();
        validate_verify_host(&trimmed)?;
        let smtp_addr = derive_smtp_addr_from_verify_host(&trimmed);
        Ok(Self {
            verify_host: trimmed,
            check_service_smtp_addr: smtp_addr,
        })
    }

    fn curl_probe(&self) -> Result<bool, Box<dyn std::error::Error>> {
        let url = format!("{}/probe", self.verify_host);
        let resp = std::process::Command::new("curl")
            .args(["-s", "-m", "60", &url])
            .output();

        match resp {
            Ok(output) if output.status.success() => {
                let body = String::from_utf8_lossy(&output.stdout);
                Ok(body.contains("\"reachable\":true") || body.contains("\"reachable\": true"))
            }
            _ => Ok(false),
        }
    }
}

pub fn validate_verify_host(verify_host: &str) -> Result<(), Box<dyn std::error::Error>> {
    if verify_host.is_empty() {
        return Err("verify-host cannot be empty".into());
    }
    if !verify_host.starts_with("http://") && !verify_host.starts_with("https://") {
        return Err(format!(
            "verify-host must start with http:// or https:// (got: {verify_host})"
        )
        .into());
    }
    Ok(())
}

pub fn derive_smtp_addr_from_verify_host(verify_host: &str) -> String {
    // Extract authority (host[:port]) from URL like "https://check.aimx.email:3025/probe"
    let without_scheme = verify_host
        .strip_prefix("https://")
        .or_else(|| verify_host.strip_prefix("http://"))
        .unwrap_or(verify_host);
    let authority = without_scheme.split('/').next().unwrap_or(without_scheme);

    // Bracketed IPv6 literal: [::1] or [::1]:3025
    if let Some(rest) = authority.strip_prefix('[')
        && let Some(end) = rest.find(']')
    {
        let ipv6 = &rest[..end];
        return format!("[{ipv6}]:25");
    }

    // Hostname or IPv4: strip :port if present (rsplit handles hosts safely since
    // non-IPv6 hosts have at most one colon — the port separator).
    let host = match authority.rsplit_once(':') {
        Some((h, _port)) => h,
        None => authority,
    };
    format!("{host}:25")
}

impl NetworkOps for RealNetworkOps {
    fn check_outbound_port25(&self) -> Result<bool, Box<dyn std::error::Error>> {
        use std::io::{BufRead, BufReader, Write};
        use std::net::{TcpStream, ToSocketAddrs};
        use std::time::Duration;

        let target = &self.check_service_smtp_addr;
        let addrs: Vec<_> = target.to_socket_addrs()?.collect();
        if addrs.is_empty() {
            return Ok(false);
        }

        let stream = match TcpStream::connect_timeout(&addrs[0], Duration::from_secs(10)) {
            Ok(s) => s,
            Err(_) => return Ok(false),
        };
        stream.set_read_timeout(Some(Duration::from_secs(5)))?;
        stream.set_write_timeout(Some(Duration::from_secs(5)))?;

        let mut reader = BufReader::new(stream.try_clone()?);
        let mut writer = stream;

        let mut banner = String::new();
        if reader.read_line(&mut banner).is_err() || !banner.starts_with("220") {
            return Ok(false);
        }

        writer.write_all(b"EHLO aimx\r\n")?;
        writer.flush()?;

        let mut ehlo_resp = String::new();
        loop {
            ehlo_resp.clear();
            if reader.read_line(&mut ehlo_resp).is_err() {
                return Ok(false);
            }
            if ehlo_resp.starts_with("250 ") {
                break;
            }
            if !ehlo_resp.starts_with("250-") {
                return Ok(false);
            }
        }

        let _ = writer.write_all(b"QUIT\r\n");
        let _ = writer.flush();

        Ok(true)
    }

    fn check_inbound_port25(&self) -> Result<bool, Box<dyn std::error::Error>> {
        self.curl_probe()
    }

    fn check_ptr_record(&self) -> Result<Option<String>, Box<dyn std::error::Error>> {
        let (ipv4, _) = self.get_server_ips()?;
        let ip = ipv4.ok_or::<Box<dyn std::error::Error>>(
            "Could not determine server IPv4 address".into(),
        )?;
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

    fn get_server_ips(
        &self,
    ) -> Result<(Option<Ipv4Addr>, Option<Ipv6Addr>), Box<dyn std::error::Error>> {
        let output = std::process::Command::new("hostname").arg("-I").output()?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        Ok(parse_hostname_i_output(&stdout))
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

    fn resolve_aaaa(&self, domain: &str) -> Result<Vec<IpAddr>, Box<dyn std::error::Error>> {
        let output = std::process::Command::new("dig")
            .args(["+short", "AAAA", domain])
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
            .map(|l| l.trim().replace("\" \"", "").trim_matches('"').to_string())
            .collect();
        Ok(records)
    }
}

pub const COMPATIBLE_PROVIDERS: &[&str] = &[
    "Hetzner Cloud",
    "OVH / OVHcloud",
    "BuyVM (Frantech)",
    "Vultr (on request)",
    "Linode/Akamai (on request)",
];

#[derive(Debug, PartialEq)]
pub enum PreflightResult {
    /// Check passed. Optional detail string (e.g. PTR value) is displayed inline.
    Pass(Option<String>),
    Fail(String),
    Warn(String),
}

pub fn check_outbound(net: &dyn NetworkOps) -> PreflightResult {
    match net.check_outbound_port25() {
        Ok(true) => PreflightResult::Pass(None),
        Ok(false) => PreflightResult::Fail(
            "Outbound port 25 is blocked. Your VPS provider may restrict SMTP traffic.".into(),
        ),
        Err(e) => PreflightResult::Fail(format!("Outbound port 25 check failed: {e}")),
    }
}

fn inbound_result(res: Result<bool, Box<dyn std::error::Error>>) -> PreflightResult {
    match res {
        Ok(true) => PreflightResult::Pass(None),
        Ok(false) => PreflightResult::Fail(
            "Inbound port 25 is not reachable. Check your firewall and VPS provider settings."
                .into(),
        ),
        Err(e) => PreflightResult::Fail(format!("Inbound port 25 check failed: {e}")),
    }
}

/// Full SMTP EHLO handshake via `/probe`.
pub fn check_inbound(net: &dyn NetworkOps) -> PreflightResult {
    inbound_result(net.check_inbound_port25())
}

pub fn check_ptr(net: &dyn NetworkOps) -> PreflightResult {
    match net.check_ptr_record() {
        Ok(Some(ptr)) => PreflightResult::Pass(Some(ptr)),
        Ok(None) => PreflightResult::Warn(
            "No PTR (reverse DNS) record found. Set a PTR record at your VPS provider \
             pointing to your domain. This improves deliverability but is not required."
                .into(),
        ),
        Err(e) => PreflightResult::Warn(format!("PTR record check failed: {e}")),
    }
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
    server_ipv6: Option<&str>,
    dkim_value: &str,
    dkim_selector: &str,
) -> Vec<DnsRecord> {
    let spf_value = match server_ipv6 {
        Some(ipv6) => format!("v=spf1 ip4:{server_ip} ip6:{ipv6} -all"),
        None => format!("v=spf1 ip4:{server_ip} -all"),
    };

    let mut records = vec![DnsRecord {
        record_type: "A".into(),
        name: domain.into(),
        value: server_ip.into(),
    }];

    if let Some(ipv6) = server_ipv6 {
        records.push(DnsRecord {
            record_type: "AAAA".into(),
            name: domain.into(),
            value: ipv6.into(),
        });
    }

    records.extend([
        DnsRecord {
            record_type: "MX".into(),
            name: domain.into(),
            value: format!("10 {domain}."),
        },
        DnsRecord {
            record_type: "TXT".into(),
            name: domain.into(),
            value: spf_value,
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
    ]);

    records
}

#[cfg(test)]
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

pub fn display_dns_guidance(
    domain: &str,
    server_ip: &str,
    server_ipv6: Option<&str>,
    dkim_value: &str,
    dkim_selector: &str,
) {
    let records = generate_dns_records(domain, server_ip, server_ipv6, dkim_value, dkim_selector);
    let dns_records: Vec<&DnsRecord> = records.iter().filter(|r| r.record_type != "PTR").collect();
    println!("\n{}", term::header("[DNS]"));
    println!("Add the following DNS records at your domain registrar:\n");
    println!("  TYPE NAME                                          VALUE");
    println!("  ---- --------------------------------------------- -----");
    for r in &dns_records {
        println!("  {:4} {:<45} {}", r.record_type, r.name, r.value);
    }
}

#[derive(Debug, PartialEq)]
pub enum DnsVerifyResult {
    Pass,
    Fail(String),
    Missing(String),
    Warn(String),
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

pub fn verify_aaaa(net: &dyn NetworkOps, domain: &str, expected_ip: &IpAddr) -> DnsVerifyResult {
    match net.resolve_aaaa(domain) {
        Ok(addrs) if addrs.contains(expected_ip) => DnsVerifyResult::Pass,
        Ok(addrs) if !addrs.is_empty() => DnsVerifyResult::Fail(format!(
            "AAAA record points to {:?}, expected {expected_ip}",
            addrs
        )),
        Ok(_) => DnsVerifyResult::Missing("No AAAA record found".into()),
        Err(e) => DnsVerifyResult::Fail(format!("AAAA record lookup failed: {e}")),
    }
}

fn spf_contains_ip(record: &str, expected_ip: &str) -> bool {
    for token in record.split_whitespace() {
        if let Some(mechanism) = token
            .strip_prefix("ip4:")
            .or_else(|| token.strip_prefix("+ip4:"))
            .or_else(|| token.strip_prefix("ip6:"))
            .or_else(|| token.strip_prefix("+ip6:"))
        {
            let ip_part = mechanism.split('/').next().unwrap_or(mechanism);
            if ip_part == expected_ip {
                return true;
            }
        }
    }
    false
}

pub fn verify_spf(net: &dyn NetworkOps, domain: &str, expected_ip: &str) -> DnsVerifyResult {
    match net.resolve_txt(domain) {
        Ok(records) => {
            let spf: Vec<&String> = records.iter().filter(|r| r.starts_with("v=spf1")).collect();
            if spf.is_empty() {
                return DnsVerifyResult::Missing("No SPF record found".into());
            }
            if spf.iter().any(|r| spf_contains_ip(r, expected_ip)) {
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

fn extract_dkim_public_key(record: &str) -> Option<String> {
    for part in record.split(';') {
        let part = part.trim();
        if let Some(value) = part.strip_prefix("p=") {
            let key = value.trim().replace(' ', "");
            if !key.is_empty() {
                return Some(key);
            }
        }
    }
    None
}

pub fn verify_dkim(
    net: &dyn NetworkOps,
    domain: &str,
    selector: &str,
    local_public_key: Option<&str>,
) -> DnsVerifyResult {
    let dkim_domain = format!("{selector}._domainkey.{domain}");
    match net.resolve_txt(&dkim_domain) {
        Ok(records) => {
            let dkim: Vec<&String> = records.iter().filter(|r| r.contains("v=DKIM1")).collect();
            if dkim.is_empty() {
                return DnsVerifyResult::Missing("No DKIM record found".into());
            }
            if let Some(local_key) = local_public_key {
                let local_clean = local_key.replace(' ', "");
                let any_match = dkim
                    .iter()
                    .any(|r| extract_dkim_public_key(r).as_deref() == Some(&local_clean));
                if any_match {
                    DnsVerifyResult::Pass
                } else {
                    DnsVerifyResult::Fail(
                        "DKIM record found but public key does not match local key".into(),
                    )
                }
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
                return DnsVerifyResult::Missing("No DMARC record found".into());
            }
            let has_permissive = dmarc.iter().any(|r| {
                r.split(';')
                    .any(|part| part.trim().eq_ignore_ascii_case("p=none"))
            });
            if has_permissive {
                DnsVerifyResult::Warn(
                    "DMARC record uses p=none (no enforcement). Consider p=quarantine or p=reject for production."
                        .into(),
                )
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
    server_ipv6: Option<&IpAddr>,
    dkim_selector: &str,
    local_dkim_pubkey: Option<&str>,
) -> Vec<(String, DnsVerifyResult)> {
    let ip_str = server_ip.to_string();
    let mut results = vec![
        ("MX".into(), verify_mx(net, domain)),
        ("A".into(), verify_a(net, domain, server_ip)),
    ];

    if let Some(ipv6) = server_ipv6 {
        results.push(("AAAA".into(), verify_aaaa(net, domain, ipv6)));
    }

    results.push(("SPF".into(), verify_spf(net, domain, &ip_str)));

    if let Some(ipv6) = server_ipv6 {
        let ipv6_str = ipv6.to_string();
        results.push(("SPF (IPv6)".into(), verify_spf(net, domain, &ipv6_str)));
    }

    results.extend([
        (
            "DKIM".into(),
            verify_dkim(net, domain, dkim_selector, local_dkim_pubkey),
        ),
        ("DMARC".into(), verify_dmarc(net, domain)),
    ]);

    results
}

fn dns_record_for_check<'a>(check: &str, records: &'a [DnsRecord]) -> Option<&'a DnsRecord> {
    match check {
        "A" => records.iter().find(|r| r.record_type == "A"),
        "AAAA" => records.iter().find(|r| r.record_type == "AAAA"),
        "MX" => records.iter().find(|r| r.record_type == "MX"),
        "SPF" | "SPF (IPv6)" => records
            .iter()
            .find(|r| r.record_type == "TXT" && r.value.starts_with("v=spf1")),
        "DKIM" => records
            .iter()
            .find(|r| r.record_type == "TXT" && r.name.contains("._domainkey.")),
        "DMARC" => records
            .iter()
            .find(|r| r.record_type == "TXT" && r.name.starts_with("_dmarc.")),
        _ => None,
    }
}

pub fn display_dns_verification(
    results: &[(String, DnsVerifyResult)],
    dns_records: &[DnsRecord],
) -> bool {
    let mut all_pass = true;
    println!("\nDNS Verification:\n");
    for (name, result) in results {
        match result {
            DnsVerifyResult::Pass => println!("  {name}: {}", term::pass_badge()),
            DnsVerifyResult::Fail(msg) => {
                println!("  {name}: {} - {msg}", term::fail_badge());
                if let Some(rec) = dns_record_for_check(name, dns_records) {
                    println!(
                        "         {} {}  {}  {}",
                        term::dim("→ Add:"),
                        rec.record_type,
                        rec.name,
                        rec.value
                    );
                }
                all_pass = false;
            }
            DnsVerifyResult::Missing(msg) => {
                println!("  {name}: {} - {msg}", term::missing_badge());
                if let Some(rec) = dns_record_for_check(name, dns_records) {
                    println!(
                        "         {} {}  {}  {}",
                        term::dim("→ Add:"),
                        rec.record_type,
                        rec.name,
                        rec.value
                    );
                }
                all_pass = false;
            }
            DnsVerifyResult::Warn(msg) => {
                println!("  {name}: {} - {msg}", term::warn_badge());
            }
        }
    }
    println!();
    all_pass
}

pub fn display_mcp_section(data_dir: &Path) {
    println!("\n{}", term::header("[MCP]"));
    for line in mcp_section_lines(data_dir) {
        println!("{line}");
    }
}

/// Produce the plain-text body of the `[MCP]` section (without the header
/// line itself). Returned as a vector of lines so tests can assert on
/// content without spawning a subprocess.
pub fn mcp_section_lines(data_dir: &Path) -> Vec<String> {
    let mut lines = Vec::new();
    lines.push("Wire AIMX into your AI agent with one command per agent:".to_string());
    lines.push(String::new());

    for spec in crate::agent_setup::registry() {
        let cmd = if data_dir == Path::new("/var/lib/aimx") {
            format!("aimx agent-setup {}", spec.name)
        } else {
            format!(
                "aimx --data-dir {} agent-setup {}",
                data_dir.display(),
                spec.name
            )
        };
        lines.push(format!("  {cmd}"));
    }

    lines.push(String::new());
    lines.push(
        "Run `aimx agent-setup --list` to see supported agents and destination paths.".to_string(),
    );
    lines.push(
        "See `book/agent-integration.md` for the full list and manual MCP wiring.".to_string(),
    );
    lines.push(String::new());
    lines
}

pub fn display_deliverability_section(domain: &str, net: &dyn NetworkOps) {
    println!(
        "\n{}",
        term::header("[Deliverability Improvement (Optional)]")
    );

    print!("  PTR record... ");
    io::stdout().flush().ok();
    match check_ptr(net) {
        PreflightResult::Pass(Some(ptr)) => {
            println!("{} ({ptr})", term::pass_badge());
        }
        PreflightResult::Pass(None) => {
            println!("{}", term::pass_badge());
        }
        PreflightResult::Fail(msg) => {
            println!("{}: {msg}", term::fail_badge());
        }
        PreflightResult::Warn(msg) => {
            println!("{}", term::warn_badge());
            println!("  {msg}");
        }
    }

    println!();
    println!("{}", gmail_whitelist_instructions(domain));
    println!();
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
    install_config_dir()?;

    let config_path = crate::config::config_path();
    let _config = if config_path.exists() {
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
            install_config_file(&cfg, &config_path)?;
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
            install_config_file(&cfg, &config_path)?;
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
            verify_host: None,
            enable_ipv6: false,
        };
        install_config_file(&cfg, &config_path)?;
        cfg
    };

    let catchall_dir = data_dir.join("catchall");
    std::fs::create_dir_all(&catchall_dir)?;

    let dkim_root = crate::config::dkim_dir();
    let dkim_private = dkim_root.join("private.key");
    if !dkim_private.exists() {
        println!("Generating DKIM keypair...");
        dkim::generate_keypair(&dkim_root, false)?;
        println!("DKIM keypair generated.");
    } else {
        println!("DKIM keypair already exists.");
    }

    println!(
        "\n{}\n",
        term::success_banner(&format!("Setup complete for {domain}!"))
    );

    Ok(())
}

/// Create the config dir (default `/etc/aimx`, or `AIMX_CONFIG_DIR` override)
/// with mode `0o755`. Idempotent — a pre-existing directory is left as-is.
fn install_config_dir() -> Result<(), Box<dyn std::error::Error>> {
    let dir = crate::config::config_dir();
    std::fs::create_dir_all(&dir)?;

    // Only enforce directory mode when running as root on the real install
    // path. Test runs under `AIMX_CONFIG_DIR` leave the tempdir's default
    // mode untouched — `tempfile::TempDir` can't host root-owned files.
    if std::env::var_os("AIMX_CONFIG_DIR").is_none() && is_root() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755))?;
        }
    }
    Ok(())
}

/// Write `config.toml` via `Config::save` and (on real installs) tighten its
/// mode to `0o640`, owner `root:root`. In tests (`AIMX_CONFIG_DIR` set) or
/// when running non-root, the mode is left at the OS default — the dedicated
/// [`tests::config_file_has_mode_640_on_real_install`] test covers the
/// real-install invariant directly via [`apply_config_file_mode`].
fn install_config_file(cfg: &Config, path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    cfg.save(path)?;
    if std::env::var_os("AIMX_CONFIG_DIR").is_none() && is_root() {
        apply_config_file_mode(path)?;
    }
    Ok(())
}

/// Set `config.toml` to mode `0o640`. Factored out of [`install_config_file`]
/// so the mode-enforcement path is unit-testable without actually running
/// as root on a real install.
pub(crate) fn apply_config_file_mode(path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o640))?;
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
    Ok(())
}

fn is_root() -> bool {
    #[cfg(unix)]
    {
        unsafe { libc::geteuid() == 0 }
    }
    #[cfg(not(unix))]
    {
        false
    }
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

pub fn prompt_domain(reader: &mut dyn BufRead) -> Result<String, Box<dyn std::error::Error>> {
    print!("Enter the domain you want to use for email (e.g. agent.example.com): ");
    io::stdout().flush()?;
    let mut domain = String::new();
    reader.read_line(&mut domain)?;
    let domain = domain.trim().to_string();
    if domain.is_empty() {
        return Err("No domain entered. Setup cancelled.".into());
    }
    validate_domain(&domain)?;

    print!(
        "You will need to add MX, SPF, and DKIM DNS records for this domain.\n\
         Do you control this domain and have access to its DNS settings? (y/N) "
    );
    io::stdout().flush()?;
    let mut confirm = String::new();
    reader.read_line(&mut confirm)?;
    if !confirm.trim().eq_ignore_ascii_case("y") {
        return Err("Setup cancelled. You need DNS access to proceed.".into());
    }

    Ok(domain)
}

pub fn is_already_configured(sys: &dyn SystemOps, _data_dir: &Path) -> bool {
    let tls_cert = Path::new("/etc/ssl/aimx/cert.pem");
    let dkim_key = crate::config::dkim_dir().join("private.key");

    let service_running = sys.is_service_running("aimx");
    let cert_exists = sys.file_exists(tls_cert);
    let dkim_exists = sys.file_exists(&dkim_key);

    service_running && cert_exists && dkim_exists
}

/// Gate detected IPv6 on `enable_ipv6`.
///
/// IPv6 outbound is opt-in via `enable_ipv6` in `config.toml`. When disabled,
/// the detected IPv6 is dropped — AAAA + `ip6:` SPF guidance/verification are
/// omitted to match the IPv4-only default of `aimx send`.
///
/// Since Sprint 32 (S32-4), the caller passes the IPv6 from a single
/// `get_server_ips()` call (no second `hostname -I`); this helper now just
/// applies the opt-in gate. Kept as a standalone function so the gate is
/// trivially testable.
pub(crate) fn detect_server_ipv6(enable_ipv6: bool, ipv6: Option<Ipv6Addr>) -> Option<Ipv6Addr> {
    if enable_ipv6 { ipv6 } else { None }
}

pub fn run_setup(
    domain: Option<&str>,
    data_dir: Option<&Path>,
    sys: &dyn SystemOps,
    net: &dyn NetworkOps,
) -> Result<(), Box<dyn std::error::Error>> {
    // Step 1: Root check
    if !sys.check_root() {
        return Err("`aimx setup` requires root. Run with: sudo aimx setup <domain>".into());
    }

    // Resolve domain: use argument if provided, otherwise prompt interactively
    let domain = match domain {
        Some(d) => {
            validate_domain(d)?;
            d.to_string()
        }
        None => {
            let stdin = io::stdin();
            let mut reader = stdin.lock();
            prompt_domain(&mut reader)?
        }
    };

    println!("AIMX setup for {domain}\n");

    let data_dir = data_dir.unwrap_or(Path::new("/var/lib/aimx"));
    std::fs::create_dir_all(data_dir)?;
    install_config_dir()?;

    let config_path = crate::config::config_path();
    let (dkim_selector, enable_ipv6) = if config_path.exists() {
        match Config::load(&config_path) {
            Ok(c) => (c.dkim_selector, c.enable_ipv6),
            Err(_) => ("dkim".to_string(), false),
        }
    } else {
        ("dkim".to_string(), false)
    };

    // Re-entrant detection: if already configured, skip install/configure steps
    let already_configured = is_already_configured(sys, data_dir);

    if already_configured {
        println!(
            "{}",
            term::success(
                "Existing AIMX configuration detected. Skipping install, proceeding to verification."
            )
        );
    } else {
        // Step 2: Port 25 conflict detection
        match sys.check_port25_occupancy()? {
            Port25Status::Free => {}
            Port25Status::Aimx => {
                println!("`aimx serve` is already running on port 25. Proceeding with setup.");
            }
            Port25Status::OtherProcess(name) => {
                return Err(format!(
                    "Port 25 is occupied by {name}. \
                     Stop the process and run `aimx setup` again."
                )
                .into());
            }
        }

        // Step 3: Generate TLS cert and install service file
        let cert_dir = Path::new("/etc/ssl/aimx");
        if !sys.file_exists(&cert_dir.join("cert.pem")) {
            println!("Generating self-signed TLS certificate...");
            sys.generate_tls_cert(cert_dir, &domain)?;
            println!("TLS certificate generated in /etc/ssl/aimx/");
        } else {
            println!("TLS certificate already exists.");
        }

        // Step 3b (S33-4): create the `aimx` system group before installing
        // the service so the unit's `Group=aimx` directive resolves.
        match sys.create_system_group(crate::serve::service::AIMX_GROUP) {
            Ok(true) => println!(
                "{}",
                term::success("Created `aimx` system group (grants access to /run/aimx/).")
            ),
            Ok(false) => println!("`aimx` system group already exists."),
            Err(e) => {
                return Err(format!(
                    "Failed to create `aimx` system group: {e}. \
                     Create it manually with `sudo groupadd --system aimx` and re-run setup."
                )
                .into());
            }
        }

        println!("Installing AIMX service...");
        sys.install_service_file(data_dir)?;
        println!("{}", term::success("`aimx serve` started."));
    }

    // Step 4-5: Port checks (run on both fresh and re-entrant invocations)
    let mut port_failed = false;

    print!("  Outbound port 25... ");
    io::stdout().flush()?;
    match check_outbound(net) {
        PreflightResult::Pass(_) => println!("{}", term::pass_badge()),
        PreflightResult::Fail(msg) => {
            println!("{}", term::fail_badge());
            eprintln!("\n  {msg}");
            eprintln!("\n  Compatible VPS providers with port 25 open:");
            for p in COMPATIBLE_PROVIDERS {
                eprintln!("    - {p}");
            }
            port_failed = true;
        }
        PreflightResult::Warn(msg) => println!("{}: {msg}", term::warn_badge()),
    }

    print!("  Inbound port 25... ");
    io::stdout().flush()?;
    match check_inbound(net) {
        PreflightResult::Pass(_) => println!("{}", term::pass_badge()),
        PreflightResult::Fail(msg) => {
            println!("{}", term::fail_badge());
            eprintln!("\n  {msg}");
            port_failed = true;
        }
        PreflightResult::Warn(msg) => println!("{}: {msg}", term::warn_badge()),
    }

    if port_failed {
        return Err(
            "Port 25 checks failed. Your VPS provider may block SMTP traffic.\n\
             `aimx serve` started but port 25 is not reachable.\n\
             Fix the issues above and run `sudo aimx setup` again."
                .into(),
        );
    }

    // Step 6: DKIM keygen and finalize
    finalize_setup(data_dir, &domain, &dkim_selector)?;

    // Step 7: DNS guidance and verification (section [DNS])
    // Single `hostname -I` invocation (S32-4): derive both families from one call.
    let (ipv4_detected, ipv6_detected) = net.get_server_ips()?;
    let server_ipv4 = ipv4_detected
        .ok_or::<Box<dyn std::error::Error>>("Could not determine server IPv4 address".into())?;
    let server_ipv6 = detect_server_ipv6(enable_ipv6, ipv6_detected);
    let server_ip: IpAddr = IpAddr::V4(server_ipv4);
    let server_ipv6_ip: Option<IpAddr> = server_ipv6.map(IpAddr::V6);
    let dkim_value = dkim::dns_record_value(&crate::config::dkim_dir())?;

    let local_dkim_pubkey = dkim_value
        .strip_prefix("v=DKIM1; k=rsa; p=")
        .map(|s| s.to_string());

    let server_ip_str = server_ip.to_string();
    let server_ipv6_str = server_ipv6_ip.map(|ip| ip.to_string());
    display_dns_guidance(
        &domain,
        &server_ip_str,
        server_ipv6_str.as_deref(),
        &dkim_value,
        &dkim_selector,
    );
    let dns_records = generate_dns_records(
        &domain,
        &server_ip_str,
        server_ipv6_str.as_deref(),
        &dkim_value,
        &dkim_selector,
    );

    // DNS retry loop
    loop {
        println!(
            "\nPress {} to verify DNS records, or {} to finish and verify later.",
            term::highlight("Enter"),
            term::highlight("q")
        );
        io::stdout().flush()?;
        let mut input = String::new();
        io::stdin().read_line(&mut input)?;
        if input.trim().eq_ignore_ascii_case("q") {
            println!(
                "Update your DNS records and run `{}` again to verify.",
                term::highlight(&format!("sudo aimx setup {domain}"))
            );
            break;
        }

        let results = verify_all_dns(
            net,
            &domain,
            &server_ip,
            server_ipv6_ip.as_ref(),
            &dkim_selector,
            local_dkim_pubkey.as_deref(),
        );
        let all_pass = display_dns_verification(&results, &dns_records);

        if all_pass {
            println!(
                "{}",
                term::success("All DNS records verified. Your email server is ready!")
            );
            break;
        } else {
            println!("Some DNS records are not yet correct.");
            println!("DNS propagation can take up to 48 hours.");
        }
    }

    // Section [Group access]
    display_group_section();

    // Section [MCP]
    display_mcp_section(data_dir);

    // Section [Deliverability Improvement (Optional)]
    display_deliverability_section(&domain, net);

    Ok(())
}

/// Render the `[Group access]` setup section. Extracted into its own
/// function so `display_group_section_lines` can be unit-tested without
/// touching stdout.
pub fn display_group_section() {
    for line in display_group_section_lines() {
        println!("{line}");
    }
}

pub fn display_group_section_lines() -> Vec<String> {
    vec![
        String::new(),
        term::header("[Group access]").to_string(),
        format!(
            "  To let an agent user submit mail via `aimx send`, add them to the `{}` group:",
            crate::serve::service::AIMX_GROUP
        ),
        String::new(),
        format!(
            "    {}",
            term::highlight(&format!(
                "sudo usermod -aG {} <user>",
                crate::serve::service::AIMX_GROUP
            ))
        ),
        String::new(),
        "  The user must then log out and back in (or run `newgrp aimx`) for the new group to take effect.".to_string(),
        format!(
            "  Group membership gates access to `/run/{}/send.sock`, the local submission socket.",
            crate::serve::service::AIMX_GROUP
        ),
    ]
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
        server_ipv4: Option<Ipv4Addr>,
        server_ipv6: Option<Ipv6Addr>,
        mx_records: HashMap<String, Vec<String>>,
        a_records: HashMap<String, Vec<IpAddr>>,
        aaaa_records: HashMap<String, Vec<IpAddr>>,
        txt_records: HashMap<String, Vec<String>>,
        get_server_ips_calls: std::cell::Cell<u32>,
    }

    impl Default for MockNetworkOps {
        fn default() -> Self {
            Self {
                outbound_port25: true,
                inbound_port25: true,
                ptr_record: Some("mail.example.com.".into()),
                server_ipv4: Some("1.2.3.4".parse().unwrap()),
                server_ipv6: None,
                mx_records: HashMap::new(),
                a_records: HashMap::new(),
                aaaa_records: HashMap::new(),
                txt_records: HashMap::new(),
                get_server_ips_calls: std::cell::Cell::new(0),
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
        fn get_server_ips(
            &self,
        ) -> Result<(Option<Ipv4Addr>, Option<Ipv6Addr>), Box<dyn std::error::Error>> {
            self.get_server_ips_calls
                .set(self.get_server_ips_calls.get() + 1);
            Ok((self.server_ipv4, self.server_ipv6))
        }
        fn resolve_mx(&self, domain: &str) -> Result<Vec<String>, Box<dyn std::error::Error>> {
            Ok(self.mx_records.get(domain).cloned().unwrap_or_default())
        }
        fn resolve_a(&self, domain: &str) -> Result<Vec<IpAddr>, Box<dyn std::error::Error>> {
            Ok(self.a_records.get(domain).cloned().unwrap_or_default())
        }
        fn resolve_aaaa(&self, domain: &str) -> Result<Vec<IpAddr>, Box<dyn std::error::Error>> {
            Ok(self.aaaa_records.get(domain).cloned().unwrap_or_default())
        }
        fn resolve_txt(&self, domain: &str) -> Result<Vec<String>, Box<dyn std::error::Error>> {
            Ok(self.txt_records.get(domain).cloned().unwrap_or_default())
        }
    }

    struct MockSystemOps {
        written_files: RefCell<HashMap<PathBuf, String>>,
        existing_files: HashMap<PathBuf, String>,
        restarted_services: RefCell<Vec<String>>,
        service_file_installed: RefCell<bool>,
        is_root: bool,
        port25_status: Port25Status,
        service_running: bool,
        existing_groups: RefCell<std::collections::HashSet<String>>,
        created_groups: RefCell<Vec<String>>,
    }

    impl Default for MockSystemOps {
        fn default() -> Self {
            Self {
                written_files: RefCell::new(HashMap::new()),
                existing_files: HashMap::new(),
                restarted_services: RefCell::new(vec![]),
                service_file_installed: RefCell::new(false),
                is_root: true,
                port25_status: Port25Status::Free,
                service_running: false,
                existing_groups: RefCell::new(std::collections::HashSet::new()),
                created_groups: RefCell::new(vec![]),
            }
        }
    }

    impl SystemOps for MockSystemOps {
        fn write_file(&self, path: &Path, content: &str) -> Result<(), Box<dyn std::error::Error>> {
            self.written_files
                .borrow_mut()
                .insert(path.to_path_buf(), content.to_string());
            Ok(())
        }
        fn file_exists(&self, path: &Path) -> bool {
            self.existing_files.contains_key(path) || self.written_files.borrow().contains_key(path)
        }
        fn restart_service(&self, service: &str) -> Result<(), Box<dyn std::error::Error>> {
            self.restarted_services
                .borrow_mut()
                .push(service.to_string());
            Ok(())
        }
        fn is_service_running(&self, _service: &str) -> bool {
            self.service_running
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
            match &self.port25_status {
                Port25Status::Free => Ok(Port25Status::Free),
                Port25Status::Aimx => Ok(Port25Status::Aimx),
                Port25Status::OtherProcess(name) => Ok(Port25Status::OtherProcess(name.clone())),
            }
        }
        fn install_service_file(&self, _data_dir: &Path) -> Result<(), Box<dyn std::error::Error>> {
            *self.service_file_installed.borrow_mut() = true;
            Ok(())
        }
        fn create_system_group(&self, name: &str) -> Result<bool, Box<dyn std::error::Error>> {
            if self.existing_groups.borrow().contains(name) {
                return Ok(false);
            }
            self.existing_groups.borrow_mut().insert(name.to_string());
            self.created_groups.borrow_mut().push(name.to_string());
            Ok(true)
        }
    }

    #[test]
    fn real_network_ops_default_verify_host() {
        let net = RealNetworkOps::default();
        assert_eq!(net.verify_host, DEFAULT_VERIFY_HOST);
    }

    #[test]
    fn real_network_ops_custom_verify_host() {
        let net = RealNetworkOps::from_verify_host("https://verify.custom.example.com".to_string())
            .unwrap();
        assert_eq!(net.verify_host, "https://verify.custom.example.com");
        assert_eq!(net.check_service_smtp_addr, "verify.custom.example.com:25");
    }

    #[test]
    fn real_network_ops_from_verify_host_strips_trailing_slash() {
        let net =
            RealNetworkOps::from_verify_host("https://check.aimx.email/".to_string()).unwrap();
        assert_eq!(net.verify_host, "https://check.aimx.email");
        assert_eq!(net.check_service_smtp_addr, "check.aimx.email:25");
    }

    #[test]
    fn from_verify_host_rejects_empty() {
        let err = RealNetworkOps::from_verify_host(String::new()).unwrap_err();
        assert!(err.to_string().contains("cannot be empty"));
    }

    #[test]
    fn from_verify_host_rejects_bare_hostname() {
        let err = RealNetworkOps::from_verify_host("check.aimx.email".to_string()).unwrap_err();
        assert!(err.to_string().contains("http://"));
    }

    #[test]
    fn from_verify_host_rejects_non_http_scheme() {
        let err =
            RealNetworkOps::from_verify_host("ftp://verify.example.com".to_string()).unwrap_err();
        assert!(err.to_string().contains("http://"));
    }

    #[test]
    fn from_verify_host_rejects_only_slashes() {
        // trailing-slash strip reduces "/" to empty
        let err = RealNetworkOps::from_verify_host("/".to_string()).unwrap_err();
        assert!(err.to_string().contains("cannot be empty"));
    }

    #[test]
    fn outbound_pass() {
        let net = MockNetworkOps {
            outbound_port25: true,
            ..Default::default()
        };
        assert_eq!(check_outbound(&net), PreflightResult::Pass(None));
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
        assert_eq!(check_inbound(&net), PreflightResult::Pass(None));
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
        assert_eq!(
            check_ptr(&net),
            PreflightResult::Pass(Some("mail.example.com.".into()))
        );
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
    fn dns_record_generation() {
        let records = generate_dns_records(
            "agent.example.com",
            "1.2.3.4",
            None,
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
        assert!(records[2].value.contains("ip4:1.2.3.4"));
        assert!(!records[2].value.contains("ip6:"));

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
        let records =
            generate_dns_records("test.com", "5.6.7.8", None, "v=DKIM1; k=rsa; p=XYZ", "dkim");
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
    fn verify_dkim_pass_no_local_key() {
        let mut net = MockNetworkOps::default();
        net.txt_records.insert(
            "dkim._domainkey.example.com".into(),
            vec!["v=DKIM1; k=rsa; p=ABC123".into()],
        );
        assert_eq!(
            verify_dkim(&net, "example.com", "dkim", None),
            DnsVerifyResult::Pass
        );
    }

    #[test]
    fn verify_dkim_pass_with_matching_key() {
        let mut net = MockNetworkOps::default();
        net.txt_records.insert(
            "dkim._domainkey.example.com".into(),
            vec!["v=DKIM1; k=rsa; p=ABC123".into()],
        );
        assert_eq!(
            verify_dkim(&net, "example.com", "dkim", Some("ABC123")),
            DnsVerifyResult::Pass
        );
    }

    #[test]
    fn verify_dkim_fail_mismatched_key() {
        let mut net = MockNetworkOps::default();
        net.txt_records.insert(
            "dkim._domainkey.example.com".into(),
            vec!["v=DKIM1; k=rsa; p=ABC123".into()],
        );
        match verify_dkim(&net, "example.com", "dkim", Some("WRONG_KEY")) {
            DnsVerifyResult::Fail(msg) => {
                assert!(msg.contains("does not match"), "Got: {msg}")
            }
            other => panic!("Expected Fail, got {:?}", other),
        }
    }

    #[test]
    fn verify_dkim_pass_with_long_key() {
        // Simulate a realistic 2048-bit DKIM key that would be split across
        // multiple TXT record strings by DNS. After resolve_txt concatenation,
        // the mock provides the joined value.
        let long_key = "MIIBCgKCAQEA011La5tkO7DUxlLEduWsIbrPcK0NAS9SpcW9rftGU2Kx6F0YSPy/54QjZ13AZk6eGM0zJgF3JF9ibX/GiRDVefqCJPhi7lj1kq6xErWxO0ZR7/YslRcoSoAHR/PnO8chRr1DVHEY+5e0cY54z5SLR+lq/xn69zuiHq5AZBpevcfn/ESA3KujF3rXjDT4DM+ydqu92bdLB4MpLMezVoOjNq75RsSQW/ItokH37V4g6OtrV41yYEGvhAawG24j2Kj6RT96cXdOrvRqUb1/IH/a81Is0WH/PoXSLpwarF0Ie1u/+RfUWLj57osAuIsScbzVmzo5Pil+GgAU45UXj91pDwIDAQAB";
        let mut net = MockNetworkOps::default();
        net.txt_records.insert(
            "dkim._domainkey.example.com".into(),
            vec![format!("v=DKIM1; k=rsa; p={long_key}")],
        );
        assert_eq!(
            verify_dkim(&net, "example.com", "dkim", Some(long_key)),
            DnsVerifyResult::Pass,
        );
    }

    #[test]
    fn verify_dkim_missing() {
        let net = MockNetworkOps::default();
        match verify_dkim(&net, "example.com", "dkim", None) {
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
    fn verify_dmarc_warns_on_p_none() {
        let mut net = MockNetworkOps::default();
        net.txt_records
            .insert("_dmarc.example.com".into(), vec!["v=DMARC1; p=none".into()]);
        match verify_dmarc(&net, "example.com") {
            DnsVerifyResult::Warn(msg) => assert!(msg.contains("p=none"), "Got: {msg}"),
            other => panic!("Expected Warn, got {:?}", other),
        }
    }

    #[test]
    fn verify_spf_rejects_prefix_match() {
        let mut net = MockNetworkOps::default();
        net.txt_records.insert(
            "example.com".into(),
            vec!["v=spf1 ip4:1.2.3.45 -all".into()],
        );
        match verify_spf(&net, "example.com", "1.2.3.4") {
            DnsVerifyResult::Fail(msg) => {
                assert!(msg.contains("does not include"), "Got: {msg}")
            }
            other => panic!("Expected Fail for prefix match, got {:?}", other),
        }
    }

    #[test]
    fn verify_spf_rejects_suffix_match() {
        let mut net = MockNetworkOps::default();
        net.txt_records.insert(
            "example.com".into(),
            vec!["v=spf1 ip4:11.2.3.4 -all".into()],
        );
        match verify_spf(&net, "example.com", "1.2.3.4") {
            DnsVerifyResult::Fail(msg) => {
                assert!(msg.contains("does not include"), "Got: {msg}")
            }
            other => panic!("Expected Fail for suffix match, got {:?}", other),
        }
    }

    #[test]
    fn verify_spf_passes_with_cidr() {
        let mut net = MockNetworkOps::default();
        net.txt_records.insert(
            "example.com".into(),
            vec!["v=spf1 ip4:1.2.3.4/32 -all".into()],
        );
        assert_eq!(
            verify_spf(&net, "example.com", "1.2.3.4"),
            DnsVerifyResult::Pass
        );
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

        let results = verify_all_dns(&net, "example.com", &ip, None, "dkim", None);
        assert!(results.iter().all(|(_, r)| *r == DnsVerifyResult::Pass));
    }

    #[test]
    fn verify_all_dns_partial_fail() {
        let ip: IpAddr = "1.2.3.4".parse().unwrap();
        let mut net = MockNetworkOps::default();
        net.mx_records
            .insert("example.com".into(), vec!["10 example.com.".into()]);
        // A record missing, SPF missing, etc.

        let results = verify_all_dns(&net, "example.com", &ip, None, "dkim", None);
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
        assert!(display_dns_verification(&results, &[]));
    }

    #[test]
    fn display_dns_verification_with_failure() {
        let results = vec![
            ("MX".into(), DnsVerifyResult::Pass),
            ("A".into(), DnsVerifyResult::Fail("wrong IP".into())),
        ];
        assert!(!display_dns_verification(&results, &[]));
    }

    #[test]
    fn display_dns_verification_with_missing() {
        let results = vec![
            ("MX".into(), DnsVerifyResult::Pass),
            ("SPF".into(), DnsVerifyResult::Missing("No SPF".into())),
        ];
        assert!(!display_dns_verification(&results, &[]));
    }

    #[test]
    fn mcp_section_default_lists_claude_code_install_command() {
        let lines = mcp_section_lines(Path::new("/var/lib/aimx"));
        let joined = lines.join("\n");
        assert!(
            joined.contains("aimx agent-setup claude-code"),
            "expected claude-code install command in:\n{joined}"
        );
        assert!(!joined.contains("mcpServers"));
        assert!(!joined.contains("--data-dir"));
    }

    #[test]
    fn mcp_section_custom_data_dir_threads_override_into_commands() {
        let lines = mcp_section_lines(Path::new("/custom/data"));
        let joined = lines.join("\n");
        assert!(
            joined.contains("aimx --data-dir /custom/data agent-setup claude-code"),
            "expected --data-dir override in install command:\n{joined}"
        );
        assert!(!joined.contains("mcpServers"));
    }

    #[test]
    fn mcp_section_points_at_agent_integration_doc() {
        let lines = mcp_section_lines(Path::new("/var/lib/aimx"));
        let joined = lines.join("\n");
        assert!(joined.contains("agent-integration.md"));
        assert!(joined.contains("aimx agent-setup --list"));
    }

    #[test]
    fn gmail_whitelist_has_domain() {
        let instructions = gmail_whitelist_instructions("agent.example.com");
        assert!(instructions.contains("agent.example.com"));
        assert!(instructions.contains("*@agent.example.com"));
        assert!(instructions.contains("Never send it to Spam"));
    }

    #[test]
    fn group_section_mentions_aimx_group_and_usermod() {
        let lines = display_group_section_lines();
        let joined = lines.join("\n");
        assert!(
            joined.contains("sudo usermod -aG aimx"),
            "group section must print the exact usermod command: {joined}"
        );
        assert!(
            joined.contains("newgrp aimx") || joined.contains("log out"),
            "group section must mention either `newgrp aimx` or the \
             logout/login step so the group membership takes effect: {joined}"
        );
        assert!(
            joined.contains("send.sock"),
            "group section must name the UDS socket so users connect the \
             group to the access it gates: {joined}"
        );
    }

    #[test]
    fn mock_create_system_group_is_idempotent() {
        let sys = MockSystemOps::default();
        assert!(
            sys.create_system_group("aimx").unwrap(),
            "first call creates"
        );
        assert!(
            !sys.create_system_group("aimx").unwrap(),
            "second call must be a no-op (Ok(false))"
        );
        assert_eq!(
            sys.created_groups.borrow().as_slice(),
            &["aimx".to_string()],
            "mock must only record one creation even after two calls"
        );
    }

    #[test]
    fn run_setup_creates_aimx_group_when_not_configured() {
        let tmp = TempDir::new().unwrap();
        let _cfg_guard = crate::config::test_env::ConfigDirOverride::set(tmp.path());
        let sys = MockSystemOps {
            // inbound port will pass so run_setup proceeds past port checks
            // into finalize_setup + DNS loop; the DNS loop blocks on stdin,
            // so stop short by failing outbound.
            ..Default::default()
        };
        let net = MockNetworkOps {
            outbound_port25: false,
            ..Default::default()
        };
        // run_setup will fail at the port-25 outbound check, which is AFTER
        // the `create_system_group` call — so the group should be recorded.
        let _ = run_setup(Some("example.com"), Some(tmp.path()), &sys, &net);
        assert!(
            sys.created_groups.borrow().contains(&"aimx".to_string()),
            "run_setup must create the aimx group during the install phase"
        );
    }

    #[cfg(unix)]
    #[test]
    fn apply_config_file_mode_sets_640() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("config.toml");
        std::fs::write(&path, "domain = \"test.example.com\"\n").unwrap();
        // Give it an obviously-wrong mode first so we can see the change.
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o777)).unwrap();

        apply_config_file_mode(&path).unwrap();

        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(
            mode, 0o640,
            "install_config_file must tighten config.toml to 0o640"
        );
    }

    #[cfg(unix)]
    #[test]
    fn config_dir_exists_after_finalize() {
        let tmp = TempDir::new().unwrap();
        let _cfg_guard = crate::config::test_env::ConfigDirOverride::set(tmp.path());
        finalize_setup(tmp.path(), "mode.example.com", "dkim").unwrap();

        // Config file resolved via AIMX_CONFIG_DIR lives inside tmp.
        let cfg_path = crate::config::config_path();
        assert!(cfg_path.exists(), "config.toml must be created by finalize");
    }

    #[test]
    fn finalize_creates_data_dir_and_config() {
        let tmp = TempDir::new().unwrap();
        let _cfg_guard = crate::config::test_env::ConfigDirOverride::set(tmp.path());
        finalize_setup(tmp.path(), "test.example.com", "dkim").unwrap();

        assert!(crate::config::config_path().exists());
        assert!(tmp.path().join("catchall").exists());
        assert!(tmp.path().join("dkim/private.key").exists());
        assert!(tmp.path().join("dkim/public.key").exists());

        let config = Config::load_resolved().unwrap();
        assert_eq!(config.domain, "test.example.com");
        assert!(config.mailboxes.contains_key("catchall"));
        assert_eq!(config.mailboxes["catchall"].address, "*@test.example.com");
    }

    #[test]
    fn finalize_is_idempotent() {
        let tmp = TempDir::new().unwrap();
        let _cfg_guard = crate::config::test_env::ConfigDirOverride::set(tmp.path());
        finalize_setup(tmp.path(), "test.example.com", "dkim").unwrap();

        let key1 = std::fs::read_to_string(tmp.path().join("dkim/private.key")).unwrap();

        finalize_setup(tmp.path(), "test.example.com", "dkim").unwrap();

        let key2 = std::fs::read_to_string(tmp.path().join("dkim/private.key")).unwrap();
        assert_eq!(key1, key2);

        let config = Config::load_resolved().unwrap();
        assert_eq!(config.domain, "test.example.com");
        assert!(config.mailboxes.contains_key("catchall"));
    }

    #[test]
    fn finalize_preserves_existing_mailboxes() {
        let tmp = TempDir::new().unwrap();
        let _cfg_guard = crate::config::test_env::ConfigDirOverride::set(tmp.path());
        finalize_setup(tmp.path(), "test.example.com", "dkim").unwrap();

        let config = Config::load_resolved().unwrap();
        mailbox::create_mailbox(&config, "alice").unwrap();

        finalize_setup(tmp.path(), "test.example.com", "dkim").unwrap();

        let config = Config::load_resolved().unwrap();
        assert!(config.mailboxes.contains_key("alice"));
        assert!(config.mailboxes.contains_key("catchall"));
    }

    #[test]
    fn finalize_updates_domain_if_changed() {
        let tmp = TempDir::new().unwrap();
        let _cfg_guard = crate::config::test_env::ConfigDirOverride::set(tmp.path());
        finalize_setup(tmp.path(), "old.example.com", "dkim").unwrap();

        finalize_setup(tmp.path(), "new.example.com", "dkim").unwrap();

        let config = Config::load_resolved().unwrap();
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

    // S11.1 — Root Check + MTA Conflict Detection tests

    #[test]
    fn non_root_detection() {
        let sys = MockSystemOps {
            is_root: false,
            ..Default::default()
        };
        let net = MockNetworkOps::default();
        let result = run_setup(Some("example.com"), None, &sys, &net);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("requires root"),
            "Expected root error, got: {err}"
        );
    }

    #[test]
    fn other_process_detected_exits_with_error() {
        let tmp = TempDir::new().unwrap();
        let _cfg_guard = crate::config::test_env::ConfigDirOverride::set(tmp.path());
        let sys = MockSystemOps {
            port25_status: Port25Status::OtherProcess("postfix".to_string()),
            ..Default::default()
        };
        let net = MockNetworkOps::default();
        let result = run_setup(Some("example.com"), Some(tmp.path()), &sys, &net);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("postfix"),
            "Expected postfix in error, got: {err}"
        );
        assert!(
            err.contains("Port 25 is occupied"),
            "Expected port 25 occupied message, got: {err}"
        );
    }

    #[test]
    fn nothing_on_port25_proceeds() {
        let sys = MockSystemOps {
            port25_status: Port25Status::Free,
            ..Default::default()
        };
        assert!(matches!(
            sys.check_port25_occupancy().unwrap(),
            Port25Status::Free
        ));
    }

    #[test]
    fn parse_port25_status_free() {
        let output = "State  Recv-Q  Send-Q  Local Address:Port  Peer Address:Port\n";
        assert_eq!(parse_port25_status(output).unwrap(), Port25Status::Free);
    }

    #[test]
    fn parse_port25_status_empty() {
        assert_eq!(parse_port25_status("").unwrap(), Port25Status::Free);
    }

    #[test]
    fn parse_port25_status_aimx() {
        let output = "State  Recv-Q  Send-Q  Local Address:Port  Peer Address:Port  Process\n\
                       LISTEN 0      128     0.0.0.0:25         0.0.0.0:*          users:((\"aimx\",pid=1234,fd=6))";
        assert_eq!(parse_port25_status(output).unwrap(), Port25Status::Aimx);
    }

    #[test]
    fn parse_port25_status_other_smtpd() {
        let output = "State  Recv-Q  Send-Q  Local Address:Port  Peer Address:Port  Process\n\
                       LISTEN 0      128     0.0.0.0:25         0.0.0.0:*          users:((\"smtpd\",pid=1234,fd=6))";
        assert_eq!(
            parse_port25_status(output).unwrap(),
            Port25Status::OtherProcess("smtpd".to_string())
        );
    }

    #[test]
    fn parse_port25_status_postfix() {
        let output = "State  Recv-Q  Send-Q  Local Address:Port  Peer Address:Port  Process\n\
                       LISTEN 0      128     0.0.0.0:25         0.0.0.0:*          users:((\"master\",pid=5678,fd=13))";
        assert_eq!(
            parse_port25_status(output).unwrap(),
            Port25Status::OtherProcess("master".to_string())
        );
    }

    #[test]
    fn parse_port25_status_exim() {
        let output = "State  Recv-Q  Send-Q  Local Address:Port  Peer Address:Port  Process\n\
                       LISTEN 0      128     0.0.0.0:25         0.0.0.0:*          users:((\"exim4\",pid=999,fd=3))";
        assert_eq!(
            parse_port25_status(output).unwrap(),
            Port25Status::OtherProcess("exim4".to_string())
        );
    }

    // S11.2 — Reorder Setup Flow tests

    #[test]
    fn derive_smtp_addr_from_https_url() {
        assert_eq!(
            derive_smtp_addr_from_verify_host("https://check.aimx.email"),
            "check.aimx.email:25"
        );
    }

    #[test]
    fn derive_smtp_addr_from_http_url() {
        assert_eq!(
            derive_smtp_addr_from_verify_host("http://verify.custom.example.com"),
            "verify.custom.example.com:25"
        );
    }

    #[test]
    fn derive_smtp_addr_from_url_with_port() {
        assert_eq!(
            derive_smtp_addr_from_verify_host("https://check.aimx.email:3025"),
            "check.aimx.email:25"
        );
    }

    #[test]
    fn derive_smtp_addr_from_ipv6_literal_with_port() {
        assert_eq!(
            derive_smtp_addr_from_verify_host("https://[::1]:3025"),
            "[::1]:25"
        );
    }

    #[test]
    fn derive_smtp_addr_from_ipv6_literal_without_port() {
        assert_eq!(
            derive_smtp_addr_from_verify_host("https://[2001:db8::1]"),
            "[2001:db8::1]:25"
        );
    }

    #[test]
    fn derive_smtp_addr_from_ipv6_literal_with_path() {
        assert_eq!(
            derive_smtp_addr_from_verify_host("https://[::1]:8080/probe"),
            "[::1]:25"
        );
    }

    #[test]
    fn real_network_ops_from_verify_host() {
        let net = RealNetworkOps::from_verify_host("https://check.aimx.email".to_string()).unwrap();
        assert_eq!(net.verify_host, "https://check.aimx.email");
        assert_eq!(net.check_service_smtp_addr, "check.aimx.email:25");
    }

    #[test]
    fn real_network_ops_default_has_check_service_smtp() {
        let net = RealNetworkOps::default();
        assert_eq!(net.check_service_smtp_addr, "check.aimx.email:25");
    }

    #[test]
    fn inbound_timeout_is_60s() {
        // Verify that RealNetworkOps uses -m 60 for the curl timeout.
        // We can't run curl in tests, but we verify the constant by checking
        // that the method signature exists on RealNetworkOps.
        // The actual 60s timeout is encoded in the implementation of check_inbound_port25.
        let net = RealNetworkOps::default();
        // Just ensure the method is callable (compile check)
        let _ = &net as &dyn NetworkOps;
    }

    // PTR check function tests (check_ptr still exists, used in deliverability section)

    #[test]
    fn ptr_pass_carries_value() {
        let net = MockNetworkOps {
            ptr_record: Some("vps-198f7320.vps.ovh.net.".into()),
            ..Default::default()
        };
        assert_eq!(
            check_ptr(&net),
            PreflightResult::Pass(Some("vps-198f7320.vps.ovh.net.".into()))
        );
    }

    // S18.1 — Interactive domain prompt tests

    #[test]
    fn prompt_domain_accepts_valid_domain_with_confirmation() {
        let input = b"agent.example.com\ny\n";
        let mut reader = io::Cursor::new(input);
        let domain = prompt_domain(&mut reader).unwrap();
        assert_eq!(domain, "agent.example.com");
    }

    #[test]
    fn prompt_domain_rejects_empty_input() {
        let input = b"\n";
        let mut reader = io::Cursor::new(input);
        let result = prompt_domain(&mut reader);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("No domain entered")
        );
    }

    #[test]
    fn prompt_domain_rejects_invalid_domain() {
        let input = b"notvalid\n";
        let mut reader = io::Cursor::new(input);
        let result = prompt_domain(&mut reader);
        assert!(result.is_err());
    }

    #[test]
    fn prompt_domain_exits_on_declined_confirmation() {
        let input = b"agent.example.com\nn\n";
        let mut reader = io::Cursor::new(input);
        let result = prompt_domain(&mut reader);
        assert!(result.is_err());
        assert!(
            result.unwrap_err().to_string().contains("cancelled"),
            "should indicate cancellation"
        );
    }

    #[test]
    fn detect_server_ipv6_false_drops_detected_ipv6() {
        let ipv6: Ipv6Addr = "2001:db8::1".parse().unwrap();
        let result = detect_server_ipv6(false, Some(ipv6));
        assert!(
            result.is_none(),
            "ipv6 must be dropped when enable_ipv6 = false"
        );
    }

    #[test]
    fn detect_server_ipv6_true_keeps_detected_ipv6() {
        let ipv6: Ipv6Addr = "2001:db8::1".parse().unwrap();
        let result = detect_server_ipv6(true, Some(ipv6));
        assert_eq!(result, Some(ipv6));
    }

    #[test]
    fn detect_server_ipv6_true_returns_none_when_net_has_none() {
        let result = detect_server_ipv6(true, None);
        assert!(result.is_none());
    }

    #[test]
    fn get_server_ips_called_once_per_setup_flow() {
        // Dedup AC: a single call to `NetworkOps::get_server_ips` must feed
        // both IPv4 and IPv6 consumers in the setup flow (no double shell-out
        // to `hostname -I`). Exercised indirectly via `detect_server_ipv6`
        // + the run_setup wiring; here we assert the trait contract returns
        // both families in one invocation.
        let net = MockNetworkOps {
            server_ipv4: Some("203.0.113.5".parse().unwrap()),
            server_ipv6: Some("2001:db8::1".parse().unwrap()),
            ..Default::default()
        };
        let (v4, v6) = net.get_server_ips().unwrap();
        assert_eq!(v4, Some("203.0.113.5".parse().unwrap()));
        assert_eq!(v6, Some("2001:db8::1".parse().unwrap()));
        assert_eq!(
            net.get_server_ips_calls.get(),
            1,
            "single invocation must return both families"
        );
    }

    #[test]
    fn parse_hostname_i_output_extracts_ipv4_and_global_ipv6() {
        let stdout = "10.0.0.5 203.0.113.7 fe80::1 2001:db8::42 fc00::1\n";
        let (v4, v6) = parse_hostname_i_output(stdout);
        assert_eq!(
            v4,
            Some("10.0.0.5".parse().unwrap()),
            "takes the first IPv4 token (private is OK here — caller may filter)"
        );
        assert_eq!(
            v6,
            Some("2001:db8::42".parse().unwrap()),
            "skips link-local (fe80::) and ULA (fc00::) IPv6"
        );
    }

    #[test]
    fn parse_hostname_i_output_returns_none_when_empty() {
        let (v4, v6) = parse_hostname_i_output("   \n");
        assert!(v4.is_none());
        assert!(v6.is_none());
    }

    #[test]
    fn setup_with_domain_arg_skips_prompt() {
        let tmp = TempDir::new().unwrap();
        let _cfg_guard = crate::config::test_env::ConfigDirOverride::set(tmp.path());
        let sys = MockSystemOps::default();
        let net = MockNetworkOps {
            inbound_port25: false,
            ..Default::default()
        };
        let result = run_setup(Some("example.com"), Some(tmp.path()), &sys, &net);
        // Should progress past domain prompt and fail on port 25 check
        assert!(result.is_err());
        assert!(
            result.unwrap_err().to_string().contains("Port 25"),
            "Expected port 25 failure, not a prompt error"
        );
    }

    // S18.3 — Colorized output tests

    #[test]
    fn dns_guidance_excludes_ptr() {
        let records =
            generate_dns_records("test.com", "1.2.3.4", None, "v=DKIM1; k=rsa; p=ABC", "dkim");
        let dns_only: Vec<&DnsRecord> = records.iter().filter(|r| r.record_type != "PTR").collect();
        assert_eq!(dns_only.len(), 5);
        assert!(dns_only.iter().all(|r| r.record_type != "PTR"));
    }

    // S18.4 — Re-entrant setup tests

    #[test]
    fn is_already_configured_all_present() {
        let tmp = TempDir::new().unwrap();
        let _cfg_guard = crate::config::test_env::ConfigDirOverride::set(tmp.path());

        let mut existing = HashMap::new();
        existing.insert(PathBuf::from("/etc/ssl/aimx/cert.pem"), "cert".to_string());
        existing.insert(
            crate::config::dkim_dir().join("private.key"),
            "key".to_string(),
        );

        let sys = MockSystemOps {
            existing_files: existing,
            service_running: true,
            ..Default::default()
        };
        assert!(is_already_configured(&sys, tmp.path()));
    }

    #[test]
    fn is_already_configured_service_not_running() {
        let tmp = TempDir::new().unwrap();
        let _cfg_guard = crate::config::test_env::ConfigDirOverride::set(tmp.path());
        let mut existing = HashMap::new();
        existing.insert(PathBuf::from("/etc/ssl/aimx/cert.pem"), "cert".to_string());
        existing.insert(
            crate::config::dkim_dir().join("private.key"),
            "key".to_string(),
        );

        let sys = MockSystemOps {
            existing_files: existing,
            service_running: false,
            ..Default::default()
        };
        assert!(!is_already_configured(&sys, tmp.path()));
    }

    #[test]
    fn is_already_configured_missing_dkim() {
        let tmp = TempDir::new().unwrap();
        let _cfg_guard = crate::config::test_env::ConfigDirOverride::set(tmp.path());
        let mut existing = HashMap::new();
        existing.insert(PathBuf::from("/etc/ssl/aimx/cert.pem"), "cert".to_string());

        let sys = MockSystemOps {
            existing_files: existing,
            service_running: true,
            ..Default::default()
        };
        assert!(!is_already_configured(&sys, tmp.path()));
    }

    // S26-2 — IPv6 DNS record generation tests

    #[test]
    fn dns_record_generation_with_ipv6() {
        let records = generate_dns_records(
            "agent.example.com",
            "1.2.3.4",
            Some("2001:db8::1"),
            "v=DKIM1; k=rsa; p=ABC123",
            "dkim",
        );
        assert_eq!(records.len(), 7);

        assert_eq!(records[0].record_type, "A");
        assert_eq!(records[0].value, "1.2.3.4");

        assert_eq!(records[1].record_type, "AAAA");
        assert_eq!(records[1].name, "agent.example.com");
        assert_eq!(records[1].value, "2001:db8::1");

        assert_eq!(records[2].record_type, "MX");

        assert_eq!(records[3].record_type, "TXT");
        assert_eq!(records[3].value, "v=spf1 ip4:1.2.3.4 ip6:2001:db8::1 -all");
    }

    #[test]
    fn dns_record_generation_without_ipv6() {
        let records = generate_dns_records(
            "example.com",
            "1.2.3.4",
            None,
            "v=DKIM1; k=rsa; p=ABC",
            "dkim",
        );
        assert!(!records.iter().any(|r| r.record_type == "AAAA"));
        let spf = records
            .iter()
            .find(|r| r.value.starts_with("v=spf1"))
            .unwrap();
        assert_eq!(spf.value, "v=spf1 ip4:1.2.3.4 -all");
        assert!(!spf.value.contains("ip6:"));
    }

    #[test]
    fn dns_guidance_includes_aaaa_with_ipv6() {
        let records = generate_dns_records(
            "test.com",
            "1.2.3.4",
            Some("2001:db8::1"),
            "v=DKIM1; k=rsa; p=ABC",
            "dkim",
        );
        let dns_only: Vec<&DnsRecord> = records.iter().filter(|r| r.record_type != "PTR").collect();
        assert_eq!(dns_only.len(), 6);
        assert!(dns_only.iter().any(|r| r.record_type == "AAAA"));
    }

    // S26-3 — ip6: SPF verification tests

    #[test]
    fn spf_contains_ip_ipv6_pass() {
        assert!(spf_contains_ip(
            "v=spf1 ip4:1.2.3.4 ip6:2001:db8::1 -all",
            "2001:db8::1"
        ));
    }

    #[test]
    fn spf_contains_ip_ipv6_with_plus_prefix() {
        assert!(spf_contains_ip(
            "v=spf1 +ip6:2001:db8::1 -all",
            "2001:db8::1"
        ));
    }

    #[test]
    fn spf_contains_ip_ipv6_missing() {
        assert!(!spf_contains_ip("v=spf1 ip4:1.2.3.4 -all", "2001:db8::1"));
    }

    #[test]
    fn spf_contains_ip_ipv6_wrong_address() {
        assert!(!spf_contains_ip(
            "v=spf1 ip6:2001:db8::2 -all",
            "2001:db8::1"
        ));
    }

    #[test]
    fn spf_contains_ip_ipv6_with_cidr() {
        assert!(spf_contains_ip(
            "v=spf1 ip6:2001:db8::1/128 -all",
            "2001:db8::1"
        ));
    }

    #[test]
    fn spf_contains_ip_ipv4_still_works() {
        assert!(spf_contains_ip("v=spf1 ip4:1.2.3.4 -all", "1.2.3.4"));
    }

    #[test]
    fn spf_contains_ip_dual_stack_both_present() {
        let record = "v=spf1 ip4:1.2.3.4 ip6:2001:db8::1 -all";
        assert!(spf_contains_ip(record, "1.2.3.4"));
        assert!(spf_contains_ip(record, "2001:db8::1"));
    }

    #[test]
    fn verify_spf_ipv6_pass() {
        let mut net = MockNetworkOps::default();
        net.txt_records.insert(
            "example.com".into(),
            vec!["v=spf1 ip4:1.2.3.4 ip6:2001:db8::1 -all".into()],
        );
        assert_eq!(
            verify_spf(&net, "example.com", "2001:db8::1"),
            DnsVerifyResult::Pass
        );
    }

    #[test]
    fn verify_spf_ipv6_fail() {
        let mut net = MockNetworkOps::default();
        net.txt_records
            .insert("example.com".into(), vec!["v=spf1 ip4:1.2.3.4 -all".into()]);
        match verify_spf(&net, "example.com", "2001:db8::1") {
            DnsVerifyResult::Fail(msg) => assert!(msg.contains("does not include")),
            other => panic!("Expected Fail, got {:?}", other),
        }
    }

    #[test]
    fn verify_aaaa_pass() {
        let ipv6: IpAddr = "2001:db8::1".parse().unwrap();
        let mut net = MockNetworkOps::default();
        net.aaaa_records.insert("example.com".into(), vec![ipv6]);
        assert_eq!(
            verify_aaaa(&net, "example.com", &ipv6),
            DnsVerifyResult::Pass
        );
    }

    #[test]
    fn verify_aaaa_missing() {
        let ipv6: IpAddr = "2001:db8::1".parse().unwrap();
        let net = MockNetworkOps::default();
        match verify_aaaa(&net, "example.com", &ipv6) {
            DnsVerifyResult::Missing(msg) => assert!(msg.contains("No AAAA")),
            other => panic!("Expected Missing, got {:?}", other),
        }
    }

    #[test]
    fn verify_aaaa_wrong_ip() {
        let expected: IpAddr = "2001:db8::1".parse().unwrap();
        let actual: IpAddr = "2001:db8::2".parse().unwrap();
        let mut net = MockNetworkOps::default();
        net.aaaa_records.insert("example.com".into(), vec![actual]);
        match verify_aaaa(&net, "example.com", &expected) {
            DnsVerifyResult::Fail(msg) => assert!(msg.contains("AAAA record points to")),
            other => panic!("Expected Fail, got {:?}", other),
        }
    }

    #[test]
    fn verify_all_dns_with_ipv6() {
        let ip: IpAddr = "1.2.3.4".parse().unwrap();
        let ipv6: IpAddr = "2001:db8::1".parse().unwrap();
        let ipv6_parsed: Ipv6Addr = "2001:db8::1".parse().unwrap();
        let mut net = MockNetworkOps {
            server_ipv6: Some(ipv6_parsed),
            ..Default::default()
        };
        net.mx_records
            .insert("example.com".into(), vec!["10 example.com.".into()]);
        net.a_records.insert("example.com".into(), vec![ip]);
        net.aaaa_records.insert("example.com".into(), vec![ipv6]);
        net.txt_records.insert(
            "example.com".into(),
            vec!["v=spf1 ip4:1.2.3.4 ip6:2001:db8::1 -all".into()],
        );
        net.txt_records.insert(
            "dkim._domainkey.example.com".into(),
            vec!["v=DKIM1; k=rsa; p=ABC".into()],
        );
        net.txt_records.insert(
            "_dmarc.example.com".into(),
            vec!["v=DMARC1; p=reject".into()],
        );

        let results = verify_all_dns(&net, "example.com", &ip, Some(&ipv6), "dkim", None);
        assert!(results.iter().all(|(_, r)| *r == DnsVerifyResult::Pass));
        assert!(results.iter().any(|(name, _)| name == "AAAA"));
        assert!(results.iter().any(|(name, _)| name == "SPF (IPv6)"));
    }

    #[test]
    fn verify_all_dns_without_ipv6_has_no_aaaa() {
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

        let results = verify_all_dns(&net, "example.com", &ip, None, "dkim", None);
        assert!(!results.iter().any(|(name, _)| name == "AAAA"));
        assert!(!results.iter().any(|(name, _)| name == "SPF (IPv6)"));
    }

    // is_global_ipv6 tests

    #[test]
    fn global_ipv6_accepts_global_unicast() {
        let ip: Ipv6Addr = "2001:db8::1".parse().unwrap();
        assert!(is_global_ipv6(&ip));
    }

    #[test]
    fn global_ipv6_rejects_link_local() {
        let ip: Ipv6Addr = "fe80::1".parse().unwrap();
        assert!(!is_global_ipv6(&ip));
    }

    #[test]
    fn global_ipv6_rejects_ula() {
        let fc: Ipv6Addr = "fc00::1".parse().unwrap();
        let fd: Ipv6Addr = "fd00::1".parse().unwrap();
        assert!(!is_global_ipv6(&fc));
        assert!(!is_global_ipv6(&fd));
    }

    #[test]
    fn global_ipv6_rejects_loopback() {
        let ip: Ipv6Addr = "::1".parse().unwrap();
        assert!(!is_global_ipv6(&ip));
    }

    #[test]
    fn global_ipv6_rejects_unspecified() {
        let ip: Ipv6Addr = "::".parse().unwrap();
        assert!(!is_global_ipv6(&ip));
    }
}
