//! M0: bind the display, paint a gradient into the screen buffer, present it.
//!
//! Proves the native display syscall surface (kernel issue #113) from an
//! ordinary static-musl `std` program: the `int 0x80` gate dispatches by task,
//! not by binary kind, so this is the same syscall a native app makes.
//!
//! The display stays bound for a few seconds after the first present so a
//! screenshot can catch the gradient before the kernel mux reclaims the screen.

use xui_app::sys::{self, DisplayInfo};

fn main() {
    let mut info = DisplayInfo::default();
    if let Err(code) = sys::display_bind(&mut info) {
        println!("XUIAPP:BIND:FAIL:{code}");
        std::process::exit(1);
    }
    let (width, height) = (info.width as usize, info.height as usize);
    if width == 0 || height == 0 || info.va == 0 || info.size as usize != width * height * 4 {
        println!("XUIAPP:BIND:FAIL:geometry");
        let _ = sys::display_unbind();
        std::process::exit(1);
    }

    // Safety: `va`/`size` describe the RGBA screen buffer `display_bind`
    // mapped into this task, and `size` was checked against the geometry.
    let pixels = unsafe { core::slice::from_raw_parts_mut(info.va as *mut u8, info.size as usize) };
    for y in 0..height {
        for x in 0..width {
            let at = (y * width + x) * 4;
            pixels[at] = (x * 255 / (width.max(2) - 1)) as u8;
            pixels[at + 1] = (y * 255 / (height.max(2) - 1)) as u8;
            pixels[at + 2] = 160;
            pixels[at + 3] = 255;
        }
    }

    if let Err(code) = sys::display_present(0, 0, width as i32, height as i32) {
        println!("XUIAPP:PRESENT:FAIL:{code}");
        let _ = sys::display_unbind();
        std::process::exit(1);
    }
    println!("XUIAPP:PRESENT:PASS");

    // Hold the display so the frame is observable, then release it. The raw
    // relative sleep is used because std's absolute clock_nanosleep deadline
    // confuses LazyOS's ABI (see `sys::sleep_millis`).
    sys::sleep_millis(30_000);
    let _ = sys::display_unbind();
    std::process::exit(0);
}
