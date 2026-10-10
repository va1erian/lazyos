//! Apply `display.*` from `lazyos.cfg`: switch the mode, then move every
//! screen-sized thing to it (docs/hidpi-plan.md, D1).
//!
//! Runs while `fs::init` reads the boot volume, before any task can bind the
//! display, so the console, the grant geometry, the derived limits and (in
//! `kernel_main`) the mouse bounds all follow the new mode. A refused mode
//! keeps the firmware's: the boot never depends on the switch.

use super::bochs::{self, ModeError};
use super::logical;
use super::modecfg::{self, Problem};
use super::screen;
use crate::{console, limits, mem};

/// Apply the `display.*` lines of a config text. Every outcome is logged.
pub fn apply_config(text: &str) {
    let cfg = modecfg::parse(text, |problem| match problem {
        Problem::Unknown(key) => serial_println!("display: unknown key display.{key} ignored"),
        Problem::Malformed(key) => serial_println!("display: bad value for display.{key} ignored"),
        Problem::Duplicate(key) => {
            serial_println!("display: display.{key} repeated, the first value wins")
        }
    });
    // `display.max` before `display.mode`: a cap re-fits the mode firmware
    // chose, and a `display.mode` request that follows exposes its mode whole
    // ([`switch_to`] -> `screen::init_requested`), so the separate orderings
    // compose rather than fight (issue #717).
    if let Some((width, height)) = cfg.max {
        match apply_max(width, height) {
            Ok(()) => serial_println!("display: max {width}x{height}"),
            Err(error) => {
                let (w, h) = logical::cap();
                serial_println!("display: max {width}x{height} refused ({error}), {w}x{h} kept")
            }
        }
    }
    if let Some((width, height)) = cfg.mode {
        match switch_to(width, height) {
            Ok(()) => serial_println!("display: mode {width}x{height}"),
            Err(error) => {
                let (w, h) = super::size();
                serial_println!("display: mode {width}x{height} refused ({error}), {w}x{h} kept");
            }
        }
    }
    let (width, height) = super::size();
    let scale = cfg.scale.resolve(width as u32, height as u32);
    console::set_scale(scale as usize);
    serial_println!("display: console scale {scale}");
}

/// Set the logical screen cap (`display.max`) and re-fit it over the mode
/// the firmware chose. Refused while a compositor holds the display: its
/// screen buffer was sized for the old logical screen. A request `lazyos.cfg`
/// re-reads at a later boot stage is a no-op when the cap is already in force.
fn apply_max(width: u32, height: u32) -> Result<(), ModeError> {
    if super::bound() {
        return Err(ModeError::NotApplied);
    }
    if logical::cap() == (width as usize, height as usize) {
        return Ok(());
    }
    logical::set_cap(width, height);
    screen::refit();
    limits::init_for_machine(mem::usable_ram(), super::screen_bytes());
    Ok(())
}

/// Switch the adapter to `width x height` and re-derive everything sized
/// from the screen. Refused while a compositor holds the display: its
/// screen buffer was sized for the old mode.
pub fn switch_to(width: u32, height: u32) -> Result<(), ModeError> {
    if super::bound() {
        return Err(ModeError::NotApplied);
    }
    if super::size() == (width as usize, height as usize) {
        return Ok(());
    }
    let adapter = bochs::find()?;
    let info = console::switch_mode(|| {
        adapter
            .set_mode(width, height)
            .map(|mode| (mode.base, mode.info))
    })?;
    if let Some((base, len)) = console::current_framebuffer_span() {
        mem::wc::apply_policy(base, len);
    }
    super::init_requested(info.width, info.height, info.stride, info.bytes_per_pixel);
    limits::init_for_machine(mem::usable_ram(), super::screen_bytes());
    Ok(())
}
