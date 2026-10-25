//! Bringing the last session back after a crash (`crate::session` keeps the files, `crate::prefs`
//! the choice): at start the window checks for the `running` marker a clean quit removes, offers the
//! saved tabs in a bar above the page (or opens them, when the person set `session/restore` to
//! `always`), and saves the tabs after each change. The bar is the browser's own chrome, like the
//! "not responding" one (`hang.rs`).
//!
//! Restored tabs are discarded until focused (`crate::tabs`): only the one that was in front
//! loads, so a start never loads twenty pages, and a page that crashed the last run does not crash
//! this one before it is asked for.
//!
//! Serial evidence: `LAZYWEB:SESSION:RESTORE:ASK|YES|NO|AUTO` (no address in any of them).

use std::path::PathBuf;
use std::rc::Rc;

use xui_core::app::Ui;
use xui_core::geometry::Rect;
use xui_core::widget::{Button, GroupBox, HasText, Label};

use super::{Browser, Msg};
use crate::chrome::Widgets;
use crate::prefs::Restore;
use crate::session::{self, Session, Throttle, Window};

/// The bar's widgets, what it offers, and the session being kept.
pub(super) struct State {
    frame: Rc<GroupBox<Msg>>,
    yes: Rc<Button<Msg>>,
    no: Rc<Button<Msg>>,
    text: Rc<Label<Msg>>,
    /// The tabs the bar offers.
    offer: Option<Window>,
    /// The profile folder while the session is kept (not in "page data in memory" mode, not without
    /// a home).
    profile: Option<PathBuf>,
    throttle: Throttle<Session>,
}

impl State {
    pub(super) fn new(widgets: &Widgets) -> State {
        State {
            frame: widgets.restore_bar.get(),
            yes: widgets.restore_yes.get(),
            no: widgets.restore_no.get(),
            text: widgets.restore_text.get(),
            offer: None,
            profile: None,
            throttle: Throttle::default(),
        }
    }

    pub(super) fn visible(&self) -> bool {
        self.offer.is_some()
    }

    /// Where the bar and its buttons are, by the names a session script clicks.
    pub(super) fn controls(&self, ui: &Ui<Msg>) -> Vec<(&'static str, Rect)> {
        vec![
            ("restore_bar", ui.bounds(self.frame.id())),
            ("restore_yes", ui.bounds(self.yes.id())),
            ("restore_no", ui.bounds(self.no.id())),
        ]
    }
}

impl Browser {
    /// Starts keeping the session: writes the `running` marker and finds out whether the last run
    /// left it behind. Returns the tabs to open at once, when the person asked for that.
    pub(super) fn begin_session(&mut self, explicit_address: bool) -> Option<Window> {
        let profile = self
            .host
            .profile
            .clone()
            .filter(|dir| !crate::memory::enabled(dir))?;
        let crashed = session::begin(&profile);
        self.restore.profile = Some(profile.clone());
        if !crashed {
            return None;
        }
        let window = session::load(&profile)?.windows.into_iter().next()?;
        // An address given on the command line (a link opened from another app) is the person's
        // choice of the moment: it is not replaced silently.
        if self.host.prefs.restore == Restore::Always && !explicit_address {
            crate::marker("SESSION:RESTORE:AUTO");
            return Some(window);
        }
        crate::marker("SESSION:RESTORE:ASK");
        let text = match window.tabs.len() {
            1 => String::from("LazyWeb did not close properly. Restore the last page?"),
            n => format!("LazyWeb did not close properly. Restore the {n} tabs from last time?"),
        };
        self.restore.text.set_text(&text);
        self.restore.offer = Some(window);
        None
    }

    /// Puts the bar up or away to match what it offers.
    pub(super) fn show_restore_bar(&self, ui: &Ui<Msg>) {
        ui.set_visible(self.restore.frame.id(), self.restore.visible());
        ui.relayout();
        self.announce(ui);
    }

    /// The person answered the bar.
    pub(super) fn restore_answer(&mut self, ui: &Ui<Msg>, yes: bool) {
        crate::marker(if yes {
            "SESSION:RESTORE:YES"
        } else {
            "SESSION:RESTORE:NO"
        });
        if let Some(window) = self.restore.offer.take() {
            if yes {
                self.replace_tabs(ui, &window);
            }
        }
        self.show_restore_bar(ui);
    }

    /// The tabs as they stand, kept for the next start: at most once a second. Only web pages are
    /// kept (the browser's own `data:` pages and files are not), and only while a profile folder
    /// keeps the session.
    pub(super) fn save_tabs(&mut self) {
        if self.restore.profile.is_none() {
            return;
        }
        let session = self.tabs.model.to_session();
        if session.windows.is_empty() {
            return;
        }
        let now = self.started.elapsed().as_millis() as u64;
        if let Some(session) = self.restore.throttle.note(session, now) {
            self.write_session(session);
        }
    }

    /// Writes the session that waited for its second to pass.
    pub(super) fn flush_session(&mut self) {
        let now = self.started.elapsed().as_millis() as u64;
        if let Some(session) = self.restore.throttle.due(now) {
            self.write_session(session);
        }
    }

    /// Queues the write on the disk thread (`crate::disk`): it syncs twice and renames, which on
    /// LazyOS can wait on the volume for hundreds of milliseconds. A newer session replaces one
    /// still waiting.
    fn write_session(&self, kept: Session) {
        let Some(profile) = self.restore.profile.clone() else {
            return;
        };
        crate::disk::global().replace("session", move || {
            if let Err(error) = session::save(&profile, &kept) {
                crate::marker(&format!("SESSION:SAVE:FAIL:{:?}", error.kind()));
            }
        });
    }
}
