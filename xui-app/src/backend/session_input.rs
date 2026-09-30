//! Keys from an `inputd` session (`docs/input-plan.md`): the client-mode
//! backend's second input source, next to the compositor's event endpoint.
//!
//! `inputd` sends the physical key (`KeyEvent`) and the composed text
//! (`TextInput`) separately. The backend maps them onto the `xui` vocabulary
//! the widgets already understand: a `KeyEvent` becomes `KeyDown`/`KeyUp`
//! (plus the `\n`/`\b` control characters editors expect from Enter and
//! Backspace, exactly as the compositor's legacy keys did), and `TextInput`
//! becomes `Char`. Modifier and lock keys are not forwarded as keys; they
//! arrive as bits on every event, as before.

use xui_core::backend::{Event, WidgetId, WindowId};
use xui_core::{Key, Modifiers};

use crate::display;
use crate::input::{self, keysym, mods, Event as InputEvent, KeyState};
use crate::sys::{self, errno};

use super::{LazyOSBackend, CLIENT_INPUT_BYTES};

impl LazyOSBackend {
    /// Drain the session's event endpoint. Only queued events are read (the
    /// count comes from the channel counters), so an idle session costs one
    /// cheap syscall per loop pass instead of a parked receive.
    pub(super) fn pump_session_input(&self, window: WindowId, events: u64) {
        let mut buf = [0u8; CLIENT_INPUT_BYTES];
        while matches!(sys::msg_queued(events), Ok(queued) if queued > 0) {
            match sys::msg_recv(events, &mut buf, sys::EXPIRED_DEADLINE) {
                Ok(result) => {
                    let len = result.bytes as usize;
                    let Some(parcel) = display::decode_message(&buf[..len]) else {
                        continue;
                    };
                    if let Some(event) = input::decode_event(&parcel) {
                        self.route_session_event(window, event);
                    }
                }
                // `inputd` went away or nothing was actually there: the
                // window keeps working on the compositor's legacy keys.
                Err(code) if code == -errno::ETIMEDOUT || code == -errno::EPIPE => break,
                Err(_) => break,
            }
        }
    }

    fn route_session_event(&self, window: WindowId, event: InputEvent) {
        match event {
            InputEvent::Key {
                code,
                sym,
                mods,
                state,
            } => self.session_key(window, code, sym, modifiers_of(mods), state),
            InputEvent::Text(text) => {
                let target = self.focused.get().unwrap_or(WidgetId::NONE);
                for character in text.chars() {
                    self.deliver(window, target, &Event::Char(character));
                }
            }
            // Focus left: nothing may stay pressed, or a key held while the
            // window lost the keyboard would repeat its release forever.
            InputEvent::Leave => self.release_held_keys(window),
            InputEvent::Enter(_) | InputEvent::Layout(_) => {}
        }
    }

    fn session_key(
        &self,
        window: WindowId,
        code: u32,
        sym: u32,
        modifiers: Modifiers,
        state: KeyState,
    ) {
        // Tab moves widget focus rather than reaching a widget.
        // Alt/Ctrl+Tab are the compositor's chords; `inputd` normally consumes
        // them, and if one slips through it must not also move widget focus.
        if sym == keysym::TAB && (modifiers.alt || modifiers.ctrl) {
            return;
        }
        if sym == keysym::TAB && state != KeyState::Up {
            self.cycle_focus(window, !modifiers.shift);
            return;
        }
        let Some(key) = key_of(code, sym) else {
            return;
        };
        let target = self.focused.get().unwrap_or(WidgetId::NONE);
        match state {
            KeyState::Up => {
                self.held_keys.borrow_mut().retain(|held| *held != key);
                self.deliver(
                    window,
                    target,
                    &Event::KeyUp {
                        key,
                        modifiers,
                        system: false,
                    },
                );
            }
            KeyState::Down | KeyState::Repeat => {
                let mut held = self.held_keys.borrow_mut();
                if !held.contains(&key) {
                    held.push(key);
                }
                drop(held);
                self.deliver(
                    window,
                    target,
                    &Event::KeyDown {
                        key,
                        modifiers,
                        repeat: 1,
                        system: false,
                    },
                );
                // A command chord is not text: Ctrl+C must not insert `c`.
                if !modifiers.ctrl && !modifiers.alt {
                    if let Some(character) = control_char(sym) {
                        self.deliver(window, target, &Event::Char(character));
                    }
                }
            }
        }
    }

    /// Release every key the session reported down.
    fn release_held_keys(&self, window: WindowId) {
        let held: Vec<Key> = self.held_keys.borrow_mut().drain(..).collect();
        let target = self.focused.get().unwrap_or(WidgetId::NONE);
        for key in held {
            self.deliver(
                window,
                target,
                &Event::KeyUp {
                    key,
                    modifiers: Modifiers::NONE,
                    system: false,
                },
            );
        }
    }
}

/// The `xui` modifiers of a `KeyEvent.mods` word (AltGr is a character-layer
/// selector, not an Alt).
fn modifiers_of(bits: u32) -> Modifiers {
    Modifiers {
        shift: bits & mods::SHIFT != 0,
        ctrl: bits & mods::CTRL != 0,
        alt: bits & mods::ALT != 0,
        win: bits & mods::SUPER != 0,
    }
}

/// Map a key onto the `xui` virtual-key vocabulary; `None` for keys the
/// widgets never see (modifiers, locks, the keypad digit block, media keys).
///
/// Named keys, function keys and letters go by the keysym (an AZERTY `a` is
/// `A`, so shortcuts follow the key's label). Every other character key goes by
/// its *position* (the HID usage): the digit row is `1`..`0` and punctuation
/// its US OEM code, whatever the layout types there. Going by the character
/// instead (as the compositor's legacy keys did) turns `&`, `"`, `'`, `(`, `%`,
/// `$` and `#` into the Windows codes for Up, PageDown, Right, Down, Left, Home
/// and End: typing them would move the caret.
fn key_of(code: u32, sym: u32) -> Option<Key> {
    if let Some(key) = key_of_sym(sym) {
        return Some(key);
    }
    // A character key the keysym table left out: its position decides.
    let printable = (0x21..=0x7E).contains(&sym) || (0xA0..=0xFF).contains(&sym);
    printable.then(|| key_of_position(code)).flatten()
}

/// The virtual key of the physical position `code` (HID usage) for keys that
/// type characters: digit row, punctuation and the ISO extra key.
fn key_of_position(code: u32) -> Option<Key> {
    let vk: u16 = match code {
        // 1..9, 0 in the top row.
        0x1E..=0x26 => 0x31 + (code - 0x1E) as u16,
        0x27 => 0x30,
        0x2D => 0xBD, // - _
        0x2E => 0xBB, // = +
        0x2F => 0xDB, // [ {
        0x30 => 0xDD, // ] }
        0x31 => 0xDC, // \ |
        0x33 => 0xBA, // ; :
        0x34 => 0xDE, // ' "
        0x35 => 0xC0, // ` ~
        0x36 => 0xBC, // ,
        0x37 => 0xBE, // .
        0x38 => 0xBF, // / ?
        0x64 => 0xE2, // the ISO key between Shift and Z
        _ => return None,
    };
    Some(Key::from_code(vk))
}

/// The keysym part of [`key_of`]: named keys, function keys, space and
/// letters.
fn key_of_sym(sym: u32) -> Option<Key> {
    Some(match sym {
        keysym::ENTER | keysym::KP_ENTER => Key::RETURN,
        keysym::BACKSPACE => Key::BACK,
        keysym::TAB => Key::TAB,
        keysym::ESCAPE => Key::ESCAPE,
        keysym::LEFT => Key::LEFT,
        keysym::RIGHT => Key::RIGHT,
        keysym::UP => Key::UP,
        keysym::DOWN => Key::DOWN,
        keysym::PAGE_UP => Key::PAGE_UP,
        keysym::PAGE_DOWN => Key::PAGE_DOWN,
        keysym::HOME => Key::HOME,
        keysym::END => Key::END,
        keysym::DELETE => Key::DELETE,
        keysym::INSERT => Key::INSERT,
        // `F1..F12` map to the Windows `VK_F1..VK_F12` (`0x70 + n - 1`).
        keysym::F1..=keysym::F12 => Key::from_code(0x70 + (sym - keysym::F1) as u16),
        0x20 => Key::SPACE,
        // The virtual-key codes for letters are uppercase (the Windows ABI
        // `xui` mirrors); other printable characters keep their code.
        0x61..=0x7A => Key::from_code((sym as u8).to_ascii_uppercase() as u16),
        0x41..=0x5A => Key::from_code(sym as u16),
        _ => return None,
    })
}

/// The control character an editing key types (widgets insert `\n` for Enter
/// and delete on `\b`); every printable character arrives as `TextInput`
/// instead. Tab never gets here: it moves focus.
fn control_char(sym: u32) -> Option<char> {
    match sym {
        keysym::ENTER | keysym::KP_ENTER => Some('\n'),
        keysym::BACKSPACE => Some('\u{8}'),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn editing_and_navigation_keys_map_to_named_keys() {
        assert_eq!(key_of_sym(keysym::ENTER), Some(Key::RETURN));
        assert_eq!(key_of_sym(keysym::KP_ENTER), Some(Key::RETURN));
        assert_eq!(key_of_sym(keysym::BACKSPACE), Some(Key::BACK));
        assert_eq!(key_of_sym(keysym::ESCAPE), Some(Key::ESCAPE));
        assert_eq!(key_of_sym(keysym::LEFT), Some(Key::LEFT));
        assert_eq!(key_of_sym(keysym::PAGE_DOWN), Some(Key::PAGE_DOWN));
        assert_eq!(key_of_sym(keysym::DELETE), Some(Key::DELETE));
        assert_eq!(key_of_sym(keysym::INSERT), Some(Key::INSERT));
        assert_eq!(key_of_sym(0x20), Some(Key::SPACE));
    }

    #[test]
    fn function_keys_map_to_the_windows_codes() {
        for n in 0..12u32 {
            assert_eq!(
                key_of_sym(keysym::F1 + n),
                Some(Key::from_code(0x70 + n as u16))
            );
        }
        assert_eq!(key_of_sym(keysym::F1), Some(Key::F1));
        assert_eq!(key_of_sym(keysym::F1 + 3), Some(Key::F4));
    }

    #[test]
    fn letters_are_virtual_key_uppercase_whatever_their_case() {
        assert_eq!(key_of_sym('s' as u32), Some(Key::S));
        assert_eq!(key_of_sym('S' as u32), Some(Key::from_code(b'S' as u16)));
        // An AZERTY `a` (physical Q) is the `A` key: shortcuts follow the label.
        assert_eq!(key_of(0x14, 'a' as u32), Some(Key::from_code(b'A' as u16)));
    }

    #[test]
    fn character_keys_go_by_position_not_by_character() {
        let vk = |code, sym: char| key_of(code, sym as u32).map(|key| key.code());
        // The AZERTY digit row types `& é " ' ( - è _ ç à` but is the 1..0 row.
        for (position, ch, digit) in [
            (0x1E, '&', 0x31),
            (0x1F, 'é', 0x32),
            (0x20, '"', 0x33),
            (0x21, '\'', 0x34),
            (0x22, '(', 0x35),
            (0x27, 'à', 0x30),
        ] {
            assert_eq!(vk(position, ch), Some(digit), "{ch:?}");
        }
        // US Shift+digits are the digit keys too (not PageUp, Up, Left...).
        assert_eq!(vk(0x1E, '!'), Some(0x31));
        assert_eq!(vk(0x22, '%'), Some(0x35));
        assert_eq!(vk(0x24, '&'), Some(0x37));
        // Punctuation keeps the US OEM code of its position.
        assert_eq!(vk(0x2D, '-'), Some(0xBD));
        assert_eq!(vk(0x38, '/'), Some(0xBF));
        assert_eq!(vk(0x38, '!'), Some(0xBF), "AZERTY `!` sits on the / key");
        assert_eq!(vk(0x33, ';'), Some(0xBA));
        assert_eq!(vk(0x64, '<'), Some(0xE2));
    }

    /// The bug the position mapping fixes: no character key may ever produce
    /// a Windows navigation code (PageUp..Down, End, Home, arrows, Insert,
    /// Delete), which would move the caret while typing.
    #[test]
    fn no_character_key_types_a_navigation_code() {
        let navigation = [0x21, 0x22, 0x23, 0x24, 0x25, 0x26, 0x27, 0x28, 0x2D, 0x2E];
        let positions = (0x1E..=0x38).chain([0x64]);
        for position in positions {
            for sym in (0x21..=0x7Eu32).chain(0xA0..=0xFF) {
                if let Some(key) = key_of(position, sym) {
                    assert!(
                        !navigation.contains(&key.code()),
                        "position {position:#x} sym {sym:#x} gave code {:#x}",
                        key.code()
                    );
                }
            }
        }
    }

    #[test]
    fn modifier_lock_and_keypad_keys_are_not_forwarded() {
        for sym in [
            0xFFE1, // Shift_L
            0xFFE3, // Control_L
            0xFFE9, // Alt_L
            0xFFEB, // Super_L
            0xFE03, // AltGr
            0xFFE5, // Caps_Lock
            0xFF7F, // Num_Lock
            0xFFB1, // KP_1
            0xFF67, // Menu
            0,
        ] {
            assert_eq!(key_of_sym(sym), None, "sym {sym:#x}");
        }
    }

    #[test]
    fn only_editing_keys_type_control_characters() {
        assert_eq!(control_char(keysym::ENTER), Some('\n'));
        assert_eq!(control_char(keysym::BACKSPACE), Some('\u{8}'));
        assert_eq!(control_char(keysym::TAB), None);
        assert_eq!(control_char('a' as u32), None);
        assert_eq!(control_char(keysym::DELETE), None);
    }

    #[test]
    fn modifier_bits_map_and_altgr_is_not_alt() {
        assert_eq!(modifiers_of(0), Modifiers::NONE);
        assert_eq!(
            modifiers_of(mods::SHIFT | mods::CTRL | mods::ALT | mods::SUPER),
            Modifiers {
                shift: true,
                ctrl: true,
                alt: true,
                win: true
            }
        );
        // AltGr (0x10), Caps (0x20) and Num (0x40) lock bits carry no modifier.
        assert_eq!(modifiers_of(0x10 | 0x20 | 0x40), Modifiers::NONE);
    }
}
