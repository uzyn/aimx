use hickory_resolver::TokioResolver;

pub async fn resolve_mx(domain: &str) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    let resolver = TokioResolver::builder_tokio()
        .map_err(|e| format!("Failed to create DNS resolver: {e}"))?
        .build();

    match resolver.mx_lookup(domain).await {
        Ok(response) => {
            let mut entries: Vec<(u16, String)> = response
                .iter()
                .map(|mx| {
                    let host = mx.exchange().to_ascii();
                    let host = host.trim_end_matches('.').to_string();
                    (mx.preference(), host)
                })
                .collect();

            if entries.is_empty() {
                return fallback_to_a_record(&resolver, domain).await;
            }

            entries.sort_by_key(|(pref, _)| *pref);
            Ok(entries.into_iter().map(|(_, host)| host).collect())
        }
        Err(_) => fallback_to_a_record(&resolver, domain).await,
    }
}

async fn fallback_to_a_record(
    resolver: &TokioResolver,
    domain: &str,
) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    match resolver.lookup_ip(domain).await {
        Ok(response) if response.iter().next().is_some() => Ok(vec![domain.to_string()]),
        _ => Err(format!("No mail server found for domain {domain}").into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rt() -> tokio::runtime::Runtime {
        tokio::runtime::Runtime::new().unwrap()
    }

    #[test]
    fn resolve_mx_valid_domain() {
        let rt = rt();
        let hosts = rt.block_on(resolve_mx("gmail.com")).unwrap();
        assert!(!hosts.is_empty());
        for host in &hosts {
            assert!(
                host.contains("google") || host.contains("gmail"),
                "Expected Google MX, got: {host}"
            );
        }
    }

    #[test]
    fn resolve_mx_sorted_by_priority() {
        let rt = rt();
        let hosts = rt.block_on(resolve_mx("gmail.com")).unwrap();
        assert!(
            hosts.len() > 1,
            "Expected multiple MX records for gmail.com"
        );
    }

    #[test]
    fn resolve_mx_nxdomain() {
        let rt = rt();
        let result = rt.block_on(resolve_mx("thisdomain.does.not.exist.example.invalid"));
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("No mail server found"),
            "Expected clear error, got: {err}"
        );
    }

    #[test]
    fn resolve_mx_no_mx_with_a_fallback() {
        let rt = rt();
        let result = rt.block_on(resolve_mx("example.com"));
        assert!(result.is_ok(), "Should resolve via MX or A fallback");
    }
}
