//! Kernel-side input queueing for the bound compositor: the keyboard and
//! mouse IRQ handlers push events here, the compositor drains them with
//! `input_poll`, and a key rings its doorbell (P1.4).

use core::sync::atomic::Ordering;

use super::{bound, event, key, Event, EVENTS, MAX_EVENTS, NO_OWNER, OWNER};
use crate::input::keyboard::Key;
use core::sync::atomic::AtomicUsize;

/// Queue one event; drops the oldest when the queue is full.
///
/// Called from IRQ handlers (keyboard/mouse) and from [`bind`]; safe because
/// the queue lock is a leaf and interrupts are off in both paths.
pub fn push_event(event: Event) {
    if !bound() {
        return;
    }
    let mut events = EVENTS.lock();
    if events.len() >= MAX_EVENTS {
        events.pop_front();
    }
    events.push_back(event);
}

/// Queue a key press/release, translating the terminal key into a [`key`]
/// code. Called from the keyboard IRQ when a compositor is bound. Rings the
/// compositor's key doorbell (P1.4); pointer events do not, because with
/// `inputd` running the compositor takes the pointer from it instead.
pub fn push_key(key: Key, down: bool) {
    let kind = if down { event::KEY_DOWN } else { event::KEY_UP };
    push_event(Event {
        kind,
        a: key_code(key) as i32,
        b: 0,
        reserved: 0,
    });
    let waiter = KEY_WAITER.swap(NO_OWNER, Ordering::AcqRel);
    if waiter != NO_OWNER {
        crate::ipc::channels::wake_parked(waiter);
    }
}

/// The compositor while it waits for a key (`channels::wait_any`).
pub(super) static KEY_WAITER: AtomicUsize = AtomicUsize::new(NO_OWNER);

/// Ring `me`'s doorbell on the next key, unless input is already queued.
/// Returns whether input is waiting (nothing armed then); `Err` unless `me`
/// owns the display.
pub fn arm_key_doorbell(me: usize) -> Result<bool, ()> {
    if OWNER.load(Ordering::Relaxed) != me || me == NO_OWNER {
        return Err(());
    }
    x86_64::instructions::interrupts::without_interrupts(|| {
        if !EVENTS.lock().is_empty() {
            return Ok(true);
        }
        KEY_WAITER.store(me, Ordering::Release);
        Ok(false)
    })
}

/// Withdraw `me`'s key doorbell, if armed.
pub fn disarm_key_doorbell(me: usize) {
    let _ = KEY_WAITER.compare_exchange(me, NO_OWNER, Ordering::AcqRel, Ordering::Acquire);
}

/// Queue a pointer move.
pub fn push_pointer_move(x: i32, y: i32) {
    push_event(Event {
        kind: event::POINTER_MOVE,
        a: x,
        b: y,
        reserved: 0,
    });
}

/// Queue a pointer button press/release.
pub fn push_pointer_button(button: u32, down: bool) {
    let kind = if down {
        event::POINTER_DOWN
    } else {
        event::POINTER_UP
    };
    push_event(Event {
        kind,
        a: button as i32,
        b: 0,
        reserved: 0,
    });
}

/// Queue a wheel movement of `notches` (positive scrolls up).
pub fn push_pointer_wheel(notches: i32) {
    push_event(Event {
        kind: event::POINTER_WHEEL,
        a: notches,
        b: 0,
        reserved: 0,
    });
}

/// The display code of a character key. Ctrl+letter arrives from the PS/2
/// decoder as a C0 control (Ctrl+H is 8, Ctrl+I is 9, Ctrl+M is 13), which a
/// client could not tell from Backspace/Tab/Enter; report the letter itself
/// and let the compositor add the Ctrl modifier bit.
fn char_code(c: char) -> u32 {
    match c as u32 {
        code @ 1..=26 => code + 0x60,
        code => code,
    }
}

/// Translate a decoded terminal key into its display key code.
fn key_code(key: Key) -> u32 {
    match key {
        Key::Char(c) => char_code(c),
        Key::Enter => key::ENTER,
        Key::Backspace => key::BACKSPACE,
        Key::Tab => key::TAB,
        Key::Escape => key::ESCAPE,
        Key::Space => key::SPACE,
        Key::Left => key::LEFT,
        Key::Right => key::RIGHT,
        Key::Up => key::UP,
        Key::Down => key::DOWN,
        Key::PageUp => key::PAGE_UP,
        Key::PageDown => key::PAGE_DOWN,
        Key::Home => key::HOME,
        Key::End => key::END,
        Key::Shift => key::SHIFT,
        Key::Ctrl => key::CTRL,
        Key::Alt => key::ALT,
        Key::Super => key::SUPER,
        Key::Delete => key::DELETE,
        Key::Insert => key::INSERT,
        Key::F(n) => key::F1 + u32::from(n.clamp(1, 12)) - 1,
    }
}
