//! Release-metadata fetcher (FR-4.6, PRD §8.1).
//!
//! `aimx upgrade` (Sprint 4) consumes this module through the [`ReleaseOps`]
//! trait so unit tests can inject a scripted [`MockReleaseOps`] instead of
//! hitting `api.github.com`. The real implementation, [`RealReleaseOps`],
//! uses `ureq` — blocking is fine because `aimx upgrade` is a short-lived
//! CLI process with no async runtime of its own.
//!
//! **Trust model (v1).** The only trust anchor for downloads is HTTPS on
//! the GitHub Releases domain. `fetch_asset` refuses `http://` URLs so a
//! misconfigured manifest can't silently degrade transport. Per the PRD,
//! no signing (minisign / cosign / GPG) lands in v1; [`verify_sha256`] is
//! exported purely for operator-facing integrity checks against the
//! published `SHA256SUMS` file — it is **not** called on the default
//! upgrade path.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use serde::Deserialize;
use sha2::{Digest, Sha256};

// Every item in this module is consumed from within the module itself
// (unit tests) and from `aimx upgrade` in Sprint 4. Per-item
// `#[allow(dead_code)]` narrows the lint so unrelated future dead code
// still surfaces — in contrast to a blanket module-level allow.

/// Environment variable that overrides the release-manifest URL
/// (PRD FR-4.6). Takes precedence over `[upgrade] release_manifest_url`
/// in `config.toml`.
#[allow(dead_code)]
pub const RELEASE_MANIFEST_URL_ENV: &str = "AIMX_RELEASE_MANIFEST_URL";

/// Default release-manifest URL — the GitHub Releases API `latest` endpoint
/// for the canonical `uzyn/aimx` repo.
#[allow(dead_code)]
pub const DEFAULT_RELEASE_MANIFEST_URL: &str =
    "https://api.github.com/repos/uzyn/aimx/releases/latest";

/// Base URL used to resolve a specific tag via `.../releases/tags/<tag>`
/// when `[upgrade] release_manifest_url` is unset.
#[allow(dead_code)]
pub const DEFAULT_RELEASE_TAGS_BASE_URL: &str = "https://api.github.com/repos/uzyn/aimx/releases";

/// Description of a GitHub Release, distilled to the fields `aimx upgrade`
/// needs. Only `tag`, `published_at`, and the `asset_urls` map are used by
/// the upgrade flow today; the struct is public because unit tests in other
/// modules construct it directly against [`MockReleaseOps`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)]
pub struct ReleaseManifest {
    pub tag: String,
    pub published_at: String,
    /// Filename → download URL. Filenames match the tarball naming scheme
    /// produced by `release.yml` (`aimx-<version>-<target>.tar.gz` and
    /// `...tar.gz.sha256`, plus `SHA256SUMS`).
    pub asset_urls: HashMap<String, String>,
}

impl ReleaseManifest {
    /// Look up the download URL for an asset by filename, surfacing a
    /// distinct [`ReleaseError::AssetNotFound`] so callers can distinguish
    /// "wrong name" from transport errors.
    #[allow(dead_code)]
    pub fn asset_url(&self, name: &str) -> Result<&str, ReleaseError> {
        self.asset_urls
            .get(name)
            .map(String::as_str)
            .ok_or_else(|| ReleaseError::AssetNotFound(name.to_string()))
    }
}

/// Errors surfaced by [`ReleaseOps`] implementations and [`verify_sha256`].
///
/// `aimx upgrade` maps each variant to a distinct exit-code / operator
/// message, so the variants are deliberately granular rather than collapsed
/// into a single "network failed" bucket.
#[derive(Debug)]
#[allow(dead_code)]
pub enum ReleaseError {
    /// The URL was neither `https://` nor `file://` (the only two schemes
    /// [`RealReleaseOps::fetch_asset`] accepts).
    NonHttpsUrl(String),
    /// The HTTP request failed before producing a response — DNS, TLS,
    /// connection-reset, timeout.
    Transport(String),
    /// Got a response, but not `2xx`.
    Http { status: u16, body: String },
    /// Response body was not well-formed JSON, or lacked fields the
    /// manifest parser requires.
    Parse(String),
    /// Asset filename was not present in the release.
    AssetNotFound(String),
    /// `file://` manifest could not be read.
    Io(String),
    /// SHA-256 digest mismatch surfaced by [`verify_sha256`].
    ChecksumMismatch { expected: String, actual: String },
}

impl std::fmt::Display for ReleaseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ReleaseError::NonHttpsUrl(url) => {
                write!(f, "refusing non-HTTPS release URL: {url}")
            }
            ReleaseError::Transport(msg) => write!(f, "transport error: {msg}"),
            ReleaseError::Http { status, body } => {
                write!(f, "HTTP {status}: {}", body.trim())
            }
            ReleaseError::Parse(msg) => write!(f, "failed to parse release manifest: {msg}"),
            ReleaseError::AssetNotFound(name) => write!(f, "release asset not found: {name}"),
            ReleaseError::Io(msg) => write!(f, "io error reading release manifest: {msg}"),
            ReleaseError::ChecksumMismatch { expected, actual } => {
                write!(f, "SHA-256 mismatch: expected {expected}, got {actual}")
            }
        }
    }
}

impl std::error::Error for ReleaseError {}

/// Testing seam between `aimx upgrade` and the network.
///
/// Production code uses [`RealReleaseOps`]; unit tests pass a
/// [`MockReleaseOps`] preloaded with scripted responses. `fetch_asset`
/// returns bytes rather than a stream because release tarballs are
/// small (< 20 MB per PRD §7) and the upgrade flow buffers them into
/// a temp file before extraction regardless.
#[allow(dead_code)]
pub trait ReleaseOps {
    fn latest_release(&self) -> Result<ReleaseManifest, ReleaseError>;
    fn release_by_tag(&self, tag: &str) -> Result<ReleaseManifest, ReleaseError>;
    fn fetch_asset(&self, url: &str) -> Result<Vec<u8>, ReleaseError>;
}

/// Default `ReleaseOps` backed by `ureq` and the GitHub Releases API.
///
/// The manifest URL is resolved lazily inside each method so tests that
/// swap `AIMX_RELEASE_MANIFEST_URL` mid-process (via `TestManifestUrl`)
/// pick up the new value without rebuilding the struct.
#[allow(dead_code)]
pub struct RealReleaseOps {
    /// Override for the manifest URL, typically sourced from
    /// `[upgrade] release_manifest_url` in `config.toml`. The
    /// `AIMX_RELEASE_MANIFEST_URL` env var still wins.
    config_manifest_url: Option<String>,
}

impl Default for RealReleaseOps {
    fn default() -> Self {
        Self::new(None)
    }
}

#[allow(dead_code)]
impl RealReleaseOps {
    /// `config_manifest_url` mirrors `Config::upgrade.release_manifest_url`.
    /// Callers typically pass `config.upgrade.as_ref().and_then(|u|
    /// u.release_manifest_url.clone())` here.
    pub fn new(config_manifest_url: Option<String>) -> Self {
        Self {
            config_manifest_url,
        }
    }

    /// Resolve the latest-release URL honoring the env-var > config > default
    /// precedence chain (FR-4.6).
    pub fn latest_release_url(&self) -> String {
        if let Ok(v) = std::env::var(RELEASE_MANIFEST_URL_ENV)
            && !v.is_empty()
        {
            return v;
        }
        if let Some(v) = self.config_manifest_url.as_ref()
            && !v.is_empty()
        {
            return v.clone();
        }
        DEFAULT_RELEASE_MANIFEST_URL.to_string()
    }

    /// Resolve the tag-specific URL. If the manifest override looks like it
    /// points at `/releases/latest`, rewrite it to `/releases/tags/<tag>`
    /// by swapping the trailing path segment. Otherwise append
    /// `/tags/<tag>` to the configured base.
    pub fn release_by_tag_url(&self, tag: &str) -> String {
        if let Ok(v) = std::env::var(RELEASE_MANIFEST_URL_ENV)
            && !v.is_empty()
        {
            return rewrite_to_tag(&v, tag);
        }
        if let Some(v) = self.config_manifest_url.as_ref()
            && !v.is_empty()
        {
            return rewrite_to_tag(v, tag);
        }
        format!("{DEFAULT_RELEASE_TAGS_BASE_URL}/tags/{tag}")
    }
}

/// Rewrite a URL that ends in `/latest` to `/tags/<tag>`. For any other
/// shape, append `/tags/<tag>` (makes `file:///.../releases.json` style
/// fixtures work when the test author points at a directory).
///
/// **Fixture naming convention (load-bearing).** For `file://` URLs that
/// end in `.json`, this function maps `.../latest.json` to
/// `.../tag-<tag>.json`. Test fixtures that want per-tag resolution via
/// `RealReleaseOps::release_by_tag_url` MUST be named accordingly — e.g.
/// a fixture set at `tests/fixtures/releases/latest.json` implies a peer
/// file `tests/fixtures/releases/tag-v1-2-3.json` resolves `v1.2.3`.
/// Any other `.json` filename is passed through unchanged.
#[allow(dead_code)]
fn rewrite_to_tag(url: &str, tag: &str) -> String {
    if let Some(stripped) = url.strip_suffix("/latest") {
        format!("{stripped}/tags/{tag}")
    } else if let Some(stripped) = url.strip_suffix("/releases/latest/") {
        format!("{stripped}/releases/tags/{tag}")
    } else if url.ends_with(".json") {
        // Convention for `file://` fixtures: `.../foo/latest.json` →
        // `.../foo/tag-<tag>.json`. Lets a test suite ship a directory of
        // per-tag fixtures without wiring a real HTTP server.
        if let Some(stripped) = url.strip_suffix("latest.json") {
            format!("{stripped}tag-{tag}.json")
        } else {
            url.to_string()
        }
    } else {
        format!("{}/tags/{tag}", url.trim_end_matches('/'))
    }
}

impl ReleaseOps for RealReleaseOps {
    fn latest_release(&self) -> Result<ReleaseManifest, ReleaseError> {
        let url = self.latest_release_url();
        fetch_manifest(&url)
    }

    fn release_by_tag(&self, tag: &str) -> Result<ReleaseManifest, ReleaseError> {
        let url = self.release_by_tag_url(tag);
        fetch_manifest(&url)
    }

    fn fetch_asset(&self, url: &str) -> Result<Vec<u8>, ReleaseError> {
        fetch_bytes(url)
    }
}

/// Parsed shape of a GitHub Release JSON document. Only the fields we
/// consume are deserialized — unknown fields are ignored so API additions
/// don't break parsing.
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct GithubRelease {
    tag_name: String,
    #[serde(default)]
    published_at: String,
    #[serde(default)]
    assets: Vec<GithubAsset>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct GithubAsset {
    name: String,
    browser_download_url: String,
}

impl From<GithubRelease> for ReleaseManifest {
    fn from(r: GithubRelease) -> Self {
        let asset_urls = r
            .assets
            .into_iter()
            .map(|a| (a.name, a.browser_download_url))
            .collect();
        ReleaseManifest {
            tag: r.tag_name,
            published_at: r.published_at,
            asset_urls,
        }
    }
}

/// Parse a JSON release document into a [`ReleaseManifest`]. Exposed so
/// tests can build manifests from fixture files without going through
/// [`RealReleaseOps`].
#[allow(dead_code)]
pub fn parse_release_json(bytes: &[u8]) -> Result<ReleaseManifest, ReleaseError> {
    let release: GithubRelease =
        serde_json::from_slice(bytes).map_err(|e| ReleaseError::Parse(e.to_string()))?;
    Ok(release.into())
}

#[allow(dead_code)]
fn fetch_manifest(url: &str) -> Result<ReleaseManifest, ReleaseError> {
    let bytes = fetch_bytes(url)?;
    parse_release_json(&bytes)
}

#[allow(dead_code)]
fn fetch_bytes(url: &str) -> Result<Vec<u8>, ReleaseError> {
    if let Some(path) = url.strip_prefix("file://") {
        let p = PathBuf::from(path);
        return std::fs::read(&p).map_err(|e| ReleaseError::Io(format!("{}: {e}", p.display())));
    }
    if !url.starts_with("https://") {
        return Err(ReleaseError::NonHttpsUrl(url.to_string()));
    }

    // Install the process-wide rustls default provider on first use.
    // `ureq` with `rustls-no-provider` expects the caller to pick one;
    // the rest of the aimx crate already uses `aws-lc-rs`, so match it.
    install_rustls_provider();

    let agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(30)))
        .user_agent(concat!("aimx/", env!("CARGO_PKG_VERSION")))
        .build()
        .new_agent();

    let mut resp = agent
        .get(url)
        .header("Accept", "application/vnd.github+json")
        .call()
        .map_err(|e| ReleaseError::Transport(e.to_string()))?;

    let status = resp.status().as_u16();
    let body = resp
        .body_mut()
        .with_config()
        .limit(64 * 1024 * 1024)
        .read_to_vec()
        .map_err(|e| ReleaseError::Transport(e.to_string()))?;

    if !(200..300).contains(&status) {
        let trimmed = String::from_utf8_lossy(&body[..body.len().min(512)]).to_string();
        return Err(ReleaseError::Http {
            status,
            body: trimmed,
        });
    }

    Ok(body)
}

#[allow(dead_code)]
fn install_rustls_provider() {
    use std::sync::Once;
    static INSTALL: Once = Once::new();
    INSTALL.call_once(|| {
        // Ignore the result: another crate in the same process may have
        // already installed a provider. The first one wins.
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    });
}

/// Compute the SHA-256 of `bytes` and compare against `expected_hex`
/// (case-insensitive). Pure; does no I/O. Not called on the default
/// `aimx upgrade` path — kept for future `aimx verify` surfaces and
/// integration-test assertions against published `.sha256` files.
#[allow(dead_code)]
pub fn verify_sha256(bytes: &[u8], expected_hex: &str) -> Result<(), ReleaseError> {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let digest = hasher.finalize();
    let actual = hex_encode(&digest);
    let expected = expected_hex.trim().to_ascii_lowercase();
    if actual == expected {
        Ok(())
    } else {
        Err(ReleaseError::ChecksumMismatch { expected, actual })
    }
}

/// Lowercase hex encoding of `bytes`. Tiny hand-rolled impl so we don't pull
/// in the `hex` crate just for one call site.
#[allow(dead_code)]
pub fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    out
}

/// Parse the first line of a standard `sha256sum` output (`<hex>  <name>`)
/// into the hex digest. Tolerates either two spaces (binary mode) or a
/// single space (text mode); trims trailing whitespace.
#[allow(dead_code)]
pub fn parse_sha256_file(contents: &str) -> Result<String, ReleaseError> {
    let line = contents.lines().next().unwrap_or("").trim();
    let hex = line
        .split_whitespace()
        .next()
        .ok_or_else(|| ReleaseError::Parse("empty .sha256 file".to_string()))?;
    if hex.len() != 64 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(ReleaseError::Parse(format!(
            "not a 64-char hex digest: {hex:?}"
        )));
    }
    Ok(hex.to_ascii_lowercase())
}

// -------------------------------------------------------------------------
// MockReleaseOps — test-only, shared between the unit tests in this file
// and any other module's tests that want to inject scripted responses.
// -------------------------------------------------------------------------

#[cfg(test)]
pub(crate) mod mock {
    use super::*;
    use std::collections::VecDeque;
    use std::sync::Mutex;

    /// Scripted [`ReleaseOps`] — each method pops a response off its own
    /// queue. Tests populate the queues up front and assert on the leftover
    /// state at the end.
    pub struct MockReleaseOps {
        pub latest: Mutex<VecDeque<Result<ReleaseManifest, ReleaseError>>>,
        pub by_tag: Mutex<VecDeque<Result<ReleaseManifest, ReleaseError>>>,
        pub assets: Mutex<VecDeque<Result<Vec<u8>, ReleaseError>>>,
    }

    impl Default for MockReleaseOps {
        fn default() -> Self {
            Self {
                latest: Mutex::new(VecDeque::new()),
                by_tag: Mutex::new(VecDeque::new()),
                assets: Mutex::new(VecDeque::new()),
            }
        }
    }

    impl MockReleaseOps {
        pub fn push_latest(&self, r: Result<ReleaseManifest, ReleaseError>) {
            self.latest.lock().unwrap().push_back(r);
        }
        pub fn push_by_tag(&self, r: Result<ReleaseManifest, ReleaseError>) {
            self.by_tag.lock().unwrap().push_back(r);
        }
        pub fn push_asset(&self, r: Result<Vec<u8>, ReleaseError>) {
            self.assets.lock().unwrap().push_back(r);
        }
    }

    impl ReleaseOps for MockReleaseOps {
        fn latest_release(&self) -> Result<ReleaseManifest, ReleaseError> {
            self.latest
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_else(|| Err(ReleaseError::Transport("mock exhausted".into())))
        }

        fn release_by_tag(&self, _tag: &str) -> Result<ReleaseManifest, ReleaseError> {
            self.by_tag
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_else(|| Err(ReleaseError::Transport("mock exhausted".into())))
        }

        fn fetch_asset(&self, _url: &str) -> Result<Vec<u8>, ReleaseError> {
            self.assets
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_else(|| Err(ReleaseError::Transport("mock exhausted".into())))
        }
    }
}

// -------------------------------------------------------------------------
// Test-only global override (mirrors `config::test_env::ConfigDirOverride`).
// -------------------------------------------------------------------------

#[cfg(test)]
pub(crate) mod test_env {
    use super::RELEASE_MANIFEST_URL_ENV;
    use std::path::Path;
    use std::sync::Mutex;

    static GUARD: Mutex<()> = Mutex::new(());

    /// Serialized process-wide setter for `AIMX_RELEASE_MANIFEST_URL`.
    /// Restores the previous value on drop.
    pub(crate) struct ReleaseManifestUrlOverride {
        _guard: std::sync::MutexGuard<'static, ()>,
        prev: Option<std::ffi::OsString>,
    }

    impl ReleaseManifestUrlOverride {
        pub(crate) fn set(url: &str) -> Self {
            let guard = GUARD.lock().unwrap_or_else(|e| e.into_inner());
            let prev = std::env::var_os(RELEASE_MANIFEST_URL_ENV);
            // SAFETY: env mutation serialized via GUARD.
            unsafe {
                std::env::set_var(RELEASE_MANIFEST_URL_ENV, url);
            }
            Self {
                _guard: guard,
                prev,
            }
        }

        /// Point the override at a `file://` URL derived from a local path.
        pub(crate) fn set_file(path: &Path) -> Self {
            Self::set(&format!("file://{}", path.display()))
        }

        /// Hold the guard with the env var explicitly removed. Use when a
        /// test needs to assert behaviour that is ONLY correct when the
        /// env var is absent (e.g. fall-through to config / default).
        pub(crate) fn unset() -> Self {
            let guard = GUARD.lock().unwrap_or_else(|e| e.into_inner());
            let prev = std::env::var_os(RELEASE_MANIFEST_URL_ENV);
            // SAFETY: env mutation serialized via GUARD.
            unsafe {
                std::env::remove_var(RELEASE_MANIFEST_URL_ENV);
            }
            Self {
                _guard: guard,
                prev,
            }
        }
    }

    impl Drop for ReleaseManifestUrlOverride {
        fn drop(&mut self) {
            // SAFETY: env mutation serialized via GUARD.
            unsafe {
                match &self.prev {
                    Some(v) => std::env::set_var(RELEASE_MANIFEST_URL_ENV, v),
                    None => std::env::remove_var(RELEASE_MANIFEST_URL_ENV),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::mock::MockReleaseOps;
    use super::test_env::ReleaseManifestUrlOverride;
    use super::*;

    const LATEST_JSON: &str = r#"{
        "tag_name": "v0.0.0-fixture",
        "published_at": "2026-04-20T00:00:00Z",
        "assets": [
            {
                "name": "aimx-0.0.0-fixture-x86_64-unknown-linux-gnu.tar.gz",
                "browser_download_url": "https://example.invalid/a.tar.gz"
            },
            {
                "name": "aimx-0.0.0-fixture-x86_64-unknown-linux-gnu.tar.gz.sha256",
                "browser_download_url": "https://example.invalid/a.tar.gz.sha256"
            },
            {
                "name": "SHA256SUMS",
                "browser_download_url": "https://example.invalid/SHA256SUMS"
            }
        ]
    }"#;

    #[test]
    fn asset_url_returns_not_found_for_missing_filename() {
        let manifest = parse_release_json(LATEST_JSON.as_bytes()).unwrap();
        let err = manifest
            .asset_url("aimx-0.0.0-nope-mips-unknown-linux-gnu.tar.gz")
            .unwrap_err();
        match err {
            ReleaseError::AssetNotFound(name) => {
                assert!(name.contains("mips"), "unexpected asset name: {name}");
            }
            other => panic!("expected AssetNotFound, got {other:?}"),
        }
    }

    #[test]
    fn asset_url_happy() {
        let manifest = parse_release_json(LATEST_JSON.as_bytes()).unwrap();
        let url = manifest
            .asset_url("aimx-0.0.0-fixture-x86_64-unknown-linux-gnu.tar.gz")
            .unwrap();
        assert_eq!(url, "https://example.invalid/a.tar.gz");
    }

    #[test]
    fn parse_release_json_happy() {
        let manifest = parse_release_json(LATEST_JSON.as_bytes()).unwrap();
        assert_eq!(manifest.tag, "v0.0.0-fixture");
        assert_eq!(manifest.published_at, "2026-04-20T00:00:00Z");
        assert_eq!(manifest.asset_urls.len(), 3);
        assert!(manifest.asset_urls.contains_key("SHA256SUMS"));
    }

    #[test]
    fn parse_release_json_rejects_malformed() {
        let err = parse_release_json(b"{ not json").unwrap_err();
        matches!(err, ReleaseError::Parse(_));
    }

    #[test]
    fn latest_release_reads_fixture_over_file_url() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("latest.json");
        std::fs::write(&path, LATEST_JSON).unwrap();

        let _guard = ReleaseManifestUrlOverride::set_file(&path);
        let ops = RealReleaseOps::default();
        let manifest = ops.latest_release().unwrap();
        assert_eq!(manifest.tag, "v0.0.0-fixture");
    }

    #[test]
    fn release_by_tag_rewrites_file_url() {
        let tmp = tempfile::tempdir().unwrap();
        let latest = tmp.path().join("latest.json");
        std::fs::write(&latest, LATEST_JSON).unwrap();

        // rewrite_to_tag maps `.../latest.json` → `.../tag-<tag>.json`
        let tag_path = tmp.path().join("tag-v0.0.0-fixture.json");
        std::fs::write(&tag_path, LATEST_JSON).unwrap();

        let _guard = ReleaseManifestUrlOverride::set_file(&latest);
        let ops = RealReleaseOps::default();
        let manifest = ops.release_by_tag("v0.0.0-fixture").unwrap();
        assert_eq!(manifest.tag, "v0.0.0-fixture");
    }

    #[test]
    fn fetch_asset_refuses_non_https() {
        let ops = RealReleaseOps::default();
        let err = ops.fetch_asset("http://example.invalid/foo").unwrap_err();
        match err {
            ReleaseError::NonHttpsUrl(u) => assert_eq!(u, "http://example.invalid/foo"),
            other => panic!("expected NonHttpsUrl, got {other:?}"),
        }
    }

    #[test]
    fn fetch_asset_file_scheme_reads_bytes() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("payload.bin");
        std::fs::write(&path, b"hello aimx").unwrap();
        let ops = RealReleaseOps::default();
        let bytes = ops
            .fetch_asset(&format!("file://{}", path.display()))
            .unwrap();
        assert_eq!(bytes, b"hello aimx");
    }

    #[test]
    fn verify_sha256_pass() {
        // echo -n "abc" | sha256sum
        let hex = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
        verify_sha256(b"abc", hex).unwrap();
        // Mixed case OK.
        verify_sha256(b"abc", &hex.to_ascii_uppercase()).unwrap();
    }

    #[test]
    fn verify_sha256_mismatch_names_both_sides() {
        let err = verify_sha256(b"abc", &"0".repeat(64)).unwrap_err();
        match err {
            ReleaseError::ChecksumMismatch { expected, actual } => {
                assert_eq!(expected, "0".repeat(64));
                assert_eq!(
                    actual,
                    "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
                );
            }
            other => panic!("expected ChecksumMismatch, got {other:?}"),
        }
    }

    #[test]
    fn parse_sha256_file_happy() {
        let hex = parse_sha256_file(
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad  aimx.tar.gz\n",
        )
        .unwrap();
        assert_eq!(
            hex,
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn parse_sha256_file_rejects_short() {
        let err = parse_sha256_file("deadbeef  aimx.tar.gz\n").unwrap_err();
        matches!(err, ReleaseError::Parse(_));
    }

    #[test]
    fn mock_release_ops_replays_scripted_responses() {
        let mock = MockReleaseOps::default();
        let manifest = ReleaseManifest {
            tag: "v1.2.3".to_string(),
            published_at: "2026-01-01T00:00:00Z".to_string(),
            asset_urls: [("a".to_string(), "https://x/a".to_string())].into(),
        };
        mock.push_latest(Ok(manifest.clone()));
        mock.push_by_tag(Ok(manifest.clone()));
        mock.push_asset(Ok(b"bytes".to_vec()));

        assert_eq!(mock.latest_release().unwrap(), manifest);
        assert_eq!(mock.release_by_tag("v1.2.3").unwrap(), manifest);
        assert_eq!(mock.fetch_asset("https://x/a").unwrap(), b"bytes");

        // Exhausted queues surface a clear error so tests don't silently
        // mask a missing scripted response.
        let err = mock.latest_release().unwrap_err();
        matches!(err, ReleaseError::Transport(_));
    }

    #[test]
    fn latest_release_url_env_overrides_config() {
        let _guard = ReleaseManifestUrlOverride::set("https://env.example/override");
        let ops = RealReleaseOps::new(Some("https://config.example/ignored".to_string()));
        assert_eq!(ops.latest_release_url(), "https://env.example/override");
    }

    #[test]
    fn latest_release_url_falls_through_to_config_then_default() {
        // Serialize env mutation via the shared GUARD and explicitly remove
        // the env var for the duration of the test. Previously this test
        // wrapped both assertions in `if env.is_none()`, which meant a
        // leaked `AIMX_RELEASE_MANIFEST_URL` from the ambient shell / CI
        // job would silently skip the assertion instead of failing.
        let _guard = ReleaseManifestUrlOverride::unset();

        let ops = RealReleaseOps::new(Some("https://config.example/only".to_string()));
        assert_eq!(ops.latest_release_url(), "https://config.example/only");

        let ops_default = RealReleaseOps::default();
        assert_eq!(
            ops_default.latest_release_url(),
            DEFAULT_RELEASE_MANIFEST_URL
        );
    }

    #[test]
    fn rewrite_to_tag_handles_known_shapes() {
        assert_eq!(
            rewrite_to_tag(
                "https://api.github.com/repos/uzyn/aimx/releases/latest",
                "v1.0.0"
            ),
            "https://api.github.com/repos/uzyn/aimx/releases/tags/v1.0.0"
        );
        assert_eq!(
            rewrite_to_tag("file:///tmp/latest.json", "v1.0.0"),
            "file:///tmp/tag-v1.0.0.json"
        );
        assert_eq!(
            rewrite_to_tag("https://api.example/releases", "v1.0.0"),
            "https://api.example/releases/tags/v1.0.0"
        );
    }
}
