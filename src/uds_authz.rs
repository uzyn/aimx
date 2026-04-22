//! Sprint 4 — `SO_PEERCRED` UDS authentication.
//!
//! Captures the caller's `{uid, gid, pid, username}` at accept time and
//! exposes authorization helpers that every UDS verb handler consults
//! before touching state. The PRD §6.5 authz table lives here as code:
//!
//! | Verb                                    | Rule                                    |
//! |-----------------------------------------|-----------------------------------------|
//! | `SEND`                                  | caller uid owns the `From:` mailbox, OR is root |
//! | `MARK-READ` / `MARK-UNREAD`             | caller uid owns the target mailbox, OR is root |
//! | `MAILBOX-CREATE` / `MAILBOX-DELETE`     | root only                               |
//! | `HOOK-CREATE` / `HOOK-DELETE`           | caller uid owns the target mailbox, OR is root |
//!
//! Root is a blanket bypass on every ownership check (`decision =
//! "root_bypass"`), logged at info level so `aimx logs` reveals the
//! escalation.

use std::sync::OnceLock;

use tokio::net::UnixStream;
use tokio::net::unix::UCred;

use crate::config::MailboxConfig;
use crate::send_protocol::ErrCode;

/// Peer-credentials snapshot taken at `accept()` time. `username` is
/// resolved lazily on the first call to [`Caller::username`] and then
/// memoized; `getpwuid` misses produce `None` so unknown uids don't
/// crash the handler.
#[derive(Debug)]
pub struct Caller {
    pub uid: u32,
    pub gid: u32,
    pub pid: Option<i32>,
    username_cache: OnceLock<Option<String>>,
}

impl Clone for Caller {
    fn clone(&self) -> Self {
        let cache = OnceLock::new();
        if let Some(v) = self.username_cache.get() {
            let _ = cache.set(v.clone());
        }
        Self {
            uid: self.uid,
            gid: self.gid,
            pid: self.pid,
            username_cache: cache,
        }
    }
}

impl Caller {
    /// Construct from raw `tokio::net::unix::UCred`. The production
    /// accept loop in `src/serve.rs` calls `peer_cred()` on the freshly
    /// accepted `UnixStream` and feeds the value through this helper
    /// so username resolution stays lazy.
    pub fn from_ucred(cred: UCred) -> Self {
        Self {
            uid: cred.uid(),
            gid: cred.gid(),
            pid: cred.pid(),
            username_cache: OnceLock::new(),
        }
    }

    /// Construct an explicit caller (tests and internal dispatchers).
    #[allow(dead_code)]
    pub fn new(uid: u32, gid: u32, pid: Option<i32>) -> Self {
        Self {
            uid,
            gid,
            pid,
            username_cache: OnceLock::new(),
        }
    }

    /// A root (uid 0) caller used where no real credentials exist, e.g.
    /// the in-process inbound ingest path which never crosses the UDS.
    /// Also convenient for tests and harnesses that bypass the real
    /// SO_PEERCRED path.
    #[allow(dead_code)]
    pub fn internal_root() -> Self {
        Self::new(0, 0, None)
    }

    /// Lazily resolve the caller's username via `getpwuid(3)`. Root is
    /// always `Some("root")`. Unknown uids return `None`.
    pub fn username(&self) -> Option<&str> {
        self.username_cache
            .get_or_init(|| lookup_username(self.uid))
            .as_deref()
    }

    /// Log-friendly rendering of the caller's username, falling back to
    /// `"?"` so emitted tracing fields are always populated.
    pub fn username_display(&self) -> &str {
        self.username().unwrap_or("?")
    }

    pub fn is_root(&self) -> bool {
        self.uid == 0
    }
}

fn lookup_username(uid: u32) -> Option<String> {
    // Fast path: root is guaranteed by POSIX. Avoids a `getpwuid(0)`
    // call — test resolvers and minimal container images alike have
    // been known to return null for it.
    if uid == 0 {
        return Some("root".to_string());
    }
    // SAFETY: `getpwuid` reads a process-global static; we copy the
    // `pw_name` C string into an owned `String` before returning so
    // callers never dereference it after another `getpw*` call.
    unsafe {
        let pw = libc::getpwuid(uid as libc::uid_t);
        if pw.is_null() {
            return None;
        }
        let name_ptr = (*pw).pw_name;
        if name_ptr.is_null() {
            return None;
        }
        let cstr = std::ffi::CStr::from_ptr(name_ptr);
        cstr.to_str().ok().map(str::to_string)
    }
}

/// Peer-credential extraction. Returns `Err(ErrCode::Validation)` with
/// a `peer cred unavailable` reason when the kernel can't supply creds
/// (closed connection, non-UDS socket, etc.). Callers forward the
/// error verbatim to the client.
pub fn caller_from_stream(stream: &UnixStream) -> Result<Caller, String> {
    match stream.peer_cred() {
        Ok(c) => Ok(Caller::from_ucred(c)),
        Err(e) => Err(format!("peer cred unavailable: {e}")),
    }
}

/// Decision returned from an authz check. Handlers reject on
/// [`AuthzDecision::Reject`] and continue otherwise; the `RootBypass`
/// variant carries the info-log side-effect that fires from the helper.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthzDecision {
    Accept,
    RootBypass,
}

/// Authz error with a stable `code` for the wire plus a human reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthzReject {
    pub code: ErrCode,
    pub reason: String,
}

/// Require the caller to own `mailbox`, or be root. Used by `SEND`,
/// `MARK-READ`, `MARK-UNREAD`, `HOOK-CREATE`, `HOOK-DELETE`.
pub fn require_mailbox_owner_or_root(
    caller: &Caller,
    mailbox_name: &str,
    mailbox: &MailboxConfig,
) -> Result<AuthzDecision, AuthzReject> {
    if caller.is_root() {
        return Ok(AuthzDecision::RootBypass);
    }
    let caller_name = match caller.username() {
        Some(n) => n,
        None => {
            return Err(AuthzReject {
                code: ErrCode::Eaccess,
                reason: format!(
                    "caller uid {} has no resolvable username; cannot authorize against mailbox '{mailbox_name}'",
                    caller.uid
                ),
            });
        }
    };
    if caller_name == mailbox.owner {
        Ok(AuthzDecision::Accept)
    } else {
        Err(AuthzReject {
            code: ErrCode::Eaccess,
            reason: format!(
                "caller '{caller_name}' is not the owner of mailbox '{mailbox_name}' \
                 (owner: '{}')",
                mailbox.owner
            ),
        })
    }
}

/// Require the caller to be root. Used by `MAILBOX-CREATE` and
/// `MAILBOX-DELETE` per PRD §6.5.
pub fn require_root(caller: &Caller) -> Result<AuthzDecision, AuthzReject> {
    if caller.is_root() {
        // Not a "bypass" — root is the required identity here, not an
        // escalation past an ownership rule. Surface as Accept so the
        // structured log says `decision = "accept"`.
        Ok(AuthzDecision::Accept)
    } else {
        Err(AuthzReject {
            code: ErrCode::Eaccess,
            reason: format!(
                "mailbox CRUD is root-only (caller uid {} / '{}')",
                caller.uid,
                caller.username_display()
            ),
        })
    }
}

/// Emit a structured tracing event under `aimx::uds` describing one
/// authz decision. `verb` / `mailbox` / `decision` identify the event;
/// `caller_uid` / `caller_username` identify the actor.
pub fn log_decision(
    verb: &str,
    caller: &Caller,
    mailbox: Option<&str>,
    decision: &str,
    reason: Option<&str>,
) {
    let mailbox = mailbox.unwrap_or("");
    let reason = reason.unwrap_or("");
    match decision {
        "reject" => tracing::warn!(
            target: "aimx::uds",
            verb = verb,
            caller_uid = caller.uid,
            caller_username = caller.username_display(),
            mailbox = mailbox,
            decision = decision,
            reason = reason,
            "uds authz reject"
        ),
        "root_bypass" => tracing::info!(
            target: "aimx::uds",
            verb = verb,
            caller_uid = caller.uid,
            caller_username = caller.username_display(),
            mailbox = mailbox,
            decision = decision,
            "uds authz root_bypass"
        ),
        _ => tracing::debug!(
            target: "aimx::uds",
            verb = verb,
            caller_uid = caller.uid,
            caller_username = caller.username_display(),
            mailbox = mailbox,
            decision = decision,
            "uds authz accept"
        ),
    }
}

/// Convenience: evaluate + log in one call. Used by handlers that want
/// the structured log for every decision (accept + reject + root_bypass)
/// without duplicating the match arms.
pub fn enforce_mailbox_owner_or_root(
    verb: &str,
    caller: &Caller,
    mailbox_name: &str,
    mailbox: &MailboxConfig,
) -> Result<(), AuthzReject> {
    match require_mailbox_owner_or_root(caller, mailbox_name, mailbox) {
        Ok(AuthzDecision::Accept) => {
            log_decision(verb, caller, Some(mailbox_name), "accept", None);
            Ok(())
        }
        Ok(AuthzDecision::RootBypass) => {
            log_decision(verb, caller, Some(mailbox_name), "root_bypass", None);
            Ok(())
        }
        Err(reject) => {
            log_decision(
                verb,
                caller,
                Some(mailbox_name),
                "reject",
                Some(&reject.reason),
            );
            Err(reject)
        }
    }
}

/// Convenience for verbs whose mailbox identity is not known up front
/// (e.g. `MAILBOX-CREATE` / `MAILBOX-DELETE`). Logs without a `mailbox`
/// field.
pub fn enforce_root(
    verb: &str,
    caller: &Caller,
    mailbox_hint: Option<&str>,
) -> Result<(), AuthzReject> {
    match require_root(caller) {
        Ok(_) => {
            log_decision(verb, caller, mailbox_hint, "accept", None);
            Ok(())
        }
        Err(reject) => {
            log_decision(verb, caller, mailbox_hint, "reject", Some(&reject.reason));
            Err(reject)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn mb(owner: &str) -> MailboxConfig {
        MailboxConfig {
            address: "alice@example.com".into(),
            owner: owner.into(),
            hooks: vec![],
            trust: None,
            trusted_senders: None,
            allow_root_catchall: false,
        }
    }

    #[test]
    fn caller_internal_root_is_root() {
        let c = Caller::internal_root();
        assert!(c.is_root());
        assert_eq!(c.username(), Some("root"));
    }

    #[test]
    fn caller_clone_preserves_cached_username() {
        let c = Caller::internal_root();
        let _ = c.username();
        let c2 = c.clone();
        assert_eq!(c2.username(), Some("root"));
    }

    #[test]
    fn lookup_unknown_uid_returns_none() {
        // 32-bit max UID is reserved for "nobody" on some systems but
        // typically unmapped on CI runners.
        assert!(lookup_username(4_294_967_294).is_none());
    }

    #[test]
    fn require_mailbox_owner_or_root_accepts_owner_by_name() {
        // Simulate a caller whose username resolves to "alice".
        let c = Caller::new(1001, 1001, None);
        let _ = c.username_cache.set(Some("alice".to_string()));
        let m = mb("alice");
        assert_eq!(
            require_mailbox_owner_or_root(&c, "alice", &m).unwrap(),
            AuthzDecision::Accept
        );
    }

    #[test]
    fn require_mailbox_owner_or_root_rejects_non_owner() {
        let c = Caller::new(1002, 1002, None);
        let _ = c.username_cache.set(Some("bob".to_string()));
        let m = mb("alice");
        let err = require_mailbox_owner_or_root(&c, "alice", &m).unwrap_err();
        assert_eq!(err.code, ErrCode::Eaccess);
        assert!(err.reason.contains("bob"));
        assert!(err.reason.contains("alice"));
    }

    #[test]
    fn require_mailbox_owner_or_root_root_is_bypass() {
        let c = Caller::internal_root();
        let m = mb("alice");
        assert_eq!(
            require_mailbox_owner_or_root(&c, "alice", &m).unwrap(),
            AuthzDecision::RootBypass
        );
    }

    #[test]
    fn require_mailbox_owner_or_root_unresolvable_uid_rejects() {
        let c = Caller::new(4_294_967_294, 4_294_967_294, None);
        let _ = c.username_cache.set(None);
        let m = mb("alice");
        let err = require_mailbox_owner_or_root(&c, "alice", &m).unwrap_err();
        assert_eq!(err.code, ErrCode::Eaccess);
        assert!(err.reason.contains("no resolvable username"));
    }

    #[test]
    fn require_root_rejects_non_root() {
        let c = Caller::new(1002, 1002, None);
        let _ = c.username_cache.set(Some("bob".to_string()));
        let err = require_root(&c).unwrap_err();
        assert_eq!(err.code, ErrCode::Eaccess);
        assert!(err.reason.contains("bob"));
    }

    #[test]
    fn require_root_accepts_root() {
        let c = Caller::internal_root();
        assert_eq!(require_root(&c).unwrap(), AuthzDecision::Accept);
    }

    #[test]
    #[tracing_test::traced_test]
    fn enforce_mailbox_owner_or_root_logs_accept() {
        let c = Caller::new(1001, 1001, None);
        let _ = c.username_cache.set(Some("alice".to_string()));
        let m = mb("alice");
        enforce_mailbox_owner_or_root("SEND", &c, "alice", &m).unwrap();
        assert!(logs_contain("decision=\"accept\""));
        assert!(logs_contain("verb=\"SEND\""));
        assert!(logs_contain("caller_username=\"alice\""));
    }

    #[test]
    #[tracing_test::traced_test]
    fn enforce_mailbox_owner_or_root_logs_root_bypass() {
        let c = Caller::internal_root();
        let m = mb("alice");
        enforce_mailbox_owner_or_root("SEND", &c, "alice", &m).unwrap();
        assert!(logs_contain("decision=\"root_bypass\""));
        assert!(logs_contain("caller_uid=0"));
    }

    #[test]
    #[tracing_test::traced_test]
    fn enforce_mailbox_owner_or_root_logs_reject() {
        let c = Caller::new(1002, 1002, None);
        let _ = c.username_cache.set(Some("bob".to_string()));
        let m = mb("alice");
        let err = enforce_mailbox_owner_or_root("SEND", &c, "alice", &m).unwrap_err();
        assert_eq!(err.code, ErrCode::Eaccess);
        assert!(logs_contain("decision=\"reject\""));
        assert!(logs_contain("caller_username=\"bob\""));
    }

    #[test]
    #[tracing_test::traced_test]
    fn enforce_root_logs_reject() {
        let c = Caller::new(1002, 1002, None);
        let _ = c.username_cache.set(Some("bob".to_string()));
        let err = enforce_root("MAILBOX-CREATE", &c, Some("alice")).unwrap_err();
        assert_eq!(err.code, ErrCode::Eaccess);
        assert!(logs_contain("decision=\"reject\""));
        assert!(logs_contain("verb=\"MAILBOX-CREATE\""));
    }

    // Pull in HashMap just so the import isn't flagged unused if a
    // follow-up test wants to simulate a Config lookup. Not material.
    #[allow(dead_code)]
    fn _hashmap_sanity() -> HashMap<&'static str, ()> {
        HashMap::new()
    }
}
