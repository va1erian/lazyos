//! Opening things: the browser's own pages, a typed address, a navigation the window starts.

use xui_core::widget::HasText;

use crate::address;
use crate::blocker;
use crate::downgrade;
use crate::internal::Internal;
use crate::marker_text;
use crate::pages::{self, ABOUT, BLOCKER, DOWNLOADS, HISTORY};

use super::Browser;

impl Browser {
    /// Builds one of the browser's own pages as it stands now.
    pub(super) fn build_page(&mut self, name: &'static str) -> String {
        let html = match name {
            HISTORY => pages::history(self.visits.entries()),
            DOWNLOADS => pages::downloads(
                &self.transfers.rows(),
                &self.transfers.folder().display().to_string(),
            ),
            ABOUT => pages::about(
                env!("CARGO_PKG_VERSION"),
                &self.notifications.permissions().sites(),
            ),
            BLOCKER => blocker::page::blocker(&blocker::page::Report {
                settings: self.blocker.settings(),
                lists: self.blocker.lists(),
                site: self.site_url.as_deref().and_then(blocker::site_of),
                blocked: self.blocker.blocked(),
            }),
            downgrade::NAME => self.downgrade_page(),
            _ => return self.internal.start(),
        };
        self.internal.build(name, &html)
    }

    /// Opens `target`: a URL, or the name of one of the browser's own pages. A scheme the engine
    /// does not load (typed in the address bar: the user's own act) goes to the app that has it.
    pub(super) fn open(&mut self, target: &str) {
        let url = match Internal::name_of(target) {
            Some(name) => self.build_page(name),
            None => target.to_string(),
        };
        if !address::engine_loads(&url) {
            let status = crate::handoff::open(self.host.launcher.as_ref(), &url, true);
            self.set_status(&status);
            return;
        }
        self.editing = false;
        self.start_navigation(&url);
    }

    /// Tells the engine to load `url` and shows that it is opening.
    pub(super) fn start_navigation(&mut self, url: &str) {
        let shown = self.shown_url(url);
        self.trace(&format!("NAV:{}", marker_text(&shown)));
        self.stopping = false;
        self.set_loading(true);
        self.set_status(&format!("Opening {shown}"));
        if !self.editing {
            self.address.set_text(&shown);
        }
        self.engine.load(url);
    }

    /// Opens what the address field holds.
    pub(super) fn go(&mut self) {
        let text = self.address.text();
        if let Some(name) = Internal::name_of(&text) {
            self.open(name);
            return;
        }
        match address::classify(&text, &self.host.prefs.search) {
            Some(typed) => self.open_typed(typed),
            None => self.set_status("Type an address first"),
        }
    }

    /// The page on show is one of the browser's own, named `name`.
    pub(super) fn showing(&self, name: &str) -> bool {
        let url = if self.state.committed_url.is_empty() {
            &self.state.url
        } else {
            &self.state.committed_url
        };
        self.internal.name_for(url) == Some(name)
    }
}
