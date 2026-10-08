//! Key content held back from the focused session while the shell's panel
//! menu has the keyboard (issue #648 follow-up): the compositor's
//! `NoteKeysHeld` (`os.lazy.input.shell.v1`).
//!
//! A shell panel (the start menu, a tray menu) never takes keyboard focus:
//! the window under it stays focused, its title lit, and it must not see a
//! `KeyboardLeave`/`KeyboardEnter` pair, which a client may take for a new
//! focus (Doom re-requests an escaped grab on a fresh Enter). So focus does
//! not move; instead the key content is *held*:
//!
//! * a press (or repeat) made while held never reaches the client;
//! * the release of a key the client saw go down before the hold began is
//!   still delivered, so nothing stays stuck down in the client;
//! * after the hold ends, the release of a key pressed during it is dropped
//!   too (the client never saw its press), until that key is pressed again;
//! * the key-state page shows nothing held during the hold, and never a key
//!   pressed during it.
//!
//! Text produced while held is dropped with its press. Pure bookkeeping on
//! HID usages (0..=255); `inputd` asks [`KeyHold::admit`] for every key event
//! and [`KeyHold::mask`] for every page it publishes.

/// One bit per HID usage.
type Bits = [u64; 4];

fn bit(usage: u16) -> (usize, u64) {
    (usize::from(usage >> 6) & 3, 1u64 << (usage & 63))
}

fn has(bits: &Bits, usage: u16) -> bool {
    let (word, mask) = bit(usage);
    bits[word] & mask != 0
}

fn set(bits: &mut Bits, usage: u16, on: bool) {
    let (word, mask) = bit(usage);
    if on {
        bits[word] |= mask;
    } else {
        bits[word] &= !mask;
    }
}

/// What happened to a key, for [`KeyHold::admit`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Edge {
    Down,
    Repeat,
    Up,
}

/// The hold's state.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct KeyHold {
    active: bool,
    /// Keys the client saw go down before the hold: their releases are owed.
    owed: Bits,
    /// Keys pressed while held: the client never saw them go down.
    swallowed: Bits,
}

impl KeyHold {
    pub const fn new() -> KeyHold {
        KeyHold {
            active: false,
            owed: [0; 4],
            swallowed: [0; 4],
        }
    }

    /// Whether key content is held now.
    pub fn active(&self) -> bool {
        self.active
    }

    /// Start or end the hold. `down` is the keys held right now (what the
    /// client has seen pressed when the hold starts). Returns whether the
    /// state changed.
    pub fn set(&mut self, held: bool, down: Bits) -> bool {
        if self.active == held {
            return false;
        }
        self.active = held;
        if held {
            // Owed: what is down and not already a swallowed key.
            for word in 0..4 {
                self.owed[word] = down[word] & !self.swallowed[word];
            }
        } else {
            self.owed = [0; 4];
        }
        true
    }

    /// The focus moved (or the client went away): the client that gets the
    /// keys next saw none of this, so start clean.
    pub fn reset_keys(&mut self) {
        self.owed = [0; 4];
        self.swallowed = [0; 4];
    }

    /// Whether the `edge` of key `usage` reaches the focused client.
    pub fn admit(&mut self, usage: u16, edge: Edge) -> bool {
        if self.active {
            match edge {
                Edge::Down | Edge::Repeat => {
                    set(&mut self.swallowed, usage, true);
                    false
                }
                Edge::Up => {
                    let owed = has(&self.owed, usage);
                    set(&mut self.owed, usage, false);
                    set(&mut self.swallowed, usage, false);
                    owed
                }
            }
        } else {
            match edge {
                Edge::Down => {
                    set(&mut self.swallowed, usage, false);
                    true
                }
                Edge::Repeat => !has(&self.swallowed, usage),
                Edge::Up => {
                    let swallowed = has(&self.swallowed, usage);
                    set(&mut self.swallowed, usage, false);
                    !swallowed
                }
            }
        }
    }

    /// The key-state page's view of `down`: nothing while held, and never a
    /// key pressed during the hold.
    pub fn mask(&self, down: Bits) -> Bits {
        if self.active {
            return [0; 4];
        }
        let mut shown = down;
        for (word, swallowed) in shown.iter_mut().zip(self.swallowed) {
            *word &= !swallowed;
        }
        shown
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keystate::bits_of;

    const A: u16 = 0x04;
    const SHIFT: u16 = 0xE1;
    const ENTER: u16 = 0x28;

    #[test]
    fn without_a_hold_everything_passes() {
        let mut hold = KeyHold::new();
        assert!(hold.admit(A, Edge::Down));
        assert!(hold.admit(A, Edge::Repeat));
        assert!(hold.admit(A, Edge::Up));
        assert_eq!(hold.mask(bits_of(&[A])), bits_of(&[A]));
    }

    #[test]
    fn presses_and_repeats_while_held_never_reach_the_client() {
        let mut hold = KeyHold::new();
        assert!(hold.set(true, [0; 4]));
        assert!(!hold.admit(A, Edge::Down));
        assert!(!hold.admit(A, Edge::Repeat));
        assert!(!hold.admit(A, Edge::Up));
    }

    #[test]
    fn a_key_down_when_the_hold_began_is_released_in_the_client() {
        let mut hold = KeyHold::new();
        assert!(hold.admit(SHIFT, Edge::Down));
        hold.set(true, bits_of(&[SHIFT]));
        assert!(!hold.admit(SHIFT, Edge::Repeat));
        assert!(hold.admit(SHIFT, Edge::Up), "the owed release is delivered");
        // Once: a second press-release while held is the menu's.
        assert!(!hold.admit(SHIFT, Edge::Down));
        assert!(!hold.admit(SHIFT, Edge::Up));
    }

    #[test]
    fn a_key_pressed_while_held_and_released_after_is_dropped() {
        let mut hold = KeyHold::new();
        hold.set(true, [0; 4]);
        assert!(!hold.admit(ENTER, Edge::Down), "Enter picks the menu row");
        hold.set(false, bits_of(&[ENTER]));
        assert_eq!(hold.mask(bits_of(&[ENTER])), [0; 4], "the page hides it");
        assert!(!hold.admit(ENTER, Edge::Repeat));
        assert!(
            !hold.admit(ENTER, Edge::Up),
            "its release is not the client's"
        );
        // The next press is an ordinary one.
        assert!(hold.admit(ENTER, Edge::Down));
        assert!(hold.admit(ENTER, Edge::Up));
    }

    #[test]
    fn a_fresh_press_after_the_hold_clears_the_swallowed_key() {
        let mut hold = KeyHold::new();
        hold.set(true, [0; 4]);
        hold.admit(A, Edge::Down);
        hold.set(false, bits_of(&[A]));
        // Released while not held is dropped; pressed again, it is new.
        assert!(!hold.admit(A, Edge::Up));
        assert!(hold.admit(A, Edge::Down));
        assert_eq!(hold.mask(bits_of(&[A])), bits_of(&[A]));
    }

    #[test]
    fn the_page_shows_nothing_while_held() {
        let mut hold = KeyHold::new();
        hold.set(true, bits_of(&[A]));
        assert_eq!(hold.mask(bits_of(&[A, SHIFT])), [0; 4]);
    }

    #[test]
    fn setting_the_same_state_again_changes_nothing() {
        let mut hold = KeyHold::new();
        assert!(!hold.set(false, [0; 4]));
        assert!(hold.set(true, bits_of(&[A])));
        assert!(!hold.set(true, [0; 4]), "the owed set is kept");
        assert!(hold.admit(A, Edge::Up));
    }

    #[test]
    fn a_key_swallowed_before_a_second_hold_is_not_owed() {
        let mut hold = KeyHold::new();
        hold.set(true, [0; 4]);
        hold.admit(ENTER, Edge::Down);
        hold.set(false, bits_of(&[ENTER]));
        hold.set(true, bits_of(&[ENTER]));
        assert!(
            !hold.admit(ENTER, Edge::Up),
            "the client never saw it pressed"
        );
    }

    #[test]
    fn a_focus_change_starts_clean() {
        let mut hold = KeyHold::new();
        hold.set(true, [0; 4]);
        hold.admit(A, Edge::Down);
        hold.set(false, bits_of(&[A]));
        hold.reset_keys();
        assert!(hold.admit(A, Edge::Up));
    }
}
