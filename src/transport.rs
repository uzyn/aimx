//! Daemon-side outbound SMTP transport.
//!
//! As of Sprint 35, `aimx send` no longer signs or delivers mail — it opens
//! `/run/aimx/send.sock` and hands a composed RFC 5322 message to `aimx serve`.
//! The signing + direct-SMTP delivery code that used to live in
//! `src/send.rs` now lives here, behind the [`MailTransport`] trait so the
//! send handler (`src/send_handler.rs`) can be unit-tested without touching
//! the network.

/// Trait for delivering a signed RFC 5322 message to a recipient's MX.
/// Abstracted so tests can inject a mock.
pub trait MailTransport {
    fn send(
        &self,
        sender: &str,
        recipient: &str,
        message: &[u8],
    ) -> Result<String, Box<dyn std::error::Error>>;
}

/// Real `lettre`-backed transport used by `aimx serve`. IPv4-only by default
/// (see `enable_ipv6`): SOHO / home IPv6 ranges are routinely blocked by
/// large MX providers, so the daemon pins the outbound connection to A
/// records unless the operator opts in via `config.toml`.
pub struct LettreTransport {
    enable_ipv6: bool,
}

/// Outcome of picking a connect target for outbound SMTP.
///
/// Exists so the `enable_ipv6 = false` path can be tested without real DNS.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ConnectTarget {
    /// Connect over this literal (hostname when `enable_ipv6 = true`, or an
    /// IPv4 string when `enable_ipv6 = false` and an A record was found).
    Target(String),
    /// IPv4-only was requested but the MX host has no A record. Caller should
    /// skip this MX so the flag is not silently violated.
    SkipNoIpv4,
}

/// Pure, testable connect-target selection.
///
/// - `enable_ipv6 = true` → always use the hostname; OS picks the family.
/// - `enable_ipv6 = false` + at least one A record → use the first A.
/// - `enable_ipv6 = false` + no A records → `SkipNoIpv4` so the caller can
///   move on to the next MX instead of silently falling through to the
///   hostname (which may resolve to IPv6 and violate the flag).
pub(crate) fn select_connect_target(
    host: &str,
    enable_ipv6: bool,
    ipv4_addrs: &[std::net::Ipv4Addr],
) -> ConnectTarget {
    if enable_ipv6 {
        return ConnectTarget::Target(host.to_string());
    }
    match ipv4_addrs.first() {
        Some(addr) => ConnectTarget::Target(addr.to_string()),
        None => ConnectTarget::SkipNoIpv4,
    }
}

impl LettreTransport {
    pub fn new(enable_ipv6: bool) -> Self {
        Self { enable_ipv6 }
    }

    /// Resolves an MX hostname's A records only (no AAAA).
    ///
    /// Exists specifically to honour the opt-in `enable_ipv6` flag: when the
    /// flag is false we pin the connect target to an IPv4 literal so
    /// `lettre`/the OS cannot silently select an AAAA record.
    fn resolve_ipv4(host: &str) -> Result<Vec<std::net::Ipv4Addr>, Box<dyn std::error::Error>> {
        let rt = tokio::runtime::Handle::try_current();
        match rt {
            Ok(handle) => {
                tokio::task::block_in_place(|| handle.block_on(crate::mx::resolve_a(host)))
            }
            Err(_) => {
                let rt = tokio::runtime::Runtime::new()
                    .map_err(|e| format!("Failed to create runtime: {e}"))?;
                rt.block_on(crate::mx::resolve_a(host))
            }
        }
    }

    fn extract_domain(recipient: &str) -> Result<String, Box<dyn std::error::Error>> {
        let addr = recipient
            .rsplit('<')
            .next()
            .unwrap_or(recipient)
            .trim_end_matches('>');
        addr.split('@')
            .nth(1)
            .map(|d| d.to_string())
            .ok_or_else(|| format!("Invalid recipient address: {recipient}").into())
    }
}

impl MailTransport for LettreTransport {
    fn send(
        &self,
        sender: &str,
        recipient: &str,
        message: &[u8],
    ) -> Result<String, Box<dyn std::error::Error>> {
        let domain = Self::extract_domain(recipient)?;
        let rt = tokio::runtime::Handle::try_current();

        let mx_hosts = match rt {
            Ok(handle) => {
                tokio::task::block_in_place(|| handle.block_on(crate::mx::resolve_mx(&domain)))?
            }
            Err(_) => {
                let rt = tokio::runtime::Runtime::new()
                    .map_err(|e| format!("Failed to create runtime: {e}"))?;
                rt.block_on(crate::mx::resolve_mx(&domain))?
            }
        };

        let sender_addr: lettre::Address = Self::extract_domain(sender).and_then(|_| {
            let bare = sender
                .rsplit('<')
                .next()
                .unwrap_or(sender)
                .trim_end_matches('>');
            bare.parse()
                .map_err(|e| format!("Invalid sender address '{sender}': {e}").into())
        })?;

        let envelope = lettre::address::Envelope::new(
            Some(sender_addr),
            vec![
                recipient
                    .parse()
                    .map_err(|e| format!("Invalid recipient address '{recipient}': {e}"))?,
            ],
        )
        .map_err(|e| format!("Failed to create envelope: {e}"))?;

        deliver_across_mx(&domain, &mx_hosts, |host| {
            self.try_deliver(host, &envelope, message)
        })
    }
}

/// Iterate MX hosts, short-circuiting on the first success. When every MX
/// fails, return a single error that contains *all* per-MX failures (not just
/// the last one) so operators can debug multi-MX outages without tailing logs.
pub(crate) fn deliver_across_mx<F>(
    domain: &str,
    mx_hosts: &[String],
    mut deliver: F,
) -> Result<String, Box<dyn std::error::Error>>
where
    F: FnMut(&str) -> Result<String, Box<dyn std::error::Error>>,
{
    let mut errors: Vec<String> = Vec::new();

    for host in mx_hosts {
        match deliver(host) {
            Ok(server) => return Ok(server),
            Err(e) => {
                errors.push(format!("{host}: {e}"));
            }
        }
    }

    let joined = if errors.is_empty() {
        String::new()
    } else {
        errors.join("; ")
    };
    Err(format!("All MX servers for {domain} unreachable: {joined}").into())
}

impl LettreTransport {
    fn try_deliver(
        &self,
        host: &str,
        envelope: &lettre::address::Envelope,
        message: &[u8],
    ) -> Result<String, Box<dyn std::error::Error>> {
        use lettre::Transport;

        // Note: SNI uses `host` while the connect target may be a bare IPv4
        // literal (IPv4-only mode). This is fine here because
        // `dangerous_accept_invalid_certs(true)` is set — cert name mismatch
        // is accepted. If that flag is ever flipped, SNI and the TLS peer
        // identity would need to be reconciled.
        let tls_params = lettre::transport::smtp::client::TlsParameters::builder(host.to_string())
            .dangerous_accept_invalid_certs(true)
            .build_rustls()
            .map_err(|e| format!("TLS configuration error: {e}"))?;

        let ipv4_addrs = if self.enable_ipv6 {
            Vec::new()
        } else {
            Self::resolve_ipv4(host).unwrap_or_default()
        };

        let connect_target = match select_connect_target(host, self.enable_ipv6, &ipv4_addrs) {
            ConnectTarget::Target(t) => t,
            ConnectTarget::SkipNoIpv4 => {
                return Err(format!("{host}: no A record (enable_ipv6 = false); skipping").into());
            }
        };

        let transport = lettre::SmtpTransport::builder_dangerous(&connect_target)
            .hello_name(lettre::transport::smtp::extension::ClientId::Domain(
                host.to_string(),
            ))
            .port(25)
            .tls(lettre::transport::smtp::client::Tls::Opportunistic(
                tls_params,
            ))
            .timeout(Some(std::time::Duration::from_secs(60)))
            .build();

        transport
            .send_raw(envelope, message)
            .map_err(|e| -> Box<dyn std::error::Error> {
                let msg = e.to_string();
                if msg.contains("Connection refused") {
                    format!("Connection refused by {host}").into()
                } else if msg.contains("timed out") || msg.contains("Timeout") {
                    format!("Connection timed out to {host}").into()
                } else {
                    format!("Rejected by {host}: {msg}").into()
                }
            })?;

        Ok(host.to_string())
    }
}

/// Test-only transport that writes every outbound message to a file and
/// returns a canned success. Enabled by the `AIMX_TEST_MAIL_DROP` env var —
/// when set, `aimx serve` swaps `LettreTransport` for `FileDropTransport`
/// pointing at the given path. Used by the Sprint 35 end-to-end test
/// (which cannot reach real MX inside CI).
pub struct FileDropTransport {
    path: std::path::PathBuf,
}

impl FileDropTransport {
    pub fn new<P: Into<std::path::PathBuf>>(path: P) -> Self {
        Self { path: path.into() }
    }
}

impl MailTransport for FileDropTransport {
    fn send(
        &self,
        _sender: &str,
        _recipient: &str,
        message: &[u8],
    ) -> Result<String, Box<dyn std::error::Error>> {
        use std::io::Write;
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        f.write_all(b"----- AIMX TEST DROP -----\n")?;
        f.write_all(message)?;
        f.write_all(b"\n")?;
        Ok("test-drop".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn select_connect_target_ipv6_enabled_returns_hostname() {
        let target = select_connect_target("mx.example.com", true, &[]);
        assert_eq!(target, ConnectTarget::Target("mx.example.com".to_string()));
    }

    #[test]
    fn select_connect_target_ipv6_enabled_ignores_ipv4_addrs() {
        let addrs = vec!["1.2.3.4".parse().unwrap()];
        let target = select_connect_target("mx.example.com", true, &addrs);
        assert_eq!(target, ConnectTarget::Target("mx.example.com".to_string()));
    }

    #[test]
    fn select_connect_target_ipv4_mode_uses_first_a_record() {
        let addrs: Vec<std::net::Ipv4Addr> = vec![
            "203.0.113.10".parse().unwrap(),
            "203.0.113.11".parse().unwrap(),
        ];
        let target = select_connect_target("mx.example.com", false, &addrs);
        assert_eq!(target, ConnectTarget::Target("203.0.113.10".to_string()));
    }

    #[test]
    fn select_connect_target_ipv4_mode_without_a_record_skips() {
        let target = select_connect_target("aaaa-only.example.com", false, &[]);
        assert_eq!(target, ConnectTarget::SkipNoIpv4);
    }

    #[test]
    fn deliver_across_mx_returns_first_success() {
        let hosts = vec!["mx1.example.com".to_string(), "mx2.example.com".to_string()];
        let result = deliver_across_mx("example.com", &hosts, |host| Ok(host.to_string()));
        assert_eq!(result.unwrap(), "mx1.example.com");
    }

    #[test]
    fn deliver_across_mx_falls_through_to_second() {
        let hosts = vec!["mx1.example.com".to_string(), "mx2.example.com".to_string()];
        let result = deliver_across_mx("example.com", &hosts, |host| {
            if host == "mx1.example.com" {
                Err("connection refused".into())
            } else {
                Ok(host.to_string())
            }
        });
        assert_eq!(result.unwrap(), "mx2.example.com");
    }

    #[test]
    fn deliver_across_mx_collects_all_errors_on_total_failure() {
        let hosts = vec![
            "mx1.example.com".to_string(),
            "mx2.example.com".to_string(),
            "mx3.example.com".to_string(),
        ];
        let result = deliver_across_mx(
            "example.com",
            &hosts,
            |host| -> Result<String, Box<dyn std::error::Error>> {
                Err(format!("{host}-specific failure").into())
            },
        );
        let err = result.unwrap_err().to_string();
        assert!(err.contains("example.com"));
        assert!(
            err.contains("mx1.example.com") && err.contains("mx1.example.com-specific failure"),
            "mx1 error missing from: {err}"
        );
        assert!(
            err.contains("mx2.example.com") && err.contains("mx2.example.com-specific failure"),
            "mx2 error missing from: {err}"
        );
        assert!(
            err.contains("mx3.example.com") && err.contains("mx3.example.com-specific failure"),
            "mx3 error missing from: {err}"
        );
    }

    #[test]
    fn lettre_transport_extract_domain() {
        assert_eq!(
            LettreTransport::extract_domain("user@gmail.com").unwrap(),
            "gmail.com"
        );
        assert_eq!(
            LettreTransport::extract_domain("User <user@gmail.com>").unwrap(),
            "gmail.com"
        );
        assert!(LettreTransport::extract_domain("nodomain").is_err());
    }
}
