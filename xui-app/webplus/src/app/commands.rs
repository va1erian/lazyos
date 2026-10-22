//! What the browser's own commands do (the menus, the toolbar, the keys, the pages' buttons).

use xui_core::app::Ui;
use xui_core::widget::{HasText, Placeable};

use super::{Browser, Msg};
use crate::address::{self, START};
use crate::blocker::site_of;
use crate::blocker::worker::Cmd as WorkerCmd;
use crate::chrome::Command;
use crate::hang::Answer as HangAnswer;
use crate::pages::{Command as PageCommand, ABOUT, BLOCKER, DOWNLOADS, HISTORY};

impl Browser {
    pub(super) fn page_command(&mut self, command: PageCommand) {
        match command {
            PageCommand::ClearHistory => self.clear_history(),
            PageCommand::OpenDownload(n) => {
                if let Some(path) = self.transfers.saved(n) {
                    let path = path.display().to_string();
                    let status = match self.host.launcher.open_path(&path) {
                        Ok(()) => format!("Opened {path}"),
                        Err(e) => format!("Cannot open {path}: {e}"),
                    };
                    self.set_status(&status);
                }
            }
            PageCommand::CancelDownload(n) => {
                if let Some(id) = self.transfers.running(n) {
                    self.engine.download_cancel(id);
                }
            }
            PageCommand::ForgetNotify(origin) => {
                let had = self.notifications.forget(self.engine.as_ref(), &origin);
                self.set_status(if had {
                    "Notification choice forgotten"
                } else {
                    "No notification choice for that site"
                });
                if self.showing(ABOUT) {
                    self.open(ABOUT);
                }
            }
            PageCommand::BlockerEnabled(on) => {
                self.blocker.set_enabled(on);
                if on {
                    // Nothing was fetched or loaded while it was off.
                    self.blocker.send(WorkerCmd::ListsChanged);
                }
                self.trace(&format!("BLOCK:ENABLED:{}", if on { "ON" } else { "OFF" }));
                self.blocker_page_changed();
            }
            PageCommand::Site { site, excepted } => {
                if self.blocker.set_site_excepted(&site, excepted).is_some() {
                    crate::named_marker(&format!(
                        "BLOCK:SITE:{site}:{}",
                        if excepted { "OFF" } else { "ON" }
                    ));
                }
                self.blocker_page_changed();
            }
            PageCommand::List { id, on } => {
                self.blocker.set_list_enabled(&id, on);
                self.blocker_page_changed();
            }
            PageCommand::UpdateLists => {
                self.blocker.send(WorkerCmd::UpdateNow);
                self.set_status("Updating the ad blocker's lists");
            }
            PageCommand::ReloadUnblocked => self.reload_unblocked(),
            PageCommand::OpenSite => {
                if let Some(url) = self.site_url.clone() {
                    self.open(&url);
                }
            }
        }
    }

    /// A setting of the ad blocker changed: the shield follows, and the page that shows it is built
    /// again.
    fn blocker_page_changed(&mut self) {
        self.sync_blocking();
        self.update_shield();
        if self.showing(BLOCKER) {
            self.open(BLOCKER);
        }
    }

    /// A list changed state on the worker's thread.
    pub(super) fn blocker_changed(&mut self) {
        // The stored settings may have arrived (`Blocker::attach`): webd follows them.
        self.sync_blocking();
        self.update_shield();
        if self.showing(BLOCKER) && !self.state.loading {
            self.open(BLOCKER);
        }
    }

    /// Loads the page browsed last again, once, with the blocker off for its site, until the
    /// browser leaves it.
    fn reload_unblocked(&mut self) {
        let Some(url) = self.site_url.clone() else {
            self.set_status("There is no web page to reload");
            return;
        };
        if let Some(site) = site_of(&url) {
            self.blocker.skip_once(&site);
            self.sync_blocking();
            crate::named_marker(&format!("BLOCK:RELOAD:UNBLOCKED:{site}"));
        }
        self.set_status("Reloading without ad blocking");
        self.open(&url);
    }

    /// Flips whether the page's data is kept in memory from the next start (`memory.rs`).
    fn toggle_page_data_in_memory(&mut self) {
        let Some(profile) = self.host.profile.clone() else {
            self.set_status("There is no profile folder: nothing is kept anyway");
            return;
        };
        let on = !crate::memory::enabled(&profile);
        match crate::memory::set(&profile, on) {
            Ok(()) => {
                crate::marker(&format!(
                    "PROFILE:MEMORY:SET:{}",
                    if on { "ON" } else { "OFF" }
                ));
                self.set_status(crate::memory::status(on));
            }
            Err(error) => self.set_status(&format!("Could not change it: {error}")),
        }
    }

    pub(super) fn clear_history(&mut self) {
        match self.visits.clear() {
            Ok(()) => self.set_status("History cleared"),
            Err(e) => self.set_status(&format!("History not cleared: {e}")),
        }
        self.recorded = None;
        if self.showing(HISTORY) {
            self.open(HISTORY);
        }
    }

    pub(super) fn command(&mut self, command: Command, ui: &Ui<Msg>) {
        match command {
            Command::OpenLocation => self.focus_address(),
            Command::SavePage => {
                let url = self.state.committed_url.clone();
                if address::is_network(&url) {
                    self.engine.download(&url);
                } else {
                    self.set_status("Only pages from the web can be saved");
                }
            }
            Command::Close => ui.close(),
            Command::Back => self.engine.back(),
            Command::Forward => self.engine.forward(),
            // A page that does not answer cannot reload or stop itself: end its process.
            Command::Reload if self.hang.stuck() => self.hang_answer(ui, HangAnswer::Reload),
            Command::Stop if self.hang.stuck() => self.hang_answer(ui, HangAnswer::Stop),
            Command::Reload => self.reload(),
            Command::Stop => {
                self.stopping = self.loading.get();
                self.engine.stop();
                self.set_status("Stopped");
            }
            Command::Home => self.open(START),
            Command::ShowHistory => self.open(HISTORY),
            Command::ClearHistory => self.clear_history(),
            Command::ShowDownloads => self.open(DOWNLOADS),
            Command::About => self.open(ABOUT),
            Command::Blocker => self.open(BLOCKER),
            Command::ReloadUnblocked => self.reload_unblocked(),
            Command::PageDataInMemory => self.toggle_page_data_in_memory(),
            Command::OpenLink => {
                if let Some(link) = self.context.link().map(str::to_string) {
                    // As a click does: a page command is run (from our own pages only), anything
                    // else is opened by the person who chose the menu item.
                    if PageCommand::parse(&link).is_some() {
                        self.run_command(&link);
                    } else {
                        self.open(&link);
                    }
                }
            }
            Command::CopyLink => {
                if let Some(link) = self.context.link() {
                    ui.set_clipboard_text(link);
                    self.set_status("Link address copied");
                }
            }
        }
    }

    /// Reload: the engine reloads a web page; one of ours is built again, so it is current.
    fn reload(&mut self) {
        let current = self.state.committed_url.clone();
        match self.internal.name_for(&current) {
            Some(name) => self.open(name),
            None => self.engine.reload(),
        }
    }

    pub(super) fn focus_address(&mut self) {
        self.address.set_text("");
        self.address.focus();
        self.editing = true;
    }

    /// Opens the right-click menu where the page was clicked.
    pub(super) fn show_context(&mut self, ui: &Ui<Msg>, at: (i32, i32)) {
        let link = Some(self.hover.clone()).filter(|link| !link.is_empty());
        let origin = ui.bounds(self.page.id());
        let history = (self.state.can_back, self.state.can_forward);
        self.context.show(origin, at, history, link);
    }
}
