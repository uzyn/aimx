//! Shared tracing subscriber initializer.
//!
//! Installs a global `tracing_subscriber::fmt` layer writing to stderr.
//! `EnvFilter` honors `RUST_LOG`, falling back to `info` so operators
//! always get the structured `aimx::hook`, `aimx::ingest`, and
//! `aimx::trust` lines without opting in.
//!
//! Idempotent: repeat calls (e.g. integration tests re-entering
//! `serve::run` or `ingest::run` in-process) swallow the duplicate-
//! subscriber error so the first install wins.

use tracing_subscriber::EnvFilter;

/// Install the global tracing subscriber. Safe to call more than once.
pub fn init() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_target(true)
        .compact()
        .try_init();
}
