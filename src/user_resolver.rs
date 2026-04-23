//! Linux user-name → uid/gid resolver used by config-load validation.
//!
//! Wraps `libc::getpwnam` behind a pluggable seam so tests can inject a
//! deterministic mapping without needing the running user to exist on the
//! host. Production code always goes through [`resolve_user`]; test code
//! can call [`set_test_resolver`] to override the backing function for the
//! duration of a scope (`ResolverOverride` restores the previous resolver
//! on drop).
//!
//! The single-`libc::getpwnam` call pattern here mirrors the existing
//! `src/platform.rs::lookup_user` helper — this module exposes it with a
//! slightly richer return shape so `Config::load` can distinguish a
//! present-but-orphan user from an outright lookup error.

use std::ffi::CString;

/// A resolved Linux user. `name` echoes the input for convenience so
/// callers can use the resolved value in error messages without cloning
/// the input string separately.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedUser {
    pub name: String,
    pub uid: u32,
    pub gid: u32,
}

/// Resolve a Linux user by name via `getpwnam(3)`. Returns `None` when
/// the lookup returns null (no such user) or the name can't be converted
/// to a C string (contains an interior NUL).
///
/// Production callers invoke this directly; test harnesses can swap the
/// backing implementation via [`set_test_resolver`].
pub fn resolve_user(name: &str) -> Option<ResolvedUser> {
    #[cfg(test)]
    if let Some(resolver) = test_resolver::current() {
        return resolver(name);
    }
    resolve_user_via_libc(name)
}

fn resolve_user_via_libc(name: &str) -> Option<ResolvedUser> {
    let cname = CString::new(name).ok()?;
    // SAFETY: `getpwnam` reads a process-global static. We dereference
    // the returned pointer only to read two scalar fields before any
    // subsequent getpw* call could invalidate them.
    let pw = unsafe { libc::getpwnam(cname.as_ptr()) };
    if pw.is_null() {
        return None;
    }
    let (uid, gid) = unsafe { ((*pw).pw_uid, (*pw).pw_gid) };
    Some(ResolvedUser {
        name: name.to_string(),
        uid,
        gid,
    })
}

#[cfg(test)]
pub(crate) mod test_resolver {
    use super::ResolvedUser;
    use std::sync::{Mutex, RwLock};

    type ResolverFn = fn(&str) -> Option<ResolvedUser>;

    // Two-layer scheme:
    // - `CURRENT` is an `RwLock` around the active resolver function.
    //   `current()` takes a read lock, returns the fn pointer, drops —
    //   so production callers (`validate_run_as`, `owner_uid`, ...)
    //   never block on an override for longer than an atomic-ish read.
    // - `SERIALIZE` is a `Mutex` that serializes concurrent tests that
    //   install overrides. Only one test can hold the token at a time,
    //   which keeps the "set → test-runs → drop" sequence atomic from
    //   the test's perspective without deadlocking production reads.
    static CURRENT: RwLock<Option<ResolverFn>> = RwLock::new(None);
    static SERIALIZE: Mutex<()> = Mutex::new(());

    pub(crate) fn current() -> Option<ResolverFn> {
        *CURRENT.read().unwrap_or_else(|e| e.into_inner())
    }

    /// Test-only override guard. While alive, [`super::resolve_user`]
    /// routes through the installed function. Drop restores the prior
    /// resolver (usually none).
    pub(crate) struct ResolverOverride {
        _serialize: std::sync::MutexGuard<'static, ()>,
        prev: Option<ResolverFn>,
    }

    impl ResolverOverride {
        pub(crate) fn set(f: ResolverFn) -> Self {
            // Serialize tests. If another test is mid-override, block
            // here until its guard drops — that's the whole point.
            let serialize = SERIALIZE.lock().unwrap_or_else(|e| e.into_inner());
            let mut current = CURRENT.write().unwrap_or_else(|e| e.into_inner());
            let prev = *current;
            *current = Some(f);
            drop(current);
            Self {
                _serialize: serialize,
                prev,
            }
        }
    }

    impl Drop for ResolverOverride {
        fn drop(&mut self) {
            let mut current = CURRENT.write().unwrap_or_else(|e| e.into_inner());
            *current = self.prev;
        }
    }
}

/// Global override for `resolve_user` during config-load tests. The
/// returned guard restores the previous resolver on drop; holding the
/// guard also serializes tests so parallel cargo test runs can't stomp
/// on each other's expectations.
#[cfg(test)]
pub(crate) fn set_test_resolver(
    f: fn(&str) -> Option<ResolvedUser>,
) -> test_resolver::ResolverOverride {
    test_resolver::ResolverOverride::set(f)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_root_on_linux_hosts() {
        // root exists on every sane Linux host (uid 0, gid 0). This test
        // only confirms the production path works end-to-end; test-only
        // resolvers are exercised elsewhere.
        let r = resolve_user("root").expect("root must resolve on Linux hosts");
        assert_eq!(r.uid, 0);
        assert_eq!(r.gid, 0);
        assert_eq!(r.name, "root");
    }

    #[test]
    fn returns_none_for_obviously_missing_user() {
        let missing =
            resolve_user("aimx-nonexistent-user-9b2f71a0-e4d5-4b27-9d10-b9eaf12ce4d8-sentinel");
        assert!(missing.is_none(), "sentinel user must not resolve");
    }

    #[test]
    fn test_override_routes_resolution() {
        fn fake(name: &str) -> Option<ResolvedUser> {
            if name == "alice" {
                Some(ResolvedUser {
                    name: "alice".into(),
                    uid: 1001,
                    gid: 1001,
                })
            } else {
                None
            }
        }
        let _guard = set_test_resolver(fake);
        let a = resolve_user("alice").unwrap();
        assert_eq!(a.uid, 1001);
        assert!(resolve_user("nonexistent").is_none());
    }
}
