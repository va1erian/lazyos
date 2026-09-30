//! Input routing: kernel input records (owner mode) and compositor events
//! (client mode) into `xui` widget events.

use std::sync::atomic::Ordering;

use xui_core::backend::{Event, WidgetId, WindowId};
use xui_core::{Key, Modifiers, MouseButton};

use crate::display::{self, Event as DisplayEvent};
use crate::sys::{self, button, errno, event, key, EVENT_BYTES};

use super::{LazyOSBackend, CLIENT_INPUT_BYTES, CLIENT_POLL_TICKS, INPUT_BATCH};

impl LazyOSBackend {
    /// Route one pointer move (window-relative coordinates).
    fn pointer_move(&self, window: WindowId, x: i32, y: i32) {
        self.pointer.set((x, y));
        let target = self.hit(window, x, y).unwrap_or(WidgetId::NONE);
        self.deliver(
            window,
            target,
            &Event::MouseMove {
                x,
                y,
                modifiers: Modifiers::NONE,
            },
        );
    }

    /// Route one pointer press: click-to-focus, then the press itself.
    fn pointer_down(&self, window: WindowId, x: i32, y: i32, button: MouseButton) {
        self.pointer.set((x, y));
        let target = self.hit(window, x, y);
        if let Some(id) = target {
            if self.is_focus_stop(id) {
                self.set_focus(id);
            }
        }
        self.deliver(
            window,
            target.unwrap_or(WidgetId::NONE),
            &Event::MouseDown {
                x,
                y,
                button,
                modifiers: Modifiers::NONE,
            },
        );
    }

    /// Route one pointer release.
    fn pointer_up(&self, window: WindowId, x: i32, y: i32, button: MouseButton) {
        self.pointer.set((x, y));
        let target = self.hit(window, x, y).unwrap_or(WidgetId::NONE);
        self.deliver(
            window,
            target,
            &Event::MouseUp {
                x,
                y,
                button,
                modifiers: Modifiers::NONE,
            },
        );
    }

    /// Route one key press: focus navigation first, then the focused widget.
    ///
    /// `Tab` and `Shift+Tab` move the widget focus. `PageUp`/`PageDown` are
    /// *not* focus keys: the compositor no longer reserves them, so they reach
    /// the focused widget (the Editor scrolls with them).
    fn key_down(&self, window: WindowId, raw: u32) {
        let (code, modifiers) = self.key_state(raw, true);
        if code == key::TAB {
            self.cycle_focus(window, !modifiers.shift);
            return;
        }
        let target = self.focused.get().unwrap_or(WidgetId::NONE);
        if let Some(event) = key_event(code, true, modifiers) {
            self.deliver(window, target, &event);
        }
        if let Some(character) = typed_char(code, modifiers) {
            self.deliver(window, target, &Event::Char(character));
        }
    }

    /// Route one key release to the focused widget.
    fn key_up(&self, window: WindowId, raw: u32) {
        let (code, modifiers) = self.key_state(raw, false);
        let target = self.focused.get().unwrap_or(WidgetId::NONE);
        if let Some(event) = key_event(code, false, modifiers) {
            self.deliver(window, target, &event);
        }
    }

    /// The code and modifier state of one key record.
    ///
    /// A compositor client receives the modifiers packed into the high bits of
    /// the key (the compositor never forwards modifier keys themselves). The
    /// kernel's owner-mode records carry no bits, so the backend tracks the
    /// modifier key presses it sees and reports the accumulated state.
    fn key_state(&self, raw: u32, down: bool) -> (u32, Modifiers) {
        if self.is_client() {
            (raw & key::CODE_MASK, modifiers_from_key(raw))
        } else {
            self.update_modifiers(raw, down);
            (raw, self.modifiers.get())
        }
    }

    /// Track a `Shift`/`Ctrl`/`Alt`/`Super` press or release; return whether
    /// `code` is a modifier key.
    ///
    /// The kernel forwards one code per modifier regardless of side, so a single
    /// flag per family is enough here (the kernel already merges both sides).
    fn update_modifiers(&self, code: u32, down: bool) -> bool {
        let mut modifiers = self.modifiers.get();
        match code {
            key::SHIFT => modifiers.shift = down,
            key::CTRL => modifiers.ctrl = down,
            key::ALT => modifiers.alt = down,
            key::SUPER => modifiers.win = down,
            _ => return false,
        }
        self.modifiers.set(modifiers);
        true
    }

    /// Drain the kernel input queue (owner mode), translate and route records.
    pub(super) fn pump_input(&self, window: WindowId) {
        let mut bytes = [0u8; EVENT_BYTES * INPUT_BATCH];
        while let Ok(count) = sys::display_input_poll(&mut bytes) {
            if count == 0 {
                break;
            }
            for index in 0..count {
                let Some(raw) = sys::decode_event(&bytes, index) else {
                    continue;
                };
                match raw.kind {
                    event::POINTER_MOVE => self.pointer_move(window, raw.a, raw.b),
                    event::POINTER_DOWN => {
                        let (x, y) = self.pointer.get();
                        self.pointer_down(window, x, y, mouse_button(raw.a as u32));
                    }
                    event::POINTER_UP => {
                        let (x, y) = self.pointer.get();
                        self.pointer_up(window, x, y, mouse_button(raw.a as u32));
                    }
                    event::KEY_DOWN => self.key_down(window, raw.a as u32),
                    event::KEY_UP => self.key_up(window, raw.a as u32),
                    _ => {}
                }
            }
        }
    }

    /// Drain the event endpoint (client mode): compositor messages carry
    /// pointer, key and window-close events.
    pub(super) fn pump_client_input(&self, window: WindowId, events: u64) {
        let mut buf = [0u8; CLIENT_INPUT_BYTES];
        loop {
            let deadline = sys::clock_ticks().saturating_add(CLIENT_POLL_TICKS);
            match sys::msg_recv(events, &mut buf, deadline) {
                Ok(result) => {
                    let len = result.bytes as usize;
                    let Some(parcel) = display::decode_message(&buf[..len]) else {
                        continue;
                    };
                    if parcel.header.method == display::METHOD_WINDOW_CLOSE {
                        // Close only *this* window: the app (xui-core's
                        // runtime) decides whether that ends the loop, so the
                        // Files explorer's extra folder windows close one at a
                        // time rather than taking the whole process down.
                        self.deliver(window, WidgetId::NONE, &Event::Close);
                        continue;
                    }
                    if let Some(event) = display::decode_event(&parcel) {
                        self.route_client_event(window, event);
                    }
                }
                Err(code) if code == -errno::ETIMEDOUT => break,
                Err(code) if code == -errno::EPIPE => {
                    // The compositor died; there is nothing to draw into.
                    self.quit.store(true, Ordering::Relaxed);
                    break;
                }
                Err(_) => break,
            }
        }
    }

    /// Route one decoded compositor event. Pointer coordinates are already
    /// surface-relative and presses carry their button id.
    fn route_client_event(&self, window: WindowId, event: DisplayEvent) {
        match event {
            DisplayEvent::PointerMove { x, y } => self.pointer_move(window, x, y),
            DisplayEvent::PointerDown { x, y, button } => {
                self.pointer_down(window, x, y, mouse_button(button));
            }
            DisplayEvent::PointerUp { x, y, button } => {
                self.pointer_up(window, x, y, mouse_button(button));
            }
            DisplayEvent::KeyDown { key } => self.key_down(window, key),
            DisplayEvent::KeyUp { key } => self.key_up(window, key),
        }
    }
}

fn mouse_button(code: u32) -> MouseButton {
    match code {
        button::RIGHT => MouseButton::Right,
        button::MIDDLE => MouseButton::Middle,
        _ => MouseButton::Left,
    }
}

/// One key record into the `KeyDown`/`KeyUp` vocabulary. A printable key also
/// produces a separate [`Event::Char`] (see [`key_char`]).
fn key_event(code: u32, down: bool, modifiers: Modifiers) -> Option<Event> {
    if !down {
        return Some(Event::KeyUp {
            key: key_of(code),
            modifiers,
            system: false,
        });
    }
    Some(Event::KeyDown {
        key: key_of(code),
        modifiers,
        repeat: 1,
        system: false,
    })
}

/// Map a kernel key code onto the `xui` virtual-key vocabulary.
fn key_of(code: u32) -> Key {
    match code {
        key::ENTER => Key::RETURN,
        key::BACKSPACE => Key::BACK,
        key::TAB => Key::TAB,
        key::ESCAPE => Key::ESCAPE,
        key::SPACE => Key::SPACE,
        key::LEFT => Key::LEFT,
        key::RIGHT => Key::RIGHT,
        key::UP => Key::UP,
        key::DOWN => Key::DOWN,
        key::PAGE_UP => Key::PAGE_UP,
        key::PAGE_DOWN => Key::PAGE_DOWN,
        key::HOME => Key::HOME,
        key::END => Key::END,
        key::DELETE => Key::DELETE,
        key::INSERT => Key::INSERT,
        key::SHIFT => Key::SHIFT,
        key::CTRL => Key::CONTROL,
        key::ALT => Key::MENU,
        // No named `WIN`; the VK_LWIN code, so an app can name the key.
        key::SUPER => Key::from_code(0x5B),
        // F1..F12 are contiguous in the kernel (`0x110 + n - 1`) but map to the
        // Windows `VK_F1..VK_F12` (`0x70 + n - 1`) the `Key` vocabulary uses.
        other if (key::F1..=key::F12).contains(&other) => {
            Key::from_code(0x70 + (other - key::F1) as u16)
        }
        // The kernel reports letters lowercase; the virtual-key codes are
        // uppercase, matching the Windows ABI `xui` mirrors.
        other if (b'a' as u32..=b'z' as u32).contains(&other) => {
            Key::from_code((other as u8).to_ascii_uppercase() as u16)
        }
        other => Key::from_code(other as u16),
    }
}

/// The modifier state packed into a compositor-forwarded key.
fn modifiers_from_key(key: u32) -> Modifiers {
    Modifiers {
        shift: key & key::MOD_SHIFT != 0,
        ctrl: key & key::MOD_CTRL != 0,
        alt: key & key::MOD_ALT != 0,
        win: key & key::MOD_SUPER != 0,
    }
}

/// The character a key record types, or `None` when it is not text input.
///
/// A Ctrl or Alt chord is a command, not text: it still reaches the app as a
/// `KeyDown` (the accelerator reads the key and the modifier), but no `Char`
/// follows, so Ctrl+C does not insert `c`.
fn typed_char(code: u32, modifiers: Modifiers) -> Option<char> {
    if modifiers.ctrl || modifiers.alt {
        return None;
    }
    key_char(code)
}

/// The character a printable key carries; `None` for a non-printing key.
fn key_char(code: u32) -> Option<char> {
    match code {
        key::ENTER => Some('\n'),
        key::TAB => Some('\t'),
        key::BACKSPACE => Some('\u{8}'),
        // ASCII plus Latin-1 (the accented letters of non-US layouts).
        other if (0x20..=0x7e).contains(&other) || (0xa0..=0xff).contains(&other) => {
            char::from_u32(other)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A key code plus the modifier bits it was forwarded with.
    fn encoded(code: u32, shift: bool, ctrl: bool, alt: bool, sup: bool) -> u32 {
        let mut raw = code;
        for (bit, held) in [
            (key::MOD_SHIFT, shift),
            (key::MOD_CTRL, ctrl),
            (key::MOD_ALT, alt),
            (key::MOD_SUPER, sup),
        ] {
            if held {
                raw |= bit;
            }
        }
        raw
    }

    #[test]
    fn the_code_is_masked_before_matching() {
        let raw = encoded(b's' as u32, false, true, false, false);
        let (code, modifiers) = (raw & key::CODE_MASK, modifiers_from_key(raw));
        assert_eq!(code, b's' as u32);
        assert!(modifiers.ctrl && !modifiers.shift && !modifiers.alt && !modifiers.win);
        // Without the mask the raw value would not name the key.
        assert_eq!(key_of(code), Key::S);
    }

    #[test]
    fn every_modifier_bit_is_decoded() {
        let raw = encoded(key::LEFT, true, true, true, true);
        assert_eq!(
            modifiers_from_key(raw),
            Modifiers {
                shift: true,
                ctrl: true,
                alt: true,
                win: true,
            }
        );
    }

    #[test]
    fn ctrl_or_alt_suppresses_the_typed_character() {
        assert_eq!(typed_char(b'c' as u32, Modifiers::NONE), Some('c'));
        assert_eq!(
            typed_char(
                b'c' as u32,
                Modifiers {
                    ctrl: true,
                    ..Modifiers::NONE
                }
            ),
            None
        );
        assert_eq!(
            typed_char(
                b'c' as u32,
                Modifiers {
                    alt: true,
                    ..Modifiers::NONE
                }
            ),
            None
        );
        // Shift alone keeps typing; the kernel already applied the shift.
        assert_eq!(
            typed_char(
                b'C' as u32,
                Modifiers {
                    shift: true,
                    ..Modifiers::NONE
                }
            ),
            Some('C')
        );
    }

    #[test]
    fn unknown_and_non_printing_codes_are_ignored() {
        // Beyond the defined ranges and the modifier keys: no character.
        assert_eq!(typed_char(0x200, Modifiers::NONE), None);
        assert_eq!(typed_char(key::SHIFT, Modifiers::NONE), None);
        assert_eq!(typed_char(key::DELETE, Modifiers::NONE), None);
        assert_eq!(typed_char(key::F1, Modifiers::NONE), None);
    }

    #[test]
    fn delete_insert_and_function_keys_map_to_their_vk_codes() {
        assert_eq!(key_of(key::DELETE), Key::DELETE);
        assert_eq!(key_of(key::INSERT), Key::INSERT);
        for n in 1..=12u32 {
            assert_eq!(key_of(key::F1 + n - 1), Key::from_code(0x70 + n as u16 - 1));
        }
        assert_eq!(key_of(key::F1), Key::F1);
        assert_eq!(key_of(0x113), Key::F4);
    }

    #[test]
    fn navigation_keys_keep_their_named_codes() {
        assert_eq!(key_of(key::LEFT), Key::LEFT);
        assert_eq!(key_of(key::HOME), Key::HOME);
        assert_eq!(key_of(key::PAGE_UP), Key::PAGE_UP);
        assert_eq!(key_of(key::PAGE_DOWN), Key::PAGE_DOWN);
        assert_eq!(key_of(key::END), Key::END);
    }
}
