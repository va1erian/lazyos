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
use lazyweb::layout::{layout, Layout};
use lazyweb::marker_text;
use xui_app::backend::LazyOSBackend;
use xui_app::hidpi;
use xui_core::app::{App, Ui};
use xui_core::backend::{Event, Result, WidgetId};
use xui_core::widget::{Button, Edit, HasText, Label};
use xui_core::Key;
use xui_netsurf::{NetSurfView, NetSurfViewEvent};

/// The built-in start page.
const START_HTML: &str = include_str!("start.html");

/// The name the window has while a page has no title.
const APP_NAME: &str = "LazyWeb";

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
    Resized,
}

/// The window's widgets and state.
pub struct Browser {
    view: NetSurfView<Msg>,
    address: Edit<Msg>,
    status: Label<Msg>,
    back: Button<Msg>,
    forward: Button<Msg>,
    reload: Button<Msg>,
    go: Button<Msg>,
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
        let at = layout(ui.client_rect(), hidpi::layout_scale());

        let back = Button::new(ui, at.back, "Back")?.on_click(|| Some(Msg::Back));
        let forward = Button::new(ui, at.forward, "Forward")?.on_click(|| Some(Msg::Forward));
        let reload = Button::new(ui, at.reload, "Reload")?.on_click(|| Some(Msg::Reload));
        let address = Edit::new(ui, at.address, "")?
            .cue("Type an address and press Enter")
            .on_change(|_| Some(Msg::AddressEdited));
        let go = Button::new(ui, at.go, "Go")?.on_click(|| Some(Msg::Go));
        let status = Label::new(ui, at.status, "")?;
        // The first page loads without passing through `open`.
        let initial = if first == start_url { START } else { &first };
        println!("WEB:NAV:{}", marker_text(initial));
        println!("WEB:TIME:{}ms:nav", trace::now_ms());
        let view = NetSurfView::new(ui, at.view, &first, || Msg::Frame)?;

        let field = address.id();
        ui.on_key(move |key, mods| shortcut(key, mods, backend.focused() == Some(field)));
        ui.register_events(WidgetId::NONE, |event| match event {
            Event::Resize { .. } => Some(Msg::Resized),
            _ => None,
        });

        let mut browser = Browser {
            view,
            address,
            status,
            back,
            forward,
            reload,
            go,
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
        self.view.navigate(&target);
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

    fn relayout(&self, ui: &Ui<Msg>) {
        let at: Layout = layout(ui.client_rect(), hidpi::layout_scale());
        ui.apply_moves(&[
            (self.back.id(), at.back),
            (self.forward.id(), at.forward),
            (self.reload.id(), at.reload),
            (self.address.id(), at.address),
            (self.go.id(), at.go),
            (self.status.id(), at.status),
        ]);
        // The view tells the engine its new size on its next paint.
        self.view.set_bounds(at.view);
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
                for event in self.view.update() {
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
            Msg::Resized => self.relayout(ui),
        }
        self.update_buttons();
    }
}
