//! Build-time version metadata (FR-6.1).
//!
//! `build.rs` emits four `cargo:rustc-env` values — `RELEASE_TAG`, `GIT_HASH`,
//! `TARGET`, `BUILD_DATE` — that this module re-exports through typed helpers.
//! `aimx upgrade` consumes [`release_tag`] / [`target_triple`] directly rather
//! than string-parsing `aimx --version` output, so the renderer in
//! [`version_string`] and the upgrade flow cannot drift.

/// Release tag from `git describe --tags --always --dirty`, or `dev` when the
/// build tree is not a git checkout / has no tags.
pub fn release_tag() -> &'static str {
    let raw = env!("RELEASE_TAG");
    if raw.is_empty() { "dev" } else { raw }
}

/// Short git hash (8 hex chars), or `unknown` outside a git checkout.
pub fn git_hash() -> &'static str {
    let raw = env!("GIT_HASH");
    if raw.is_empty() { "unknown" } else { raw }
}

/// Target triple the binary was built for (e.g.
/// `x86_64-unknown-linux-gnu`). `install.sh` and `aimx upgrade` use this to
/// pick the matching tarball from the release's asset list.
pub fn target_triple() -> &'static str {
    let raw = env!("TARGET");
    if raw.is_empty() { "unknown" } else { raw }
}

/// UTC build date, `YYYY-MM-DD`.
pub fn build_date() -> &'static str {
    env!("BUILD_DATE")
}

/// `aimx <tag> (<git-sha>) <target-triple> built <date>` (FR-6.1).
///
/// When `git describe` had no tags to resolve, the tag field renders as `dev`
/// and the git hash renders as `unknown`, producing the documented fallback
/// `aimx dev (unknown) <target> built <date>`.
pub fn version_string() -> &'static str {
    use std::sync::LazyLock;
    static VERSION: LazyLock<String> = LazyLock::new(|| {
        format!(
            "aimx {} ({}) {} built {}",
            release_tag(),
            git_hash(),
            target_triple(),
            build_date()
        )
    });
    &VERSION
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn release_tag_is_non_empty() {
        assert!(!release_tag().is_empty());
    }

    #[test]
    fn git_hash_is_non_empty() {
        assert!(!git_hash().is_empty());
    }

    #[test]
    fn target_triple_is_non_empty() {
        let t = target_triple();
        assert!(!t.is_empty());
        // Cargo-supplied target triples always carry at least two hyphens
        // (`arch-vendor-os` plus the optional env suffix). If the env var
        // was missing we fall back to `unknown` — accept either shape.
        assert!(t == "unknown" || t.matches('-').count() >= 2);
    }

    #[test]
    fn build_date_matches_iso() {
        let d = build_date();
        let re_ok = d.len() == 10
            && d.as_bytes()[4] == b'-'
            && d.as_bytes()[7] == b'-'
            && d[..4].chars().all(|c| c.is_ascii_digit())
            && d[5..7].chars().all(|c| c.is_ascii_digit())
            && d[8..10].chars().all(|c| c.is_ascii_digit());
        assert!(re_ok, "build_date {d:?} does not match YYYY-MM-DD");
    }

    #[test]
    fn version_string_contains_all_fields() {
        let v = version_string();
        assert!(v.starts_with("aimx "));
        assert!(v.contains(release_tag()));
        assert!(v.contains(git_hash()));
        assert!(v.contains(target_triple()));
        assert!(v.contains(build_date()));
        assert!(v.contains(" built "));
    }
}
