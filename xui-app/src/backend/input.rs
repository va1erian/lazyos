//! Input routing: kernel input records (owner mode) and compositor events
//! (client mode) into `xui` widget events.

use std::sync::atomic::Ordering;

use xui_core::backend::{Event, WidgetId, WindowId};
use xui_core::{Key, Modifiers, MouseButton};

use crate::display::{self, EventKind};
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
    /// `Tab` is the canonical cycle key; `PageDown`/`PageUp` are accepted too
    /// because a compositor reserves `Tab` for surface focus and the kernel's
    /// PS/2 driver does not decode function keys (issue #168).
    fn key_down(&self, window: WindowId, code: u32) {
        match code {
            key::TAB | key::PAGE_DOWN => {
                self.cycle_focus(window, true);
            }
            key::PAGE_UP => {
                self.cycle_focus(window, false);
            }
            _ => {
                let target = self.focused.get().unwrap_or(WidgetId::NONE);
                if let Some(event) = key_event(code as i32, true) {
                    self.deliver(window, target, &event);
                }
                if let Some(character) = key_char(code) {
                    self.deliver(window, target, &Event::Char(character));
                }
            }
        }
    }

    /// Route one key release to the focused widget.
    fn key_up(&self, window: WindowId, code: u32) {
        let target = self.focused.get().unwrap_or(WidgetId::NONE);
        if let Some(event) = key_event(code as i32, false) {
            self.deliver(window, target, &event);
        }
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
                        self.pointer_down(window, x, y, mouse_button(raw.a));
                    }
                    event::POINTER_UP => {
                        let (x, y) = self.pointer.get();
                        self.pointer_up(window, x, y, mouse_button(raw.a));
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
                    if parcel.header.method == display::method::WINDOW_CLOSE {
                        self.quit.store(true, Ordering::Relaxed);
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

    /// Route one decoded compositor event.
    ///
    /// `xuid` reports presses relative to the surface but moves in screen
    /// coordinates, so the surface origin is recovered from each press and
    /// applied to the moves that follow. Press events do not carry the button
    /// id (a protocol gap, noted in the PR), so they read as the left button.
    fn route_client_event(&self, window: WindowId, event: display::Event) {
        match event.kind {
            EventKind::PointerMove => {
                self.last_abs.set(Some((event.a as i32, event.b as i32)));
                if let Some((ox, oy)) = self.origin.get() {
                    self.pointer_move(window, event.a as i32 - ox, event.b as i32 - oy);
                }
            }
            EventKind::PointerDown => {
                let (x, y) = (event.a as i32, event.b as i32);
                if let Some((abs_x, abs_y)) = self.last_abs.get() {
                    self.origin.set(Some((abs_x - x, abs_y - y)));
                }
                self.pointer_down(window, x, y, MouseButton::Left);
            }
            EventKind::PointerUp => {
                self.pointer_up(window, event.a as i32, event.b as i32, MouseButton::Left);
            }
            EventKind::KeyDown => self.key_down(window, event.a as u32),
            EventKind::KeyUp => self.key_up(window, event.a as u32),
        }
    }
}

fn mouse_button(code: i32) -> MouseButton {
    match code as u32 {
        button::RIGHT => MouseButton::Right,
        button::MIDDLE => MouseButton::Middle,
        _ => MouseButton::Left,
    }
}

/// One key record into the `KeyDown`/`KeyUp` vocabulary. A printable key also
/// produces a separate [`Event::Char`] (see [`key_char`]).
fn key_event(code: i32, down: bool) -> Option<Event> {
    if !down {
        return Some(Event::KeyUp {
            key: key_of(code as u32),
            modifiers: Modifiers::NONE,
            system: false,
        });
    }
    Some(Event::KeyDown {
        key: key_of(code as u32),
        modifiers: Modifiers::NONE,
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
        // The kernel reports letters lowercase; the virtual-key codes are
        // uppercase, matching the Windows ABI `xui` mirrors.
        other if (b'a' as u32..=b'z' as u32).contains(&other) => {
            Key::from_code((other as u8).to_ascii_uppercase() as u16)
        }
        other => Key::from_code(other as u16),
    }
}

/// The character a printable key carries; `None` for a non-printing key.
fn key_char(code: u32) -> Option<char> {
    match code {
        key::ENTER => Some('\n'),
        key::TAB => Some('\t'),
        key::BACKSPACE => Some('\u{8}'),
        other if (0x20..=0x7e).contains(&other) => char::from_u32(other),
        _ => None,
    }
}
