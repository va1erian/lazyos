//! The `inputd` session's I3 features as Doom uses them (`docs/input-plan.md`):
//! the key-state page as the authority on held keys, and a keyboard grab
//! while the window is maximized.
//!
//! - [`SessionKeys`] remembers which physical key (HID usage) pressed which
//!   Doom key, so the polled bitmap can release a key whose `Up` event never
//!   arrived (a dropped backlog, focus moving between two events): if the
//!   page says a key is up, the player stops, whatever the event queue holds.
//! - [`GrabPolicy`] asks for a keyboard grab while the window is maximized
//!   and focused (so Ctrl+Esc, Alt+Tab and Super reach the game: Ctrl fires
//!   and Esc opens the menu), releases it when either stops, and does not ask
//!   again after the user escaped it (Ctrl+Alt+Esc) or the compositor said no,
//!   until the window is re-maximized or focused again.

use crate::keys::Keys;

/// Physical keys and the Doom key each one pressed.
pub struct SessionKeys {
    held: [Option<u8>; 256],
}

impl Default for SessionKeys {
    fn default() -> SessionKeys {
        SessionKeys::new()
    }
}

impl SessionKeys {
    pub fn new() -> SessionKeys {
        SessionKeys { held: [None; 256] }
    }

    /// HID usage `code` went down as Doom key `key`.
    pub fn press(&mut self, code: u32, key: u8, keys: &mut Keys) {
        if let Some(slot) = self.held.get_mut(code as usize) {
            *slot = Some(key);
        }
        keys.press(key);
    }

    /// HID usage `code` went up.
    pub fn release(&mut self, code: u32, keys: &mut Keys) {
        let Some(key) = self.held.get_mut(code as usize).and_then(Option::take) else {
            return;
        };
        // Both Ctrl keys fire: Doom's key stays down while either is held.
        if !self.held.contains(&Some(key)) {
            keys.release(key);
        }
    }

    /// Focus left: everything is up.
    pub fn release_all(&mut self, keys: &mut Keys) {
        self.held = [None; 256];
        keys.release_all();
    }

    /// The key-state page says which usages are down (`is_down`, only for a
    /// focused page): release every key it says is up. Returns how many.
    pub fn reconcile(&mut self, is_down: impl Fn(u16) -> bool, keys: &mut Keys) -> usize {
        let stale: Vec<u32> = (0..=255u16)
            .filter(|&code| self.held[code as usize].is_some() && !is_down(code))
            .map(u32::from)
            .collect();
        for &code in &stale {
            self.release(code, keys);
        }
        stale.len()
    }
}

/// What the window should do about the grab after an event.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GrabAction {
    None,
    Request,
    Release,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Grab {
    Idle,
    Asked,
    Held,
    /// Ended by the user or refused: not asked again until wanted afresh.
    Declined,
}

pub struct GrabPolicy {
    maximized: bool,
    focused: bool,
    grab: Grab,
}

impl Default for GrabPolicy {
    fn default() -> GrabPolicy {
        GrabPolicy::new()
    }
}

impl GrabPolicy {
    pub fn new() -> GrabPolicy {
        GrabPolicy {
            maximized: false,
            focused: false,
            grab: Grab::Idle,
        }
    }

    fn wanted(&self) -> bool {
        self.maximized && self.focused
    }

    /// Whether the grab is held right now.
    pub fn held(&self) -> bool {
        self.grab == Grab::Held
    }

    fn update(&mut self, was_wanted: bool) -> GrabAction {
        match (was_wanted, self.wanted(), self.grab) {
            (false, true, Grab::Idle | Grab::Declined) => {
                self.grab = Grab::Asked;
                GrabAction::Request
            }
            (true, false, Grab::Asked | Grab::Held) => {
                self.grab = Grab::Idle;
                GrabAction::Release
            }
            (true, false, Grab::Declined) => {
                self.grab = Grab::Idle;
                GrabAction::None
            }
            _ => GrabAction::None,
        }
    }

    /// The window was configured (`maximized`: its `WindowState`).
    pub fn configured(&mut self, maximized: bool) -> GrabAction {
        let was = self.wanted();
        self.maximized = maximized;
        self.update(was)
    }

    /// Keyboard focus arrived (`true`) or left.
    pub fn focus(&mut self, focused: bool) -> GrabAction {
        let was = self.wanted();
        self.focused = focused;
        self.update(was)
    }

    /// `inputd` reported the grab started or ended.
    pub fn granted(&mut self, active: bool) {
        self.grab = match (active, self.grab) {
            (true, Grab::Asked | Grab::Held) => Grab::Held,
            // A late grant for a request already withdrawn: let it go.
            (true, _) => self.grab,
            (false, _) if self.wanted() => Grab::Declined,
            (false, _) => Grab::Idle,
        };
    }

    /// A request that failed outright (no focus yet, no `inputd`).
    pub fn request_failed(&mut self) {
        if self.grab == Grab::Asked {
            self.grab = Grab::Declined;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keymap::doom;

    fn drain(keys: &mut Keys) -> Vec<(bool, u8)> {
        std::iter::from_fn(|| keys.pop()).collect()
    }

    #[test]
    fn the_page_releases_a_key_whose_up_event_was_lost() {
        let (mut session, mut keys) = (SessionKeys::new(), Keys::new());
        session.press(0x1A, doom::UP, &mut keys); // W
        session.press(0xE0, doom::FIRE, &mut keys); // Left Ctrl
        drain(&mut keys);
        // The page: Ctrl still down, W up (its Up never came).
        let released = session.reconcile(|code| code == 0xE0, &mut keys);
        assert_eq!(released, 1);
        assert_eq!(drain(&mut keys), [(false, doom::UP)]);
        assert!(keys.is_down(doom::FIRE));
        // A late Up for W changes nothing.
        session.release(0x1A, &mut keys);
        assert_eq!(drain(&mut keys), []);
    }

    #[test]
    fn two_physical_keys_one_doom_key() {
        let (mut session, mut keys) = (SessionKeys::new(), Keys::new());
        session.press(0xE0, doom::FIRE, &mut keys);
        session.press(0xE4, doom::FIRE, &mut keys);
        session.release(0xE0, &mut keys);
        assert!(keys.is_down(doom::FIRE), "the right Ctrl still fires");
        session.release(0xE4, &mut keys);
        assert!(!keys.is_down(doom::FIRE));
        session.press(0xE0, doom::FIRE, &mut keys);
        session.release_all(&mut keys);
        assert!(!keys.is_down(doom::FIRE));
        assert_eq!(session.reconcile(|_| false, &mut keys), 0);
    }

    #[test]
    fn grabbed_while_maximized_and_focused() {
        let mut policy = GrabPolicy::new();
        assert_eq!(policy.focus(true), GrabAction::None);
        assert_eq!(policy.configured(true), GrabAction::Request);
        policy.granted(true);
        assert!(policy.held());
        // A second Configure with the same state asks nothing.
        assert_eq!(policy.configured(true), GrabAction::None);
        assert_eq!(policy.configured(false), GrabAction::Release);
        assert!(!policy.held());
        assert_eq!(policy.configured(true), GrabAction::Request);
        assert_eq!(policy.focus(false), GrabAction::Release);
        assert_eq!(policy.focus(true), GrabAction::Request);
    }

    #[test]
    fn an_escaped_grab_is_not_taken_back_at_once() {
        let mut policy = GrabPolicy::new();
        policy.focus(true);
        policy.configured(true);
        policy.granted(true);
        // Ctrl+Alt+Esc: inputd ends it; the window stays maximized and
        // focused, and must not ask again by itself.
        policy.granted(false);
        assert!(!policy.held());
        assert_eq!(policy.configured(true), GrabAction::None);
        assert_eq!(policy.focus(true), GrabAction::None);
        // Focusing it afresh (or re-maximizing) is a new wish.
        assert_eq!(policy.focus(false), GrabAction::None);
        assert_eq!(policy.focus(true), GrabAction::Request);
        // A refusal is the same.
        policy.granted(false);
        assert_eq!(policy.configured(true), GrabAction::None);
        policy.request_failed();
        assert_eq!(policy.configured(false), GrabAction::None);
        assert_eq!(policy.configured(true), GrabAction::Request);
    }
}
