//! What one event of a page does to the window, whichever tab it came from. The front tab's page has
//! the person's hands on it; a tab behind it has not, so its pointer, clipboard, hand-offs and
//! commands are dropped, a notification request is dismissed, and the rest (downloads, dialogs,
//! windows, notifications, rule lists) is what the person must see at once. A dialog of a tab
//! behind the front one brings that tab to the front, but only when the person is free for it (no
//! dialog open, no address being typed); until then it waits in a queue, its tab marked in the
//! strip, and the page waits for an answer.
//!
//! The numbers a page gives its dialogs, downloads and notifications are per view, so the lists
//! that hold them (`dialogs`, `transfers`, `notify`) get them with the tab on top ([`wire_id`]),
//! and `Switch` sends an answer back to the tab that asked.

use xui_core::app::Ui;
use xui_webkit::engine::NotifyAnswer;
use xui_webkit::events::Event;

use super::{Browser, Msg};
use crate::address;
use crate::handoff;
use crate::notify::Incoming;
use crate::switch::wire_id;
use crate::tabs::TabId;
use crate::transfers::Started;

use super::tabs::Waiting;

impl Browser {
    /// Applies `event`, which tab `tab`'s page reported.
    pub(super) fn page_event(&mut self, ui: &Ui<Msg>, tab: TabId, event: Event) {
        let front = self.tabs.model.active_id() == Some(tab);
        match event {
            Event::Hover(url) if front => {
                self.link_hover
                    .set(crate::context::link_items(&url).is_some());
                self.tab_link.set(address::is_network(&url));
                self.hover = url;
                self.refresh_status();
            }
            Event::Cursor(kind) if front => self.page.set_cursor(kind),
            Event::Launch { url, by_user } if front => {
                let status = handoff::open(self.host.launcher.as_ref(), &url, by_user);
                self.set_status(&status);
            }
            Event::Command { url } if front => self.run_command(&url),
            Event::Clipboard(text) if front => ui.set_clipboard_text(&text),
            Event::Hover(_)
            | Event::Cursor(_)
            | Event::Launch { .. }
            | Event::Command { .. }
            | Event::Clipboard(_) => {}
            Event::Popup { url } => self.popup(ui, tab, &url),
            Event::DownloadStarted {
                id,
                url,
                name,
                total,
                ..
            } => {
                let status = self.transfers.started(Started {
                    id: wire_id(tab, id),
                    url,
                    name,
                    total,
                });
                self.tabs.downloading(tab, 1);
                self.tab_busy(tab);
                self.set_status(&status);
                self.downloads_changed(ui);
            }
            Event::DownloadProgress {
                id,
                received,
                total,
            } => {
                self.transfers.progress(wire_id(tab, id), received, total);
                self.update_downloads(ui);
            }
            Event::DownloadDone { id, name } => {
                self.download_ended(ui, wire_id(tab, id), Some(&name), None);
            }
            Event::DownloadFailed { id, reason } => {
                self.download_ended(ui, wire_id(tab, id), None, Some(reason));
            }
            Event::Dialog { id, kind, text, .. } => {
                let id = wire_id(tab, id);
                if front {
                    self.dialog(id, kind, &text);
                } else {
                    // Never a switch from here: this may run inside one (`adopt` reads the news of
                    // the tab it leaves), and a dialog open for the front tab is not to be stolen.
                    self.tabs.waiting.push_back(Waiting {
                        tab,
                        id,
                        kind,
                        text,
                    });
                    self.set_status("A page in another tab is asking something");
                    self.refresh_strip();
                }
                self.tab_busy(tab);
            }
            Event::NotifyShow {
                id,
                origin,
                page_url,
                title,
                body,
                tag,
            } => self.notify_show(&Incoming {
                id: wire_id(tab, id),
                origin: &origin,
                page_url: &page_url,
                title: &title,
                body: &body,
                tag: &tag,
            }),
            Event::NotifyCancel { id } => self.notifications.cancel(wire_id(tab, id)),
            Event::NotifyGone { id } => self.notifications.gone(wire_id(tab, id)),
            Event::NotifyPermission {
                origin,
                user_gesture,
            } if front => self.notify_request(ui, &origin, user_gesture),
            Event::NotifyPermission { origin, .. } => {
                // The bar asks about the page in front; another tab's request is refused, and the
                // site may ask again once its tab is in front.
                if let Some(engine) = self.tabs.switch.engine(tab) {
                    engine.notify_permission(&origin, NotifyAnswer::Dismiss);
                }
            }
            Event::NotifyPermissionGone { origin } if front => {
                self.notifications.request_gone(&origin);
                self.update_notify_bar(ui);
            }
            Event::NotifyPermissionGone { .. } => {}
            Event::BlockingList { id, status } => {
                self.trace(&format!("BLOCK:LIST:{id}:{}", status.name()));
                self.blocker.list_result(&id, status);
            }
            Event::PageResponsive(responsive) if front => self.page_responsive(ui, responsive),
            Event::PageResponsive(responsive) => {
                if let Some(slot) = self.tabs.slots.get_mut(&tab) {
                    slot.stuck = !responsive;
                }
            }
            Event::Blocked(count) if front => {
                // The count starts again with every commit, our own pages' included; what
                // `about:blocker` and the shield report is the web page browsed last.
                if count > 0 || address::is_network(&self.state.committed_url) {
                    self.blocker.set_blocked(count);
                    self.update_shield();
                }
            }
            Event::Blocked(_) => {}
        }
    }

    /// Shows the oldest dialog a tab behind asked, once nothing holds the person: no dialog is
    /// open and no address is being typed. Called after every message, so the moment the front
    /// dialog is answered or the address bar is left, the next one comes.
    pub(super) fn surface_dialog(&mut self, ui: &Ui<Msg>) {
        if self.dialogs.is_open() || self.editing {
            return;
        }
        let Some(next) = self.tabs.waiting.pop_front() else {
            return;
        };
        self.switch_to(ui, next.tab);
        self.dialog(next.id, next.kind, &next.text);
        self.tab_busy(next.tab);
        self.refresh_strip();
    }

    /// Whether tab `tab` has a dialog open or waiting or a download running, which keeps its view.
    pub(super) fn tab_busy(&mut self, tab: TabId) {
        let dialog = self
            .dialogs
            .open_id()
            .is_some_and(|id| crate::switch::split_id(id).0 == tab)
            || self.tabs.waiting.iter().any(|w| w.tab == tab);
        let busy = dialog || self.tabs.running_downloads(tab) > 0;
        self.tabs.model.set_busy(tab, busy);
    }
}
