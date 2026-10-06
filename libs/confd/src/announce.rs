//! Which committed changes are announced, and on which topic.
//!
//! * `sys/...` is world-readable, so its changes go on the public
//!   `system/confd/changed/<path>` topic.
//! * `user/<uid>/<rest>` belongs to one uid. Its changes go on
//!   `user/<uid>/confd/changed/<rest>`, in the kernel's per-uid topic
//!   namespace: only that uid and root may subscribe there
//!   (`kernel/src/ipc/topics/private.rs`, issue #407), which is the same rule
//!   `confd` applies to reading the keys. The payload still carries the full
//!   path.
//!
//! Anything else (`user` itself, `user/<uid>` with nothing below it, an owner
//! segment that is not a uid) is committed but never announced.

use crate::path::{scope, Scope};

/// Where a change to one path is announced.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Announcement<'a> {
    /// `system/confd/changed/<path>`.
    System,
    /// `user/<uid>/confd/changed/<rest>`: `rest` is the path below
    /// `user/<owner segment>/`, `uid` the owner it names.
    User { uid: u32, rest: &'a str },
}

/// Where changes to `path` (already valid) are announced, if anywhere.
pub fn announcement(path: &str) -> Option<Announcement<'_>> {
    match scope(path) {
        Scope::System => Some(Announcement::System),
        Scope::User(uid) => {
            let below_user = path.strip_prefix("user/")?;
            let (_, rest) = below_user.split_once('/')?;
            (!rest.is_empty()).then_some(Announcement::User { uid, rest })
        }
        Scope::Unclaimed => None,
    }
}

/// Whether changes to `path` are announced at all.
pub fn announceable(path: &str) -> bool {
    announcement(path).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_and_user_paths_are_announced_apart() {
        assert_eq!(announcement("sys"), Some(Announcement::System));
        assert_eq!(announcement("sys/ui/mode"), Some(Announcement::System));
        assert_eq!(
            announcement("user/1000/ui/mode"),
            Some(Announcement::User {
                uid: 1000,
                rest: "ui/mode"
            })
        );
        // The owner is the parsed uid, so a padded spelling still reaches
        // that uid's topics.
        assert_eq!(
            announcement("user/0100/x"),
            Some(Announcement::User {
                uid: 100,
                rest: "x"
            })
        );
    }

    #[test]
    fn unowned_or_bare_paths_are_not_announced() {
        for path in ["user", "user/1000", "user/alice/x", "user/4294967296/x"] {
            assert_eq!(announcement(path), None, "{path}");
            assert!(!announceable(path));
        }
        assert!(announceable("user/7/a/b"));
    }
}
