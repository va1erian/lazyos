//! The LazyWeb window: what the page's events, the menus, the toolbar and
//! the keyboard do to the chrome (`chrome.rs`).
//!
//! Serial evidence (the screenshot sessions wait on it): `WEB:LOAD:<url>`
//! then `WEB:TITLE:<title>` when a load finishes, `WEB:FAIL:<reason>` when a
//! page cannot be opened or fetched, `WEB:NAV:<url>` when the app starts a
//! navigation, `WEB:LAUNCH:...` (`handoff.rs`) when a link goes to another app,
//! and the download markers of `transfers.rs`.

use std::cell::Cell;
use std::rc::Rc;
use std::time::{SystemTime, UNIX_EPOCH};

use lazyweb::address::{self, START};
use lazyweb::fetch::trace;
use lazyweb::history::History;
use lazyweb::marker_text;
use lazyweb::pages::{self, Command as PageCommand, ABOUT, DOWNLOADS, HISTORY};
use lazyweb::visits::Visits;
use xui_app::backend::LazyOSBackend;
use xui_app::platform::launcher;
use xui_blitz::BlitzViewEvent;
use xui_core::app::{App, Ui};
use xui_core::backend::Result;
use xui_core::icon::Lucide;
use xui_core::widget::{Button, Edit, HasText, Label, Menu, ProgressBar};

use crate::chrome::{self, Command, Widgets};
use crate::handoff;
use crate::indicators::{Badge, Security, Throbber};
use crate::internal::Internal;
use crate::keys::shortcut;
use crate::page::Page;
use crate::transfers::Transfers;

/// The name the window has while a page has no title.
const APP_NAME: &str = "LazyWeb";

/// Everything the window reacts to.
#[derive(Clone)]
pub enum Msg {
    /// The engine drew a new frame.
    Frame,
    /// The view reports something (built on the engine's thread).
    View(BlitzViewEvent),
    Menu(Command),
    /// The toolbar's Reload button, which is Stop while a page loads.
    ReloadOrStop,
    /// Enter in the address field, or the Go button.
    Go,
    /// Ctrl+L: an empty, focused address field to type into. (`Edit` has no
    /// public select-all, so the old address is cleared instead; Escape
    /// brings it back.)
    FocusAddress,
    /// Escape in the address field.
    RestoreAddress,
    /// The user changed the address field's text.
    AddressEdited,
}

/// What the browser keeps beside its widgets.
pub struct Setup {
    pub url: Option<String>,
    pub visits: Visits,
    pub transfers: Transfers,
}

/// The window's widgets and state.
pub struct Browser {
    page: Rc<Page>,
    menu: Rc<Menu<Msg>>,
    address: Rc<Edit<Msg>>,
    back: Rc<Button<Msg>>,
    forward: Rc<Button<Msg>>,
    reload: Rc<Button<Msg>>,
    throbber: Rc<Throbber>,
    status: Rc<Label<Msg>>,
    badge: Rc<Badge>,
    download_label: Rc<Label<Msg>>,
    download_bar: Rc<ProgressBar<Msg>>,
    history: History,
    visits: Visits,
    transfers: Transfers,
    internal: Internal,
    /// The URL on show (as the view reported it) and its title.
    url: String,
    title: String,
    loading: bool,
    /// The current load failed: its end is not a `WEB:LOAD`.
    failed: bool,
    /// The user is typing an address: the page's own news (its URL as it
    /// loads, redirects) must not replace what they typed.
    editing: bool,
    /// Back, Forward and loading as last shown (`None` before the first
    /// update): setting a button repaints it, and the page's frames arrive
    /// many times a second.
    buttons: Cell<Option<[bool; 3]>>,
}

impl Browser {
    /// Builds the window, opening `setup.url` (or the start page).
    pub fn build(ui: &mut Ui<Msg>, backend: Rc<LazyOSBackend>, setup: Setup) -> Result<Browser> {
        let mut internal = Internal::default();
        let first = setup.url.unwrap_or_else(|| internal.start());
        // The first page loads without passing through `open`.
        println!("WEB:NAV:{}", marker_text(internal.shown(&first)));
        println!("WEB:TIME:{}ms:nav", trace::now_ms());

        let widgets = Widgets::default();
        ui.root(chrome::window(&widgets, first.clone()))?;

        let address = widgets.address.get();
        let field = address.id();
        ui.on_key(move |key, mods| shortcut(key, mods, backend.focused() == Some(field)));

        let mut browser = Browser::mounted(&widgets, setup.visits, setup.transfers, internal);
        browser.show_url(&first);
        browser.set_loading(true);
        let shown = browser.internal.shown(&first).to_string();
        browser.set_status(&format!("Opening {shown}"));
        browser.update_buttons();
        browser.update_downloads(ui);
        ui.set_window_title(APP_NAME);
        Ok(browser)
    }

    fn mounted(w: &Widgets, visits: Visits, transfers: Transfers, internal: Internal) -> Browser {
        Browser {
            page: w.page.get(),
            menu: w.menu.get(),
            address: w.address.get(),
            back: w.back.get(),
            forward: w.forward.get(),
            reload: w.reload.get(),
            throbber: w.throbber.get(),
            status: w.status.get(),
            badge: w.badge.get(),
            download_label: w.download_label.get(),
            download_bar: w.download_bar.get(),
            history: History::new(),
            visits,
            transfers,
            internal,
            url: String::new(),
            title: String::new(),
            loading: false,
            failed: false,
            editing: false,
            buttons: Cell::new(None),
        }
    }

    fn show_url(&mut self, url: &str) {
        self.url = url.to_string();
        self.badge.set(Security::of(url));
        if self.editing {
            return;
        }
        let text = self.internal.shown(url).to_string();
        self.address.set_text(&text);
    }

    fn set_status(&self, text: &str) {
        self.status.set_text(text);
    }

    fn set_loading(&mut self, on: bool) {
        self.loading = on;
        self.throbber.set_spinning(on);
    }

    /// Enables Back, Forward and Stop, and turns Reload into Stop while a
    /// page loads, touching them only when that changed: each change damages
    /// its button, which would otherwise add the toolbar to every frame.
    fn update_buttons(&self) {
        let state = [
            self.history.can_go_back(),
            self.history.can_go_forward(),
            self.loading,
        ];
        if self.buttons.replace(Some(state)) == Some(state) {
            return;
        }
        self.back.set_enabled(state[0]);
        self.forward.set_enabled(state[1]);
        let (icon, tip) = if self.loading {
            (Lucide::X, "Stop (Esc)")
        } else {
            (Lucide::RefreshCw, "Reload (F5)")
        };
        self.reload.set_icon(Some(icon));
        let _ = self.reload.set_tooltip(tip);
        self.menu.set_enabled(Command::Back.id(), state[0]);
        self.menu.set_enabled(Command::Forward.id(), state[1]);
        self.menu.set_enabled(Command::Stop.id(), state[2]);
    }

    /// Shows the running downloads in the status bar, or hides that part.
    fn update_downloads(&self, ui: &Ui<Msg>) {
        let summary = self.transfers.summary();
        ui.set_visible(self.download_label.id(), summary.is_some());
        let percent = summary.as_ref().and_then(|s| s.percent);
        ui.set_visible(self.download_bar.id(), percent.is_some());
        if let Some(summary) = summary {
            self.download_label.set_text(&summary.text);
            self.download_bar.set_value(percent.unwrap_or(0));
        }
    }

    /// Opens `target`: a URL, or the name of one of LazyWeb's own pages.
    fn open(&mut self, target: &str) {
        let url = match Internal::name_of(target) {
            Some(name) => self.build_page(name),
            None => target.to_string(),
        };
        let shown = self.internal.shown(&url).to_string();
        self.editing = false;
        println!("WEB:NAV:{}", marker_text(&shown));
        println!("WEB:TIME:{}ms:nav", trace::now_ms());
        self.failed = false;
        self.set_loading(true);
        self.set_status(&format!("Opening {shown}"));
        self.page.view().navigate(&url);
    }

    /// Builds one of LazyWeb's own pages as it stands now.
    fn build_page(&mut self, name: &'static str) -> String {
        let html = match name {
            HISTORY => pages::history(self.visits.entries()),
            DOWNLOADS => pages::downloads(
                &self.transfers.rows(),
                &self.transfers.folder().display().to_string(),
            ),
            ABOUT => pages::about(env!("CARGO_PKG_VERSION")),
            _ => return self.internal.start(),
        };
        self.internal.build(name, &html)
    }

    /// Opens what the address field holds.
    fn go(&mut self) {
        let text = self.address.text();
        if let Some(name) = Internal::name_of(&text) {
            self.open(name);
            return;
        }
        match address::normalize(&text) {
            Some(url) => {
                self.address.set_text(&url);
                self.open(&url);
            }
            None => self.set_status("Type an address first"),
        }
    }

    /// The page on show is one of LazyWeb's own, named `name`.
    fn showing(&self, name: &str) -> bool {
        self.internal.name_for(&self.url) == Some(name)
    }

    /// Applies what the view reported.
    fn on_view_event(&mut self, ui: &Ui<Msg>, event: BlitzViewEvent) {
        match event {
            BlitzViewEvent::TitleChanged(title) => {
                self.title = title;
                let window = if self.title.trim().is_empty() {
                    APP_NAME
                } else {
                    &self.title
                };
                ui.set_window_title(window);
            }
            BlitzViewEvent::UrlChanged(url) => {
                let shown = self.internal.shown(&url).to_string();
                self.history.on_url(&shown);
                self.show_url(&url);
            }
            BlitzViewEvent::LoadingChanged(true) => {
                self.failed = false;
                self.set_loading(true);
            }
            BlitzViewEvent::LoadingChanged(false) => self.load_ended(),
            BlitzViewEvent::StatusChanged(text) => {
                if !text.is_empty() && !self.failed {
                    self.set_status(&text);
                }
            }
            BlitzViewEvent::LaunchUrl { url, by_user } => self.launch(&url, by_user),
            BlitzViewEvent::Failed(why) => self.fail(&why),
            BlitzViewEvent::FetchFailed { url, message } => {
                let shown = self.internal.shown(&url).to_string();
                self.fail(&format!("{shown}: {message}"));
            }
            // The view only reports Ctrl+C; the app owns the clipboard.
            BlitzViewEvent::CopyRequested(text) => ui.set_clipboard_text(&text),
            // The view follows links itself (`follow_links(true)`).
            BlitzViewEvent::LinkClicked(_) => {}
            BlitzViewEvent::DownloadStarted(info) => {
                self.not_a_page(&info.url);
                let status = self.transfers.started(info);
                self.set_status(&status);
                self.downloads_changed(ui);
            }
            BlitzViewEvent::DownloadProgress { id, received } => {
                self.transfers.progress(id, received);
                self.update_downloads(ui);
            }
            BlitzViewEvent::DownloadFinished { id, error } => {
                if let Some(status) = self.transfers.finished(id, error) {
                    self.set_status(&status);
                }
                self.downloads_changed(ui);
            }
        }
        self.update_buttons();
    }

    /// The download list changed: the status bar follows, and so does the
    /// downloads page when it is on show.
    fn downloads_changed(&mut self, ui: &Ui<Msg>) {
        self.update_downloads(ui);
        if self.showing(DOWNLOADS) && !self.loading {
            let url = self.build_page(DOWNLOADS);
            self.page.view().navigate(&url);
        }
    }

    /// A load of `url` became a download: the page that started it stays,
    /// and neither the Back list nor the history keeps `url`.
    fn not_a_page(&mut self, url: &str) {
        self.history.forget(url);
        if let Err(e) = self.visits.forget_newest(url) {
            eprintln!("lazyweb: history not saved: {e}");
        }
        if self.url == url {
            if let Some(page) = self.history.current() {
                let page = self.internal.url_of(page);
                self.show_url(&page);
            }
        }
    }

    fn load_ended(&mut self) {
        self.history.on_load_end();
        self.set_loading(false);
        if self.failed {
            return;
        }
        // "Opening <url>" must not outlive the load; hovering a link shows its
        // address again.
        self.set_status("");
        let shown = self.internal.shown(&self.url).to_string();
        println!("WEB:TIME:{}ms:done", trace::now_ms());
        println!("WEB:LOAD:{}", marker_text(&shown));
        println!("WEB:TITLE:{}", marker_text(&self.title));
        if let Err(e) = self.visits.record(&self.url, &self.title, now()) {
            eprintln!("lazyweb: history not saved: {e}");
        }
    }

    fn fail(&mut self, why: &str) {
        self.failed = true;
        self.history.on_load_end();
        self.set_loading(false);
        println!("WEB:TIME:{}ms:fail", trace::now_ms());
        println!("WEB:FAIL:{}", marker_text(why));
        self.set_status(&format!("Failed: {why}"));
    }

    /// A link the view cannot follow: one of our pages' commands, or a URL
    /// for the app registered for its scheme (`mailto:` opens Mail).
    fn launch(&mut self, url: &str, by_user: bool) {
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

    fn page_command(&mut self, command: PageCommand) {
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

    fn clear_history(&mut self) {
        match self.visits.clear() {
            Ok(()) => self.set_status("History cleared"),
            Err(e) => self.set_status(&format!("History not cleared: {e}")),
        }
        if self.showing(HISTORY) {
            self.open(HISTORY);
        }
    }

    fn command(&mut self, command: Command, ui: &Ui<Msg>) {
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
        }
    }

    fn focus_address(&mut self) {
        self.address.set_text("");
        self.address.focus();
        self.editing = true;
    }
}

/// Seconds since the epoch.
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

impl App for Browser {
    type Msg = Msg;

    fn update(&mut self, msg: Msg, ui: &mut Ui<Msg>) {
        match msg {
            Msg::Frame => self.page.view().update(),
            Msg::View(event) => self.on_view_event(ui, event),
            Msg::Menu(command) => self.command(command, ui),
            Msg::ReloadOrStop => {
                let command = if self.loading {
                    Command::Stop
                } else {
                    Command::Reload
                };
                self.command(command, ui);
            }
            Msg::Go => self.go(),
            Msg::FocusAddress => self.focus_address(),
            Msg::RestoreAddress => {
                self.editing = false;
                let url = self.url.clone();
                self.show_url(&url);
            }
            Msg::AddressEdited => self.editing = true,
        }
        self.update_buttons();
    }
}
