use std::fs;
use std::io::BufReader;
use std::path::Path;
use std::sync::Arc;
use tokio_rustls::TlsAcceptor;

pub fn build_tls_acceptor(
    cert_path: &Path,
    key_path: &Path,
) -> Result<TlsAcceptor, Box<dyn std::error::Error>> {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

    let cert_pem = fs::read(cert_path)
        .map_err(|e| format!("Failed to read TLS cert at {}: {e}", cert_path.display()))?;
    let key_pem = fs::read(key_path)
        .map_err(|e| format!("Failed to read TLS key at {}: {e}", key_path.display()))?;

    let certs: Vec<_> = rustls_pemfile::certs(&mut BufReader::new(cert_pem.as_slice()))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("Failed to parse TLS certificate: {e}"))?;

    if certs.is_empty() {
        return Err("No certificates found in cert file".into());
    }

    let key = rustls_pemfile::private_key(&mut BufReader::new(key_pem.as_slice()))
        .map_err(|e| format!("Failed to parse TLS private key: {e}"))?
        .ok_or("No private key found in key file")?;

    let config = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .map_err(|e| format!("TLS configuration error: {e}"))?;

    Ok(TlsAcceptor::from(Arc::new(config)))
}

#[cfg(test)]
pub fn generate_test_certs(
    dir: &Path,
) -> Result<(std::path::PathBuf, std::path::PathBuf), Box<dyn std::error::Error>> {
    use std::io::Write;
    use std::process::Command;

    let cert_path = dir.join("cert.pem");
    let key_path = dir.join("key.pem");

    // Generate with proper end-entity extensions (no CA:TRUE) so rustls accepts it
    let output = Command::new("openssl")
        .args([
            "req",
            "-x509",
            "-newkey",
            "rsa:2048",
            "-keyout",
            key_path.to_str().unwrap(),
            "-out",
            cert_path.to_str().unwrap(),
            "-days",
            "1",
            "-nodes",
            "-subj",
            "/CN=localhost",
            "-addext",
            "basicConstraints=critical,CA:FALSE",
            "-addext",
            "subjectAltName=DNS:localhost",
        ])
        .output()?;

    if !output.status.success() {
        std::io::stderr().write_all(&output.stderr)?;
        return Err("Failed to generate test certificates with openssl".into());
    }

    Ok((cert_path, key_path))
}
