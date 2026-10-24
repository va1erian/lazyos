//! The news of the tabs: what the chrome shows when a tab comes to the front (its page as it was
//! left), the strip's items, and what a tab behind the front one tells the window. Its page state
//! is kept (title, address, spinner, a lost view) and its downloads, dialogs and windows are the
//! person's business at once; its pointer, clipboard, hand-offs and commands are not (only the page
//! in front has the person's hands on it).

use xui_core::app::Ui;
use xui_core::widget::HasText;
use xui_webkit::mailbox::PageState;

use super::events::lost_kind;
use super::{caption, now, Browser, Msg};
use crate::address;
use crate::indicators::Security;
use crate::strip::Item;
use crate::tabs::{process_key, Tab, TabId};

/// The title a tab has in the strip: the page's, else the host it is on, else "New Tab".
fn title_of(tab: &Tab) -> String {
    if !tab.title.trim().is_empty() {
        return tab.title.trim().to_owned();
    }
    let host = process_key(&tab.url);
    if host.is_empty() {
        String::from("New Tab")
    } else {
        host
    }
}

/// The address of the page `state` describes: the committed one, else the one being loaded.
pub(super) fn page_of(state: &PageState) -> &str {
    if state.committed_url.is_empty() {
        &state.url
    } else {
        &state.committed_url
    }
}

impl Browser {
    /// The chrome shows the page of the tab that just came to the front, as that tab left it: the
    /// address, the padlock, the title, the spinner, the buttons and the lost-view page.
    pub(super) fn show_tab(&mut self, ui: &Ui<Msg>, state: PageState) {
        self.state = state;
        self.hover.clear();
        self.link_hover.set(false);
        self.tab_link.set(false);
        self.editing = false;
        self.stopping = false;
        self.pending_https = None;
        self.offered = None;
        self.recorded = None;
        let shown = self.shown_url(&self.state.url);
        self.address.set_text(&shown);
        let on_screen = page_of(&self.state).to_owned();
        self.badge
            .set(Security::of_page(&on_screen, self.state.secure));
        if address::is_network(&on_screen) {
            self.site_url = Some(on_screen);
        }
        let caption = caption(&self.state.title);
        if caption != self.caption {
            ui.set_window_title(&caption);
            self.caption = caption;
        }
        self.set_loading(self.state.loading);
        self.set_status("");
        self.buttons.set(None);
        self.update_buttons();
        self.blocker.set_blocked(0);
        self.update_shield();
        let why = self.state.crashed.then(|| lost_kind(&self.state.error));
        self.set_crashed_page(ui, why);
        self.show_hang_bar(ui);
        self.update_notify_bar(ui);
        self.announce(ui);
    }

    /// The strip shows the tabs as the model has them.
    pub(super) fn refresh_strip(&self) {
        let items = self
            .tabs
            .model
            .iter()
            .map(|tab| Item {
                id: tab.id,
                title: title_of(tab),
                loading: tab.loading,
                crashed: tab.crashed,
                discarded: tab.discarded,
            })
            .collect();
        self.tabs.strip.set(items, self.tabs.model.active_id());
    }

    /// The front tab's page changed: the model follows, and the session when its address or title
    /// did.
    pub(super) fn front_page_changed(&mut self, changed_text: bool) {
        let Some(front) = self.tabs.model.active_id() else {
            return;
        };
        let url = page_of(&self.state).to_owned();
        let title = self.state.title.clone();
        self.tabs.model.note(front, &url, &title);
        self.tabs.model.set_loading(front, self.state.loading);
        self.tabs.model.set_crashed(front, self.state.crashed);
        self.refresh_strip();
        if changed_text {
            self.save_tabs();
        }
    }

    /// A tab behind the front one has news.
    pub(super) fn background_news(&mut self, ui: &Ui<Msg>, id: TabId) {
        if self.tabs.model.active_id() == Some(id) {
            // The tab came to the front after the notice was sent.
            self.show_events(ui);
            return;
        }
        let Some(slot) = self.tabs.slots.get(&id) else {
            return;
        };
        let mailbox = std::sync::Arc::clone(&slot.mailbox);
        // Pixels of a page nobody looks at: a whole frame is asked for when it comes to the front.
        drop(mailbox.take_update(Vec::new()));
        if let Some(state) = mailbox.take_state() {
            self.background_state(id, state);
        }
        for event in mailbox.take_events() {
            self.page_event(ui, id, event);
        }
        self.refresh_strip();
    }

    fn background_state(&mut self, id: TabId, state: PageState) {
        let Some(slot) = self.tabs.slots.get_mut(&id) else {
            return;
        };
        let old = std::mem::replace(&mut slot.state, state.clone());
        let url = page_of(&state).to_owned();
        self.tabs.model.note(id, &url, &state.title);
        self.tabs.model.set_loading(id, state.loading);
        self.tabs.model.set_crashed(id, state.crashed);
        if state.crashed && !old.crashed {
            self.note_engine(&format!("view lost: {}", lost_kind(&state.error)));
        }
        // A page loaded behind the front one is history all the same.
        if state.finished > old.finished
            && state.error.is_empty()
            && address::is_network(&state.committed_url)
        {
            let _ = self
                .visits
                .record(&state.committed_url, &state.title, now());
        }
        if page_of(&old) != url || old.title != state.title {
            self.save_tabs();
        }
    }
}
