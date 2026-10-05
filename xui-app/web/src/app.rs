//! The LazyWeb window: the toolbar, the NetSurf page and the status line,
//! and what the page's events do to them.
//!
//! Serial evidence (the screenshot sessions wait on it): `WEB:LOAD:<url>`
//! then `WEB:TITLE:<title>` when a load finishes, `WEB:FAIL:<reason>` when a
//! page cannot be opened or fetched, `WEB:NAV:<url>` when the app starts a
//! navigation.

use std::rc::Rc;

use lazyweb::address::{self, START};
use lazyweb::fetch::trace;
use lazyweb::history::History;
use lazyweb::marker_text;
use xui_app::backend::LazyOSBackend;
use xui_core::app::{App, Ui};
use xui_core::arrange::{build, button, column, edit, label, row, Handle, Layout, LayoutExt};
use xui_core::backend::Result;
use xui_core::layout::Insets;
use xui_core::widget::{Button, Edit, HasText, Label};
use xui_core::{Dip, Key};
use xui_netsurf::NetSurfViewEvent;

use crate::page::Page;

/// The built-in start page.
const START_HTML: &str = include_str!("start.html");

/// The name the window has while a page has no title.
const APP_NAME: &str = "LazyWeb";

/// The toolbar's height and its buttons' widths.
const TOOLBAR_HEIGHT: Dip = Dip(36.0);
const BACK_WIDTH: Dip = Dip(52.0);
const FORWARD_WIDTH: Dip = Dip(64.0);
const RELOAD_WIDTH: Dip = Dip(60.0);
const GO_WIDTH: Dip = Dip(40.0);
/// The status line's height.
const STATUS_HEIGHT: Dip = Dip(24.0);

/// Everything the window reacts to.
pub enum Msg {
    /// The engine has news for the view.
    Frame,
    Back,
    Forward,
    Reload,
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

/// The window's widgets and state.
pub struct Browser {
    page: Rc<Page>,
    address: Rc<Edit<Msg>>,
    status: Rc<Label<Msg>>,
    back: Rc<Button<Msg>>,
    forward: Rc<Button<Msg>>,
    reload: Rc<Button<Msg>>,
    history: History,
    /// The start page's `data:` URL, shown as [`START`].
    start_url: String,
    /// The URL on show (as the view reported it) and its title.
    url: String,
    title: String,
    /// The current load failed: its end is not a `WEB:LOAD`.
    failed: bool,
    /// The user is typing an address: the page's own news (its URL as it
    /// loads, redirects) must not replace what they typed.
    editing: bool,
}

impl Browser {
    /// Builds the window, opening `url` (or the start page).
    pub fn build(
        ui: &mut Ui<Msg>,
        backend: Rc<LazyOSBackend>,
        url: Option<String>,
    ) -> Result<Browser> {
        let start_url = address::html_data_url(START_HTML);
        let first = url.unwrap_or_else(|| start_url.clone());
        // The first page loads without passing through `open`.
        let initial = if first == start_url { START } else { &first };
        println!("WEB:NAV:{}", marker_text(initial));
        println!("WEB:TIME:{}ms:nav", trace::now_ms());

        let widgets = Widgets::default();
        let url = first.clone();
        ui.root(
            column().children((
                toolbar(&widgets).fixed(TOOLBAR_HEIGHT),
                build(move |ui| Page::new(ui, &url))
                    .bind(&widgets.page)
                    .fill(1),
                row()
                    .padding(Insets::new(Dip(8.0), Dip(3.0), Dip(8.0), Dip(0.0)))
                    .child(label("").bind(&widgets.status).fill(1))
                    .fixed(STATUS_HEIGHT),
            )),
        )?;

        let address = widgets.address.get();
        let field = address.id();
        ui.on_key(move |key, mods| shortcut(key, mods, backend.focused() == Some(field)));

        let mut browser = Browser {
            page: widgets.page.get(),
            address,
            status: widgets.status.get(),
            back: widgets.back.get(),
            forward: widgets.forward.get(),
            reload: widgets.reload.get(),
            history: History::new(),
            start_url,
            url: String::new(),
            title: String::new(),
            failed: false,
            editing: false,
        };
        browser.show_url(&first);
        browser.set_status(&format!("Opening {}", browser.shown(&first)));
        browser.update_buttons();
        ui.set_window_title(APP_NAME);
        Ok(browser)
    }

    /// `url` as the address bar shows it: the start page by its short name.
    fn shown<'a>(&'a self, url: &'a str) -> &'a str {
        if url == self.start_url {
            START
        } else {
            url
        }
    }

    fn show_url(&mut self, url: &str) {
        self.url = url.to_string();
        if self.editing {
            return;
        }
        let text = self.shown(url).to_string();
        self.address.set_text(&text);
    }

    fn set_status(&self, text: &str) {
        self.status.set_text(text);
    }

    fn update_buttons(&self) {
        self.back.set_enabled(self.history.can_go_back());
        self.forward.set_enabled(self.history.can_go_forward());
        self.reload.set_enabled(self.history.current().is_some());
    }

    /// Opens `url` in the view.
    fn open(&mut self, url: &str) {
        let target = if url.eq_ignore_ascii_case(START) {
            self.start_url.clone()
        } else {
            url.to_string()
        };
        self.editing = false;
        println!("WEB:NAV:{}", marker_text(self.shown(&target)));
        println!("WEB:TIME:{}ms:nav", trace::now_ms());
        self.failed = false;
        self.set_status(&format!("Opening {}", self.shown(&target)));
        self.page.view().navigate(&target);
    }

    /// Opens what the address field holds.
    fn go(&mut self) {
        match address::normalize(&self.address.text()) {
            Some(url) => {
                self.address.set_text(&url);
                self.open(&url);
            }
            None => self.set_status("Type an address first"),
        }
    }

    /// Applies what the view reported.
    fn on_view_event(&mut self, ui: &Ui<Msg>, event: NetSurfViewEvent) {
        match event {
            NetSurfViewEvent::TitleChanged(title) => {
                self.title = title;
                let window = if self.title.trim().is_empty() {
                    APP_NAME
                } else {
                    &self.title
                };
                ui.set_window_title(window);
            }
            NetSurfViewEvent::UrlChanged(url) => {
                self.history.on_url(&url);
                self.show_url(&url);
            }
            NetSurfViewEvent::LoadingChanged(true) => {
                self.failed = false;
                self.set_status(&format!("Loading {}...", self.shown(&self.url)));
            }
            NetSurfViewEvent::LoadingChanged(false) => self.load_ended(),
            NetSurfViewEvent::Failed(why) => self.fail(&why),
            NetSurfViewEvent::FetchFailed { url, message } => {
                self.fail(&format!("{}: {message}", self.shown(&url)));
            }
        }
        self.update_buttons();
    }

    fn load_ended(&mut self) {
        self.history.on_load_end();
        if self.failed {
            return;
        }
        let shown = self.shown(&self.url).to_string();
        println!("WEB:TIME:{}ms:done", trace::now_ms());
        println!("WEB:LOAD:{}", marker_text(&shown));
        println!("WEB:TITLE:{}", marker_text(&self.title));
        let done = if self.title.trim().is_empty() {
            shown
        } else {
            self.title.clone()
        };
        self.set_status(&done);
    }

    fn fail(&mut self, why: &str) {
        self.failed = true;
        self.history.on_load_end();
        println!("WEB:TIME:{}ms:fail", trace::now_ms());
        println!("WEB:FAIL:{}", marker_text(why));
        self.set_status(&format!("Failed: {why}"));
    }
}

/// The widgets the window changes after building.
#[derive(Default)]
struct Widgets {
    page: Handle<Page>,
    address: Handle<Edit<Msg>>,
    status: Handle<Label<Msg>>,
    back: Handle<Button<Msg>>,
    forward: Handle<Button<Msg>>,
    reload: Handle<Button<Msg>>,
}

/// Back, Forward and Reload, the address field taking the rest, and Go.
fn toolbar(widgets: &Widgets) -> Layout<Msg> {
    row()
        .gap(6)
        .padding(Insets::symmetric(Dip(6.0), Dip(5.0)))
        .children((
            row().gap(4).children((
                button("Back")
                    .bind(&widgets.back)
                    .on_click_with(|| Some(Msg::Back))
                    .width(BACK_WIDTH),
                button("Forward")
                    .bind(&widgets.forward)
                    .on_click_with(|| Some(Msg::Forward))
                    .width(FORWARD_WIDTH),
                button("Reload")
                    .bind(&widgets.reload)
                    .on_click_with(|| Some(Msg::Reload))
                    .width(RELOAD_WIDTH),
            )),
            edit()
                .bind(&widgets.address)
                .placeholder("Type an address and press Enter")
                .on_change(|_| Msg::AddressEdited)
                .fill(1),
            button("Go").on_click_with(|| Some(Msg::Go)).width(GO_WIDTH),
        ))
}

/// The window's keyboard shortcuts; Enter only while the address field has
/// the focus, so a page's own forms still get it.
fn shortcut(key: Key, mods: xui_core::Modifiers, in_address: bool) -> Option<Msg> {
    match key {
        Key::RETURN if in_address => Some(Msg::Go),
        Key::LEFT if mods.alt => Some(Msg::Back),
        Key::RIGHT if mods.alt => Some(Msg::Forward),
        Key::F5 => Some(Msg::Reload),
        Key::L if mods.ctrl => Some(Msg::FocusAddress),
        Key::ESCAPE if in_address => Some(Msg::RestoreAddress),
        _ => None,
    }
}

impl App for Browser {
    type Msg = Msg;

    fn update(&mut self, msg: Msg, ui: &mut Ui<Msg>) {
        match msg {
            Msg::Frame => {
                for event in self.page.view().update() {
                    self.on_view_event(ui, event);
                }
            }
            Msg::Back => {
                if let Some(url) = self.history.back() {
                    self.open(&url);
                }
            }
            Msg::Forward => {
                if let Some(url) = self.history.forward() {
                    self.open(&url);
                }
            }
            Msg::Reload => {
                if let Some(url) = self.history.reload() {
                    self.open(&url);
                }
            }
            Msg::Go => self.go(),
            Msg::FocusAddress => {
                self.address.set_text("");
                self.address.focus();
                self.editing = true;
            }
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
