//! `present` (display op 3, issue #340) validates only the damage rows it
//! reads: the byte-range arithmetic, small presents landing in the right
//! place, a hole unmapped in the screen buffer refusing only the presents
//! that touch it, and a soak of many thousands of small presents.

use super::*;
use crate::display::{damage_rows, op, INFO_WORDS};

pub(super) const EFAULT: i64 = 14;
pub(super) const PAGE: u64 = 4096;

/// Turns user-pointer validation on for the guard's lifetime; the suite's
/// other tests pass kernel buffers as "user" pointers, so it is off by
/// default under `lazyos_tests`, which would hide the check under test.
struct Strict(bool);

impl Strict {
    fn on() -> Strict {
        Strict(crate::user_ptr::set_trust_kernel_pointers(false))
    }
}

impl Drop for Strict {
    fn drop(&mut self) {
        crate::user_ptr::set_trust_kernel_pointers(self.0);
    }
}

/// A bound screen buffer; presents against it run with validation on.
pub(super) struct Screen {
    pub(super) width: usize,
    pub(super) height: usize,
    pub(super) va: u64,
    _strict: Strict,
}

impl Screen {
    pub(super) fn row_bytes(&self) -> u64 {
        self.width as u64 * 4
    }

    /// Page-aligned address of a page in the middle of the buffer.
    fn middle_page(&self) -> u64 {
        self.va + ((self.row_bytes() * self.height as u64 / 2) & !(PAGE - 1))
    }

    /// Unmap one page of the buffer, as the owner's `munmap` would.
    pub(super) fn unmap_page(&self, page: u64) -> Result<(), String> {
        let cleared = crate::mem::unmap_range(crate::mem::kernel_table(), page, page + PAGE);
        check!(cleared == 1, "unmap_range cleared {cleared} pages");
        Ok(())
    }
}

/// Bind the display on a fresh scratch task. The bind info block is a kernel
/// stack buffer, so validation is only switched on once bind has returned.
pub(super) fn bind_screen() -> Result<Screen, String> {
    crate::display::reset();
    scratch_task()?;
    let mut info = [0u64; INFO_WORDS];
    let code = process::dispatch_for_test(12, op::BIND, info.as_mut_ptr() as u64, 0);
    check!(code == 0, "bind -> {code:#x}");
    Ok(Screen {
        width: info[0] as usize,
        height: info[1] as usize,
        va: info[5],
        _strict: Strict::on(),
    })
}

/// Unbind and return to the kernel task.
pub(super) fn unbind_screen() -> Result<(), String> {
    let code = process::dispatch_for_test(12, op::UNBIND, 0, 0);
    check!(code == 0, "unbind -> {code:#x}");
    task::harness::switch_current(task::KERNEL_TASK);
    crate::display::reset();
    task::harness::reset();
    Ok(())
}

pub(super) fn present(x: usize, y: usize, w: usize, h: usize) -> u64 {
    let packed = x as u64 | (y as u64) << 16 | (w as u64) << 32 | (h as u64) << 48;
    process::dispatch_for_test(12, op::PRESENT, packed, 0)
}

/// Write one RGBA pixel into the bound screen buffer.
pub(super) fn paint(screen: &Screen, x: usize, y: usize, rgb: (u8, u8, u8)) {
    let at = (y * screen.width + x) * 4;
    // SAFETY: `(x, y)` is on screen, so `at..at + 4` is inside the mapped
    // `width * height * 4`-byte screen buffer at `va`.
    unsafe {
        let pixel = (screen.va as *mut u8).add(at);
        pixel.write(rgb.0);
        pixel.add(1).write(rgb.1);
        pixel.add(2).write(rgb.2);
        pixel.add(3).write(0xFF);
    }
}

/// Whether the real framebuffer shows roughly `rgb` at `(x, y)`.
pub(super) fn shows(x: usize, y: usize, rgb: (u8, u8, u8)) -> Result<bool, String> {
    let color =
        crate::console::with_framebuffer(|fb| fb.read_pixel(x, y)).ok_or("no framebuffer")?;
    let near = |a: u8, b: u8| a.abs_diff(b) < 16;
    Ok(near(color.r, rgb.0) && near(color.g, rgb.1) && near(color.b, rgb.2))
}

/// The validated span is exactly rows `y..y + h`, and overflow is refused.
pub fn present_damage_rows_arithmetic() -> Result<(), String> {
    check!(damage_rows(1280, 0, 1) == Some((0, 5120)), "first row");
    check!(
        damage_rows(1280, 719, 1) == Some((719 * 5120, 5120)),
        "last row"
    );
    check!(
        damage_rows(1280, 10, 16) == Some((10 * 5120, 16 * 5120)),
        "cursor-sized rows"
    );
    check!(
        damage_rows(1280, 0, 720) == Some((0, 1280 * 720 * 4)),
        "whole screen"
    );
    check!(damage_rows(usize::MAX, 0, 1).is_none(), "row overflow");
    check!(
        damage_rows(1 << 40, 1 << 30, 1).is_none(),
        "offset overflow"
    );
    check!(
        damage_rows(1 << 40, 0, 1 << 30).is_none(),
        "length overflow"
    );
    Ok(())
}

/// A 1x1 present of a mapped buffer succeeds and lands where it was drawn,
/// including the last row and column (the source slice starts at row `y`).
pub fn present_one_pixel() -> Result<(), String> {
    let screen = bind_screen()?;
    let (right, bottom) = (screen.width - 1, screen.height - 1);
    let spots = [
        (0, 0, (0xF0, 0x10, 0x10)),
        (right, bottom, (0x10, 0xF0, 0x10)),
        (right / 2, bottom / 2 + 1, (0x10, 0x10, 0xF0)),
    ];
    for &(x, y, rgb) in &spots {
        paint(&screen, x, y, rgb);
        let code = present(x, y, 1, 1);
        check!(code == 0, "1x1 present at ({x}, {y}) -> {code:#x}");
        check!(shows(x, y, rgb)?, "pixel ({x}, {y}) not presented");
    }
    // An oversized rectangle is clamped to the screen, not refused.
    check!(present(right, bottom, 500, 500) == 0, "clamped present");
    // Off-screen and empty rectangles are no-ops.
    check!(present(screen.width, 0, 1, 1) == 0, "off-screen x");
    check!(present(0, 0, 0, 1) == 0, "zero width");
    unbind_screen()
}

/// Unmap one page in the middle of the screen buffer: presents whose rows
/// touch it are `-EFAULT`, presents of rows that are still mapped succeed.
/// This also proves a small present no longer walks the whole grant.
pub fn present_partial_unmap() -> Result<(), String> {
    let screen = bind_screen()?;
    let hole = screen.middle_page();
    screen.unmap_page(hole)?;
    let first = ((hole - screen.va) / screen.row_bytes()) as usize;
    let last = ((hole + PAGE - 1 - screen.va) / screen.row_bytes()) as usize;

    let fault = failed(EFAULT);
    check!(present(0, first, 1, 1) == fault, "row {first} not -EFAULT");
    check!(present(0, last, 1, 1) == fault, "row {last} not -EFAULT");
    check!(
        present(0, 0, screen.width, screen.height) == fault,
        "whole-screen present over the hole not -EFAULT"
    );
    check!(
        present(0, first - 2, 4, 3) == fault,
        "rows spanning into the hole not -EFAULT"
    );

    paint(&screen, 3, 0, (0xF0, 0xF0, 0x10));
    check!(present(3, 0, 1, 1) == 0, "row 0 present failed");
    check!(shows(3, 0, (0xF0, 0xF0, 0x10))?, "row 0 not presented");
    check!(
        present(0, 0, screen.width, first) == 0,
        "rows above the hole"
    );
    let below = last + 1;
    paint(&screen, 5, below, (0x10, 0xF0, 0xF0));
    check!(present(5, below, 1, 1) == 0, "row {below} present failed");
    check!(shows(5, below, (0x10, 0xF0, 0xF0))?, "row below not shown");
    check!(
        present(0, below, screen.width, screen.height) == 0,
        "rows below the hole (clamped) failed"
    );
    unbind_screen()
}

/// Twenty thousand cursor-sized presents across the screen, with a hole
/// unmapped halfway through: every present whose rows miss the hole
/// succeeds, every one touching it is `-EFAULT`, and the grant still works.
pub fn present_small_soak() -> Result<(), String> {
    const ROUNDS: usize = 20_000;
    const SIDE: usize = 16;
    let screen = bind_screen()?;
    let hole = screen.middle_page();
    let (mut shown, mut refused) = (0usize, 0usize);
    for round in 0..ROUNDS {
        if round == ROUNDS / 2 {
            screen.unmap_page(hole)?;
        }
        let x = (round * 37) % screen.width;
        let y = (round * 101) % screen.height;
        let rows = SIDE.min(screen.height - y) as u64;
        let start = screen.va + y as u64 * screen.row_bytes();
        let end = start + rows * screen.row_bytes();
        let touches = round >= ROUNDS / 2 && start < hole + PAGE && end > hole;
        let code = present(x, y, SIDE, SIDE);
        if touches {
            check!(code == failed(EFAULT), "round {round}: hole not -EFAULT");
            refused += 1;
        } else {
            check!(code == 0, "round {round}: present -> {code:#x}");
            shown += 1;
        }
    }
    check!(
        refused > 0 && shown > ROUNDS / 2,
        "shown {shown}, refused {refused}"
    );
    paint(&screen, 1, 1, (0xF0, 0x10, 0xF0));
    check!(present(1, 1, 1, 1) == 0, "final present failed");
    check!(shows(1, 1, (0xF0, 0x10, 0xF0))?, "final pixel not shown");
    unbind_screen()
}
