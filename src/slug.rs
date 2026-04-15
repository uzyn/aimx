//! Slug algorithm + filename helper (Sprint 36 / FR-13b).
//!
//! `slugify` is a pure transform: MIME-decoded subject → deterministic
//! filesystem-safe stem. `allocate_filename` picks the final on-disk path,
//! resolving collisions and deciding between a flat `<stem>.md` layout and
//! a Zola-style bundle directory (`<stem>/<stem>.md` with attachments as
//! siblings). Both helpers are pure except for the directory check that
//! `allocate_filename` performs to detect collisions.

use chrono::{DateTime, Utc};
use std::path::{Path, PathBuf};

const SLUG_MAX_LEN: usize = 20;
const SLUG_EMPTY_FALLBACK: &str = "no-subject";

/// Convert a subject line into a deterministic slug.
///
/// Callers pass the already-MIME-decoded subject (e.g. the `&str` returned
/// by `mail_parser::Message::subject`, which decodes RFC 2047 encoded words
/// transparently). The transformation is:
/// 1. Lowercase (Unicode-aware).
/// 2. Every non-alphanumeric character becomes `-`.
/// 3. Runs of `-` collapse to one.
/// 4. Leading/trailing `-` are trimmed.
/// 5. Truncated to 20 characters (counted as Unicode scalar values, then
///    re-trimmed in case truncation left a trailing `-`).
/// 6. Empty result becomes `no-subject`.
pub fn slugify(subject: &str) -> String {
    let lowered: String = subject.to_lowercase();

    let mut out = String::with_capacity(lowered.len());
    let mut last_was_dash = false;
    for ch in lowered.chars() {
        if ch.is_alphanumeric() {
            out.push(ch);
            last_was_dash = false;
        } else if !last_was_dash {
            out.push('-');
            last_was_dash = true;
        }
    }

    let trimmed = out.trim_matches('-');

    let truncated: String = trimmed.chars().take(SLUG_MAX_LEN).collect();
    let truncated = truncated.trim_end_matches('-').to_string();

    if truncated.is_empty() {
        SLUG_EMPTY_FALLBACK.to_string()
    } else {
        truncated
    }
}

/// Build the UTC timestamp prefix used in inbound/outbound filenames.
pub fn format_timestamp(ts: DateTime<Utc>) -> String {
    ts.format("%Y-%m-%d-%H%M%S").to_string()
}

/// Allocate a non-colliding filename for a new email under `dir`.
///
/// The resolved path:
/// * zero attachments → `<dir>/<stem>.md`
/// * one+ attachments → `<dir>/<stem>/<stem>.md` (the attachments live
///   beside it inside the `<stem>/` bundle directory)
///
/// Collisions append `-2`, `-3`, … to the stem. When `has_attachments` is
/// true, collisions are detected against the bundle directory name so the
/// inner `.md` always shares the parent directory's stem.
///
/// The returned path points at the `.md` file the caller must create; the
/// caller is responsible for actually creating any missing parent
/// directories and writing the file.
pub fn allocate_filename(
    dir: &Path,
    timestamp: DateTime<Utc>,
    slug: &str,
    has_attachments: bool,
) -> PathBuf {
    let base_stem = format!("{}-{}", format_timestamp(timestamp), slug);

    for suffix in 1.. {
        let stem = if suffix == 1 {
            base_stem.clone()
        } else {
            format!("{base_stem}-{suffix}")
        };

        if has_attachments {
            let bundle = dir.join(&stem);
            if !bundle.exists() {
                return bundle.join(format!("{stem}.md"));
            }
        } else {
            let file = dir.join(format!("{stem}.md"));
            let bundle = dir.join(&stem);
            if !file.exists() && !bundle.exists() {
                return file;
            }
        }
    }

    unreachable!("collision loop exhausted u32::MAX suffixes")
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use tempfile::TempDir;

    #[test]
    fn slugify_ascii_subject() {
        assert_eq!(slugify("Hello World"), "hello-world");
    }

    #[test]
    fn slugify_mixed_case() {
        assert_eq!(slugify("Meeting Tomorrow"), "meeting-tomorrow");
    }

    #[test]
    fn slugify_unicode_subject() {
        // mail-parser decodes RFC 2047 into UTF-8 before slugify sees it.
        // Unicode letters pass the `is_alphanumeric` predicate, so they
        // survive (lowercased) while punctuation collapses to dashes.
        let slug = slugify("Héllo Wörld!");
        assert!(slug.starts_with("héllo"), "got: {slug}");
        assert!(slug.contains("wörld"), "got: {slug}");
    }

    #[test]
    fn slugify_all_non_alphanumeric_becomes_fallback() {
        assert_eq!(slugify("!!!???@@@"), "no-subject");
        assert_eq!(slugify("---"), "no-subject");
        assert_eq!(slugify(""), "no-subject");
        assert_eq!(slugify("   "), "no-subject");
    }

    #[test]
    fn slugify_long_subject_truncated_to_20() {
        let subject = "This is a very long subject that should be truncated";
        let slug = slugify(subject);
        assert!(
            slug.chars().count() <= 20,
            "got: {slug} ({} chars)",
            slug.chars().count()
        );
        assert_eq!(slug, "this-is-a-very-long");
    }

    #[test]
    fn slugify_collapses_dash_runs() {
        assert_eq!(slugify("foo   bar"), "foo-bar");
        assert_eq!(slugify("foo---bar"), "foo-bar");
        assert_eq!(slugify("foo!!!bar"), "foo-bar");
        assert_eq!(slugify("foo!@#$bar"), "foo-bar");
    }

    #[test]
    fn slugify_trims_leading_trailing_dashes() {
        assert_eq!(slugify("!!!hello"), "hello");
        assert_eq!(slugify("hello!!!"), "hello");
        assert_eq!(slugify("!!!hello!!!"), "hello");
    }

    #[test]
    fn slugify_truncation_retrims_trailing_dash() {
        // Truncation can expose a trailing `-`; re-trim so slugs never
        // end on a separator.
        let subject = "abc defghijklmnopqrstuvwxyz";
        let slug = slugify(subject);
        assert!(!slug.ends_with('-'), "got: {slug}");
    }

    #[test]
    fn slugify_numbers_preserved() {
        assert_eq!(slugify("Invoice #123 for 2025"), "invoice-123-for-2025");
    }

    #[test]
    fn format_timestamp_utc_exact_string() {
        let ts = Utc.with_ymd_and_hms(2025, 3, 14, 15, 9, 26).unwrap();
        assert_eq!(format_timestamp(ts), "2025-03-14-150926");
    }

    #[test]
    fn format_timestamp_midnight() {
        let ts = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
        assert_eq!(format_timestamp(ts), "2026-01-01-000000");
    }

    #[test]
    fn allocate_filename_flat_no_collision() {
        let tmp = TempDir::new().unwrap();
        let ts = Utc.with_ymd_and_hms(2025, 6, 1, 12, 0, 0).unwrap();
        let path = allocate_filename(tmp.path(), ts, "hello", false);
        assert_eq!(path, tmp.path().join("2025-06-01-120000-hello.md"),);
    }

    #[test]
    fn allocate_filename_flat_single_collision() {
        let tmp = TempDir::new().unwrap();
        let ts = Utc.with_ymd_and_hms(2025, 6, 1, 12, 0, 0).unwrap();
        std::fs::write(tmp.path().join("2025-06-01-120000-hello.md"), "x").unwrap();

        let path = allocate_filename(tmp.path(), ts, "hello", false);
        assert_eq!(path, tmp.path().join("2025-06-01-120000-hello-2.md"),);
    }

    #[test]
    fn allocate_filename_flat_double_collision() {
        let tmp = TempDir::new().unwrap();
        let ts = Utc.with_ymd_and_hms(2025, 6, 1, 12, 0, 0).unwrap();
        std::fs::write(tmp.path().join("2025-06-01-120000-hello.md"), "x").unwrap();
        std::fs::write(tmp.path().join("2025-06-01-120000-hello-2.md"), "x").unwrap();

        let path = allocate_filename(tmp.path(), ts, "hello", false);
        assert_eq!(path, tmp.path().join("2025-06-01-120000-hello-3.md"),);
    }

    #[test]
    fn allocate_filename_bundle_no_collision() {
        let tmp = TempDir::new().unwrap();
        let ts = Utc.with_ymd_and_hms(2025, 6, 1, 12, 0, 0).unwrap();
        let path = allocate_filename(tmp.path(), ts, "hello", true);
        let stem = "2025-06-01-120000-hello";
        assert_eq!(path, tmp.path().join(stem).join(format!("{stem}.md")),);
    }

    #[test]
    fn allocate_filename_bundle_collision_uses_directory_name() {
        let tmp = TempDir::new().unwrap();
        let ts = Utc.with_ymd_and_hms(2025, 6, 1, 12, 0, 0).unwrap();
        // A bundle already occupies the base stem.
        std::fs::create_dir_all(tmp.path().join("2025-06-01-120000-hello")).unwrap();

        let path = allocate_filename(tmp.path(), ts, "hello", true);
        let stem = "2025-06-01-120000-hello-2";
        assert_eq!(path, tmp.path().join(stem).join(format!("{stem}.md")),);
    }

    #[test]
    fn allocate_filename_flat_collides_with_existing_bundle() {
        // A bundle directory at the same stem means the flat `.md` path
        // would live inside someone else's bundle. Bump instead.
        let tmp = TempDir::new().unwrap();
        let ts = Utc.with_ymd_and_hms(2025, 6, 1, 12, 0, 0).unwrap();
        std::fs::create_dir_all(tmp.path().join("2025-06-01-120000-hello")).unwrap();

        let path = allocate_filename(tmp.path(), ts, "hello", false);
        assert_eq!(path, tmp.path().join("2025-06-01-120000-hello-2.md"),);
    }

    #[test]
    fn allocate_filename_no_subject_slug() {
        let tmp = TempDir::new().unwrap();
        let ts = Utc.with_ymd_and_hms(2025, 6, 1, 12, 0, 0).unwrap();
        let path = allocate_filename(tmp.path(), ts, "no-subject", false);
        assert_eq!(path, tmp.path().join("2025-06-01-120000-no-subject.md"),);
    }
}
