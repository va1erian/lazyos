//! The logical screen through the display grant (H1 of
//! `docs/real-pc-boot-plan.md`): under a 4K, a 2560x1600 and a 640x480 mode,
//! `bind` reports and allocates the logical screen, the buffer fits the
//! shared-buffer cap, and `present` lands at the centring offset without
//! touching the borders.

use super::*;
use crate::display::logical;
use crate::limits::Limits;

/// Bind under the current geometry; returns the info words.
fn bind() -> Result<[u64; crate::display::INFO_WORDS], String> {
    let mut info = [0u64; crate::display::INFO_WORDS];
    let code =
        process::dispatch_for_test(12, crate::display::op::BIND, info.as_mut_ptr() as u64, 0);
    check!(code == 0, "bind -> {code:#x}");
    Ok(info)
}

fn finish() {
    let _ = process::dispatch_for_test(12, crate::display::op::UNBIND, 0, 0);
    task::harness::switch_current(task::KERNEL_TASK);
    crate::display::reset();
    task::harness::reset();
}

/// `bind` under three firmware modes reports `min(mode, 1920x1080)` per axis
/// and a buffer of exactly that size, inside the per-process cap.
pub fn logical_bind_sizes() -> Result<(), String> {
    for (mode, want) in [
        ((3840, 2160), (1920, 1080)),
        ((2560, 1600), (1920, 1080)),
        ((2560, 1440), (1920, 1080)),
        ((640, 480), (640, 480)),
        ((1280, 720), (1280, 720)),
    ] {
        crate::display::reset();
        scratch_task()?;
        let info = crate::display::with_mode_for_test(mode.0, mode.1, bind);
        finish();
        let info = info?;
        let (width, height, size) = (info[0], info[1], info[6]);
        check!(
            (width, height) == (want.0, want.1),
            "{mode:?}: bound {width}x{height}, want {want:?}"
        );
        check!(size == width * height * 4, "{mode:?}: buffer {size}");
        // The per-process cap a machine with this screen derives.
        let cap = Limits::for_machine(crate::mem::usable_ram(), size).shared_buffer_max;
        check!(size <= cap, "{mode:?}: buffer {size} over the cap");
        // The shell holds a double-buffered full-screen desktop window and
        // its double-buffered taskbar (32 px tall) in one process.
        let shell = 2 * size + 2 * width * 32 * 4;
        check!(
            shell <= cap,
            "{mode:?}: desktop plus taskbar {shell} over the per-process cap"
        );
    }
    Ok(())
}

/// Same bind sizes under a configured cap (issue #717): `display.max` set to
/// 2560x1440 lets a 1440p firmware framebuffer bind whole, sizes every
/// screen buffer by it, and re-derives the limits to cover the shell's
/// double-buffered desktop.
pub fn logical_bind_sizes_configured_cap() -> Result<(), String> {
    for (mode, want) in [
        ((3840, 2160), (2560, 1440)),
        ((2560, 1600), (2560, 1440)),
        ((2560, 1440), (2560, 1440)),
        ((1920, 1080), (1920, 1080)),
        ((640, 480), (640, 480)),
    ] {
        crate::display::reset();
        logical::set_cap(2560, 1440);
        let limits_pre = Limits::for_machine(
            crate::mem::usable_ram(),
            logical::fit(mode.0, mode.1).rgba_bytes(),
        );
        scratch_task()?;
        let info = crate::display::with_mode_for_test(mode.0, mode.1, bind);
        finish();
        let info = info?;
        let (width, height, size) = (info[0], info[1], info[6]);
        check!(
            (width, height) == (want.0, want.1),
            "{mode:?}: bound {width}x{height}, want {want:?}"
        );
        check!(size == width * height * 4, "{mode:?}: buffer {size}");
        check!(
            Limits::for_machine(crate::mem::usable_ram(), size).shared_buffer_max >= size,
            "{mode:?}: a single buffer over the re-derived cap"
        );
        let shell = 2 * size + 2 * width * 32 * 4;
        check!(
            shell <= limits_pre.shared_buffer_max,
            "{mode:?}: desktop plus taskbar {shell} over the re-derived cap"
        );
        logical::set_cap(1920, 1080);
    }
    check!(
        logical::cap() == (1920, 1080),
        "cap not restored: {:?}",
        logical::cap()
    );
    Ok(())
}

/// Under a pretend 4K mode the logical screen starts at (960, 540), which is
/// inside QEMU's real 1280x720 framebuffer: a presented pixel lands there,
/// shifted by the offset, and a border pixel is never written, across a soak
/// of random damage rectangles (the view clips what does not fit).
pub fn logical_present_offsets_and_clips() -> Result<(), String> {
    let real =
        crate::console::with_framebuffer(|fb| (fb.width(), fb.height())).ok_or("no framebuffer")?;
    let screen = logical::fit(3840, 2160);
    check!((screen.x, screen.y) == (960, 540), "4K offset {:?}", screen);
    if real.0 <= screen.x + 20 || real.1 <= screen.y + 20 {
        return Ok(()); // a framebuffer too small to observe the offset
    }
    crate::display::reset();
    scratch_task()?;
    let border = (100usize, 100usize);
    let marker = crate::gfx::Color::rgb(0x12, 0x34, 0x56);
    let result = crate::display::with_mode_for_test(3840, 2160, || -> Result<(), String> {
        let info = bind()?;
        // `bind` blacked the borders; mark one to prove nothing writes there.
        let black = crate::console::with_framebuffer(|fb| fb.read_pixel(border.0, border.1))
            .ok_or("no framebuffer")?;
        check!(
            (black.r, black.g, black.b) == (0, 0, 0),
            "bind left the border {black:?}"
        );
        crate::console::with_framebuffer(|fb| fb.write_pixel(border.0, border.1, marker));
        let (width, va) = (info[0] as usize, info[5]);
        // Paint the whole logical buffer green, then a red dot at (10, 10).
        for i in 0..(info[6] as usize / 4) {
            let rgba = if i == 10 * width + 10 {
                [0xF0, 0x10, 0x10, 0xFF]
            } else {
                [0x10, 0xF0, 0x10, 0xFF]
            };
            // SAFETY: inside the bound screen buffer of `info[6]` bytes.
            unsafe { (va as *mut [u8; 4]).add(i).write(rgba) };
        }
        let packed = 10u64 | (10 << 16) | (1 << 32) | (1 << 48);
        let code = process::dispatch_for_test(12, crate::display::op::PRESENT, packed, 0);
        check!(code == 0, "present -> {code:#x}");
        let dot =
            crate::console::with_framebuffer(|fb| fb.read_pixel(screen.x + 10, screen.y + 10))
                .ok_or("no framebuffer")?;
        check!(
            dot.r > 200 && dot.g < 60,
            "presented dot is {dot:?} at the offset"
        );
        // Random rectangles, many past the real framebuffer's edge.
        let mut seed = 0x1234_5678_9ABC_DEF0u64;
        for round in 0..400 {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            let field = |shift: u32, modulo: u64| (seed >> shift) % modulo;
            let (x, y) = (field(0, 2000), field(16, 1200));
            let (w, h) = (field(32, 400) + 1, field(48, 300) + 1);
            let packed = x | (y << 16) | (w << 32) | (h << 48);
            let code = process::dispatch_for_test(12, crate::display::op::PRESENT, packed, 0);
            check!(code == 0, "round {round}: present -> {code:#x}");
        }
        let kept = crate::console::with_framebuffer(|fb| fb.read_pixel(border.0, border.1))
            .ok_or("no framebuffer")?;
        check!(
            (kept.r, kept.g, kept.b) == (marker.r, marker.g, marker.b),
            "border pixel overwritten: {kept:?}"
        );
        Ok(())
    });
    finish();
    result
}
