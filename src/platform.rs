//! Small platform/OS helpers shared across subcommands.

/// Returns `true` when the current process has effective UID 0 (root).
///
/// On non-Unix targets this always returns `false`.
pub fn is_root() -> bool {
    #[cfg(unix)]
    {
        unsafe { libc::geteuid() == 0 }
    }
    #[cfg(not(unix))]
    {
        false
    }
}
