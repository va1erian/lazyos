//! Keyboard to joypad: the keys a game needs, and turbo.
//!
//! The console sees level state, so the screen control maps key down/up edges
//! through [`Pad`] and re-applies every button each frame; turbo is a pulse
//! the pad computes itself. Keys a window shortcut owns are the caller's to
//! filter out of [`action`].

use tetanes_core::input::JoypadBtn;
use xui_core::message::{Key, Modifiers};

/// Frames a turbo button is held before it releases (about 15 presses a
/// second at 60 Hz).
pub const TURBO_FRAMES: u32 = 4;

/// What a key does while it is held.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    /// A joypad button.
    Button(JoypadBtn),
    /// The A button, pulsing while held.
    TurboA,
    /// The B button, pulsing while held.
    TurboB,
    /// Run at twice the speed while held.
    FastForward,
}

/// The action of `key` with `modifiers` held, or `None` for a key the console
/// does not use. A key with Ctrl, Alt or the Windows key is a window shortcut,
/// never a button: Ctrl+S must not also press turbo B.
pub fn action(key: Key, modifiers: Modifiers) -> Option<Action> {
    if modifiers.ctrl || modifiers.alt || modifiers.win {
        return None;
    }
    if key == Key::LEFT {
        return Some(Action::Button(JoypadBtn::Left));
    }
    if key == Key::RIGHT {
        return Some(Action::Button(JoypadBtn::Right));
    }
    if key == Key::UP {
        return Some(Action::Button(JoypadBtn::Up));
    }
    if key == Key::DOWN {
        return Some(Action::Button(JoypadBtn::Down));
    }
    if key == Key::Z {
        return Some(Action::Button(JoypadBtn::A));
    }
    if key == Key::X {
        return Some(Action::Button(JoypadBtn::B));
    }
    if key == Key::Q || key == Key::TAB {
        return Some(Action::Button(JoypadBtn::Select));
    }
    if key == Key::W || key == Key::RETURN {
        return Some(Action::Button(JoypadBtn::Start));
    }
    if key == Key::A {
        return Some(Action::TurboA);
    }
    if key == Key::S {
        return Some(Action::TurboB);
    }
    if key == Key::SPACE {
        return Some(Action::FastForward);
    }
    None
}

/// The state of the keys a game holds.
#[derive(Debug, Default)]
pub struct Pad {
    left: bool,
    right: bool,
    up: bool,
    down: bool,
    a: bool,
    b: bool,
    select: bool,
    start: bool,
    turbo_a: bool,
    turbo_b: bool,
    forward: bool,
    /// Where the turbo pulse is; advanced once a frame by [`Pad::tick`].
    phase: u32,
}

impl Pad {
    pub fn new() -> Pad {
        Pad::default()
    }

    /// A key went down or came up.
    pub fn key(&mut self, action: Action, down: bool) {
        match action {
            Action::Button(JoypadBtn::Left) => self.left = down,
            Action::Button(JoypadBtn::Right) => self.right = down,
            Action::Button(JoypadBtn::Up) => self.up = down,
            Action::Button(JoypadBtn::Down) => self.down = down,
            Action::Button(JoypadBtn::A) => self.a = down,
            Action::Button(JoypadBtn::B) => self.b = down,
            Action::Button(JoypadBtn::Select) => self.select = down,
            Action::Button(JoypadBtn::Start) => self.start = down,
            Action::TurboA => self.turbo_a = down,
            Action::TurboB => self.turbo_b = down,
            Action::FastForward => self.forward = down,
            // The console's own turbo buttons are not offered by the keyboard
            // map (the pad pulses A and B itself).
            Action::Button(JoypadBtn::TurboA) => self.turbo_a = down,
            Action::Button(JoypadBtn::TurboB) => self.turbo_b = down,
        }
    }

    /// Nothing is held (the window lost focus, or the game was paused).
    pub fn release_all(&mut self) {
        *self = Pad {
            phase: self.phase,
            ..Pad::default()
        };
    }

    /// Advances the turbo pulse one frame.
    pub fn tick(&mut self) {
        self.phase = (self.phase + 1) % TURBO_FRAMES;
    }

    /// Whether the fast-forward key is held.
    pub fn fast_forward(&self) -> bool {
        self.forward
    }

    /// The pulse is on for the first half of every [`TURBO_FRAMES`].
    fn pulse(&self) -> bool {
        self.phase < TURBO_FRAMES / 2
    }

    /// Every button's state for this frame, in the order the joypad is set.
    pub fn buttons(&self) -> [(JoypadBtn, bool); 8] {
        let pulse = self.pulse();
        [
            (JoypadBtn::Left, self.left),
            (JoypadBtn::Right, self.right),
            (JoypadBtn::Up, self.up),
            (JoypadBtn::Down, self.down),
            (JoypadBtn::A, self.a || (self.turbo_a && pulse)),
            (JoypadBtn::B, self.b || (self.turbo_b && pulse)),
            (JoypadBtn::Select, self.select),
            (JoypadBtn::Start, self.start),
        ]
    }

    /// Whether any button is held (the welcome screen uses it to skip help).
    pub fn any_held(&self) -> bool {
        self.buttons().iter().any(|(_, held)| *held)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_keys_map_to_the_console() {
        assert_eq!(
            action(Key::LEFT, Modifiers::NONE),
            Some(Action::Button(JoypadBtn::Left))
        );
        assert_eq!(
            action(Key::Z, Modifiers::NONE),
            Some(Action::Button(JoypadBtn::A))
        );
        assert_eq!(
            action(Key::RETURN, Modifiers::NONE),
            Some(Action::Button(JoypadBtn::Start))
        );
        assert_eq!(action(Key::SPACE, Modifiers::NONE), Some(Action::FastForward));
        assert_eq!(action(Key::F1, Modifiers::NONE), None);
        assert_eq!(action(Key::O, Modifiers::NONE), None);
    }

    #[test]
    fn a_shortcut_never_presses_a_button() {
        let ctrl = Modifiers {
            ctrl: true,
            ..Modifiers::NONE
        };
        assert_eq!(action(Key::S, ctrl), None);
        assert_eq!(action(Key::A, ctrl), None);
        assert_eq!(action(Key::SPACE, ctrl), None);
        let alt = Modifiers {
            alt: true,
            ..Modifiers::NONE
        };
        assert_eq!(action(Key::LEFT, alt), None);
    }

    #[test]
    fn turbo_pulses_and_release_clears() {
        let mut pad = Pad::new();
        pad.key(Action::TurboA, true);
        let mut on = 0;
        for _ in 0..TURBO_FRAMES * 2 {
            pad.tick();
            on += usize::from(pad.buttons()[4].1);
        }
        assert_eq!(on, TURBO_FRAMES as usize, "one pulse per cycle");
        pad.key(Action::TurboA, false);
        pad.tick();
        assert!(!pad.any_held());
        pad.key(Action::Button(JoypadBtn::Start), true);
        assert!(pad.any_held());
        pad.release_all();
        assert!(!pad.any_held());
    }

    #[test]
    fn fast_forward_is_tracked() {
        let mut pad = Pad::new();
        assert!(!pad.fast_forward());
        pad.key(Action::FastForward, true);
        assert!(pad.fast_forward());
        pad.key(Action::FastForward, false);
        assert!(!pad.fast_forward());
    }
}
