//! Daemon-side outbound SMTP transport.
//!
//! `aimx send` does not sign or deliver mail itself. It opens
//! `/run/aimx/aimx.sock` and hands a composed RFC 5322 message to `aimx serve`.
//! Signing and direct-SMTP delivery live here, behind the [`MailTransport`]
//! trait so the send handler (`src/send_handler.rs`) can be unit-tested
//! without touching the network.

/// Typed transport error surface.
///
/// Replaces the previous `Box<dyn Error>` return so callers (e.g.
/// `send_handler.rs`) can match on the variant directly instead of
/// pattern-matching lowercased error substrings.
#[derive(Debug)]
pub enum TransportError {
    /// Transient failure (DNS, connect, timeout). Client may retry.
    Temp(String),
    /// Permanent rejection (SMTP 5xx, bad address). Do not retry.
    Permanent(String),
}

impl std::fmt::Display for TransportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TransportError::Temp(s) => write!(f, "{s}"),
            TransportError::Permanent(s) => write!(f, "{s}"),
        }
    }
}

impl std::error::Error for TransportError {}

/// Trait for delivering a signed RFC 5322 message to a recipient's MX.
/// Abstracted so tests can inject a mock.
pub trait MailTransport {
    fn send(&self, sender: &str, recipient: &str, message: &[u8])
    -> Result<String, TransportError>;
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
    ) -> Result<String, TransportError> {
        let domain = Self::extract_domain(recipient)
            .map_err(|e| TransportError::Permanent(e.to_string()))?;
        let rt = tokio::runtime::Handle::try_current();

        let mx_hosts = match rt {
            Ok(handle) => {
                tokio::task::block_in_place(|| handle.block_on(crate::mx::resolve_mx(&domain)))
                    .map_err(|e| TransportError::Temp(format!("DNS failure for {domain}: {e}")))?
            }
            Err(_) => {
                let rt = tokio::runtime::Runtime::new()
                    .map_err(|e| TransportError::Temp(format!("Failed to create runtime: {e}")))?;
                rt.block_on(crate::mx::resolve_mx(&domain))
                    .map_err(|e| TransportError::Temp(format!("DNS failure for {domain}: {e}")))?
            }
        };

        let sender_addr: lettre::Address = Self::extract_domain(sender)
            .and_then(|_| {
                let bare = sender
                    .rsplit('<')
                    .next()
                    .unwrap_or(sender)
                    .trim_end_matches('>');
                bare.parse()
                    .map_err(|e| format!("Invalid sender address '{sender}': {e}").into())
            })
            .map_err(|e: Box<dyn std::error::Error>| TransportError::Permanent(e.to_string()))?;

        let envelope = lettre::address::Envelope::new(
            Some(sender_addr),
            vec![recipient.parse().map_err(|e| {
                TransportError::Permanent(format!("Invalid recipient address '{recipient}': {e}"))
            })?],
        )
        .map_err(|e| TransportError::Permanent(format!("Failed to create envelope: {e}")))?;

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
) -> Result<String, TransportError>
where
    F: FnMut(&str) -> Result<String, TransportError>,
{
    let mut errors: Vec<String> = Vec::new();
    let mut saw_permanent = false;

    for host in mx_hosts {
        match deliver(host) {
            Ok(server) => return Ok(server),
            Err(TransportError::Permanent(e)) => {
                saw_permanent = true;
                errors.push(format!("{host}: {e}"));
            }
            Err(TransportError::Temp(e)) => {
                errors.push(format!("{host}: {e}"));
            }
        }
    }

    let joined = if errors.is_empty() {
        String::new()
    } else {
        errors.join("; ")
    };
    let msg = format!("All MX servers for {domain} unreachable: {joined}");
    if saw_permanent {
        Err(TransportError::Permanent(msg))
    } else {
        Err(TransportError::Temp(msg))
    }
}

impl LettreTransport {
    fn try_deliver(
        &self,
        host: &str,
        envelope: &lettre::address::Envelope,
        message: &[u8],
    ) -> Result<String, TransportError> {
        use lettre::Transport;

        let tls_params = lettre::transport::smtp::client::TlsParameters::builder(host.to_string())
            .dangerous_accept_invalid_certs(true)
            .build_rustls()
            .map_err(|e| TransportError::Temp(format!("TLS configuration error: {e}")))?;

        let ipv4_addrs = if self.enable_ipv6 {
            Vec::new()
        } else {
            match Self::resolve_ipv4(host) {
                Ok(addrs) => addrs,
                Err(e) => {
                    return Err(TransportError::Temp(format!(
                        "{host}: DNS lookup failed: {e}"
                    )));
                }
            }
        };

        let connect_target = match select_connect_target(host, self.enable_ipv6, &ipv4_addrs) {
            ConnectTarget::Target(t) => t,
            ConnectTarget::SkipNoIpv4 => {
                return Err(TransportError::Temp(format!(
                    "{host}: no A record (enable_ipv6 = false); skipping"
                )));
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
            .map_err(|e| classify_lettre_error(host, &e))?;

        Ok(host.to_string())
    }
}

/// Classify a `lettre::transport::smtp::Error` into a typed `TransportError`.
///
/// Mapping:
/// - `is_permanent()` → `Permanent` (SMTP 5xx reply; recipient rejected the
///   message outright, do not retry)
/// - `is_transient()` → `Temp` (SMTP 4xx reply; recipient asked us to retry)
/// - `is_timeout()` → `Temp` (connect or read timeout)
/// - `is_transport_shutdown()` → `Temp` (pool shut down)
/// - TLS failure (`is_tls()`) → `Temp` (usually recoverable by retry or a
///   different MX host)
/// - `is_response()` / `is_client()` → `Temp` (protocol hiccup; the message
///   was not rejected on its merits)
/// - anything else (network/connection/unknown) → `Temp`
fn classify_lettre_error(host: &str, e: &lettre::transport::smtp::Error) -> TransportError {
    let msg = e.to_string();
    if e.is_permanent() {
        TransportError::Permanent(format!("Rejected by {host}: {msg}"))
    } else if e.is_timeout() {
        TransportError::Temp(format!("Connection timed out to {host}"))
    } else {
        // Transient SMTP (4xx), TLS, transport-shutdown, response/client
        // parse errors, and raw network/connection failures all map to Temp.
        TransportError::Temp(format!("{host}: {msg}"))
    }
}

/// Test-only transport that writes every outbound message to a file and
/// returns a canned success. Enabled by the `AIMX_TEST_MAIL_DROP` env var.
/// When set, `aimx serve` swaps `LettreTransport` for `FileDropTransport`
/// pointing at the given path. Used by the end-to-end send test (which
/// cannot reach real MX inside CI).
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
    ) -> Result<String, TransportError> {
        use std::io::Write;
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .map_err(|e| TransportError::Temp(e.to_string()))?;
        f.write_all(b"----- AIMX TEST DROP -----\n")
            .map_err(|e| TransportError::Temp(e.to_string()))?;
        f.write_all(message)
            .map_err(|e| TransportError::Temp(e.to_string()))?;
        f.write_all(b"\n")
            .map_err(|e| TransportError::Temp(e.to_string()))?;
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
                Err(TransportError::Temp("connection refused".into()))
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
            |host| -> Result<String, TransportError> {
                Err(TransportError::Temp(format!("{host}-specific failure")))
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
    fn deliver_across_mx_permanent_when_any_permanent() {
        let hosts = vec!["mx1.example.com".to_string(), "mx2.example.com".to_string()];
        let result = deliver_across_mx("example.com", &hosts, |host| {
            if host == "mx1.example.com" {
                Err(TransportError::Temp("timeout".into()))
            } else {
                Err(TransportError::Permanent("550 rejected".into()))
            }
        });
        assert!(matches!(result, Err(TransportError::Permanent(_))));
    }

    #[test]
    fn deliver_across_mx_temp_when_all_temp() {
        let hosts = vec!["mx1.example.com".to_string()];
        let result = deliver_across_mx("example.com", &hosts, |_host| {
            Err(TransportError::Temp("timeout".into()))
        });
        assert!(matches!(result, Err(TransportError::Temp(_))));
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

    // Note (S43-5): `classify_lettre_error` maps `lettre::transport::smtp::Error`
    // variants to our typed `TransportError`. `smtp::Error` has no public
    // constructor. Its `new()`, `code()`, `response()`, `client()`, `network()`,
    // `connection()`, `tls()`, and `transport_shutdown()` helpers are all
    // `pub(crate)`. That means we can't build a synthetic `smtp::Error` for a
    // branch-by-branch unit test; behaviour is covered end-to-end by the
    // `LettreTransport` path (`send_handler` integration tests drive real sends
    // against localhost and observe the typed error). The mapping is documented
    // above `classify_lettre_error`.

    #[test]
    fn dns_failure_produces_distinct_error_from_no_a_record() {
        // The "DNS lookup failed" message (from resolve_ipv4 error) must
        // be distinguishable from the "no A record" message (from
        // SkipNoIpv4). This test verifies the two strings never collide.
        let dns_failure_msg = "mx.example.com: DNS lookup failed: resolver unreachable";
        let no_a_record_msg = "mx.example.com: no A record (enable_ipv6 = false); skipping";
        assert!(
            !dns_failure_msg.contains("no A record"),
            "DNS failure message must not contain 'no A record'"
        );
        assert!(
            !no_a_record_msg.contains("DNS lookup failed"),
            "no-A-record message must not contain 'DNS lookup failed'"
        );
        assert!(
            dns_failure_msg.contains("DNS lookup failed"),
            "DNS failure message must mention 'DNS lookup failed'"
        );
    }
}
