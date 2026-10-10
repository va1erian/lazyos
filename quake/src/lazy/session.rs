//! The `inputd` session's key-state page and keyboard grab, as Quake uses
//! them (kept in step with the Doom port's `session.rs`; `docs/input-plan.md`):
//!
//! - [`SessionKeys`] remembers which physical key (HID usage) pressed which
//!   Quake key, so the polled bitmap can release a key whose `Up` event
//!   never arrived (a dropped backlog, focus moving between two events):
//!   if the page says a key is up, the player stops, whatever the event
//!   queue holds;
//! - [`GrabPolicy`] asks for a keyboard grab while the window is maximized
//!   and focused — so Ctrl+Esc, Alt+Tab and Super reach the game (menu,
//!   quicksave) — releases it when either stops, and does not ask again
//!   after the user escaped it (Ctrl+Alt+Esc) or the compositor said no,
//!   until the window is re-maximized or focused again.
//!
//! Same file as the Doom port's, with Quake keys; keep them in step.

/// Physical keys and the Quake keynum each one pressed.
pub struct SessionKeys {
    held: [Option<(u8, u32)>; 256],
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

    /// Whether a physical key is still holding a key edge it has not
    /// released.
    pub fn holding(&self, code: u32) -> bool {
        self.held.get(code as usize).is_some_and(Option::is_some)
    }

    /// HID usage `code` went down as Quake key `key` (its character
    /// recorded, since the legacy path's late release needs it back).
    /// Returns whether this edge is new.
    pub fn press(&mut self, code: u32, key: (u8, u32)) -> bool {
        let Some(slot) = self.held.get_mut(code as usize) else {
            return false;
        };
        if slot.is_some() {
            return false;
        }
        *slot = Some(key);
        true
    }

    /// HID usage `code` went up; the key edge leaves when no other physical
    /// key still holds it.
    pub fn release(&mut self, code: u32) -> Option<(u8, u32)> {
        let Some(key) = self.held.get_mut(code as usize).and_then(Option::take) else {
            return None;
        };
        (!self.held.contains(&Some(key))).then_some(key)
    }

    /// Focus left: everything is up.
    pub fn release_all(&mut self) -> Vec<(u8, u32)> {
        let keys = self.held.iter().flatten().copied().collect();
        self.held = [None; 256];
        keys
    }

    /// The key-state page says which usages are down (`is_down`, only for a
    /// focused page): release every key it says is up. Returns the Quake
    /// keys released.
    pub fn reconcile(&mut self, is_down: impl Fn(u16) -> bool) -> Vec<(u8, u32)> {
        // The stale codes first (the closure reads `self`), then their
        // releases one by one.
        let stale: Vec<u16> = (0..=255u16)
            .filter(|&code| self.held[code as usize].is_some() && !is_down(code))
            .collect();
        stale
            .into_iter()
            .filter_map(|code| self.release(u32::from(code)))
            .collect()
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

    const W: (u8, u32) = (b'w' as u8, b'w' as u32);
    const CTRL: (u8, u32) = (133, 0);

    #[test]
    fn the_page_releases_a_key_whose_up_event_was_lost() {
        let mut session = SessionKeys::new();
        assert!(session.press(0x1a, W)); // W
        let released = session.reconcile(|code| code != 0x1a);
        assert_eq!(released, vec![W]);
        // A late Up for W changes nothing.
        assert_eq!(session.release(0x1a), None);
    }

    #[test]
    fn two_physical_keys_one_quake_key() {
        let mut session = SessionKeys::new();
        assert!(session.press(0xe0, CTRL));
        assert!(session.press(0xe4, CTRL));
        assert_eq!(session.release(0xe0), None, "the right Ctrl still fires");
        assert_eq!(session.release(0xe4), Some(CTRL));
        assert!(session.press(0xe0, CTRL));
        assert_eq!(session.release_all().len(), 1);
        assert_eq!(session.reconcile(|_| false), Vec::<(u8, u32)>::new());
    }

    #[test]
    fn grabbed_while_maximized_and_focused() {
        let mut policy = GrabPolicy::new();
        assert_eq!(policy.focus(true), GrabAction::None);
        assert_eq!(policy.configured(true), GrabAction::Request);
        policy.granted(true);
        assert!(policy.held());
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
        // Focusing it afresh (or re-maximizing) is a new wish.
        assert_eq!(policy.focus(false), GrabAction::None);
        assert_eq!(policy.focus(true), GrabAction::Request);
        policy.granted(false);
        assert_eq!(policy.configured(true), GrabAction::None);
        policy.request_failed();
        assert_eq!(policy.configured(true), GrabAction::None, "the refusal stands");
    }
}
