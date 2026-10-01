//! Who may drive the shell through `os.lazy.shell.v1`.
//!
//! The rule (idl/shell.midl): the caller's kernel-stamped uid is 0 or the
//! shell's own uid. The identity always comes from the kernel (`cred_get` on
//! the message's sender slot), never from the request; when the shell cannot
//! read it, the call is refused.

/// Whether a caller with kernel-stamped `caller_uid` may call a shell running
/// as `shell_uid`. Either is `None` when it could not be read, and then every
/// call is refused: the rule fails closed.
pub fn caller_allowed(caller_uid: Option<u32>, shell_uid: Option<u32>) -> bool {
    match (caller_uid, shell_uid) {
        (Some(caller), Some(shell)) => caller == 0 || caller == shell,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn root_and_the_shells_own_user_are_allowed() {
        assert!(caller_allowed(Some(0), Some(1000)));
        assert!(caller_allowed(Some(1000), Some(1000)));
        assert!(caller_allowed(Some(0), Some(0)));
    }

    #[test]
    fn other_users_and_unreadable_callers_are_refused() {
        assert!(!caller_allowed(Some(1001), Some(1000)));
        assert!(!caller_allowed(Some(5), Some(0)));
        assert!(!caller_allowed(None, Some(1000)));
        assert!(!caller_allowed(None, Some(0)));
        // The shell's own uid unknown: nobody, not even root, gets in.
        assert!(!caller_allowed(Some(0), None));
        assert!(!caller_allowed(Some(1000), None));
    }
}
