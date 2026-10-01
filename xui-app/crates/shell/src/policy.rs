//! Who may drive the shell through `os.lazy.shell.v1`.
//!
//! The rule (idl/shell.midl): the caller's kernel-stamped uid is 0 or the
//! shell's own uid. The identity always comes from the kernel (`cred_get` on
//! the message's sender slot), never from the request; when the shell cannot
//! read it, the call is refused.

/// Whether a caller with kernel-stamped `caller_uid` (`None` when the shell
/// could not read it) may call a shell running as `shell_uid`.
pub fn caller_allowed(caller_uid: Option<u32>, shell_uid: u32) -> bool {
    matches!(caller_uid, Some(uid) if uid == 0 || uid == shell_uid)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn root_and_the_shells_own_user_are_allowed() {
        assert!(caller_allowed(Some(0), 1000));
        assert!(caller_allowed(Some(1000), 1000));
        assert!(caller_allowed(Some(0), 0));
    }

    #[test]
    fn other_users_and_unreadable_callers_are_refused() {
        assert!(!caller_allowed(Some(1001), 1000));
        assert!(!caller_allowed(Some(5), 0));
        assert!(!caller_allowed(None, 1000));
        assert!(!caller_allowed(None, 0));
    }
}
