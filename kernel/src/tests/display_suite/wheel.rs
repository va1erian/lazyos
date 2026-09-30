//! The mouse wheel: PS/2 IntelliMouse packets decode to signed notches, and a
//! bound compositor receives them as `POINTER_WHEEL` records (up positive).

use super::*;
use crate::input::mouse::{self, decode_packet, Decoded};

const POINTER_MOVE: u32 = 0;
const POINTER_DOWN: u32 = 1;
const POINTER_WHEEL: u32 = 5;

/// Feed one 4-byte packet (`flags`, `dx`, `dy`, `z`) to the driver.
fn push_packet(flags: u8, dx: i8, dy: i8, z: i8) {
    for byte in [flags | 0x08, dx as u8, dy as u8, z as u8] {
        mouse::push_byte(byte);
    }
}

/// Bind the display to a scratch task, drain the seeded pointer move, and
/// leave the driver in wheel mode. Callers must call [`finish`].
fn bound_with_wheel() -> Result<(), String> {
    crate::display::reset();
    scratch_task()?;
    let mut info = [0u64; crate::display::INFO_WORDS];
    let code =
        process::dispatch_for_test(12, crate::display::op::BIND, info.as_mut_ptr() as u64, 0);
    check!(code == 0, "bind -> {code:#x}");
    drain()?;
    mouse::set_wheel_mode_for_test(true);
    Ok(())
}

/// Poll everything queued for the compositor; returns the event count.
fn poll(events: &mut [u8]) -> usize {
    process::dispatch_for_test(
        12,
        crate::display::op::INPUT_POLL,
        events.as_mut_ptr() as u64,
        events.len() as u64,
    ) as usize
}

/// Discard whatever is queued.
fn drain() -> Result<(), String> {
    let mut events = [0u8; 16 * 300];
    poll(&mut events);
    Ok(())
}

fn finish() {
    mouse::set_wheel_mode_for_test(false);
    process::dispatch_for_test(12, crate::display::op::UNBIND, 0, 0);
    task::harness::switch_current(task::KERNEL_TASK);
    crate::display::reset();
    task::harness::reset();
}

/// A packet decodes to motion, buttons and a signed, up-positive wheel.
pub fn mouse_packet_decoding() -> Result<(), String> {
    let plain = decode_packet(&[0x09, 5, 0xFB, 0x7F], false);
    check!(
        plain
            == Decoded {
                dx: 5,
                dy: -5,
                left: true,
                right: false,
                middle: false,
                wheel: 0,
            },
        "a 3-byte packet ignores the fourth byte: {plain:?}"
    );
    // Z is positive toward the user (down); the decoded wheel is up-positive.
    for (z, want) in [(1i8, -1), (-1, 1), (127, -127), (-127, 127), (0, 0)] {
        let got = decode_packet(&[0x08, 0, 0, z as u8], true).wheel;
        check!(got == want, "z {z} decoded to {got}, want {want}");
    }
    let all = decode_packet(&[0x07, 0, 0, 0], true);
    check!(
        all.left && all.right && all.middle,
        "buttons decode: {all:?}"
    );
    Ok(())
}

/// Wheel packets reach a bound compositor as `POINTER_WHEEL`, a motionless one
/// queues no move, and a button change alongside a wheel still reports both.
pub fn wheel_reaches_compositor() -> Result<(), String> {
    bound_with_wheel()?;
    push_packet(0, 0, 0, -1); // one notch up
    push_packet(0, 0, 0, 3); // three notches down
    push_packet(0x01, 0, 0, -2); // left press while the wheel rolls up two
    let mut events = [0u8; 16 * 16];
    let count = poll(&mut events);
    let want = [
        (POINTER_WHEEL, 1),
        (POINTER_WHEEL, -3),
        (POINTER_DOWN, 1),
        (POINTER_WHEEL, 2),
    ];
    check!(
        count == want.len(),
        "polled {count} events, want {}",
        want.len()
    );
    for (index, &expected) in want.iter().enumerate() {
        let got = event_at(&events, index);
        check!(
            got == expected,
            "event {index} is {got:?}, want {expected:?}"
        );
    }
    // Release the button so the driver's state does not leak into later tests.
    push_packet(0, 0, 0, 0);
    finish();
    Ok(())
}

/// A plain 3-byte mouse never produces a wheel event, and a stray byte with
/// bit 3 clear resynchronises the packet stream instead of shifting it.
pub fn plain_mouse_and_resync() -> Result<(), String> {
    bound_with_wheel()?;
    mouse::set_wheel_mode_for_test(false);
    for byte in [0x08u8, 0, 0] {
        mouse::push_byte(byte);
    }
    mouse::push_byte(0x00); // garbage between packets: dropped
    for byte in [0x08u8, 4, 0] {
        mouse::push_byte(byte);
    }
    let mut events = [0u8; 16 * 8];
    let count = poll(&mut events);
    check!(count == 1, "polled {count}, want the one move");
    check!(
        event_at(&events, 0).0 == POINTER_MOVE,
        "the only event is a move"
    );

    // Back in wheel mode the stream is still aligned.
    mouse::set_wheel_mode_for_test(true);
    push_packet(0, 0, 0, -1);
    let count = poll(&mut events);
    check!(
        count == 1 && event_at(&events, 0) == (POINTER_WHEEL, 1),
        "wheel after resync: {count} events"
    );
    finish();
    Ok(())
}

/// Sustained wheel traffic with nobody polling keeps the queue bounded, and a
/// drained queue is still in order afterwards.
pub fn wheel_soak_bounded_queue() -> Result<(), String> {
    bound_with_wheel()?;
    for round in 0..50_000usize {
        push_packet(0, 0, 0, if round % 2 == 0 { -1 } else { 1 });
    }
    let mut events = [0u8; 16 * 512];
    let count = poll(&mut events);
    check!(
        (1..=256).contains(&count),
        "queue held {count} events, want 1..=256"
    );
    for index in 0..count {
        let (kind, notches) = event_at(&events, index);
        check!(
            kind == POINTER_WHEEL && notches.abs() == 1,
            "event {index}: kind {kind}, notches {notches}"
        );
    }
    // The stream is still aligned after the flood.
    push_packet(0, 0, 0, -3);
    let count = poll(&mut events);
    check!(
        count == 1 && event_at(&events, 0) == (POINTER_WHEEL, 3),
        "post-soak wheel: {count} events"
    );
    finish();
    Ok(())
}
