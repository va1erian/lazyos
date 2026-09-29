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
    /// `Tab` is the canonical cycle key; `PageDown`/`PageUp` are accepted too
    /// because a compositor reserves `Tab` for surface focus and the kernel's
    /// PS/2 driver does not decode function keys (issue #168).
    fn key_down(&self, window: WindowId, code: u32) {
        let modifier = self.update_modifiers(code, true);
        if !modifier {
            match code {
                key::TAB | key::PAGE_DOWN => {
                    self.cycle_focus(window, true);
                    return;
                }
                key::PAGE_UP => {
                    self.cycle_focus(window, false);
                    return;
                }
                _ => {}
            }
        }
        let target = self.focused.get().unwrap_or(WidgetId::NONE);
        if let Some(event) = key_event(code, true, self.modifiers.get()) {
            self.deliver(window, target, &event);
        }
        if !modifier {
            if let Some(character) = key_char(code) {
                self.deliver(window, target, &Event::Char(character));
            }
        }
    }

    /// Route one key release to the focused widget.
    fn key_up(&self, window: WindowId, code: u32) {
        self.update_modifiers(code, false);
        let target = self.focused.get().unwrap_or(WidgetId::NONE);
        if let Some(event) = key_event(code, false, self.modifiers.get()) {
            self.deliver(window, target, &event);
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
        key::SHIFT => Key::SHIFT,
        key::CTRL => Key::CONTROL,
        key::ALT => Key::MENU,
        // No named `WIN`; the VK_LWIN code, so an app can name the key.
        key::SUPER => Key::from_code(0x5B),
        key::F4 => Key::F4,
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
        // ASCII plus Latin-1 (the accented letters of non-US layouts).
        other if (0x20..=0x7e).contains(&other) || (0xa0..=0xff).contains(&other) => {
            char::from_u32(other)
        }
        _ => None,
    }
}
