//! PS/2 mouse driver (i8042 auxiliary port, IRQ12).

use crate::arch::io::{inb, outb};
use core::sync::atomic::{AtomicUsize, Ordering};
use spin::Mutex;

/// Current mouse state in screen pixels.
#[derive(Clone, Copy)]
pub struct MouseState {
    pub x: i32,
    pub y: i32,
    pub left: bool,
    pub right: bool,
    pub middle: bool,
    moved: bool,
}

static STATE: Mutex<MouseState> = Mutex::new(MouseState {
    x: 400,
    y: 300,
    left: false,
    right: false,
    middle: false,
    moved: false,
});
static BOUNDS: Mutex<(i32, i32)> = Mutex::new((1280, 720));
static PACKET: Mutex<Packet> = Mutex::new(Packet::new());

/// Bytes per packet: 3 for a plain PS/2 mouse, 4 once the IntelliMouse wheel
/// extension is enabled (the fourth byte carries the wheel movement).
static PACKET_LEN: AtomicUsize = AtomicUsize::new(3);

struct Packet {
    data: [u8; 4],
    index: usize,
}

impl Packet {
    const fn new() -> Self {
        Packet {
            data: [0; 4],
            index: 0,
        }
    }
}

/// One decoded mouse packet.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Decoded {
    pub dx: i32,
    pub dy: i32,
    pub left: bool,
    pub right: bool,
    pub middle: bool,
    /// Wheel notches, positive when the wheel rolled away from the user (up).
    pub wheel: i32,
}

/// One axis of motion: the movement byte is the low eight bits of a 9-bit
/// two's-complement value whose sign is bit `sign_bit` of the header byte, so a
/// fast movement (128..=255) stays positive instead of wrapping negative.
fn motion(header: u8, byte: u8, sign_bit: u8) -> i32 {
    i32::from(byte) - if header & sign_bit != 0 { 256 } else { 0 }
}

/// Decode a complete packet. `data[3]` is only read when `wheel` is set (a
/// 4-byte IntelliMouse packet); it is a signed 8-bit count that is positive
/// when the wheel rolls *toward* the user, so it is negated to make "up"
/// positive like the display protocol's wheel event.
pub fn decode_packet(data: &[u8; 4], wheel: bool) -> Decoded {
    Decoded {
        dx: motion(data[0], data[1], 0x10),
        dy: motion(data[0], data[2], 0x20),
        left: data[0] & 0x01 != 0,
        right: data[0] & 0x02 != 0,
        middle: data[0] & 0x04 != 0,
        wheel: if wheel { -(data[3] as i8 as i32) } else { 0 },
    }
}

fn wait_write() {
    for _ in 0..100_000 {
        // Safety: reading the i8042 status register has no side effect; it
        // exists to be polled.
        let status: u8 = unsafe { inb(0x64) };
        if status & 0x02 == 0 {
            return;
        }
    }
}

fn wait_read() {
    for _ in 0..100_000 {
        // Safety: reading the i8042 status register has no side effect; it
        // exists to be polled.
        let status: u8 = unsafe { inb(0x64) };
        if status & 0x01 != 0 {
            return;
        }
    }
}

fn ctrl_write(command: u8) {
    wait_write();
    // Safety: `wait_write` above confirmed the i8042 input buffer is empty,
    // which is the documented precondition for writing its command port.
    unsafe { outb(0x64, command) };
}

fn mouse_read() -> u8 {
    wait_read();
    // Safety: `wait_read` above confirmed the output buffer holds a byte.
    unsafe { inb(0x60) }
}

fn mouse_write(byte: u8) {
    ctrl_write(0xD4); // next byte goes to the auxiliary device
    wait_write();
    // Safety: `wait_write` above confirmed the input buffer is empty.
    unsafe { outb(0x60, byte) };
    wait_read();
    // Safety: `wait_read` above confirmed the output buffer holds the ack.
    let _ack: u8 = unsafe { inb(0x60) };
}

/// Initialise the auxiliary device and start data reporting.
pub fn init() {
    // Enable the auxiliary (mouse) port.
    ctrl_write(0xA8);

    // Read/modify/write the controller command byte: enable IRQ12, enable the
    // mouse clock.
    ctrl_write(0x20);
    wait_read();
    // Safety: `wait_read` above confirmed the output buffer holds the
    // requested controller command byte.
    let mut config: u8 = unsafe { inb(0x60) };
    config = (config | 0x02) & !0x20;
    ctrl_write(0x60);
    wait_write();
    // Safety: `wait_write` above confirmed the input buffer is empty.
    unsafe { outb(0x60, config) };

    // Defaults, then probe for the wheel, then enable data reporting.
    mouse_write(0xF6);
    let wheel = enable_wheel();
    mouse_write(0xF4);

    crate::serial_println!(
        "mouse: PS/2 auxiliary device enabled ({})",
        if wheel { "wheel" } else { "no wheel" }
    );
}

/// The IntelliMouse handshake: setting the sample rate to 200, 100, 80 in turn
/// makes a wheel mouse report device id 3 and switch to 4-byte packets. A plain
/// mouse ignores the sequence and keeps id 0, so it stays on 3-byte packets.
/// Returns whether the wheel is now enabled.
fn enable_wheel() -> bool {
    for rate in [200u8, 100, 80] {
        mouse_write(0xF3); // set sample rate
        mouse_write(rate);
    }
    mouse_write(0xF2); // get device id
    let id = mouse_read();
    let wheel = id == 3;
    PACKET_LEN.store(if wheel { 4 } else { 3 }, Ordering::Relaxed);
    wheel
}

/// Set the screen bounds used to clamp the cursor.
pub fn set_bounds(width: i32, height: i32) {
    // `BOUNDS` and `STATE` are shared with the IRQ12 handler.
    x86_64::instructions::interrupts::without_interrupts(|| {
        let mut bounds = BOUNDS.lock();
        *bounds = (width.max(1), height.max(1));
        let mut state = STATE.lock();
        state.x = state.x.clamp(0, width - 1);
        state.y = state.y.clamp(0, height - 1);
    })
}

/// Feed a byte from the auxiliary port (called from the IRQ12 handler).
pub fn push_byte(byte: u8) {
    let len = PACKET_LEN.load(Ordering::Relaxed);
    let mut packet = PACKET.lock();
    let index = packet.index;
    if index == 0 && byte & 0x08 == 0 {
        // First byte must have bit 3 set; otherwise resynchronise.
        return;
    }
    packet.data[index] = byte;
    packet.index = index + 1;
    if packet.index < len {
        return;
    }

    let decoded = decode_packet(&packet.data, len == 4);
    packet.index = 0;
    drop(packet);
    // The raw bus gets every packet; the legacy stream below runs beside it
    // until the compositor takes its pointer from `inputd`.
    super::mouse_tap::TAP.lock().feed(&decoded);
    let Decoded {
        dx,
        dy,
        left,
        right,
        middle,
        wheel,
    } = decoded;

    let (width, height) = *BOUNDS.lock();
    let mut state = STATE.lock();
    let previous = (state.left, state.right, state.middle);
    // PS/2 Y is positive upwards; screen Y grows downwards.
    state.x = (state.x + dx).clamp(0, width - 1);
    state.y = (state.y - dy).clamp(0, height - 1);
    state.left = left;
    state.right = right;
    state.middle = middle;
    state.moved = true;
    let position = (state.x, state.y);
    let buttons = (state.left, state.right, state.middle);
    drop(state);

    // When a compositor owns the display, it gets every move and button
    // transition as an input event; the multiplexer's cursor path is then
    // dormant (it checks `display::bound` before consuming `moved`).
    if crate::display::bound() {
        if dx != 0 || dy != 0 {
            crate::display::push_pointer_move(position.0, position.1);
        }
        for (was, now, button) in [
            (previous.0, buttons.0, crate::display::button::LEFT),
            (previous.1, buttons.1, crate::display::button::RIGHT),
            (previous.2, buttons.2, crate::display::button::MIDDLE),
        ] {
            if was != now {
                crate::display::push_pointer_button(button, now);
            }
        }
        if wheel != 0 {
            crate::display::push_pointer_wheel(wheel);
        }
    }
}

/// A copy of the current mouse state (for the display grant's initial pointer
/// seed and diagnostics).
pub fn state() -> MouseState {
    x86_64::instructions::interrupts::without_interrupts(|| *STATE.lock())
}

/// Return the position if the mouse moved since the last call.
///
/// The kernel mux calls this every frame with interrupts enabled, and the
/// IRQ12 handler (`push_byte`) takes the same `STATE` lock, so the lock is
/// held with interrupts off: a mouse IRQ inside the critical section would
/// otherwise spin forever on a single CPU.
pub fn take_moved() -> Option<(i32, i32)> {
    x86_64::instructions::interrupts::without_interrupts(|| {
        #[cfg(lazyos_tests)]
        crate::task::harness::note_critical_section();
        let mut state = STATE.lock();
        if state.moved {
            state.moved = false;
            Some((state.x, state.y))
        } else {
            None
        }
    })
}

/// Put the driver in 3- or 4-byte packet mode and drop any half-received
/// packet, for tests (the real switch happens in [`init`]).
#[cfg(lazyos_tests)]
pub fn set_wheel_mode_for_test(wheel: bool) {
    PACKET_LEN.store(if wheel { 4 } else { 3 }, Ordering::Relaxed);
    PACKET.lock().index = 0;
}
