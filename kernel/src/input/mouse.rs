//! PS/2 mouse driver (i8042 auxiliary port, IRQ12).

use spin::Mutex;
use x86_64::instructions::port::Port;

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

struct Packet {
    data: [u8; 3],
    index: usize,
}

impl Packet {
    const fn new() -> Self {
        Packet {
            data: [0; 3],
            index: 0,
        }
    }
}

fn wait_write() {
    for _ in 0..100_000 {
        let status: u8 = unsafe { Port::<u8>::new(0x64).read() };
        if status & 0x02 == 0 {
            return;
        }
    }
}

fn wait_read() {
    for _ in 0..100_000 {
        let status: u8 = unsafe { Port::<u8>::new(0x64).read() };
        if status & 0x01 != 0 {
            return;
        }
    }
}

fn ctrl_write(command: u8) {
    wait_write();
    unsafe { Port::<u8>::new(0x64).write(command) };
}

fn mouse_write(byte: u8) {
    ctrl_write(0xD4); // next byte goes to the auxiliary device
    wait_write();
    unsafe { Port::<u8>::new(0x60).write(byte) };
    wait_read();
    let _ack: u8 = unsafe { Port::<u8>::new(0x60).read() };
}

/// Initialise the auxiliary device and start data reporting.
pub fn init() {
    // Enable the auxiliary (mouse) port.
    ctrl_write(0xA8);

    // Read/modify/write the controller command byte: enable IRQ12, enable the
    // mouse clock.
    ctrl_write(0x20);
    wait_read();
    let mut config: u8 = unsafe { Port::<u8>::new(0x60).read() };
    config = (config | 0x02) & !0x20;
    ctrl_write(0x60);
    wait_write();
    unsafe { Port::<u8>::new(0x60).write(config) };

    // Defaults, then enable data reporting.
    mouse_write(0xF6);
    mouse_write(0xF4);

    crate::serial_println!("mouse: PS/2 auxiliary device enabled");
}

/// Set the screen bounds used to clamp the cursor.
pub fn set_bounds(width: i32, height: i32) {
    let mut bounds = BOUNDS.lock();
    *bounds = (width.max(1), height.max(1));
    let mut state = STATE.lock();
    state.x = state.x.clamp(0, width - 1);
    state.y = state.y.clamp(0, height - 1);
}

/// Feed a byte from the auxiliary port (called from the IRQ12 handler).
pub fn push_byte(byte: u8) {
    let mut packet = PACKET.lock();
    let index = packet.index;
    if index == 0 && byte & 0x08 == 0 {
        // First byte must have bit 3 set; otherwise resynchronise.
        return;
    }
    packet.data[index] = byte;
    packet.index = index + 1;
    if packet.index < 3 {
        return;
    }

    let flags = packet.data[0];
    let dx = packet.data[1] as i8 as i32;
    let dy = packet.data[2] as i8 as i32;
    packet.index = 0;
    drop(packet);

    let (width, height) = *BOUNDS.lock();
    let mut state = STATE.lock();
    // PS/2 Y is positive upwards; screen Y grows downwards.
    state.x = (state.x + dx).clamp(0, width - 1);
    state.y = (state.y - dy).clamp(0, height - 1);
    state.left = flags & 0x01 != 0;
    state.right = flags & 0x02 != 0;
    state.middle = flags & 0x04 != 0;
    state.moved = true;
}

/// Return the position if the mouse moved since the last call.
pub fn take_moved() -> Option<(i32, i32)> {
    let mut state = STATE.lock();
    if state.moved {
        state.moved = false;
        Some((state.x, state.y))
    } else {
        None
    }
}
