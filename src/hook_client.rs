//! Client-side UDS helpers for hook CRUD.
//!
//! With the legacy template / `run_as` / `origin` schema gone, the
//! UDS hook verbs are stubs (see [`crate::hook_handler`]). The CLI
//! routes hook CRUD through `sudo aimx hooks ...` and writes
//! `config.toml` directly; nothing in this module is currently wired
//! to a production caller. The fallback enum is kept here so the CLI
//! surface compiles unchanged while the UDS rewiring lands in a later
//! sprint.

#![allow(dead_code)]

/// Outcome of a hook CRUD submission that didn't succeed via UDS. Tracks
/// socket-missing distinctly from daemon-side errors so the CLI can
/// decide whether to fall back to a direct on-disk edit.
pub(crate) enum HookCrudFallback {
    /// Socket not present / not connectable (daemon stopped, socket
    /// cleaned up, first-time setup). Callers fall back to direct edit.
    SocketMissing,
    /// Daemon connected and answered but reported an error (validation,
    /// NOTFOUND, IO, ...). Caller should surface this verbatim.
    Daemon(String),
}

/// Outcome of a template-CRUD submission that didn't succeed. Kept as a
/// stub for compatibility while the UDS verbs are reworked. Callers
/// today never construct a `Daemon` / `Local` variant — the CLI takes
/// the `SocketMissing` branch and writes `config.toml` directly.
#[derive(Debug)]
pub(crate) enum TemplateCrudFallback {
    /// Socket not present / not connectable.
    SocketMissing,
    /// Daemon answered with `AIMX/1 ERR <code> <reason>`.
    Daemon { code: String, reason: String },
    /// Local I/O or protocol-framing error.
    Local(String),
}
