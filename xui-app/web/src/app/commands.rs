//! What links, page commands and menu commands do to the window (a child of
//! `app`, so it reaches the browser's own state).

use lazyweb::address::{self, START};
use lazyweb::pages::{Command as PageCommand, ABOUT, DOWNLOADS, HISTORY};
use xui_app::platform::launcher;
use xui_core::app::Ui;
use xui_core::widget::HasText;

use super::{Browser, Msg};
use crate::chrome::Command;
use crate::handoff;

impl Browser {
    /// A link the view cannot follow: one of our pages' commands, or a URL
    /// for the app registered for its scheme (`mailto:` opens Mail).
    pub(super) fn launch(&mut self, url: &str, by_user: bool) {
        if let Some(command) = PageCommand::parse(url) {
            // Only our own pages may ask: a web page linking to a command
            // gets nothing.
            if self.internal.name_for(&self.url).is_some() {
                self.page_command(command);
            }
            return;
        }
        let status = handoff::open(url, by_user);
        self.set_status(&status);
    }

    pub(super) fn page_command(&mut self, command: PageCommand) {
        match command {
            PageCommand::ClearHistory => self.clear_history(),
            PageCommand::OpenDownload(n) => {
                let opened = self
                    .transfers
                    .saved(n)
                    .map(|path| path.display().to_string())
                    .map(|path| (launcher::open_path(&path), path));
                match opened {
                    Some((Ok(()), path)) => self.set_status(&format!("Opened {path}")),
                    Some((Err(e), path)) => self.set_status(&format!("Cannot open {path}: {e}")),
                    None => {}
                }
            }
            PageCommand::CancelDownload(n) => {
                if let Some(id) = self.transfers.running(n) {
                    self.page.view().cancel_download(id);
                }
            }
        }
    }

    pub(super) fn clear_history(&mut self) {
        match self.visits.clear() {
            Ok(()) => self.set_status("History cleared"),
            Err(e) => self.set_status(&format!("History not cleared: {e}")),
        }
        if self.showing(HISTORY) {
            self.open(HISTORY);
        }
    }

    pub(super) fn command(&mut self, command: Command, ui: &Ui<Msg>) {
        match command {
            Command::OpenLocation => self.focus_address(),
            Command::SavePage => {
                if address::is_network(&self.url) {
                    self.page.view().download(&self.url);
                } else {
                    self.set_status("Only pages from the web can be saved");
                }
            }
            Command::Close => ui.close(),
            Command::Back => {
                if let Some(url) = self.history.back() {
                    self.open(&url);
                }
            }
            Command::Forward => {
                if let Some(url) = self.history.forward() {
                    self.open(&url);
                }
            }
            Command::Reload => {
                if let Some(url) = self.history.reload() {
                    self.open(&url);
                }
            }
            Command::Stop => {
                self.page.view().stop();
                self.set_status("Stopped");
            }
            Command::Home => self.open(START),
            Command::ShowHistory => self.open(HISTORY),
            Command::ClearHistory => self.clear_history(),
            Command::ShowDownloads => self.open(DOWNLOADS),
            Command::About => self.open(ABOUT),
            Command::OpenLink => {
                if let Some(link) = self.context.link().map(str::to_string) {
                    // As a click does: a page command is run, not navigated
                    // to; everything else is opened, and the view hands a
                    // scheme it cannot show to `launch` itself.
                    if PageCommand::parse(&link).is_some() {
                        self.launch(&link, true);
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
            Command::SaveImage => {
                if let Some(image) = self.context.image().map(str::to_string) {
                    self.set_status(&format!("Saving {image}"));
                    self.page.view().download(&image);
                }
            }
        }
    }

    pub(super) fn focus_address(&mut self) {
        self.address.set_text("");
        self.address.focus();
        self.editing = true;
    }
}
