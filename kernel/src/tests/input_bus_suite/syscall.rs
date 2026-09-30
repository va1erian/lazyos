//! Syscall 25: the `input.raw` capability gate and the poll contract.

use super::*;
use crate::input::keyboard;
use crate::input::rawsys::op;
use crate::ipc::credentials::{self, Cred, CAP_INPUT_RAW, CAP_SYS_ADMIN};

const EPERM: i64 = 1;
const EBADF: i64 = 9;
const EFAULT: i64 = 14;
const EINVAL: i64 = 22;

fn failed(code: i64) -> u64 {
    (code as u64).wrapping_neg()
}

fn call(operation: u64, buf: u64, capacity: u64) -> u64 {
    process::dispatch_for_test(25, operation, buf, capacity)
}

fn poll(buf: &mut [u8]) -> u64 {
    call(op::POLL, buf.as_mut_ptr() as u64, buf.len() as u64)
}

/// A scratch task that is the current task and holds `input.raw`.
fn inputd_task() -> Result<usize, String> {
    fresh();
    let slot = scratch()?;
    credentials::set(slot, Cred::new(0, 0, CAP_INPUT_RAW, 0, 1));
    task::harness::switch_current(slot);
    Ok(slot)
}

fn record(buf: &[u8], index: usize) -> (u64, u8, u8, u16, i32) {
    let at = index * 24;
    (
        u64::from_le_bytes(buf[at..at + 8].try_into().unwrap()),
        buf[at + 16],
        buf[at + 17],
        u16::from_le_bytes(buf[at + 18..at + 20].try_into().unwrap()),
        i32::from_le_bytes(buf[at + 20..at + 24].try_into().unwrap()),
    )
}

fn press_and_release(bytes: &[u8]) {
    for &byte in bytes {
        keyboard::push_scancode(byte);
    }
}

/// PS/2 bytes pushed through the real driver entry come out as HID events.
/// Only keys the legacy path never routes to a task (modifiers, function keys,
/// Delete/Insert) are used, so the test cannot leak input into a shell.
pub fn ps2_reaches_bus() -> Result<(), String> {
    inputd_task()?;
    check!(call(op::OPEN, 0, 0) == 0, "open failed");
    press_and_release(&[0x3B, 0xBB]); // F1 down, up
    press_and_release(&[0xE0, 0x53, 0xE0, 0xD3]); // Delete
    press_and_release(&[0x2A, 0xAA]); // left Shift
    press_and_release(&[0xE0, 0x38, 0xE0, 0xB8]); // right Alt
    press_and_release(&[0xE1, 0x1D, 0x45, 0xE1, 0x9D, 0xC5]); // Pause
    let mut buf = [0u8; 24 * 16];
    let count = poll(&mut buf);
    check!(count == 10, "poll returned {count:#x}, want 10");
    let want = [
        (0x3A, 1),
        (0x3A, 0),
        (0x4C, 1),
        (0x4C, 0),
        (0xE1, 1),
        (0xE1, 0),
        (0xE6, 1),
        (0xE6, 0),
        (0x48, 1),
        (0x48, 0),
    ];
    for (index, (code, value)) in want.iter().enumerate() {
        let (seq, device, kind_, got_code, got_value) = record(&buf, index);
        check!(
            seq == index as u64 + 1
                && device == bus::device::PS2_KEYBOARD
                && kind_ == kind::KEY
                && got_code == *code
                && got_value == *value,
            "record {index}: seq {seq} code {got_code:#x} value {got_value}"
        );
    }
    check!(poll(&mut buf) == 0, "second poll not empty");
    keyboard::reset();
    bus::reset();
    fresh();
    Ok(())
}

/// Only `input.raw` opens the bus, and the kernel task never does.
pub fn capability_gate() -> Result<(), String> {
    fresh();
    let slot = scratch()?;
    task::harness::switch_current(slot);

    // Root-with-everything-but-the-bit, and an unprivileged uid.
    for cred in [
        Cred::new(0, 0, CAP_SYS_ADMIN, 0, 1),
        Cred::new(1000, 1000, 0, 0, 1),
    ] {
        credentials::set(slot, cred);
        for operation in [op::OPEN, op::POLL, op::CLOSE] {
            let code = call(operation, 0, 0);
            check!(
                code == failed(EPERM),
                "op {operation} without the capability -> {code:#x}"
            );
        }
        check!(bus::consumer_of(slot).is_none(), "ring created without cap");
    }

    // The bit alone, on an otherwise powerless uid, is enough.
    credentials::set(slot, Cred::new(1000, 1000, CAP_INPUT_RAW, 0, 1));
    check!(call(op::OPEN, 0, 0) == 0, "open with the capability failed");
    check!(call(99, 0, 0) == failed(EINVAL), "unknown op accepted");
    check!(call(op::CLOSE, 0, 0) == 0, "close failed");

    // The kernel task is refused even though root holds every bit.
    task::harness::switch_current(task::KERNEL_TASK);
    check!(
        call(op::OPEN, 0, 0) == failed(EPERM),
        "kernel task opened the bus"
    );
    bus::reset();
    fresh();
    Ok(())
}

/// The boot path withholds the bit from kernel-started programs (all root)
/// without touching their other capabilities.
pub fn drop_caps_only_removes_the_named_bits() -> Result<(), String> {
    fresh();
    let slot = scratch()?;
    credentials::set(slot, Cred::ROOT);
    check!(
        credentials::of(slot).caps & CAP_INPUT_RAW != 0,
        "root lacks it"
    );
    credentials::drop_caps(slot, CAP_INPUT_RAW);
    let after = credentials::of(slot);
    check!(
        after.caps == credentials::CAP_ALL & !CAP_INPUT_RAW,
        "caps are {:#x}",
        after.caps
    );
    check!(after.uid == 0, "uid changed");
    // The stripped task cannot open the bus even though it is root.
    task::harness::switch_current(slot);
    check!(
        call(op::OPEN, 0, 0) == failed(EPERM),
        "root without the bit opened it"
    );
    credentials::drop_caps(usize::MAX, CAP_INPUT_RAW); // out of range: ignored
    fresh();
    Ok(())
}

/// Capacity handling, the `EBADF` states, and the "a bad buffer loses no
/// input" guarantee.
pub fn poll_bounds_and_faults() -> Result<(), String> {
    inputd_task()?;
    let mut buf = [0u8; 24 * 8];
    check!(poll(&mut buf) == failed(EBADF), "poll before open");
    check!(call(op::OPEN, 0, 0) == 0, "open");
    check!(call(op::OPEN, 0, 0) == 0, "re-open is idempotent");
    for code in 0..5u16 {
        bus::publish(bus::device::PS2_KEYBOARD, kind::KEY, 0x04 + code, 1);
    }
    // No room for a whole record: nothing is consumed.
    check!(
        call(op::POLL, buf.as_mut_ptr() as u64, 23) == 0,
        "23-byte poll"
    );
    check!(
        call(op::POLL, buf.as_mut_ptr() as u64, 0) == 0,
        "0-byte poll"
    );
    check!(call(op::POLL, 0, 48) == failed(EFAULT), "null buffer");
    // An unmapped buffer fails without swallowing the queued events.
    let previous = crate::user_ptr::set_trust_kernel_pointers(false);
    let bad = call(op::POLL, 0x10, 24 * 8);
    crate::user_ptr::set_trust_kernel_pointers(previous);
    check!(bad == failed(EFAULT), "unmapped buffer -> {bad:#x}");
    // Two records fit, then the remaining three.
    check!(
        call(op::POLL, buf.as_mut_ptr() as u64, 48) == 2,
        "two-record poll"
    );
    check!(
        record(&buf, 0).0 == 1 && record(&buf, 1).0 == 2,
        "lost input"
    );
    check!(poll(&mut buf) == 3, "remaining three");
    check!(record(&buf, 2).0 == 5, "tail order");
    // Close, then the ring is gone.
    check!(call(op::CLOSE, 0, 0) == 0, "close");
    check!(poll(&mut buf) == failed(EBADF), "poll after close");
    check!(call(op::CLOSE, 0, 0) == failed(EBADF), "double close");
    bus::reset();
    fresh();
    Ok(())
}

/// The old `display_input_poll` stream is untouched by the new bus: with both
/// a display grant and a raw consumer, one keystroke reaches each.
pub fn legacy_path_unchanged() -> Result<(), String> {
    inputd_task()?;
    crate::display::reset();
    check!(call(op::OPEN, 0, 0) == 0, "open");
    let mut info = [0u64; crate::display::INFO_WORDS];
    let bound =
        process::dispatch_for_test(12, crate::display::op::BIND, info.as_mut_ptr() as u64, 0);
    // The scratch task holds only `input.raw`, which is below what `bind`
    // needs; the legacy path is exercised through its real gate.
    check!(
        bound == failed(EPERM),
        "bind without SYS_ADMIN -> {bound:#x}"
    );
    credentials::set(
        task::current(),
        Cred::new(0, 0, CAP_INPUT_RAW | CAP_SYS_ADMIN, 0, 1),
    );
    let bound =
        process::dispatch_for_test(12, crate::display::op::BIND, info.as_mut_ptr() as u64, 0);
    check!(bound == 0, "bind -> {bound:#x}");
    let mut seed = [0u8; 16];
    process::dispatch_for_test(
        12,
        crate::display::op::INPUT_POLL,
        seed.as_mut_ptr() as u64,
        16,
    );
    keyboard::reset();
    press_and_release(&[0x3B, 0xBB]); // F1
    let mut legacy = [0u8; 64];
    let count = process::dispatch_for_test(
        12,
        crate::display::op::INPUT_POLL,
        legacy.as_mut_ptr() as u64,
        64,
    );
    check!(count == 2, "legacy stream got {count} events, want 2");
    check!(
        u32::from_le_bytes(legacy[0..4].try_into().unwrap()) == 3
            && i32::from_le_bytes(legacy[4..8].try_into().unwrap()) == 0x110,
        "legacy first event is not KeyDown F1"
    );
    let mut raw = [0u8; 24 * 4];
    check!(poll(&mut raw) == 2, "raw stream missing events");
    check!(record(&raw, 0).3 == 0x3A, "raw F1 usage");
    process::dispatch_for_test(12, crate::display::op::UNBIND, 0, 0);
    crate::display::reset();
    keyboard::reset();
    bus::reset();
    fresh();
    Ok(())
}
