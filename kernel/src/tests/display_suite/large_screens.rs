//! Full-HD and 4K screens fit the display budgets: with the limits derived
//! for the screen (`crate::limits`), `bind` gets its screen-sized buffer and
//! a compositor can still create a double-buffered full-screen window, the
//! three surfaces the per-process allowance is sized for.

use super::*;

/// Bind on a `width x height` screen, create two more full-screen buffers,
/// then release everything. The real framebuffer is never presented to.
fn bind_three_surfaces(width: usize, height: usize) -> Result<(), String> {
    crate::display::init(width, height, width, 4);
    crate::limits::init_for_machine(mem::usable_ram(), crate::display::screen_bytes());
    crate::display::reset();
    let slot = scratch_task()?;
    let mut info = [0u64; crate::display::INFO_WORDS];
    let code =
        process::dispatch_for_test(12, crate::display::op::BIND, info.as_mut_ptr() as u64, 0);
    check!(code == 0, "{width}x{height}: bind -> {code:#x}");
    let size = (width * height * 4) as u64;
    check!(
        info[6] == size,
        "{width}x{height}: screen buffer {}",
        info[6]
    );
    let mut windows = Vec::new();
    for slot_index in 0..2 {
        let mut out = [0u64; 3];
        let code = process::dispatch_for_test(
            12,
            crate::display::op::CREATE_BUFFER,
            size,
            out.as_mut_ptr() as u64,
        );
        check!(
            code == 0,
            "{width}x{height}: window buffer {slot_index} -> {code:#x}"
        );
        windows.push(out[0]);
    }
    for handle in windows {
        let code = process::dispatch_for_test(12, crate::display::op::CLOSE_BUFFER, handle, 0);
        check!(code == 0, "close -> {code:#x}");
    }
    let code = process::dispatch_for_test(12, crate::display::op::UNBIND, 0, 0);
    check!(code == 0, "unbind -> {code:#x}");
    check!(
        crate::ipc::shared::process_stats(slot).bytes == 0,
        "{width}x{height}: buffers left charged"
    );
    Ok(())
}

/// 1920x1080 and 3840x2160 bind with a double-buffered window beside the
/// screen buffer; the boot geometry and limits are restored afterwards.
pub fn large_screens_fit_the_budgets() -> Result<(), String> {
    let (width, height, stride, bpp) = crate::display::geometry_for_test();
    let outcome = bind_three_surfaces(1920, 1080).and_then(|()| bind_three_surfaces(3840, 2160));
    task::harness::switch_current(task::KERNEL_TASK);
    crate::display::reset();
    task::harness::reset();
    crate::display::init(width, height, stride, bpp);
    crate::limits::init_for_machine(mem::usable_ram(), crate::display::screen_bytes());
    outcome
}
