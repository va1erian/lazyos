//! What the engine's news does to the window: the page's state (address, title, padlock, load
//! start and end) and the events (the link under the pointer, downloads, dialogs, requests for
//! a new window, links for other apps).

use xui_core::app::Ui;
use xui_core::widget::HasText;
use xui_webkit::events::Event;

use super::{caption, now, Browser, Msg};
use crate::address;
use crate::dialogs;
use crate::handoff;
use crate::indicators::Security;
use crate::marker_text;
use crate::pages::{Command as PageCommand, DOWNLOADS};
use crate::transfers::Started;

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
            self.page.clear();
        }
        if self.state.committed_url != old.committed_url {
            // The page that asked is gone: its dialog is answered "no".
            if let Some(answer) = self.dialogs.dismiss() {
                self.engine
                    .dialog_reply(answer.id, answer.accepted, &answer.value);
            }
        }
        self.load_state(&old, &shown);
        self.update_buttons();
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
                println!("WEBPLUS:STOPPED");
            }
        } else if self.state.title != old.title {
            self.title_changed();
        }
    }

    fn load_finished(&mut self) {
        // "Opening <url>" must not outlive the load; hovering a link shows its address again.
        self.set_status("");
        let url = self.state.committed_url.clone();
        let shown = self.shown_url(&url);
        self.trace(&format!("LOAD:{}", marker_text(&shown)));
        self.trace(&format!("TITLE:{}", marker_text(&self.state.title)));
        self.record_visit();
    }

    fn load_failed(&mut self) {
        let error = self.state.error.clone();
        if self.host.traces {
            println!("WEBPLUS:FAIL:{}", marker_text(&error));
        } else {
            println!("WEBPLUS:FAIL");
        }
        self.set_status(&error);
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
            eprintln!("lazywebplus: history not saved: {e}");
        }
        self.recorded = Some((url, title));
    }

    /// Applies everything the engine reported since the last time.
    pub(super) fn show_events(&mut self, ui: &Ui<Msg>) {
        // The state first: a command or a download refers to the page that is on show now.
        self.show_state(ui);
        for event in self.mailbox.take_events() {
            match event {
                Event::Hover(url) => {
                    self.hover = url;
                    self.refresh_status();
                }
                Event::Cursor(kind) => self.page.set_cursor(kind),
                Event::Launch { url, by_user } => {
                    let status = handoff::open(self.host.launcher.as_ref(), &url, by_user);
                    self.set_status(&status);
                }
                Event::Command { url } => self.run_command(&url),
                Event::Popup { url } => self.popup(&url),
                Event::DownloadStarted {
                    id,
                    url,
                    name,
                    total,
                    ..
                } => {
                    let status = self.transfers.started(Started {
                        id,
                        url,
                        name,
                        total,
                    });
                    self.set_status(&status);
                    self.downloads_changed(ui);
                }
                Event::DownloadProgress {
                    id,
                    received,
                    total,
                } => {
                    self.transfers.progress(id, received, total);
                    self.update_downloads(ui);
                }
                Event::DownloadDone { id } => self.download_ended(ui, id, None),
                Event::DownloadFailed { id, reason } => {
                    self.download_ended(ui, id, Some(reason));
                }
                Event::Dialog { id, kind, text, .. } => self.dialog(id, kind, &text),
            }
        }
        self.update_buttons();
    }

    fn download_ended(&mut self, ui: &Ui<Msg>, id: u64, error: Option<String>) {
        if let Some(status) = self.transfers.finished(id, error) {
            self.set_status(&status);
        }
        self.downloads_changed(ui);
    }

    /// The download list changed: the status bar follows, and so does the downloads page when it
    /// is on show.
    fn downloads_changed(&mut self, ui: &Ui<Msg>) {
        self.update_downloads(ui);
        if self.showing(DOWNLOADS) && !self.state.loading {
            self.open(DOWNLOADS);
        }
    }

    /// A page asked for a new window with a user gesture: the one view opens it.
    fn popup(&mut self, url: &str) {
        self.trace(&format!("POPUP:{}", marker_text(url)));
        // Only what the engine would load itself: a scheme for another app earns no handoff
        // from a page's window request.
        let lower = url.trim().to_ascii_lowercase();
        if lower.starts_with("http:") || lower.starts_with("https:") || lower == "about:blank" {
            self.editing = false;
            self.start_navigation(url);
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
    fn dialog(&mut self, id: u64, kind: xui_webkit::engine::DialogKind, text: &str) {
        println!("WEBPLUS:DIALOG:{}", kind.name());
        let title = dialogs::title_for(&self.state.committed_url);
        if let Some(replaced) = self.dialogs.show(id, kind, &title, text) {
            self.engine
                .dialog_reply(replaced.id, replaced.accepted, &replaced.value);
        }
    }
}
