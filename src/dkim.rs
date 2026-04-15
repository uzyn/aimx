use crate::term;
use base64::Engine;
use rsa::pkcs1::EncodeRsaPrivateKey;
use rsa::pkcs8::{EncodePublicKey, LineEnding};
use rsa::{RsaPrivateKey, RsaPublicKey};
use std::path::Path;

const DKIM_KEY_BITS: usize = 2048;

/// Resolve `<dkim_root>/{private,public}.key`.
///
/// `dkim_root` is treated as the directory containing the DKIM keys
/// themselves — callers should pass `config_dir().join("dkim")` for the
/// v0.2 layout. The legacy `<data_dir>/dkim` layout is no longer written
/// by any production path; only tests now supply arbitrary tempdir roots.
fn dkim_paths(dkim_root: &Path) -> (std::path::PathBuf, std::path::PathBuf) {
    (dkim_root.join("private.key"), dkim_root.join("public.key"))
}

/// Generate a 2048-bit RSA DKIM keypair at `<dkim_root>/{private,public}.key`.
///
/// v0.2 permissions (Sprint 33 S33-3):
/// - Private key: `0o600` (root-only; `aimx serve` reads it in-process)
/// - Public key:  `0o644` (world-readable — it's advertised via DNS)
pub fn generate_keypair(dkim_root: &Path, force: bool) -> Result<(), Box<dyn std::error::Error>> {
    let (private_path, public_path) = dkim_paths(dkim_root);

    if private_path.exists() && !force {
        return Err("DKIM keys already exist. Use --force to overwrite.".into());
    }

    std::fs::create_dir_all(dkim_root)?;

    let mut rng = rsa::rand_core::OsRng;
    let private_key = RsaPrivateKey::new(&mut rng, DKIM_KEY_BITS)?;

    let private_pem = private_key.to_pkcs1_pem(LineEnding::LF)?;
    std::fs::write(&private_path, private_pem.as_bytes())?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&private_path, std::fs::Permissions::from_mode(0o600))?;
    }

    let public_key = RsaPublicKey::from(&private_key);
    let public_pem = public_key.to_public_key_pem(LineEnding::LF)?;
    std::fs::write(&public_path, public_pem.as_bytes())?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&public_path, std::fs::Permissions::from_mode(0o644))?;
    }

    Ok(())
}

pub fn dns_record_value(dkim_root: &Path) -> Result<String, Box<dyn std::error::Error>> {
    let (_, public_path) = dkim_paths(dkim_root);
    let pem = std::fs::read_to_string(&public_path)
        .map_err(|_| "DKIM public key not found. Run `aimx dkim-keygen` first.")?;

    let public_key = if pem.contains("BEGIN RSA PUBLIC KEY") {
        use rsa::pkcs1::DecodeRsaPublicKey;
        RsaPublicKey::from_pkcs1_pem(&pem)?
    } else {
        use rsa::pkcs8::DecodePublicKey;
        RsaPublicKey::from_public_key_pem(&pem)?
    };

    let spki_der = public_key.to_public_key_der()?;
    let b64 = base64::engine::general_purpose::STANDARD.encode(spki_der.as_ref());

    Ok(format!("v=DKIM1; k=rsa; p={b64}"))
}

/// Load the RSA DKIM private key from `<dkim_root>/private.key`.
///
/// v0.2 (Sprint 33 S33-3) tightened the private key to `0o600`. When a
/// non-root process hits `PermissionDenied`, the error message steers
/// the caller back to `aimx send` (which in Sprint 34 delegates to the
/// `aimx serve` UDS socket for DKIM signing) rather than suggesting a
/// permissions hack.
pub fn load_private_key(dkim_root: &Path) -> Result<RsaPrivateKey, Box<dyn std::error::Error>> {
    let (private_path, _) = dkim_paths(dkim_root);
    let pem = std::fs::read_to_string(&private_path).map_err(|e| {
        if e.kind() == std::io::ErrorKind::PermissionDenied {
            format!(
                "DKIM private key is readable only by root. \
                 This command must be invoked by `aimx serve` (root) — \
                 non-root processes must submit mail via `aimx send` instead. \
                 (path: {})",
                private_path.display()
            )
        } else {
            format!(
                "DKIM private key not found at {}: {e}. Run `aimx dkim-keygen` first.",
                private_path.display()
            )
        }
    })?;

    let key = rsa::pkcs1::DecodeRsaPrivateKey::from_pkcs1_pem(&pem)?;
    Ok(key)
}

pub fn sign_message(
    message: &[u8],
    private_key: &RsaPrivateKey,
    domain: &str,
    selector: &str,
) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    use mail_auth::common::crypto::{RsaKey, Sha256};
    use mail_auth::common::headers::HeaderWriter;
    use mail_auth::dkim::{Canonicalization, DkimSigner};
    use rustls_pki_types::{PrivateKeyDer, pem::PemObject};

    let private_pem = private_key.to_pkcs1_pem(LineEnding::LF)?;
    let pem_str: &str = &private_pem;
    let key_der = PrivateKeyDer::from_pem_slice(pem_str.as_bytes())
        .map_err(|e| format!("Failed to parse PEM: {e}"))?;
    let rsa_key = RsaKey::<Sha256>::from_key_der(key_der)
        .map_err(|e| format!("Failed to load RSA key for DKIM signing: {e}"))?;

    let signer = DkimSigner::from_key(rsa_key)
        .domain(domain)
        .selector(selector)
        .headers([
            "From",
            "To",
            "Subject",
            "Date",
            "Message-ID",
            "In-Reply-To",
            "References",
        ])
        .header_canonicalization(Canonicalization::Relaxed)
        .body_canonicalization(Canonicalization::Relaxed);

    let signature = signer
        .sign(message)
        .map_err(|e| format!("DKIM signing failed: {e}"))?;

    let signature_header = signature.to_header();

    let mut signed = Vec::with_capacity(signature_header.len() + message.len());
    signed.extend_from_slice(signature_header.as_bytes());
    signed.extend_from_slice(message);

    Ok(signed)
}

pub fn run_keygen(
    dkim_root: &Path,
    domain: &str,
    selector: &str,
    force: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let (private_path, _) = dkim_paths(dkim_root);
    let already_existed = private_path.exists() && !force;

    if already_existed {
        eprintln!(
            "{} DKIM keys already exist. Use --force to overwrite.",
            term::warn("Warning:")
        );
    } else {
        generate_keypair(dkim_root, force)?;
    }

    let record = dns_record_value(dkim_root)?;

    if !already_existed {
        println!("{}", term::success("DKIM keypair generated successfully."));
        println!();
    }
    println!("Add this DNS TXT record:");
    println!(
        "  {}",
        term::highlight(&format!("{selector}._domainkey.{domain}"))
    );
    println!("  {record}");
    println!();

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsa::traits::PublicKeyParts;
    use tempfile::TempDir;

    #[test]
    fn generate_valid_2048_bit_keypair() {
        let tmp = TempDir::new().unwrap();
        generate_keypair(tmp.path(), false).unwrap();

        let private_pem = std::fs::read_to_string(tmp.path().join("private.key")).unwrap();
        let public_pem = std::fs::read_to_string(tmp.path().join("public.key")).unwrap();

        let private_key: RsaPrivateKey =
            rsa::pkcs1::DecodeRsaPrivateKey::from_pkcs1_pem(&private_pem).unwrap();
        let public_key: rsa::RsaPublicKey =
            rsa::pkcs8::DecodePublicKey::from_public_key_pem(&public_pem).unwrap();

        assert_eq!(private_key.size() * 8, DKIM_KEY_BITS);
        assert_eq!(public_key.size() * 8, DKIM_KEY_BITS);
    }

    #[test]
    fn no_overwrite_without_force() {
        let tmp = TempDir::new().unwrap();
        generate_keypair(tmp.path(), false).unwrap();

        let result = generate_keypair(tmp.path(), false);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("already exist"));
    }

    #[test]
    fn overwrite_with_force() {
        let tmp = TempDir::new().unwrap();
        generate_keypair(tmp.path(), false).unwrap();

        let original = std::fs::read_to_string(tmp.path().join("private.key")).unwrap();

        generate_keypair(tmp.path(), true).unwrap();

        let new = std::fs::read_to_string(tmp.path().join("private.key")).unwrap();

        assert_ne!(original, new);
    }

    #[test]
    fn dns_record_format() {
        let tmp = TempDir::new().unwrap();
        generate_keypair(tmp.path(), false).unwrap();

        let record = dns_record_value(tmp.path()).unwrap();
        assert!(record.starts_with("v=DKIM1; k=rsa; p="));
        assert!(!record.contains('\n'));
        assert!(!record.contains("-----"));
    }

    #[test]
    fn dns_record_missing_key() {
        let tmp = TempDir::new().unwrap();
        let result = dns_record_value(tmp.path());
        assert!(result.is_err());
    }

    #[test]
    fn load_private_key_roundtrip() {
        let tmp = TempDir::new().unwrap();
        generate_keypair(tmp.path(), false).unwrap();

        let key = load_private_key(tmp.path()).unwrap();
        assert_eq!(key.size() * 8, DKIM_KEY_BITS);
    }

    #[test]
    fn load_private_key_missing() {
        let tmp = TempDir::new().unwrap();
        let result = load_private_key(tmp.path());
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("not found"),
            "Error should mention 'not found': {err}"
        );
        assert!(
            err.contains("private.key"),
            "Error should include the file path: {err}"
        );
    }

    #[test]
    fn sign_and_verify_roundtrip() {
        let tmp = TempDir::new().unwrap();
        generate_keypair(tmp.path(), false).unwrap();

        let private_key = load_private_key(tmp.path()).unwrap();

        let message = b"From: test@example.com\r\n\
            To: user@example.com\r\n\
            Subject: Test\r\n\
            Date: Thu, 01 Jan 2025 12:00:00 +0000\r\n\
            Message-ID: <test123@example.com>\r\n\
            \r\n\
            Hello world\r\n";

        let signed = sign_message(message, &private_key, "example.com", "dkim").unwrap();
        let signed_str = String::from_utf8_lossy(&signed);

        assert!(signed_str.contains("DKIM-Signature:"));
        assert!(signed_str.contains("a=rsa-sha256"));
        assert!(signed_str.contains("d=example.com"));
        assert!(signed_str.contains("s=dkim"));

        assert!(signed_str.contains("From: test@example.com"));
    }

    #[test]
    fn sign_missing_key() {
        let tmp = TempDir::new().unwrap();
        let result = load_private_key(tmp.path());
        assert!(result.is_err());
    }

    #[cfg(unix)]
    #[test]
    fn private_key_has_restricted_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = TempDir::new().unwrap();
        generate_keypair(tmp.path(), false).unwrap();

        let metadata = std::fs::metadata(tmp.path().join("private.key")).unwrap();
        let mode = metadata.permissions().mode() & 0o777;
        assert_eq!(
            mode, 0o600,
            "v0.2: DKIM private key must be root-only (0o600) — \
             Sprint 33 reverses the 0o644 relaxation from Sprint 25 \
             because signing moves inside the `aimx serve` daemon."
        );
    }

    #[cfg(unix)]
    #[test]
    fn public_key_is_world_readable() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = TempDir::new().unwrap();
        generate_keypair(tmp.path(), false).unwrap();

        let public_path = tmp.path().join("public.key");
        let metadata = std::fs::metadata(&public_path).unwrap();
        let mode = metadata.permissions().mode() & 0o777;
        assert_eq!(
            mode, 0o644,
            "DKIM public key is advertised via DNS and must stay world-readable"
        );
    }

    #[test]
    fn sign_cryptographic_body_hash_verification() {
        use base64::Engine;
        use sha2::{Digest, Sha256};

        let tmp = TempDir::new().unwrap();
        generate_keypair(tmp.path(), false).unwrap();

        let private_key = load_private_key(tmp.path()).unwrap();

        let message = b"From: test@example.com\r\n\
            To: user@example.com\r\n\
            Subject: Test\r\n\
            Date: Thu, 01 Jan 2025 12:00:00 +0000\r\n\
            Message-ID: <test123@example.com>\r\n\
            \r\n\
            Hello world\r\n";

        let signed = sign_message(message, &private_key, "example.com", "dkim").unwrap();
        let signed_str = String::from_utf8_lossy(&signed);

        let dkim_header = signed_str
            .lines()
            .take_while(|l| !l.is_empty())
            .collect::<Vec<_>>()
            .join("");

        let bh_start = dkim_header
            .find("bh=")
            .expect("bh= not found in DKIM-Signature");
        let bh_value = &dkim_header[bh_start + 3..];
        let bh_end = bh_value.find(';').unwrap_or(bh_value.len());
        let bh_b64 = bh_value[..bh_end].replace([' ', '\t'], "");

        let body_start = signed_str.find("\r\n\r\n").expect("No body separator") + 4;
        let body = &signed.as_slice()[body_start..];

        // Relaxed body canonicalization: reduce trailing whitespace on each line,
        // reduce sequences of WSP to single SP, ignore trailing empty lines
        let body_str = String::from_utf8_lossy(body);
        let mut canonical_body = String::new();
        for line in body_str.split("\r\n") {
            let trimmed = line.split_whitespace().collect::<Vec<_>>().join(" ");
            let trimmed = trimmed.trim_end();
            canonical_body.push_str(trimmed);
            canonical_body.push_str("\r\n");
        }
        // Remove trailing empty lines (but keep one final CRLF)
        while canonical_body.ends_with("\r\n\r\n") {
            canonical_body.truncate(canonical_body.len() - 2);
        }

        let mut hasher = Sha256::new();
        hasher.update(canonical_body.as_bytes());
        let computed_hash = base64::engine::general_purpose::STANDARD.encode(hasher.finalize());

        assert_eq!(
            computed_hash, bh_b64,
            "Body hash mismatch: computed={computed_hash}, signed={bh_b64}"
        );
    }

    #[test]
    fn sign_uses_relaxed_canonicalization() {
        let tmp = TempDir::new().unwrap();
        generate_keypair(tmp.path(), false).unwrap();

        let private_key = load_private_key(tmp.path()).unwrap();

        let message = b"From: test@example.com\r\n\
            To: user@example.com\r\n\
            Subject: Test\r\n\
            Date: Thu, 01 Jan 2025 12:00:00 +0000\r\n\
            Message-ID: <test123@example.com>\r\n\
            \r\n\
            Hello world\r\n";

        let signed = sign_message(message, &private_key, "example.com", "dkim").unwrap();
        let signed_str = String::from_utf8_lossy(&signed);

        assert!(
            signed_str.contains("c=relaxed/relaxed"),
            "Signature must use relaxed/relaxed canonicalization, got: {}",
            signed_str
                .lines()
                .find(|l| l.contains("DKIM-Signature"))
                .unwrap_or("no DKIM header")
        );
    }

    #[test]
    fn sign_has_valid_rsa_signature_bytes() {
        use base64::Engine;

        let tmp = TempDir::new().unwrap();
        generate_keypair(tmp.path(), false).unwrap();

        let private_key = load_private_key(tmp.path()).unwrap();

        let message = b"From: test@example.com\r\n\
            To: user@example.com\r\n\
            Subject: Test\r\n\
            Date: Thu, 01 Jan 2025 12:00:00 +0000\r\n\
            Message-ID: <test123@example.com>\r\n\
            \r\n\
            Hello world\r\n";

        let signed = sign_message(message, &private_key, "example.com", "dkim").unwrap();
        let signed_str = String::from_utf8_lossy(&signed);

        // Collect the full DKIM-Signature header (may span multiple lines)
        let mut dkim_header = String::new();
        let mut in_dkim = false;
        for line in signed_str.lines() {
            if line.starts_with("DKIM-Signature:") {
                in_dkim = true;
                dkim_header.push_str(line);
            } else if in_dkim && (line.starts_with('\t') || line.starts_with(' ')) {
                dkim_header.push_str(line);
            } else if in_dkim {
                break;
            }
        }

        // Extract b= value (comes after " b=" and before ";" or end)
        let b_start = dkim_header.find(" b=").expect("b= not found");
        let b_value = &dkim_header[b_start + 3..];
        let b_end = b_value.find(';').unwrap_or(b_value.len());
        let b_b64 = b_value[..b_end].replace([' ', '\t'], "");
        let sig_bytes = base64::engine::general_purpose::STANDARD
            .decode(&b_b64)
            .expect("b= value must be valid base64");

        // RSA-2048 produces 256-byte signature
        assert_eq!(
            sig_bytes.len(),
            256,
            "RSA-2048 signature should be 256 bytes, got {}",
            sig_bytes.len()
        );
    }
}
