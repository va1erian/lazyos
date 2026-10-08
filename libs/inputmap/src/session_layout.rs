//! Per-user keyboard layout: which layout is in effect.
//!
//! The machine default is `confd`'s [`LAYOUT_KEY`](crate::LAYOUT_KEY)
//! (`sys/input/layout`, written through `elevd`); it is what the login screen,
//! the text console and every account without a choice of its own type with.
//! An account's own choice is `user/<uid>/input/layout` ([`user_layout_key`]),
//! which the account writes with no prompt. `inputd` cannot read another
//! uid's keys and does not know who is logged in, so the compositor reads the
//! user's key and hands `inputd` the result (`NoteSessionLayout` on
//! `os.lazy.input.shell.v1`); [`LayoutChoice`] combines the two.

use alloc::format;
use alloc::string::String;

use crate::Layout;

/// The path below `user/<uid>/` holding an account's own layout.
pub const USER_SUBPATH: &str = "input/layout";
/// The change-topic `path...` (below `user/<uid>/confd/changed/`) covering
/// an account's own input settings.
pub const USER_FILTER_PATH: &str = "input/#";

/// The key holding `uid`'s own layout. `None` for uid 0, which no session runs
/// as: what it chooses is the machine default (like `uitheme::personal`).
pub fn user_layout_key(uid: u32) -> Option<String> {
    (uid != 0).then(|| format!("user/{uid}/{USER_SUBPATH}"))
}

/// The machine default and the logged-in user's own layout.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LayoutChoice {
    machine: Layout,
    session: Option<Layout>,
}

impl LayoutChoice {
    /// Only the machine default, no session layout.
    pub const fn new(machine: Layout) -> LayoutChoice {
        LayoutChoice {
            machine,
            session: None,
        }
    }

    /// The layout in effect: the session's when it has one.
    pub fn effective(&self) -> Layout {
        self.session.unwrap_or(self.machine)
    }

    /// The machine default changed; the layout now in effect.
    pub fn set_machine(&mut self, layout: Layout) -> Layout {
        self.machine = layout;
        self.effective()
    }

    /// The session's own layout by name (`None`: follow the machine default);
    /// the layout now in effect. A name no layout has is not trusted: the
    /// session follows the machine default, as `inputd` does for `confd`.
    pub fn set_session(&mut self, name: Option<&str>) -> Layout {
        self.session = name.and_then(Layout::from_name);
        self.effective()
    }

    /// The session's own layout, if any.
    pub fn session(&self) -> Option<Layout> {
        self.session
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_session_layout_wins_and_falls_back_to_the_machine() {
        let mut choice = LayoutChoice::new(Layout::Us);
        assert_eq!(choice.effective(), Layout::Us);
        assert_eq!(choice.set_session(Some("fr")), Layout::Fr);
        // A machine change under a session layout changes nothing in effect.
        assert_eq!(choice.set_machine(Layout::Us), Layout::Fr);
        // Logging out goes back to the machine default.
        assert_eq!(choice.set_session(None), Layout::Us);
        assert_eq!(choice.set_machine(Layout::Fr), Layout::Fr);
    }

    #[test]
    fn an_unknown_session_layout_follows_the_machine() {
        let mut choice = LayoutChoice::new(Layout::Fr);
        assert_eq!(choice.set_session(Some("us")), Layout::Us);
        assert_eq!(choice.set_session(Some("klingon")), Layout::Fr);
        assert_eq!(choice.session(), None);
        assert_eq!(choice.set_session(Some("")), Layout::Fr);
    }

    #[test]
    fn every_account_but_uid_0_has_its_own_key() {
        assert_eq!(
            user_layout_key(1000).as_deref(),
            Some("user/1000/input/layout")
        );
        assert_eq!(
            user_layout_key(u32::MAX).as_deref(),
            Some("user/4294967295/input/layout")
        );
        assert_eq!(user_layout_key(0), None);
    }
}
