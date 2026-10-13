//! What the browser's own commands do (the menus, the toolbar, the keys, the pages' buttons).

use xui_core::app::Ui;
use xui_core::widget::{HasText, Placeable};

use super::{Browser, Msg};
use crate::address::{self, START};
use crate::chrome::Command;
use crate::pages::{Command as PageCommand, ABOUT, DOWNLOADS, HISTORY};

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
            Command::Reload => self.reload(),
            Command::Stop => {
                self.engine.stop();
                self.set_status("Stopped");
            }
            Command::Home => self.open(START),
            Command::ShowHistory => self.open(HISTORY),
            Command::ClearHistory => self.clear_history(),
            Command::ShowDownloads => self.open(DOWNLOADS),
            Command::About => self.open(ABOUT),
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
