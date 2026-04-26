//! Single-predicate authorization for every CLI and UDS surface.
//!
//! The whole policy is a small data-in / decision-out function:
//! `authorize(caller_uid, action, mailbox) -> Result<(), AuthError>`.
//!
//! Root passes everything. For every other caller, the action's
//! mailbox argument must resolve to a mailbox whose `owner_uid()` matches
//! the caller. `MailboxCrud` and `SystemCommand` are root-only.
//!
//! The module deliberately imports nothing tokio — it is pure logic so
//! both UDS handlers and CLI gating points can share one predicate.

use crate::config::MailboxConfig;

/// What the caller is asking to do. The variants carrying a mailbox
/// name are informational — the caller still passes the resolved
/// `MailboxConfig` (or `None` when the mailbox does not exist) so the
/// predicate can compare uids without re-looking-up state.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)]
pub enum Action {
    /// Create or delete a mailbox. Root-only.
    MailboxCrud,
    /// Read mail in a named mailbox.
    MailboxRead(String),
    /// Send mail as a named mailbox (the resolved sender of the
    /// outbound message).
    MailboxSendAs(String),
    /// Mark mail in a named mailbox as read or unread.
    MarkReadWrite(String),
    /// Create or delete a hook on a named mailbox.
    HookCrud(String),
    /// Run a system-level command (setup, serve, dkim-keygen, …).
    /// Root-only.
    SystemCommand,
}

/// Why a call was rejected. `NotOwner` carries the mailbox name so
/// callers can build a precise error message; the wire-format response
/// in UDS handlers deliberately drops this detail (no information leak
/// per the PRD).
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)]
pub enum AuthError {
    /// Action requires root and the caller is not uid 0.
    NotRoot,
    /// Caller is not the owner of the named mailbox.
    NotOwner { mailbox: String },
    /// Action references a mailbox the caller could not resolve in the
    /// current config snapshot.
    NoSuchMailbox,
}

impl std::fmt::Display for AuthError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AuthError::NotRoot => write!(f, "not authorized: action requires root"),
            AuthError::NotOwner { mailbox } => {
                write!(f, "not authorized: caller does not own mailbox '{mailbox}'")
            }
            AuthError::NoSuchMailbox => write!(f, "not authorized: no such mailbox"),
        }
    }
}

impl std::error::Error for AuthError {}

/// The single authorization predicate.
///
/// Root (`caller_uid == 0`) passes every action unconditionally.
///
/// For any other caller:
/// - `MailboxCrud` and `SystemCommand` always reject as `NotRoot`.
/// - The remaining actions require `mailbox` to be `Some`; `None`
///   produces `NoSuchMailbox`. When present, the mailbox's
///   `owner_uid()` must equal `caller_uid`, otherwise `NotOwner`.
///
/// The predicate intentionally takes a borrowed `MailboxConfig` so
/// callers can pass either a daemon snapshot view or a freshly-loaded
/// CLI view without copying.
#[allow(dead_code)]
pub fn authorize(
    caller_uid: u32,
    action: Action,
    mailbox: Option<&MailboxConfig>,
) -> Result<(), AuthError> {
    if caller_uid == 0 {
        return Ok(());
    }

    match action {
        Action::MailboxCrud | Action::SystemCommand => Err(AuthError::NotRoot),
        Action::MailboxRead(name)
        | Action::MailboxSendAs(name)
        | Action::MarkReadWrite(name)
        | Action::HookCrud(name) => {
            let mb = mailbox.ok_or(AuthError::NoSuchMailbox)?;
            match mb.owner_uid() {
                Ok(uid) if uid == caller_uid => Ok(()),
                Ok(_) => Err(AuthError::NotOwner { mailbox: name }),
                // An unresolvable owner means the mailbox is currently
                // inactive; treat that as "no such mailbox" from the
                // caller's perspective so we never leak which side
                // (caller mismatch vs. orphan owner) failed.
                Err(_) => Err(AuthError::NoSuchMailbox),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::MailboxConfig;

    fn mailbox_owned_by(owner: &str) -> MailboxConfig {
        MailboxConfig {
            address: "alice@test.com".to_string(),
            owner: owner.to_string(),
            hooks: vec![],
            trust: None,
            trusted_senders: None,
            allow_root_catchall: false,
        }
    }

    fn current_user() -> (u32, String) {
        // SAFETY: getuid() is always safe on Unix. We only read; we do
        // not pass any pointers into libc.
        let uid = unsafe { libc::getuid() };
        let name = std::env::var("USER")
            .or_else(|_| std::env::var("LOGNAME"))
            .unwrap_or_else(|_| "nobody".to_string());
        (uid, name)
    }

    #[test]
    fn root_passes_every_action_with_no_mailbox() {
        assert!(authorize(0, Action::MailboxCrud, None).is_ok());
        assert!(authorize(0, Action::SystemCommand, None).is_ok());
        assert!(authorize(0, Action::MailboxRead("any".into()), None).is_ok());
        assert!(authorize(0, Action::MailboxSendAs("any".into()), None).is_ok());
        assert!(authorize(0, Action::MarkReadWrite("any".into()), None).is_ok());
        assert!(authorize(0, Action::HookCrud("any".into()), None).is_ok());
    }

    #[test]
    fn root_passes_every_action_even_with_unowned_mailbox() {
        let mb = mailbox_owned_by("nobody");
        assert!(authorize(0, Action::MailboxRead("hi".into()), Some(&mb)).is_ok());
        assert!(authorize(0, Action::MailboxSendAs("hi".into()), Some(&mb)).is_ok());
        assert!(authorize(0, Action::MarkReadWrite("hi".into()), Some(&mb)).is_ok());
        assert!(authorize(0, Action::HookCrud("hi".into()), Some(&mb)).is_ok());
    }

    #[test]
    fn non_root_mailbox_crud_is_not_root() {
        assert_eq!(
            authorize(1000, Action::MailboxCrud, None),
            Err(AuthError::NotRoot),
        );
    }

    #[test]
    fn non_root_system_command_is_not_root() {
        assert_eq!(
            authorize(1000, Action::SystemCommand, None),
            Err(AuthError::NotRoot),
        );
    }

    #[test]
    fn non_root_owner_match_passes() {
        let (uid, name) = current_user();
        if uid == 0 {
            // Skip: we cannot test the non-root branch as root.
            return;
        }
        let mb = mailbox_owned_by(&name);
        assert!(authorize(uid, Action::MailboxRead("hi".into()), Some(&mb)).is_ok());
        assert!(authorize(uid, Action::MailboxSendAs("hi".into()), Some(&mb)).is_ok());
        assert!(authorize(uid, Action::MarkReadWrite("hi".into()), Some(&mb)).is_ok());
        assert!(authorize(uid, Action::HookCrud("hi".into()), Some(&mb)).is_ok());
    }

    #[test]
    fn non_root_owner_mismatch_returns_not_owner() {
        let (uid, _) = current_user();
        if uid == 0 {
            return;
        }
        // `root` always resolves to uid 0 on every Linux box, so this
        // is a stable mismatch target regardless of what the test user
        // actually is.
        let mb = mailbox_owned_by("root");
        assert_eq!(
            authorize(uid, Action::MailboxRead("hi".into()), Some(&mb)),
            Err(AuthError::NotOwner {
                mailbox: "hi".into()
            }),
        );
        assert_eq!(
            authorize(uid, Action::MailboxSendAs("hi".into()), Some(&mb)),
            Err(AuthError::NotOwner {
                mailbox: "hi".into()
            }),
        );
        assert_eq!(
            authorize(uid, Action::MarkReadWrite("hi".into()), Some(&mb)),
            Err(AuthError::NotOwner {
                mailbox: "hi".into()
            }),
        );
        assert_eq!(
            authorize(uid, Action::HookCrud("hi".into()), Some(&mb)),
            Err(AuthError::NotOwner {
                mailbox: "hi".into()
            }),
        );
    }

    #[test]
    fn no_such_mailbox_for_mailbox_actions() {
        // Both root-and-non-root: passing `None` for a mailbox-bearing
        // action surfaces `NoSuchMailbox` for non-root callers. Root
        // bypasses the check entirely (covered above).
        assert_eq!(
            authorize(1000, Action::MailboxRead("missing".into()), None),
            Err(AuthError::NoSuchMailbox),
        );
        assert_eq!(
            authorize(1000, Action::MailboxSendAs("missing".into()), None),
            Err(AuthError::NoSuchMailbox),
        );
        assert_eq!(
            authorize(1000, Action::MarkReadWrite("missing".into()), None),
            Err(AuthError::NoSuchMailbox),
        );
        assert_eq!(
            authorize(1000, Action::HookCrud("missing".into()), None),
            Err(AuthError::NoSuchMailbox),
        );
    }

    #[test]
    fn orphan_owner_collapses_to_no_such_mailbox_for_non_root() {
        let (uid, _) = current_user();
        if uid == 0 {
            return;
        }
        // A regex-valid but unresolvable owner: `getpwnam` returns
        // None, so `owner_uid()` errs. The predicate must not leak the
        // distinction; surface as NoSuchMailbox.
        let mb = mailbox_owned_by("aimx-nonexistent-orphan-user");
        assert_eq!(
            authorize(uid, Action::MailboxRead("hi".into()), Some(&mb)),
            Err(AuthError::NoSuchMailbox),
        );
    }

    #[test]
    fn auth_error_display_does_not_leak_uids() {
        // Just confirms Display formats for each variant produce a
        // human-readable line — no panic, no formatting bug. The
        // explicit shape is checked by callers as they wire up.
        assert_eq!(
            format!("{}", AuthError::NotRoot),
            "not authorized: action requires root",
        );
        assert_eq!(
            format!(
                "{}",
                AuthError::NotOwner {
                    mailbox: "hi".into()
                }
            ),
            "not authorized: caller does not own mailbox 'hi'",
        );
        assert_eq!(
            format!("{}", AuthError::NoSuchMailbox),
            "not authorized: no such mailbox",
        );
    }
}
