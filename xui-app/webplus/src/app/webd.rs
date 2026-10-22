//! The window's side of its engine in webd: when to read the view's news (`Host::pump`), and what
//! the ad blocker's switches tell webd.
//!
//! The view's event channel wakes the window by itself (the platform parks the loop on it), so the
//! timers here only cover what has no wake of its own: a pointer move waiting its turn and a view
//! webd is asked for again ([`FAST_MS`] while the engine says so), and a slow beat that keeps the
//! rest honest ([`BEAT_MS`]).

use xui_core::app::Ui;

use super::{Browser, Msg};

/// The fast timer while the engine wants to be called soon (a pointer move's turn).
const FAST_MS: u32 = 8;
/// The slow beat: messages webd's full inbox refused are sent again, a lost view is retried.
const BEAT_MS: u32 = 250;
/// Sites webd takes as exceptions to content blocking (`idl/webd.midl`): the person's first ones
/// when they have more.
const EXCEPTIONS_MAX: usize = 64;

impl Browser {
    /// Every window timer is the engine's; the beat starts now, and webd learns the blocker's
    /// switches as they stand.
    pub(super) fn start_engine(&self, ui: &mut Ui<Msg>) {
        ui.on_timer(|_| Some(Msg::Engine));
        ui.set_timer(BEAT_MS);
        self.sync_blocking();
    }

    /// Reads the engine's news; keeps the fast timer only while the engine asks for it.
    pub(super) fn pump(&self, ui: &Ui<Msg>) {
        let again = (self.host.pump)();
        match (again, self.fast.get()) {
            (true, None) => self.fast.set(Some(ui.set_timer(FAST_MS))),
            (false, Some(timer)) => {
                ui.kill_timer(timer);
                self.fast.set(None);
            }
            _ => {}
        }
    }

    /// Tells webd the blocker's switch and the sites it is off for: the person's exceptions, and
    /// the site shown "without blocking" once until the browser leaves it.
    pub(super) fn sync_blocking(&self) {
        let settings = self.blocker.settings();
        let mut sites: Vec<String> = settings.exceptions.into_iter().collect();
        if let Some(once) = self.blocker.once() {
            if !sites.contains(&once) {
                sites.insert(0, once);
            }
        }
        sites.truncate(EXCEPTIONS_MAX);
        self.engine.set_blocking(settings.enabled, &sites);
    }
}
