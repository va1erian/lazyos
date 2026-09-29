#![forbid(unsafe_code)]

//! The open-folder flash: the per-window `Flash` and its single repeating
//! timer, the visual-only state [`ExplorerWindow`] drives from `update`.
//!
//! The list lives in the window, keyed by borrowed name. Opening a folder (even
//! one already open) flags it; a ~200 ms timer prunes expired entries and
//! repaints, and stops as soon as nothing is flashing, so an idle window has no
//! timer. It is a [`Control`](xui_core::widget::Control) timer, so it never
//! displaces the app's own [`Ui::on_timer`](xui_core::app::Ui::on_timer)
//! mapping and is stopped when the window (and the control) drops.

use std::cell::Cell;
use std::ffi::OsStr;
use std::rc::Rc;

use super::{ExplorerWindow, FLASH_TICK_MS, Msg};
use crate::model::{Flash, Listing};
use crate::platform::Kind;

/// A shared view of one window's open-folder flash, for tests and diagnostics:
/// which names are flashing and whether the tick timer is running.
#[derive(Clone)]
pub struct FlashHandle {
    flash: Rc<Flash>,
    ticking: Rc<Cell<bool>>,
}

impl FlashHandle {
    /// Whether `name` is showing its open icon.
    pub fn is_flashing(&self, name: &OsStr) -> bool {
        self.flash.is_flashing(name)
    }

    /// How many names are recorded.
    pub fn len(&self) -> usize {
        self.flash.len()
    }

    /// Whether no name is recorded.
    pub fn is_empty(&self) -> bool {
        self.flash.is_empty()
    }

    /// Whether the repeating tick timer is running.
    pub fn timer_running(&self) -> bool {
        self.ticking.get()
    }
}

impl ExplorerWindow {
    /// The open-folder flash, shared so a test can read the flashing names and
    /// the tick timer after the app is built.
    pub fn flash_handle(&self) -> FlashHandle {
        FlashHandle {
            flash: Rc::clone(&self.flash),
            ticking: Rc::clone(&self.ticking),
        }
    }

    /// Flags `name` open for two seconds and repaints, starting the tick timer
    /// if it is not already running.
    pub(super) fn flash_name(&mut self, name: &OsStr) {
        if self.flash.flash(name) {
            self.start_flash_timer();
        }
        self.view.invalidate();
    }

    /// Drops expired flashes and any whose folder is no longer a directory in
    /// `listing`, stopping the tick timer when nothing is left to flash. Called
    /// after a refresh, so a folder that was deleted or renamed stops flashing.
    pub(super) fn prune_flash(&mut self, listing: &Listing) {
        let remaining = self.flash.retain(|name| {
            listing
                .index_of(name)
                .is_some_and(|index| listing.entries[index].kind == Kind::Dir)
        });
        if remaining == 0 {
            self.stop_flash_timer();
        }
    }

    /// One tick of the flash timer: forget what expired and repaint. An idle
    /// window has no timer, so this only runs while something is flashing.
    pub(super) fn flash_tick(&mut self) {
        if self.flash.prune() == 0 {
            self.stop_flash_timer();
        }
        self.view.invalidate();
    }

    /// Starts the repeating tick, unless one is already running. The `ticking`
    /// flag is the single source of truth, so several folders flashing within
    /// the two seconds never start a second timer.
    fn start_flash_timer(&mut self) {
        if self.ticking.get() {
            return;
        }
        self.ticking.set(true);
        if let Some(id) = self.timer.set_timer(FLASH_TICK_MS, || Some(Msg::FlashTick)) {
            self.timer_id.set(Some(id));
        }
    }

    /// Stops the tick timer if it is running.
    fn stop_flash_timer(&mut self) {
        if !self.ticking.replace(false) {
            return;
        }
        if let Some(id) = self.timer_id.take() {
            self.timer.kill_timer(id);
        }
    }
}
