use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

fn main() {
    let hash = Command::new("git")
        .args(["rev-parse", "--short=8", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|| "unknown".to_string());

    // `git describe --tags --always --dirty` → release tag or `dev` fallback.
    // Non-git checkouts (e.g. published crate tarball extract) fall back to `dev`.
    //
    // Tags are bare SemVer (`1.95.0`, matching Rust's own release
    // convention). If git returns a legacy `v`-prefixed tag, strip the
    // leading `v` leniently so `aimx --version` renders the bare form.
    // Non-tag describe output (`dev`, `g<sha>`, `<tag>-N-g<sha>-dirty`)
    // is passed through unchanged.
    let tag = Command::new("git")
        .args(["describe", "--tags", "--always", "--dirty"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
        .map(strip_legacy_v_prefix)
        .unwrap_or_else(|| "dev".to_string());

    let target = std::env::var("TARGET").unwrap_or_else(|_| "unknown".to_string());
    let build_date = format_utc_date(SystemTime::now());

    println!("cargo:rustc-env=GIT_HASH={hash}");
    println!("cargo:rustc-env=RELEASE_TAG={tag}");
    println!("cargo:rustc-env=TARGET={target}");
    println!("cargo:rustc-env=BUILD_DATE={build_date}");
    println!("cargo:rerun-if-changed=.git/HEAD");
    println!("cargo:rerun-if-changed=.git/refs");
}

/// Strip a single leading `v` only when it is immediately followed by a
/// digit — i.e. treat `v1.2.3`, `v0.0.0-fixture`, and `v1.0.0-12-gabcdef1`
/// as legacy-prefixed SemVer and return the bare form (`1.2.3`,
/// `0.0.0-fixture`, `1.0.0-12-gabcdef1`). Leaves short-hash describe output
/// (`g<hex>`), `dev`, and arbitrary non-tag strings untouched.
fn strip_legacy_v_prefix(s: String) -> String {
    if let Some(rest) = s.strip_prefix('v')
        && rest.chars().next().is_some_and(|c| c.is_ascii_digit())
    {
        return rest.to_string();
    }
    s
}

/// Format a `SystemTime` as a UTC `YYYY-MM-DD` date string without pulling in
/// `chrono` here (build.rs has no access to the main crate's deps without
/// also adding them as build-deps). Hand-rolled proleptic Gregorian conversion.
fn format_utc_date(now: SystemTime) -> String {
    let secs = now
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let days = secs.div_euclid(86_400);

    // Days since 1970-01-01 → (year, month, day). Algorithm from Howard
    // Hinnant's "date algorithms" — correct for the proleptic Gregorian
    // calendar for every date `chrono` or `time` would produce here.
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = if m <= 2 { y + 1 } else { y };

    format!("{y:04}-{m:02}-{d:02}")
}
