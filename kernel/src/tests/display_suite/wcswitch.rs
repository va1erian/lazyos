//! A framebuffer set up by a mode switch can be write-combining
//! (`mem::fbwindow`): it is mapped with 4 KiB leaves of its own, not through
//! the physical-memory map's large pages, which `wc::map_write_combining`
//! refuses. The boot applies WC only on bare metal (`wc::apply_policy`), so
//! the test retypes the window itself, as `firmware_suite` does for the boot
//! framebuffer, and checks pixels still read back.

use super::*;
use crate::display::bochs;

/// Write and read back both far corners of the current framebuffer.
fn corners_round_trip(label: &str) -> Result<(), String> {
    let (width, height) = crate::display::size();
    let marker = crate::gfx::Color::rgb(0x65, 0x43, 0x21);
    let read = crate::console::with_framebuffer(|fb| {
        [(0, 0), (width - 1, height - 1)].map(|(x, y)| {
            fb.write_pixel(x, y, marker);
            fb.read_pixel(x, y)
        })
    })
    .ok_or("no framebuffer")?;
    for color in read {
        check!(
            (color.r, color.g, color.b) == (0x65, 0x43, 0x21),
            "{label}: pixel read back {color:?}"
        );
    }
    Ok(())
}

pub fn mode_switch_write_combining() -> Result<(), String> {
    if bochs::find().is_err() {
        return Ok(());
    }
    with_boot_mode(|_| {
        let boot = crate::console::framebuffer_span().ok_or("no boot framebuffer")?;
        for (round, (width, height)) in [(2560, 1440), (1920, 1080), (2560, 1440)]
            .into_iter()
            .enumerate()
        {
            switch_and_probe(width, height)?;
            let (base, len) =
                crate::console::current_framebuffer_span().ok_or("no current framebuffer")?;
            check!(
                base != boot.0 && (base >> 39) == (boot.0 >> 39),
                "round {round}: framebuffer at {base:#x}, not in a window beside {:#x}",
                boot.0
            );
            let pages = len.div_ceil(4096);
            let mapped = crate::mem::wc::map_write_combining(base, len)
                .map_err(|reason| format!("round {round}: {reason}"))?;
            check!(
                mapped == pages,
                "round {round}: remapped {mapped} of {pages}"
            );
            for page in [0, pages / 2, pages - 1] {
                check!(
                    crate::mem::wc::is_write_combining(base + page * 4096),
                    "round {round}: page {page} of {pages} is not write-combining"
                );
            }
            corners_round_trip(&format!("round {round} {width}x{height}"))?;
        }
        Ok(())
    })
}
