//! Single-predicate authorization for every CLI and UDS surface.
//!
//! The whole policy is a small data-in / decision-out function:
//! `authorize(caller_uid, action, mailbox) -> Result<(), AuthError>`.
//!
//! Root passes everything. For every other caller, the action's
//! mailbox argument must resolve to a mailbox whose `owner_uid()` matches
//! the caller. `MailboxCreate { owner_uid }` passes when the requested
//! owner equals the caller; `MailboxDelete { mailbox }` passes when the
//! caller owns the resolved mailbox. `SystemCommand` is root-only.
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
    /// Create a mailbox owned by `owner_uid`. Non-root passes only when
    /// `caller_uid == owner_uid` (i.e. the caller is asking for a
    /// mailbox owned by themselves). Cross-uid creates remain
    /// operator-only because no other surface accepts an owner override
    /// from a non-root caller — the daemon synthesizes the owner from
    /// `SO_PEERCRED` for non-root UDS callers, so this variant is
    /// always called with the caller's own uid in that path.
    MailboxCreate { owner_uid: u32 },
    /// Delete a named mailbox. Same shape as `MailboxRead`/`HookCrud`:
    /// non-root passes when the resolved `MailboxConfig`'s `owner_uid()`
    /// equals the caller's uid.
    MailboxDelete { mailbox: String },
    /// Read mail in a named mailbox. Reachable from MCP and CLI gating
    /// only — there is no UDS verb for reads, since `aimx mcp` and
    /// the CLI inspect the filesystem directly.
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
/// - `SystemCommand` always rejects as `NotRoot`.
/// - `MailboxCreate { owner_uid }` passes when `caller_uid == owner_uid`;
///   otherwise `NotOwner { mailbox: "<new>" }` so the wire-format error
///   stays consistent with the other owner-mismatch paths.
/// - `MailboxDelete { mailbox }` and the other mailbox-bearing variants
///   require `mailbox` to be `Some`; `None` produces `NoSuchMailbox`.
///   When present, the mailbox's `owner_uid()` must equal `caller_uid`,
///   otherwise `NotOwner`.
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
        Action::SystemCommand => Err(AuthError::NotRoot),
        Action::MailboxCreate { owner_uid } => {
            if caller_uid == owner_uid {
                Ok(())
            } else {
                // The mailbox does not exist yet. Use the sentinel
                // `"<new>"` so the wire-shaped error keeps the owner-
                // mismatch framing without inventing a fresh variant
                // (and without leaking the requested owner uid into
                // the response).
                Err(AuthError::NotOwner {
                    mailbox: "<new>".to_string(),
                })
            }
        }
        Action::MailboxDelete { mailbox: name }
        | Action::MailboxRead(name)
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
        let uid = nix::unistd::Uid::current();
        let name = nix::unistd::User::from_uid(uid)
            .ok()
            .flatten()
            .map(|u| u.name)
            .unwrap_or_else(|| "nobody".to_string());
        (uid.as_raw(), name)
    }

    #[test]
    fn root_passes_every_action_with_no_mailbox() {
        assert!(authorize(0, Action::MailboxCreate { owner_uid: 0 }, None).is_ok());
        assert!(authorize(0, Action::MailboxCreate { owner_uid: 1000 }, None).is_ok());
        assert!(
            authorize(
                0,
                Action::MailboxDelete {
                    mailbox: "any".into()
                },
                None
            )
            .is_ok()
        );
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
    fn non_root_mailbox_create_owner_match_passes() {
        // The structural privilege-escalation defense: the daemon
        // synthesizes `owner_uid` from `SO_PEERCRED` for non-root
        // callers, so the predicate is always called with caller_uid
        // == owner_uid in the legitimate path. This test pins that
        // exact contract.
        assert!(authorize(1000, Action::MailboxCreate { owner_uid: 1000 }, None).is_ok());
    }

    #[test]
    fn non_root_mailbox_create_owner_mismatch_rejects() {
        // If a non-root caller somehow reached `authorize` with a
        // mismatching owner_uid, the predicate must reject. In the
        // production path the daemon never lets that happen because it
        // ignores the wire `Owner:` header and supplies the caller's
        // own uid. Belt-and-braces: the predicate refuses on its own.
        assert_eq!(
            authorize(1000, Action::MailboxCreate { owner_uid: 0 }, None),
            Err(AuthError::NotOwner {
                mailbox: "<new>".into()
            }),
        );
        assert_eq!(
            authorize(1000, Action::MailboxCreate { owner_uid: 1001 }, None),
            Err(AuthError::NotOwner {
                mailbox: "<new>".into()
            }),
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
    fn root_passes_new_variants_unconditionally() {
        // Root passes MailboxCreate even when the owner_uid is some
        // arbitrary other user, and passes MailboxDelete even when the
        // mailbox arg is None or an unowned/orphaned mailbox.
        assert!(authorize(0, Action::MailboxCreate { owner_uid: 12345 }, None).is_ok());
        assert!(
            authorize(
                0,
                Action::MailboxDelete {
                    mailbox: "ghost".into()
                },
                None
            )
            .is_ok()
        );
        let mb = mailbox_owned_by("aimx-nonexistent-orphan-user");
        assert!(
            authorize(
                0,
                Action::MailboxDelete {
                    mailbox: "alice".into()
                },
                Some(&mb)
            )
            .is_ok()
        );
    }

    #[test]
    #[ignore = "requires non-root host; surfaces via cargo test --ignored"]
    fn non_root_owner_match_passes() {
        let (uid, name) = current_user();
        assert_ne!(uid, 0, "test must run as a non-root user");
        let mb = mailbox_owned_by(&name);
        assert!(authorize(uid, Action::MailboxRead("hi".into()), Some(&mb)).is_ok());
        assert!(authorize(uid, Action::MailboxSendAs("hi".into()), Some(&mb)).is_ok());
        assert!(authorize(uid, Action::MarkReadWrite("hi".into()), Some(&mb)).is_ok());
        assert!(authorize(uid, Action::HookCrud("hi".into()), Some(&mb)).is_ok());
    }

    #[test]
    #[ignore = "requires non-root host; surfaces via cargo test --ignored"]
    fn non_root_mailbox_delete_owner_match_passes() {
        // Mirrors the existing `non_root_owner_match_passes` test:
        // when the resolved mailbox's owner_uid matches the caller,
        // MailboxDelete is allowed.
        let (uid, name) = current_user();
        assert_ne!(uid, 0, "test must run as a non-root user");
        let mb = mailbox_owned_by(&name);
        assert!(
            authorize(
                uid,
                Action::MailboxDelete {
                    mailbox: "hi".into()
                },
                Some(&mb)
            )
            .is_ok()
        );
    }

    #[test]
    #[ignore = "requires non-root host; surfaces via cargo test --ignored"]
    fn non_root_mailbox_delete_owner_mismatch_returns_not_owner() {
        let (uid, _) = current_user();
        assert_ne!(uid, 0, "test must run as a non-root user");
        // `root` always resolves to uid 0 — a stable mismatch target.
        let mb = mailbox_owned_by("root");
        assert_eq!(
            authorize(
                uid,
                Action::MailboxDelete {
                    mailbox: "hi".into()
                },
                Some(&mb)
            ),
            Err(AuthError::NotOwner {
                mailbox: "hi".into()
            }),
        );
    }

    #[test]
    #[ignore = "requires non-root host; surfaces via cargo test --ignored"]
    fn non_root_owner_mismatch_returns_not_owner() {
        let (uid, _) = current_user();
        assert_ne!(uid, 0, "test must run as a non-root user");
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
    #[ignore = "requires non-root host; surfaces via cargo test --ignored"]
    fn orphan_owner_collapses_to_no_such_mailbox_for_non_root() {
        let (uid, _) = current_user();
        assert_ne!(uid, 0, "test must run as a non-root user");
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
