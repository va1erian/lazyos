//! Kernel-bind refusal, bind/input-present round-trip, and modifier
//! keys reaching the compositor.

use super::*;

/// The kernel multiplexer itself may not bind: the grant is for a user
/// compositor, and the mux is the fallback owner.
pub fn kernel_bind_refused() -> Result<(), String> {
    crate::display::reset();
    task::register_kernel();
    task::harness::reset();
    task::harness::switch_current(task::KERNEL_TASK);
    let mut info = [0u64; crate::display::INFO_WORDS];
    let code =
        process::dispatch_for_test(12, crate::display::op::BIND, info.as_mut_ptr() as u64, 0);
    check!(
        code == failed(1),
        "kernel bind -> {code:#x} (expected -EPERM)"
    );
    check!(!crate::display::bound(), "bound() after a refused bind");
    crate::display::reset();
    Ok(())
}

/// Bind returns the screen geometry and a mapped buffer; a second task is
/// refused while the grant is live; input events round-trip; present
/// reaches the real framebuffer; unbind releases the grant.
pub fn bind_input_present_roundtrip() -> Result<(), String> {
    crate::display::reset();
    let first = scratch_task()?;

    // Bind: the screen buffer is created and mapped in this task.
    let mut info = [0u64; crate::display::INFO_WORDS];
    let code =
        process::dispatch_for_test(12, crate::display::op::BIND, info.as_mut_ptr() as u64, 0);
    check!(code == 0, "bind -> {code:#x}");
    let (width, height, va, size) = (info[0], info[1], info[5], info[6]);
    check!(
        width > 0 && height > 0 && width < 8192 && height < 8192,
        "implausible screen {width}x{height}"
    );
    check!(size == width * height * 4, "screen size {size}");
    check!(va != 0, "bind returned no buffer mapping");
    check!(crate::display::bound(), "bound() false after a live bind");

    // A second live task cannot steal the display.
    let second = task::spawn_fork().map_err(to_string)?;
    task::harness::switch_current(second);
    let mut other = [0u64; crate::display::INFO_WORDS];
    let code =
        process::dispatch_for_test(12, crate::display::op::BIND, other.as_mut_ptr() as u64, 0);
    check!(
        code == failed(16),
        "second bind -> {code:#x} (expected -EBUSY)"
    );
    task::harness::switch_current(first);

    // Write a red square into the screen buffer and present it: the pixel
    // must land in the real framebuffer.
    let square = (12usize, 20usize);
    for row in 0..2usize {
        for col in 0..2usize {
            let at = ((square.1 + row) * width as usize + square.0 + col) * 4;
            // Safety: inside the screen buffer `va` of `size` bytes.
            unsafe {
                let pixel = (va as *mut u8).add(at);
                pixel.write(0xF0);
                pixel.add(1).write(0x10);
                pixel.add(2).write(0x10);
                pixel.add(3).write(0xFF);
            }
        }
    }
    let packed = square.0 as u64 | ((square.1 as u64) << 16) | (2 << 32) | (2 << 48);
    let code = process::dispatch_for_test(12, crate::display::op::PRESENT, packed, 0);
    check!(code == 0, "present -> {code:#x}");
    let color = crate::console::with_framebuffer(|fb| fb.read_pixel(square.0, square.1))
        .ok_or("no framebuffer")?;
    check!(
        color.r > 200 && color.g < 60 && color.b < 60,
        "presented pixel is {color:?}, expected red"
    );

    // Input queued for the owner comes back through the poll op. `bind`
    // seeds one pointer-move event, so drain that first.
    let mut drain = [0u8; 16];
    let seeded = process::dispatch_for_test(
        12,
        crate::display::op::INPUT_POLL,
        drain.as_mut_ptr() as u64,
        drain.len() as u64,
    );
    check!(seeded == 1, "bind seeded {seeded} events, expected 1");
    crate::display::push_pointer_move(100, 120);
    crate::display::push_key(Key::Char('a'), true);
    let mut events = [0u8; 32];
    let count = process::dispatch_for_test(
        12,
        crate::display::op::INPUT_POLL,
        events.as_mut_ptr() as u64,
        events.len() as u64,
    );
    check!(count == 2, "input_poll returned {count}, expected 2");
    check!(
        u32::from_le_bytes(events[0..4].try_into().unwrap()) == 0
            && i32::from_le_bytes(events[4..8].try_into().unwrap()) == 100,
        "first event is not a pointer move to (100, 120)"
    );
    check!(
        u32::from_le_bytes(events[16..20].try_into().unwrap()) == 3
            && i32::from_le_bytes(events[20..24].try_into().unwrap()) == 'a' as i32,
        "second event is not a key down for 'a'"
    );

    // Unbind releases the grant and the mux fallback sees it.
    let code = process::dispatch_for_test(12, crate::display::op::UNBIND, 0, 0);
    check!(code == 0, "unbind -> {code:#x}");
    check!(!crate::display::bound(), "bound() after unbind");
    task::harness::switch_current(task::KERNEL_TASK);
    crate::display::reset();
    task::harness::reset();
    Ok(())
}

/// Modifier and function keys are tracked by the driver and forwarded to a
/// bound compositor (issue #167), so `xuid` can implement Alt+Tab,
/// Ctrl+Esc/Super and Alt+F4. The kernel terminal behavior is unchanged.
pub fn modifier_keys_reach_compositor() -> Result<(), String> {
    crate::display::reset();
    crate::input::keyboard::reset();
    scratch_task()?;

    let mut info = [0u64; crate::display::INFO_WORDS];
    let code =
        process::dispatch_for_test(12, crate::display::op::BIND, info.as_mut_ptr() as u64, 0);
    check!(code == 0, "bind -> {code:#x}");

    // Drain the pointer-move event bind seeds.
    let mut drain = [0u8; 16];
    let seeded = process::dispatch_for_test(
        12,
        crate::display::op::INPUT_POLL,
        drain.as_mut_ptr() as u64,
        drain.len() as u64,
    );
    check!(seeded == 1, "bind seeded {seeded} events, expected 1");

    // Alt down/up, Tab down/up, F4 down/up, and the extended Super and
    // right-Ctrl sequences. Ctrl+A must still decode as ETX (0x01): the
    // terminal translation is unchanged by the new forwarding.
    for scancode in [
        0x38u8, // left Alt down
        0x0F,   // Tab down
        0x8F,   // Tab up
        0x3E,   // F4 down
        0xBE,   // F4 up
        0xE0, 0x5B, // left Super down
        0xE0, 0xDB, // left Super up
        0xB8, // left Alt up
        0xE0, 0x1D, // right Ctrl down
        0x1E, // 'a' down (Ctrl held -> ETX)
        0x9E, // 'a' up (still ETX)
        0xE0, 0x9D, // right Ctrl up
    ] {
        crate::input::keyboard::push_scancode(scancode);
    }
    let mut events = [0u8; 16 * 16];
    let count = process::dispatch_for_test(
        12,
        crate::display::op::INPUT_POLL,
        events.as_mut_ptr() as u64,
        events.len() as u64,
    );
    let expected: &[(u32, i32)] = &[
        (3, crate::display::key::ALT as i32),
        (3, crate::display::key::TAB as i32),
        (4, crate::display::key::TAB as i32),
        (3, crate::display::key::F4 as i32),
        (4, crate::display::key::F4 as i32),
        (3, crate::display::key::SUPER as i32),
        (4, crate::display::key::SUPER as i32),
        (4, crate::display::key::ALT as i32),
        (3, crate::display::key::CTRL as i32),
        (3, 0x01), // Ctrl+A stays the terminal's ETX
        (4, 0x01),
        (4, crate::display::key::CTRL as i32),
    ];
    check!(
        count as usize == expected.len(),
        "polled {count} events, expected {}",
        expected.len()
    );
    for (index, &want) in expected.iter().enumerate() {
        let got = event_at(&events, index);
        check!(got == want, "event {index} is {got:?}, expected {want:?}");
    }

    let code = process::dispatch_for_test(12, crate::display::op::UNBIND, 0, 0);
    check!(code == 0, "unbind -> {code:#x}");
    task::harness::switch_current(task::KERNEL_TASK);
    crate::display::reset();
    crate::input::keyboard::reset();
    task::harness::reset();
    Ok(())
}
