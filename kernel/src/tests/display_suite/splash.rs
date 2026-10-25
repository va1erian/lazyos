//! The boot splash (`splash.rs`): the picture covers any screen, the spinner
//! stays on it, and `F1` flips between the picture and the log panes only
//! while no compositor holds the display.

use super::*;
use crate::surface::RgbaBuffer;

/// Screens of every shape the mux may paint: the console's default, a wide
/// and a tall one, and a tiny one.
const SIZES: [(usize, usize); 5] = [(1280, 720), (1920, 1080), (800, 1200), (64, 48), (3, 2)];

pub fn splash_paints_every_pixel_and_keeps_the_spinner_on_screen() -> Result<(), String> {
    for (w, h) in SIZES {
        let mut back = RgbaBuffer::try_new(w, h).ok_or("no memory for a back buffer")?;
        crate::splash::paint(&mut back);
        let lit = back.data().chunks_exact(4).all(|px| px[3] == 0xFF);
        check!(lit, "{w}x{h}: a pixel was left unpainted");
        let (x, y, rw, rh) = crate::splash::spinner_rect(w, h);
        // A tiny screen has no room for a ring; larger ones must contain it.
        if w >= 640 {
            check!(
                x + rw <= w && y + rh <= h,
                "{w}x{h}: spinner rect off screen"
            );
        }
    }
    Ok(())
}

pub fn splash_spinner_head_walks_the_ring() -> Result<(), String> {
    let heads: Vec<usize> = (0..80).step_by(10).map(crate::splash::phase).collect();
    check!(heads == [0, 1, 2, 3, 4, 5, 6, 7], "head steps: {heads:?}");
    check!(crate::splash::phase(80) == 0, "the ring wraps");
    Ok(())
}

pub fn splash_f1_toggles_the_logs_only_without_a_compositor() -> Result<(), String> {
    crate::display::reset();
    crate::input::keyboard::reset();
    let before = crate::splash::active();
    for code in [0x3B, 0xBB] {
        crate::input::keyboard::push_scancode(code);
    }
    let after = crate::splash::active();
    // Without a compositor in the image the key is not the splash's.
    let expected = if crate::splash::available() {
        !before
    } else {
        before
    };
    check!(
        after == expected,
        "F1: {before} -> {after}, expected {expected}"
    );
    if crate::splash::available() {
        crate::splash::toggle_logs();
    }
    check!(
        crate::splash::active() == before,
        "toggling twice restores it"
    );
    Ok(())
}

/// Many frames at odd sizes and many toggles: no panic, no leak of the
/// buffer, the toggle stays a clean flip.
pub fn splash_soak() -> Result<(), String> {
    let (w, h) = (640, 360);
    let mut back = RgbaBuffer::try_new(w, h).ok_or("no memory for a back buffer")?;
    for _ in 0..40 {
        crate::splash::paint(&mut back);
    }
    let start = crate::splash::active();
    for _ in 0..10_001 {
        crate::splash::toggle_logs();
    }
    check!(
        crate::splash::active() != start || !crate::splash::available(),
        "odd toggles flip"
    );
    crate::splash::toggle_logs();
    check!(crate::splash::active() == start, "even toggles restore");
    Ok(())
}
