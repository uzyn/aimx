use crate::term;
use base64::Engine;
use rsa::pkcs1::EncodeRsaPrivateKey;
use rsa::pkcs8::{EncodePublicKey, LineEnding};
use rsa::{RsaPrivateKey, RsaPublicKey};
use std::path::Path;

const DKIM_KEY_BITS: usize = 2048;

/// Wrap an `io::Error` from the DKIM write path with a message naming the
/// target directory and suggesting `sudo` or the `AIMX_CONFIG_DIR` override.
///
/// A hard root check is deliberately avoided: `AIMX_CONFIG_DIR` is the
/// supported non-root path (used by tests and dev loops) and pointing that
/// at a user-writable tempdir must continue to work without `sudo`.
fn wrap_dkim_write_error(dkim_root: &Path, err: std::io::Error) -> Box<dyn std::error::Error> {
    if err.kind() == std::io::ErrorKind::PermissionDenied {
        format!(
            "Permission denied writing to {}. \
             Run `sudo aimx dkim-keygen`, or set `AIMX_CONFIG_DIR=<writable-path>` \
             and rerun `aimx dkim-keygen` for development.",
            dkim_root.display()
        )
        .into()
    } else {
        err.into()
    }
}

/// Resolve `<dkim_root>/{private,public}.key`.
///
/// `dkim_root` is treated as the directory containing the DKIM keys
/// themselves — callers pass `config_dir().join("dkim")` in production,
/// and tests may supply arbitrary tempdir roots.
fn dkim_paths(dkim_root: &Path) -> (std::path::PathBuf, std::path::PathBuf) {
    (dkim_root.join("private.key"), dkim_root.join("public.key"))
}

/// Generate a 2048-bit RSA DKIM keypair at `<dkim_root>/{private,public}.key`.
///
/// v0.2 permissions:
/// - Private key: `0o600` (root-only; `aimx serve` reads it in-process)
/// - Public key:  `0o644` (world-readable — it's advertised via DNS)
///
/// On Unix the files are created atomically with their target mode via
/// `OpenOptions::mode(...).create_new(true)` to avoid a brief window of
/// umask-default permissions between `write` and `chmod`.
pub fn generate_keypair(dkim_root: &Path, force: bool) -> Result<(), Box<dyn std::error::Error>> {
    let (private_path, public_path) = dkim_paths(dkim_root);

    if private_path.exists() && !force {
        return Err("DKIM keys already exist. Use --force to overwrite.".into());
    }

    std::fs::create_dir_all(dkim_root).map_err(|e| wrap_dkim_write_error(dkim_root, e))?;

    // Force-overwrite path: remove pre-existing files so `create_new(true)`
    // succeeds with the tightened mode. (Otherwise `OpenOptions` would
    // fail with `AlreadyExists` and leave the old file at its old mode.)
    if force {
        for p in [&private_path, &public_path] {
            if p.exists() {
                std::fs::remove_file(p).map_err(|e| wrap_dkim_write_error(dkim_root, e))?;
            }
        }
    }

    let mut rng = rsa::rand_core::OsRng;
    let private_key = RsaPrivateKey::new(&mut rng, DKIM_KEY_BITS)?;

    let private_pem = private_key.to_pkcs1_pem(LineEnding::LF)?;
    write_file_with_mode(&private_path, private_pem.as_bytes(), 0o600)
        .map_err(|e| wrap_dkim_write_error_from_boxed(dkim_root, e))?;

    let public_key = RsaPublicKey::from(&private_key);
    let public_pem = public_key.to_public_key_pem(LineEnding::LF)?;
    write_file_with_mode(&public_path, public_pem.as_bytes(), 0o644)
        .map_err(|e| wrap_dkim_write_error_from_boxed(dkim_root, e))?;

    Ok(())
}

/// Like [`wrap_dkim_write_error`] but accepts a boxed error coming out of
/// [`write_file_with_mode`]. If the underlying cause is an
/// `io::ErrorKind::PermissionDenied`, rewrap with the friendly message;
/// otherwise pass through unchanged.
fn wrap_dkim_write_error_from_boxed(
    dkim_root: &Path,
    err: Box<dyn std::error::Error>,
) -> Box<dyn std::error::Error> {
    if let Some(io_err) = err.downcast_ref::<std::io::Error>()
        && io_err.kind() == std::io::ErrorKind::PermissionDenied
    {
        return format!(
            "Permission denied writing to {}. \
             Run `sudo aimx dkim-keygen`, or set `AIMX_CONFIG_DIR=<writable-path>` \
             and rerun `aimx dkim-keygen` for development.",
            dkim_root.display()
        )
        .into();
    }
    err
}

/// Create `path` with `bytes` as contents and the given Unix mode applied
/// atomically at open time (no umask-default window between write and
/// chmod). On non-Unix the mode is ignored and a plain `fs::write` is used.
fn write_file_with_mode(
    path: &Path,
    bytes: &[u8],
    mode: u32,
) -> Result<(), Box<dyn std::error::Error>> {
    #[cfg(unix)]
    {
        use std::io::Write as _;
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(mode)
            .open(path)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    #[cfg(not(unix))]
    {
        let _ = mode;
        std::fs::write(path, bytes)?;
    }
    Ok(())
}

pub fn dns_record_value(dkim_root: &Path) -> Result<String, Box<dyn std::error::Error>> {
    let b64 = public_key_spki_base64(dkim_root)?;
    Ok(format!("v=DKIM1; k=rsa; p={b64}"))
}

/// Read the DKIM public key PEM at `<dkim_root>/public.key` and return its
/// SPKI-DER base64 — i.e. the `p=` value in the DKIM1 TXT record.
///
/// Used by both the setup-time DNS-record advertiser and the daemon's
/// startup DNS sanity check (which compares the DNS-published `p=`
/// against the on-disk key) so they share one PEM-parse path.
pub fn public_key_spki_base64(dkim_root: &Path) -> Result<String, Box<dyn std::error::Error>> {
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
    Ok(base64::engine::general_purpose::STANDARD.encode(spki_der.as_ref()))
}

/// Extract the `p=` value from a DKIM1 TXT record. Returns `None` if the
/// record does not contain a non-empty `p=` tag. Whitespace inside the
/// value is stripped (DNS-multi-string records can have spaces inserted by
/// resolvers).
///
/// Shared between `aimx setup`'s verify_dkim check and `aimx serve`'s
/// startup DNS sanity check (S44-2). Keeping a single parser prevents the
/// two checks from drifting and producing contradictory verdicts.
pub fn extract_dkim_p_value(record: &str) -> Option<String> {
    for part in record.split(';') {
        let part = part.trim();
        if let Some(value) = part.strip_prefix("p=") {
            let key = value.replace(char::is_whitespace, "");
            if !key.is_empty() {
                return Some(key);
            }
        }
    }
    None
}

/// Load the RSA DKIM private key from `<dkim_root>/private.key`.
///
/// The private key is `0o600` (root-only). When a non-root process hits
/// `PermissionDenied`, the error message steers the caller back to
/// `aimx send` (which delegates to the `aimx serve` UDS socket for DKIM
/// signing) rather than suggesting a permissions hack.
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

    #[cfg(unix)]
    #[test]
    fn generate_keypair_permission_denied_suggests_sudo_or_aimx_config_dir() {
        use std::os::unix::fs::PermissionsExt;

        // Skip when running as root — `chmod 0o500` is bypassed by CAP_DAC_OVERRIDE
        // / euid 0, so we can't force PermissionDenied.
        if unsafe { libc::geteuid() } == 0 {
            eprintln!("skipping: test must run as non-root");
            return;
        }

        let tmp = TempDir::new().unwrap();
        let parent = tmp.path().join("ro-config");
        std::fs::create_dir_all(&parent).unwrap();

        // Strip write permission on the parent. `create_dir_all(dkim_root)`
        // inside `generate_keypair` will then fail with PermissionDenied.
        let dkim_root = parent.join("dkim");
        std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o500)).unwrap();

        let result = generate_keypair(&dkim_root, false);

        // Always restore so TempDir cleanup can succeed.
        let _ = std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o755));

        let err = result.expect_err("generate_keypair must fail on a read-only parent");
        let msg = err.to_string();
        assert!(
            msg.contains("Permission denied"),
            "error must mention permission denial: {msg}"
        );
        assert!(
            msg.contains(&dkim_root.display().to_string()),
            "error must name the target directory: {msg}"
        );
        assert!(
            msg.contains("sudo") || msg.contains("AIMX_CONFIG_DIR"),
            "error must suggest sudo or AIMX_CONFIG_DIR: {msg}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn generate_keypair_non_permission_io_error_passes_through() {
        // A non-permission IO error (here: `dkim_root` is an existing *file*,
        // not a directory — `create_dir_all` returns NotADirectory /
        // AlreadyExists, not PermissionDenied) must NOT be rewrapped with
        // the sudo guidance.
        let tmp = TempDir::new().unwrap();
        let bogus = tmp.path().join("not-a-dir");
        std::fs::write(&bogus, b"").unwrap();

        let result = generate_keypair(&bogus, false);
        let err = result.expect_err("must fail");
        let msg = err.to_string();
        assert!(
            !msg.contains("sudo") && !msg.contains("AIMX_CONFIG_DIR"),
            "non-PermissionDenied errors must surface their native message: {msg}"
        );
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
    fn public_key_spki_base64_matches_dns_record() {
        let tmp = TempDir::new().unwrap();
        generate_keypair(tmp.path(), false).unwrap();
        let b64 = public_key_spki_base64(tmp.path()).unwrap();
        let record = dns_record_value(tmp.path()).unwrap();
        // The SPKI base64 is embedded verbatim in the DNS record's `p=`
        // value — same bytes, no whitespace.
        assert!(record.ends_with(&b64), "record={record}, b64={b64}");
    }

    #[test]
    fn extract_dkim_p_value_strips_whitespace() {
        let record = "v=DKIM1; k=rsa; p=ABC DEF\tGHI";
        assert_eq!(extract_dkim_p_value(record).as_deref(), Some("ABCDEFGHI"));
    }

    #[test]
    fn extract_dkim_p_value_none_when_missing() {
        let record = "v=DKIM1; k=rsa";
        assert_eq!(extract_dkim_p_value(record), None);
    }

    #[test]
    fn extract_dkim_p_value_empty_returns_none() {
        let record = "v=DKIM1; k=rsa; p=";
        assert_eq!(extract_dkim_p_value(record), None);
    }

    #[test]
    fn extract_dkim_p_value_ignores_other_tags() {
        let record = "v=DKIM1; k=rsa; p=HELLO; s=email";
        assert_eq!(extract_dkim_p_value(record).as_deref(), Some("HELLO"));
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

    #[cfg(unix)]
    #[test]
    fn load_private_key_permission_denied_surfaces_root_guidance() {
        use std::os::unix::fs::PermissionsExt;

        // Skip this test when running as root — `chmod 0o000` is bypassed
        // by CAP_DAC_READ_SEARCH / euid 0, so we can't force PermissionDenied.
        if unsafe { libc::geteuid() } == 0 {
            eprintln!("skipping: test must run as non-root");
            return;
        }

        let tmp = TempDir::new().unwrap();
        let private_path = tmp.path().join("private.key");
        std::fs::write(&private_path, b"fake pem").unwrap();
        std::fs::set_permissions(&private_path, std::fs::Permissions::from_mode(0o000)).unwrap();

        let result = load_private_key(tmp.path());
        // Always restore mode so TempDir cleanup can succeed.
        let _ = std::fs::set_permissions(&private_path, std::fs::Permissions::from_mode(0o600));

        let err = result.expect_err("load_private_key should fail on 0o000 file");
        let msg = err.to_string();
        assert!(
            msg.contains("readable only by root"),
            "PermissionDenied path must surface the root-only guidance: {msg}"
        );
        assert!(
            msg.contains("aimx send"),
            "PermissionDenied guidance must redirect non-root callers to `aimx send`: {msg}"
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

        let signed = sign_message(message, &private_key, "example.com", "aimx").unwrap();
        let signed_str = String::from_utf8_lossy(&signed);

        assert!(signed_str.contains("DKIM-Signature:"));
        assert!(signed_str.contains("a=rsa-sha256"));
        assert!(signed_str.contains("d=example.com"));
        assert!(signed_str.contains("s=aimx"));

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
            "v0.2: DKIM private key must be root-only (0o600) because \
             signing happens inside the `aimx serve` daemon."
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

        let signed = sign_message(message, &private_key, "example.com", "aimx").unwrap();
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

        let signed = sign_message(message, &private_key, "example.com", "aimx").unwrap();
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

        let signed = sign_message(message, &private_key, "example.com", "aimx").unwrap();
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
