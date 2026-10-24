//! What the engine's news does to the window: the page's state (address, title, padlock, load
//! start and end) and the events (the link under the pointer, downloads, dialogs, requests for
//! a new window, links for other apps).

use xui_core::app::Ui;
use xui_core::widget::HasText;

use super::{caption, now, Browser, Msg};
use crate::blocker;
use crate::dialogs;
use crate::indicators::Security;
use crate::marker_text;
use crate::pages::{Command as PageCommand, DOWNLOADS};
use crate::switch::split_id;
use crate::tabs::{Place, TabId};

/// The kind of a lost view, read from the engine's sentence: one of a few fixed words, never any of
/// its text.
pub(super) fn lost_kind(error: &str) -> &'static str {
    let lower = error.to_ascii_lowercase();
    if lower.contains("memory") {
        "out-of-memory"
    } else if lower.contains("crash") {
        "crashed"
    } else if lower.contains("webd") || lower.contains("reconnect") {
        "service-gone"
    } else if lower.contains("ended") {
        "ended"
    } else {
        "other"
    }
}

impl Browser {
    /// Applies the page state the engine last posted.
    pub(super) fn show_state(&mut self, ui: &Ui<Msg>) {
        let Some(state) = self.mailbox.take_state() else {
            return;
        };
        let old = std::mem::replace(&mut self.state, state);
        let url = self.state.url.clone();
        let shown = self.shown_url(&url);
        if !self.editing && url != old.url {
            self.address.set_text(&shown);
        }
        let on_screen = if self.state.committed_url.is_empty() {
            &self.state.url
        } else {
            &self.state.committed_url
        };
        self.badge
            .set(Security::of_page(on_screen, self.state.secure));

        let caption = caption(&self.state.title);
        if caption != self.caption {
            ui.set_window_title(&caption);
            self.caption = caption;
            self.announce(ui);
        }

        if self.state.crashed && !old.crashed {
            // Only a fixed word goes in the list a report may carry: the error is the engine's
            // sentence, which can hold an address.
            let kind = lost_kind(&self.state.error);
            self.note_engine(&format!("view lost: {kind}"));
            self.page.clear();
            self.notifications.page_crashed();
        }
        if self.state.crashed != old.crashed {
            let why = self.state.crashed.then(|| lost_kind(&self.state.error));
            self.set_crashed_page(ui, why);
        }
        // Another document took the view: the engine counts commits, so a reload of the same address
        // counts and a script's pushState or a link to an anchor (the same document) does not, however
        // many steps of a fast navigation one post carries.
        if self.state.commits != old.commits {
            self.page.page_committed();
            self.page.forget_bands();
        }
        if self.state.committed_url != old.committed_url {
            // The new page owns the view from here, long before its load ends (`LOAD`): what the
            // wheel turns and the frames draw are its own.
            if !self.state.committed_url.is_empty() {
                let committed = self.shown_url(&self.state.committed_url);
                self.trace(&format!("COMMIT:{}", marker_text(&committed)));
            }
            self.page.forget_bands();
            // Another page took over: a later failure is not the typed address's. A report that
            // also carries an error is the typed load failing, whose offer `load_failed` makes.
            if self.state.error.is_empty() {
                self.pending_https = None;
            }
            // The page browsed last, and the one-shot "without blocking" ends with its site.
            let committed = self.state.committed_url.clone();
            let site = blocker::site_of(&committed);
            if site.is_some() {
                self.site_url = Some(committed);
            }
            let once = self.blocker.once();
            self.blocker.leave_unless(site.as_deref());
            if self.blocker.once() != once {
                self.sync_blocking();
            }
            // The page that asked is gone: its dialog is answered "no", and a site that asked to
            // notify and was not answered may ask again.
            // (A dialog another tab asked stays: that page has not moved.)
            let front = self.tabs.model.active_id();
            if self.dialogs.open_id().map(|id| split_id(id).0) == front {
                if let Some(answer) = self.dialogs.dismiss() {
                    self.engine
                        .dialog_reply(answer.id, answer.accepted, &answer.value);
                }
            }
            self.notifications
                .page_moved(self.engine.as_ref(), &self.state.committed_url);
            self.update_notify_bar(ui);
        }
        self.load_state(&old, &shown);
        self.front_page_changed(old.url != self.state.url || old.title != self.state.title);
        self.update_buttons();
        self.update_shield();
    }

    /// A load began, went on, or ended. The engine posts its newest state only (several load
    /// steps can be one report), so how a load ended is read from what changed: `finished` grew
    /// (it loaded), an error appeared (it failed), or the window was waiting and the engine is
    /// idle with neither (it was stopped, or a download took the page's place).
    fn load_state(&mut self, old: &xui_webkit::mailbox::PageState, shown: &str) {
        let was_loading = self.loading.get();
        let loaded = self.state.finished > old.finished;
        let failed =
            !self.state.loading && !self.state.error.is_empty() && self.state.error != old.error;
        if self.state.loading {
            if !was_loading || self.state.url != old.url {
                self.set_status(&format!("Opening {shown}"));
            }
            self.set_loading(true);
        } else if loaded {
            self.set_loading(false);
            self.load_finished();
        } else if failed {
            self.set_loading(false);
            self.load_failed();
        } else if was_loading {
            self.set_loading(false);
            self.set_status("");
            if std::mem::take(&mut self.stopping) {
                crate::marker("STOPPED");
            }
        } else if self.state.title != old.title {
            self.title_changed();
        }
    }

    fn load_finished(&mut self) {
        // "Opening <url>" must not outlive the load; hovering a link shows its address again.
        self.set_status("");
        self.pending_https = None;
        let url = self.state.committed_url.clone();
        let shown = self.shown_url(&url);
        self.trace(&format!("LOAD:{}", marker_text(&shown)));
        self.trace(&format!("TITLE:{}", marker_text(&self.state.title)));
        self.record_visit();
    }

    fn load_failed(&mut self) {
        let error = self.state.error.clone();
        if self.host.traces {
            crate::marker(&format!("FAIL:{}", marker_text(&error)));
        } else {
            crate::marker("FAIL");
        }
        self.set_status(&error);
        self.offer_http(&error);
    }

    /// The title changed after the load ended (a script set it): say so and keep the history's.
    fn title_changed(&mut self) {
        if self.state.error.is_empty() && !self.state.committed_url.is_empty() {
            self.trace(&format!("TITLE:{}", marker_text(&self.state.title)));
            self.record_visit();
        }
    }

    /// Writes the page on show into the browsing history (or its new title).
    fn record_visit(&mut self) {
        let url = self.state.committed_url.clone();
        let title = self.state.title.clone();
        if self.recorded.as_ref() == Some(&(url.clone(), title.clone())) {
            return;
        }
        if let Err(e) = self.visits.record(&url, &title, now()) {
            eprintln!("lazyweb: history not saved: {e}");
        }
        self.save_tabs();
        self.recorded = Some((url, title));
    }

    /// Applies everything the engine reported since the last time.
    pub(super) fn show_events(&mut self, ui: &Ui<Msg>) {
        // The state first: a command or a download refers to the page that is on show now.
        self.show_state(ui);
        let tab = self.tabs.model.active_id().unwrap_or_default();
        for event in self.mailbox.take_events() {
            self.page_event(ui, tab, event);
        }
        self.update_buttons();
    }

    pub(super) fn download_ended(
        &mut self,
        ui: &Ui<Msg>,
        id: u64,
        name: Option<&str>,
        error: Option<String>,
    ) {
        let tab = split_id(id).0;
        self.tabs.downloading(tab, -1);
        self.tab_busy(tab);
        if let Some(status) = self.transfers.finished(id, name, error) {
            self.set_status(&status);
        }
        self.downloads_changed(ui);
    }

    /// The download list changed: the status bar follows, and so does the downloads page when it
    /// is on show.
    pub(super) fn downloads_changed(&mut self, ui: &Ui<Msg>) {
        self.update_downloads(ui);
        if self.showing(DOWNLOADS) && !self.state.loading {
            self.open(DOWNLOADS);
        }
    }

    /// A page asked for a new window with a user gesture: a tab opens next to the page's own (in
    /// front when that page is).
    pub(super) fn popup(&mut self, ui: &Ui<Msg>, opener: TabId, url: &str) {
        self.trace(&format!("POPUP:{}", marker_text(url)));
        // Only what the engine would load itself: a scheme for another app earns no handoff
        // from a page's window request.
        let lower = url.trim().to_ascii_lowercase();
        if lower.starts_with("http:") || lower.starts_with("https:") || lower == "about:blank" {
            let front = self.tabs.model.active_id() == Some(opener);
            self.new_tab(ui, url, Place::After(opener), front);
        }
    }

    /// A command from one of the browser's own pages, accepted by the navigation policy.
    pub(super) fn run_command(&mut self, url: &str) {
        let current = if self.state.committed_url.is_empty() {
            &self.state.url
        } else {
            &self.state.committed_url
        };
        if self.internal.name_for(current).is_none() {
            // The page changed under the click: not one of ours any more.
            crate::named_marker(&format!("COMMAND:{}:BLOCKED", marker_text(url)));
            return;
        }
        if let Some(command) = PageCommand::parse(url) {
            self.page_command(command);
        }
    }

    /// A JavaScript dialog: drawn over the window; the page waits for the answer.
    pub(super) fn dialog(&mut self, id: u64, kind: xui_webkit::engine::DialogKind, text: &str) {
        crate::marker(&format!("DIALOG:{}", kind.name()));
        let title = dialogs::title_for(&self.state.committed_url);
        if let Some(replaced) = self.dialogs.show(id, kind, &title, text) {
            self.engine
                .dialog_reply(replaced.id, replaced.accepted, &replaced.value);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::lost_kind;

    #[test]
    fn a_lost_view_is_reported_by_a_fixed_word_whatever_the_error_says() {
        assert_eq!(lost_kind("The page's process crashed"), "crashed");
        assert_eq!(
            lost_kind("The page's process ran out of memory"),
            "out-of-memory"
        );
        assert_eq!(
            lost_kind("The web engine (webd) stopped; reconnecting"),
            "service-gone"
        );
        assert_eq!(lost_kind("The page's process ended"), "ended");
        let odd = lost_kind("Could not make a view for https://secret.example/path?token=1");
        assert_eq!(odd, "other");
    }
}
